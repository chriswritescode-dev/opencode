pub mod normalize;
pub mod service;
pub mod source;
pub mod v1;

pub use service::{CATALOG_PATH, DEFAULT_SOURCE, ModelsDevError, ModelsDevService, Options, body_digest, cache_key};

use std::io::Read;
use std::sync::LazyLock;

use opencode_contract::generated::{ModelInfo, ProviderInfo};
use serde_json::{Map, Value};

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub info: ProviderInfo,
    pub models: Vec<ModelInfo>,
    pub voices: Option<Vec<VoiceModel>>,
    pub environment: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct VoiceModel {
    pub id: String,
    pub name: String,
    pub api: String,
    pub release_date: String,
}

pub fn decode_snapshot(text: &str, recommended: bool) -> Result<Vec<Snapshot>, serde_json::Error> {
    let catalog: Map<String, Value> = serde_json::from_str(text)?;
    Ok(normalize::normalize(&catalog, recommended))
}

static TEXT: LazyLock<String> = LazyLock::new(|| {
    let mut decoder = flate2::read::GzDecoder::new(include_bytes!("generated/models-dev-snapshot.json.gz").as_slice());
    let mut text = String::new();
    decoder.read_to_string(&mut text).expect("the embedded models.dev snapshot decompresses");
    text
});

pub fn snapshot_text() -> &'static str {
    &TEXT
}
