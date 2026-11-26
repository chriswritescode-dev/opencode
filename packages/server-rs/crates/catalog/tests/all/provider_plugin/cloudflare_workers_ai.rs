use opencode_catalog::credential::CreateInput;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    CredentialKey, CredentialKeyType, CredentialValue, IntegrationID, IntegrationInfo, IntegrationKeyMethod,
    IntegrationMethod, ProviderID, ProviderSettings,
};
use serde_json::{Value, json};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{plugin_context, provider_settings_from, settings_json, wait_for_setting};

const PROVIDER_ID: &str = "cloudflare-workers-ai";

const NATIVE_PACKAGE: &str = "@opencode/ai/providers/cloudflare-workers-ai";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_an_account_form_when_the_environment_does_not_provide_one() {
    let mut harness = start(Input::new(Vec::new())).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["cloudflare_workers_ai"], context).await;

    let method = key_method(&harness).await;
    assert_eq!(method.label.as_deref(), Some("API key"));
    let form = method.form.as_ref().expect("the key method form");
    assert_eq!(form.0.len(), 1);
    let field: Value =
        serde_json::from_str(&opencode_contract::codec::encode_json(&form.0[0])).expect("the form field json");
    assert_eq!(
        field,
        json!({
            "key": "accountId",
            "title": "Enter your Cloudflare Account ID",
            "required": true,
            "type": "string",
            "placeholder": "e.g. 1234567890abcdef1234567890abcdef",
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn routes_the_openai_compatible_package_to_the_native_workers_ai_package() {
    let mut harness = start(
        Input::new(Vec::new()).environment("CLOUDFLARE_ACCOUNT_ID", "acct").environment("CLOUDFLARE_API_KEY", "key"),
    )
    .await;
    let _seeded = seed(&harness, PROVIDER_ID, NATIVE_PACKAGE, None).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["cloudflare_workers_ai"], context).await;

    let method = key_method(&harness).await;
    assert_eq!(method.label.as_deref(), Some("API key"));
    assert_eq!(method.form, None);
    assert_eq!(provider(&harness).await.package, NATIVE_PACKAGE);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gives_the_native_workers_ai_package_its_account_id() {
    let mut harness = start(Input::new(Vec::new()).environment("CLOUDFLARE_ACCOUNT_ID", "acct")).await;
    let _seeded = seed(&harness, PROVIDER_ID, NATIVE_PACKAGE, None).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["cloudflare_workers_ai"], context).await;

    assert_eq!(settings(&harness).await, Some(json!({ "accountId": "acct" })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gives_the_native_workers_ai_package_the_account_id_of_a_stored_form_login() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed(&harness, PROVIDER_ID, NATIVE_PACKAGE, None).await;
    harness.credential(PROVIDER_ID, credential("secret", &[("accountId", "form-acct")])).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["cloudflare_workers_ai"], context).await;

    assert_eq!(settings(&harness).await, Some(json!({ "accountId": "form-acct" })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn follows_the_account_id_when_another_form_login_becomes_active() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed(&harness, PROVIDER_ID, NATIVE_PACKAGE, None).await;
    let first = harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: IntegrationID(PROVIDER_ID.to_owned()),
            value: credential("secret", &[("accountId", "first-acct")]),
            label: None,
            activate: None,
        })
        .unwrap();
    harness
        .services
        .credentials
        .create(CreateInput {
            id: None,
            integration_id: IntegrationID(PROVIDER_ID.to_owned()),
            value: credential("secret", &[("accountId", "second-acct")]),
            label: None,
            activate: None,
        })
        .unwrap();
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["cloudflare_workers_ai"], context).await;

    assert_eq!(settings(&harness).await, Some(json!({ "accountId": "second-acct" })));

    harness.services.credentials.activate(&first.id).unwrap();
    wait_for_setting(&harness, PROVIDER_ID, "accountId", "first-acct").await;
    assert_eq!(settings(&harness).await, Some(json!({ "accountId": "first-acct" })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preserves_a_configured_endpoint_url_instead_of_deriving_one_from_account_id() {
    let mut harness = start(Input::new(Vec::new()).environment("CLOUDFLARE_ACCOUNT_ID", "acct")).await;
    let _seeded = seed(
        &harness,
        PROVIDER_ID,
        "aisdk:test-provider",
        Some(provider_settings_from(json!({ "baseURL": "https://proxy.example/v1" }))),
    )
    .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["cloudflare_workers_ai"], context).await;

    let provider = provider(&harness).await;
    assert_eq!(provider.package, "aisdk:test-provider");
    assert_eq!(settings_json(&provider), Some(json!({ "baseURL": "https://proxy.example/v1" })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uses_configured_account_id_over_env_account_id() {
    let mut harness = start(Input::new(Vec::new()).environment("CLOUDFLARE_ACCOUNT_ID", "env-acct")).await;
    let _seeded = seed(
        &harness,
        PROVIDER_ID,
        "aisdk:test-provider",
        Some(provider_settings_from(json!({ "accountId": "configured-acct" }))),
    )
    .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["cloudflare_workers_ai"], context).await;

    let provider = provider(&harness).await;
    assert_eq!(provider.package, "aisdk:test-provider");
    assert_eq!(
        settings_json(&provider).and_then(|settings| settings.get("baseURL").cloned()),
        Some(json!("https://api.cloudflare.com/client/v4/accounts/configured-acct/ai/v1"))
    );
}

fn credential(key: &str, metadata: &[(&str, &str)]) -> CredentialValue {
    CredentialValue::Key(CredentialKey {
        r#type: CredentialKeyType::Key,
        key: key.to_owned(),
        metadata: Some(JsonMap(metadata.iter().map(|(name, value)| ((*name).to_owned(), json!(value))).collect())),
    })
}

async fn key_method(harness: &Harness) -> IntegrationKeyMethod {
    let integration = integration(harness).await;
    integration
        .methods
        .iter()
        .find_map(|method| match method {
            IntegrationMethod::Key(method) => Some(method.clone()),
            _ => None,
        })
        .expect("the key method is registered")
}

async fn integration(harness: &Harness) -> IntegrationInfo {
    harness
        .services
        .integration
        .get(&IntegrationID(PROVIDER_ID.to_owned()))
        .await
        .expect("the integration reads")
        .expect("the integration is registered")
}

async fn provider(harness: &Harness) -> opencode_contract::generated::ProviderInfo {
    harness.services.provider.get(&ProviderID(PROVIDER_ID.to_owned())).await.unwrap().unwrap()
}

async fn settings(harness: &Harness) -> Option<Value> {
    settings_json(&provider(harness).await)
}

async fn seed(
    harness: &Harness,
    id: &str,
    package: &str,
    settings: Option<ProviderSettings>,
) -> opencode_location::state::Registration {
    let id = ProviderID(id.to_owned());
    let package = package.to_owned();
    harness
        .services
        .provider
        .transform(move |editor| {
            editor.update(&id, |provider| {
                provider.package = package.clone();
                if let Some(settings) = settings.clone() {
                    provider.settings = Some(settings);
                }
            });
            Ok(())
        })
        .await
}
