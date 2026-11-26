pub mod fuzzysort;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use tokio::sync::{Mutex, watch};

use crate::filesystem::{Entry, EntryKind};
use crate::protected;
use crate::ripgrep::{Error, FindInput as RipgrepFindInput, Ripgrep};

pub const REFRESH_INTERVAL: Duration = Duration::from_secs(10);
pub const DEFAULT_LIMIT: usize = 50;
const MAX_SAFE_INTEGER: usize = 9_007_199_254_740_991;
const BOUNDED_SCAN_LIMIT: usize = 100_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FindKind {
    #[default]
    All,
    File,
    Directory,
}

#[derive(Clone, Debug)]
pub struct FindInput {
    pub query: String,
    pub kind: FindKind,
    pub limit: usize,
}

impl Default for FindInput {
    fn default() -> Self {
        Self { query: String::new(), kind: FindKind::All, limit: DEFAULT_LIMIT }
    }
}

#[derive(Clone, Debug)]
pub struct SearchOptions {
    pub directory: String,
    pub home: String,
    pub version_controlled: bool,
}

#[derive(Clone)]
pub struct Search {
    inner: Arc<Inner>,
}

struct Inner {
    scan: Scanner,
    directory: String,
    home: String,
    version_controlled: bool,
    state: Mutex<State>,
    done: watch::Sender<u64>,
}

type ScanFuture = Pin<Box<dyn Future<Output = Result<Vec<Entry>, Error>> + Send>>;
type Scanner = Arc<dyn Fn(ScanRequest) -> ScanFuture + Send + Sync>;

struct ScanRequest {
    directory: String,
    limit: usize,
    exclude: Vec<String>,
}

struct State {
    index: Arc<Index>,
    initialized: bool,
    settled_at: Option<tokio::time::Instant>,
    refreshing: bool,
    error: Option<Error>,
}

#[derive(Default)]
struct Index {
    files: IndexMap<String, Arc<fuzzysort::Prepared>>,
    directories: IndexMap<String, Arc<fuzzysort::Prepared>>,
}

impl Search {
    pub fn new(ripgrep: Arc<Ripgrep>, options: SearchOptions) -> Self {
        let scan = Arc::new(move |request: ScanRequest| {
            let ripgrep = ripgrep.clone();
            Box::pin(async move {
                ripgrep
                    .find(RipgrepFindInput {
                        cwd: request.directory,
                        pattern: "*".to_owned(),
                        limit: request.limit,
                        exclude: request.exclude,
                        hidden: true,
                        follow: false,
                        signal: None,
                        on_entry: None,
                    })
                    .await
            }) as ScanFuture
        });
        Self::with_scanner(scan, options)
    }

    fn with_scanner(scan: Scanner, options: SearchOptions) -> Self {
        let (done, _) = watch::channel(0u64);
        Self {
            inner: Arc::new(Inner {
                scan,
                directory: options.directory,
                home: options.home,
                version_controlled: options.version_controlled,
                state: Mutex::new(State {
                    index: Arc::new(Index::default()),
                    initialized: false,
                    settled_at: None,
                    refreshing: false,
                    error: None,
                }),
                done,
            }),
        }
    }

    pub async fn find(&self, input: FindInput) -> Result<Vec<Entry>, Error> {
        self.refresh().await?;
        let index = self.inner.state.lock().await.index.clone();
        let targets: Vec<&fuzzysort::Prepared> = match input.kind {
            FindKind::File => index.files.values().map(AsRef::as_ref).collect(),
            FindKind::Directory => index.directories.values().map(AsRef::as_ref).collect(),
            FindKind::All => index.files.values().chain(index.directories.values()).map(AsRef::as_ref).collect(),
        };
        Ok(fuzzysort::go(&input.query, &targets, input.limit)
            .into_iter()
            .map(|found| {
                let kind = if found.target.ends_with(std::path::MAIN_SEPARATOR) {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                };
                Entry { path: found.target, kind }
            })
            .collect())
    }

    async fn refresh(&self) -> Result<(), Error> {
        let mut state = self.inner.state.lock().await;
        if state.refreshing {
            if state.initialized {
                return Ok(());
            }
            let mut done = self.inner.done.subscribe();
            let generation = *done.borrow();
            drop(state);
            while *done.borrow() == generation {
                if done.changed().await.is_err() {
                    break;
                }
            }
            let state = self.inner.state.lock().await;
            return match &state.error {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            };
        }
        let settled = state.settled_at.is_some_and(|at| at.elapsed() < REFRESH_INTERVAL);
        if state.initialized && settled {
            return Ok(());
        }
        state.refreshing = true;
        state.error = None;
        let initialized = state.initialized;
        let previous = state.index.clone();
        let mut done = self.inner.done.subscribe();
        let generation = *done.borrow();
        drop(state);
        self.start_scan(previous);
        if initialized {
            return Ok(());
        }
        while *done.borrow() == generation {
            if done.changed().await.is_err() {
                break;
            }
        }
        let state = self.inner.state.lock().await;
        match &state.error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn start_scan(&self, previous: Arc<Index>) {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            inner.scan_once(previous).await;
        });
    }
}

impl Inner {
    async fn scan_once(&self, previous: Arc<Index>) {
        let result = (self.scan)(scan_request(&self.directory, &self.home, self.version_controlled)).await;
        let mut state = self.state.lock().await;
        match result {
            Ok(entries) => {
                let mut next = Index::default();
                for entry in entries {
                    next.insert(entry.path, &previous);
                }
                state.index = Arc::new(next);
                state.initialized = true;
                state.error = None;
            }
            Err(error) => state.error = Some(error),
        }
        state.settled_at = Some(tokio::time::Instant::now());
        state.refreshing = false;
        drop(state);
        self.done.send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}

impl Index {
    fn insert(&mut self, path: String, previous: &Index) {
        let prepared = previous.files.get(&path).cloned().unwrap_or_else(|| Arc::new(fuzzysort::prepare(&path)));
        let parts: Vec<&str> = path.split('/').collect();
        for offset in 0..parts.len().saturating_sub(1) {
            let directory = format!("{}{}", parts[..=offset].join("/"), std::path::MAIN_SEPARATOR);
            let prepared = previous
                .directories
                .get(&directory)
                .cloned()
                .unwrap_or_else(|| Arc::new(fuzzysort::prepare(&directory)));
            self.directories.entry(directory).or_insert(prepared);
        }
        let _ = self.files.insert(path, prepared);
    }
}

fn scan_request(directory: &str, home: &str, version_controlled: bool) -> ScanRequest {
    let at_home = protected::is_home(directory, home);
    let limit = if version_controlled && !at_home { MAX_SAFE_INTEGER } else { BOUNDED_SCAN_LIMIT };
    let exclude =
        if at_home { protected::names().iter().map(|name| format!("{name}/**")).collect() } else { Vec::new() };
    ScanRequest { directory: directory.to_owned(), limit, exclude }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use serde_json::json;
    use tokio::sync::Semaphore;

    use super::*;

    fn entry(path: &str) -> Entry {
        Entry { path: path.to_owned(), kind: EntryKind::File }
    }

    fn scripted(entries: Vec<Entry>) -> (Scanner, Arc<StdMutex<Vec<ScanRequest>>>) {
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let recorded = requests.clone();
        let entries = Arc::new(entries);
        let scan: Scanner = Arc::new(move |request| {
            recorded.lock().expect("records scan requests").push(request);
            let entries = entries.clone();
            Box::pin(async move { Ok(entries.as_ref().clone()) })
        });
        (scan, requests)
    }

    fn options(directory: &str) -> SearchOptions {
        SearchOptions { directory: directory.to_owned(), home: "/home/agent".to_owned(), version_controlled: false }
    }

    #[tokio::test]
    async fn home_scans_are_bounded_and_exclude_protected_directories() {
        let (scan, requests) = scripted(Vec::new());
        let search = Search::with_scanner(
            scan,
            SearchOptions {
                directory: "/home/agent".to_owned(),
                home: "/home/agent".to_owned(),
                version_controlled: true,
            },
        );
        search.find(FindInput { query: "src".to_owned(), ..FindInput::default() }).await.expect("finds");
        let requests = requests.lock().expect("reads requests");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].limit, BOUNDED_SCAN_LIMIT);
        assert_eq!(requests[0].exclude, protected::names().iter().map(|name| format!("{name}/**")).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn version_controlled_scans_are_unbounded() {
        let (scan, requests) = scripted(Vec::new());
        let search = Search::with_scanner(scan, SearchOptions { version_controlled: true, ..options("/repo") });
        search.find(FindInput::default()).await.expect("finds");
        assert_eq!(requests.lock().expect("reads requests")[0].limit, MAX_SAFE_INTEGER);
    }

    #[tokio::test]
    async fn indexes_parent_directories_and_filters_by_kind() {
        let (scan, _) = scripted(vec![entry("src/nested/file.ts"), entry("README.md")]);
        let search = Search::with_scanner(scan, options("/repo"));
        let files = search
            .find(FindInput { query: "file".to_owned(), kind: FindKind::File, ..FindInput::default() })
            .await
            .expect("finds");
        assert_eq!(files.iter().map(|entry| entry.path.as_str()).collect::<Vec<_>>(), ["src/nested/file.ts"]);
        let directories = search
            .find(FindInput { query: "nested".to_owned(), kind: FindKind::Directory, ..FindInput::default() })
            .await
            .expect("finds");
        assert_eq!(directories.len(), 1);
        assert_eq!(directories[0].path, format!("src/nested{}", std::path::MAIN_SEPARATOR));
        assert_eq!(directories[0].kind, EntryKind::Directory);
    }

    #[tokio::test]
    async fn first_query_waits_and_later_queries_read_the_old_index_while_a_rescan_runs() {
        let started = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let calls = Arc::new(StdMutex::new(0usize));
        let scan: Scanner = {
            let started = started.clone();
            let release = release.clone();
            let calls = calls.clone();
            Arc::new(move |_| {
                let started = started.clone();
                let release = release.clone();
                let calls = calls.clone();
                Box::pin(async move {
                    let call = {
                        let mut calls = calls.lock().expect("counts scans");
                        *calls += 1;
                        *calls
                    };
                    if call > 1 {
                        started.add_permits(1);
                        let _permit = release.acquire().await.expect("a release permit");
                    }
                    Ok(vec![entry(if call == 1 { "old.ts" } else { "new.ts" })])
                })
            })
        };
        let search = Search::with_scanner(scan, options("/repo"));
        let first = search.find(FindInput { query: "old".to_owned(), ..FindInput::default() }).await.expect("finds");
        assert_eq!(first[0].path, "old.ts");

        search.find(FindInput { query: "old".to_owned(), ..FindInput::default() }).await.expect("finds");
        assert_eq!(*calls.lock().expect("counts scans"), 1);

        tokio::time::pause();
        tokio::time::advance(REFRESH_INTERVAL).await;
        let during = search.find(FindInput { query: "old".to_owned(), ..FindInput::default() }).await.expect("finds");
        assert_eq!(during[0].path, "old.ts");
        let _started = started.acquire().await.expect("the rescan starts");
        release.add_permits(1);
        for _ in 0..100 {
            tokio::task::yield_now().await;
            let found =
                search.find(FindInput { query: "new".to_owned(), ..FindInput::default() }).await.expect("finds");
            if !found.is_empty() {
                assert_eq!(found[0].path, "new.ts");
                return;
            }
        }
        panic!("the refreshed index never became visible");
    }

    #[tokio::test]
    async fn reuses_prepared_targets_across_refreshes() {
        let (scan, requests) = scripted(vec![entry("src/index.ts")]);
        let search = Search::with_scanner(scan, options("/repo"));
        search.find(FindInput { query: "index".to_owned(), ..FindInput::default() }).await.expect("finds");
        let before = {
            let state = search.inner.state.lock().await;
            state.index.files.get("src/index.ts").cloned().expect("the prepared target")
        };
        tokio::time::pause();
        tokio::time::advance(REFRESH_INTERVAL).await;
        search.find(FindInput { query: "index".to_owned(), ..FindInput::default() }).await.expect("finds");
        for _ in 0..100 {
            tokio::task::yield_now().await;
            if requests.lock().expect("reads requests").len() == 2 {
                let state = search.inner.state.lock().await;
                let after = state.index.files.get("src/index.ts").cloned().expect("the prepared target");
                assert!(Arc::ptr_eq(&before, &after), "a refresh must reuse prepared targets");
                return;
            }
        }
        panic!("the refresh never scanned again");
    }

    #[test]
    fn results_carry_the_expected_shape() {
        let prepared = fuzzysort::prepare("src/index.ts");
        let found = fuzzysort::go("index", &[&prepared], 50);
        assert_eq!(found[0].target, "src/index.ts");
        assert_eq!(serde_json::to_value(&found[0].indexes).expect("serialises"), json!([4, 5, 6, 7, 8]));
    }
}
