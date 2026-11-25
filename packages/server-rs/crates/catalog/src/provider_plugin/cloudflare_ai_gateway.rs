use opencode_contract::generated::{
    CredentialValue, FormField1, FormFields1, FormStringField1, FormStringField1Type, IntegrationID,
    IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod, ProviderID,
};
use serde_json::Value;

use crate::aisdk_native;
use crate::builtin::{Builtin, Services};
use crate::integration::Implementation;
use crate::provider;
use crate::provider_plugin::settings;

pub const ID: &str = "opencode.provider.cloudflare.ai.gateway";

const PROVIDER_ID: &str = "cloudflare-ai-gateway";

pub async fn register(services: &Services, context: &super::PluginContext, builtin: &mut Builtin) {
    let account_id = super::env_value(&context.env, "CLOUDFLARE_ACCOUNT_ID");
    let gateway_id = super::env_value(&context.env, "CLOUDFLARE_GATEWAY_ID");
    let provider_id = ProviderID(PROVIDER_ID.to_owned());
    let configured = settings::configured_settings(services, &provider_id).await.ok().flatten();
    let fields = if super::setting(configured.as_ref(), "baseURL").is_some() {
        Vec::new()
    } else {
        let mut fields = Vec::new();
        if account_id.is_none() && super::setting(configured.as_ref(), "accountId").is_none() {
            fields.push(FormField1::String(string_field(
                "accountId",
                "Enter your Cloudflare Account ID",
                "e.g. 1234567890abcdef1234567890abcdef",
            )));
        }
        if gateway_id.is_none() && super::setting(configured.as_ref(), "gatewayId").is_none() {
            fields.push(FormField1::String(string_field(
                "gatewayId",
                "Enter your Cloudflare AI Gateway ID",
                "e.g. my-gateway",
            )));
        }
        fields
    };
    let form = (!fields.is_empty()).then_some(FormFields1(fields));
    let registration = services
        .integration
        .transform(move |editor| {
            editor.method_update(Implementation::Method {
                integration_id: IntegrationID(PROVIDER_ID.to_owned()),
                method: IntegrationMethod::Key(IntegrationKeyMethod {
                    r#type: IntegrationKeyMethodType::Key,
                    label: Some("Gateway API token".to_owned()),
                    form: form.clone(),
                }),
            });
            Ok(())
        })
        .await;
    builtin.registrations.push(registration);

    let login =
        settings::credential_settings(services, builtin, &IntegrationID(PROVIDER_ID.to_owned()), |value| match value {
            CredentialValue::Key(key) => {
                settings::metadata_settings(key.metadata.as_ref(), &["accountId", "gatewayId"])
            }
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
            if account_id.is_none() && gateway_id.is_none() && login.is_empty() {
                return Ok(());
            }
            editor.update(&provider_id, |provider| {
                if super::setting(provider.settings.as_ref(), "baseURL").is_some() {
                    return;
                }
                let mut merged = aisdk_native::JsonMap::new();
                if let Some(account_id) = &account_id {
                    merged.insert("accountId".to_owned(), Value::String(account_id.clone()));
                }
                if let Some(gateway_id) = &gateway_id {
                    merged.insert("gatewayId".to_owned(), Value::String(gateway_id.clone()));
                }
                if let Some(current) = provider.settings.as_ref() {
                    merged.extend(provider::json_object(current));
                }
                merged.extend(login.clone());
                provider.settings = Some(provider::decode_object(merged));
            });
            Ok(())
        })
        .await;
    builtin.registrations.push(registration);
}

fn string_field(key: &str, title: &str, placeholder: &str) -> FormStringField1 {
    FormStringField1 {
        key: key.to_owned(),
        title: Some(title.to_owned()),
        description: None,
        required: Some(true),
        hidden: None,
        when: None,
        r#type: FormStringField1Type::String,
        format: None,
        min_length: None,
        max_length: None,
        pattern: None,
        placeholder: Some(placeholder.to_owned()),
        default: None,
        options: None,
        custom: None,
    }
}
