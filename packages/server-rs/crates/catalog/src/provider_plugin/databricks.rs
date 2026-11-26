use opencode_contract::generated::{
    IntegrationEnvMethod, IntegrationEnvMethodType, IntegrationID, IntegrationMethod, ModelCacheFormat,
    ModelCompatibility, ProviderID,
};

use crate::builtin::{Builtin, Services};
use crate::integration::Implementation;

pub const ID: &str = "opencode.provider.databricks";

const PROVIDER_ID: &str = "databricks";

const HOST_TEMPLATE: &str = "${DATABRICKS_HOST}";

pub async fn register(services: &Services, context: &super::PluginContext, builtin: &mut Builtin) {
    let integration_registration = services
        .integration
        .transform(|editor| {
            editor.method_update(Implementation::Method {
                integration_id: IntegrationID(PROVIDER_ID.to_owned()),
                method: IntegrationMethod::Env(IntegrationEnvMethod {
                    r#type: IntegrationEnvMethodType::Env,
                    names: vec!["DATABRICKS_TOKEN".to_owned()],
                }),
            });
            Ok(())
        })
        .await;
    builtin.registrations.push(integration_registration);

    let host = (context.env)("DATABRICKS_HOST")
        .map(|value| normalize_host(&value.to_string_lossy()))
        .filter(|host| !host.is_empty());
    let provider_registration = services
        .provider
        .transform(move |editor| {
            let Some(host) = host.as_deref() else {
                return Ok(());
            };
            let provider_id = ProviderID(PROVIDER_ID.to_owned());
            let Some(base_url) = editor
                .get(&provider_id)
                .and_then(|definition| super::base_url(&definition.provider))
                .filter(|base_url| base_url.contains(HOST_TEMPLATE))
                .map(|base_url| base_url.replace(HOST_TEMPLATE, host))
            else {
                return Ok(());
            };
            editor.update(&provider_id, |provider| {
                let Some(settings) = provider.settings.as_mut() else {
                    return;
                };
                if let Some((_, value)) = settings.rest.0.iter_mut().find(|(key, _)| key == "baseURL") {
                    *value = serde_json::Value::String(base_url);
                }
            });
            Ok(())
        })
        .await;
    builtin.registrations.push(provider_registration);

    let model_registration = services
        .model
        .transform(|editor| {
            for model in editor.list(Some(&ProviderID(PROVIDER_ID.to_owned()))) {
                let api_id = if model.model_id.is_empty() { model.id.as_str() } else { model.model_id.as_str() };
                if !api_id.contains("claude") {
                    continue;
                }
                let compatibility = model.compatibility.get_or_insert(ModelCompatibility {
                    reasoning_field: None,
                    require_reasoning: None,
                    max_tokens_field: None,
                    require_finish_reason: None,
                    require_assistant_after_tool: None,
                    supports_prompt_cache_key: None,
                    stream_usage: None,
                    cache_format: None,
                    supports_thinking_block_binding: None,
                    supports_effort_updates: None,
                    media_input: None,
                });
                if compatibility.cache_format.is_none() {
                    compatibility.cache_format = Some(ModelCacheFormat::Anthropic);
                }
            }
            Ok(())
        })
        .await;
    builtin.registrations.push(model_registration);
}

fn normalize_host(host: &str) -> String {
    strip_scheme(host).trim_end_matches('/').to_owned()
}

fn strip_scheme(host: &str) -> &str {
    ["https://", "http://"]
        .into_iter()
        .find_map(|scheme| {
            let prefix = host.get(..scheme.len())?;
            prefix.eq_ignore_ascii_case(scheme).then_some(&host[scheme.len()..])
        })
        .unwrap_or(host)
}
