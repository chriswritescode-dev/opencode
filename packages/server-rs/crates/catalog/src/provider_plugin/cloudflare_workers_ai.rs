use opencode_contract::generated::{
    CredentialValue, FormField1, FormFields1, FormStringField1, FormStringField1Type, IntegrationEnvMethod,
    IntegrationEnvMethodType, IntegrationID, IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod,
    ProviderID, ProviderSettings,
};
use serde_json::Value;

use crate::aisdk_native;
use crate::builtin::{Builtin, Services};
use crate::integration::Implementation;
use crate::provider;
use crate::provider_plugin::settings;

pub const ID: &str = "opencode.provider.cloudflare.workers.ai";

const PROVIDER_ID: &str = "cloudflare-workers-ai";

const NATIVE_PACKAGE: &str = "@opencode/ai/providers/cloudflare-workers-ai";

pub async fn register(services: &Services, context: &super::PluginContext, builtin: &mut Builtin) {
    let account_id = super::env_value(&context.env, "CLOUDFLARE_ACCOUNT_ID");
    let provider_id = ProviderID(PROVIDER_ID.to_owned());
    let configured = settings::configured_settings(services, &provider_id).await.ok().flatten();
    let form = if super::setting(configured.as_ref(), "baseURL").is_some()
        || resolve_account_id(configured.as_ref(), account_id.as_deref()).is_some_and(|value| !value.is_empty())
    {
        None
    } else {
        Some(FormFields1(vec![FormField1::String(FormStringField1 {
            key: "accountId".to_owned(),
            title: Some("Enter your Cloudflare Account ID".to_owned()),
            description: None,
            required: Some(true),
            hidden: None,
            when: None,
            r#type: FormStringField1Type::String,
            format: None,
            min_length: None,
            max_length: None,
            pattern: None,
            placeholder: Some("e.g. 1234567890abcdef1234567890abcdef".to_owned()),
            default: None,
            options: None,
            custom: None,
        })]))
    };
    let registration = services
        .integration
        .transform(move |editor| {
            for method in [
                IntegrationMethod::Env(IntegrationEnvMethod {
                    r#type: IntegrationEnvMethodType::Env,
                    names: vec![
                        "CLOUDFLARE_API_KEY".to_owned(),
                        "CLOUDFLARE_WORKERS_AI_TOKEN".to_owned(),
                        "CLOUDFLARE_API_TOKEN".to_owned(),
                    ],
                }),
                IntegrationMethod::Key(IntegrationKeyMethod {
                    r#type: IntegrationKeyMethodType::Key,
                    label: Some("API key".to_owned()),
                    form: form.clone(),
                }),
            ] {
                editor.method_update(Implementation::Method {
                    integration_id: IntegrationID(PROVIDER_ID.to_owned()),
                    method,
                });
            }
            Ok(())
        })
        .await;
    builtin.registrations.push(registration);

    let login =
        settings::credential_settings(services, builtin, &IntegrationID(PROVIDER_ID.to_owned()), |value| match value {
            CredentialValue::Key(key) => settings::metadata_settings(key.metadata.as_ref(), &["accountId"]),
            _ => settings::metadata_settings(None, &[]),
        })
        .await;

    let registration = services
        .provider
        .transform(move |editor| {
            if editor.get(&provider_id).is_none() {
                return Ok(());
            }
            let login = login.lock().expect("lock the provider settings").clone();
            let login = provider::json_object(&login);
            editor.update(&provider_id, |provider| {
                if super::setting(provider.settings.as_ref(), "baseURL").is_some() {
                    return;
                }
                let mut merged = aisdk_native::JsonMap::new();
                if let Some(current) = provider.settings.as_ref() {
                    merged.extend(provider::json_object(current));
                }
                merged.extend(login.clone());
                let resolved =
                    merged.get("accountId").and_then(Value::as_str).map(str::to_owned).or_else(|| account_id.clone());
                let Some(account_id) = resolved.filter(|value| !value.is_empty()) else {
                    return;
                };
                let mut next = provider.settings.as_ref().map(provider::json_object).unwrap_or_default();
                if provider.package == NATIVE_PACKAGE {
                    next.insert("accountId".to_owned(), Value::String(account_id));
                } else {
                    next.insert(
                        "baseURL".to_owned(),
                        Value::String(format!("https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1")),
                    );
                }
                provider.settings = provider::decode_object(next);
            });
            Ok(())
        })
        .await;
    builtin.registrations.push(registration);
}

fn resolve_account_id<'a>(settings: Option<&'a ProviderSettings>, env: Option<&'a str>) -> Option<&'a str> {
    super::setting(settings, "accountId").or(env)
}
