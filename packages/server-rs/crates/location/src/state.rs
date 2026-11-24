use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

pub use futures_util::future::BoxFuture;

pub type TransformError = Box<dyn std::error::Error + Send + Sync>;

pub type NotifyFn<S> = Box<dyn Fn(Arc<S>) -> BoxFuture<'static, ()> + Send + Sync>;

type ApplyFn<S> = dyn for<'a> Fn(&'a mut S) -> BoxFuture<'a, Result<(), TransformError>> + Send + Sync;

type NotifyHandle = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;

static NEXT_STATE_ID: AtomicU64 = AtomicU64::new(1);

tokio::task_local! {
    static CURRENT_GROUP: Arc<GroupInner>;
    static CURRENT_BATCH: Arc<Batch>;
}

pub struct Options<S> {
    pub name: Option<String>,
    pub initial: Box<dyn Fn() -> S + Send + Sync>,
    pub notify: Option<NotifyFn<S>>,
}

impl<S> Options<S> {
    pub fn new(initial: impl Fn() -> S + Send + Sync + 'static) -> Self {
        Options { name: None, initial: Box::new(initial), notify: None }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("{state} transform failed: {cause}")]
    Transform { state: String, cause: TransformError },
}

struct Data<S> {
    current: Arc<S>,
    entries: Vec<Arc<Entry<S>>>,
    dirty: bool,
    closed: bool,
    version: u64,
    next_entry_id: u64,
}

struct Shared<S> {
    state_id: u64,
    name: String,
    initial: Box<dyn Fn() -> S + Send + Sync>,
    notify: Option<NotifyFn<S>>,
    data: Mutex<Data<S>>,
    rebuild: tokio::sync::Mutex<()>,
}

impl<S> Shared<S> {
    fn lock(&self) -> MutexGuard<'_, Data<S>> {
        self.data.lock().expect("the state lock is never poisoned")
    }
}

pub struct State<S> {
    shared: Arc<Shared<S>>,
}

impl<S> Clone for State<S> {
    fn clone(&self) -> Self {
        State { shared: self.shared.clone() }
    }
}

impl<S: Send + Sync + 'static> State<S> {
    pub fn new(options: Options<S>) -> Self {
        let current = Arc::new((options.initial)());
        State {
            shared: Arc::new(Shared {
                state_id: NEXT_STATE_ID.fetch_add(1, Ordering::Relaxed),
                name: options.name.unwrap_or_else(|| "anonymous".to_string()),
                initial: options.initial,
                notify: options.notify,
                data: Mutex::new(Data {
                    current,
                    entries: Vec::new(),
                    dirty: false,
                    closed: false,
                    version: 0,
                    next_entry_id: 1,
                }),
                rebuild: tokio::sync::Mutex::new(()),
            }),
        }
    }

    pub async fn transform<F>(&self, f: F) -> Registration
    where
        F: Fn(&mut S) -> Result<(), TransformError> + Send + Sync + 'static,
    {
        let apply: Arc<ApplyFn<S>> = Arc::new(move |value: &mut S| {
            let result = f(value);
            Box::pin(std::future::ready(result))
        });
        self.register_entry(EntryApply::Transform(apply)).await
    }

    pub async fn transform_async<F>(&self, f: F) -> Registration
    where
        F: for<'a> Fn(&'a mut S) -> BoxFuture<'a, Result<(), TransformError>> + Send + Sync + 'static,
    {
        let apply: Arc<ApplyFn<S>> = Arc::new(f);
        self.register_entry(EntryApply::Transform(apply)).await
    }

    pub async fn block(&self, block: Arc<dyn ExternalBlock<S>>) -> Registration {
        self.register_entry(EntryApply::External(block)).await
    }

    pub async fn get(&self) -> Result<Arc<S>, StateError> {
        self.shared.read().await
    }

    pub fn invalidate(&self) {
        let mut data = self.shared.lock();
        data.dirty = true;
        data.version += 1;
    }

    pub fn revision(&self) -> u64 {
        self.shared.lock().version
    }

    pub async fn reload(&self) {
        changed(self.shared.clone()).await;
    }

    async fn register_entry(&self, apply: EntryApply<S>) -> Registration {
        let group = CURRENT_GROUP.try_with(Arc::clone).ok();
        let Some(record) = register(&self.shared, group, apply) else {
            return Registration { record: None };
        };
        let registration = Registration { record: Some(record) };
        changed(self.shared.clone()).await;
        registration
    }
}

impl<S: Send + Sync + 'static> Shared<S> {
    async fn read(self: &Arc<Self>) -> Result<Arc<S>, StateError> {
        {
            let data = self.lock();
            if data.closed || !data.dirty {
                return Ok(data.current.clone());
            }
        }
        let _rebuild = self.rebuild.lock().await;
        'rebuild: loop {
            let (version, entries) = {
                let data = self.lock();
                if data.closed || !data.dirty {
                    return Ok(data.current.clone());
                }
                (data.version, data.entries.clone())
            };
            let mut candidate = Some((self.initial)());
            for entry in &entries {
                let Some(mut value) = candidate.take() else {
                    continue 'rebuild;
                };
                let result = match &entry.apply {
                    EntryApply::Transform(apply) => {
                        let result = apply(&mut value).await;
                        candidate = Some(value);
                        result
                    }
                    EntryApply::External(block) => match block.apply(value).await {
                        Ok(next) => {
                            candidate = Some(next);
                            Ok(())
                        }
                        Err(cause) => Err(cause),
                    },
                };
                if let Err(cause) = result {
                    let Some(group) = &entry.group else {
                        return Err(StateError::Transform { state: self.name.clone(), cause });
                    };
                    group.disable(Failure { state: self.name.clone(), cause });
                    continue 'rebuild;
                }
                if self.lock().version != version {
                    continue 'rebuild;
                }
            }
            let Some(candidate) = candidate else {
                continue 'rebuild;
            };
            let mut data = self.lock();
            if data.version != version {
                continue 'rebuild;
            }
            if data.closed {
                return Ok(data.current.clone());
            }
            data.current = Arc::new(candidate);
            data.dirty = false;
            return Ok(data.current.clone());
        }
    }
}

pub trait ExternalBlock<S>: Send + Sync {
    fn apply<'a>(&'a self, state: S) -> BoxFuture<'a, Result<S, TransformError>>;
}

enum EntryApply<S> {
    Transform(Arc<ApplyFn<S>>),
    External(Arc<dyn ExternalBlock<S>>),
}

struct Entry<S> {
    id: u64,
    group: Option<Arc<GroupInner>>,
    apply: EntryApply<S>,
}

struct EntryRecord {
    state_id: u64,
    entry_id: u64,
    disposed: AtomicBool,
    detach: Box<dyn Fn() -> bool + Send + Sync>,
    notify: Box<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>,
    notify_sync: Box<dyn Fn() + Send + Sync>,
}

impl EntryRecord {
    fn remove(&self) -> bool {
        if self.disposed.swap(true, Ordering::AcqRel) {
            return false;
        }
        (self.detach)()
    }

    fn changed(&self) -> BoxFuture<'static, ()> {
        (self.notify)()
    }

    fn changed_sync(&self) {
        (self.notify_sync)();
    }
}

pub struct Registration {
    record: Option<Arc<EntryRecord>>,
}

impl Registration {
    pub async fn dispose(&self) {
        let Some(record) = &self.record else {
            return;
        };
        if record.remove() {
            record.changed().await;
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let Some(record) = &self.record else {
            return;
        };
        if record.remove() {
            record.changed_sync();
        }
    }
}

pub struct Failure {
    pub state: String,
    pub cause: TransformError,
}

struct GroupInner {
    failed: AtomicBool,
    entries: Mutex<Vec<Arc<EntryRecord>>>,
    report: Box<dyn Fn(Failure, BoxFuture<'static, ()>) + Send + Sync>,
}

impl GroupInner {
    fn lock(&self) -> MutexGuard<'_, Vec<Arc<EntryRecord>>> {
        self.entries.lock().expect("the group lock is never poisoned")
    }

    fn disable(&self, failure: Failure) {
        if self.failed.swap(true, Ordering::AcqRel) {
            return;
        }
        let records = {
            let mut entries = self.lock();
            std::mem::take(&mut *entries)
        };
        let mut notified = Vec::new();
        let mut refreshes: Vec<BoxFuture<'static, ()>> = Vec::new();
        for record in records {
            record.remove();
            if notified.contains(&record.state_id) {
                continue;
            }
            notified.push(record.state_id);
            refreshes.push(record.changed());
        }
        let refresh: BoxFuture<'static, ()> = Box::pin(async move {
            for refresh in refreshes {
                refresh.await;
            }
        });
        (self.report)(failure, refresh);
    }
}

pub struct Group {
    inner: Arc<GroupInner>,
}

impl Group {
    pub fn new(report: impl Fn(Failure, BoxFuture<'static, ()>) + Send + Sync + 'static) -> Self {
        Group {
            inner: Arc::new(GroupInner {
                failed: AtomicBool::new(false),
                entries: Mutex::new(Vec::new()),
                report: Box::new(report),
            }),
        }
    }

    pub async fn scope<F: Future>(&self, fut: F) -> F::Output {
        CURRENT_GROUP.scope(self.inner.clone(), fut).await
    }
}

struct Batch {
    active: AtomicBool,
    shutdown: bool,
    notifications: Mutex<Vec<(u64, NotifyHandle)>>,
}

impl Batch {
    fn lock(&self) -> MutexGuard<'_, Vec<(u64, NotifyHandle)>> {
        self.notifications.lock().expect("the batch lock is never poisoned")
    }

    fn push<S>(&self, shared: &Arc<Shared<S>>)
    where
        S: Send + Sync + 'static,
    {
        let state_id = shared.state_id;
        let shared = Arc::downgrade(shared);
        let handle: NotifyHandle = Box::new(move || {
            let Some(shared) = shared.upgrade() else {
                return noop();
            };
            Box::pin(notify_once(shared))
        });
        let mut notifications = self.lock();
        if notifications.iter().any(|(existing, _)| *existing == state_id) {
            return;
        }
        notifications.push((state_id, handle));
    }
}

pub struct BatchScope {
    batch: Option<Arc<Batch>>,
}

impl BatchScope {
    pub async fn scope<F: Future>(&self, fut: F) -> F::Output {
        match &self.batch {
            Some(batch) => CURRENT_BATCH.scope(batch.clone(), fut).await,
            None => fut.await,
        }
    }
}

pub async fn batch<F: Future>(fut: F) -> F::Output {
    match CURRENT_BATCH.try_with(Arc::clone) {
        Ok(current) if current.active.load(Ordering::Acquire) => fut.await,
        _ => run_batch(fut, false).await,
    }
}

pub async fn shutdown<F: Future>(fut: F) -> F::Output {
    run_batch(fut, true).await
}

struct Deactivate(Arc<Batch>);

impl Drop for Deactivate {
    fn drop(&mut self) {
        self.0.active.store(false, Ordering::Release);
    }
}

async fn run_batch<F: Future>(fut: F, shutdown: bool) -> F::Output {
    let batch = Arc::new(Batch { active: AtomicBool::new(true), shutdown, notifications: Mutex::new(Vec::new()) });
    let deactivate = Deactivate(batch.clone());
    let output = CURRENT_BATCH.scope(batch.clone(), fut).await;
    drop(deactivate);
    if batch.shutdown {
        return output;
    }
    let notifications = {
        let mut notifications = batch.lock();
        std::mem::take(&mut *notifications)
    };
    for (_, notify) in notifications {
        (notify)().await;
    }
    output
}

pub async fn inherit() -> BatchScope {
    BatchScope { batch: CURRENT_BATCH.try_with(Arc::clone).ok() }
}

pub fn changes<S, P, N>(project: P, notify: N) -> Box<dyn Fn(Arc<S>) -> BoxFuture<'static, ()> + Send + Sync>
where
    S: 'static,
    P: Fn(&S) -> String + Send + Sync + 'static,
    N: Fn(Arc<S>) -> BoxFuture<'static, ()> + Send + Sync + 'static,
{
    let published: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    Box::new(move |state: Arc<S>| {
        let next = project(&state);
        {
            let mut published = published.lock().expect("the changes lock is never poisoned");
            if published.as_ref() == Some(&next) {
                return noop();
            }
            *published = Some(next);
        }
        notify(state)
    })
}

fn register<S>(
    shared: &Arc<Shared<S>>,
    group: Option<Arc<GroupInner>>,
    apply: EntryApply<S>,
) -> Option<Arc<EntryRecord>>
where
    S: Send + Sync + 'static,
{
    let entry_id = {
        let mut data = shared.lock();
        let entry_id = data.next_entry_id;
        data.next_entry_id += 1;
        entry_id
    };
    let state_id = shared.state_id;
    let detach: Box<dyn Fn() -> bool + Send + Sync> = {
        let shared = Arc::downgrade(shared);
        let group = group.as_ref().map(Arc::downgrade);
        Box::new(move || {
            let Some(shared) = shared.upgrade() else {
                return false;
            };
            let group = group.as_ref().and_then(Weak::upgrade);
            let records = group.as_ref().map(|group| group.lock());
            if let Some(mut records) = records {
                records.retain(|record| record.state_id != state_id || record.entry_id != entry_id);
            }
            let mut data = shared.lock();
            let Some(position) = data.entries.iter().position(|entry| entry.id == entry_id) else {
                return false;
            };
            data.entries.remove(position);
            data.dirty = true;
            data.version += 1;
            true
        })
    };
    let notify: Box<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync> = {
        let shared = Arc::downgrade(shared);
        Box::new(move || {
            let Some(shared) = shared.upgrade() else {
                return noop();
            };
            Box::pin(changed(shared))
        })
    };
    let notify_sync: Box<dyn Fn() + Send + Sync> = {
        let shared = Arc::downgrade(shared);
        Box::new(move || {
            let Some(shared) = shared.upgrade() else {
                return;
            };
            changed_sync(shared);
        })
    };
    let record =
        Arc::new(EntryRecord { state_id, entry_id, disposed: AtomicBool::new(false), detach, notify, notify_sync });
    let mut records = match &group {
        Some(group) => {
            let records = group.lock();
            if group.failed.load(Ordering::Acquire) {
                return None;
            }
            Some(records)
        }
        None => None,
    };
    {
        let mut data = shared.lock();
        data.entries.push(Arc::new(Entry { id: entry_id, group: group.clone(), apply }));
    }
    if let Some(records) = records.as_mut() {
        records.push(record.clone());
    }
    Some(record)
}

fn noop() -> BoxFuture<'static, ()> {
    Box::pin(std::future::ready(()))
}

async fn notify_once<S>(shared: Arc<Shared<S>>)
where
    S: Send + Sync + 'static,
{
    if shared.lock().closed {
        return;
    }
    let value = match shared.read().await {
        Ok(value) => value,
        Err(error) => {
            log::warn!("state {} skipped a notification: {error}", shared.name);
            return;
        }
    };
    if let Some(notify) = &shared.notify {
        notify(value).await;
    }
}

enum Change {
    Skip,
    Notify,
    Deferred,
}

fn record_change<S>(shared: &Arc<Shared<S>>) -> Change
where
    S: Send + Sync + 'static,
{
    {
        let mut data = shared.lock();
        if data.closed {
            return Change::Skip;
        }
        data.dirty = true;
        data.version += 1;
    }
    let Ok(batch) = CURRENT_BATCH.try_with(Arc::clone) else {
        return Change::Notify;
    };
    if !batch.active.load(Ordering::Acquire) {
        return Change::Notify;
    }
    if batch.shutdown {
        let mut data = shared.lock();
        data.closed = true;
        return Change::Skip;
    }
    batch.push(shared);
    Change::Deferred
}

async fn changed<S>(shared: Arc<Shared<S>>)
where
    S: Send + Sync + 'static,
{
    if !matches!(record_change(&shared), Change::Notify) {
        return;
    }
    notify_once(shared).await;
}

fn changed_sync<S>(shared: Arc<Shared<S>>)
where
    S: Send + Sync + 'static,
{
    if !matches!(record_change(&shared), Change::Notify) {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let _task = runtime.spawn(async move {
        notify_once(shared).await;
    });
}
