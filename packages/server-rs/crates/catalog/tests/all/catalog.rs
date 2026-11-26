use opencode_catalog::builtin::{self, Builtin, Services};
use opencode_catalog::credential::CreateInput;
use opencode_contract::codec::{decode_json, encode_json};
use opencode_contract::generated::requests::CredentialCreateInput;
use opencode_contract::generated::{
    CredentialID, CredentialValue, IntegrationID, ModelType, ModelTypeString, ModelTypeString2, ModelTypeString3,
    ModelTypeString4, ProviderID, ProviderInfoActivation,
};
use serde_json::{Map, Value};

use crate::builtin::{Harness, Input, start};
use crate::corpus::area;

const PLUGINS: [&str; 4] = ["wellknown", "models_dev", "config_provider", "config_policy"];

fn value<T: opencode_contract::codec::Encode>(input: &T) -> Value {
    serde_json::from_str(&encode_json(input)).expect("the wire encoding is JSON")
}

fn values<T: opencode_contract::codec::Encode>(input: &[T]) -> Value {
    Value::Array(input.iter().map(value).collect())
}

async fn register(harness: &mut Harness, plugins: &[String]) {
    let mut builtin = Builtin::default();
    for name in PLUGINS {
        if !plugins.iter().any(|plugin| plugin == name) {
            continue;
        }
        match name {
            "wellknown" => builtin::wellknown::register(&harness.services, &mut builtin).await,
            "models_dev" => builtin::models_dev::register(&harness.services, &mut builtin).await,
            "config_provider" => builtin::config_provider::register(&harness.services, &mut builtin).await,
            "config_policy" => builtin::config_policy::register(&harness.services, &mut builtin).await,
            _ => unreachable!(),
        }
    }
    harness.record(builtin);

    let provider_plugins: Vec<&str> =
        plugins.iter().map(String::as_str).filter(|name| !PLUGINS.contains(name)).collect();
    if provider_plugins.is_empty() {
        return;
    }
    let context = crate::provider_plugin::support::plugin_context(harness);
    harness.register_provider_plugins(&provider_plugins, context).await;
}

async fn observe(services: &Services, activate: bool) -> Value {
    let registration = match activate {
        true => Some(
            services
                .provider
                .transform(|editor| {
                    for record in editor.list() {
                        editor.update(&record.provider.id, |provider| {
                            provider.activation = ProviderInfoActivation::Enabled;
                        });
                    }
                    Ok(())
                })
                .await,
        ),
        false => None,
    };
    let value = observe_registries(services).await;
    drop(registration);
    value
}

async fn observe_registries(services: &Services) -> Value {
    let providers = services.provider.all().await.expect("the providers");
    let models = services.model.all().await.expect("the models");
    let available = services.model.available(None).await.expect("the available models");
    let typed = Value::Object(Map::from_iter([
        (
            "chat".to_owned(),
            values(
                &services
                    .model
                    .available(Some(&[ModelType::String(ModelTypeString::Chat)]))
                    .await
                    .expect("chat models"),
            ),
        ),
        (
            "decision".to_owned(),
            values(
                &services
                    .model
                    .available(Some(&[ModelType::String2(ModelTypeString2::Decision)]))
                    .await
                    .expect("decision models"),
            ),
        ),
        (
            "image".to_owned(),
            values(
                &services
                    .model
                    .available(Some(&[ModelType::String3(ModelTypeString3::Image)]))
                    .await
                    .expect("image models"),
            ),
        ),
        (
            "video".to_owned(),
            values(
                &services
                    .model
                    .available(Some(&[ModelType::String4(ModelTypeString4::Video)]))
                    .await
                    .expect("video models"),
            ),
        ),
    ]));
    let default = services.model.default_model().await.expect("the default model");
    let mut small = Map::new();
    for provider in &providers {
        let model = services.model.small(&ProviderID(provider.id.0.clone())).await.expect("the small model");
        small.insert(provider.id.0.clone(), model.as_ref().map_or(Value::Null, value));
    }
    Value::Object(Map::from_iter([
        (
            "providers".to_owned(),
            Value::Object(Map::from_iter([
                ("all".to_owned(), values(&providers)),
                ("available".to_owned(), values(&services.provider.available().await.expect("available providers"))),
            ])),
        ),
        (
            "models".to_owned(),
            Value::Object(Map::from_iter([
                ("all".to_owned(), values(&models)),
                ("available".to_owned(), values(&available)),
                ("typed".to_owned(), typed),
                ("default".to_owned(), default.as_ref().map_or(Value::Null, value)),
                ("small".to_owned(), Value::Object(small)),
            ])),
        ),
        ("integrations".to_owned(), values(&services.integration.list().await.expect("the integrations"))),
    ]))
}

#[tokio::test]
async fn catalog_corpus_matches_the_pinned_typescript() {
    let cases = area("catalog");
    assert_eq!(cases.len(), 39, "the corpus holds every catalog case");

    for (name, case) in &cases {
        let input = &case["input"];
        let entries = input["entries"].as_array().cloned().unwrap_or_default();
        let mut prepared = Input::new(entries);
        if let Some(models) = input.get("models").filter(|value| !value.is_null()) {
            prepared = prepared.models(models.clone());
        }
        if let Some(environment) = input["environment"].as_object() {
            for (key, value) in environment {
                prepared = prepared.environment(key, value.as_str().expect("an environment value"));
            }
        }
        let mut harness = start(prepared).await;
        if let Some(credentials) = input["credentials"].as_array() {
            for credential in credentials {
                let create: CredentialCreateInput =
                    decode_json(&credential.to_string()).expect("the corpus credential decodes");
                let value: CredentialValue =
                    decode_json(&encode_json(&create.value)).expect("the corpus credential value decodes");
                harness
                    .services
                    .credentials
                    .create(CreateInput {
                        id: create.id.map(|id| CredentialID(id.0)),
                        integration_id: IntegrationID(create.integration_id.0),
                        value,
                        label: create.label,
                        activate: create.activate,
                    })
                    .expect("create the corpus credential");
            }
        }
        let plugins: Vec<String> = input["plugins"]
            .as_array()
            .expect("plugins")
            .iter()
            .map(|value| value.as_str().expect("a plugin").to_owned())
            .collect();
        register(&mut harness, &plugins).await;
        let actual = observe(&harness.services, input["activate"].as_bool().unwrap_or(false)).await;
        assert_eq!(actual, case["expected"], "case {name}");
    }
}
