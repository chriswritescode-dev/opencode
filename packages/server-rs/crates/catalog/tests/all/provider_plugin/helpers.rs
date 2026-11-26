use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_bus::{Definition, PublishOptions};
use opencode_catalog::provider_plugin::refresh::{self, Loaded, Start};
use opencode_catalog::provider_plugin::settings;
use opencode_catalog::provider_plugin::storage::Storage;
use opencode_config::entry::{Document, Entry, Origin};
use opencode_config::seams::{BoxFuture, KvStore};
use opencode_contract::codec::{self, JsonMap};
use opencode_contract::generated::{
    ConfigEntryDocumentInfo, ConnectionCredentialInfo, ConnectionCredentialInfoMethod, ConnectionCredentialInfoType,
    ConnectionInfo, CredentialID, CredentialKey, CredentialKeyType, CredentialValue, IntegrationID,
    ProviderSettingsTimeout, ProviderSettingsTimeoutNumber,
};
use serde_json::{Map, Value, json};
use tokio::sync::{Notify, watch};

use crate::builtin::{Harness, Input};

#[derive(Default)]
pub(crate) struct MemoryKv {
    values: Mutex<Map<String, Value>>,
}

impl MemoryKv {
    fn values(&self) -> Map<String, Value> {
        self.values.lock().expect("the test kv lock is never poisoned").clone()
    }
}

impl KvStore for MemoryKv {
    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Value>> {
        Box::pin(async move { self.values.lock().expect("the test kv lock is never poisoned").get(key).cloned() })
    }

    fn set<'a>(&'a self, key: &'a str, value: Value) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.values.lock().expect("the test kv lock is never poisoned").insert(key.to_owned(), value);
        })
    }

    fn remove<'a>(&'a self, key: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.values.lock().expect("the test kv lock is never poisoned").remove(key);
        })
    }

    fn scan<'a>(&'a self, prefix: &'a str, limit: usize) -> BoxFuture<'a, Vec<(String, Value)>> {
        Box::pin(async move {
            let values = self.values.lock().expect("the test kv lock is never poisoned");
            let mut entries: Vec<(String, Value)> = values
                .iter()
                .filter(|(key, _)| key.starts_with(prefix))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            entries.truncate(limit);
            entries
        })
    }
}

pub(crate) struct TestClock {
    now_ms: AtomicU64,
}

impl TestClock {
    pub fn new() -> Self {
        Self { now_ms: AtomicU64::new(1_700_000_000_000) }
    }

    pub fn advance(&self, duration: Duration) {
        self.now_ms.fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
    }
}

impl opencode_ai::clock::Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

fn connection(id: &str) -> ConnectionInfo {
    ConnectionInfo::Credential(ConnectionCredentialInfo {
        r#type: ConnectionCredentialInfoType::Credential,
        id: CredentialID(id.to_owned()),
        label: id.to_owned(),
        method: ConnectionCredentialInfoMethod::Key,
        status: None,
    })
}

fn key_credential(key: &str, base_url: &str) -> CredentialValue {
    CredentialValue::Key(CredentialKey {
        r#type: CredentialKeyType::Key,
        key: key.to_owned(),
        metadata: Some(JsonMap(vec![("baseURL".to_owned(), json!(base_url))])),
    })
}

#[tokio::test]
async fn storage_namespaces_keys_per_plugin() {
    let kv = Arc::new(MemoryKv::default());
    let abc = Storage::new(kv.clone(), "abc");
    let xyz = Storage::new(kv.clone(), "xyz");

    abc.set("token", json!("a")).await;
    xyz.set("token", json!("b")).await;

    assert_eq!(abc.get("token").await, Some(json!("a")));
    assert_eq!(xyz.get("token").await, Some(json!("b")));
    assert_eq!(kv.values().get("plugin:006100620063:token"), Some(&json!("a")));
    assert_eq!(abc.namespace(), "plugin:006100620063:");

    abc.remove("token").await;
    assert_eq!(abc.get("token").await, None);
    assert_eq!(xyz.get("token").await, Some(json!("b")));

    abc.set("watch-deleted:one", json!(1)).await;
    abc.set("watch-deleted:two", json!(2)).await;
    assert_eq!(
        abc.scan("watch-deleted:", 10).await,
        vec![("watch-deleted:one".to_owned(), json!(1)), ("watch-deleted:two".to_owned(), json!(2))]
    );
    assert_eq!(abc.scan("watch-deleted:", 1).await, vec![("watch-deleted:one".to_owned(), json!(1))]);
    assert_eq!(xyz.scan("watch-deleted:", 10).await, Vec::new());
}

#[test]
fn fold_settings_merges_document_overlays_in_order() {
    let entries: Vec<Entry> = [
        json!({"providers":{"acme":{"settings":{"baseURL":"one","timeout":10}}}}),
        json!({"providers":{"acme":{"settings":{"baseURL":"two"}}}}),
        json!({"model":"embedded/model"}),
    ]
    .into_iter()
    .map(|json| {
        Entry::Document(Document {
            path: None,
            info: codec::decode_json::<ConfigEntryDocumentInfo>(&json.to_string()).expect("a config document"),
            origin: Origin::File,
        })
    })
    .collect();

    let folded = settings::fold_settings(&entries, "acme", None).expect("the folded settings");

    assert_eq!(folded.rest.0, vec![("baseURL".to_owned(), json!("two"))]);
    assert_eq!(folded.timeout, Some(ProviderSettingsTimeout::Number(ProviderSettingsTimeoutNumber(10.0))));
}

#[tokio::test]
async fn credential_settings_reloads_when_the_active_credential_changes() {
    let mut harness = Harness::start(Input::new(Vec::new())).await;
    harness.credential("test", key_credential("token-a", "a")).await;

    let cell = settings::credential_settings(
        &harness.services,
        &mut harness.registrations,
        &IntegrationID("test".to_owned()),
        |value| match value {
            CredentialValue::Key(key) => settings::metadata_settings(key.metadata.as_ref(), &["baseURL"]),
            _ => settings::metadata_settings(None, &["baseURL"]),
        },
    )
    .await;
    assert_eq!(
        *cell.lock().expect("lock provider settings"),
        settings::metadata_settings(Some(&JsonMap(vec![("baseURL".to_owned(), json!("a"))])), &["baseURL"])
    );

    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    harness.credential("test", key_credential("token-b", "b")).await;

    tokio::time::timeout(Duration::from_secs(1), async {
        while cell.lock().expect("lock provider settings").rest.0 != vec![("baseURL".to_owned(), json!("b"))] {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the credential settings reload");
}

#[tokio::test]
async fn refresh_start_drops_a_stale_connection_result() {
    let harness = Harness::start(Input::new(Vec::new())).await;
    let active = Arc::new(Mutex::new(Some(connection("a"))));
    let (release, gate) = watch::channel(false);
    let started = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(Notify::new());
    let reloads = Arc::new(AtomicUsize::new(0));

    let first = refresh::start(
        &harness.services.bus,
        Start::<u32, Infallible> {
            integration_id: "test".to_owned(),
            initial: 0u32,
            interval: Some(Duration::from_secs(600)),
            active: Box::new({
                let active = active.clone();
                move || {
                    let active = active.clone();
                    Box::pin(async move { active.lock().expect("lock the active connection").clone() })
                }
            }),
            load: Box::new({
                let started = started.clone();
                let finished = finished.clone();
                let gate = gate.clone();
                move |_value, _| {
                    let started = started.clone();
                    let finished = finished.clone();
                    let mut gate = gate.clone();
                    Box::pin(async move {
                        started.store(true, Ordering::SeqCst);
                        let _ = gate.changed().await;
                        finished.notify_one();
                        Ok(Loaded { connection: Some(connection("a")), value: 1u32 })
                    })
                }
            }),
            reload: Box::new({
                let reloads = reloads.clone();
                move || {
                    let reloads = reloads.clone();
                    Box::pin(async move {
                        reloads.fetch_add(1, Ordering::SeqCst);
                    })
                }
            }),
        },
    );

    while !started.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
    *active.lock().expect("lock the active connection") = Some(connection("b"));
    release.send_replace(true);
    finished.notified().await;
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    assert_eq!(first.current(), 0);
    assert_eq!(reloads.load(Ordering::SeqCst), 0);

    let active = Arc::new(Mutex::new(Some(connection("a"))));
    let reloads = Arc::new(AtomicUsize::new(0));
    let second = refresh::start(
        &harness.services.bus,
        Start::<u32, Infallible> {
            integration_id: "test".to_owned(),
            initial: 0u32,
            interval: Some(Duration::from_secs(600)),
            active: Box::new({
                let active = active.clone();
                move || {
                    let active = active.clone();
                    Box::pin(async move { active.lock().expect("lock the active connection").clone() })
                }
            }),
            load: Box::new(|_value, _| {
                Box::pin(async move { Ok(Loaded { connection: Some(connection("a")), value: 1u32 }) })
            }),
            reload: Box::new({
                let reloads = reloads.clone();
                move || {
                    let reloads = reloads.clone();
                    Box::pin(async move {
                        reloads.fetch_add(1, Ordering::SeqCst);
                    })
                }
            }),
        },
    );

    tokio::time::timeout(Duration::from_secs(1), async {
        while reloads.load(Ordering::SeqCst) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the refresh reloads");
    assert_eq!(second.current(), 1);
    assert_eq!(reloads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn refresh_apply_completes_its_reload_when_a_switch_replaces_the_load() {
    let harness = Harness::start(Input::new(Vec::new())).await;
    let active = Arc::new(Mutex::new(Some(connection("a"))));
    let (release, gate) = watch::channel(false);
    let loads = Arc::new(AtomicUsize::new(0));
    let reloads = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));

    let refresh = refresh::start(
        &harness.services.bus,
        Start::<u32, Infallible> {
            integration_id: "test".to_owned(),
            initial: 0u32,
            interval: Some(Duration::from_secs(600)),
            active: Box::new({
                let active = active.clone();
                move || {
                    let active = active.clone();
                    Box::pin(async move { active.lock().expect("lock the active connection").clone() })
                }
            }),
            load: Box::new({
                let loads = loads.clone();
                move |_value, _| {
                    let loads = loads.clone();
                    Box::pin(async move {
                        loads.fetch_add(1, Ordering::SeqCst);
                        Ok(Loaded { connection: Some(connection("a")), value: 1u32 })
                    })
                }
            }),
            reload: Box::new({
                let reloads = reloads.clone();
                let completed = completed.clone();
                let gate = gate.clone();
                move || {
                    let reloads = reloads.clone();
                    let completed = completed.clone();
                    let mut gate = gate.clone();
                    Box::pin(async move {
                        reloads.fetch_add(1, Ordering::SeqCst);
                        let _ = gate.changed().await;
                        completed.fetch_add(1, Ordering::SeqCst);
                    })
                }
            }),
        },
    );

    tokio::time::timeout(Duration::from_secs(1), async {
        while reloads.load(Ordering::SeqCst) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the startup reload started");

    harness
        .services
        .bus
        .publish(
            &Definition::ephemeral("credential.switched"),
            json!({ "integrationID": "test", "credentialID": null }),
            PublishOptions { global: true, ..Default::default() },
        )
        .expect("publish the credential switch");

    tokio::time::timeout(Duration::from_secs(1), async {
        while loads.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the switched load started");
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }

    release.send_replace(true);

    tokio::time::timeout(Duration::from_secs(1), async {
        while completed.load(Ordering::SeqCst) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the interrupted reload completed");

    assert_eq!(refresh.current(), 1);
    assert_eq!(reloads.load(Ordering::SeqCst), 1);
    assert_eq!(completed.load(Ordering::SeqCst), 1);
}
