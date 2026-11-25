use opencode_contract::codec::{JsonMap, encode_json};
use opencode_contract::generated::{IntegrationMethod, ProviderID};
use serde_json::{Value, json};

use crate::builtin::{Input, enable, json_value, start};

fn base_url(rest: &JsonMap<Value>) -> Option<&str> {
    rest.0.iter().find(|(key, _)| key == "baseURL").and_then(|(_, value)| value.as_str())
}

fn chat_model(id: &str, sdk: &str) -> Value {
    json!({
        "id": id,
        "type": "chat",
        "name": id,
        "release_date": "2026-01-01",
        "modalities": { "input": ["text"], "output": ["text"] },
        "capabilities": { "tools": { "supported": true }, "reasoning": { "supported": false } },
        "api": { "generate-content": { "sdk": { "opencodeai": sdk } } }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn models_dev_preserves_provider_and_model_url_templates_in_the_catalog() {
    let catalog = json!({
        "acme": {
            "id": "acme",
            "name": "Acme",
            "env": ["ACME_HOST", "ACME_MODEL_PATH", "ACME_API_KEY"],
            "models": {
                "gpt-5.4": {
                    "id": "gpt-5.4",
                    "type": "chat",
                    "name": "GPT-5.4",
                    "release_date": "2026-01-01",
                    "modalities": { "input": ["text"], "output": ["text"] },
                    "capabilities": { "tools": { "supported": true }, "reasoning": { "supported": false } },
                    "limit": { "context": 1050000, "output": 128000 },
                    "api": {
                        "generate-content": {
                            "base_url": "https://${ACME_HOST}/${ACME_MODEL_PATH}/v1",
                            "sdk": { "opencodeai": "@opencode/ai/providers/openai-compatible" }
                        }
                    }
                }
            }
        },
        "proxy": {
            "id": "proxy",
            "name": "Proxy",
            "env": [],
            "models": {
                "one": {
                    "id": "one",
                    "type": "chat",
                    "name": "One",
                    "release_date": "2026-01-01",
                    "modalities": { "input": ["text"], "output": ["text"] },
                    "capabilities": { "tools": { "supported": true }, "reasoning": { "supported": false } },
                    "api": {
                        "generate-content": {
                            "base_url": "https://${ACME_HOST}/one/v1",
                            "sdk": { "opencodeai": "@opencode/ai/providers/openai-compatible" }
                        }
                    }
                },
                "two": {
                    "id": "two",
                    "type": "chat",
                    "name": "Two",
                    "release_date": "2026-01-01",
                    "modalities": { "input": ["text"], "output": ["text"] },
                    "capabilities": { "tools": { "supported": true }, "reasoning": { "supported": false } },
                    "api": {
                        "generate-content": {
                            "base_url": "https://${UNDECLARED_HOST}/two/v1",
                            "sdk": { "opencodeai": "@opencode/ai/providers/openai-compatible" }
                        }
                    }
                }
            }
        }
    });
    let mut harness = start(
        Input::new(Vec::new())
            .models(catalog)
            .environment("ACME_HOST", "api.acme.test")
            .environment("UNDECLARED_HOST", "private.example"),
    )
    .await;
    harness.register(&["models_dev"]).await;
    let _enabled = enable(&harness).await;

    let acme = harness.services.provider.get(&ProviderID("acme".to_owned())).await.unwrap().unwrap();
    let acme_settings = acme.settings.expect("the acme provider settings");
    assert_eq!(
        base_url(&acme_settings.rest),
        Some("https://${ACME_HOST}/${ACME_MODEL_PATH}/v1"),
        "the acme provider base URL stays a literal template"
    );

    let proxy = harness.services.provider.get(&ProviderID("proxy".to_owned())).await.unwrap().unwrap();
    let proxy_settings = proxy.settings.expect("the proxy provider settings");
    assert_eq!(
        base_url(&proxy_settings.rest),
        Some("https://${ACME_HOST}/one/v1"),
        "the proxy provider base URL stays a literal template"
    );

    let two = harness.services.model.get(&ProviderID("proxy".to_owned()), "two").await.unwrap().unwrap();
    let two_settings = two.settings.expect("the two model settings");
    assert_eq!(
        base_url(&two_settings.rest),
        Some("https://${UNDECLARED_HOST}/two/v1"),
        "the model base URL keeps the undeclared host template"
    );

    let integrations = harness.services.integration.list().await.unwrap();
    let integration = integrations.iter().find(|integration| integration.id.0 == "acme").expect("the acme integration");
    let env = integration
        .methods
        .iter()
        .find_map(|method| match method {
            IntegrationMethod::Env(env) => Some(env),
            _ => None,
        })
        .expect("the acme env method");
    assert_eq!(env.names, ["ACME_HOST", "ACME_MODEL_PATH", "ACME_API_KEY"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn models_dev_copies_model_request_bodies_without_reinterpreting_literal_proto_keys() {
    let catalog = json!({
        "acme": {
            "id": "acme",
            "name": "Acme",
            "env": [],
            "models": {
                "gpt-5.4": {
                    "id": "gpt-5.4",
                    "type": "chat",
                    "name": "GPT-5.4",
                    "release_date": "2026-01-01",
                    "modalities": { "input": ["text"], "output": ["text"] },
                    "capabilities": { "tools": { "supported": true }, "reasoning": { "supported": false } },
                    "limit": { "context": 1050000, "output": 128000 },
                    "api": {
                        "generate-content": {
                            "sdk": { "opencodeai": "@opencode/ai/providers/openai-compatible" }
                        }
                    },
                    "experimental": {
                        "modes": {
                            "fast": {
                                "body": { "__proto__": { "service_tier": "priority" }, "keep": true },
                                "headers": { "x-mode": "fast" }
                            }
                        }
                    }
                }
            }
        }
    });
    let mut harness = start(Input::new(Vec::new()).models(catalog)).await;
    harness.register(&["models_dev"]).await;
    let _enabled = enable(&harness).await;

    let base = harness.services.model.get(&ProviderID("acme".to_owned()), "gpt-5.4").await.unwrap().unwrap();
    assert!(base.body.is_none(), "the base model has no request body");

    let fast = harness.services.model.get(&ProviderID("acme".to_owned()), "gpt-5.4-fast").await.unwrap().unwrap();
    let body = fast.body.expect("the fast mode body");
    let keys = body.0.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>();
    assert_eq!(keys, ["__proto__", "keep"], "the body keeps exactly its own keys in order");
    assert!(!keys.contains(&"service_tier"), "the body has no top-level service_tier key");
    assert_eq!(
        json_value(&encode_json(&body)),
        json!({ "__proto__": { "service_tier": "priority" }, "keep": true }),
        "the body round-trips unchanged"
    );

    let headers = fast.headers.expect("the fast mode headers");
    assert_eq!(json_value(&encode_json(&headers)), json!({ "x-mode": "fast" }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn models_dev_registers_every_listed_environment_variable_for_provider_plugins_to_narrow() {
    let catalog = json!({
        "cloudflare-workers-ai": {
            "id": "cloudflare-workers-ai",
            "name": "Cloudflare Workers AI",
            "env": ["CLOUDFLARE_ACCOUNT_ID", "CLOUDFLARE_API_KEY"],
            "models": {
                "workers": chat_model("workers", "@opencode/ai/providers/cloudflare-workers-ai")
            }
        },
        "google-vertex": {
            "id": "google-vertex",
            "name": "Google Vertex",
            "env": ["GOOGLE_VERTEX_PROJECT", "GOOGLE_VERTEX_LOCATION", "GOOGLE_APPLICATION_CREDENTIALS"],
            "models": {
                "vertex": chat_model("vertex", "@opencode/ai/providers/google-vertex")
            }
        }
    });
    let mut harness = start(Input::new(Vec::new()).models(catalog)).await;
    harness.register(&["models_dev"]).await;

    let integrations = harness.services.integration.list().await.unwrap();
    let cloudflare = integrations
        .iter()
        .find(|integration| integration.id.0 == "cloudflare-workers-ai")
        .expect("the cloudflare integration");
    assert_eq!(cloudflare.name, "Cloudflare Workers AI");
    let mut methods = cloudflare.methods.iter();
    assert!(matches!(methods.next(), Some(IntegrationMethod::Key(_))), "the cloudflare key method");
    match methods.next() {
        Some(IntegrationMethod::Env(env)) => {
            assert_eq!(env.names, ["CLOUDFLARE_ACCOUNT_ID", "CLOUDFLARE_API_KEY"]);
        }
        other => panic!("unexpected cloudflare method {other:?}"),
    }
    assert!(methods.next().is_none(), "the cloudflare integration has only key and env methods");

    let vertex = integrations
        .iter()
        .find(|integration| integration.id.0 == "google-vertex")
        .expect("the google vertex integration");
    assert_eq!(vertex.name, "Google Vertex");
    let mut methods = vertex.methods.iter();
    assert!(matches!(methods.next(), Some(IntegrationMethod::Key(_))), "the vertex key method");
    match methods.next() {
        Some(IntegrationMethod::Env(env)) => {
            assert_eq!(
                env.names,
                ["GOOGLE_VERTEX_PROJECT", "GOOGLE_VERTEX_LOCATION", "GOOGLE_APPLICATION_CREDENTIALS"]
            );
        }
        other => panic!("unexpected google vertex method {other:?}"),
    }
    assert!(methods.next().is_none(), "the google vertex integration has only key and env methods");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn models_dev_projects_normalized_definitions_for_available_providers() {
    let catalog = json!({
        "acme": {
            "id": "acme",
            "name": "Acme",
            "env": [],
            "models": {
                "gpt-5.4": {
                    "id": "gpt-5.4",
                    "type": "chat",
                    "name": "GPT-5.4",
                    "family": "gpt",
                    "release_date": "2026-01-01",
                    "recommended_api": "generate-content",
                    "modalities": { "input": ["text"], "output": ["text"] },
                    "capabilities": { "tools": { "supported": true }, "reasoning": { "supported": false } },
                    "limit": { "context": 1050000, "input": 922000, "output": 128000 },
                    "cost": {
                        "text": {
                            "input": {
                                "price": 2.5,
                                "cache": { "read": 0 },
                                "overrides": [
                                    { "when": { "context": { "gt": 272000 } }, "price": 3, "cache": { "read": 0.25 } },
                                    { "when": { "context": { "gt": 200000 } }, "price": 5, "cache": { "read": 0.5 } }
                                ]
                            },
                            "output": {
                                "price": 15,
                                "overrides": [
                                    { "when": { "context": { "gt": 272000 } }, "price": 18 },
                                    { "when": { "context": { "gt": 200000 } }, "price": 22.5 }
                                ]
                            }
                        }
                    },
                    "api": {
                        "generate-content": {
                            "sdk": { "opencodeai": "@opencode/ai/providers/openai-compatible" }
                        }
                    },
                    "experimental": {
                        "modes": {
                            "fast": {
                                "body": { "service_tier": "priority" },
                                "headers": { "x-mode": "fast" },
                                "cost": {
                                    "text": {
                                        "input": { "price": 5, "cache": { "read": 0.5 } },
                                        "output": { "price": 30 }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    });
    let mut harness = start(Input::new(Vec::new()).models(catalog)).await;
    harness.register(&["models_dev"]).await;
    let _enabled = enable(&harness).await;

    let base = harness.services.model.get(&ProviderID("acme".to_owned()), "gpt-5.4").await.unwrap().unwrap();
    assert!(base.variants.is_empty(), "the base model has no variants");
    assert!(base.body.is_none(), "the base model has no request body");
    assert_eq!(json_value(&encode_json(&base.limit)), json!({ "context": 1050000, "input": 922000, "output": 128000 }));
    assert_eq!(
        json_value(&encode_json(&base.cost)),
        json!([
            { "input": 2.5, "output": 15, "cache": { "read": 0, "write": 0 } },
            {
                "tier": { "type": "context", "size": 272000 },
                "input": 3,
                "output": 18,
                "cache": { "read": 0.25, "write": 0 }
            },
            {
                "tier": { "type": "context", "size": 200000 },
                "input": 5,
                "output": 22.5,
                "cache": { "read": 0.5, "write": 0 }
            }
        ])
    );

    let fast = harness.services.model.get(&ProviderID("acme".to_owned()), "gpt-5.4-fast").await.unwrap().unwrap();
    assert_eq!(fast.id, "gpt-5.4-fast");
    assert_eq!(fast.model_id, "gpt-5.4");
    assert_eq!(fast.provider_id.0, "acme");
    assert_eq!(fast.name, "GPT-5.4 Fast");
    assert_eq!(fast.package.as_deref(), Some("@opencode/ai/providers/openai-compatible"));
    assert_eq!(
        json_value(&encode_json(fast.body.as_ref().expect("the fast model body"))),
        json!({ "service_tier": "priority" })
    );
    assert_eq!(
        json_value(&encode_json(fast.headers.as_ref().expect("the fast model headers"))),
        json!({ "x-mode": "fast" })
    );
    assert!(fast.variants.is_empty(), "the mode model has no variants");
    assert_eq!(
        json_value(&encode_json(&fast.cost)),
        json!([
            { "input": 5, "output": 30, "cache": { "read": 0.5, "write": 0 } },
            {
                "tier": { "type": "context", "size": 272000 },
                "input": 3,
                "output": 18,
                "cache": { "read": 0.25, "write": 0 }
            },
            {
                "tier": { "type": "context", "size": 200000 },
                "input": 5,
                "output": 22.5,
                "cache": { "read": 0.5, "write": 0 }
            }
        ])
    );
}
