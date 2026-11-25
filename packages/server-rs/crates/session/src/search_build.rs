use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_storage::{Database, StorageError};

use crate::search_index::{self, CHUNK_LIMIT, Ensured, Status};

const PAUSE: Duration = Duration::from_millis(5);
const BUSY: Duration = Duration::from_secs(1);
const MAX_FAILURES: usize = 3;

pub struct SessionSearchBuild {
    database: Arc<Database>,
    started: Mutex<bool>,
}

impl SessionSearchBuild {
    pub fn new(database: Arc<Database>) -> Self {
        Self { database, started: Mutex::new(false) }
    }

    pub fn start(self: &Arc<Self>) {
        {
            let mut started = self.started.lock().expect("the session search build latch is never poisoned");
            if *started {
                return;
            }
            *started = true;
        }
        let build = self.clone();
        tokio::spawn(async move { build.run().await });
    }

    async fn run(&self) {
        if let Err(error) = self.build().await {
            log::error!("session search build stopped: {error}");
        }
        *self.started.lock().expect("the session search build latch is never poisoned") = false;
    }

    async fn build(&self) -> Result<(), StorageError> {
        let database = self.database.as_ref();
        let state = match retry_on_busy(|| search_index::ensure(database)).await? {
            Ensured::Unsupported(reason) => {
                log::warn!("session search is not supported by this SQLite: {reason}");
                return Ok(());
            }
            Ensured::Newer => return Ok(()),
            Ensured::Ready(state) => state,
        };
        match state.status {
            Status::Done => return Ok(()),
            Status::Failed => retry_on_busy(|| search_index::retry(database)).await?,
            Status::Building => {}
        }
        let mut single = 0usize;
        let mut failures = 0usize;
        log::info!("session search build started");
        loop {
            let limit = (single > 0).then_some(1);
            match retry_on_busy(|| search_index::build_chunk(database, limit, false)).await {
                Ok(chunk) => {
                    single = single.saturating_sub(1);
                    failures = 0;
                    if chunk.done {
                        log::info!("session search build finished");
                        return Ok(());
                    }
                    tokio::time::sleep(PAUSE).await;
                }
                Err(error) => {
                    if single == 0 {
                        single = CHUNK_LIMIT;
                        continue;
                    }
                    failures += 1;
                    if failures >= MAX_FAILURES {
                        if let Err(fail_error) =
                            retry_on_busy(|| search_index::fail(database, &error.to_string())).await
                        {
                            log::error!("session search build could not record its failure: {fail_error}");
                        }
                        log::error!("session search build failed: {error}");
                        return Ok(());
                    }
                    log::warn!("session search skipped a row it could not index: {error}");
                    retry_on_busy(|| search_index::build_chunk(database, Some(1), true)).await?;
                }
            }
        }
    }
}

async fn retry_on_busy<T>(mut action: impl FnMut() -> Result<T, StorageError>) -> Result<T, StorageError> {
    loop {
        match action() {
            Err(error) if search_index::is_busy(&error) => tokio::time::sleep(BUSY).await,
            result => return result,
        }
    }
}
