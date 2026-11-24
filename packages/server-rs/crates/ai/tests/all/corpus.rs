#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use opencode_ai::schema::{AiError, LlmEvent, LlmRequest};
use serde::de::DeserializeOwned;

pub fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

#[derive(Debug, Clone)]
pub struct Corpus {
    pub cassettes: Vec<Cassette>,
    pub provider_errors: Vec<ProviderErrorRow>,
}

impl Corpus {
    pub fn load() -> Corpus {
        let index: Index = read_json(&corpus_dir().join("llm/index.json"));
        let cassettes = index
            .cassettes
            .into_iter()
            .map(|entry| {
                let file: CassetteFile = read_json(&corpus_dir().join("llm").join(format!("{}.json", entry.cassette)));
                Cassette {
                    name: file.cassette,
                    protocol: file.protocol,
                    group: entry.group,
                    route_id: file.route_id,
                    clock_epoch_ms: file.clock_epoch_ms,
                    steps: file.steps,
                }
            })
            .collect();
        let provider_errors: ProviderErrors = read_json(&corpus_dir().join("provider-errors.json"));
        Corpus { cassettes, provider_errors: provider_errors.errors }
    }
}

#[derive(Debug, Clone)]
pub struct Cassette {
    pub name: String,
    pub protocol: String,
    pub group: String,
    pub route_id: String,
    pub clock_epoch_ms: u64,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Step {
    pub model: serde_json::Value,
    #[serde(default)]
    pub provider: Option<ProviderStep>,
    pub request: serde_json::Value,
    pub transport: String,
    #[serde(default)]
    pub operation: Option<String>,
    pub exchanges: Option<Vec<Exchange>>,
    pub websocket: Option<serde_json::Value>,
    pub events: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    pub error: Option<StepError>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStep {
    pub package: String,
    #[serde(rename = "modelID")]
    pub model_id: String,
    #[serde(default)]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

impl Step {
    pub fn request(&self) -> LlmRequest {
        match serde_json::from_value(self.request.clone()) {
            Ok(request) => request,
            Err(error) => panic!("failed to decode request: {error}"),
        }
    }

    pub fn events(&self) -> Vec<LlmEvent> {
        let Some(events) = &self.events else {
            panic!("step has no events");
        };
        events
            .iter()
            .map(|value| match serde_json::from_value(value.clone()) {
                Ok(event) => event,
                Err(error) => panic!("failed to decode event: {error}"),
            })
            .collect()
    }

    pub fn error(&self) -> Option<AiError> {
        let error = self.error.as_ref()?;
        let value = serde_json::json!({ "reason": error.reason.clone() });
        match serde_json::from_value(value) {
            Ok(error) => Some(error),
            Err(failure) => panic!("failed to decode error: {failure}"),
        }
    }

    pub fn websocket_record(&self) -> WebSocketRecord {
        let request_id = self.request.get("id").and_then(serde_json::Value::as_str).unwrap_or_default();
        let Some(websocket) = &self.websocket else {
            panic!("step {request_id} has no websocket record");
        };
        match serde_json::from_value(websocket.clone()) {
            Ok(record) => record,
            Err(error) => panic!("step {request_id} websocket record: {error}"),
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSocketRecord {
    #[serde(default)]
    pub ops: Vec<ChannelOp>,
    pub sent: Vec<String>,
    pub received: Vec<serde_json::Value>,
    #[serde(default)]
    pub connect: Option<ConnectRecord>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ChannelOp {
    #[serde(rename_all = "camelCase")]
    Reconnect {
        #[serde(default)]
        preserve_checkpoint: bool,
    },
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ConnectRecord {
    pub url: String,
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Exchange {
    pub prepared: Prepared,
    pub response: RecordedResponse,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prepared {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: String,
    pub body_encoding: Option<String>,
    #[serde(default)]
    pub boundary: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: String,
    pub body_encoding: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct StepError {
    pub reason: serde_json::Value,
    pub retryable: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProviderErrorRow {
    pub input: serde_json::Value,
    pub reason: serde_json::Value,
    pub retryable: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Index {
    cassettes: Vec<IndexEntry>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexEntry {
    cassette: String,
    group: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CassetteFile {
    cassette: String,
    protocol: String,
    route_id: String,
    clock_epoch_ms: u64,
    steps: Vec<Step>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderErrors {
    errors: Vec<ProviderErrorRow>,
}

fn read_json<T: DeserializeOwned>(path: &Path) -> T {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => panic!("failed to read {}: {error}", path.display()),
    };
    match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => panic!("failed to parse {}: {error}", path.display()),
    }
}
