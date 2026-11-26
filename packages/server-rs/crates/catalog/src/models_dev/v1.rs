use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::{Map, Value};

const RESPONSES_BY_DEFAULT: [&str; 3] = ["@ai-sdk/openai", "@ai-sdk/azure", "@ai-sdk/xai"];

#[derive(Clone, Debug, Deserialize)]
struct Provider {
    id: String,
    name: String,
    env: Vec<String>,
    npm: String,
    api: Option<String>,
    models: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize)]
struct Model {
    id: String,
    name: String,
    #[serde(rename = "type")]
    kind: Option<String>,
    family: Option<String>,
    release_date: String,
    status: Option<String>,
    reasoning: Option<bool>,
    reasoning_options: Option<Vec<ReasoningOption>>,
    temperature: Option<bool>,
    tool_call: Option<bool>,
    interleaved: Option<Interleaved>,
    cost: Option<Cost>,
    limit: Option<IndexMap<String, f64>>,
    modalities: Option<Modalities>,
    experimental: Option<Experimental>,
    provider: Option<ModelProvider>,
}

#[derive(Clone, Debug, Deserialize)]
struct ReasoningOption {
    #[serde(rename = "type")]
    kind: String,
    values: Option<Vec<Option<String>>>,
    min: Option<f64>,
    max: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
struct Cost {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
    tiers: Option<Vec<Tier>>,
}

#[derive(Clone, Debug, Deserialize)]
struct Tier {
    tier: TierSize,
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
struct TierSize {
    size: f64,
}

#[derive(Clone, Debug, Deserialize)]
struct Modalities {
    input: Vec<String>,
    output: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct Experimental {
    modes: Option<IndexMap<String, Mode>>,
}

#[derive(Clone, Debug, Deserialize)]
struct Mode {
    cost: Option<Cost>,
    provider: Option<ModeProvider>,
}

#[derive(Clone, Debug, Deserialize)]
struct ModeProvider {
    body: Option<Map<String, Value>>,
    headers: Option<IndexMap<String, String>>,
}

#[derive(Clone, Debug, Deserialize)]
struct ModelProvider {
    npm: Option<String>,
    api: Option<String>,
    shape: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum Interleaved {
    Boolean(#[allow(dead_code)] bool),
    Text(String),
    Field { field: String },
}

pub fn is_v1(catalog: &Map<String, Value>) -> bool {
    catalog.values().any(|provider| provider.get("npm").is_some_and(Value::is_string))
}

pub fn to_v2(catalog: &Map<String, Value>) -> Map<String, Value> {
    catalog
        .iter()
        .filter_map(|(key, value)| {
            let provider = Provider::deserialize(value).ok()?;
            let models = provider
                .models
                .iter()
                .filter_map(|(id, value)| {
                    let model = Model::deserialize(value).ok()?;
                    Some((id.clone(), convert_model(&model, &provider)))
                })
                .collect::<Map<String, Value>>();
            let mut entry = Map::new();
            entry.insert("id".to_string(), Value::String(provider.id));
            entry.insert("name".to_string(), Value::String(provider.name));
            entry.insert("env".to_string(), Value::Array(provider.env.into_iter().map(Value::String).collect()));
            entry.insert("models".to_string(), Value::Object(models));
            Some((key.clone(), Value::Object(entry)))
        })
        .collect()
}

fn convert_model(model: &Model, provider: &Provider) -> Value {
    let npm = model.provider.as_ref().and_then(|entry| entry.npm.as_deref()).unwrap_or(&provider.npm);
    let shape = model.provider.as_ref().and_then(|entry| entry.shape.as_deref());
    let protocol = if shape == Some("responses") || (shape.is_none() && RESPONSES_BY_DEFAULT.contains(&npm)) {
        "responses"
    } else {
        "chat-completions"
    };
    let field = match &model.interleaved {
        Some(Interleaved::Text(field)) => Some(field.as_str()),
        Some(Interleaved::Field { field }) => Some(field.as_str()),
        _ => None,
    }
    .filter(|field| !field.is_empty());
    let base_url =
        model.provider.as_ref().and_then(|entry| entry.api.as_deref()).or(provider.api.as_deref()).unwrap_or("");

    let mut sdk = Map::new();
    sdk.insert("aisdk".to_string(), Value::String(npm.to_string()));
    let mut api = Map::new();
    api.insert("base_url".to_string(), Value::String(base_url.to_string()));
    api.insert("sdk".to_string(), Value::Object(sdk));
    if let Some(field) = field {
        let mut compatibility = Map::new();
        compatibility.insert("reasoning_field".to_string(), Value::String(field.to_string()));
        api.insert("compatibility".to_string(), Value::Object(compatibility));
    }

    let mut tools = Map::new();
    tools.insert("supported".to_string(), Value::Bool(model.tool_call.unwrap_or(false)));
    let mut capabilities = Map::new();
    capabilities.insert("tools".to_string(), Value::Object(tools));
    capabilities.insert("reasoning".to_string(), reasoning(model));
    if let Some(temperature) = model.temperature {
        capabilities.insert("temperature".to_string(), Value::Bool(temperature));
    }

    let modalities = match &model.modalities {
        Some(modalities) => modalities_value(&modalities.input, &modalities.output),
        None => modalities_value(&["text".to_string()], &["text".to_string()]),
    };

    let mut out = Map::new();
    out.insert("id".to_string(), Value::String(model.id.clone()));
    out.insert("type".to_string(), Value::String(model.kind.clone().unwrap_or_else(|| "chat".to_string())));
    out.insert("name".to_string(), Value::String(model.name.clone()));
    if let Some(family) = &model.family {
        out.insert("family".to_string(), Value::String(family.clone()));
    }
    out.insert("release_date".to_string(), Value::String(model.release_date.clone()));
    if let Some(status) = &model.status {
        out.insert("status".to_string(), Value::String(status.clone()));
    }
    out.insert("modalities".to_string(), modalities);
    out.insert("capabilities".to_string(), Value::Object(capabilities));
    if let Some(limit) = &model.limit {
        out.insert(
            "limit".to_string(),
            Value::Object(limit.iter().map(|(key, value)| (key.clone(), Value::from(*value))).collect()),
        );
    }
    if let Some(cost) = &model.cost {
        out.insert("cost".to_string(), cost_value(cost));
    }
    out.insert("api".to_string(), {
        let mut entry = Map::new();
        entry.insert(protocol.to_string(), Value::Object(api));
        Value::Object(entry)
    });
    if let Some(modes) = model.experimental.as_ref().and_then(|experimental| experimental.modes.as_ref()) {
        let mut experimental = Map::new();
        experimental.insert(
            "modes".to_string(),
            Value::Object(
                modes
                    .iter()
                    .map(|(name, mode)| {
                        let mut entry = Map::new();
                        if let Some(cost) = &mode.cost {
                            entry.insert("cost".to_string(), cost_value(cost));
                        }
                        if let Some(body) = mode.provider.as_ref().and_then(|entry| entry.body.as_ref()) {
                            entry.insert("body".to_string(), Value::Object(body.clone()));
                        }
                        if let Some(headers) = mode.provider.as_ref().and_then(|entry| entry.headers.as_ref()) {
                            entry.insert(
                                "headers".to_string(),
                                Value::Object(
                                    headers
                                        .iter()
                                        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                                        .collect(),
                                ),
                            );
                        }
                        (name.clone(), Value::Object(entry))
                    })
                    .collect(),
            ),
        );
        out.insert("experimental".to_string(), Value::Object(experimental));
    }
    Value::Object(out)
}

fn modalities_value(input: &[String], output: &[String]) -> Value {
    let mut entry = Map::new();
    entry.insert("input".to_string(), Value::Array(input.iter().cloned().map(Value::String).collect()));
    entry.insert("output".to_string(), Value::Array(output.iter().cloned().map(Value::String).collect()));
    Value::Object(entry)
}

fn reasoning(model: &Model) -> Value {
    let mut out = Map::new();
    if model.reasoning != Some(true) {
        out.insert("supported".to_string(), Value::Bool(false));
        return Value::Object(out);
    }
    out.insert("supported".to_string(), Value::Bool(true));
    let options = model.reasoning_options.as_deref().unwrap_or(&[]);
    if options.iter().any(|option| option.kind == "toggle") {
        out.insert("toggle".to_string(), Value::Bool(true));
    }
    if let Some(effort) = options.iter().find(|option| option.kind == "effort") {
        let values = effort
            .values
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .filter_map(|value| value.as_deref())
            .filter(|value| *value != "null")
            .map(|value| Value::String(value.to_string()))
            .collect();
        out.insert("effort".to_string(), Value::Array(values));
    }
    if let Some(budget) = options.iter().find(|option| option.kind == "budget_tokens") {
        let mut entry = Map::new();
        if let Some(min) = budget.min {
            entry.insert("min".to_string(), integer(min));
        }
        if let Some(max) = budget.max {
            entry.insert("max".to_string(), integer(max));
        }
        out.insert("budget".to_string(), Value::Object(entry));
    }
    Value::Object(out)
}

fn integer(value: f64) -> Value {
    if value.fract() == 0.0 {
        return Value::from(value as i64);
    }
    Value::from(value)
}

fn number(value: Option<f64>) -> Value {
    value.map(Value::from).unwrap_or(Value::Null)
}

fn context_condition(size: f64) -> Value {
    let mut context = Map::new();
    context.insert("gt".to_string(), Value::from(size));
    let mut when = Map::new();
    when.insert("context".to_string(), Value::Object(context));
    Value::Object(when)
}

fn cost_value(input: &Cost) -> Value {
    let tiers = input.tiers.as_deref().unwrap_or(&[]);

    let mut input_cache = Map::new();
    input_cache.insert("read".to_string(), number(input.cache_read));
    input_cache.insert("write".to_string(), number(input.cache_write));
    let mut text_input = Map::new();
    text_input.insert("price".to_string(), Value::from(input.input.unwrap_or(0.0)));
    text_input.insert("cache".to_string(), Value::Object(input_cache));
    text_input.insert(
        "overrides".to_string(),
        Value::Array(
            tiers
                .iter()
                .map(|tier| {
                    let mut entry = Map::new();
                    entry.insert("when".to_string(), context_condition(tier.tier.size));
                    if let Some(price) = tier.input {
                        entry.insert("price".to_string(), Value::from(price));
                    }
                    let mut cache = Map::new();
                    cache.insert("read".to_string(), number(tier.cache_read));
                    cache.insert("write".to_string(), number(tier.cache_write));
                    entry.insert("cache".to_string(), Value::Object(cache));
                    Value::Object(entry)
                })
                .collect(),
        ),
    );

    let mut text_output = Map::new();
    text_output.insert("price".to_string(), Value::from(input.output.unwrap_or(0.0)));
    text_output.insert(
        "overrides".to_string(),
        Value::Array(
            tiers
                .iter()
                .map(|tier| {
                    let mut entry = Map::new();
                    entry.insert("when".to_string(), context_condition(tier.tier.size));
                    if let Some(price) = tier.output {
                        entry.insert("price".to_string(), Value::from(price));
                    }
                    Value::Object(entry)
                })
                .collect(),
        ),
    );

    let mut text = Map::new();
    text.insert("input".to_string(), Value::Object(text_input));
    text.insert("output".to_string(), Value::Object(text_output));
    let mut out = Map::new();
    out.insert("text".to_string(), Value::Object(text));
    Value::Object(out)
}
