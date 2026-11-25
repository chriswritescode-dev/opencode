use opencode_ai::{Auth, MapEnv, RuntimeModel};
use opencode_catalog::{Catalog, CatalogError};
use opencode_config::entry::{Document, Entry, Origin};
use opencode_contract::codec::{decode_json, encode_json};
use opencode_contract::generated::{ConfigEntryDocumentInfo, ModelRef, ProviderID};
use serde_json::{Value, json};

fn document(info: &str) -> Entry {
    Entry::Document(Document {
        path: None,
        info: decode_json::<ConfigEntryDocumentInfo>(info).expect("document decodes"),
        origin: Origin::File,
    })
}

fn base_config() -> &'static str {
    r#"{"model":"openai-test/test-model","providers":{"openai-test":{"name":"OpenAI Test","package":"@ai-sdk/openai-compatible","settings":{"apiKey":"parity","baseURL":"http://127.0.0.1:9/v1"},"models":{"test-model":{"name":"Test Model","capabilities":{"tools":true,"input":["text"],"output":["text"]},"cost":{"input":0,"output":0},"limit":{"context":100000,"output":10000}}}},"anthropic-test":{"name":"Anthropic Test","package":"@ai-sdk/anthropic","settings":{"apiKey":"parity","baseURL":"http://127.0.0.1:9/v1"},"models":{"test-model":{"name":"Test Model","capabilities":{"tools":true,"input":["text"],"output":["text"]},"cost":{"input":0,"output":0},"limit":{"context":100000,"output":10000}}}}}}"#
}

fn expected_models() -> Value {
    json!([
        {
            "capabilities": { "input": ["text"], "output": ["text"], "tools": true },
            "cost": [{ "cache": { "read": 0, "write": 0 }, "input": 0, "output": 0 }],
            "enabled": true,
            "id": "test-model",
            "limit": { "context": 100000, "output": 10000 },
            "modelID": "test-model",
            "name": "Test Model",
            "package": "@opencode/ai/providers/openai-compatible",
            "providerID": "openai-test",
            "settings": {
                "apiKey": "parity",
                "baseURL": "http://127.0.0.1:9/v1",
                "provider": "openai-test"
            },
            "status": "active",
            "time": { "released": 0 },
            "variants": [
                { "id": "low", "settings": { "reasoningEffort": "low" } },
                { "id": "medium", "settings": { "reasoningEffort": "medium" } },
                { "id": "high", "settings": { "reasoningEffort": "high" } }
            ]
        },
        {
            "capabilities": { "input": ["text"], "output": ["text"], "tools": true },
            "cost": [{ "cache": { "read": 0, "write": 0 }, "input": 0, "output": 0 }],
            "enabled": true,
            "id": "test-model",
            "limit": { "context": 100000, "output": 10000 },
            "modelID": "test-model",
            "name": "Test Model",
            "package": "@opencode/ai/providers/anthropic",
            "providerID": "anthropic-test",
            "settings": {
                "apiKey": "parity",
                "baseURL": "http://127.0.0.1:9/v1"
            },
            "status": "active",
            "time": { "released": 0 },
            "variants": [
                {
                    "id": "low",
                    "settings": {
                        "effort": "low",
                        "thinking": { "display": "summarized", "type": "adaptive" }
                    }
                },
                {
                    "id": "medium",
                    "settings": {
                        "effort": "medium",
                        "thinking": { "display": "summarized", "type": "adaptive" }
                    }
                },
                {
                    "id": "high",
                    "settings": {
                        "effort": "high",
                        "thinking": { "display": "summarized", "type": "adaptive" }
                    }
                },
                {
                    "id": "xhigh",
                    "settings": {
                        "effort": "xhigh",
                        "thinking": { "display": "summarized", "type": "adaptive" }
                    }
                },
                {
                    "id": "max",
                    "settings": {
                        "effort": "max",
                        "thinking": { "display": "summarized", "type": "adaptive" }
                    }
                }
            ]
        }
    ])
}

fn reference(provider_id: &str, id: &str) -> ModelRef {
    ModelRef { id: id.to_string(), provider_id: ProviderID(provider_id.to_string()), variant: None }
}

#[test]
fn parity_base_config_lists_both_models() {
    let entries = [document(base_config())];
    let catalog = Catalog::from_entries(&entries);
    let models = catalog.models(None);
    let actual = serde_json::from_str::<Value>(&encode_json(&models)).expect("models encode to json");
    assert_eq!(actual, expected_models());
    let default = catalog.default_model().expect("default model");
    assert_eq!(default.provider_id.0, "openai-test");
    assert_eq!(default.id, "test-model");

    let anthropic_variants: Vec<String> = models[1].variants.iter().map(|variant| variant.id.clone()).collect();
    assert_eq!(anthropic_variants, ["low", "medium", "high", "xhigh", "max"]);

    let entries = [document(base_config()), document(r#"{"model":"anthropic-test/test-model"}"#)];
    let catalog = Catalog::from_entries(&entries);
    let default = catalog.default_model().expect("default model");
    assert_eq!(default.provider_id.0, "anthropic-test");
    assert_eq!(default.id, "test-model");
}

#[test]
fn aisdk_mapping() {
    let entries = [document(
        r#"{"providers":{"anthropic":{"package":"@ai-sdk/anthropic","models":{"m":{"name":"M"}}},"anthropic-prefixed":{"package":"aisdk:@ai-sdk/anthropic","models":{"m":{"name":"M"}}},"compatible":{"package":"@ai-sdk/openai-compatible","models":{"m":{"name":"M"}}},"unmapped":{"package":"some-npm-package","models":{"m":{"name":"M"}}}}}"#,
    )];
    let catalog = Catalog::from_entries(&entries);
    let packages: Vec<(String, Option<String>)> = catalog
        .models(Some(&["chat".to_string()]))
        .into_iter()
        .map(|model| (model.provider_id.0, model.package))
        .collect();
    assert_eq!(
        packages,
        [
            ("anthropic".to_string(), Some("@opencode/ai/providers/anthropic".to_string())),
            ("anthropic-prefixed".to_string(), Some("@opencode/ai/providers/anthropic".to_string())),
            ("compatible".to_string(), Some("@opencode/ai/providers/openai-compatible".to_string())),
            ("unmapped".to_string(), Some("some-npm-package".to_string())),
        ]
    );
}

#[test]
fn runtime_model_uses_config_base_url() {
    let keyless = [document(
        r#"{"providers":{"anthropic-test":{"name":"Anthropic Test","package":"@ai-sdk/anthropic","settings":{"baseURL":"http://127.0.0.1:9/v1"},"models":{"test-model":{"name":"Test Model","capabilities":{"tools":true,"input":["text"],"output":["text"]}}}}}}"#,
    )];
    let catalog = Catalog::from_entries(&keyless);
    let runtime =
        catalog.runtime_model(&reference("anthropic-test", "test-model"), &MapEnv::default()).expect("runtime model");
    let RuntimeModel::Language(model) = runtime else {
        panic!("expected a language model");
    };
    assert_eq!(model.route.id, "anthropic-messages");
    assert_eq!(model.route.endpoint.base_url.as_deref(), Some("http://127.0.0.1:9/v1"));
    assert!(matches!(model.route.auth, Auth::None));

    let keyed = [document(
        r#"{"providers":{"anthropic-test":{"name":"Anthropic Test","package":"@ai-sdk/anthropic","settings":{"apiKey":"parity","baseURL":"http://127.0.0.1:9/v1"},"models":{"test-model":{"name":"Test Model","capabilities":{"tools":true,"input":["text"],"output":["text"]}}}}}}"#,
    )];
    let catalog = Catalog::from_entries(&keyed);
    let runtime =
        catalog.runtime_model(&reference("anthropic-test", "test-model"), &MapEnv::default()).expect("runtime model");
    let RuntimeModel::Language(model) = runtime else {
        panic!("expected a language model");
    };
    assert_eq!(model.route.endpoint.base_url.as_deref(), Some("http://127.0.0.1:9/v1"));
    assert!(!matches!(model.route.auth, Auth::None));
}

#[test]
fn runtime_model_rejects_unmapped_package() {
    let entries = [document(
        r#"{"providers":{"unmapped":{"package":"some-npm-package","models":{"test-model":{"name":"Test Model"}}}}}"#,
    )];
    let catalog = Catalog::from_entries(&entries);
    let error = catalog
        .runtime_model(&reference("unmapped", "test-model"), &MapEnv::default())
        .err()
        .expect("unsupported package");
    assert!(matches!(error, CatalogError::UnsupportedPackage { .. }));
    assert_eq!(error.to_string(), "Unsupported package for unmapped/test-model: some-npm-package");
}

#[test]
fn later_entries_overlay_earlier_ones() {
    let entries = [
        document(
            r#"{"providers":{"overlay":{"name":"First","package":"@ai-sdk/anthropic","models":{"disabled":{"name":"Disabled"},"kept":{"name":"Kept"}}}}}"#,
        ),
        document(
            r#"{"providers":{"overlay":{"name":"Second","models":{"disabled":{"disabled":true},"kept":{"name":"Renamed"}}}}}"#,
        ),
    ];
    let catalog = Catalog::from_entries(&entries);
    let models = catalog.models(None);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "kept");
    assert_eq!(models[0].name, "Renamed");
}
