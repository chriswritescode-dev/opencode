use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use opencode_location::activity::{Activity, ActivityOptions, ActivityProbe, IdleTimeout};
use opencode_location::lifecycle::Lifecycle;
use opencode_location::location::{BootError, LocationInfo, LocationProject, LocationRef};
use opencode_location::map::{LocationMap, LocationServices};
use opencode_storage::{Database, DatabaseLocation};

const SWEEP: Duration = Duration::from_secs(1);

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
}

impl LocationServices for TestServices {
    fn lifecycle(&self) -> &Lifecycle {
        &self.lifecycle
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

fn map() -> LocationMap<TestServices> {
    let projects = Arc::new(crate::project_service(database()));
    LocationMap::new(move |reference: LocationRef| -> BoxFuture<'static, Result<Arc<TestServices>, BootError>> {
        let projects = projects.clone();
        Box::pin(async move {
            let info = info(&reference);
            let lifecycle = Lifecycle::start(projects, &info).await?;
            Ok(Arc::new(TestServices { info, lifecycle }))
        })
    })
}

fn options(time_to_live: Duration) -> ActivityOptions<TestServices> {
    ActivityOptions { time_to_live, sweep_interval: SWEEP, ..ActivityOptions::default() }
}

async fn advance(duration: Duration) {
    tokio::task::yield_now().await;
    tokio::time::advance(duration).await;
    for _ in 0..128 {
        tokio::task::yield_now().await;
    }
}

struct TestProbe {
    occupied: Mutex<HashSet<String>>,
}

impl TestProbe {
    fn new(directories: &[&str]) -> Self {
        TestProbe { occupied: Mutex::new(directories.iter().map(|directory| directory.to_string()).collect()) }
    }

    fn set(&self, directory: &str, occupied: bool) {
        let mut entries = self.occupied.lock().expect("the probe lock is not poisoned");
        if occupied {
            entries.insert(directory.to_string());
        } else {
            entries.remove(directory);
        }
    }
}

impl ActivityProbe for TestProbe {
    fn occupied(&self, reference: &LocationRef) -> bool {
        self.occupied.lock().expect("the probe lock is not poisoned").contains(&reference.directory)
    }
}

#[tokio::test(start_paused = true)]
async fn keeps_a_retained_location_and_evicts_it_once_released() {
    let map = map();
    let retained = LocationRef::new("/fake/retained", None);
    let sibling = LocationRef::new("/fake/sibling", None);

    let retained_services = map.get(&retained).await.expect("boots the retained location").services().clone();
    let sibling_lease = map.get(&sibling).await.expect("boots the sibling");
    let hold = retained_services.lifecycle().retain();
    drop(sibling_lease);

    let _activity = Activity::spawn(map.clone(), options(Duration::from_secs(3)));
    advance(SWEEP).await;
    advance(Duration::from_secs(3)).await;

    assert!(!map.has(&sibling).await);
    assert!(map.has(&retained).await);
    assert!(!retained_services.lifecycle().is_closed());

    drop(hold);
    advance(Duration::from_secs(3)).await;

    assert!(!map.has(&retained).await);
    assert!(retained_services.lifecycle().is_closed());
}

#[tokio::test(start_paused = true)]
async fn keeps_a_location_an_activity_probe_reports_occupied() {
    let map = map();
    let occupied = LocationRef::new("/fake/occupied", None);
    let idle = LocationRef::new("/fake/idle", None);

    let occupied_services = map.get(&occupied).await.expect("boots the occupied location").services().clone();
    let idle_lease = map.get(&idle).await.expect("boots the idle location");
    drop(idle_lease);

    let probe = Arc::new(TestProbe::new(&["/fake/occupied"]));
    let mut options = options(Duration::from_secs(3));
    options.probe = Some(probe.clone());
    let _activity = Activity::spawn(map.clone(), options);

    advance(SWEEP).await;
    advance(Duration::from_secs(3)).await;

    assert!(!map.has(&idle).await);
    assert!(map.has(&occupied).await);
    assert!(!occupied_services.lifecycle().is_closed());

    probe.set("/fake/occupied", false);
    advance(Duration::from_secs(3)).await;

    assert!(!map.has(&occupied).await);
    assert!(occupied_services.lifecycle().is_closed());
}

#[tokio::test(start_paused = true)]
async fn honors_a_per_location_idle_timeout() {
    let map = map();
    let configured = LocationRef::new("/fake/project", Some("wrk_fast".to_string()));
    let fallback = LocationRef::new("/fake/project", None);

    let configured_lease = map.get(&configured).await.expect("boots the configured location");
    let fallback_lease = map.get(&fallback).await.expect("boots the fallback location");
    drop(configured_lease);
    drop(fallback_lease);

    let mut options = options(Duration::from_secs(6));
    options.idle_timeout = Box::new(|services: &TestServices| {
        (services.info.workspace_id.as_deref() == Some("wrk_fast"))
            .then_some(IdleTimeout::After(Duration::from_secs(3)))
    });
    let _activity = Activity::spawn(map.clone(), options);

    advance(SWEEP).await;
    advance(Duration::from_secs(4)).await;

    assert!(!map.has(&configured).await);
    assert!(map.has(&fallback).await);

    advance(Duration::from_secs(3)).await;

    assert!(!map.has(&fallback).await);
}

#[tokio::test(start_paused = true)]
async fn honors_a_disabled_idle_timeout() {
    let map = map();
    let configured = LocationRef::new("/fake/project", Some("wrk_hold".to_string()));
    let fallback = LocationRef::new("/fake/project", None);

    let configured_lease = map.get(&configured).await.expect("boots the configured location");
    let fallback_lease = map.get(&fallback).await.expect("boots the fallback location");
    drop(configured_lease);
    drop(fallback_lease);

    let mut options = options(Duration::from_secs(6));
    options.idle_timeout = Box::new(|services: &TestServices| {
        (services.info.workspace_id.as_deref() == Some("wrk_hold")).then_some(IdleTimeout::Disabled)
    });
    let _activity = Activity::spawn(map.clone(), options);

    advance(SWEEP).await;
    advance(Duration::from_secs(10)).await;

    assert!(map.has(&configured).await);
    assert!(!map.has(&fallback).await);

    advance(Duration::from_secs(10)).await;

    assert!(map.has(&configured).await);
}

#[tokio::test(start_paused = true)]
async fn touch_refreshes_only_cached_locations() {
    let map = map();
    let refreshed = LocationRef::new("/fake/refreshed", None);
    let sibling = LocationRef::new("/fake/sibling", None);
    let uncached = LocationRef::new("/fake/uncached", None);

    let refreshed_lease = map.get(&refreshed).await.expect("boots the refreshed location");
    let sibling_lease = map.get(&sibling).await.expect("boots the sibling");
    drop(refreshed_lease);
    drop(sibling_lease);

    let activity = Activity::spawn(map.clone(), options(Duration::from_secs(4)));
    advance(SWEEP).await;
    advance(Duration::from_secs(2)).await;

    activity.touch(&LocationRef::new("/fake/refreshed/", None)).await;
    activity.touch(&uncached).await;

    assert!(!map.has(&uncached).await);

    advance(Duration::from_secs(2)).await;

    assert!(map.has(&refreshed).await);
    assert!(!map.has(&sibling).await);

    advance(Duration::from_secs(2)).await;

    assert!(!map.has(&refreshed).await);
}
