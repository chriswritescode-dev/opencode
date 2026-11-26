mod aisdk_native;
mod model;
mod provider;

use opencode_ai::{Auth, Env, ProviderPackages, RuntimeModel, Settings};
use opencode_config::entry::Entry;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::{ModelInfo, ModelRef, ProviderInfoActivation};
use serde_json::{Map, Value};

use provider::ProviderState;

pub struct Catalog {
    providers: Vec<ProviderState>,
    default: Option<(String, String)>,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("Unsupported package for {provider_id}/{model_id}: {package}")]
    UnsupportedPackage { provider_id: String, model_id: String, package: String },
    #[error("Model not found: {provider_id}/{model_id}")]
    NotFound { provider_id: String, model_id: String },
    #[error("{0}")]
    Construction(String),
}

impl Catalog {
    pub fn from_entries(entries: &[Entry]) -> Self {
        let builtins = ProviderPackages::builtin().specifiers();
        let mut providers = Vec::new();
        let mut default = None;
        for entry in entries {
            let Entry::Document(document) = entry else {
                continue;
            };
            let Ok(info) = serde_json::from_str::<Value>(&encode_json(&document.info)) else {
                continue;
            };
            if let Some(model) = info.get("model").filter(|model| !model.is_null())
                && let Some(selection) = parse_selection(model)
            {
                default = Some(selection);
            }
            let Some(configured) = info.get("providers").and_then(Value::as_object) else {
                continue;
            };
            let mut limits = Vec::new();
            for (provider_id, item) in configured {
                for (model_id, limit) in provider::apply_provider(&mut providers, provider_id, item, &builtins) {
                    limits.push((provider_id.clone(), model_id, limit));
                }
            }
            for (provider_id, model_id, limit) in limits {
                provider::apply_limit(&mut providers, &provider_id, &model_id, &limit);
            }
        }
        Self { providers, default }
    }

    pub fn models(&self, types: Option<&[String]>) -> Vec<ModelInfo> {
        let types = match types {
            None => vec!["chat".to_string()],
            Some([]) => vec!["chat".to_string()],
            Some(types) => {
                let mut unique = Vec::new();
                for value in types {
                    if !unique.contains(value) {
                        unique.push(value.clone());
                    }
                }
                unique
            }
        };
        let mut models = Vec::new();
        for r#type in types {
            let mut group: Vec<ModelInfo> = self
                .providers
                .iter()
                .flat_map(|provider| {
                    provider
                        .models
                        .iter()
                        .filter(|model| model.enabled && model.r#type.as_deref().unwrap_or("chat") == r#type)
                        .map(|model| provider.info(model))
                })
                .collect();
            group.sort_by(|left, right| right.time.released.total_cmp(&left.time.released));
            models.extend(group);
        }
        models
    }

    pub fn default_model(&self) -> Option<ModelInfo> {
        if let Some((provider_id, model_id)) = &self.default
            && let Some(provider) = self.providers.iter().find(|provider| provider.id == *provider_id)
            && let Some(model) = provider.find_model(model_id)
            && model.enabled
        {
            return Some(provider.info(model));
        }
        self.models(None).into_iter().find(supports_text)
    }

    pub fn runtime_model(&self, reference: &ModelRef, env: &dyn Env) -> Result<RuntimeModel, CatalogError> {
        let provider =
            self.providers.iter().find(|provider| provider.id == reference.provider_id.0).ok_or_else(|| {
                CatalogError::NotFound { provider_id: reference.provider_id.0.clone(), model_id: reference.id.clone() }
            })?;
        let model = provider.find_model(&reference.id).ok_or_else(|| CatalogError::NotFound {
            provider_id: reference.provider_id.0.clone(),
            model_id: reference.id.clone(),
        })?;
        let package = model.package.clone().or_else(|| provider.package.clone());
        let unsupported = || CatalogError::UnsupportedPackage {
            provider_id: reference.provider_id.0.clone(),
            model_id: reference.id.clone(),
            package: package.clone().unwrap_or_else(|| "unknown".to_string()),
        };
        let Some(specifier) =
            aisdk_native::package_name(package.as_deref()).filter(|specifier| !aisdk_native::is_aisdk(specifier))
        else {
            return Err(unsupported());
        };
        let builtins = ProviderPackages::builtin();
        let specifiers = builtins.specifiers();
        let settings = provider.runtime_settings(model).unwrap_or_default();
        let context = aisdk_native::Context {
            provider_id: &provider.id,
            model_id: Some(&model.model_id),
            shape: None,
            settings: Some(&settings),
            builtins: &specifiers,
        };
        let Some(native) = aisdk_native::resolve(&specifier, &context) else {
            return Err(unsupported());
        };
        let mut native_settings = settings.clone();
        for key in ["timeout", "headerTimeout", "chunkTimeout", "compaction", "cache", "transport", "encoding", "tls"] {
            native_settings.remove(key);
        }
        if let Some(base_url) = native_settings.get("baseURL").and_then(Value::as_str).map(str::to_string) {
            let prepared = substitute(&base_url, &native_settings, env).map_err(|variables| {
                CatalogError::Construction(format!("unresolved provider variables: {}", variables.join(", ")))
            })?;
            native_settings.insert("baseURL".to_string(), Value::String(prepared));
        }
        if let Some(canonical) = &provider.canonical {
            native_settings.insert("provider".to_string(), Value::String(canonical.clone()));
        }
        let mut runtime = builtins
            .model(&native, &model.model_id, &Settings::new(native_settings), env)
            .map_err(|error| CatalogError::Construction(error.to_string()))?;
        if provider.activation == ProviderInfoActivation::Enabled
            && !has_configured_auth(&settings)
            && uses_api_key_auth(&specifier)
            && let RuntimeModel::Language(language) = &mut runtime
        {
            language.route.auth = Auth::None;
        }
        Ok(runtime)
    }
}

fn parse_selection(value: &Value) -> Option<(String, String)> {
    match value {
        Value::String(value) => {
            let (provider_id, rest) = value.split_once('/')?;
            let model_id = rest.split('#').next()?;
            if provider_id.is_empty() || model_id.is_empty() {
                return None;
            }
            Some((provider_id.to_string(), model_id.to_string()))
        }
        Value::Object(value) => {
            Some((value.get("providerID")?.as_str()?.to_string(), value.get("model")?.as_str()?.to_string()))
        }
        _ => None,
    }
}

fn supports_text(model: &ModelInfo) -> bool {
    let accepts = |values: &[String]| values.is_empty() || values.iter().any(|value| value.starts_with("text"));
    accepts(&model.capabilities.input) && accepts(&model.capabilities.output)
}

fn substitute(base_url: &str, settings: &Map<String, Value>, env: &dyn Env) -> Result<String, Vec<String>> {
    let mut prepared = String::new();
    let mut rest = base_url;
    let mut unresolved: Vec<String> = Vec::new();
    while let Some(start) = rest.find("${") {
        prepared.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            prepared.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let name = &after[..end];
        let value = if name == "AWS_REGION" {
            settings.get("region").and_then(Value::as_str).map(str::to_string).or_else(|| env.var(name))
        } else {
            env.var(name)
        };
        match value {
            Some(value) => prepared.push_str(&value),
            None => {
                prepared.push_str(&rest[start..start + 2 + end + 1]);
                if !unresolved.iter().any(|existing| existing == name) {
                    unresolved.push(name.to_string());
                }
            }
        }
        rest = &after[end + 1..];
    }
    prepared.push_str(rest);
    if unresolved.is_empty() { Ok(prepared) } else { Err(unresolved) }
}

fn has_configured_auth(settings: &Map<String, Value>) -> bool {
    ["apiKey", "gatewayApiKey", "authToken", "accessToken"]
        .iter()
        .any(|key| settings.get(*key).and_then(Value::as_str).is_some_and(|value| !value.is_empty()))
}

fn uses_api_key_auth(specifier: &str) -> bool {
    const PACKAGES: [&str; 33] = [
        "@ai-sdk/openai",
        "@ai-sdk/anthropic",
        "@ai-sdk/cerebras",
        "@ai-sdk/deepinfra",
        "@ai-sdk/openai-compatible",
        "@ai-sdk/google",
        "@ai-sdk/groq",
        "@ai-sdk/mistral",
        "@ai-sdk/togetherai",
        "@ai-sdk/xai",
        "@openrouter/ai-sdk-provider",
        "@ai-sdk/azure",
        "@opencode/ai/providers/openai",
        "@opencode/ai/providers/anthropic",
        "@opencode/ai/providers/anthropic-compatible",
        "@opencode/ai/providers/baseten",
        "@opencode/ai/providers/cerebras",
        "@opencode/ai/providers/cloudflare-ai-gateway",
        "@opencode/ai/providers/cloudflare-workers-ai",
        "@opencode/ai/providers/deepinfra",
        "@opencode/ai/providers/deepseek",
        "@opencode/ai/providers/fireworks",
        "@opencode/ai/providers/openai-compatible",
        "@opencode/ai/providers/openai-compatible/decisions",
        "@opencode/ai/providers/google",
        "@opencode/ai/providers/google/interactions",
        "@opencode/ai/providers/groq",
        "@opencode/ai/providers/mistral",
        "@opencode/ai/providers/togetherai",
        "@opencode/ai/providers/vercel-ai-gateway",
        "@opencode/ai/providers/xai",
        "@opencode/ai/providers/openrouter",
        "@opencode/ai/providers/azure",
    ];
    PACKAGES.contains(&specifier)
        || specifier.starts_with("@opencode/ai/providers/openai/")
        || specifier.starts_with("@opencode/ai/providers/azure/")
}

#[cfg(test)]
mod tests {
    use opencode_ai::MapEnv;
    use opencode_contract::generated::ProviderID;

    use super::*;
    use crate::legacy::model::ModelState;
    use crate::legacy::provider::ProviderState;

    #[test]
    fn runtime_model_keeps_env_auth_for_a_provider_that_is_not_explicitly_enabled() {
        let mut provider = ProviderState::new("anthropic-test");
        provider.package = Some("@ai-sdk/anthropic".to_string());
        provider.models.push(ModelState::new("test-model"));
        let catalog = Catalog { providers: vec![provider], default: None };
        let reference = ModelRef {
            id: "test-model".to_string(),
            provider_id: ProviderID("anthropic-test".to_string()),
            variant: None,
        };
        let env = MapEnv::new([("ANTHROPIC_API_KEY".to_string(), "env-key".to_string())]);
        let runtime = catalog.runtime_model(&reference, &env).expect("runtime model");
        let RuntimeModel::Language(model) = runtime else {
            panic!("expected a language model");
        };
        assert!(!matches!(model.route.auth, Auth::None));
    }
}
