use std::sync::{Arc, Mutex};

use opencode_bus::{Bus, BusOptions, Event};
use opencode_session::ProjectionSettings;
use opencode_session::SessionProjector;
use opencode_session::instruction_state;
use opencode_session::instructions::{self, BuiltinsInput, Observed, Source};
use opencode_storage::{Database, DatabaseLocation};
use opencode_util::time::Zone;
use serde_json::{Value, json};

use crate::TempDir;

fn setup() -> (TempDir, Arc<Database>, Arc<Bus>) {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus =
        Bus::new(database.clone(), BusOptions { persist: true, now: Some(Arc::new(|| 0i64)), ..Default::default() });
    SessionProjector::register(&bus, Arc::new(ProjectionSettings::default));
    database
        .read(|connection| {
            connection.execute(
                "INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) \
                 VALUES ('prj_1', '/repo', '[]', 1, 2)",
                [],
            )?;
            connection.execute(
                "INSERT INTO session_v2 (id, project_id, slug, directory, version, time_created, time_updated) \
                 VALUES ('ses_1', 'prj_1', 'slug-1', '/repo', '1', 1000, 1000)",
                [],
            )?;
            Ok(())
        })
        .expect("seed the session");
    (dir, database, bus)
}

fn capture(bus: &Bus) -> (Arc<Mutex<Vec<Event>>>, opencode_bus::ListenerHandle) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let listener = bus.listen(Box::new(move |event: &Event| {
        if event.r#type() == "session.instructions.updated" {
            sink.lock().expect("lock the events").push(event.clone());
        }
        Ok(())
    }));
    (events, listener)
}

fn date_source(value: Arc<Mutex<String>>) -> Source {
    instructions::text_source(
        "core/date",
        move || Observed::Value(Value::String(value.lock().expect("lock the date").clone())),
        |date| format!("Today's date: {date}"),
        |_previous, date| Some(format!("Today's date is now: {date}")),
    )
}

fn environment_source() -> Source {
    instructions::text_source(
        "core/environment",
        || Observed::Value(Value::String("<env>\n</env>".to_string())),
        |environment| {
            format!("Here is some useful information about the environment you are running in:\n{environment}")
        },
        |_previous, environment| Some(format!("The environment you are running in is now:\n{environment}")),
    )
}

fn journal_events(database: &Database) -> i64 {
    database
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT COUNT(*) FROM event WHERE type = 'session.instructions.updated.2'",
                [],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .expect("count the instruction events")
}

fn count(database: &Database, table: &str) -> i64 {
    database
        .read(|connection| {
            Ok(connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get::<_, i64>(0))?)
        })
        .expect("count the rows")
}

#[test]
fn hash_matches_typescript_vectors() {
    assert_eq!(
        instructions::hash(&json!("Tue Nov 24 2026")),
        "1a2867dfb6d20b9333899d1a2f3ebf58f6df1641a8451a6a55aa1da85d55154c"
    );
    assert_eq!(
        instructions::hash(&json!({ "a": 1, "b": { "x": true, "y": false } })),
        "21dc66ac9fce28900235d1cf33c48a7d131c80022dfa54990c7ef3070e926210"
    );
}

#[test]
fn canonical_json_sorts_keys() {
    assert_eq!(
        instructions::canonical_json(&json!({ "b": { "y": false, "x": true }, "a": 1 })),
        r#"{"a":1,"b":{"x":true,"y":false}}"#
    );
}

#[test]
fn first_prepare_publishes_initial_then_second_is_quiet() {
    let (_dir, database, bus) = setup();
    let (events, _listener) = capture(&bus);
    let date = Arc::new(Mutex::new("Tue Nov 24 2026".to_string()));
    let list = vec![date_source(date), environment_source()];

    instruction_state::prepare(&database, &bus, &list, "ses_1").expect("prepare the initial instructions");

    let published = events.lock().expect("lock the events").clone();
    assert_eq!(published.len(), 1);
    let initial = &published[0];
    assert_eq!(initial.payload().get("metadata"), Some(&json!({ "instructions": { "initial": true } })));
    assert!(initial.data().get("text").is_none(), "the baseline carries no chronological text");
    let delta = initial.data()["delta"].as_object().expect("the delta is an object");
    assert_eq!(delta.len(), 2);
    assert!(delta.contains_key("core/date"));
    assert!(delta.contains_key("core/environment"));
    assert_eq!(count(&database, "instruction_blob"), 2);
    assert_eq!(count(&database, "instruction_state"), 1);

    instruction_state::prepare(&database, &bus, &list, "ses_1").expect("prepare the unchanged instructions");

    assert_eq!(events.lock().expect("lock the events").len(), 1);
    assert_eq!(journal_events(&database), 1);
    assert_eq!(count(&database, "instruction_blob"), 2);
    assert_eq!(count(&database, "instruction_state"), 1);
}

#[test]
fn prepare_renders_update_text() {
    let (_dir, database, bus) = setup();
    let (events, _listener) = capture(&bus);
    let date = Arc::new(Mutex::new("Tue Nov 24 2026".to_string()));
    let list = vec![date_source(date.clone()), environment_source()];

    instruction_state::prepare(&database, &bus, &list, "ses_1").expect("prepare the initial instructions");
    *date.lock().expect("lock the date") = "Wed Nov 25 2026".to_string();
    instruction_state::prepare(&database, &bus, &list, "ses_1").expect("prepare the changed instructions");

    let published = events.lock().expect("lock the events").clone();
    assert_eq!(published.len(), 2);
    let update = &published[1];
    assert!(update.payload().get("metadata").is_none(), "the update carries no initial metadata");
    assert_eq!(update.data()["text"], json!("Today's date is now: Wed Nov 25 2026"));
    let delta = update.data()["delta"].as_object().expect("the delta is an object");
    assert_eq!(delta.len(), 1);
    assert_eq!(delta["core/date"], json!(instructions::hash(&json!("Wed Nov 25 2026"))));
    assert_eq!(journal_events(&database), 2);
    assert_eq!(count(&database, "instruction_blob"), 3);

    let current = database
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT current_values FROM instruction_state WHERE session_id = 'ses_1'",
                [],
                |row| row.get::<_, String>(0),
            )?)
        })
        .expect("read the projected state");
    let current: Value = serde_json::from_str(&current).expect("parse the projected state");
    assert_eq!(current["core/date"], json!(instructions::hash(&json!("Wed Nov 25 2026"))));
    assert_eq!(current["core/environment"], json!(instructions::hash(&json!("<env>\n</env>"))));
}

#[test]
fn builtins_initial_matches_typescript_shape() {
    let fixed_now = 1_780_000_000_000i64;
    let dir = TempDir::new();
    let (directory, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(directory);
    let bus =
        Bus::new(database.clone(), BusOptions { persist: true, now: Some(Arc::new(|| 0i64)), ..Default::default() });
    SessionProjector::register(&bus, Arc::new(ProjectionSettings::default));
    database
        .read(|connection| {
            connection.execute(
                "INSERT INTO project (id, worktree, sandboxes, time_created, time_updated) \
                 VALUES ('prj_1', '/repo', '[]', 1, 2)",
                [],
            )?;
            connection.execute(
                "INSERT INTO session_v2 (id, project_id, slug, directory, version, time_created, time_updated) \
                 VALUES ('ses_1', 'prj_1', 'slug-1', '/repo', '1', 1000, 1000)",
                [],
            )?;
            Ok(())
        })
        .expect("seed the session");
    let tmp = dir.path().to_string_lossy().into_owned();
    let list = instructions::builtins_at(
        BuiltinsInput {
            directory: "/repo/packages/core".to_string(),
            workspace_root: "/repo".to_string(),
            vcs: Some("git".to_string()),
            tmp: tmp.clone(),
        },
        fixed_now,
    )
    .expect("build the builtin instructions");

    instruction_state::prepare(&database, &bus, &list, "ses_1").expect("prepare the builtin instructions");

    let date = Zone::Local.at(fixed_now).expect("the fixed instant has a local date").date_string();
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let expected = format!(
        "Today's date: {date}\n\nHere is some useful information about the environment you are running in:\n<env>\n  \
         Working directory: /repo/packages/core\n  Workspace root folder: /repo\n  Is directory a git repo: \
         yes\n  Platform: {platform}\n  Prefer {tmp} over generic system temporary directories such as /tmp; it is \
         pre-created and approved for external access.\n</env>"
    );
    let rendered = instruction_state::initial(&database, "ses_1", &list).expect("read the initial instructions");
    assert_eq!(instructions::join(&rendered), expected);
}
