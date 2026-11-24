use std::sync::Arc;

use futures_util::future::BoxFuture;
use opencode_location::environment::Environment;
use opencode_location::environment::memory::MemoryEnvironment;
use opencode_location::workspace::{WorkspaceDriver, WorkspaceError, Workspaces};
use opencode_storage::{Database, DatabaseLocation, workspace};
use serde_json::{Value, json};

struct StubDriver;

impl WorkspaceDriver for StubDriver {
    fn connect<'a>(
        &'a self,
        id: &'a str,
        binding: &'a Value,
    ) -> BoxFuture<'a, Result<Arc<dyn Environment>, WorkspaceError>> {
        let environment: Arc<dyn Environment> = Arc::new(MemoryEnvironment::new(id));
        let _ = binding;
        Box::pin(async move { Ok(environment) })
    }
}

fn database() -> Database {
    let (database, _) =
        Database::open(DatabaseLocation::Memory, std::env::temp_dir().as_path()).expect("open the database");
    database
}

fn reset_last_used_at(database: &Database, id: &str) {
    database
        .read(|connection| {
            connection.execute("UPDATE workspace SET last_used_at = 1 WHERE id = ?1", [id])?;
            Ok(())
        })
        .expect("reset last_used_at");
}

fn last_used_at(database: &Database, id: &str) -> i64 {
    database
        .read(|connection| workspace::get(connection, id))
        .expect("read the workspace")
        .expect("the workspace is present")
        .last_used_at
}

#[tokio::test]
async fn connect_returns_the_environment_and_touches_the_row() {
    let database = database();
    let binding = json!({ "key": "value" });
    database
        .read(|connection| workspace::insert(connection, "w1", "stub", Some(&binding)))
        .expect("insert the workspace");
    reset_last_used_at(&database, "w1");
    let mut workspaces = Workspaces::new();
    workspaces.register("stub", Arc::new(StubDriver));

    let environment = workspaces.connect(&database, "w1").await.expect("connect the workspace");

    assert_eq!(environment.id(), "w1");
    assert!(last_used_at(&database, "w1") > 1, "a usable connection touches the workspace");
}

#[tokio::test]
async fn connect_fails_when_the_workspace_is_missing() {
    let database = database();
    let workspaces = Workspaces::new();

    let error = workspaces.connect(&database, "missing").await.err().expect("fail for a missing workspace");

    assert!(matches!(error, WorkspaceError::NotFound(id) if id == "missing"));
}

#[tokio::test]
async fn connect_fails_when_the_provider_is_not_registered() {
    let database = database();
    database.read(|connection| workspace::insert(connection, "w1", "other", None)).expect("insert the workspace");
    let workspaces = Workspaces::new();

    let error = workspaces.connect(&database, "w1").await.err().expect("fail for an unknown provider");

    assert!(matches!(error, WorkspaceError::ProviderNotFound(provider) if provider == "other"));
}

#[tokio::test]
async fn connect_fails_without_a_binding_and_does_not_touch_the_row() {
    let database = database();
    database.read(|connection| workspace::insert(connection, "w1", "stub", None)).expect("insert the workspace");
    reset_last_used_at(&database, "w1");
    let mut workspaces = Workspaces::new();
    workspaces.register("stub", Arc::new(StubDriver));

    let error = workspaces.connect(&database, "w1").await.err().expect("fail without a binding");

    assert!(matches!(error, WorkspaceError::MissingBinding(id) if id == "w1"));
    assert_eq!(last_used_at(&database, "w1"), 1, "a failed connection leaves last_used_at alone");
}
