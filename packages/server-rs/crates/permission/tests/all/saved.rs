use std::sync::Arc;

use opencode_contract::generated::PermissionSavedID;
use opencode_permission::saved::{PermissionSaved, SavedError};
use opencode_storage::project;
use opencode_storage::{Database, DatabaseLocation};

fn insert_project(database: &Database, id: &str) {
    let worktree = std::env::temp_dir().join(id).display().to_string();
    database.read(|connection| project::upsert(connection, id, &worktree, None)).expect("inserts the project");
}

fn saved() -> PermissionSaved {
    let (database, _) = Database::open(DatabaseLocation::Memory, &std::env::temp_dir()).expect("opens the database");
    let database = Arc::new(database);
    insert_project(&database, "p1");
    insert_project(&database, "p2");
    PermissionSaved::new(database)
}

#[test]
fn add_without_resources_writes_nothing() {
    let saved = saved();

    saved.add("p1", "read", &[]).expect("adds nothing");

    assert!(saved.list(None).expect("lists").is_empty());
}

#[test]
fn add_inserts_saved_rules_and_list_filters_by_project() {
    let saved = saved();
    let resources = ["src/*".to_string(), "docs/*".to_string()];

    saved.add("p1", "read", &resources).expect("adds the rules");

    let all = saved.list(None).expect("lists every rule");
    assert_eq!(all.len(), 2, "one rule per resource");
    assert!(all.iter().all(|item| item.id.0.starts_with("psv_")), "the rules use the saved permission id prefix");
    assert!(all.iter().all(|item| item.project_id.0 == "p1"));
    assert!(all.iter().all(|item| item.action == "read"));
    assert_eq!(all.iter().map(|item| item.resource.as_str()).collect::<Vec<_>>(), ["src/*", "docs/*"]);
    assert!(all.iter().all(|item| item.time.created.is_finite() && item.time.created != 0.0));
    assert!(all.iter().all(|item| item.time.updated.is_finite() && item.time.updated != 0.0));

    assert_eq!(saved.list(Some("p1")).expect("lists the project"), all);
    assert!(saved.list(Some("p2")).expect("lists another project").is_empty());
}

#[test]
fn update_changes_only_the_resource() {
    let saved = saved();
    saved.add("p1", "read", &["src/*".to_string()]).expect("adds the rule");
    let before = saved.list(None).expect("lists").remove(0);

    let updated = saved.update(&before.id.0, None, Some("src/allowed.ts")).expect("updates the resource");

    assert_eq!(updated.id, before.id);
    assert_eq!(updated.project_id.0, "p1");
    assert_eq!(updated.action, "read", "the action keeps its value");
    assert_eq!(updated.resource, "src/allowed.ts");
    assert_eq!(updated.time.created, before.time.created);
    assert_eq!(saved.list(None).expect("lists the change"), vec![updated]);
}

#[test]
fn update_to_an_existing_resource_conflicts() {
    let saved = saved();
    saved.add("p1", "read", &["src/*".to_string(), "docs/*".to_string()]).expect("adds the rules");
    let src = saved.list(None).expect("lists").into_iter().find(|item| item.resource == "src/*").expect("src rule");

    let error = saved.update(&src.id.0, None, Some("docs/*")).expect_err("conflicts with the other rule");

    let SavedError::Conflict { id, action, resource } = error else {
        panic!("the update conflicts");
    };
    assert_eq!(id, src.id);
    assert_eq!(action, "read");
    assert_eq!(resource, "docs/*");
}

#[test]
fn update_of_a_missing_id_is_not_found() {
    let saved = saved();

    let error = saved.update("psv_missing", Some("edit"), None).expect_err("reports the missing rule");

    let SavedError::NotFound { id } = error else {
        panic!("the update is not found");
    };
    assert_eq!(id, PermissionSavedID("psv_missing".to_string()));
}

#[test]
fn remove_deletes_the_rule_and_a_second_remove_is_harmless() {
    let saved = saved();
    saved.add("p1", "read", &["src/*".to_string()]).expect("adds the rule");
    let id = saved.list(None).expect("lists").remove(0).id;

    saved.remove(&id.0).expect("removes the rule");

    assert!(saved.list(None).expect("lists").is_empty());
    saved.remove(&id.0).expect("removes nothing");
    assert!(saved.list(None).expect("lists").is_empty());
}
