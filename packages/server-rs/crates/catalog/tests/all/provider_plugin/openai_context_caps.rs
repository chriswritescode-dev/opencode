use opencode_contract::generated::{
    CredentialKey, CredentialKeyType, CredentialOAuth, CredentialOAuthType, CredentialValue, IntegrationMethodID,
    ModelInfoLimit, ProviderID,
};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::plugin_context;

const FULL: ModelInfoLimit = ModelInfoLimit { context: 1_050_000.0, input: Some(922_000.0), output: 128_000.0 };

const LEGACY: ModelInfoLimit = ModelInfoLimit { context: 400_000.0, input: Some(272_000.0), output: 128_000.0 };

const CURRENT: ModelInfoLimit = ModelInfoLimit { context: 628_000.0, input: Some(500_000.0), output: 128_000.0 };

fn oauth(method: &str) -> CredentialValue {
    CredentialValue::Oauth(CredentialOAuth {
        r#type: CredentialOAuthType::Oauth,
        method_id: IntegrationMethodID(method.to_owned()),
        access: "chatgpt-token".to_owned(),
        refresh: "refresh".to_owned(),
        expires: 9_999_999_999_999.0,
        metadata: None,
    })
}

fn key() -> CredentialValue {
    CredentialValue::Key(CredentialKey { r#type: CredentialKeyType::Key, key: "sk-test".to_owned(), metadata: None })
}

async fn setup(harness: &mut Harness, value: CredentialValue) -> opencode_location::state::Registration {
    let provider = ProviderID("openai".to_owned());
    let seeded = harness
        .services
        .provider
        .transform({
            let provider = provider.clone();
            move |editor| {
                for id in
                    ["gpt-5.5", "gpt-5.6-sol", "gpt-6-luna", "gpt-6.1-sol", "gpt-6.2-astra", "gpt-7", "gpt-7-astra"]
                {
                    editor.models_update(&provider, id, |model| {
                        model.limit = FULL;
                    });
                }
                editor.models_update(&provider, "gpt-6.1-sol-fast", |model| {
                    model.model_id = "gpt-6.1-sol".to_owned();
                    model.limit = FULL;
                });
                editor.models_update(&provider, "gpt-6.2-mini", |model| {
                    model.limit = LEGACY;
                });
                Ok(())
            }
        })
        .await;
    harness.credential("openai", value).await;
    let context = plugin_context(harness);
    harness.register_provider_plugins(&["openai_context_caps"], context).await;
    seeded
}

async fn limit(harness: &Harness, id: &str) -> ModelInfoLimit {
    harness.services.model.get(&ProviderID("openai".to_owned()), id).await.unwrap().unwrap().limit
}

async fn assert_caps(harness: &Harness) {
    assert_eq!(limit(harness, "gpt-5.5").await, LEGACY, "gpt-5.5");
    assert_eq!(limit(harness, "gpt-5.6-sol").await, LEGACY, "gpt-5.6-sol");
    assert_eq!(limit(harness, "gpt-6-luna").await, LEGACY, "gpt-6-luna");
    assert_eq!(limit(harness, "gpt-6.1-sol").await, CURRENT, "gpt-6.1-sol");
    assert_eq!(limit(harness, "gpt-6.1-sol-fast").await, CURRENT, "gpt-6.1-sol-fast");
    assert_eq!(limit(harness, "gpt-7").await, CURRENT, "gpt-7");
    assert_eq!(limit(harness, "gpt-6.2-astra").await, LEGACY, "gpt-6.2-astra");
    assert_eq!(limit(harness, "gpt-7-astra").await, LEGACY, "gpt-7-astra");
    assert_eq!(limit(harness, "gpt-6.2-mini").await, LEGACY, "gpt-6.2-mini");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caps_openai_models_under_chatgpt_browser() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = setup(&mut harness, oauth("chatgpt-browser")).await;
    assert_caps(&harness).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caps_openai_models_under_chatgpt_headless() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = setup(&mut harness, oauth("chatgpt-headless")).await;
    assert_caps(&harness).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caps_openai_models_under_chatgpt_token_sharing() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = setup(&mut harness, oauth("chatgpt-token-sharing")).await;
    assert_caps(&harness).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keeps_catalog_limits_under_an_api_key() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = setup(&mut harness, key()).await;
    assert_eq!(limit(&harness, "gpt-5.6-sol").await, FULL, "gpt-5.6-sol");
    assert_eq!(limit(&harness, "gpt-6.1-sol").await, FULL, "gpt-6.1-sol");
}
