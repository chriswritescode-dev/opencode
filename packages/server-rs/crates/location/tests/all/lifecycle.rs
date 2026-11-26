use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use opencode_location::events::LocationEvent;
use opencode_location::lifecycle::Lifecycle;
use opencode_location::location::{LocationInfo, LocationProject, LocationRef};
use opencode_location::project::{ACTIVATE_INTERVAL_MS, ProjectService};
use opencode_storage::{Database, DatabaseLocation};

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

fn active(database: &Database) -> i64 {
    database
        .read(|connection| opencode_storage::project::get(connection, "project"))
        .expect("reads the project")
        .expect("the project exists")
        .time_active
}

#[tokio::test]
async fn shutdown_announces_once_and_await_shutdown_after_it() {
    let projects = Arc::new(crate::project_service(database()));
    let info = info(&LocationRef::new("/fake/lifecycle", Some("wrk".to_string())));
    let mut receiver = projects.subscribe();
    let lifecycle = Lifecycle::start(projects.clone(), &info).await.expect("starts the lifecycle");
    assert!(!lifecycle.is_closed());

    lifecycle.shutdown().await;
    lifecycle.shutdown().await;
    assert!(lifecycle.is_closed());

    tokio::time::timeout(Duration::from_secs(1), lifecycle.await_shutdown())
        .await
        .expect("await_shutdown returns after the completed shutdown");

    let shutdowns: Vec<(String, Option<String>)> = std::iter::from_fn(|| receiver.try_recv().ok())
        .filter_map(|event| match event {
            LocationEvent::LocationShutdown { directory, workspace_id } => Some((directory, workspace_id)),
            _ => None,
        })
        .collect();
    assert_eq!(shutdowns, vec![(info.directory.clone(), info.workspace_id.clone())]);
}

#[tokio::test]
async fn retain_guard_tracks_retention() {
    let projects = Arc::new(crate::project_service(database()));
    let info = info(&LocationRef::new("/fake/retain", None));
    let lifecycle = Lifecycle::start(projects, &info).await.expect("starts the lifecycle");

    assert!(!lifecycle.is_retained());
    let first = lifecycle.retain();
    assert!(lifecycle.is_retained());
    let second = lifecycle.retain();
    assert!(lifecycle.is_retained());
    drop(first);
    assert!(lifecycle.is_retained());
    drop(second);
    assert!(!lifecycle.is_retained());
}

#[tokio::test]
async fn session_event_reactivates_the_project() {
    let database = database();
    database
        .read(|connection| opencode_storage::project::upsert(connection, "project", "/fake/session", None))
        .expect("inserts the project");
    let clock = Arc::new(AtomicI64::new(100_000));
    let projects = Arc::new(ProjectService::with_now(
        database.clone(),
        crate::test_git(),
        Arc::new({
            let clock = clock.clone();
            move || clock.load(Ordering::Relaxed)
        }),
    ));
    let info = info(&LocationRef::new("/fake/session", None));

    let lifecycle = Lifecycle::start(projects, &info).await.expect("starts the lifecycle");
    assert_eq!(active(&database), 100_000);

    clock.fetch_add(ACTIVATE_INTERVAL_MS, Ordering::Relaxed);
    lifecycle.session_event().await.expect("reactivates the project");
    assert_eq!(active(&database), 160_000);
}
