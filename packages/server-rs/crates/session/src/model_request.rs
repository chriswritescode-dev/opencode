use opencode_ai::{
    CachePolicy, CachePolicyObject, Freeform, GenerationOptions, LlmRequest, StringMap, SystemPart, ToolEntry,
};
use opencode_contract::generated::{ModelInfo, ModelInfoLimit, ToolFreeformFormat};
use serde_json::{Value, json};

use crate::context::Loaded;

const OUTPUT_TOKEN_FALLBACK: u64 = 32_000;
const OUTPUT_TOKEN_MAX: u64 = 256_000;
const OUTPUT_TOKEN_MIN: i64 = 1_024;
const ESTIMATE_ERROR: f64 = 0.15;
const WINDOW_HEADROOM: f64 = 0.01;

pub struct AppInfo {
    pub name: String,
    pub channel: String,
    pub version: String,
}

pub struct InputTokens {
    pub measured: i64,
    pub estimated: i64,
}

pub fn output_limit(limit: &ModelInfoLimit, input: Option<&InputTokens>, headroom: f64) -> u64 {
    let requested = if limit.output > 0.0 { limit.output as u64 } else { OUTPUT_TOKEN_FALLBACK }.min(OUTPUT_TOKEN_MAX);
    let Some(input) = input else { return requested };
    if limit.context <= 0.0 {
        return requested;
    }
    let room = (limit.context * (1.0 - headroom)).floor() as i64
        - input.measured
        - (input.estimated as f64 * (1.0 + ESTIMATE_ERROR)).ceil() as i64;
    requested.min(OUTPUT_TOKEN_MIN.max(room) as u64)
}

pub fn prepare(loaded: &Loaded, app: &AppInfo) -> LlmRequest {
    let provider_metadata_key =
        loaded.runtime.route.provider_metadata_key.as_deref().unwrap_or(loaded.runtime.provider.as_str());
    let names = loaded.snapshot.definitions.iter().map(|definition| definition.name.clone()).collect::<Vec<_>>();
    let first =
        loaded.agent.system.clone().filter(|system| !system.is_empty()).unwrap_or_else(|| system_prompt(&names));
    let instructions = crate::instructions::join(&loaded.initial);
    let mut system = Vec::new();
    if !first.is_empty() {
        system.push(SystemPart::Text { text: first, cache: None, metadata: None });
    }
    if !instructions.is_empty() {
        let rendered =
            Value::Array(loaded.initial.iter().map(|item| json!({ "key": &item.key, "text": &item.text })).collect());
        let metadata = json!({ "experimental": { "instructions": rendered } }).as_object().cloned().unwrap();
        system.push(SystemPart::Text { text: instructions, cache: None, metadata: Some(metadata) });
    }
    system.insert(
        system.len().min(1),
        SystemPart::Text { text: identity_part(&loaded.model), cache: None, metadata: None },
    );
    let messages = crate::to_llm_message::to_llm_messages(&loaded.messages, &loaded.model_ref(), provider_metadata_key);
    let affinity = session_affinity(loaded);
    let tools = loaded
        .snapshot
        .definitions
        .iter()
        .map(|definition| {
            let mut definition = definition.clone();
            if definition.freeform.as_ref().is_some_and(|freeform| !supports_freeform(&loaded.model, freeform)) {
                definition.freeform = None;
            }
            ToolEntry::Tool(definition)
        })
        .collect();
    LlmRequest {
        system,
        messages,
        tools,
        generation: GenerationOptions {
            max_tokens: Some(serde_json::Number::from(output_limit(&loaded.model.limit, None, WINDOW_HEADROOM))),
            ..Default::default()
        },
        cache: cache_policy(&loaded.model),
        prompt_cache_key: Some(prompt_cache_key(affinity)),
        headers: Some(headers(loaded, app, affinity)),
        ..Default::default()
    }
}

fn supports_freeform(model: &ModelInfo, freeform: &Freeform) -> bool {
    let Some(formats) = &model.capabilities.freeform else { return false };
    formats.iter().any(|format| {
        matches!(
            (freeform, format),
            (Freeform::Text, ToolFreeformFormat::Text)
                | (Freeform::Regex { .. }, ToolFreeformFormat::Regex)
                | (Freeform::Lark { .. }, ToolFreeformFormat::Lark)
        )
    })
}

fn system_prompt(tools: &[String]) -> String {
    let mut guidance = Vec::new();
    if tools.iter().any(|tool| tool == "shell") {
        guidance.push(
            "- Prefer dedicated tools over shell commands; fall back to the shell when a tool cannot do what you need.",
        );
        guidance.push(
            "- Do not chain shell commands with separators like `echo \"====\";` or `printf '---'`; the output becomes \
             noisy in a way that makes the user's side of the conversation worse.",
        );
    }
    if tools.iter().any(|tool| tool == "write") {
        guidance.push(
            "- Use the write tool to create files or completely replace their content. Prefer using the edit tool for \
             targeted changes.",
        );
    }
    if tools.iter().any(|tool| tool == "edit") {
        guidance.push(
            "- Use the edit tool for targeted changes to existing text files. It replaces the exact text in `oldString` \
             with `newString`, and the values must differ. By default, `oldString` must occur exactly once. If it \
             occurs multiple times, include more surrounding context to make it unique or set `replaceAll` to true to \
             replace every occurrence.",
        );
    }
    include_str!("prompt/system.txt").replace("${OPENCODE_TOOL_GUIDANCE}", &guidance.join("\n"))
}

fn identity_part(model: &ModelInfo) -> String {
    format!("# Your Model\n- Name: {}\n- Provider ID: {}\n- Model ID: {}", model.name, model.provider_id.0, model.id)
}

fn session_affinity(loaded: &Loaded) -> &str {
    loaded
        .session
        .parent_id
        .as_ref()
        .map(|parent| parent.0.as_str())
        .or_else(|| loaded.session.fork.as_ref().map(|fork| fork.session_id.0.as_str()))
        .unwrap_or(loaded.session.id.0.as_str())
}

fn prompt_cache_key(affinity: &str) -> String {
    let Some(rest) = affinity.strip_prefix("ses_") else { return affinity.to_string() };
    let hex = rest.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    if rest.len() != 64 || !hex {
        return affinity.to_string();
    }
    rest.to_string()
}

fn cache_policy(model: &ModelInfo) -> Option<CachePolicy> {
    let settings = model.settings.as_ref()?;
    let ttl = settings.cache.as_ref()?.ttl.as_ref()?;
    Some(CachePolicy::Object(CachePolicyObject { ttl: Some(ttl.0.clone()), ..Default::default() }))
}

fn headers(loaded: &Loaded, app: &AppInfo, affinity: &str) -> StringMap {
    let parent = loaded.session.parent_id.as_ref().map(|parent| parent.0.as_str());
    let mut pairs = vec![("x-opencode-session-id".to_string(), loaded.session.id.0.clone())];
    if let Some(parent) = parent {
        pairs.push(("x-opencode-parent-session-id".to_string(), parent.to_string()));
    }
    pairs.push(("x-session-affinity".to_string(), affinity.to_string()));
    pairs.push(("X-Session-Id".to_string(), affinity.to_string()));
    if let Some(parent) = parent {
        pairs.push(("x-parent-session-id".to_string(), parent.to_string()));
    }
    pairs.push(("User-Agent".to_string(), format!("opencode/{}/{}/{}", app.channel, app.version, app.name)));
    pairs.push(("x-opencode-project".to_string(), loaded.session.project_id.0.clone()));
    pairs.push(("x-opencode-session".to_string(), affinity.to_string()));
    pairs.push(("x-opencode-client".to_string(), app.name.clone()));
    StringMap::from_pairs(pairs)
}
