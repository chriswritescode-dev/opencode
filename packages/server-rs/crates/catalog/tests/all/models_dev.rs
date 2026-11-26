use opencode_catalog::models_dev::{Snapshot, decode_snapshot, snapshot_text};
use opencode_contract::codec::{Encode, encode_json};
use opencode_util::hash::sha256_hex;
use serde_json::{Map, Value, json};

use crate::corpus;

fn encoded<T: Encode + ?Sized>(value: &T) -> Value {
    serde_json::from_str(&encode_json(value)).expect("contract JSON")
}

fn snapshot_json(snapshot: &Snapshot) -> Value {
    let mut value = Map::new();
    value.insert("info".to_string(), encoded(&snapshot.info));
    value.insert("models".to_string(), Value::Array(snapshot.models.iter().map(encoded).collect()));
    if let Some(voices) = &snapshot.voices {
        value.insert(
            "voices".to_string(),
            Value::Array(
                voices
                    .iter()
                    .map(|voice| {
                        json!({
                            "id": voice.id,
                            "name": voice.name,
                            "api": voice.api,
                            "releaseDate": voice.release_date,
                        })
                    })
                    .collect(),
            ),
        );
    }
    value.insert(
        "environment".to_string(),
        Value::Array(snapshot.environment.iter().cloned().map(Value::String).collect()),
    );
    Value::Object(value)
}

fn canonical(value: &Value) -> String {
    match value {
        Value::Array(items) => format!("[{}]", items.iter().map(canonical).collect::<Vec<_>>().join(",")),
        Value::Object(entries) => {
            let mut keys = entries.keys().collect::<Vec<_>>();
            keys.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
            let fields = keys
                .iter()
                .map(|key| format!("{}:{}", encode_json(*key), canonical(&entries[*key])))
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{fields}}}")
        }
        _ => encode_json(value),
    }
}

#[test]
fn models_dev_corpus_matches_the_pinned_typescript() {
    let cases = corpus::area("models-dev");
    assert_eq!(cases.len(), 5, "the corpus holds every pinned models-dev case");

    for (name, case) in &cases {
        match case["call"].as_str().expect("a call name") {
            "decode" => {
                let text = serde_json::to_string(&case["input"]).expect("the catalog text");
                for (recommended, flag) in [(false, "false"), (true, "true")] {
                    let actual = decode_snapshot(&text, recommended)
                        .expect("decode the catalog")
                        .iter()
                        .map(snapshot_json)
                        .collect::<Vec<_>>();
                    assert_eq!(Value::Array(actual), case["expected"][flag], "case {name} recommended {recommended}");
                }
            }
            "text" => {
                assert_eq!(snapshot_text().len() as u64, case["bytes"].as_u64().expect("bytes"), "case {name}");
                assert_eq!(
                    sha256_hex(snapshot_text().as_bytes()),
                    case["sha256"].as_str().expect("sha256"),
                    "case {name}"
                );
            }
            "snapshot" => {
                for (recommended, flag) in [(false, "false"), (true, "true")] {
                    let snapshots = decode_snapshot(snapshot_text(), recommended).expect("decode the snapshot");
                    assert_eq!(
                        snapshots.len() as u64,
                        case["count"][flag].as_u64().expect("count"),
                        "case {name} recommended {recommended}"
                    );
                    for snapshot in &snapshots {
                        let id = snapshot.info.id.0.as_str();
                        let digest = sha256_hex(canonical(&snapshot_json(snapshot)).as_bytes());
                        assert_eq!(
                            digest,
                            case["providers"][flag][id].as_str().expect("a provider digest"),
                            "provider {id} recommended {recommended}"
                        );
                    }
                }
            }
            call => panic!("case {name} has an unknown call {call}"),
        }
    }
}

#[test]
fn skips_unknown_opencode_entrypoints_in_favour_of_the_aisdk_fallback() {
    let catalog = json!({
        "acme": {
            "id": "acme",
            "name": "Acme",
            "env": ["ACME_API_KEY"],
            "models": {
                "known-fallback": {
                    "id": "known-fallback",
                    "type": "chat",
                    "name": "Known Fallback",
                    "release_date": "2026-01-01",
                    "modalities": { "input": ["text"], "output": ["text"] },
                    "capabilities": { "tools": { "supported": true }, "reasoning": { "supported": false } },
                    "api": {
                        "chat-completions": {
                            "sdk": {
                                "opencodeai": "@opencode/ai/providers/acme/unknown",
                                "aisdk": "@ai-sdk/openai-compatible",
                            }
                        }
                    }
                },
                "unknown-only": {
                    "id": "unknown-only",
                    "type": "chat",
                    "name": "Unknown Only",
                    "release_date": "2026-01-01",
                    "modalities": { "input": ["text"], "output": ["text"] },
                    "capabilities": { "tools": { "supported": true }, "reasoning": { "supported": false } },
                    "api": {
                        "chat-completions": {
                            "sdk": { "opencodeai": "@opencode/ai/providers/acme/unknown" }
                        }
                    }
                }
            }
        }
    });
    let text = serde_json::to_string(&catalog).expect("the catalog text");
    let snapshots = decode_snapshot(&text, false).expect("decode the catalog");

    assert_eq!(snapshots.len(), 1, "the provider keeps the route with a package");
    assert_eq!(snapshots[0].info.package, "@opencode/ai/providers/openai-compatible");
    let ids = snapshots[0].models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids, ["known-fallback"], "only the model with a loadable entrypoint survives");
}
