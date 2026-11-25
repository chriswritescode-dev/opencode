use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::sync::{Arc, Mutex};

use opencode_ai::ProviderPackages;
use opencode_bus::{Bus, BusOptions, Event, ListenerHandle};
use opencode_catalog::aisdk_native::aisdk;
use opencode_catalog::credential::{CreateInput, CredentialService};
use opencode_catalog::integration::{ConfigEnv, Implementation, IntegrationService};
use opencode_catalog::models_dev::snapshot_text;
use opencode_catalog::provider::{ProviderService, model_settings, native_settings};
use opencode_contract::codec::{Encode, decode_json, encode_json};
use opencode_contract::generated::{
    AbsolutePath, CredentialKey, CredentialKeyType, CredentialValue, IntegrationEnvMethod, IntegrationEnvMethodType,
    IntegrationID, IntegrationMethod, LocationRef, ProviderID, ProviderInfo, ProviderInfoActivation, ProviderSettings,
};
use opencode_storage::{Database, DatabaseLocation};
use serde_json::{Value, json};

use crate::TempDir;

#[allow(dead_code)]
pub(crate) struct Harness {
    pub dir: TempDir,
    pub bus: Arc<Bus>,
    pub credentials: Arc<CredentialService>,
    pub integrations: Arc<IntegrationService>,
    pub providers: Arc<ProviderService>,
    pub location: LocationRef,
}

pub(crate) fn config_env(values: &HashMap<String, String>) -> ConfigEnv {
    let values = values.clone();
    Arc::new(move |name| values.get(name).cloned().map(OsString::from))
}

pub(crate) fn setup_with_env(values: HashMap<String, String>) -> Harness {
    let dir = TempDir::new();
    let (database, _) = Database::open(DatabaseLocation::Memory, dir.path()).expect("open the database");
    let database = Arc::new(database);
    let bus = Bus::new(database.clone(), BusOptions::default());
    let credentials = CredentialService::new(database.clone(), bus.clone(), dir.path().join("locks"));
    let location = LocationRef { directory: AbsolutePath("/main".to_owned()), workspace_id: None };
    let command_env =
        Arc::new(values.iter().map(|(name, value)| (OsString::from(name), OsString::from(value))).collect());
    let integrations =
        IntegrationService::new(credentials.clone(), bus.clone(), location.clone(), config_env(&values), command_env);
    let providers = ProviderService::new(integrations.clone(), bus.clone(), location.clone());
    Harness { dir, bus, credentials, integrations, providers, location }
}

pub(crate) fn setup() -> Harness {
    setup_with_env(HashMap::new())
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

fn credential_input(integration: &IntegrationID, key: &str, label: &str) -> CreateInput {
    CreateInput {
        id: None,
        integration_id: integration.clone(),
        value: CredentialValue::Key(CredentialKey {
            r#type: CredentialKeyType::Key,
            key: key.to_owned(),
            metadata: None,
        }),
        label: Some(label.to_owned()),
        activate: None,
    }
}

fn provider_ids(providers: &[ProviderInfo]) -> Vec<ProviderID> {
    providers.iter().map(|provider| provider.id.clone()).collect()
}

async fn available_ids(harness: &Harness) -> Vec<ProviderID> {
    provider_ids(&harness.providers.available().await.expect("available providers"))
}

fn encoded(value: &impl Encode) -> Value {
    serde_json::from_str(&encode_json(value)).expect("encode contract value")
}

#[tokio::test]
async fn native_settings_keeps_provider_options() {
    let settings = decode_json::<ProviderSettings>(
        r#"{"apiKey":"secret","reasoningEffort":"high","chunkTimeout":1000,"compaction":{"type":"native"},"cache":{"ttl":"1h"},"encoding":"gzip","transport":"websocket"}"#,
    )
    .expect("decode provider settings");
    assert_eq!(Value::Object(native_settings(&settings)), json!({"apiKey":"secret","reasoningEffort":"high"}));
}

#[tokio::test]
async fn model_settings_inherits_shared_and_loose_settings() {
    let settings = decode_json::<ProviderSettings>(
        r#"{"timeout":60000,"chunkTimeout":30000,"transport":"websocket","compaction":{"type":"native"},"reasoningEffort":"high"}"#,
    )
    .expect("decode provider settings");
    let projected = model_settings(Some(&settings)).expect("model settings");
    assert_eq!(encoded(&projected), json!({"compaction":{"type":"native"},"reasoningEffort":"high"}));
}

#[tokio::test]
async fn publishes_provider_updated_after_changes() {
    let harness = setup();
    let (events, _handle) = capture(&harness.bus);
    let provider_id = ProviderID("test".to_owned());
    let _registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, |_| {});
                Ok(())
            }
        })
        .await;
    let published =
        events.lock().expect("lock the events").iter().filter(|event| event.r#type() == "provider.updated").count();
    assert_eq!(published, 1);
}

#[tokio::test]
async fn preserves_provider_identity_when_updating_new_and_existing_providers() {
    let harness = setup();
    let original = ProviderID("original".to_owned());
    let renamed = ProviderID("renamed".to_owned());
    let _registration = harness
        .providers
        .transform({
            let original = original.clone();
            let renamed = renamed.clone();
            move |editor| {
                editor.update(&original, |provider| {
                    provider.id = renamed.clone();
                    provider.name = "Created".to_owned();
                });
                assert_eq!(editor.get(&original).expect("created provider").provider.id, original);
                editor.update(&original, |provider| {
                    provider.id = renamed.clone();
                    provider.name = "Updated".to_owned();
                });
                Ok(())
            }
        })
        .await;
    let updated = harness.providers.get(&original).await.expect("get provider").expect("provider");
    assert_eq!(updated.id, original);
    assert_eq!(updated.name, "Updated");
    assert!(harness.providers.get(&renamed).await.expect("get renamed").is_none());
    assert_eq!(provider_ids(&harness.providers.all().await.expect("list providers")), vec![original.clone()]);

    harness.providers.reload().await;
    let reloaded = harness.providers.get(&original).await.expect("get provider").expect("provider");
    assert_eq!(reloaded.id, original);
    assert_eq!(reloaded.name, "Updated");
    assert!(harness.providers.get(&renamed).await.expect("get renamed").is_none());
    assert_eq!(provider_ids(&harness.providers.all().await.expect("list providers")), vec![original]);
}

#[tokio::test]
async fn derives_availability_from_active_credentials_without_changing_provider_state() {
    let harness = setup();
    let integration = IntegrationID("test".to_owned());
    let provider_id = ProviderID("test".to_owned());
    let _registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, |_| {});
                Ok(())
            }
        })
        .await;
    harness.credentials.create(credential_input(&integration, "first", "First")).expect("create credential");
    assert_eq!(available_ids(&harness).await, vec![provider_id.clone()]);
    assert!(harness.providers.get(&provider_id).await.expect("get provider").expect("provider").body.is_none());

    harness.credentials.create(credential_input(&integration, "second", "Second")).expect("create credential");
    assert_eq!(available_ids(&harness).await, vec![provider_id.clone()]);
    assert!(harness.providers.get(&provider_id).await.expect("get provider").expect("provider").body.is_none());
}

#[tokio::test]
async fn derives_availability_from_a_provider_integration() {
    let harness = setup();
    let integration = IntegrationID("gateway".to_owned());
    let provider_id = ProviderID("remote".to_owned());
    let _integration_registration = harness
        .integrations
        .transform({
            let integration = integration.clone();
            move |editor| {
                editor.update(&integration, |_| {});
                Ok(())
            }
        })
        .await;
    let _provider_registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            let integration = integration.clone();
            move |editor| {
                editor.update(&provider_id, |provider| {
                    provider.integration_id = Some(integration.clone());
                });
                Ok(())
            }
        })
        .await;
    assert!(harness.providers.available().await.expect("available providers").is_empty());

    harness.credentials.create(credential_input(&integration, "secret", "default")).expect("create credential");
    assert_eq!(available_ids(&harness).await, vec![provider_id]);
}

#[tokio::test]
async fn makes_an_explicitly_enabled_provider_available_without_a_connection() {
    let harness = setup();
    let integration = IntegrationID("gateway".to_owned());
    let provider_id = ProviderID("remote".to_owned());
    let _integration_registration = harness
        .integrations
        .transform({
            let integration = integration.clone();
            move |editor| {
                editor.update(&integration, |_| {});
                Ok(())
            }
        })
        .await;
    let _provider_registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            let integration = integration.clone();
            move |editor| {
                editor.update(&provider_id, |provider| {
                    provider.integration_id = Some(integration.clone());
                    provider.settings = Some(
                        decode_json::<ProviderSettings>(r#"{"baseURL":"https://gateway.example.com/v1"}"#)
                            .expect("decode provider settings"),
                    );
                });
                Ok(())
            }
        })
        .await;
    assert!(harness.providers.available().await.expect("available providers").is_empty());

    let _enabled = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, |provider| {
                    provider.activation = ProviderInfoActivation::Enabled;
                });
                Ok(())
            }
        })
        .await;
    assert_eq!(available_ids(&harness).await, vec![provider_id]);
}

#[tokio::test]
async fn projects_environment_connections_without_a_catalog_plugin() {
    let harness = setup_with_env(HashMap::from([("CATALOG_TEST_API_KEY".to_owned(), "secret".to_owned())]));
    let provider_id = ProviderID("test".to_owned());
    let _integration_registration = harness
        .integrations
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.method_update(Implementation::Method {
                    integration_id: IntegrationID(provider_id.0.clone()),
                    method: IntegrationMethod::Env(IntegrationEnvMethod {
                        r#type: IntegrationEnvMethodType::Env,
                        names: vec!["CATALOG_TEST_API_KEY".to_owned()],
                    }),
                });
                Ok(())
            }
        })
        .await;
    let _provider_registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, |_| {});
                Ok(())
            }
        })
        .await;
    assert!(available_ids(&harness).await.contains(&provider_id));
}

#[tokio::test]
async fn stores_provider_package_settings() {
    let harness = setup();
    let provider_id = ProviderID("test".to_owned());
    let _registration = harness
        .providers
        .transform({
            let provider_id = provider_id.clone();
            move |editor| {
                editor.update(&provider_id, |provider| {
                    provider.package = aisdk("custom-sdk");
                    provider.settings = Some(
                        decode_json::<ProviderSettings>(r#"{"baseURL":"https://override.example.com"}"#)
                            .expect("decode provider settings"),
                    );
                });
                Ok(())
            }
        })
        .await;
    let provider = harness.providers.get(&provider_id).await.expect("get provider").expect("provider");
    let value = encoded(&provider);
    assert_eq!(value["package"], json!("aisdk:custom-sdk"));
    assert_eq!(value["settings"], json!({"baseURL":"https://override.example.com"}));
}

#[test]
fn keeps_loading_the_hyphenated_provider_paths_existing_configs_reference() {
    let packages = ProviderPackages::builtin().specifiers();
    for specifier in
        ["@opencode/ai/providers/google-vertex-messages", "@opencode/ai/providers/openai-compatible-responses"]
    {
        assert!(packages.contains(specifier), "{specifier} stays a builtin package");
    }
}

#[test]
fn loads_every_bundled_models_dev_entrypoint() {
    let snapshot: Value = serde_json::from_str(snapshot_text()).expect("decode the bundled models.dev snapshot");
    let specifiers: BTreeSet<&str> = snapshot
        .as_object()
        .expect("the snapshot is an object")
        .values()
        .filter_map(|provider| provider.get("models"))
        .filter_map(Value::as_object)
        .flat_map(|models| models.values())
        .filter_map(|model| model.get("api"))
        .filter_map(Value::as_object)
        .flat_map(|apis| apis.values())
        .filter_map(|api| api.get("sdk"))
        .filter_map(|sdk| sdk.get("opencodeai"))
        .filter_map(Value::as_str)
        .filter(|specifier| specifier.starts_with("@opencode/ai/"))
        .collect();

    let packages = ProviderPackages::builtin().specifiers();
    let protocol_only = BTreeSet::from(["@opencode/ai/protocols/openai-decisions"]);
    let missing: BTreeSet<&str> =
        specifiers.iter().copied().filter(|specifier| !packages.contains(*specifier)).collect();
    let expected: BTreeSet<&str> =
        specifiers.iter().copied().filter(|specifier| protocol_only.contains(*specifier)).collect();
    assert_eq!(missing, expected);
}
