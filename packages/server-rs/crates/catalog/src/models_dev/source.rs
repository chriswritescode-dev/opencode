use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Clone, Debug, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub env: Vec<String>,
    pub models: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Model {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub family: Option<String>,
    pub release_date: String,
    pub status: Option<String>,
    pub modalities: Modalities,
    pub capabilities: Capabilities,
    pub limit: Option<Limit>,
    pub cost: Option<Cost>,
    pub api: Map<String, Value>,
    pub recommended_api: Option<Value>,
    pub experimental: Option<Experimental>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Modalities {
    pub input: Vec<String>,
    pub output: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Capabilities {
    pub tools: Tools,
    pub reasoning: Reasoning,
    pub temperature: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Tools {
    pub supported: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Reasoning {
    pub supported: bool,
    pub toggle: Option<bool>,
    pub effort: Option<Vec<String>>,
    pub budget: Option<Budget>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Budget {
    pub min: Option<i64>,
    pub max: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Limit {
    pub context: Option<f64>,
    pub input: Option<f64>,
    pub output: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Cost {
    pub text: Option<Text>,
    pub multipliers: Option<Vec<Multiplier>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Text {
    pub input: Option<TextInput>,
    pub output: Option<TextOutput>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TextInput {
    pub unit: Option<String>,
    pub price: f64,
    pub cache: Option<CachePrices>,
    pub overrides: Option<Vec<TextInputOverride>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TextOutput {
    pub unit: Option<String>,
    pub price: f64,
    pub overrides: Option<Vec<TextOutputOverride>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TextInputOverride {
    pub when: Map<String, Value>,
    pub price: Option<f64>,
    pub cache: Option<CachePrices>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TextOutputOverride {
    pub when: Map<String, Value>,
    pub price: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CachePrices {
    pub read: Option<f64>,
    pub write: Option<CacheWrite>,
    pub explicit: Option<ExplicitCache>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExplicitCache {
    pub write: Option<CacheWrite>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum CacheWrite {
    Price(f64),
    Record(IndexMap<String, f64>),
}

#[derive(Clone, Debug, Deserialize)]
pub struct Multiplier {
    pub when: Map<String, Value>,
    pub factor: Factor,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Factor {
    Price(f64),
    Composite(IndexMap<String, Factor>),
}

#[derive(Clone, Debug, Deserialize)]
pub struct Api {
    pub base_url: Option<String>,
    pub capabilities: Option<ApiCapabilities>,
    pub sdk: Option<Sdk>,
    pub compatibility: Option<Compatibility>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ApiCapabilities {
    pub tools: Option<ApiTools>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ApiTools {
    pub freeform: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Sdk {
    pub aisdk: Option<String>,
    pub opencodeai: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Compatibility {
    pub reasoning_field: Option<String>,
    pub max_tokens_field: Option<String>,
    pub media_input: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Mode {
    pub service_tier: Option<String>,
    pub cost: Option<Cost>,
    pub body: Option<Map<String, Value>>,
    pub headers: Option<IndexMap<String, String>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Experimental {
    pub modes: Option<Map<String, Value>>,
}

pub fn decode_provider(value: &Value) -> Option<Provider> {
    Provider::deserialize(value).ok()
}

pub fn decode_model(value: &Value) -> Result<Model, serde_json::Error> {
    Model::deserialize(value)
}

pub fn decode_api(value: &Value) -> Option<Api> {
    Api::deserialize(value).ok()
}

pub fn decode_mode(value: &Value) -> Option<Mode> {
    Mode::deserialize(value).ok()
}
