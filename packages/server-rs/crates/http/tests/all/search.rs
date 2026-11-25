use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use http::StatusCode;
use opencode_config::paths::GlobalPaths;
use opencode_http::services::GlobalServices;
use opencode_storage::{Database, DatabaseLocation, kv};

use crate::{authorized, ready_app_with_services, temp_directory};

fn config_uri(directory: &Path) -> String {
    let encoded = url::form_urlencoded::byte_serialize(directory.to_string_lossy().as_bytes()).collect::<String>();
    format!("/api/config?location%5Bdirectory%5D={encoded}")
}

fn services_with_flags(
    start_directory: &Path,
    home: PathBuf,
    flags: Option<&str>,
) -> (Arc<GlobalServices>, Arc<Database>) {
    let (database, _) = Database::open(DatabaseLocation::Memory, &std::env::temp_dir()).expect("open the database");
    let database = Arc::new(database);
    let paths =
        GlobalPaths::from_vars(|name| (name == "HOME").then(|| home.clone().into_os_string())).expect("global paths");
    let flags = flags.map(str::to_owned);
    let environment = move |name: &str| match name {
        "OPENCODE_FLAGS" => flags.clone().map(OsString::from),
        _ => None,
    };
    let bus = opencode_bus::Bus::new(database.clone(), opencode_bus::BusOptions::default());
    let services = Arc::new(GlobalServices::new(
        database.clone(),
        paths,
        start_directory.to_string_lossy().into_owned(),
        Arc::new(environment),
        bus,
        crate::global_plugins(),
        "test-version".to_owned(),
        "test".to_owned(),
    ));
    (services, database)
}

fn search_objects(database: &Database) -> Vec<String> {
    database
        .read(|connection| {
            let mut statement =
                connection.prepare("SELECT name FROM sqlite_master WHERE name GLOB 'session_search*'")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .expect("read sqlite_master")
}

fn write_flag(directory: &Path, enabled: bool) {
    std::fs::write(directory.join("opencode.json"), format!(r#"{{"experimental":{{"sqlite_fts":{enabled}}}}}"#))
        .expect("write the project config");
}

async fn wait_for_done(database: &Database) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let state =
                database.read(|connection| kv::get(connection, "session.search")).expect("read the search state");
            if state.as_ref().is_some_and(|value| value["status"] == "done") {
                break state.expect("the search state row");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the search index to finish")
}

#[tokio::test]
async fn the_flag_off_creates_no_search_objects() {
    let start = temp_directory("search-off-start");
    let home = temp_directory("search-off-home");
    let project = temp_directory("search-off-project");
    let (services, database) = services_with_flags(&start, home, Some("-sqlite_fts"));
    let app = ready_app_with_services(services);

    let response = app.handle(authorized("GET", &config_uri(&project))).await;
    assert_eq!(response.status(), StatusCode::OK);

    tokio::time::sleep(Duration::from_millis(200)).await;

    let objects = database
        .read(|connection| {
            let count: i64 = connection.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name GLOB 'session_search*'",
                [],
                |row| row.get(0),
            )?;
            Ok(count)
        })
        .expect("count the search objects");
    assert_eq!(objects, 0);

    let state = database.read(|connection| kv::get(connection, "session.search")).expect("read the search state");
    assert!(state.is_none());
}

#[tokio::test]
async fn location_boot_builds_the_index_when_the_flag_is_on() {
    let start = temp_directory("search-on-start");
    let home = temp_directory("search-on-home");
    let project = temp_directory("search-on-project");
    let (services, database) = services_with_flags(&start, home, Some("sqlite_fts"));
    let app = ready_app_with_services(services);

    let response = app.handle(authorized("GET", &config_uri(&project))).await;
    assert_eq!(response.status(), StatusCode::OK);

    let state = wait_for_done(&database).await;

    let objects = search_objects(&database);
    for name in ["session_search", "session_search_document", "session_search_source"] {
        assert!(objects.iter().any(|object| object == name), "missing the {name} object");
    }
    for trigger in [
        "session_search_message_insert",
        "session_search_message_update",
        "session_search_message_delete",
        "session_search_part_insert",
        "session_search_part_update",
        "session_search_part_delete",
        "session_search_session_insert",
        "session_search_session_update",
        "session_search_session_delete",
    ] {
        assert!(objects.iter().any(|object| object == trigger), "missing the {trigger} trigger");
    }

    assert_eq!(state["layout"], "compact");
    assert_eq!(state["version"], 1);
}

#[tokio::test]
async fn re_enabling_the_flag_starts_the_build() {
    let start = temp_directory("search-reenable-start");
    let home = temp_directory("search-reenable-home");
    let project = temp_directory("search-reenable-project");
    write_flag(&project, false);
    let (services, database) = services_with_flags(&start, home, None);
    let app = ready_app_with_services(services.clone());

    let response = app.handle(authorized("GET", &config_uri(&project))).await;
    assert_eq!(response.status(), StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(search_objects(&database).is_empty());

    write_flag(&project, true);
    let key = services.locations.keys().await.into_iter().next().expect("the location");
    let location = services.locations.services(&key).await.expect("the booted location");
    location.config.reload().await;

    wait_for_done(&database).await;
    assert!(search_objects(&database).iter().any(|name| name == "session_search"));
}

#[tokio::test]
async fn invalidating_a_location_releases_its_config_service() {
    let start = temp_directory("search-release-start");
    let home = temp_directory("search-release-home");
    let project = temp_directory("search-release-project");
    let (services, _database) = services_with_flags(&start, home, Some("sqlite_fts"));
    let app = ready_app_with_services(services.clone());

    let response = app.handle(authorized("GET", &config_uri(&project))).await;
    assert_eq!(response.status(), StatusCode::OK);

    let key = services.locations.keys().await.into_iter().next().expect("the location");
    let location = services.locations.services(&key).await.expect("the booted location");
    let mut updates = location.config.subscribe_updated();
    drop(location);

    services.locations.invalidate(&key).await;

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match updates.recv().await {
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    })
    .await
    .expect("the invalidated location's ConfigService to be released");
}
