use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use opencode_bus::{Bus, BusOptions, Event, ListenerHandle};
use opencode_catalog::credential::{CreateInput, CredentialService, Update, expiring};
use opencode_config::seams::CredentialLookup;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    CredentialExternal, CredentialExternalType, CredentialID, CredentialKey, CredentialKeyType, CredentialOAuth,
    CredentialOAuthType, CredentialValue, IntegrationID, IntegrationMethodID,
};
use opencode_storage::{Database, DatabaseLocation};
use rusqlite::params;
use serde_json::{Value, json};

use crate::TempDir;

const UPDATED: &str = "credential.updated";
const SWITCHED: &str = "credential.switched";

fn setup() -> (TempDir, Arc<Database>, Arc<Bus>, Arc<CredentialService>) {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus = Bus::new(database.clone(), BusOptions::default());
    let service = CredentialService::new(database.clone(), bus.clone());
    (dir, database, bus, service)
}

fn key(text: &str) -> CredentialValue {
    CredentialValue::Key(CredentialKey { r#type: CredentialKeyType::Key, key: text.to_owned(), metadata: None })
}

fn create_input(integration: &str, value: CredentialValue) -> CreateInput {
    CreateInput { id: None, integration_id: IntegrationID(integration.to_owned()), value, label: None, activate: None }
}

fn key_credential(integration: &str, text: &str) -> CreateInput {
    create_input(integration, key(text))
}

fn capture(bus: &Bus) -> (Arc<Mutex<Vec<Event>>>, ListenerHandle) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let handle = bus.listen(Box::new(move |event: &Event| {
        sink.lock().expect("lock the events").push(event.clone());
        Ok(())
    }));
    (events, handle)
}

fn captured(events: &Arc<Mutex<Vec<Event>>>) -> Vec<(String, Value)> {
    events
        .lock()
        .expect("lock the events")
        .iter()
        .map(|event| (event.r#type().to_owned(), event.data().clone()))
        .collect()
}

fn event(r#type: &str, credential_id: Option<&str>) -> (String, Value) {
    (r#type.to_owned(), json!({ "integrationID": "openai", "credentialID": credential_id }))
}

fn missing_id() -> CredentialID {
    CredentialID(format!("cred_{}", opencode_storage::id::ascending()))
}

fn oauth(expires: i64) -> CredentialOAuth {
    CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: IntegrationMethodID("chatgpt-browser".to_owned()),
        refresh: "refresh".to_owned(),
        access: "access".to_owned(),
        expires: expires as f64,
        metadata: None,
    }
}

#[test]
fn stores_updates_lists_and_removes_credentials() {
    let (_dir, _database, _bus, service) = setup();
    let integration = IntegrationID("openai".to_owned());
    let created = service
        .create(CreateInput {
            id: None,
            integration_id: integration.clone(),
            value: key("secret"),
            label: Some("Work".to_owned()),
            activate: None,
        })
        .expect("create the first credential");

    assert_eq!(service.list(&integration).expect("list the credentials"), vec![created.clone()]);
    service.update(&created.id, Update { label: Some("Personal".to_owned()), value: None }).expect("update the label");
    assert_eq!(service.list(&integration).expect("list the credentials")[0].label, "Personal");

    let additional = service
        .create(CreateInput {
            id: None,
            integration_id: integration.clone(),
            value: key("additional"),
            label: Some("Additional".to_owned()),
            activate: None,
        })
        .expect("create the additional credential");
    let listed = service.list(&integration).expect("list the credentials");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, created.id);
    assert_eq!(listed[0].label, "Personal");
    assert_eq!(listed[1], additional);

    service.remove(&additional.id).expect("remove the additional credential");
    let listed = service.list(&integration).expect("list the credentials");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);
    assert_eq!(listed[0].label, "Personal");
}

#[test]
fn reads_key_configuration_as_metadata() {
    let (_dir, database, _bus, service) = setup();
    let legacy = json!({
        "type": "key",
        "key": "secret",
        "metadata": { "imported": true, "resourceName": "older" },
        "configuration": { "resourceName": "answered" }
    });
    database
        .read(|connection| {
            connection.execute(
                "INSERT INTO credential (id, integration_id, label, value, active, time_created, time_updated) VALUES ('cred_legacy', ?1, 'Legacy', ?2, 1, 0, 0)",
                params!["azure", serde_json::to_string(&legacy).expect("serialize the legacy credential")],
            )?;
            Ok(())
        })
        .expect("insert the legacy credential");

    let info = service
        .get(&CredentialID("cred_legacy".to_owned()))
        .expect("get the legacy credential")
        .expect("the legacy credential exists");
    assert_eq!(info.integration_id, IntegrationID("azure".to_owned()));
    let CredentialValue::Key(key) = info.value else {
        panic!("expected a key credential");
    };
    assert_eq!(key.key, "secret");
    let metadata = key.metadata.expect("the legacy configuration became metadata");
    let metadata: BTreeMap<_, _> = metadata.0.into_iter().collect();
    assert_eq!(metadata.get("imported"), Some(&json!(true)));
    assert_eq!(metadata.get("resourceName"), Some(&json!("answered")));
}

#[test]
fn publishes_global_events_only_for_observable_mutations() {
    let (_dir, _database, bus, service) = setup();
    let integration = IntegrationID("openai".to_owned());
    let (events, _handle) = capture(&bus);

    let older = service.create(key_credential("openai", "older")).expect("create the older credential");
    let newer = service.create(key_credential("openai", "newer")).expect("create the newer credential");

    service.activate(&newer.id).expect("activate the newer credential");
    service.activate(&missing_id()).expect("activate a missing credential");
    service.activate(&older.id).expect("activate the older credential");
    service.activate(&older.id).expect("activate the older credential again");
    service.update(&older.id, Update::default()).expect("apply an empty update");
    service
        .update(&missing_id(), Update { label: Some("Missing".to_owned()), value: None })
        .expect("update a missing credential");
    service
        .update(&older.id, Update { label: Some("default".to_owned()), value: None })
        .expect("update the unchanged default label");
    service.update(&older.id, Update { label: None, value: Some(key("refreshed")) }).expect("update only the value");
    service
        .update(&older.id, Update { label: Some("Renamed".to_owned()), value: None })
        .expect("rename the credential");
    service.remove(&missing_id()).expect("remove a missing credential");
    service.remove(&newer.id).expect("remove the inactive newer credential");
    service.remove(&newer.id).expect("remove the newer credential again");
    assert_eq!(service.list(&integration).expect("list the credentials").last().map(|info| &info.id), Some(&older.id));

    let replacement = service.create(key_credential("openai", "replacement")).expect("create the replacement");
    service.remove(&replacement.id).expect("remove the replacement");
    assert_eq!(service.list(&integration).expect("list the credentials").last().map(|info| &info.id), Some(&older.id));
    service.remove(&older.id).expect("remove the older credential");
    assert!(service.list(&integration).expect("list the credentials").is_empty());

    let expected = vec![
        event(UPDATED, Some(older.id.0.as_str())),
        event(SWITCHED, Some(older.id.0.as_str())),
        event(UPDATED, Some(newer.id.0.as_str())),
        event(SWITCHED, Some(newer.id.0.as_str())),
        event(SWITCHED, Some(older.id.0.as_str())),
        event(UPDATED, Some(older.id.0.as_str())),
        event(UPDATED, Some(newer.id.0.as_str())),
        event(UPDATED, Some(replacement.id.0.as_str())),
        event(SWITCHED, Some(replacement.id.0.as_str())),
        event(UPDATED, Some(replacement.id.0.as_str())),
        event(SWITCHED, Some(older.id.0.as_str())),
        event(UPDATED, Some(older.id.0.as_str())),
        event(SWITCHED, None),
    ];
    assert_eq!(captured(&events), expected);
    assert!(events.lock().expect("lock the events").iter().all(|event| !event.payload().contains_key("location")));
}

#[test]
fn promotes_the_newest_remaining_legacy_credential_when_the_effective_selection_is_removed() {
    let (_dir, database, bus, service) = setup();
    let integration = IntegrationID("openai".to_owned());
    let oldest = service.create(key_credential("openai", "oldest")).expect("create the oldest credential");
    let newer = service.create(key_credential("openai", "newer")).expect("create the newer credential");
    let newest = service.create(key_credential("openai", "newest")).expect("create the newest credential");
    database
        .read(|connection| {
            connection.execute("UPDATE credential SET active = NULL WHERE integration_id = ?1", params!["openai"])?;
            Ok(())
        })
        .expect("clear the active flags");

    let (events, _handle) = capture(&bus);
    service.activate(&newest.id).expect("activate the newest credential");
    service.remove(&oldest.id).expect("remove the oldest credential");
    service.remove(&newest.id).expect("remove the newest credential");

    let listed = service.list(&integration).expect("list the credentials");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, newer.id);
    let active = database
        .read(|connection| {
            Ok(connection.query_row("SELECT active FROM credential WHERE id = ?1", params![newer.id.0], |row| {
                row.get::<_, Option<bool>>(0)
            })?)
        })
        .expect("read the active flag");
    assert_eq!(active, Some(true));

    let expected = vec![
        event(UPDATED, Some(oldest.id.0.as_str())),
        event(UPDATED, Some(newest.id.0.as_str())),
        event(SWITCHED, Some(newer.id.0.as_str())),
    ];
    assert_eq!(captured(&events), expected);
}

#[test]
fn activates_older_credentials_without_affecting_other_integrations() {
    let (_dir, _database, _bus, service) = setup();
    let integration = IntegrationID("openai".to_owned());
    let other_integration = IntegrationID("anthropic".to_owned());
    let older = service.create(key_credential("openai", "older")).expect("create the older credential");
    let newer = service.create(key_credential("openai", "newer")).expect("create the newer credential");
    let other_older = service.create(key_credential("anthropic", "other-older")).expect("create the other older");
    let other_newer = service.create(key_credential("anthropic", "other-newer")).expect("create the other newer");

    service.activate(&older.id).expect("activate the older credential");
    let listed = service.list(&integration).expect("list the credentials");
    assert_eq!(listed.iter().map(|info| &info.id).collect::<Vec<_>>(), vec![&newer.id, &older.id]);
    let other_listed = service.list(&other_integration).expect("list the other credentials");
    assert_eq!(other_listed.iter().map(|info| &info.id).collect::<Vec<_>>(), vec![&other_older.id, &other_newer.id]);

    service.activate(&missing_id()).expect("activate a missing credential");
    assert_eq!(service.list(&integration).expect("list the credentials").last().map(|info| &info.id), Some(&older.id));

    service.activate(&other_older.id).expect("activate the other older credential");
    let other_listed = service.list(&other_integration).expect("list the other credentials");
    assert_eq!(other_listed.iter().map(|info| &info.id).collect::<Vec<_>>(), vec![&other_newer.id, &other_older.id]);
    assert_eq!(service.list(&integration).expect("list the credentials").last().map(|info| &info.id), Some(&older.id));

    service.remove(&older.id).expect("remove the older credential");
    assert_eq!(service.list(&integration).expect("list the credentials").last().map(|info| &info.id), Some(&newer.id));
    assert_eq!(
        service.list(&other_integration).expect("list the other credentials").last().map(|info| &info.id),
        Some(&other_older.id)
    );
}

#[test]
fn creates_with_a_requested_id_without_taking_over_the_current_selection() {
    let (_dir, database, bus, service) = setup();
    let integration = IntegrationID("openai".to_owned());
    let (events, _handle) = capture(&bus);

    let first = service
        .create(CreateInput {
            id: Some(CredentialID("cred_0000imported".to_owned())),
            integration_id: integration.clone(),
            value: key("first"),
            label: None,
            activate: Some(false),
        })
        .expect("create the requested credential");
    assert_eq!(first.id, CredentialID("cred_0000imported".to_owned()));
    assert_eq!(service.list(&integration).expect("list the credentials").last().map(|info| &info.id), Some(&first.id));

    let legacy = service.create(key_credential("openai", "legacy")).expect("create the legacy credential");
    database
        .read(|connection| {
            connection.execute("UPDATE credential SET active = NULL WHERE integration_id = ?1", params!["openai"])?;
            Ok(())
        })
        .expect("clear the active flags");
    let imported = service
        .create(CreateInput {
            id: None,
            integration_id: integration.clone(),
            value: key("imported"),
            label: None,
            activate: Some(false),
        })
        .expect("import the credential without selecting it");

    let listed = service.list(&integration).expect("list the credentials");
    assert_eq!(listed.iter().map(|info| &info.id).collect::<Vec<_>>(), vec![&first.id, &imported.id, &legacy.id]);

    let expected = vec![
        event(UPDATED, Some(first.id.0.as_str())),
        event(SWITCHED, Some(first.id.0.as_str())),
        event(UPDATED, Some(legacy.id.0.as_str())),
        event(SWITCHED, Some(legacy.id.0.as_str())),
        event(UPDATED, Some(imported.id.0.as_str())),
    ];
    assert_eq!(captured(&events), expected);
}

#[test]
fn lists_credentials_imported_from_auth_json() {
    let dir = TempDir::new();
    std::fs::write(dir.path().join("auth.json"), r#"{"openai":{"type":"api","key":"secret"}}"#)
        .expect("write auth.json");
    let (database, _) =
        Database::open(DatabaseLocation::File(dir.path().join("opencode.db")), dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus = Bus::new(database.clone(), BusOptions::default());
    let service = CredentialService::new(database, bus);

    let all = service.all().expect("list every credential");
    assert_eq!(all.len(), 1);
    let info = &all[0];
    assert_eq!(info.integration_id, IntegrationID("openai".to_owned()));
    assert_eq!(info.label, "API key");
    let CredentialValue::Key(key) = &info.value else {
        panic!("expected a key credential");
    };
    assert_eq!(key.key, "secret");
}

#[test]
fn expiring_detects_tokens_within_the_refresh_margin() {
    let now = 1_700_000_000_000;
    assert!(expiring(&oauth(now + 4 * 60_000), now));
    assert!(!expiring(&oauth(now + 6 * 60_000), now));
    assert!(!expiring(&oauth(0), now));
}

#[test]
fn credential_lookup_exposes_a_key_only_for_key_credentials() {
    let (_dir, _database, _bus, service) = setup();

    let key_info = service
        .create(CreateInput {
            id: None,
            integration_id: IntegrationID("openai".to_owned()),
            value: CredentialValue::Key(CredentialKey {
                r#type: CredentialKeyType::Key,
                key: "secret".to_owned(),
                metadata: Some(JsonMap(vec![("orgID".to_owned(), json!("org"))])),
            }),
            label: None,
            activate: None,
        })
        .expect("create the key credential");
    let latest = CredentialLookup::latest(&*service, "openai").expect("the key credential is selected");
    assert_eq!(latest.key.as_deref(), Some("secret"));
    assert_eq!(latest.metadata.get("orgID"), Some(&json!("org")));
    assert_eq!(CredentialLookup::get(&*service, &key_info.id.0), Some(latest));

    service
        .create(CreateInput {
            id: None,
            integration_id: IntegrationID("anthropic".to_owned()),
            value: CredentialValue::Oauth(CredentialOAuth {
                r#type: CredentialOAuthType::Oauth,
                method_id: IntegrationMethodID("oauth".to_owned()),
                refresh: "refresh".to_owned(),
                access: "access".to_owned(),
                expires: 0.0,
                metadata: Some(JsonMap(vec![("accountID".to_owned(), json!("account"))])),
            }),
            label: None,
            activate: None,
        })
        .expect("create the oauth credential");
    let latest = CredentialLookup::latest(&*service, "anthropic").expect("the oauth credential is selected");
    assert_eq!(latest.key, None);
    assert_eq!(latest.metadata.get("accountID"), Some(&json!("account")));

    service
        .create(CreateInput {
            id: None,
            integration_id: IntegrationID("external".to_owned()),
            value: CredentialValue::External(CredentialExternal {
                r#type: CredentialExternalType::External,
                method_id: IntegrationMethodID("oauth".to_owned()),
                metadata: Some(JsonMap(vec![("orgID".to_owned(), json!("external-org"))])),
            }),
            label: None,
            activate: None,
        })
        .expect("create the external credential");
    let latest = CredentialLookup::latest(&*service, "external").expect("the external credential is selected");
    assert_eq!(latest.key, None);
    assert_eq!(latest.metadata.get("orgID"), Some(&json!("external-org")));
}

#[test]
fn credential_lookup_latest_follows_the_active_selection() {
    let (_dir, _database, _bus, service) = setup();
    let older = service.create(key_credential("openai", "older")).expect("create the older credential");
    service.create(key_credential("openai", "newer")).expect("create the newer credential");
    assert_eq!(
        CredentialLookup::latest(&*service, "openai").expect("the newer credential is selected").key.as_deref(),
        Some("newer")
    );

    service.activate(&older.id).expect("activate the older credential");
    assert_eq!(
        CredentialLookup::latest(&*service, "openai").expect("the older credential is selected").key.as_deref(),
        Some("older")
    );
}
