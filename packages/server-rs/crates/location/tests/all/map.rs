use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::future::BoxFuture;
use opencode_location::environment::Environment;
use opencode_location::environment::local::LocalEnvironment;
use opencode_location::events::LocationEvent;
use opencode_location::lifecycle::Lifecycle;
use opencode_location::location::{BootError, LocationInfo, LocationProject, LocationRef, boot_location};
use opencode_location::map::{LocationMap, LocationServices};
use opencode_location::project::ProjectService;
use opencode_storage::{Database, DatabaseLocation};
use tokio::sync::{Barrier, Notify};

fn database() -> Arc<Database> {
    let (database, _) =
        Database::open(DatabaseLocation::Memory, std::env::temp_dir().as_path()).expect("opens the database");
    Arc::new(database)
}

fn info(reference: &LocationRef) -> LocationInfo {
    LocationInfo {
        directory: reference.directory.clone(),
        workspace_id: reference.workspace_id.clone(),
        project: LocationProject {
            id: "project".to_string(),
            directory: reference.directory.clone(),
            canonical: reference.directory.clone(),
        },
        vcs: None,
    }
}

struct TestServices {
    info: LocationInfo,
    lifecycle: Lifecycle,
    closes: Arc<AtomicUsize>,
    barrier: Option<Arc<Barrier>>,
}

impl LocationServices for TestServices {
    fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Some(barrier) = &self.barrier {
                barrier.wait().await;
            }
            self.closes.fetch_add(1, Ordering::SeqCst);
        })
    }
}

async fn services(
    projects: Arc<ProjectService>,
    reference: LocationRef,
    closes: Arc<AtomicUsize>,
    barrier: Option<Arc<Barrier>>,
) -> Result<Arc<TestServices>, BootError> {
    let info = info(&reference);
    let lifecycle = Lifecycle::start(projects, &info).await.expect("starts the lifecycle");
    Ok(Arc::new(TestServices { info, lifecycle, closes, barrier }))
}

fn simple_boot(
    projects: Arc<ProjectService>,
    closes: Arc<AtomicUsize>,
    barrier: Option<Arc<Barrier>>,
) -> impl Fn(LocationRef) -> BoxFuture<'static, Result<Arc<TestServices>, BootError>> + Send + Sync + 'static {
    move |reference: LocationRef| -> BoxFuture<'static, Result<Arc<TestServices>, BootError>> {
        let projects = projects.clone();
        let closes = closes.clone();
        let barrier = barrier.clone();
        Box::pin(async move { services(projects, reference, closes, barrier).await })
    }
}

fn counting_boot(
    projects: Arc<ProjectService>,
    closes: Arc<AtomicUsize>,
    boots: Arc<AtomicUsize>,
) -> impl Fn(LocationRef) -> BoxFuture<'static, Result<Arc<TestServices>, BootError>> + Send + Sync + 'static {
    move |reference: LocationRef| -> BoxFuture<'static, Result<Arc<TestServices>, BootError>> {
        boots.fetch_add(1, Ordering::SeqCst);
        let projects = projects.clone();
        let closes = closes.clone();
        Box::pin(async move { services(projects, reference, closes, None).await })
    }
}

fn gated_boot(
    projects: Arc<ProjectService>,
    closes: Arc<AtomicUsize>,
    attempts: Arc<AtomicUsize>,
    gate: Arc<Notify>,
) -> impl Fn(LocationRef) -> BoxFuture<'static, Result<Arc<TestServices>, BootError>> + Send + Sync + 'static {
    move |reference: LocationRef| -> BoxFuture<'static, Result<Arc<TestServices>, BootError>> {
        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
        let projects = projects.clone();
        let closes = closes.clone();
        let gate = gate.clone();
        Box::pin(async move {
            if attempt == 0 {
                gate.notified().await;
                return Err(BootError::DirectoryNotFound(reference.directory.clone()));
            }
            services(projects, reference, closes, None).await
        })
    }
}

fn filesystem_boot(
    projects: Arc<ProjectService>,
    environment: Arc<dyn Environment>,
    closes: Arc<AtomicUsize>,
) -> impl Fn(LocationRef) -> BoxFuture<'static, Result<Arc<TestServices>, BootError>> + Send + Sync + 'static {
    move |reference: LocationRef| -> BoxFuture<'static, Result<Arc<TestServices>, BootError>> {
        let projects = projects.clone();
        let environment = environment.clone();
        let closes = closes.clone();
        Box::pin(async move {
            let info = boot_location(&reference, &projects, environment.as_ref()).await?;
            let lifecycle = Lifecycle::start(projects, &info).await?;
            Ok(Arc::new(TestServices { info, lifecycle, closes, barrier: None }))
        })
    }
}

async fn wait_for_boot(attempts: &AtomicUsize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while attempts.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the boot starts");
}

async fn wait_for_closes(closes: &AtomicUsize, count: usize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while closes.load(Ordering::SeqCst) < count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the locations close");
}

async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

type Records = Arc<Mutex<Vec<(log::Level, String)>>>;

static RECORDS: OnceLock<Records> = OnceLock::new();

struct RecordingLogger {
    records: Records,
}

impl log::Log for RecordingLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        self.records
            .lock()
            .expect("the log records lock is not poisoned")
            .push((record.level(), record.args().to_string()));
    }

    fn flush(&self) {}
}

fn install_logger() -> Records {
    let records = RECORDS.get_or_init(|| Arc::new(Mutex::new(Vec::new()))).clone();
    let _ = log::set_boxed_logger(Box::new(RecordingLogger { records: records.clone() }));
    log::set_max_level(log::LevelFilter::Warn);
    records
}

fn warnings_for(directory: &str) -> Vec<String> {
    install_logger()
        .lock()
        .expect("the log records lock is not poisoned")
        .iter()
        .filter(|(level, message)| *level == log::Level::Warn && message.contains(directory))
        .map(|(_, message)| message.clone())
        .collect()
}

#[tokio::test]
async fn canonicalizes_equivalent_refs_to_one_cached_graph() {
    let projects = Arc::new(crate::project_service(database()));
    let closes = Arc::new(AtomicUsize::new(0));
    let boots = Arc::new(AtomicUsize::new(0));
    let map = LocationMap::new(counting_boot(projects, closes, boots.clone()));
    let directory = "/fake/canonical".to_string();
    let base = LocationRef::new(directory.clone(), None);

    let first = map.get(&base).await.expect("boots the canonical directory");
    for variant in [
        format!("{directory}/"),
        format!("{directory}//"),
        "/fake//canonical".to_string(),
        format!("{directory}/./child/.."),
    ] {
        let lease = map.get(&LocationRef::new(variant, None)).await.expect("reuses the canonical graph");
        assert!(Arc::ptr_eq(lease.services(), first.services()));
    }
    assert_eq!(boots.load(Ordering::SeqCst), 1);

    assert_eq!(LocationRef::new("/", None).canonical().directory, "/");
    assert_eq!(LocationRef::new("//", None).canonical().directory, "/");
    assert_eq!(
        LocationRef::new(format!("{directory}/"), Some("w".to_string())).canonical().workspace_id,
        Some("w".to_string())
    );

    let workspace = LocationRef::new(directory.clone(), Some("w".to_string()));
    let workspace_lease = map.get(&workspace).await.expect("boots the workspace location");
    assert!(!Arc::ptr_eq(workspace_lease.services(), first.services()));
    assert_eq!(boots.load(Ordering::SeqCst), 2);

    drop(first);
    drop(workspace_lease);
    map.invalidate(&LocationRef::new(format!("{directory}/"), None)).await;
    assert!(!map.has(&base).await);
    assert!(map.has(&workspace).await);
}

#[tokio::test]
async fn retries_a_missing_directory_after_it_is_recreated() {
    let temp = crate::TempDir::new();
    let root = PathBuf::from(temp.path());
    std::fs::create_dir_all(&root).expect("creates the temporary root");
    let directory = root.join("recreated");
    let directory_text = directory.to_string_lossy().into_owned();
    let projects = Arc::new(crate::project_service(database()));
    let closes = Arc::new(AtomicUsize::new(0));
    let environment: Arc<dyn Environment> = Arc::new(LocalEnvironment::new());
    let map = LocationMap::new(filesystem_boot(projects.clone(), environment, closes));
    let reference = LocationRef::new(directory_text.clone(), None);

    let error = map.get(&reference).await.err().expect("fails while the directory is missing");
    assert!(matches!(error, BootError::DirectoryNotFound(ref path) if path == &directory_text));
    assert!(!map.has(&reference).await);
    assert!(projects.list(true).await.expect("lists the projects").is_empty());

    map.invalidate(&reference).await;

    std::fs::create_dir_all(&directory).expect("recreates the directory");
    let lease = map.get(&reference).await.expect("boots the recreated directory");
    assert_eq!(lease.services().info.directory, directory_text);
    let again = map.get(&reference).await.expect("reuses the repaired graph");
    assert!(Arc::ptr_eq(again.services(), lease.services()));
    assert_eq!(projects.list(true).await.expect("lists the projects").len(), 1);
}

#[tokio::test]
async fn keeps_the_repaired_graph_after_concurrent_failures() {
    let projects = Arc::new(crate::project_service(database()));
    let closes = Arc::new(AtomicUsize::new(0));
    let attempts = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Notify::new());
    let map = Arc::new(LocationMap::new(gated_boot(projects, closes, attempts.clone(), gate.clone())));
    let reference = LocationRef::new("/fake/concurrent", None);

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let map = map.clone();
            let reference = reference.clone();
            tokio::spawn(async move { map.get(&reference).await })
        })
        .collect();
    wait_for_boot(&attempts).await;
    settle().await;
    gate.notify_one();
    for handle in handles {
        let result = handle.await.expect("the acquisition joins");
        assert!(matches!(result, Err(BootError::DirectoryNotFound(ref path)) if path == &reference.directory));
    }
    assert_eq!(attempts.load(Ordering::SeqCst), 1);

    let repaired = map.get(&reference).await.expect("retries the boot");
    let again = map.get(&reference).await.expect("reuses the repaired graph");
    assert!(Arc::ptr_eq(repaired.services(), again.services()));
    drop(repaired);
    drop(again);
    assert!(map.has(&reference).await);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn invalidated_boot_failure_does_not_evict_its_replacement() {
    let projects = Arc::new(crate::project_service(database()));
    let closes = Arc::new(AtomicUsize::new(0));
    let attempts = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Notify::new());
    let map = Arc::new(LocationMap::new(gated_boot(projects, closes, attempts.clone(), gate.clone())));
    let reference = LocationRef::new("/fake/invalidated", None);

    let first = tokio::spawn({
        let map = map.clone();
        let reference = reference.clone();
        async move { map.get(&reference).await }
    });
    wait_for_boot(&attempts).await;
    map.invalidate(&reference).await;
    let replacement = map.get(&reference).await.expect("boots the replacement");
    gate.notify_one();
    let retried = first.await.expect("the first acquisition joins").expect("retries after the invalidated boot");
    assert!(Arc::ptr_eq(retried.services(), replacement.services()));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert!(map.has(&reference).await);
    let again = map.get(&reference).await.expect("reuses the replacement");
    assert!(Arc::ptr_eq(again.services(), replacement.services()));
}

#[tokio::test]
async fn an_invalidated_boot_that_succeeds_shuts_down_without_installing() {
    let projects = Arc::new(crate::project_service(database()));
    let closes = Arc::new(AtomicUsize::new(0));
    let attempts = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Notify::new());
    let map = Arc::new(LocationMap::new({
        let projects = projects.clone();
        let closes = closes.clone();
        let attempts = attempts.clone();
        let gate = gate.clone();
        move |reference: LocationRef| -> BoxFuture<'static, Result<Arc<TestServices>, BootError>> {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            let projects = projects.clone();
            let closes = closes.clone();
            let gate = gate.clone();
            Box::pin(async move {
                if attempt == 0 {
                    gate.notified().await;
                }
                services(projects, reference, closes, None).await
            })
        }
    }));
    let reference = LocationRef::new("/fake/detached", None);
    let mut events = projects.subscribe();

    let first = tokio::spawn({
        let map = map.clone();
        let reference = reference.clone();
        async move { map.get(&reference).await }
    });
    wait_for_boot(&attempts).await;
    map.invalidate(&reference).await;
    let replacement = map.get(&reference).await.expect("boots the replacement");
    gate.notify_one();
    let retried = first.await.expect("the first acquisition joins").expect("retries after the invalidated boot");
    assert!(Arc::ptr_eq(retried.services(), replacement.services()));

    wait_for_closes(&closes, 1).await;
    assert_eq!(closes.load(Ordering::SeqCst), 1);
    assert!(!replacement.lifecycle().is_closed());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert!(map.has(&reference).await);
    let again = map.get(&reference).await.expect("reuses the replacement");
    assert!(Arc::ptr_eq(again.services(), replacement.services()));

    let shutdowns: Vec<(String, Option<String>)> = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            LocationEvent::LocationShutdown { directory, workspace_id } => Some((directory, workspace_id)),
            _ => None,
        })
        .collect();
    assert_eq!(shutdowns, vec![(reference.directory.clone(), reference.workspace_id.clone())]);
    assert!(!replacement.lifecycle().is_closed());
}

#[tokio::test]
async fn invalidating_an_in_flight_boot_shuts_it_down_and_boots_fresh_services() {
    let projects = Arc::new(crate::project_service(database()));
    let closes = Arc::new(AtomicUsize::new(0));
    let attempts = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Notify::new());
    let map = Arc::new(LocationMap::new({
        let projects = projects.clone();
        let closes = closes.clone();
        let attempts = attempts.clone();
        let gate = gate.clone();
        move |reference: LocationRef| -> BoxFuture<'static, Result<Arc<TestServices>, BootError>> {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            let projects = projects.clone();
            let closes = closes.clone();
            let gate = gate.clone();
            Box::pin(async move {
                if attempt == 0 {
                    gate.notified().await;
                }
                services(projects, reference, closes, None).await
            })
        }
    }));
    let reference = LocationRef::new("/fake/in-flight-boot", None);

    let first = tokio::spawn({
        let map = map.clone();
        let reference = reference.clone();
        async move { map.get(&reference).await }
    });
    wait_for_boot(&attempts).await;
    map.invalidate(&reference).await;
    assert!(!map.has(&reference).await);
    gate.notify_one();
    wait_for_closes(&closes, 1).await;
    assert_eq!(closes.load(Ordering::SeqCst), 1);

    let replacement = map.get(&reference).await.expect("boots fresh services");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert!(!replacement.lifecycle().is_closed());
    let retried = first.await.expect("the first acquisition joins").expect("retries after the invalidated boot");
    assert!(Arc::ptr_eq(retried.services(), replacement.services()));
}

#[tokio::test]
async fn recovers_from_a_panicking_boot() {
    let projects = Arc::new(crate::project_service(database()));
    let closes = Arc::new(AtomicUsize::new(0));
    let attempts = Arc::new(AtomicUsize::new(0));
    let map = Arc::new(LocationMap::new({
        let projects = projects.clone();
        let closes = closes.clone();
        let attempts = attempts.clone();
        move |reference: LocationRef| -> BoxFuture<'static, Result<Arc<TestServices>, BootError>> {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            let projects = projects.clone();
            let closes = closes.clone();
            Box::pin(async move {
                if attempt == 0 {
                    panic!("the boot panics");
                }
                services(projects, reference, closes, None).await
            })
        }
    }));
    let reference = LocationRef::new("/fake/panicking", None);

    let error = tokio::time::timeout(Duration::from_secs(1), map.get(&reference))
        .await
        .expect("the panicking boot finishes")
        .err()
        .expect("the panicking boot fails");
    assert!(matches!(error, BootError::Panicked));
    assert!(!map.has(&reference).await);

    let lease = tokio::time::timeout(Duration::from_secs(1), map.get(&reference))
        .await
        .expect("the retried boot finishes")
        .expect("boots after the panic");
    assert!(!lease.lifecycle().is_closed());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn failed_boot_finishes_after_a_cancelled_acquisition() {
    let projects = Arc::new(crate::project_service(database()));
    let closes = Arc::new(AtomicUsize::new(0));
    let attempts = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Notify::new());
    let map = Arc::new(LocationMap::new(gated_boot(projects, closes, attempts.clone(), gate.clone())));
    let reference = LocationRef::new("/fake/cancelled", None);

    let first = tokio::spawn({
        let map = map.clone();
        let reference = reference.clone();
        async move { map.get(&reference).await }
    });
    wait_for_boot(&attempts).await;
    first.abort();
    let cancelled = first.await.err().expect("the acquisition is cancelled");
    assert!(cancelled.is_cancelled());
    gate.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        while map.has(&reference).await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the failed boot removes its slot");

    let recovered = map.get(&reference).await.expect("boots after the cancelled acquisition");
    assert!(map.has(&reference).await);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    drop(recovered);
}

#[tokio::test]
async fn an_in_flight_lease_keeps_old_services_after_invalidate() {
    let projects = Arc::new(crate::project_service(database()));
    let closes = Arc::new(AtomicUsize::new(0));
    let map = LocationMap::new(simple_boot(projects, closes.clone(), None));
    let reference = LocationRef::new("/fake/in-flight", None);

    let lease = map.get(&reference).await.expect("boots the location");
    let old = lease.services().clone();
    assert!(!old.lifecycle().is_closed());

    map.invalidate(&reference).await;
    assert!(old.lifecycle().is_closed());
    assert_eq!(closes.load(Ordering::SeqCst), 0);

    let new = map.get(&reference).await.expect("boots fresh services");
    assert!(!Arc::ptr_eq(new.services(), &old));
    assert!(!new.lifecycle().is_closed());

    drop(lease);
    wait_for_closes(&closes, 1).await;
    assert_eq!(closes.load(Ordering::SeqCst), 1);

    assert!(map.has(&reference).await);
    map.invalidate(&reference).await;
    drop(new);
    wait_for_closes(&closes, 2).await;
    assert_eq!(closes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn evicts_one_location_without_touching_another() {
    let projects = Arc::new(crate::project_service(database()));
    let closes = Arc::new(AtomicUsize::new(0));
    let map = LocationMap::new(simple_boot(projects, closes.clone(), None));
    let a_reference = LocationRef::new("/fake/a", None);
    let b_reference = LocationRef::new("/fake/b", None);

    let a = map.get(&a_reference).await.expect("boots A");
    let b = map.get(&b_reference).await.expect("boots B");
    let b_services = b.services().clone();

    map.invalidate(&a_reference).await;
    assert!(a.lifecycle().is_closed());
    assert!(!b.lifecycle().is_closed());
    assert!(Arc::ptr_eq(b.services(), &b_services));
    assert!(!b_services.lifecycle().is_closed());
    assert_eq!(map.keys().await, vec![b_reference.canonical()]);
    assert_eq!(closes.load(Ordering::SeqCst), 0);

    drop(a);
    wait_for_closes(&closes, 1).await;
    assert_eq!(closes.load(Ordering::SeqCst), 1);
    assert!(!b.lifecycle().is_closed());
}

#[tokio::test]
async fn reload_reboots_every_location_and_reports_the_first_failure() {
    let projects = Arc::new(crate::project_service(database()));
    let closes = Arc::new(AtomicUsize::new(0));
    let fail_a = Arc::new(AtomicBool::new(false));
    let fail_b = Arc::new(AtomicBool::new(false));
    let a_directory = "/fake/reload-a".to_string();
    let b_directory = "/fake/reload-b".to_string();
    let map = LocationMap::new({
        let projects = projects.clone();
        let closes = closes.clone();
        let fail_a = fail_a.clone();
        let fail_b = fail_b.clone();
        let a_directory = a_directory.clone();
        let b_directory = b_directory.clone();
        move |reference: LocationRef| -> BoxFuture<'static, Result<Arc<TestServices>, BootError>> {
            let failed = (reference.directory == a_directory && fail_a.load(Ordering::SeqCst))
                || (reference.directory == b_directory && fail_b.load(Ordering::SeqCst));
            let projects = projects.clone();
            let closes = closes.clone();
            Box::pin(async move {
                if failed {
                    return Err(BootError::DirectoryNotFound(reference.directory.clone()));
                }
                services(projects, reference, closes, None).await
            })
        }
    });
    let a_reference = LocationRef::new(a_directory.clone(), None);
    let b_reference = LocationRef::new(b_directory.clone(), None);

    let a = map.get(&a_reference).await.expect("boots A");
    let b = map.get(&b_reference).await.expect("boots B");
    let old_b = b.services().clone();
    drop(a);
    drop(b);

    fail_a.store(true, Ordering::SeqCst);
    fail_b.store(true, Ordering::SeqCst);
    let error = map.reload().await.expect_err("reload reports the first failure");
    assert!(matches!(error, BootError::DirectoryNotFound(ref path) if path == &a_directory));
    assert!(!map.has(&a_reference).await);
    assert!(!map.has(&b_reference).await);

    fail_b.store(false, Ordering::SeqCst);
    let fresh_b = map.get(&b_reference).await.expect("boots B again");
    assert!(map.has(&b_reference).await);
    assert!(!Arc::ptr_eq(fresh_b.services(), &old_b));

    fail_a.store(false, Ordering::SeqCst);
    let a_again = map.get(&a_reference).await.expect("re-arms A");
    map.reload().await.expect("reloads every location");
    assert!(map.has(&a_reference).await);
    assert!(map.has(&b_reference).await);
    drop(a_again);
    drop(fresh_b);
}

#[tokio::test]
async fn shutdown_all_closes_borrowed_entries_concurrently() {
    let projects = Arc::new(crate::project_service(database()));
    let closes = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(3));
    let map = LocationMap::new(simple_boot(projects, closes.clone(), Some(barrier)));
    let references = ["/fake/one", "/fake/two", "/fake/three"].map(|directory| LocationRef::new(directory, None));

    let first = map.get(&references[0]).await.expect("boots the first location");
    let second = map.get(&references[1]).await.expect("boots the second location");
    let third = map.get(&references[2]).await.expect("boots the third location");
    drop(second);
    drop(third);

    tokio::time::timeout(Duration::from_secs(2), map.shutdown_all()).await.expect("closes every location concurrently");
    assert_eq!(closes.load(Ordering::SeqCst), 3);

    let error = map.get(&references[0]).await.err().expect("a closed map refuses boots");
    assert!(matches!(error, BootError::Closed));

    drop(first);
    settle().await;
    assert_eq!(closes.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn warns_when_a_borrower_keeps_an_invalidated_location() {
    install_logger();
    let temp = crate::TempDir::new();
    let directory = temp.path();
    let projects = Arc::new(crate::project_service(database()));
    let closes = Arc::new(AtomicUsize::new(0));
    let map = LocationMap::new(simple_boot(projects, closes, None));
    let reference = LocationRef::new(directory.clone(), None);

    let lease = map.get(&reference).await.expect("boots the location");
    map.invalidate(&reference).await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(30)).await;
    tokio::task::yield_now().await;

    let warnings = warnings_for(&directory);
    assert_eq!(warnings.len(), 1, "warns once: {warnings:?}");
    assert!(warnings[0].contains("location still held after shutdown"));
    assert!(warnings[0].contains(&directory));
    drop(lease);
}

#[tokio::test(start_paused = true)]
async fn does_not_warn_when_the_borrower_releases_on_shutdown() {
    install_logger();
    let temp = crate::TempDir::new();
    let directory = temp.path();
    let projects = Arc::new(crate::project_service(database()));
    let closes = Arc::new(AtomicUsize::new(0));
    let map = LocationMap::new(simple_boot(projects, closes, None));
    let reference = LocationRef::new(directory.clone(), None);

    let lease = map.get(&reference).await.expect("boots the location");
    let services = lease.services().clone();
    let holder = tokio::spawn(async move {
        services.lifecycle().await_shutdown().await;
        drop(lease);
    });
    map.invalidate(&reference).await;
    holder.await.expect("the holder joins");
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(30)).await;
    tokio::task::yield_now().await;

    assert!(warnings_for(&directory).is_empty());
}

#[tokio::test(start_paused = true)]
async fn warns_when_a_held_location_was_released_before() {
    install_logger();
    let temp = crate::TempDir::new();
    let directory = temp.path();
    let projects = Arc::new(crate::project_service(database()));
    let closes = Arc::new(AtomicUsize::new(0));
    let map = LocationMap::new(simple_boot(projects, closes, None));
    let reference = LocationRef::new(directory.clone(), None);

    let released = map.get(&reference).await.expect("boots the location");
    drop(released);
    let lease = map.get(&reference).await.expect("reuses the released location");
    map.invalidate(&reference).await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(30)).await;
    tokio::task::yield_now().await;

    let warnings = warnings_for(&directory);
    assert_eq!(warnings.len(), 1, "warns once: {warnings:?}");
    assert!(warnings[0].contains("location still held after shutdown"));
    assert!(warnings[0].contains(&directory));
    drop(lease);
}

#[tokio::test]
async fn admits_a_workspace_location_without_a_local_directory() {
    let temp = crate::TempDir::new();
    let root = PathBuf::from(temp.path());
    std::fs::create_dir_all(&root).expect("creates the temporary root");
    let directory = root.join("workspace-only").to_string_lossy().into_owned();
    let projects = Arc::new(crate::project_service(database()));
    let closes = Arc::new(AtomicUsize::new(0));
    let environment: Arc<dyn Environment> = Arc::new(LocalEnvironment::new());
    let map = LocationMap::new(filesystem_boot(projects.clone(), environment, closes));
    let workspace_reference = LocationRef::new(directory.clone(), Some("wrk_liveness".to_string()));
    let local_reference = LocationRef::new(directory.clone(), None);

    let workspace = map.get(&workspace_reference).await.expect("admits the workspace location");
    assert_eq!(workspace.services().info.directory, directory);
    assert_eq!(map.keys().await, vec![workspace_reference.canonical()]);

    let error = map.get(&local_reference).await.err().expect("rejects the missing local directory");
    assert!(matches!(error, BootError::DirectoryNotFound(ref path) if path == &directory));
    assert_eq!(map.keys().await, vec![workspace_reference.canonical()]);
    assert_eq!(projects.list(true).await.expect("lists the projects").len(), 1);
}
