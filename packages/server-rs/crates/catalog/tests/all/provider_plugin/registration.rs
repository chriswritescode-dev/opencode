use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use opencode_catalog::builtin::{Builtin, Deps, Services};
use opencode_catalog::provider_plugin::hooks::{ModelRequest, ProviderHooks, RequestKind};
use opencode_catalog::provider_plugin::{ORDER, Slot};
use opencode_contract::generated::{
    AgentID, IntegrationEnvMethodType, IntegrationExternalMethodType, IntegrationID, IntegrationInfo,
    IntegrationKeyMethodType, IntegrationMethod, IntegrationOAuthMethodType, ModelRef, ProviderID, SessionID,
};

use crate::builtin::{Input, start};
use crate::provider_plugin::support::plugin_context;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn order_matches_the_pinned_provider_plugin_list() {
    let ids: Vec<&str> = ORDER.iter().map(Slot::id).collect();
    assert_eq!(
        ids,
        [
            "opencode.provider.amazon.bedrock",
            "opencode.provider.azure",
            "opencode.provider.cerebras",
            "opencode.provider.chatgpt",
            "opencode.provider.cloudflare.ai.gateway",
            "opencode.provider.cloudflare.workers.ai",
            "opencode.provider.databricks",
            "opencode.provider.digitalocean",
            "opencode.provider.github.copilot",
            "opencode.provider.gitlab",
            "opencode.provider.google.vertex",
            "opencode.provider.kilo",
            "opencode.provider.llmgateway",
            "opencode.provider.lmstudio",
            "opencode.provider.modal",
            "opencode.provider.neon",
            "opencode.provider.nvidia",
            "opencode.provider.ollama",
            "opencode.provider.opencode",
            "opencode.provider.snowflake.cortex",
            "opencode.provider.openai",
            "opencode.provider.openai-context-caps",
            "opencode.provider.openrouter",
            "opencode.provider.poe",
            "opencode.provider.prompt-cache-key",
            "opencode.provider.sap.ai.core",
            "opencode.provider.vllm",
            "opencode.provider.xai",
            "opencode.provider.zenmux",
            "opencode.provider.claude-agent",
        ]
    );
    assert_eq!(ORDER.iter().filter(|slot| matches!(slot, Slot::Native(_))).count(), 27);
    assert!(
        matches!(ORDER[18], Slot::Elsewhere { id, owner } if id == "opencode.provider.opencode" && owner == "OR-48")
    );
    assert!(
        matches!(ORDER[25], Slot::Elsewhere { id, owner } if id == "opencode.provider.sap.ai.core" && owner == "OR-64")
    );
    assert!(
        matches!(ORDER[29], Slot::Elsewhere { id, owner } if id == "opencode.provider.claude-agent" && owner == "OR-64")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_native_slot_is_handled_by_provider_plugin_register() {
    let harness = start(Input::new(Vec::new())).await;
    let context = plugin_context(&harness);
    let mut builtin = Builtin::default();
    let dispatched = opencode_catalog::provider_plugin::register(&harness.services, &context, &mut builtin).await;

    let expected: Vec<&str> = ORDER
        .iter()
        .filter_map(|slot| match slot {
            Slot::Native(id) => Some(*id),
            Slot::Elsewhere { .. } => None,
        })
        .collect();
    assert_eq!(dispatched, expected);
    assert_eq!(dispatched.len(), 27);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_plugins_register_their_integrations_with_an_empty_catalog() {
    let mut harness = start(Input::new(Vec::new())).await;
    let context = plugin_context(&harness);
    let builtin = opencode_catalog::builtin::register(
        Services {
            bus: harness.services.bus.clone(),
            config: harness.services.config.clone(),
            credentials: harness.services.credentials.clone(),
            models_dev: harness.services.models_dev.clone(),
            wellknown: harness.services.wellknown.clone(),
            integration: harness.services.integration.clone(),
            provider: harness.services.provider.clone(),
            model: harness.services.model.clone(),
        },
        Deps { provider_plugins: Some(context), external: None },
    )
    .await;
    harness.record(builtin);

    let expected: [(&str, Vec<ExpectedMethod>); 15] = [
        ("amazon-bedrock", vec![env(&["AWS_BEARER_TOKEN_BEDROCK"]), ExpectedMethod::Key, external("aws-profile")]),
        ("azure", vec![env(&["AZURE_API_KEY", "AZURE_COGNITIVE_SERVICES_API_KEY"]), ExpectedMethod::Key]),
        ("cloudflare-ai-gateway", vec![ExpectedMethod::Key]),
        (
            "cloudflare-workers-ai",
            vec![
                env(&["CLOUDFLARE_API_KEY", "CLOUDFLARE_WORKERS_AI_TOKEN", "CLOUDFLARE_API_TOKEN"]),
                ExpectedMethod::Key,
            ],
        ),
        ("databricks", vec![env(&["DATABRICKS_TOKEN"])]),
        ("digitalocean", vec![oauth("browser")]),
        ("github-copilot", vec![env(&["COPILOT_GITHUB_TOKEN"]), oauth("device")]),
        ("gitlab", vec![oauth("pkce")]),
        ("google-vertex", vec![env(&["GOOGLE_VERTEX_API_KEY"]), external("google-adc")]),
        ("modal", vec![env(&["MODAL_PROXY_TOKEN"])]),
        ("neon", vec![env(&["NEON_AI_GATEWAY_TOKEN"])]),
        (
            "snowflake-cortex",
            vec![ExpectedMethod::Key, env(&["SNOWFLAKE_CORTEX_TOKEN", "SNOWFLAKE_CORTEX_PAT"]), oauth("browser")],
        ),
        ("openai", vec![oauth("chatgpt-token-sharing"), oauth("chatgpt-browser"), oauth("chatgpt-headless")]),
        ("poe", vec![oauth("browser")]),
        ("xai", vec![oauth("device"), ExpectedMethod::Key]),
    ];

    for (id, expected_methods) in expected {
        let integration = harness
            .services
            .integration
            .get(&IntegrationID(id.to_owned()))
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("the {id} integration is registered"));
        assert_eq!(methods(&integration), expected_methods, "{id}");
    }

    for id in ["lmstudio", "ollama", "vllm"] {
        assert!(harness.services.integration.get(&IntegrationID(id.to_owned())).await.unwrap().is_none(), "{id}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prompt_cache_key_marks_only_its_configured_providers() {
    let mut harness = start(Input::new(Vec::new())).await;
    let ids = [
        "abliteration-ai",
        "aihubmix",
        "anyapi",
        "auriko",
        "edenai",
        "jalapeno",
        "llama",
        "morph",
        "snowflake-cortex",
        "tencent-tokenhub",
        "upstage",
        "venice",
    ];
    let _seeded = harness
        .services
        .provider
        .transform(move |editor| {
            for id in ids.into_iter().chain(["other"]) {
                editor.update(&ProviderID(id.to_owned()), |provider| provider.package = "aisdk:test".to_owned());
            }
            Ok(())
        })
        .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["prompt_cache_key"], context).await;

    for id in ids {
        let provider = harness.services.provider.get(&ProviderID(id.to_owned())).await.unwrap().unwrap();
        assert_eq!(
            provider.compatibility.and_then(|compatibility| compatibility.supports_prompt_cache_key),
            Some(true),
            "{id}"
        );
    }
    let other = harness.services.provider.get(&ProviderID("other".to_owned())).await.unwrap().unwrap();
    assert_eq!(other.compatibility, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn neon_registers_only_the_gateway_token_env_method() {
    let mut harness = start(Input::new(Vec::new())).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["neon"], context).await;

    let integration = harness.services.integration.get(&IntegrationID("neon".to_owned())).await.unwrap().unwrap();
    assert_eq!(integration.methods.len(), 1);
    let IntegrationMethod::Env(method) = &integration.methods[0] else {
        panic!("expected the env method");
    };
    assert_eq!(method.names, ["NEON_AI_GATEWAY_TOKEN"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hooks_run_in_registration_order_ignore_other_providers_and_remove_on_drop() {
    let hooks = ProviderHooks::default();
    let events = Arc::new(Mutex::new(Vec::new()));
    let all = {
        let events = events.clone();
        hooks.on_model_request(None, move |mut event| {
            events.lock().expect("the hook events lock is never poisoned").push("all".to_owned());
            event.headers.insert("first".to_owned(), "1".to_owned());
            Box::pin(async move { event })
        })
    };
    let _filtered = {
        let events = events.clone();
        hooks.on_model_request(Some(ProviderID("a".to_owned())), move |event| {
            events.lock().expect("the hook events lock is never poisoned").push("filtered".to_owned());
            Box::pin(async move { event })
        })
    };

    let event = hooks.trigger_model_request(model_request("a")).await;
    assert_eq!(*events.lock().expect("the hook events lock is never poisoned"), ["all", "filtered"]);
    assert_eq!(event.headers.get("first").map(String::as_str), Some("1"));

    hooks.trigger_model_request(model_request("b")).await;
    assert_eq!(*events.lock().expect("the hook events lock is never poisoned"), ["all", "filtered", "all"]);

    drop(all);
    hooks.trigger_model_request(model_request("b")).await;
    assert_eq!(*events.lock().expect("the hook events lock is never poisoned"), ["all", "filtered", "all"]);
}

#[derive(Debug, PartialEq)]
enum ExpectedMethod {
    Env(Vec<String>),
    Key,
    Oauth(String),
    External(String),
}

fn env(names: &[&str]) -> ExpectedMethod {
    ExpectedMethod::Env(names.iter().map(|name| (*name).to_owned()).collect())
}

fn oauth(id: &str) -> ExpectedMethod {
    ExpectedMethod::Oauth(id.to_owned())
}

fn external(id: &str) -> ExpectedMethod {
    ExpectedMethod::External(id.to_owned())
}

fn methods(integration: &IntegrationInfo) -> Vec<ExpectedMethod> {
    integration
        .methods
        .iter()
        .map(|method| match method {
            IntegrationMethod::Env(method) => {
                assert_eq!(method.r#type, IntegrationEnvMethodType::Env);
                ExpectedMethod::Env(method.names.clone())
            }
            IntegrationMethod::Key(method) => {
                assert_eq!(method.r#type, IntegrationKeyMethodType::Key);
                ExpectedMethod::Key
            }
            IntegrationMethod::Oauth(method) => {
                assert_eq!(method.r#type, IntegrationOAuthMethodType::Oauth);
                ExpectedMethod::Oauth(method.id.0.clone())
            }
            IntegrationMethod::External(method) => {
                assert_eq!(method.r#type, IntegrationExternalMethodType::External);
                ExpectedMethod::External(method.id.0.clone())
            }
            IntegrationMethod::Command(method) => panic!("unexpected command method {}", method.id.0),
        })
        .collect()
}

fn model_request(provider: &str) -> ModelRequest {
    ModelRequest {
        session_id: SessionID("s".to_owned()),
        agent: AgentID("agent".to_owned()),
        model: ModelRef { id: "m".to_owned(), provider_id: ProviderID(provider.to_owned()), variant: None },
        kind: RequestKind::Primary,
        base_url: None,
        headers: IndexMap::new(),
    }
}
