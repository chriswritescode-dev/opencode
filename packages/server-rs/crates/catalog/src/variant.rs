use std::collections::HashSet;
use std::sync::LazyLock;

use regex_lite::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub type JsonMap = Map<String, Value>;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Budget {
    #[serde(default)]
    pub min: Option<i64>,
    #[serde(default)]
    pub max: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Reasoning {
    pub supported: bool,
    #[serde(default)]
    pub toggle: bool,
    #[serde(default)]
    pub effort: Option<Vec<String>>,
    #[serde(default)]
    pub budget: Option<Budget>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Support {
    Effort {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        values: Option<Vec<String>>,
    },
    Toggle,
    BudgetTokens {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<i64>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Model {
    pub id: String,
    #[serde(rename = "modelID")]
    pub model_id: String,
    #[serde(rename = "providerID")]
    pub provider_id: String,
    pub package: Option<String>,
    pub output: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Variant {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<JsonMap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<JsonMap>,
}

#[derive(Clone, Default)]
struct Overlay {
    settings: Option<JsonMap>,
    body: Option<JsonMap>,
}

type Protocol = fn(&Model, &Support, &[Support]) -> Vec<Variant>;

const EFFORTS: [&str; 3] = ["low", "medium", "high"];
const ENCRYPTED_REASONING: [&str; 1] = ["reasoning.encrypted_content"];
const ANTHROPIC_OUTPUT_TOKEN_MAX: i64 = 32_000;
const ALIBABA_THINKING_BUDGET_MAX: i64 = 64_000;

static CLAUDE_FAMILY_FIRST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:claude-)?(opus|sonnet|haiku|fable|mythos)-(\d+)(?:[.-](\d+))?")
        .expect("the Claude family pattern is valid")
});
static CLAUDE_VERSION_FIRST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)claude-(\d+)(?:[.-](\d+))?-(opus|sonnet|haiku|fable|mythos)")
        .expect("the Claude version pattern is valid")
});
static MINIMAX_M2: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)minimax[-.]?m2(?:[.-]|$)").expect("the MiniMax M2 pattern is valid"));
static KIMI_K2_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)kimi[-.]?k2[.-]7-code").expect("the Kimi K2.7 code pattern is valid"));
static KIMI_K2: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)kimi[-.]?k2").expect("the Kimi K2 pattern is valid"));
static GLM_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"glm-?(\d+)(?:(?:[.-]|p)(\d+))?").expect("the GLM version pattern is valid"));

pub fn supports(reasoning: &Reasoning) -> Vec<Support> {
    if !reasoning.supported {
        return Vec::new();
    }
    let mut result = Vec::new();
    if reasoning.toggle {
        result.push(Support::Toggle);
    }
    if let Some(effort) = &reasoning.effort {
        result.push(Support::Effort {
            values: Some(effort.iter().filter(|value| value.as_str() != "default").cloned().collect()),
        });
    }
    if let Some(budget) = &reasoning.budget {
        result.push(Support::BudgetTokens { min: budget.min, max: budget.max });
    }
    result
}

pub fn resolve(model: &Model, supports: &[Support]) -> Vec<Variant> {
    let Some(protocol) = model.package.as_deref().and_then(protocol_for) else {
        return Vec::new();
    };
    let toggle = if supports.iter().any(|support| matches!(support, Support::Toggle)) {
        protocol(model, &Support::Toggle, supports)
    } else {
        Vec::new()
    };
    let effort = supports.iter().find(|support| matches!(support, Support::Effort { .. }));
    let budget = supports.iter().find(|support| matches!(support, Support::BudgetTokens { .. }));
    let main = match (effort, budget) {
        (Some(effort), _) => protocol(model, effort, supports),
        (None, Some(budget)) => protocol(model, budget, supports),
        (None, None) => toggle.clone(),
    };
    let mut variants: Vec<Variant> = toggle.into_iter().filter(|variant| variant.id == "none").collect();
    variants.extend(main);
    let mut seen = HashSet::new();
    variants.retain(|variant| seen.insert(variant.id.clone()));
    variants
}

fn protocol_for(package: &str) -> Option<Protocol> {
    PROTOCOLS.iter().find(|(name, _)| *name == package).map(|(_, protocol)| *protocol)
}

fn variant(id: &str, overlay: Overlay) -> Variant {
    Variant { id: id.to_string(), settings: overlay.settings, body: overlay.body }
}

fn settings(value: Value) -> Overlay {
    Overlay { settings: Some(as_object(value)), body: None }
}

fn body(value: Value) -> Overlay {
    Overlay { settings: None, body: Some(as_object(value)) }
}

fn as_object(value: Value) -> JsonMap {
    match value {
        Value::Object(map) => map,
        _ => panic!("a JSON object"),
    }
}

fn one(key: &str, value: Value) -> JsonMap {
    let mut map = JsonMap::new();
    map.insert(key.to_string(), value);
    map
}

fn efforts(values: Vec<String>, spell: impl Fn(&str) -> Overlay) -> Vec<Variant> {
    values.iter().map(|effort| variant(effort, spell(effort))).collect()
}

fn toggle(off: Overlay, on: Overlay) -> Vec<Variant> {
    vec![variant("none", off), variant("thinking", on)]
}

fn values_or(values: &Option<Vec<String>>, fallback: &[&str]) -> Vec<String> {
    match values {
        Some(values) => values.clone(),
        None => fallback.iter().map(|value| value.to_string()).collect(),
    }
}

fn budgets(model: &Model, support: &Support, spell: impl Fn(i64) -> Overlay, ceiling: i64) -> Vec<Variant> {
    let Support::BudgetTokens { min, max } = support else {
        return Vec::new();
    };
    let maximum = max.unwrap_or(ceiling - 1).min(model.output - 1).min(ceiling - 1);
    if maximum <= 0 {
        return Vec::new();
    }
    let high = min.unwrap_or(0).max((maximum + 1) / 2).min(maximum);
    vec![variant("high", spell(high)), variant("max", spell(maximum))]
}

fn adaptive_thinking() -> Value {
    json!({ "type": "adaptive", "display": "summarized" })
}

fn manual_thinking(model: &Model) -> Option<Overlay> {
    let tokens = 16_000_i64.min(model.output - 1);
    if tokens < 1024 {
        return None;
    }
    Some(settings(json!({ "thinking": { "type": "enabled", "budgetTokens": tokens } })))
}

struct ClaudeInfo {
    family: Option<String>,
    major: Option<i64>,
    minor: i64,
    manual: bool,
    always: bool,
}

fn claude_info(model: &Model) -> ClaudeInfo {
    let id = &model.model_id;
    let family_first = CLAUDE_FAMILY_FIRST.captures(id);
    let version_first = CLAUDE_VERSION_FIRST.captures(id);
    let family = family_first
        .as_ref()
        .and_then(|captures| captures.get(1))
        .or_else(|| version_first.as_ref().and_then(|captures| captures.get(3)))
        .map(|family| family.as_str().to_lowercase());
    let major = family_first
        .as_ref()
        .and_then(|captures| captures.get(2))
        .or_else(|| version_first.as_ref().and_then(|captures| captures.get(1)))
        .and_then(|major| major.as_str().parse::<i64>().ok());
    let minor = family_first
        .as_ref()
        .and_then(|captures| captures.get(3))
        .or_else(|| version_first.as_ref().and_then(|captures| captures.get(2)))
        .and_then(|minor| minor.as_str().parse::<i64>().ok())
        .unwrap_or(0);
    let manual = (major == Some(3) && minor == 7) || (major == Some(4) && minor < 6);
    let always =
        matches!(family.as_deref(), Some("fable") | Some("mythos")) || id.to_lowercase().contains("mythos-preview");
    ClaudeInfo { family, major, minor, manual, always }
}

fn openai_chat(_model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    let Support::Effort { values } = support else {
        return Vec::new();
    };
    efforts(values_or(values, &EFFORTS), |effort| settings(json!({ "reasoningEffort": effort })))
}

fn vertex_chat(model: &Model, support: &Support, supports: &[Support]) -> Vec<Variant> {
    if !matches!(support, Support::Toggle) {
        return openai_chat(model, support, supports);
    }
    let key = if model.model_id.to_lowercase().contains("deepseek") { "thinking" } else { "enable_thinking" };
    toggle(
        body(json!({ "chat_template_kwargs": one(key, json!(false)) })),
        body(json!({ "chat_template_kwargs": one(key, json!(true)) })),
    )
}

fn openai_responses(_model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    let Support::Effort { values } = support else {
        return Vec::new();
    };
    efforts(values_or(values, &["none", "minimal", "low", "medium", "high", "xhigh"]), responses_effort)
}

fn responses_effort(effort: &str) -> Overlay {
    settings(json!({
        "reasoningEffort": effort,
        "reasoningSummary": "auto",
        "include": ENCRYPTED_REASONING,
    }))
}

fn open_responses(_model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    let Support::Effort { values } = support else {
        return Vec::new();
    };
    efforts(values_or(values, &EFFORTS), responses_effort)
}

fn google_interactions(_model: &Model, support: &Support, supports: &[Support]) -> Vec<Variant> {
    if matches!(support, Support::Toggle) && supports.iter().all(|support| matches!(support, Support::Toggle)) {
        return toggle(
            settings(json!({ "thinkingLevel": "minimal" })),
            settings(json!({ "thinkingLevel": "high", "thinkingSummaries": "auto" })),
        );
    }
    let Support::Effort { values } = support else {
        return Vec::new();
    };
    efforts(values_or(values, &EFFORTS), |effort| {
        settings(json!({ "thinkingLevel": effort, "thinkingSummaries": "auto" }))
    })
}

fn cloudflare_ai_gateway(model: &Model, support: &Support, supports: &[Support]) -> Vec<Variant> {
    let id = &model.model_id;
    if id.starts_with("openai/") {
        return openai_responses(model, support, supports);
    }
    if id.starts_with("anthropic/") {
        return anthropic_messages(model, support, supports);
    }
    openai_chat(model, support, supports)
}

fn deepseek_chat(_model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    match support {
        Support::Effort { values } => efforts(values_or(values, &["low", "high", "max"]), |effort| {
            if effort == "none" {
                return body(json!({ "thinking": { "type": "disabled" } }));
            }
            Overlay {
                settings: Some(as_object(json!({ "reasoningEffort": effort }))),
                body: Some(as_object(json!({ "thinking": { "type": "enabled" } }))),
            }
        }),
        Support::Toggle => toggle(
            body(json!({ "thinking": { "type": "disabled" } })),
            body(json!({ "thinking": { "type": "enabled" } })),
        ),
        Support::BudgetTokens { .. } => Vec::new(),
    }
}

fn nvidia_template(chat_template_kwargs: JsonMap) -> Overlay {
    body(Value::Object(one("chat_template_kwargs", Value::Object(chat_template_kwargs))))
}

fn nvidia_toggle(key: &str) -> Vec<Variant> {
    toggle(nvidia_template(one(key, json!(false))), nvidia_template(one(key, json!(true))))
}

fn nvidia_chat(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    let id = model.model_id.to_lowercase();
    if id.contains("deepseek-v4") {
        if matches!(support, Support::Toggle) {
            return nvidia_toggle("thinking");
        }
        let Support::Effort { values } = support else {
            return Vec::new();
        };
        return efforts(values_or(values, &["none", "high", "max"]), |effort| {
            if effort == "none" {
                return nvidia_template(one("thinking", json!(false)));
            }
            let mut template = one("thinking", json!(true));
            template.insert("reasoning_effort".to_string(), json!(effort));
            nvidia_template(template)
        });
    }
    if id.contains("kimi-k2.6") || id.contains("kimi-k2-6") {
        return nvidia_toggle("thinking");
    }
    if id.contains("kimi-k3") {
        if matches!(support, Support::Toggle) {
            return nvidia_toggle("thinking");
        }
        let Support::Effort { values } = support else {
            return Vec::new();
        };
        return efforts(values_or(values, &["low", "high", "max"]), |effort| {
            settings(json!({ "reasoningEffort": effort }))
        });
    }
    if id.contains("minimax-m3") {
        return toggle(
            nvidia_template(one("thinking_mode", json!("disabled"))),
            nvidia_template(one("thinking_mode", json!("enabled"))),
        );
    }
    if id.contains("glm") {
        let mut enabled = JsonMap::new();
        enabled.insert("enable_thinking".to_string(), json!(true));
        enabled.insert("clear_thinking".to_string(), json!(false));
        return toggle(nvidia_template(one("enable_thinking", json!(false))), nvidia_template(enabled));
    }
    let toggle_only = id.contains("qwen") || id.contains("gemma") || id.contains("nemotron");
    match support {
        Support::Effort { values } => {
            if toggle_only && values.is_none() {
                nvidia_toggle("enable_thinking")
            } else {
                openai_chat(model, support, &[])
            }
        }
        Support::Toggle => nvidia_toggle("enable_thinking"),
        Support::BudgetTokens { .. } => budgets(
            model,
            support,
            |tokens| {
                body(json!({
                    "chat_template_kwargs": { "enable_thinking": true },
                    "reasoning_budget": tokens,
                }))
            },
            model.output,
        ),
    }
}

fn workers_ai_template(thinking: bool) -> Overlay {
    body(json!({ "chat_template_kwargs": { "enable_thinking": thinking, "thinking": thinking } }))
}

fn workers_ai_chat(_model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    match support {
        Support::Effort { values } => efforts(values_or(values, &EFFORTS), |effort| {
            if effort == "none" {
                return workers_ai_template(false);
            }
            settings(json!({ "reasoningEffort": effort }))
        }),
        Support::Toggle => toggle(workers_ai_template(false), workers_ai_template(true)),
        Support::BudgetTokens { .. } => Vec::new(),
    }
}

fn baseten_template(enable_thinking: bool) -> Overlay {
    body(json!({ "chat_template_args": { "enable_thinking": enable_thinking } }))
}

fn baseten_chat(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    let id = model.model_id.to_lowercase();
    match support {
        Support::Effort { values } => {
            let glm = id.contains("glm-5.2") || id.contains("glm-5-2") || id.contains("glm-5p2");
            efforts(values_or(values, &EFFORTS), |effort| {
                let mut spell = settings(json!({ "reasoningEffort": effort }));
                if glm {
                    spell.body = baseten_template(effort != "none").body;
                }
                if id.contains("deepseek-v4-pro-0813") && effort != "none" {
                    spell.body = body(json!({ "thinking": { "type": "enabled" } })).body;
                }
                spell
            })
        }
        Support::Toggle => toggle(baseten_template(false), baseten_template(true)),
        Support::BudgetTokens { .. } => Vec::new(),
    }
}

fn deepinfra_chat(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    if KIMI_K2_CODE.is_match(&model.model_id) {
        return Vec::new();
    }
    match support {
        Support::Effort { values } => {
            efforts(values_or(values, &EFFORTS), |effort| settings(json!({ "reasoningEffort": effort })))
        }
        Support::Toggle => toggle(
            body(json!({ "reasoning": { "enabled": false } })),
            body(json!({ "reasoning": { "enabled": true } })),
        ),
        Support::BudgetTokens { .. } => Vec::new(),
    }
}

fn openai_compatible(model: &Model, support: &Support, supports: &[Support]) -> Vec<Variant> {
    if model.provider_id == "nvidia" {
        return nvidia_chat(model, support, supports);
    }
    openai_chat(model, support, supports)
}

fn venice_chat(_model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    match support {
        Support::Effort { values } => {
            efforts(values_or(values, &EFFORTS), |effort| settings(json!({ "reasoningEffort": effort })))
        }
        Support::Toggle => toggle(
            settings(json!({ "reasoning": { "enabled": false } })),
            settings(json!({ "reasoning": { "enabled": true } })),
        ),
        Support::BudgetTokens { .. } => Vec::new(),
    }
}

fn moonshot_chat(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    let id = model.model_id.to_lowercase();
    let toggleable = id.contains("k2.5") || id.contains("k2-5") || id.contains("k2.6") || id.contains("k2-6");
    let fixed = id.contains("k2-thinking") || id.contains("k2.7-code") || id.contains("k2-7-code");
    if fixed {
        return Vec::new();
    }
    if toggleable && (matches!(support, Support::Toggle) || matches!(support, Support::Effort { values: None })) {
        return toggle(
            settings(json!({ "thinking": { "type": "disabled" } })),
            settings(json!({ "thinking": { "type": "enabled" } })),
        );
    }
    if toggleable || !matches!(support, Support::Effort { .. }) {
        return Vec::new();
    }
    let Support::Effort { values } = support else {
        return Vec::new();
    };
    efforts(values_or(values, &["low", "high", "max"]), |effort| settings(json!({ "reasoningEffort": effort })))
}

fn alibaba_chat(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    let id = model.model_id.to_lowercase();
    let fixed =
        id.contains("-thinking") || id.contains("qwq") || id.contains("deepseek-r1") || id.contains("minimax-m2");
    match support {
        Support::Effort { values } => {
            if fixed && values.is_none() {
                return Vec::new();
            }
            let hosted = id.contains("glm") || id.contains("deepseek");
            let fallback: &[&str] = if hosted { &["high", "max"] } else { &["low", "medium", "xhigh"] };
            efforts(values_or(values, fallback), |effort| {
                if effort == "none" {
                    return settings(json!({ "enableThinking": false }));
                }
                settings(json!({ "enableThinking": true, "reasoningEffort": effort }))
            })
        }
        Support::Toggle => {
            toggle(settings(json!({ "enableThinking": false })), settings(json!({ "enableThinking": true })))
        }
        Support::BudgetTokens { .. } => budgets(
            model,
            support,
            |tokens| settings(json!({ "enableThinking": true, "thinkingBudget": tokens })),
            ALIBABA_THINKING_BUDGET_MAX,
        ),
    }
}

fn zai_chat(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    let id = model.model_id.to_lowercase();
    let version = GLM_VERSION.captures(&id);
    let major =
        version.as_ref().and_then(|captures| captures.get(1)).and_then(|major| major.as_str().parse::<i64>().ok());
    let minor = version
        .as_ref()
        .and_then(|captures| captures.get(2))
        .and_then(|minor| minor.as_str().parse::<i64>().ok())
        .unwrap_or(0);
    let coding = model.package.as_deref().is_some_and(|package| package.contains("zai-coding-plan"));
    let latest = coding || major.is_none_or(|major| major > 5 || (major == 5 && minor >= 3));
    let effort = major == Some(5) && minor == 2;
    if matches!(support, Support::Toggle) || (!latest && !effort && matches!(support, Support::Effort { values: None }))
    {
        if latest {
            return Vec::new();
        }
        return toggle(
            settings(json!({ "thinking": { "type": "disabled" } })),
            settings(json!({ "thinking": { "type": "enabled", "clear_thinking": false } })),
        );
    }
    match support {
        Support::Effort { values } => {
            let fallback: &[&str] = if latest { &["low", "high", "max"] } else { &["high", "max"] };
            efforts(values_or(values, fallback), |value| {
                if value == "none" || value == "minimal" {
                    return settings(json!({ "thinking": { "type": "disabled" } }));
                }
                settings(json!({
                    "thinking": { "type": "enabled", "clear_thinking": false },
                    "reasoningEffort": value,
                }))
            })
        }
        _ => Vec::new(),
    }
}

fn anthropic_messages(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    let info = claude_info(model);
    let opus45 = info.family.as_deref() == Some("opus") && info.major == Some(4) && info.minor == 5;
    match support {
        Support::Effort { values } => {
            if info.manual && !opus45 {
                return anthropic_messages(model, &Support::BudgetTokens { min: Some(1024), max: None }, _supports);
            }
            let thinking = if opus45 {
                manual_thinking(model)
            } else {
                Some(settings(json!({ "thinking": adaptive_thinking() })))
            };
            let Some(thinking) = thinking else {
                return Vec::new();
            };
            let defaults: &[&str] = if info.major == Some(4) && info.minor == 6 {
                &["low", "medium", "high", "max"]
            } else {
                &["low", "medium", "high", "xhigh", "max"]
            };
            let values: Vec<String> =
                values_or(values, defaults).into_iter().filter(|effort| !info.always || effort != "none").collect();
            let base = thinking.settings.unwrap_or_default();
            efforts(values, |effort| {
                if effort == "none" {
                    return settings(json!({ "thinking": { "type": "disabled" } }));
                }
                let mut map = base.clone();
                map.insert("effort".to_string(), json!(effort));
                Overlay { settings: Some(map), body: None }
            })
        }
        Support::Toggle => {
            if info.always {
                return Vec::new();
            }
            let thinking = if info.manual {
                manual_thinking(model)
            } else {
                Some(settings(json!({ "thinking": adaptive_thinking() })))
            };
            let Some(thinking) = thinking else {
                return Vec::new();
            };
            toggle(settings(json!({ "thinking": { "type": "disabled" } })), thinking)
        }
        Support::BudgetTokens { .. } => budgets(
            model,
            support,
            |tokens| settings(json!({ "thinking": { "type": "enabled", "budgetTokens": tokens } })),
            ANTHROPIC_OUTPUT_TOKEN_MAX,
        ),
    }
}

fn minimax_messages(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    if MINIMAX_M2.is_match(&model.model_id) {
        return Vec::new();
    }
    let configurable = matches!(support, Support::Toggle) || matches!(support, Support::Effort { values: None });
    if !configurable {
        return Vec::new();
    }
    toggle(
        settings(json!({ "thinking": { "type": "disabled" } })),
        settings(json!({ "thinking": { "type": "adaptive" } })),
    )
}

fn moonshot_messages(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    let Support::Effort { values } = support else {
        return Vec::new();
    };
    if KIMI_K2.is_match(&model.model_id) {
        return Vec::new();
    }
    efforts(values_or(values, &["low", "high", "max"]), |effort| settings(json!({ "effort": effort })))
}

fn alibaba_messages(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    let id = model.model_id.to_lowercase();
    match support {
        Support::Effort { values } => {
            let hosted = id.contains("glm") || id.contains("deepseek");
            let fallback: &[&str] = if hosted { &["high", "max"] } else { &["low", "medium", "xhigh"] };
            efforts(values_or(values, fallback), |effort| {
                if effort == "none" {
                    return settings(json!({ "thinking": { "type": "disabled" } }));
                }
                settings(json!({ "thinking": { "type": "enabled" }, "effort": effort }))
            })
        }
        Support::Toggle => toggle(
            settings(json!({ "thinking": { "type": "disabled" } })),
            settings(json!({ "thinking": { "type": "enabled" } })),
        ),
        Support::BudgetTokens { .. } => budgets(
            model,
            support,
            |tokens| settings(json!({ "thinking": { "type": "enabled", "budgetTokens": tokens } })),
            ALIBABA_THINKING_BUDGET_MAX,
        ),
    }
}

fn zai_messages(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    let id = model.model_id.to_lowercase();
    let forced = id.contains("glm-5.3") || id.contains("glm-5-3") || id.contains("glm-5p3");
    match support {
        Support::Effort { values } => {
            let fallback: &[&str] = if forced { &["low", "high", "max"] } else { &["high", "max"] };
            efforts(values_or(values, fallback), |effort| {
                let thinking = if effort == "none" || effort == "minimal" { "disabled" } else { "enabled" };
                settings(json!({ "thinking": { "type": thinking }, "effort": effort }))
            })
        }
        Support::Toggle => {
            if forced {
                return Vec::new();
            }
            toggle(
                settings(json!({ "thinking": { "type": "disabled" } })),
                settings(json!({ "thinking": { "type": "enabled" } })),
            )
        }
        Support::BudgetTokens { .. } => Vec::new(),
    }
}

fn gemini(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    match support {
        Support::Effort { values } => efforts(values_or(values, &EFFORTS), |effort| {
            settings(json!({ "thinkingConfig": { "includeThoughts": true, "thinkingLevel": effort } }))
        }),
        Support::Toggle => toggle(
            settings(json!({ "thinkingConfig": { "includeThoughts": false, "thinkingBudget": 0 } })),
            settings(json!({ "thinkingConfig": { "includeThoughts": true, "thinkingBudget": -1 } })),
        ),
        Support::BudgetTokens { .. } => budgets(
            model,
            support,
            |tokens| settings(json!({ "thinkingConfig": { "includeThoughts": true, "thinkingBudget": tokens } })),
            model.output,
        ),
    }
}

fn openrouter(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    match support {
        Support::Effort { values } => {
            efforts(values_or(values, &EFFORTS), |effort| settings(json!({ "reasoning": { "effort": effort } })))
        }
        Support::Toggle => toggle(
            settings(json!({ "reasoning": { "enabled": false } })),
            settings(json!({ "reasoning": { "enabled": true } })),
        ),
        Support::BudgetTokens { .. } => {
            budgets(model, support, |tokens| settings(json!({ "reasoning": { "max_tokens": tokens } })), model.output)
        }
    }
}

fn bedrock_converse(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    let id = &model.model_id;
    let claude = id.contains("anthropic");
    let fields = |fields: JsonMap| body(Value::Object(one("additionalModelRequestFields", Value::Object(fields))));
    match support {
        Support::Effort { values } => efforts(values_or(values, &EFFORTS), |effort| {
            if claude {
                let mut request = JsonMap::new();
                if !claude_info(model).manual {
                    request.insert("thinking".to_string(), adaptive_thinking());
                }
                request.insert("output_config".to_string(), json!({ "effort": effort }));
                return fields(request);
            }
            if id.contains("openai.gpt-oss") {
                return fields(one("reasoning_effort", json!(effort)));
            }
            if id.contains("openai.") || id.contains("xai.") {
                return fields(one("reasoning", json!({ "effort": effort })));
            }
            fields(one("reasoningConfig", json!({ "type": "enabled", "maxReasoningEffort": effort })))
        }),
        Support::Toggle => {
            if claude {
                return toggle(
                    fields(one("thinking", json!({ "type": "disabled" }))),
                    fields(one("thinking", adaptive_thinking())),
                );
            }
            toggle(
                fields(one("reasoningConfig", json!({ "type": "disabled" }))),
                fields(one("reasoningConfig", json!({ "type": "enabled" }))),
            )
        }
        Support::BudgetTokens { .. } => budgets(
            model,
            support,
            |tokens| {
                if claude {
                    return settings(json!({ "thinking": { "type": "enabled", "budgetTokens": tokens } }));
                }
                fields(one("reasoningConfig", json!({ "type": "enabled", "budgetTokens": tokens })))
            },
            if claude { ANTHROPIC_OUTPUT_TOKEN_MAX } else { model.output },
        ),
    }
}

fn cohere(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    match support {
        Support::Effort { .. } => Vec::new(),
        Support::Toggle => toggle(
            settings(json!({ "thinking": { "type": "disabled" } })),
            settings(json!({ "thinking": { "type": "enabled" } })),
        ),
        Support::BudgetTokens { .. } => budgets(
            model,
            support,
            |tokens| settings(json!({ "thinking": { "type": "enabled", "tokenBudget": tokens } })),
            model.output,
        ),
    }
}

fn vercel_gateway(model: &Model, support: &Support, supports: &[Support]) -> Vec<Variant> {
    let id = &model.model_id;
    if id.starts_with("openai/gpt-") {
        return openai_responses(model, support, supports);
    }
    if id.starts_with("spacexai/grok-") {
        return open_responses(model, support, supports);
    }
    vercel_messages(model, support, supports)
}

fn vercel_responses(model: &Model, support: &Support, supports: &[Support]) -> Vec<Variant> {
    if model.model_id.starts_with("openai/gpt-") {
        return openai_responses(model, support, supports);
    }
    open_responses(model, support, supports)
}

fn vercel_messages(model: &Model, support: &Support, supports: &[Support]) -> Vec<Variant> {
    if model.model_id.starts_with("anthropic/") {
        return anthropic_messages(model, support, supports);
    }
    vercel_chat(model, support, supports)
}

fn vercel_chat(model: &Model, support: &Support, supports: &[Support]) -> Vec<Variant> {
    match support {
        Support::Effort { .. } => openai_chat(model, support, supports),
        Support::Toggle => toggle(
            settings(json!({ "thinking": { "type": "disabled" } })),
            settings(json!({ "thinking": { "type": "adaptive" } })),
        ),
        Support::BudgetTokens { .. } => budgets(
            model,
            support,
            |tokens| settings(json!({ "thinking": { "type": "enabled", "budgetTokens": tokens } })),
            if model.model_id.starts_with("alibaba/") { ALIBABA_THINKING_BUDGET_MAX } else { model.output },
        ),
    }
}

fn sap_ai_core(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    let id = &model.model_id;
    let sap = |model_params: JsonMap| settings(Value::Object(one("modelParams", Value::Object(model_params))));
    match support {
        Support::Effort { values } => efforts(values_or(values, &EFFORTS), |effort| {
            if id.contains("anthropic") {
                let mut request = JsonMap::new();
                if !claude_info(model).manual {
                    request.insert("thinking".to_string(), adaptive_thinking());
                }
                request.insert("output_config".to_string(), json!({ "effort": effort }));
                return sap(one("additionalModelRequestFields", Value::Object(request)));
            }
            if id.contains("gemini") {
                return sap(one("thinkingConfig", json!({ "includeThoughts": true, "thinkingLevel": effort })));
            }
            if id.contains("amazon--nova") {
                return sap(one("additionalModelRequestFields", json!({ "output_config": { "effort": effort } })));
            }
            sap(one("reasoning_effort", json!(effort)))
        }),
        Support::Toggle => {
            if id.contains("gemini") {
                return toggle(
                    sap(one("thinkingConfig", json!({ "includeThoughts": false, "thinkingBudget": 0 }))),
                    sap(one("thinkingConfig", json!({ "includeThoughts": true, "thinkingBudget": -1 }))),
                );
            }
            if id.contains("cohere") {
                return toggle(
                    sap(one("thinking", json!({ "type": "disabled" }))),
                    sap(one("thinking", json!({ "type": "enabled" }))),
                );
            }
            if id.contains("amazon--nova") {
                return toggle(
                    sap(one("additionalModelRequestFields", json!({ "thinking": { "type": "disabled" } }))),
                    sap(one("additionalModelRequestFields", json!({ "thinking": { "type": "enabled" } }))),
                );
            }
            if id.contains("anthropic") {
                return toggle(
                    sap(one("additionalModelRequestFields", json!({ "thinking": { "type": "disabled" } }))),
                    sap(one("additionalModelRequestFields", Value::Object(one("thinking", adaptive_thinking())))),
                );
            }
            Vec::new()
        }
        Support::BudgetTokens { .. } => {
            if id.contains("anthropic") {
                return budgets(
                    model,
                    support,
                    |tokens| {
                        sap(one(
                            "additionalModelRequestFields",
                            json!({ "thinking": { "type": "enabled", "budget_tokens": tokens } }),
                        ))
                    },
                    ANTHROPIC_OUTPUT_TOKEN_MAX,
                );
            }
            if id.contains("gemini") {
                return budgets(
                    model,
                    support,
                    |tokens| sap(one("thinkingConfig", json!({ "includeThoughts": true, "thinkingBudget": tokens }))),
                    model.output,
                );
            }
            if id.contains("cohere") {
                return budgets(
                    model,
                    support,
                    |tokens| sap(one("thinking", json!({ "type": "enabled", "token_budget": tokens }))),
                    model.output,
                );
            }
            Vec::new()
        }
    }
}

fn gitlab_anthropic(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    let info = claude_info(model);
    let disabled = || settings(json!({ "thinking": { "type": "disabled" } }));
    let adaptive = |effort: &str| settings(json!({ "thinking": { "type": "adaptive", "effort": effort } }));
    match support {
        Support::Effort { values } => {
            if info.manual {
                return gitlab_anthropic(model, &Support::BudgetTokens { min: Some(1024), max: None }, &[]);
            }
            let defaults: &[&str] = if info.major == Some(4) && info.minor == 6 {
                &["low", "medium", "high", "max"]
            } else {
                &["low", "medium", "high", "xhigh", "max"]
            };
            efforts(values_or(values, defaults), |effort| if effort == "none" { disabled() } else { adaptive(effort) })
        }
        Support::Toggle => {
            if info.always {
                return Vec::new();
            }
            let thinking = if info.manual { manual_thinking(model) } else { Some(adaptive("high")) };
            let Some(thinking) = thinking else {
                return Vec::new();
            };
            toggle(disabled(), thinking)
        }
        Support::BudgetTokens { .. } => budgets(
            model,
            support,
            |tokens| settings(json!({ "thinking": { "type": "enabled", "budgetTokens": tokens } })),
            ANTHROPIC_OUTPUT_TOKEN_MAX,
        ),
    }
}

fn gitlab(model: &Model, support: &Support, _supports: &[Support]) -> Vec<Variant> {
    let id = model.model_id.to_lowercase();
    if id.starts_with("duo-workflow") {
        return Vec::new();
    }
    if claude_info(model).family.is_some() {
        return gitlab_anthropic(model, support, &[]);
    }
    if id.contains("gpt") || id.contains("codex") {
        return openai_chat(model, support, &[]);
    }
    Vec::new()
}

const PROTOCOLS: &[(&str, Protocol)] = &[
    ("@opencode/ai/providers/openai", openai_responses),
    ("@opencode/ai/providers/openai/responses", openai_responses),
    ("@opencode/ai/providers/azure", openai_responses),
    ("@opencode/ai/providers/azure/responses", openai_responses),
    ("@opencode/ai/providers/amazon-bedrock/mantle", openai_responses),
    ("@opencode/ai/providers/amazon-bedrock/mantle/responses", openai_responses),
    ("@opencode/ai/providers/alibaba/responses", openai_responses),
    ("@opencode/ai/providers/meta", openai_responses),
    ("@opencode/ai/providers/meta/responses", openai_responses),
    ("@opencode/ai/providers/minimax/responses", openai_responses),
    ("@opencode/ai/providers/moonshot/responses", openai_responses),
    ("@opencode/ai/providers/zai-coding-plan/responses", openai_responses),
    ("@opencode/ai/providers/openai-compatible/responses", open_responses),
    ("@opencode/ai/providers/openai-compatible-responses", open_responses),
    ("@opencode/ai/providers/google-vertex/responses", open_responses),
    ("@opencode/ai/providers/xai", open_responses),
    ("@opencode/ai/providers/digitalocean/responses", open_responses),
    ("@opencode/ai/providers/openrouter/responses", open_responses),
    ("@opencode/ai/providers/venice/responses", open_responses),
    ("@opencode/ai/providers/openai-compatible", openai_compatible),
    ("@opencode/ai/providers/openai/chat", openai_chat),
    ("@opencode/ai/providers/azure/chat", openai_chat),
    ("@opencode/ai/providers/amazon-bedrock/mantle/chat", openai_chat),
    ("@opencode/ai/providers/google-vertex/chat", vertex_chat),
    ("@opencode/ai/providers/alibaba", alibaba_chat),
    ("@opencode/ai/providers/alibaba/chat", alibaba_chat),
    ("@opencode/ai/providers/baseten", baseten_chat),
    ("@opencode/ai/providers/cerebras", openai_chat),
    ("@opencode/ai/providers/cloudflare-workers-ai", workers_ai_chat),
    ("@opencode/ai/providers/cohere/chat", openai_chat),
    ("@opencode/ai/providers/deepinfra", deepinfra_chat),
    ("@opencode/ai/providers/deepseek", deepseek_chat),
    ("@opencode/ai/providers/digitalocean", openai_chat),
    ("@opencode/ai/providers/fireworks", openai_chat),
    ("@opencode/ai/providers/groq", openai_chat),
    ("@opencode/ai/providers/meta/chat", openai_chat),
    ("@opencode/ai/providers/minimax/chat", openai_chat),
    ("@opencode/ai/providers/mistral", openai_chat),
    ("@opencode/ai/providers/google-vertex/mistral", openai_chat),
    ("@opencode/ai/providers/moonshot", moonshot_chat),
    ("@opencode/ai/providers/moonshot/chat", moonshot_chat),
    ("@opencode/ai/providers/togetherai", openai_chat),
    ("@opencode/ai/providers/venice", venice_chat),
    ("@opencode/ai/providers/zai", zai_chat),
    ("@opencode/ai/providers/zai/chat", zai_chat),
    ("@opencode/ai/providers/zai-coding-plan", zai_chat),
    ("@opencode/ai/providers/zai-coding-plan/chat", zai_chat),
    ("@opencode/ai/providers/anthropic", anthropic_messages),
    ("@opencode/ai/providers/anthropic-compatible", anthropic_messages),
    ("@opencode/ai/providers/amazon-bedrock/mantle/messages", anthropic_messages),
    ("@opencode/ai/providers/azure/messages", anthropic_messages),
    ("@opencode/ai/providers/google-vertex/messages", anthropic_messages),
    ("@opencode/ai/providers/google-vertex-messages", anthropic_messages),
    ("@opencode/ai/providers/alibaba/messages", alibaba_messages),
    ("@opencode/ai/providers/meta/messages", anthropic_messages),
    ("@opencode/ai/providers/minimax", minimax_messages),
    ("@opencode/ai/providers/minimax/messages", minimax_messages),
    ("@opencode/ai/providers/moonshot/messages", moonshot_messages),
    ("@opencode/ai/providers/zai-coding-plan/messages", zai_messages),
    ("@opencode/ai/providers/google", gemini),
    ("@opencode/ai/providers/google-vertex", gemini),
    ("@opencode/ai/providers/google-vertex/gemini", gemini),
    ("@opencode/ai/providers/google/images", gemini),
    ("@opencode/ai/providers/google-vertex/images", gemini),
    ("@opencode/ai/providers/google/interactions", google_interactions),
    ("@opencode/ai/providers/google-vertex/interactions", google_interactions),
    ("@opencode/ai/providers/amazon-bedrock", bedrock_converse),
    ("@opencode/ai/providers/cohere", cohere),
    ("@opencode/ai/providers/openrouter", openrouter),
    ("aisdk:venice-ai-sdk-provider", venice_chat),
    ("@opencode/ai/providers/cloudflare-ai-gateway", cloudflare_ai_gateway),
    ("@opencode/ai/providers/vercel-ai-gateway", vercel_gateway),
    ("@opencode/ai/providers/vercel-ai-gateway/chat", vercel_chat),
    ("@opencode/ai/providers/vercel-ai-gateway/messages", vercel_messages),
    ("@opencode/ai/providers/vercel-ai-gateway/responses", vercel_responses),
    ("aisdk:@ai-sdk/gateway", vercel_gateway),
    ("aisdk:@jerome-benoit/sap-ai-provider-v2", sap_ai_core),
    ("aisdk:gitlab-ai-provider", gitlab),
];
