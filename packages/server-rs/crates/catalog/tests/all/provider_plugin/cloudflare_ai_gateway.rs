use opencode_catalog::integration::KeyInput;
use opencode_contract::codec::{JsonMap, encode_json};
use opencode_contract::generated::{
    CredentialKey, CredentialKeyType, CredentialValue, FormFields1, FormValue, IntegrationID, IntegrationInfo,
    IntegrationKeyMethod, IntegrationMethod, ProviderID, ProviderSettings,
};
use serde_json::{Value, json};

use crate::builtin::{Harness, Input, start};
use crate::provider_plugin::support::{plugin_context, provider_settings_from, settings_json, wait_for_setting};

const PROVIDER_ID: &str = "cloudflare-ai-gateway";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registers_account_and_gateway_forms_when_the_environment_does_not_provide_them() {
    let mut harness = start(Input::new(Vec::new())).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["cloudflare_ai_gateway"], context).await;

    let method = key_method(&harness).await;
    assert_eq!(method.label.as_deref(), Some("Gateway API token"));
    let form = method.form.as_ref().expect("the key method form");
    assert_eq!(form.0.len(), 2);
    assert_eq!(field_json(form, 0), account_field());
    assert_eq!(field_json(form, 1), gateway_field());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uses_environment_account_and_gateway_values_without_requesting_form_fields() {
    let mut harness = start(
        Input::new(Vec::new())
            .environment("CLOUDFLARE_ACCOUNT_ID", "account")
            .environment("CLOUDFLARE_GATEWAY_ID", "gateway"),
    )
    .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["cloudflare_ai_gateway"], context).await;

    let method = key_method(&harness).await;
    assert_eq!(method.label.as_deref(), Some("Gateway API token"));
    assert_eq!(method.form, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_only_the_gateway_id_when_the_environment_provides_the_account_id() {
    let mut harness = start(Input::new(Vec::new()).environment("CLOUDFLARE_ACCOUNT_ID", "account")).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["cloudflare_ai_gateway"], context).await;

    let method = key_method(&harness).await;
    assert_eq!(method.label.as_deref(), Some("Gateway API token"));
    let form = method.form.as_ref().expect("the key method form");
    assert_eq!(form.0.len(), 1);
    assert_eq!(field_json(form, 0), gateway_field());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gives_the_provider_its_environment_account_and_gateway_ids() {
    let mut harness = start(
        Input::new(Vec::new())
            .environment("CLOUDFLARE_ACCOUNT_ID", "account")
            .environment("CLOUDFLARE_GATEWAY_ID", "gateway"),
    )
    .await;
    let _seeded = seed(&harness, PROVIDER_ID, "", None).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["cloudflare_ai_gateway"], context).await;

    assert_eq!(settings(&harness).await, Some(json!({ "accountId": "account", "gatewayId": "gateway" })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prefers_configured_ids_over_environment_ids() {
    let mut harness = start(
        Input::new(Vec::new())
            .environment("CLOUDFLARE_ACCOUNT_ID", "env-account")
            .environment("CLOUDFLARE_GATEWAY_ID", "env-gateway"),
    )
    .await;
    let _seeded = seed(
        &harness,
        PROVIDER_ID,
        "",
        Some(provider_settings_from(json!({ "accountId": "configured-account", "gatewayId": "configured-gateway" }))),
    )
    .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["cloudflare_ai_gateway"], context).await;

    assert_eq!(
        settings(&harness).await,
        Some(json!({ "accountId": "configured-account", "gatewayId": "configured-gateway" }))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gives_the_provider_the_account_and_gateway_ids_of_a_stored_form_login() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed(&harness, PROVIDER_ID, "", None).await;
    harness
        .credential(PROVIDER_ID, credential("token", &[("accountId", "form-account"), ("gatewayId", "form-gateway")]))
        .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["cloudflare_ai_gateway"], context).await;

    assert_eq!(settings(&harness).await, Some(json!({ "accountId": "form-account", "gatewayId": "form-gateway" })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn applies_the_account_and_gateway_ids_of_a_form_login_made_after_startup() {
    let mut harness = start(Input::new(Vec::new())).await;
    let _seeded = seed(&harness, PROVIDER_ID, "", None).await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["cloudflare_ai_gateway"], context).await;

    harness
        .services
        .integration
        .connection()
        .key(KeyInput {
            integration_id: IntegrationID(PROVIDER_ID.to_owned()),
            key: "token".to_owned(),
            answer: Some(JsonMap(vec![
                ("accountId".to_owned(), FormValue::String("form-account".to_owned())),
                ("gatewayId".to_owned(), FormValue::String("form-gateway".to_owned())),
            ])),
            label: None,
        })
        .await
        .unwrap();

    wait_for_setting(&harness, PROVIDER_ID, "accountId", "form-account").await;
    assert_eq!(settings(&harness).await, Some(json!({ "accountId": "form-account", "gatewayId": "form-gateway" })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn leaves_a_configured_endpoint_url_untouched() {
    let mut harness = start(
        Input::new(Vec::new())
            .environment("CLOUDFLARE_ACCOUNT_ID", "account")
            .environment("CLOUDFLARE_GATEWAY_ID", "gateway"),
    )
    .await;
    let _seeded =
        seed(&harness, PROVIDER_ID, "", Some(provider_settings_from(json!({ "baseURL": "https://proxy.example/v1" }))))
            .await;
    let context = plugin_context(&harness);
    harness.register_provider_plugins(&["cloudflare_ai_gateway"], context).await;

    assert_eq!(settings(&harness).await, Some(json!({ "baseURL": "https://proxy.example/v1" })));
}

fn account_field() -> Value {
    json!({
        "key": "accountId",
        "title": "Enter your Cloudflare Account ID",
        "required": true,
        "type": "string",
        "placeholder": "e.g. 1234567890abcdef1234567890abcdef",
    })
}

fn gateway_field() -> Value {
    json!({
        "key": "gatewayId",
        "title": "Enter your Cloudflare AI Gateway ID",
        "required": true,
        "type": "string",
        "placeholder": "e.g. my-gateway",
    })
}

fn field_json(form: &FormFields1, index: usize) -> Value {
    serde_json::from_str(&encode_json(&form.0[index])).expect("the form field json")
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

async fn settings(harness: &Harness) -> Option<Value> {
    let provider = harness.services.provider.get(&ProviderID(PROVIDER_ID.to_owned())).await.unwrap().unwrap();
    settings_json(&provider)
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
