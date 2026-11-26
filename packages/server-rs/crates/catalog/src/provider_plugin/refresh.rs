#![allow(clippy::type_complexity)]

use std::fmt::Debug;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt;
use opencode_bus::{Bus, Event};
use opencode_contract::generated::ConnectionInfo;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::provider_plugin::BoxFuture;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub interval: Option<Duration>,
    pub force: bool,
}

pub type Load<T, E> = Arc<dyn Fn(Options) -> BoxFuture<Result<T, E>> + Send + Sync>;

pub type Apply<T> = Arc<dyn Fn(T) -> BoxFuture<()> + Send + Sync>;

pub struct Loaded<A> {
    pub connection: Option<ConnectionInfo>,
    pub value: A,
}

pub struct Start<A, E> {
    pub integration_id: String,
    pub initial: A,
    pub interval: Option<Duration>,
    pub active: Box<dyn Fn() -> BoxFuture<Option<ConnectionInfo>> + Send + Sync>,
    pub load: Box<dyn Fn(A, Options) -> BoxFuture<Result<Loaded<A>, E>> + Send + Sync>,
    pub reload: Box<dyn Fn() -> BoxFuture<()> + Send + Sync>,
}

pub struct Refresh<A> {
    current: Arc<Mutex<A>>,
    task: JoinHandle<()>,
}

impl<A: Clone> Refresh<A> {
    pub fn current(&self) -> A {
        self.current.lock().expect("lock the provider refresh value").clone()
    }
}

impl<A> Drop for Refresh<A> {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn on_switch(
    bus: &Arc<Bus>,
    integration_id: &str,
    effect: impl Fn() -> BoxFuture<()> + Send + 'static,
) -> JoinHandle<()> {
    let integration_id = integration_id.to_owned();
    let mut subscription = bus.subscribe(&["credential.switched"], None);
    tokio::spawn(async move {
        while let Some(event) = subscription.recv().await {
            if switched_integration(&event) == Some(integration_id.as_str()) {
                effect().await;
            }
        }
    })
}

pub async fn reload_if_changed(
    load: impl std::future::Future<Output = bool>,
    reload: impl std::future::Future<Output = ()>,
) {
    if load.await {
        reload.await;
    }
}

struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub fn switched<T, E>(load: Load<T, E>, apply: Apply<T>) -> BoxFuture<()>
where
    T: Send + 'static,
    E: Send + 'static,
{
    async move {
        let mut forced = AbortOnDrop({
            let load = load.clone();
            tokio::spawn(load(Options { force: true, ..Options::default() }))
        });
        if let Ok(value) = load(Options { interval: Some(Duration::MAX), ..Options::default() }).await {
            apply(value).await;
        }
        if let Ok(Ok(value)) = (&mut forced.0).await {
            apply(value).await;
        }
    }
    .boxed()
}

pub fn start<A, E>(bus: &Arc<Bus>, input: Start<A, E>) -> Refresh<A>
where
    A: Clone + PartialEq + Send + Sync + 'static,
    E: Debug + Send + 'static,
{
    let interval = input.interval.unwrap_or(crate::shared_cache::INTERVAL);
    let current = Arc::new(Mutex::new(input.initial));
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    let active: Arc<dyn Fn() -> BoxFuture<Option<ConnectionInfo>> + Send + Sync> = input.active.into();
    let reload: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync> = input.reload.into();
    let loader: Arc<dyn Fn(A, Options) -> BoxFuture<Result<Loaded<A>, E>> + Send + Sync> = input.load.into();

    let apply: Apply<Loaded<A>> = {
        let state = current.clone();
        let lock = lock.clone();
        let active = active.clone();
        let reload = reload.clone();
        Arc::new(move |next| {
            let state = state.clone();
            let lock = lock.clone();
            let active = active.clone();
            let reload = reload.clone();
            async move {
                let job = tokio::spawn(async move {
                    let _permit = lock.lock().await;
                    let active_connection = active().await;
                    let unchanged = match (&next.connection, &active_connection) {
                        (Some(next), Some(active)) => crate::integration::key(next) == crate::integration::key(active),
                        (None, None) => true,
                        (Some(_), None) | (None, Some(_)) => false,
                    };
                    if !unchanged {
                        return;
                    }
                    let changed = {
                        let mut stored = state.lock().expect("lock the provider refresh value");
                        if *stored == next.value {
                            false
                        } else {
                            *stored = next.value;
                            true
                        }
                    };
                    if !changed {
                        return;
                    }
                    reload().await;
                });
                let _ = job.await;
            }
            .boxed()
        })
    };

    let load: Load<Loaded<A>, E> = {
        let state = current.clone();
        let loader = loader.clone();
        Arc::new(move |options| {
            let value = state.lock().expect("lock the provider refresh value").clone();
            let options = Options { interval: options.interval.or(Some(interval)), force: options.force };
            loader(value, options)
        })
    };

    let refresh: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync> = {
        let load = load.clone();
        let apply = apply.clone();
        Arc::new(move || {
            let load = load.clone();
            let apply = apply.clone();
            async move {
                match load(Options::default()).await {
                    Ok(next) => apply(next).await,
                    Err(error) => log::warn!("provider refresh failed: {error:?}"),
                }
            }
            .boxed()
        })
    };

    let mut subscription = bus.subscribe(&["credential.switched"], None);
    let task = {
        let integration_id = input.integration_id;
        let load = load.clone();
        let apply = apply.clone();
        let refresh = refresh.clone();
        tokio::spawn(async move {
            let mut running = Running(None);
            running.replace(tokio::spawn(refresh()));
            let mut ticker = tokio::time::interval_at(Instant::now() + interval, interval);
            loop {
                tokio::select! {
                    event = subscription.recv() => {
                        let Some(event) = event else {
                            break;
                        };
                        if switched_integration(&event) != Some(integration_id.as_str()) {
                            continue;
                        }
                        running.replace(tokio::spawn(switched(load.clone(), apply.clone())));
                    }
                    _ = ticker.tick() => {
                        if running.busy() {
                            continue;
                        }
                        running.replace(tokio::spawn(refresh()));
                    }
                }
            }
        })
    };

    Refresh { current, task }
}

struct Running(Option<JoinHandle<()>>);

impl Running {
    fn replace(&mut self, handle: JoinHandle<()>) {
        self.stop();
        self.0 = Some(handle);
    }

    fn busy(&self) -> bool {
        self.0.as_ref().is_some_and(|handle| !handle.is_finished())
    }

    fn stop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

fn switched_integration(event: &Event) -> Option<&str> {
    event.payload().get("data")?.get("integrationID")?.as_str()
}
