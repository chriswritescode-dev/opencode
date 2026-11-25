use std::sync::{Arc, Mutex};
use std::time::Duration;

use opencode_catalog::builtin::Builtin;
use opencode_catalog::credential::{CreateInput, Info};
use opencode_catalog::integration::Implementation;
use opencode_catalog::provider_plugin::BoxFuture;
use opencode_catalog::provider_plugin::gitlab::{self, Discovery, Options, WorkflowModel};
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    CredentialKey, CredentialKeyType, CredentialOAuth, CredentialOAuthType, CredentialValue, IntegrationEnvMethod,
    IntegrationEnvMethodType, IntegrationID, IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod,
    IntegrationMethodID, ModelInfo, ProviderID,
};
use serde_json::{Value, json};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{Routes, plugin_context_with, provider_settings_from, settings_json};

const PROVIDER_ID: &str = "gitlab";

const PACKAGE: &str = "aisdk:gitlab-ai-provider";

fn provider_id() -> ProviderID {
    ProviderID(PROVIDER_ID.to_owned())
}

fn integration_id() -> IntegrationID {
    IntegrationID(PROVIDER_ID.to_owned())
}

fn discovered() -> Vec<WorkflowModel> {
    vec![
        WorkflowModel {
            id: "duo-workflow-new-model".to_owned(),
            r#ref: "new_model_ref".to_owned(),
            name: "New Model".to_owned(),
            context: 128_000.0,
            output: 16_000.0,
            pinned: false,
        },
        WorkflowModel {
            id: "duo-workflow-default".to_owned(),
            r#ref: "__default__duo_agent_platform_agentic_chat".to_owned(),
            name: "Default".to_owned(),
            context: 200_000.0,
            output: 64_000.0,
            pinned: false,
        },
    ]
}

fn default_workflow() -> Vec<WorkflowModel> {
    vec![discovered().remove(1)]
}

#[derive(Clone, Debug, PartialEq)]
struct Call {
    instance_url: String,
    headers: Vec<(String, String)>,
    directory: String,
    cache_key: String,
}

#[derive(Clone, Debug)]
enum Reply {
    Models(Vec<WorkflowModel>),
    Empty,
    Error(String),
    Hang,
}

struct FakeDiscovery {
    calls: Mutex<Vec<Call>>,
    reply: Mutex<Reply>,
}

impl FakeDiscovery {
    fn new() -> Arc<Self> {
        Arc::new(Self { calls: Mutex::new(Vec::new()), reply: Mutex::new(Reply::Empty) })
    }

    fn set(&self, reply: Reply) {
        *self.reply.lock().expect("lock the GitLab discovery reply") = reply;
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("lock the GitLab discovery calls").clone()
    }
}

impl Discovery for FakeDiscovery {
    fn discover(
        &self,
        instance_url: String,
        headers: Vec<(String, String)>,
        directory: String,
        cache_key: String,
    ) -> BoxFuture<Result<Option<Vec<WorkflowModel>>, String>> {
        self.calls.lock().expect("lock the GitLab discovery calls").push(Call {
            instance_url,
            headers,
            directory,
            cache_key,
        });
        let reply = self.reply.lock().expect("lock the GitLab discovery reply").clone();
        Box::pin(async move {
            match reply {
                Reply::Models(models) => Ok(Some(models)),
                Reply::Empty => Ok(None),
                Reply::Error(message) => Err(message),
                Reply::Hang => {
                    std::future::pending::<()>().await;
                    unreachable!()
                }
            }
        })
    }
}

async fn seed_integration(harness: &mut Harness) {
    let id = integration_id();
    let registration = harness
        .services
        .integration
        .transform(move |editor| {
            editor.method_update(Implementation::Method {
                integration_id: id.clone(),
                method: IntegrationMethod::Key(IntegrationKeyMethod {
                    r#type: IntegrationKeyMethodType::Key,
                    label: None,
                    form: None,
                }),
            });
            editor.method_update(Implementation::Method {
                integration_id: id.clone(),
                method: IntegrationMethod::Env(IntegrationEnvMethod {
                    r#type: IntegrationEnvMethodType::Env,
                    names: vec!["GITLAB_TOKEN".to_owned()],
                }),
            });
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(registration);
}

async fn seed_provider(harness: &mut Harness, settings: Option<Value>) {
    let id = provider_id();
    let settings = settings.map(provider_settings_from);
    let registration = harness
        .services
        .provider
        .transform(move |editor| {
            editor.update(&id, |provider| {
                provider.package = PACKAGE.to_owned();
                if let Some(settings) = settings.clone() {
                    provider.settings = Some(settings);
                }
            });
            editor.models_update(&id, "duo-chat-sonnet-5", |_| {});
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(registration);
}

async fn seed_configured_workflow_model(harness: &mut Harness) {
    let id = provider_id();
    let registration = harness
        .services
        .provider
        .transform(move |editor| {
            editor.models_update(&id, "duo-workflow-new-model", |model| {
                model.name = "Configured model".to_owned();
                model.limit.output = 42.0;
            });
            Ok(())
        })
        .await;
    harness.registrations.registrations.push(registration);
}

async fn add_plugin(harness: &mut Harness, discovery: Arc<FakeDiscovery>, options: Options) {
    let mut context = plugin_context_with(harness, Routes::new());
    context.gitlab_discovery = discovery;
    let mut builtin = Builtin::default();
    gitlab::register_with(&harness.services, &context, &mut builtin, options).await;
    harness.record(builtin);
}

fn key_credential(harness: &Harness, key: &str) -> Info {
    harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: CredentialValue::Key(CredentialKey {
                r#type: CredentialKeyType::Key,
                key: key.to_owned(),
                metadata: None,
            }),
            label: None,
            activate: None,
        })
        .expect("create the GitLab key credential")
}

fn oauth_credential(harness: &Harness, access: &str, metadata: Option<JsonMap<Value>>) -> Info {
    harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: integration_id(),
            value: CredentialValue::Oauth(CredentialOAuth {
                r#type: CredentialOAuthType::Oauth,
                method_id: IntegrationMethodID("oauth".to_owned()),
                refresh: "refresh-token".to_owned(),
                access: access.to_owned(),
                expires: 0.0,
                metadata,
            }),
            label: None,
            activate: None,
        })
        .expect("create the GitLab OAuth credential")
}

fn model_settings(model: &ModelInfo) -> Option<Value> {
    let settings = model.settings.as_ref()?;
    Some(serde_json::from_str(&opencode_contract::codec::encode_json(settings)).expect("the model settings json"))
}

async fn wait_for_calls(discovery: &FakeDiscovery, count: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if discovery.calls().len() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the GitLab discovery calls did not arrive"));
}

async fn wait_for_model(harness: &Harness, id: &str) -> ModelInfo {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(model) = harness.services.model.get(&provider_id(), id).await.unwrap_or(None) {
                return model;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the model {id} did not appear"))
}

async fn wait_for_model_absent(harness: &Harness, id: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if harness.services.model.get(&provider_id(), id).await.unwrap_or(None).is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the model {id} did not disappear"))
}

async fn wait_for_settings(harness: &Harness, expected: Value) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let provider = harness.services.provider.get(&provider_id()).await.unwrap().unwrap();
            if settings_json(&provider) == Some(expected.clone()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the GitLab provider settings settle");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovers_workflow_models_for_the_location_and_exposes_their_refs_in_the_model_registry() {
    let mut harness = start(Input::new(Vec::new())).await;
    let discovery = FakeDiscovery::new();
    seed_integration(&mut harness).await;
    seed_provider(&mut harness, Some(json!({ "instanceUrl": "https://configured.gitlab.example" }))).await;
    let saved = key_credential(&harness, "pat-token");
    discovery.set(Reply::Models(discovered()));
    add_plugin(&mut harness, discovery.clone(), Options::default()).await;

    let model = wait_for_model(&harness, "duo-workflow-new-model").await;
    let calls = discovery.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].directory, harness.root().to_string_lossy().into_owned());
    assert_eq!(calls[0].cache_key, saved.id.0);
    assert_eq!(calls[0].instance_url, "https://configured.gitlab.example");
    assert_eq!(calls[0].headers, [("PRIVATE-TOKEN".to_owned(), "pat-token".to_owned())]);

    let mut ids: Vec<String> =
        harness.services.model.available(None).await.unwrap().into_iter().map(|model| model.id).collect();
    ids.sort();
    assert_eq!(ids, ["duo-chat-sonnet-5", "duo-workflow-default", "duo-workflow-new-model"]);

    assert_eq!(model.name, "Agent Platform (New Model)");
    assert_eq!(model.package.as_deref(), Some(PACKAGE));
    let settings = model_settings(&model).expect("the discovered model settings");
    assert_eq!(settings["workflowRef"], json!("new_model_ref"));
    assert_eq!(model.limit.context, 128_000.0);
    assert_eq!(model.limit.output, 16_000.0);
    assert!(model.capabilities.tools);
    assert_eq!(model.capabilities.input, ["text", "image", "pdf"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uses_oauth_access_tokens_for_discovery() {
    let mut harness = start(Input::new(Vec::new())).await;
    let discovery = FakeDiscovery::new();
    seed_integration(&mut harness).await;
    seed_provider(&mut harness, None).await;
    oauth_credential(&harness, "access-token", None);
    add_plugin(&mut harness, discovery.clone(), Options::default()).await;

    wait_for_calls(&discovery, 1).await;
    let calls = discovery.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].headers, [("Authorization".to_owned(), "Bearer access-token".to_owned())]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sends_requests_to_the_instance_an_oauth_login_was_issued_against() {
    let mut harness = start(Input::new(Vec::new())).await;
    let discovery = FakeDiscovery::new();
    seed_integration(&mut harness).await;
    seed_provider(&mut harness, None).await;
    oauth_credential(
        &harness,
        "access-token",
        Some(JsonMap(vec![
            ("instanceUrl".to_owned(), json!("https://self-managed.gitlab.example")),
            ("clientID".to_owned(), json!("client")),
        ])),
    );
    add_plugin(&mut harness, discovery.clone(), Options::default()).await;

    wait_for_settings(&harness, json!({ "instanceUrl": "https://self-managed.gitlab.example" })).await;
    let provider = harness.services.provider.get(&provider_id()).await.unwrap().unwrap();
    assert_eq!(settings_json(&provider), Some(json!({ "instanceUrl": "https://self-managed.gitlab.example" })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn does_not_discover_with_only_an_ambient_gitlab_token_or_configured_api_key() {
    let mut harness = start(Input::new(Vec::new()).environment("GITLAB_TOKEN", "env-token")).await;
    let discovery = FakeDiscovery::new();
    seed_integration(&mut harness).await;
    seed_provider(&mut harness, Some(json!({ "apiKey": "configured-token" }))).await;
    discovery.set(Reply::Models(discovered()));
    add_plugin(&mut harness, discovery.clone(), Options::default()).await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(discovery.calls().is_empty());
    assert!(harness.services.model.get(&provider_id(), "duo-workflow-default").await.unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discovers_with_a_stored_login_without_replacing_configured_model_definitions() {
    let mut harness =
        start(Input::new(Vec::new()).environment("GITLAB_INSTANCE_URL", "https://env.gitlab.example")).await;
    let discovery = FakeDiscovery::new();
    seed_integration(&mut harness).await;
    seed_provider(&mut harness, None).await;
    seed_configured_workflow_model(&mut harness).await;
    key_credential(&harness, "stored-token");
    discovery.set(Reply::Models(discovered()));
    add_plugin(&mut harness, discovery.clone(), Options::default()).await;

    wait_for_model(&harness, "duo-workflow-default").await;
    let calls = discovery.calls();
    assert_eq!(calls[0].instance_url, "https://env.gitlab.example");
    assert_eq!(calls[0].headers, [("PRIVATE-TOKEN".to_owned(), "stored-token".to_owned())]);

    let model = harness.services.model.get(&provider_id(), "duo-workflow-new-model").await.unwrap().unwrap();
    assert_eq!(model.name, "Configured model");
    assert_eq!(model.limit.output, 42.0);
    assert!(harness.services.model.get(&provider_id(), "duo-workflow-default").await.unwrap().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scopes_the_discovery_cache_to_the_active_account() {
    let mut harness = start(Input::new(Vec::new())).await;
    let discovery = FakeDiscovery::new();
    seed_integration(&mut harness).await;
    seed_provider(&mut harness, None).await;
    let first = key_credential(&harness, "first");
    add_plugin(&mut harness, discovery.clone(), Options::default()).await;

    wait_for_calls(&discovery, 1).await;
    let second = key_credential(&harness, "second");
    wait_for_calls(&discovery, 2).await;
    let cache_keys: Vec<String> = discovery.calls().into_iter().map(|call| call.cache_key).collect();
    assert_eq!(cache_keys, [first.id.0, second.id.0]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gives_up_on_unresponsive_discovery_so_later_account_switches_still_load() {
    let mut harness = start(Input::new(Vec::new())).await;
    let discovery = FakeDiscovery::new();
    seed_integration(&mut harness).await;
    seed_provider(&mut harness, None).await;
    key_credential(&harness, "hanging");
    discovery.set(Reply::Hang);
    add_plugin(&mut harness, discovery.clone(), Options { timeout: Some(Duration::from_millis(50)) }).await;

    wait_for_calls(&discovery, 1).await;
    discovery.set(Reply::Models(discovered()));
    key_credential(&harness, "responsive");
    wait_for_model(&harness, "duo-workflow-new-model").await;
    assert_eq!(discovery.calls().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skips_unauthenticated_discovery_and_refreshes_after_credential_changes_including_failures() {
    let mut harness = start(Input::new(Vec::new())).await;
    let discovery = FakeDiscovery::new();
    seed_integration(&mut harness).await;
    seed_provider(&mut harness, None).await;
    add_plugin(&mut harness, discovery.clone(), Options::default()).await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(discovery.calls().is_empty());

    discovery.set(Reply::Models(discovered()));
    key_credential(&harness, "first-token");
    wait_for_model(&harness, "duo-workflow-new-model").await;

    discovery.set(Reply::Error("discovery unavailable".to_owned()));
    let second = key_credential(&harness, "second-token");
    wait_for_calls(&discovery, 2).await;
    wait_for_model(&harness, "duo-chat-sonnet-5").await;
    wait_for_model_absent(&harness, "duo-workflow-new-model").await;
    let calls = discovery.calls();
    assert_eq!(
        calls.last().expect("the failed discovery call").headers,
        [("PRIVATE-TOKEN".to_owned(), "second-token".to_owned())]
    );
    assert!(harness.services.model.get(&provider_id(), "duo-chat-sonnet-5").await.unwrap().is_some());

    discovery.set(Reply::Models(default_workflow()));
    key_credential(&harness, "third-token");
    wait_for_model(&harness, "duo-workflow-default").await;
    assert!(harness.services.model.get(&provider_id(), "duo-workflow-new-model").await.unwrap().is_none());

    discovery.set(Reply::Empty);
    harness.services.credentials.activate(&second.id).expect("activate the second GitLab credential");
    wait_for_model_absent(&harness, "duo-workflow-default").await;
}
