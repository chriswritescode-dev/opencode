use std::future::Future;
use std::sync::Mutex;

use tokio::task::JoinSet;

pub(crate) fn spawn(tasks: &Mutex<JoinSet<()>>, future: impl Future<Output = ()> + Send + 'static) {
    let mut tasks = tasks.lock().unwrap_or_else(|error| error.into_inner());
    while tasks.try_join_next().is_some() {}
    tasks.spawn(future);
}

pub(crate) fn abort_all(tasks: &Mutex<JoinSet<()>>) {
    tasks.lock().unwrap_or_else(|error| error.into_inner()).abort_all();
}
