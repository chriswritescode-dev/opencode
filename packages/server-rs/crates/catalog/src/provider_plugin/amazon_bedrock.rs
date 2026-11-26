use std::collections::BTreeSet;
use std::path::PathBuf;

use opencode_contract::generated::{
    CredentialValue, FormField1, FormFields1, FormOption, FormStringField1, FormStringField1Type, IntegrationEnvMethod,
    IntegrationEnvMethodType, IntegrationExternalMethod, IntegrationExternalMethodType, IntegrationID,
    IntegrationKeyMethod, IntegrationKeyMethodType, IntegrationMethod, IntegrationMethodID, ProviderInfoActivation,
};

use crate::builtin::{Builtin, Services};
use crate::integration::{ConfigEnv, Implementation};
use crate::provider_plugin::settings;

pub const ID: &str = "opencode.provider.amazon.bedrock";

const INTEGRATION_ID: &str = "amazon-bedrock";

const PROVIDER_PACKAGE: &str = "@opencode/ai/providers/amazon-bedrock";

const PROFILE_METHOD_ID: &str = "aws-profile";

const CHAIN_ENV: [&str; 5] = [
    "AWS_PROFILE",
    "AWS_ACCESS_KEY_ID",
    "AWS_WEB_IDENTITY_TOKEN_FILE",
    "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
    "AWS_CONTAINER_CREDENTIALS_FULL_URI",
];

pub async fn register(services: &Services, context: &super::PluginContext, builtin: &mut Builtin) {
    let profiles = aws_profiles(&context.env);
    let description = if profiles.is_empty() {
        "No profiles found in your AWS configuration.".to_owned()
    } else {
        format!(
            "Found {} profile{} in your AWS configuration.",
            profiles.len(),
            if profiles.len() == 1 { "" } else { "s" }
        )
    };
    let options = profiles
        .iter()
        .map(|profile| FormOption { value: profile.clone(), label: profile.clone(), description: None })
        .collect::<Vec<_>>();

    let integration_registration = services
        .integration
        .transform(move |editor| {
            let integration_id = IntegrationID(INTEGRATION_ID.to_owned());
            editor.method_update(Implementation::Method {
                integration_id: integration_id.clone(),
                method: IntegrationMethod::Env(IntegrationEnvMethod {
                    r#type: IntegrationEnvMethodType::Env,
                    names: vec!["AWS_BEARER_TOKEN_BEDROCK".to_owned()],
                }),
            });
            editor.method_update(Implementation::Method {
                integration_id: integration_id.clone(),
                method: IntegrationMethod::Key(IntegrationKeyMethod {
                    r#type: IntegrationKeyMethodType::Key,
                    label: Some("Bedrock API key".to_owned()),
                    form: None,
                }),
            });
            editor.method_update(Implementation::Method {
                integration_id,
                method: IntegrationMethod::External(IntegrationExternalMethod {
                    id: IntegrationMethodID(PROFILE_METHOD_ID.to_owned()),
                    r#type: IntegrationExternalMethodType::External,
                    label: "AWS profile (SSO or named profile)".to_owned(),
                    form: Some(FormFields1(vec![FormField1::String(FormStringField1 {
                        key: "profile".to_owned(),
                        title: Some("AWS profile".to_owned()),
                        description: Some(description.clone()),
                        required: Some(true),
                        hidden: None,
                        when: None,
                        r#type: FormStringField1Type::String,
                        format: None,
                        min_length: Some(1.0),
                        max_length: None,
                        pattern: Some("\\S".to_owned()),
                        placeholder: Some("Profile name".to_owned()),
                        default: None,
                        options: Some(options.clone()),
                        custom: Some(true),
                    })])),
                }),
            });
            Ok(())
        })
        .await;
    builtin.registrations.push(integration_registration);

    let login =
        settings::credential_settings(
            services,
            builtin,
            &IntegrationID(INTEGRATION_ID.to_owned()),
            |value| match value {
                CredentialValue::External(external) if external.method_id.0 == PROFILE_METHOD_ID => {
                    settings::metadata_settings(external.metadata.as_ref(), &["profile"])
                }
                _ => settings::metadata_settings(None, &[]),
            },
        )
        .await;

    let env = context.env.clone();
    let provider_registration = services
        .provider
        .transform(move |editor| {
            let login = crate::provider::json_object(&login.lock().expect("lock provider settings").clone());
            for item in editor.list() {
                if !item.provider.package.starts_with(PROVIDER_PACKAGE) {
                    continue;
                }
                editor.update(&item.provider.id, |provider| {
                    let mut merged = provider.settings.as_ref().map(crate::provider::json_object).unwrap_or_default();
                    let integration =
                        provider.integration_id.as_ref().map(|id| id.0.as_str()).unwrap_or(provider.id.0.as_str());
                    if integration == INTEGRATION_ID {
                        for (key, value) in &login {
                            merged.insert(key.clone(), value.clone());
                        }
                    }
                    let chain = merged.get("profile").is_some_and(serde_json::Value::is_string)
                        || CHAIN_ENV.iter().any(|name| (env)(name).is_some_and(|value| !value.is_empty()));
                    if chain && provider.activation == ProviderInfoActivation::Auto {
                        provider.activation = ProviderInfoActivation::Enabled;
                    }
                    let region = (env)("AWS_REGION")
                        .or_else(|| (env)("AWS_DEFAULT_REGION"))
                        .map(|value| value.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "us-east-1".to_owned());
                    if !merged.get("region").is_some_and(serde_json::Value::is_string) {
                        merged.insert("region".to_owned(), serde_json::Value::String(region));
                    }
                    if !merged.get("baseURL").is_some_and(serde_json::Value::is_string)
                        && let Some(endpoint) = merged.get("endpoint").and_then(serde_json::Value::as_str)
                    {
                        merged.insert("baseURL".to_owned(), serde_json::Value::String(endpoint.to_owned()));
                    }
                    merged.remove("endpoint");
                    provider.settings = crate::provider::decode_object(merged);
                });
            }
            Ok(())
        })
        .await;
    builtin.registrations.push(provider_registration);
}

fn aws_profiles(env: &ConfigEnv) -> Vec<String> {
    let home = (env)("HOME").map(PathBuf::from);
    let paths = [
        (env)("AWS_CONFIG_FILE")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(".aws").join("config"))),
        (env)("AWS_SHARED_CREDENTIALS_FILE")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(".aws").join("credentials"))),
    ];
    let mut profiles = BTreeSet::new();
    for (index, path) in paths.into_iter().enumerate() {
        let Some(content) = path.and_then(|path| std::fs::read_to_string(path).ok()) else {
            continue;
        };
        for line in content.lines() {
            let Some(name) = section_name(line).and_then(|section| profile_name(index, section)) else {
                continue;
            };
            if !name.is_empty() {
                profiles.insert(name.to_owned());
            }
        }
    }
    profiles.into_iter().collect()
}

fn section_name(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('[')?;
    Some(rest[..rest.find(']')?].trim())
}

fn profile_name(index: usize, section: &str) -> Option<&str> {
    if index == 1 || section == "default" {
        return Some(section);
    }
    section.strip_prefix("profile ").map(str::trim)
}
