use std::time::Duration;

use indexmap::IndexMap;
use opencode_ai::HttpSender;
use opencode_contract::codec::JsonMap;
use opencode_contract::generated::{
    ModelCapabilities, ModelCost, ModelCostCache, ModelInfo, ModelInfoLimit, ModelInfoStatus, ModelInfoTime,
    ModelSettings, ModelVariant, ModelVariantSettings, MoneyUSDPerMillionTokens, ProviderID,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::provider;
use crate::provider_plugin::http::{self, HttpError};

const PROVIDER_ID: &str = "github-copilot";

const MESSAGES_PACKAGE: &str = "@opencode/ai/providers/anthropic";

const RESPONSES_PACKAGE: &str = "@opencode/ai/providers/openai-compatible-responses";

const CHAT_PACKAGE: &str = "@opencode/ai/providers/openai-compatible";

const TIMEOUT: Duration = Duration::from_secs(5);

pub const AUTO: &str = "auto";

pub type Snapshot = IndexMap<String, UsableModel>;

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RemoteModel {
    pub model_picker_enabled: bool,
    #[serde(default)]
    pub is_chat_fallback: Option<bool>,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub supported_endpoints: Option<Vec<String>>,
    #[serde(default)]
    pub policy: Option<Policy>,
    #[serde(default)]
    pub billing: Option<Billing>,
    pub capabilities: Capabilities,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Policy {
    #[serde(default)]
    pub state: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Billing {
    #[serde(default)]
    pub auto_discount: Option<f64>,
    #[serde(default)]
    pub token_prices: Option<TokenPrices>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct TokenPrices {
    pub batch_size: f64,
    pub default: Prices,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Prices {
    #[serde(default)]
    pub cache_price: Option<f64>,
    #[serde(default)]
    pub cache_read_price: Option<f64>,
    pub input_price: f64,
    pub output_price: f64,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Capabilities {
    pub family: String,
    #[serde(default)]
    pub limits: Option<Limits>,
    pub supports: Supports,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Limits {
    #[serde(default)]
    pub max_context_window_tokens: Option<f64>,
    #[serde(default)]
    pub max_output_tokens: Option<f64>,
    #[serde(default)]
    pub max_prompt_tokens: Option<f64>,
    #[serde(default)]
    pub vision: Option<Vision>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Vision {
    pub max_prompt_image_size: f64,
    pub max_prompt_images: f64,
    pub supported_media_types: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Supports {
    #[serde(default)]
    pub adaptive_thinking: Option<bool>,
    #[serde(default)]
    pub max_thinking_budget: Option<f64>,
    #[serde(default)]
    pub min_thinking_budget: Option<f64>,
    #[serde(default)]
    pub reasoning_effort: Option<Vec<String>>,
    #[serde(default)]
    pub streaming: Option<bool>,
    #[serde(default)]
    pub structured_outputs: Option<bool>,
    #[serde(default)]
    pub tool_calls: Option<bool>,
    #[serde(default)]
    pub vision: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsableModel(pub RemoteModel);

impl UsableModel {
    fn usable(remote: RemoteModel) -> Option<Self> {
        let limits = remote.capabilities.limits.as_ref()?;
        limits.max_output_tokens?;
        limits.max_prompt_tokens?;
        remote.capabilities.supports.tool_calls?;
        if remote.policy.as_ref().and_then(|policy| policy.state.as_deref()) == Some("disabled") {
            return None;
        }
        Some(Self(remote))
    }

    fn limits(&self) -> Option<&Limits> {
        self.0.capabilities.limits.as_ref()
    }

    fn supports(&self) -> &Supports {
        &self.0.capabilities.supports
    }

    fn max_output_tokens(&self) -> f64 {
        self.limits().and_then(|limits| limits.max_output_tokens).unwrap_or(0.0)
    }

    fn max_prompt_tokens(&self) -> f64 {
        self.limits().and_then(|limits| limits.max_prompt_tokens).unwrap_or(0.0)
    }

    fn context_tokens(&self) -> f64 {
        self.limits().and_then(|limits| limits.max_context_window_tokens.or(limits.max_prompt_tokens)).unwrap_or(0.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Messages,
    Responses,
    Chat,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    pub endpoint: Endpoint,
    pub package: &'static str,
    pub base_url: String,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error("invalid GitHub Copilot models response: {0}")]
    Json(String),
    #[error("GitHub Copilot connection has no OAuth credential")]
    MissingCredential,
}

impl crate::shared_cache::Transient for LoadError {
    fn is_transient(&self) -> bool {
        matches!(self, Self::Http(error) if error.transient())
    }
}

#[derive(Deserialize)]
struct Envelope {
    data: Vec<Value>,
}

pub async fn load(
    http: &dyn HttpSender,
    base_url: &str,
    headers: Vec<(String, String)>,
) -> Result<Snapshot, LoadError> {
    let response = http::send(http, http::get(format!("{base_url}/models"), headers), Some(TIMEOUT)).await?;
    let envelope: Envelope =
        serde_json::from_slice(&response.body).map_err(|error| LoadError::Json(error.to_string()))?;
    Ok(envelope
        .data
        .iter()
        .filter_map(|raw| serde_json::from_value::<RemoteModel>(raw.clone()).ok())
        .filter_map(UsableModel::usable)
        .map(|model| (model.0.id.clone(), model))
        .collect())
}

pub async fn get(
    http: &dyn HttpSender,
    base_url: &str,
    headers: Vec<(String, String)>,
    existing: &[ModelInfo],
) -> Result<IndexMap<String, ModelInfo>, LoadError> {
    let remote = load(http, base_url, headers).await?;
    Ok(derive(base_url, &remote, existing))
}

pub fn derive(base_url: &str, remote: &Snapshot, existing: &[ModelInfo]) -> IndexMap<String, ModelInfo> {
    let mut result: IndexMap<String, ModelInfo> =
        existing.iter().map(|model| (model.id.clone(), model.clone())).collect();
    for id in result.keys().cloned().collect::<Vec<_>>() {
        let previous = result.get(&id).cloned().expect("an existing model stays in the result");
        let Some(current) = remote.get(&previous.model_id) else {
            result.shift_remove(&id);
            continue;
        };
        result.insert(id, build(&previous.id, current, base_url, Some(&previous)));
    }
    for (id, model) in remote {
        if result.contains_key(id) {
            continue;
        }
        result.insert(id.clone(), build(id, model, base_url, None));
    }
    result
}

pub fn auto_model(base_url: &str, remote: &Snapshot) -> Option<ModelInfo> {
    let pool: Vec<&UsableModel> = remote
        .values()
        .filter(|model| model.0.billing.as_ref().is_some_and(|billing| billing.auto_discount.is_some()))
        .collect();
    if pool.is_empty() {
        return None;
    }
    let route = route_for(AUTO, Some(Endpoint::Chat), base_url);
    let mut model = provider::default_model(&ProviderID(PROVIDER_ID.to_owned()), AUTO);
    model.name = "Auto".to_owned();
    model.package = Some(route.package.to_owned());
    model.settings = Some(ModelSettings {
        compaction: None,
        cache: None,
        encoding: None,
        rest: JsonMap(vec![("baseURL".to_owned(), Value::String(route.base_url))]),
    });
    model.capabilities = ModelCapabilities {
        tools: true,
        freeform: None,
        input: vec!["text".to_owned(), "image".to_owned()],
        output: vec!["text".to_owned()],
        temperature: None,
    };
    model.limit = ModelInfoLimit {
        context: pool.iter().map(|model| model.context_tokens()).fold(f64::INFINITY, f64::min),
        input: Some(pool.iter().map(|model| model.max_prompt_tokens()).fold(f64::INFINITY, f64::min)),
        output: pool.iter().map(|model| model.max_output_tokens()).fold(f64::INFINITY, f64::min),
    };
    Some(model)
}

pub fn route_for(model_id: &str, advertised: Option<Endpoint>, root: &str) -> Route {
    let endpoint = advertised.unwrap_or_else(|| fallback_endpoint(model_id));
    Route {
        endpoint,
        package: match endpoint {
            Endpoint::Messages => MESSAGES_PACKAGE,
            Endpoint::Responses => RESPONSES_PACKAGE,
            Endpoint::Chat => CHAT_PACKAGE,
        },
        base_url: match endpoint {
            Endpoint::Messages => format!("{root}/v1"),
            Endpoint::Responses | Endpoint::Chat => root.to_owned(),
        },
    }
}

pub(crate) fn merge_settings(settings: Option<&ModelSettings>, base_url: &str) -> ModelSettings {
    let base = settings.map(provider::json_object);
    let mut overlay = crate::aisdk_native::JsonMap::new();
    overlay.insert("baseURL".to_owned(), Value::String(base_url.to_owned()));
    provider::decode_object(crate::aisdk_native::merge_overlay(base.as_ref(), Some(&overlay)).unwrap_or_default())
}

fn build(id: &str, remote: &UsableModel, base_url: &str, previous: Option<&ModelInfo>) -> ModelInfo {
    let route = route_for(&remote.0.id, endpoint(remote.0.supported_endpoints.as_deref()), base_url);
    let supports = remote.supports();
    let media_types =
        remote.limits().and_then(|limits| limits.vision.as_ref()).map(|vision| &vision.supported_media_types);
    let vision = supports.vision == Some(true);
    let image = vision || media_types.is_some_and(|types| types.iter().any(|item| item.starts_with("image/")));
    let pdf = vision && media_types.is_some_and(|types| types.iter().any(|item| item == "application/pdf"));
    let mut input = vec!["text".to_owned()];
    if image {
        input.push("image".to_owned());
    }
    if pdf {
        input.push("pdf".to_owned());
    }
    let prices = remote.0.billing.as_ref().and_then(|billing| billing.token_prices.as_ref());
    let usd = prices.filter(|prices| prices.batch_size > 0.0).map(|prices| 10_000.0 / prices.batch_size).unwrap_or(0.0);
    let default = prices.map(|prices| &prices.default);
    let cache_read = default.and_then(|prices| prices.cache_read_price.or(prices.cache_price)).unwrap_or(0.0);

    let mut model = provider::default_model(&ProviderID(PROVIDER_ID.to_owned()), id);
    model.model_id = remote.0.id.clone();
    model.family =
        previous.and_then(|model| model.family.clone()).or_else(|| Some(remote.0.capabilities.family.clone()));
    model.name = previous.map(|model| model.name.clone()).unwrap_or_else(|| remote.0.name.clone());
    model.package = Some(route.package.to_owned());
    model.settings = Some(merge_settings(previous.and_then(|model| model.settings.as_ref()), &route.base_url));
    model.headers = previous.and_then(|model| model.headers.clone());
    model.body = previous.and_then(|model| model.body.clone());
    model.capabilities = ModelCapabilities {
        tools: supports.tool_calls.unwrap_or(true),
        freeform: None,
        input,
        output: vec!["text".to_owned()],
        temperature: None,
    };
    model.variants = variants(remote, route.endpoint == Endpoint::Messages);
    model.time = ModelInfoTime {
        released: previous
            .map(|model| model.time.released)
            .filter(|released| *released != 0.0)
            .unwrap_or_else(|| released(version(remote))),
    };
    model.cost = vec![ModelCost {
        tier: None,
        input: MoneyUSDPerMillionTokens(default.map(|prices| prices.input_price).unwrap_or(0.0) * usd),
        output: MoneyUSDPerMillionTokens(default.map(|prices| prices.output_price).unwrap_or(0.0) * usd),
        cache: ModelCostCache {
            read: MoneyUSDPerMillionTokens(cache_read * usd),
            write: MoneyUSDPerMillionTokens(0.0),
        },
    }];
    model.status = ModelInfoStatus::Active;
    model.enabled = remote.0.model_picker_enabled;
    model.limit = ModelInfoLimit {
        context: remote.context_tokens(),
        input: Some(remote.max_prompt_tokens()),
        output: remote.max_output_tokens(),
    };
    model
}

fn endpoint(supported: Option<&[String]>) -> Option<Endpoint> {
    let supported = supported?;
    if supported.iter().any(|item| item == "/v1/messages") {
        return Some(Endpoint::Messages);
    }
    if supported.iter().any(|item| item == "/responses") {
        return Some(Endpoint::Responses);
    }
    if supported.iter().any(|item| item == "/chat/completions") {
        return Some(Endpoint::Chat);
    }
    None
}

fn fallback_endpoint(model_id: &str) -> Endpoint {
    let major = model_id
        .strip_prefix("gpt-")
        .map(|rest| rest.chars().take_while(char::is_ascii_digit).collect::<String>())
        .filter(|digits| !digits.is_empty())
        .and_then(|digits| digits.parse::<u64>().ok());
    if major.is_some_and(|major| major >= 5) && !model_id.starts_with("gpt-5-mini") {
        return Endpoint::Responses;
    }
    if model_id.starts_with("grok-") || model_id.starts_with("mai-code-") {
        return Endpoint::Responses;
    }
    Endpoint::Chat
}

fn variants(remote: &UsableModel, messages: bool) -> Vec<ModelVariant> {
    let supports = remote.supports();
    let efforts = supports.reasoning_effort.as_deref().unwrap_or_default();
    if !messages && !efforts.is_empty() {
        return efforts
            .iter()
            .map(|effort| {
                variant(
                    effort,
                    JsonMap(vec![
                        ("reasoningEffort".to_owned(), Value::String(effort.clone())),
                        ("reasoningSummary".to_owned(), Value::String("auto".to_owned())),
                        ("include".to_owned(), json!(["reasoning.encrypted_content"])),
                    ]),
                )
            })
            .collect();
    }
    if !efforts.is_empty() && supports.adaptive_thinking == Some(true) {
        return efforts
            .iter()
            .map(|effort| {
                variant(
                    effort,
                    JsonMap(vec![
                        ("thinking".to_owned(), json!({ "type": "adaptive", "display": "summarized" })),
                        ("effort".to_owned(), Value::String(effort.clone())),
                    ]),
                )
            })
            .collect();
    }
    let Some(max) = supports.max_thinking_budget else {
        return Vec::new();
    };
    vec![
        variant("max", JsonMap(vec![("thinking".to_owned(), json!({ "type": "enabled", "budgetTokens": max - 1.0 }))])),
        variant(
            "high",
            JsonMap(vec![("thinking".to_owned(), json!({ "type": "enabled", "budgetTokens": (max / 2.0).floor() }))]),
        ),
    ]
}

fn variant(id: &str, rest: JsonMap<Value>) -> ModelVariant {
    ModelVariant {
        id: id.to_owned(),
        settings: Some(ModelVariantSettings { compaction: None, encoding: None, rest }),
        headers: None,
        body: None,
    }
}

fn version(remote: &UsableModel) -> &str {
    remote
        .0
        .version
        .strip_prefix(remote.0.id.as_str())
        .and_then(|rest| rest.strip_prefix('-'))
        .unwrap_or(&remote.0.version)
}

fn released(version: &str) -> f64 {
    let Some((year, month, day)) = date(version) else {
        return 0.0;
    };
    days_from_civil(year, month, day) as f64 * 86_400_000.0
}

fn date(value: &str) -> Option<(i64, u32, u32)> {
    let bytes = value.as_bytes();
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year = value.get(..4)?.parse::<i64>().ok()?;
    let month = value.get(5..7)?.parse::<u32>().ok()?;
    let day = value.get(8..10)?.parse::<u32>().ok()?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    Some((year, month, day))
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}
