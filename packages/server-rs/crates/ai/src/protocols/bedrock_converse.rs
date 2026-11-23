use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use opencode_api_types::js::encode_component;
use regex_lite::Regex;
use serde_json::{Map, Number, Value};

use crate::ExecutionContext;
use crate::protocols::bedrock_event_stream;
use crate::protocols::shared;
use crate::protocols::utils::bedrock_cache::{self, Breakpoints};
use crate::protocols::utils::bedrock_media;
use crate::protocols::utils::claude_model;
use crate::protocols::utils::lifecycle;
use crate::protocols::utils::mistral_tool_id;
use crate::protocols::utils::tool_stream;
use crate::provider_error::{ProviderFailure, classify_provider_failure};
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Frame, Protocol, ProtocolState};
use crate::schema::{
    AiError, CacheHint, ContentPart, FinishReason, FinishReasonDetails, LlmEvent, LlmRequest, Message,
    ProviderMetadata, ReasoningPart, SystemPart, ToolCallPart, ToolChoice, ToolChoiceType, ToolContent, ToolDefinition,
    ToolResultPart, ToolResultValue, Usage,
};

pub const ID: &str = "bedrock-converse";

const ROUTE: &str = "bedrock-converse";
const MIN_THINKING_BUDGET: i64 = 1_024;

static NOVA2: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bamazon\.nova-2-").expect("nova 2 pattern compiles"));

pub struct BedrockConverse;

#[derive(Debug, Clone)]
enum Thinking {
    Enabled { budget_tokens: Number },
    Adaptive { display: Option<String> },
    Disabled,
}

#[derive(Debug, Clone)]
struct OutputFormat {
    r#type: String,
    schema: Map<String, Value>,
    name: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Clone)]
struct Guardrail {
    identifier: String,
    version: String,
    trace: Option<String>,
    stream_processing_mode: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct Options {
    thinking: Option<Thinking>,
    effort: Option<String>,
    service_tier: Option<String>,
    output_format: Option<OutputFormat>,
    guardrail: Option<Guardrail>,
    request_metadata: Option<Map<String, Value>>,
}

pub fn from_request(request: &LlmRequest, model: &Model) -> Result<Value, AiError> {
    let tool_choice = request.tool_choice.as_ref().map(lower_tool_choice).transpose()?.flatten();
    let (tools, flattened) = shared::flatten_tool_request(request);
    let options = decode_options(request.provider_options.as_ref());
    let max_tokens = if is_nova2(model)
        && (options.effort.as_deref() == Some("high")
            || is_high_reasoning_effort(request.http.as_ref().and_then(|http| http.body.as_ref())))
    {
        None
    } else {
        request.generation.max_tokens.clone()
    };
    let thinking = apply_thinking_binding_default(request, model, build_thinking(&options, max_tokens.as_ref(), model));
    let mut additional = Map::new();
    if let Some(top_k) = request.generation.top_k.clone() {
        additional.insert("top_k".to_string(), Value::Number(top_k));
    }
    if let Some(thinking) = thinking.as_ref() {
        additional.insert("thinking".to_string(), thinking.clone());
    }
    if let Some(effort) = options.effort.as_deref() {
        additional.extend(effort_fields(model, effort));
    }
    if thinking.as_ref().is_some_and(|thinking| thinking.get("block_binding").is_some()) {
        additional.insert(
            "anthropic_beta".to_string(),
            Value::Array(vec![Value::String(claude_model::THINKING_BINDING_BETA.to_string())]),
        );
    }
    let mut breakpoints = bedrock_cache::breakpoints(model);
    let tool_config = if tools.is_empty() {
        None
    } else {
        let mut config = Map::new();
        config.insert("tools".to_string(), Value::Array(lower_tools(&mut breakpoints, &tools)));
        if let Some(choice) = tool_choice {
            config.insert("toolChoice".to_string(), choice);
        }
        Some(Value::Object(config))
    };
    let system = lower_system(&mut breakpoints, &request.system);
    let messages = lower_messages(&flattened, model, &mut breakpoints)?;
    let inference_config = inference_config(request, max_tokens.as_ref());
    let mut body = Map::new();
    body.insert("modelId".to_string(), Value::String(model.id.clone()));
    body.insert("messages".to_string(), Value::Array(messages));
    if let Some(system) = system {
        body.insert("system".to_string(), Value::Array(system));
    }
    if let Some(inference_config) = inference_config {
        body.insert("inferenceConfig".to_string(), inference_config);
    }
    if let Some(tool_config) = tool_config {
        body.insert("toolConfig".to_string(), tool_config);
    }
    if !additional.is_empty() {
        body.insert("additionalModelRequestFields".to_string(), Value::Object(additional));
    }
    if let Some(service_tier) = options.service_tier.as_deref() {
        body.insert(
            "serviceTier".to_string(),
            Value::Object(single_entry("type", Value::String(service_tier.to_string()))),
        );
    }
    if let Some(guardrail) = options.guardrail.as_ref() {
        let mut config = Map::new();
        config.insert("guardrailIdentifier".to_string(), Value::String(guardrail.identifier.clone()));
        config.insert("guardrailVersion".to_string(), Value::String(guardrail.version.clone()));
        if let Some(trace) = guardrail.trace.as_ref() {
            config.insert("trace".to_string(), Value::String(trace.clone()));
        }
        if let Some(mode) = guardrail.stream_processing_mode.as_ref() {
            config.insert("streamProcessingMode".to_string(), Value::String(mode.clone()));
        }
        body.insert("guardrailConfig".to_string(), Value::Object(config));
    }
    if let Some(format) = options.output_format.as_ref() {
        let mut json_schema = Map::new();
        json_schema.insert(
            "schema".to_string(),
            Value::String(crate::json::encode_body(&Value::Object(format.schema.clone()))),
        );
        if let Some(name) = format.name.as_ref() {
            json_schema.insert("name".to_string(), Value::String(name.clone()));
        }
        if let Some(description) = format.description.as_ref() {
            json_schema.insert("description".to_string(), Value::String(description.clone()));
        }
        let mut structure = Map::new();
        structure.insert("jsonSchema".to_string(), Value::Object(json_schema));
        let mut text_format = Map::new();
        text_format.insert("type".to_string(), Value::String(format.r#type.clone()));
        text_format.insert("structure".to_string(), Value::Object(structure));
        body.insert("outputConfig".to_string(), Value::Object(single_entry("textFormat", Value::Object(text_format))));
    }
    if let Some(metadata) = options.request_metadata.as_ref() {
        body.insert("requestMetadata".to_string(), Value::Object(metadata.clone()));
    }
    Ok(Value::Object(body))
}

fn build_thinking(options: &Options, max_tokens: Option<&Number>, model: &Model) -> Option<Value> {
    match options.thinking.as_ref() {
        Some(Thinking::Enabled { budget_tokens }) => {
            let mut thinking = Map::new();
            thinking.insert("type".to_string(), Value::String("enabled".to_string()));
            thinking.insert(
                "budget_tokens".to_string(),
                Value::Number(shared::fit_thinking_budget(budget_tokens, max_tokens, MIN_THINKING_BUDGET)),
            );
            Some(Value::Object(thinking))
        }
        Some(Thinking::Adaptive { display }) => {
            let mut thinking = Map::new();
            thinking.insert("type".to_string(), Value::String("adaptive".to_string()));
            if let Some(display) = display.as_ref() {
                thinking.insert("display".to_string(), Value::String(display.clone()));
            }
            Some(Value::Object(thinking))
        }
        Some(Thinking::Disabled) => Some(Value::Object(single_entry("type", Value::String("disabled".to_string())))),
        None if options.effort.is_some() && supports_adaptive_thinking(model) => {
            Some(Value::Object(single_entry("type", Value::String("adaptive".to_string()))))
        }
        None => None,
    }
}

fn supports_adaptive_thinking(model: &Model) -> bool {
    let Some(version) = claude_model::claude_version(&model.id) else {
        return false;
    };
    version.major > 4 || (version.major == 4 && version.minor >= 6)
}

fn apply_thinking_binding_default(request: &LlmRequest, model: &Model, thinking: Option<Value>) -> Option<Value> {
    if thinking.as_ref().and_then(|thinking| thinking.get("type")).and_then(Value::as_str) == Some("disabled") {
        return thinking;
    }
    if is_thinking_disabled(request.http.as_ref().and_then(|http| http.body.as_ref())) {
        return thinking;
    }
    if !claude_model::supports_thinking_block_binding(model) {
        return thinking;
    }
    let mut thinking = match thinking {
        Some(Value::Object(thinking)) => thinking,
        _ => single_entry("type", Value::String("adaptive".to_string())),
    };
    thinking.insert(
        "block_binding".to_string(),
        Value::Object(single_entry("prefix_mismatch_behavior", Value::String("drop_block".to_string()))),
    );
    Some(Value::Object(thinking))
}

fn is_thinking_disabled(body: Option<&Map<String, Value>>) -> bool {
    body.and_then(|body| body.get("additionalModelRequestFields"))
        .and_then(|fields| fields.get("thinking"))
        .and_then(|thinking| thinking.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "disabled" || kind == "between_tools")
}

fn is_high_reasoning_effort(body: Option<&Map<String, Value>>) -> bool {
    body.and_then(|body| body.get("additionalModelRequestFields"))
        .and_then(|fields| fields.get("reasoningConfig"))
        .and_then(|config| config.get("maxReasoningEffort"))
        .and_then(Value::as_str)
        == Some("high")
}

fn is_nova2(model: &Model) -> bool {
    NOVA2.is_match(&model.id)
}

fn effort_fields(model: &Model, effort: &str) -> Map<String, Value> {
    if model.id.contains("anthropic") {
        return single_entry("output_config", Value::Object(single_entry("effort", Value::String(effort.to_string()))));
    }
    if model.id.contains("openai.gpt-oss") {
        return single_entry("reasoning_effort", Value::String(effort.to_string()));
    }
    if model.id.contains("openai.") || model.id.contains("xai.") {
        return single_entry("reasoning", Value::Object(single_entry("effort", Value::String(effort.to_string()))));
    }
    let mut config = Map::new();
    config.insert("type".to_string(), Value::String("enabled".to_string()));
    config.insert("maxReasoningEffort".to_string(), Value::String(effort.to_string()));
    single_entry("reasoningConfig", Value::Object(config))
}

fn lower_tool_choice(choice: &ToolChoice) -> Result<Option<Value>, AiError> {
    let mut encoded = Map::new();
    match choice.r#type {
        ToolChoiceType::Auto => {
            encoded.insert("auto".to_string(), Value::Object(Map::new()));
        }
        ToolChoiceType::None => return Ok(None),
        ToolChoiceType::Required => {
            encoded.insert("any".to_string(), Value::Object(Map::new()));
        }
        ToolChoiceType::Tool => {
            let name = choice
                .name
                .as_deref()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| shared::invalid_request("Bedrock Converse tool choice requires a tool name"))?;
            encoded.insert("tool".to_string(), Value::Object(single_entry("name", Value::String(name.to_string()))));
        }
    }
    Ok(Some(Value::Object(encoded)))
}

fn lower_tools(breakpoints: &mut Breakpoints, tools: &[ToolDefinition]) -> Vec<Value> {
    let mut lowered = Vec::new();
    for tool in tools {
        lowered.push(lower_tool_spec(tool));
        if let Some(cache_point) = bedrock_cache::block(breakpoints, tool.cache.as_ref()) {
            lowered.push(cache_point);
        }
    }
    lowered
}

fn lower_tool_spec(tool: &ToolDefinition) -> Value {
    let mut spec = Map::new();
    spec.insert("name".to_string(), Value::String(tool.name.clone()));
    if !tool.description.trim().is_empty() {
        spec.insert("description".to_string(), Value::String(tool.description.clone()));
    }
    spec.insert("inputSchema".to_string(), Value::Object(single_entry("json", tool.input_schema.clone())));
    Value::Object(single_entry("toolSpec", Value::Object(spec)))
}

fn text_with_cache(breakpoints: &mut Breakpoints, text: &str, cache: Option<&CacheHint>) -> Vec<Value> {
    match bedrock_cache::block(breakpoints, cache) {
        Some(cache_point) => vec![text_block(text.to_string()), cache_point],
        None => vec![text_block(text.to_string())],
    }
}

fn lower_system(breakpoints: &mut Breakpoints, system: &[SystemPart]) -> Option<Vec<Value>> {
    let content = system
        .iter()
        .flat_map(|part| match part {
            SystemPart::Text { text, cache, .. } => {
                if text.is_empty() {
                    Vec::new()
                } else {
                    text_with_cache(breakpoints, text, cache.as_ref())
                }
            }
        })
        .collect::<Vec<Value>>();
    (!content.is_empty()).then_some(content)
}

fn inference_config(request: &LlmRequest, max_tokens: Option<&Number>) -> Option<Value> {
    let stop = request.generation.stop.as_ref();
    if max_tokens.is_none()
        && request.generation.temperature.is_none()
        && request.generation.top_p.is_none()
        && stop.is_none_or(|stop| stop.is_empty())
    {
        return None;
    }
    let mut config = Map::new();
    if let Some(max_tokens) = max_tokens {
        config.insert("maxTokens".to_string(), Value::Number(max_tokens.clone()));
    }
    if let Some(temperature) = request.generation.temperature.as_ref() {
        config.insert("temperature".to_string(), Value::Number(temperature.clone()));
    }
    if let Some(top_p) = request.generation.top_p.as_ref() {
        config.insert("topP".to_string(), Value::Number(top_p.clone()));
    }
    if let Some(stop) = stop {
        config.insert("stopSequences".to_string(), Value::Array(stop.iter().cloned().map(Value::String).collect()));
    }
    Some(Value::Object(config))
}

struct Lowering {
    messages: Vec<Value>,
    pending_images: Vec<Value>,
    document_names: HashSet<String>,
    provider_metadata_key: String,
    hoist_images: bool,
}

fn lower_messages(request: &LlmRequest, model: &Model, breakpoints: &mut Breakpoints) -> Result<Vec<Value>, AiError> {
    let hoist_images = !keep_tool_images_inline(&model.id);
    let provider_metadata_key = model.route.provider_metadata_key.clone().unwrap_or_else(|| model.provider.clone());
    let mut normalize_id: Box<dyn FnMut(&str) -> String + '_> = if model.id.contains("mistral.") {
        mistral_tool_id::normalizer(request)
    } else {
        Box::new(|id: &str| id.to_string())
    };
    let mut lowering = Lowering {
        messages: Vec::new(),
        pending_images: Vec::new(),
        document_names: HashSet::new(),
        provider_metadata_key,
        hoist_images,
    };
    for message in &request.messages {
        if !matches!(message, Message::Tool(_)) {
            lowering.flush_images();
        }
        match message {
            Message::System(_) => lowering.push_system(message, breakpoints)?,
            Message::User(user) => lowering.push_user(&user.content, breakpoints)?,
            Message::Assistant(assistant) => {
                lowering.push_assistant(&assistant.content, breakpoints, &mut *normalize_id)?
            }
            Message::Tool(tool) => lowering.push_tool(&tool.content, breakpoints, &mut *normalize_id)?,
        }
    }
    lowering.flush_images();
    Ok(lowering.messages)
}

fn keep_tool_images_inline(model_id: &str) -> bool {
    model_id.contains("anthropic.claude-") || model_id.contains("amazon.nova-")
}

impl Lowering {
    fn flush_images(&mut self) {
        if self.pending_images.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending_images);
        let previous = self.messages.last_mut().filter(|previous| is_user_message(previous));
        if let Some(content) = previous.and_then(|previous| previous.get_mut("content")).and_then(Value::as_array_mut) {
            content.extend(pending);
        }
    }

    fn append_user(&mut self, content: Vec<Value>) {
        let previous = self.messages.last_mut().filter(|previous| is_user_message(previous));
        if let Some(existing) = previous.and_then(|previous| previous.get_mut("content")).and_then(Value::as_array_mut)
        {
            existing.extend(content);
            return;
        }
        self.messages.push(user_message(content));
    }

    fn push_system(&mut self, message: &Message, breakpoints: &mut Breakpoints) -> Result<(), AiError> {
        let part = shared::wrapped_system_update("Bedrock Converse", message)?;
        let content = text_with_cache(breakpoints, &part.text, part.cache.as_ref());
        self.append_user(content);
        Ok(())
    }

    fn push_user(&mut self, content: &[ContentPart], breakpoints: &mut Breakpoints) -> Result<(), AiError> {
        let mut blocks = Vec::new();
        for part in content {
            match part {
                ContentPart::Text(part) => {
                    blocks.extend(text_with_cache(breakpoints, &part.text, part.cache.as_ref()));
                }
                ContentPart::Media(part) => {
                    blocks.extend(bedrock_media::lower(part, &mut self.document_names)?);
                }
                _ => {}
            }
        }
        self.append_user(blocks);
        Ok(())
    }

    fn push_assistant(
        &mut self,
        content: &[ContentPart],
        breakpoints: &mut Breakpoints,
        normalize_id: &mut dyn FnMut(&str) -> String,
    ) -> Result<(), AiError> {
        let mut blocks = Vec::new();
        for part in content {
            match part {
                ContentPart::Text(part) => {
                    blocks.extend(text_with_cache(breakpoints, &part.text, part.cache.as_ref()));
                }
                ContentPart::Reasoning(part) => {
                    let signature = reasoning_signature(part, &self.provider_metadata_key);
                    let redacted = reasoning_redacted_data(part, &self.provider_metadata_key);
                    if signature.is_none()
                        && let Some(redacted) = redacted
                    {
                        let mut block = Map::new();
                        block.insert(
                            "reasoningContent".to_string(),
                            Value::Object(single_entry("redactedContent", Value::String(redacted))),
                        );
                        blocks.push(Value::Object(block));
                        continue;
                    }
                    let Some(signature) = signature.filter(|signature| !signature.trim().is_empty()) else {
                        continue;
                    };
                    let mut reasoning_text = Map::new();
                    reasoning_text.insert("text".to_string(), Value::String(part.text.clone()));
                    reasoning_text.insert("signature".to_string(), Value::String(signature));
                    let mut block = Map::new();
                    block.insert(
                        "reasoningContent".to_string(),
                        Value::Object(single_entry("reasoningText", Value::Object(reasoning_text))),
                    );
                    blocks.push(Value::Object(block));
                }
                ContentPart::ToolCall(part) => {
                    blocks.push(lower_tool_call(part, normalize_id));
                }
                _ => {
                    return Err(shared::invalid_request(
                        "Bedrock Converse assistant messages only support text, reasoning, and tool-call content for now",
                    ));
                }
            }
        }
        if blocks.is_empty() {
            return Ok(());
        }
        let mut message = Map::new();
        message.insert("role".to_string(), Value::String("assistant".to_string()));
        message.insert("content".to_string(), Value::Array(blocks));
        self.messages.push(Value::Object(message));
        Ok(())
    }

    fn push_tool(
        &mut self,
        content: &[ContentPart],
        breakpoints: &mut Breakpoints,
        normalize_id: &mut dyn FnMut(&str) -> String,
    ) -> Result<(), AiError> {
        let mut blocks = Vec::new();
        for part in content {
            let ContentPart::ToolResult(part) = part else {
                continue;
            };
            let mut tool_result = lower_tool_result(part, &mut self.document_names, normalize_id)?;
            if self.hoist_images {
                let items = tool_result.get("content").and_then(Value::as_array).cloned().unwrap_or_default();
                let images = items.iter().filter(|item| item.get("image").is_some()).cloned().collect::<Vec<Value>>();
                if !images.is_empty() {
                    let non_images =
                        items.iter().filter(|item| item.get("image").is_none()).cloned().collect::<Vec<Value>>();
                    let content = if non_images.is_empty() {
                        vec![text_block("See attached image.".to_string())]
                    } else {
                        non_images
                    };
                    tool_result.insert("content".to_string(), Value::Array(content));
                    self.pending_images.extend(images);
                }
            }
            blocks.push(Value::Object(single_entry("toolResult", Value::Object(tool_result))));
            if let Some(cache_point) = bedrock_cache::block(breakpoints, part.cache.as_ref()) {
                blocks.push(cache_point);
            }
        }
        self.append_user(blocks);
        Ok(())
    }
}

fn reasoning_signature(part: &ReasoningPart, provider_metadata_key: &str) -> Option<String> {
    if let Some(encrypted) = part.encrypted.as_ref() {
        return Some(encrypted.clone());
    }
    let metadata = part.provider_metadata.as_ref()?.get(provider_metadata_key)?;
    metadata.get("signature")?.as_str().map(str::to_string)
}

fn reasoning_redacted_data(part: &ReasoningPart, provider_metadata_key: &str) -> Option<String> {
    let metadata = part.provider_metadata.as_ref()?.get(provider_metadata_key)?;
    metadata.get("redactedData")?.as_str().map(str::to_string)
}

fn remove_empty_object_keys(input: &Value) -> Value {
    match input {
        Value::Array(items) => Value::Array(items.iter().map(remove_empty_object_keys).collect()),
        Value::Object(entries) => {
            let cleaned = entries
                .iter()
                .filter(|(key, _)| !key.is_empty())
                .map(|(key, value)| (key.clone(), remove_empty_object_keys(value)))
                .collect::<Map<String, Value>>();
            Value::Object(cleaned)
        }
        value => value.clone(),
    }
}

fn lower_tool_call(part: &ToolCallPart, normalize_id: &mut dyn FnMut(&str) -> String) -> Value {
    let mut tool_use = Map::new();
    tool_use.insert("toolUseId".to_string(), Value::String(normalize_id(&part.id)));
    tool_use.insert("name".to_string(), Value::String(part.name.chars().take(64).collect()));
    tool_use.insert("input".to_string(), remove_empty_object_keys(&part.input));
    Value::Object(single_entry("toolUse", Value::Object(tool_use)))
}

fn lower_tool_result(
    part: &ToolResultPart,
    document_names: &mut HashSet<String>,
    normalize_id: &mut dyn FnMut(&str) -> String,
) -> Result<Map<String, Value>, AiError> {
    let mut tool_result = Map::new();
    tool_result.insert("toolUseId".to_string(), Value::String(normalize_id(&part.id)));
    tool_result.insert("content".to_string(), Value::Array(lower_tool_result_content(part, document_names)?));
    let status = if matches!(part.result, ToolResultValue::Error { .. }) { "error" } else { "success" };
    tool_result.insert("status".to_string(), Value::String(status.to_string()));
    Ok(tool_result)
}

fn lower_tool_result_content(
    part: &ToolResultPart,
    document_names: &mut HashSet<String>,
) -> Result<Vec<Value>, AiError> {
    match &part.result {
        ToolResultValue::Text { .. } | ToolResultValue::Error { .. } => {
            Ok(vec![text_block(shared::tool_result_text(part))])
        }
        ToolResultValue::Json { value } => Ok(vec![Value::Object(single_entry("json", value.clone()))]),
        ToolResultValue::Content { value } => {
            let mut blocks = Vec::new();
            for item in value {
                if let ToolContent::Text { text } = item {
                    blocks.push(text_block(text.clone()));
                    continue;
                }
                let Some(media) = shared::tool_file_media(item) else {
                    continue;
                };
                blocks.extend(bedrock_media::lower(&media, document_names)?);
            }
            Ok(blocks)
        }
    }
}

fn text_block(text: String) -> Value {
    Value::Object(single_entry("text", Value::String(text)))
}

fn user_message(content: Vec<Value>) -> Value {
    let mut message = Map::new();
    message.insert("role".to_string(), Value::String("user".to_string()));
    message.insert("content".to_string(), Value::Array(content));
    Value::Object(message)
}

fn is_user_message(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("user")
}

fn single_entry(key: &str, value: Value) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert(key.to_string(), value);
    map
}

fn provider_metadata(provider_metadata_key: &str, metadata: Map<String, Value>) -> ProviderMetadata {
    let mut provider_metadata = ProviderMetadata::new();
    provider_metadata.insert(provider_metadata_key.to_string(), Value::Object(metadata));
    provider_metadata
}

fn decode_options(input: Option<&Map<String, Value>>) -> Options {
    let Some(input) = input else {
        return Options::default();
    };
    Options {
        thinking: input.get("thinking").and_then(decode_thinking),
        effort: input.get("effort").and_then(Value::as_str).map(str::to_string),
        service_tier: input.get("serviceTier").and_then(Value::as_str).map(str::to_string),
        output_format: input.get("outputFormat").and_then(decode_output_format),
        guardrail: input.get("guardrail").and_then(decode_guardrail),
        request_metadata: input.get("requestMetadata").and_then(decode_request_metadata),
    }
}

fn decode_thinking(value: &Value) -> Option<Thinking> {
    let object = value.as_object()?;
    match object.get("type")?.as_str()? {
        "enabled" => {
            let budget_tokens = object.get("budgetTokens")?.as_number()?.clone();
            Some(Thinking::Enabled { budget_tokens })
        }
        "adaptive" => {
            Some(Thinking::Adaptive { display: object.get("display").and_then(Value::as_str).map(str::to_string) })
        }
        "disabled" => Some(Thinking::Disabled),
        _ => None,
    }
}

fn decode_output_format(value: &Value) -> Option<OutputFormat> {
    let object = value.as_object()?;
    let r#type = object.get("type")?.as_str()?.to_string();
    let schema = object.get("schema")?.as_object()?.clone();
    Some(OutputFormat {
        r#type,
        schema,
        name: object.get("name").and_then(Value::as_str).map(str::to_string),
        description: object.get("description").and_then(Value::as_str).map(str::to_string),
    })
}

fn decode_guardrail(value: &Value) -> Option<Guardrail> {
    let object = value.as_object()?;
    Some(Guardrail {
        identifier: object.get("identifier")?.as_str()?.to_string(),
        version: object.get("version")?.as_str()?.to_string(),
        trace: object.get("trace").and_then(Value::as_str).map(str::to_string),
        stream_processing_mode: object.get("streamProcessingMode").and_then(Value::as_str).map(str::to_string),
    })
}

fn decode_request_metadata(value: &Value) -> Option<Map<String, Value>> {
    let object = value.as_object()?;
    if object.values().any(|value| !value.is_string()) {
        return None;
    }
    Some(object.clone())
}

fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "max_tokens" | "model_context_window_exceeded" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCalls,
        "content_filtered" | "guardrail_intervened" => FinishReason::ContentFilter,
        _ => FinishReason::Unknown,
    }
}

fn map_usage(usage: &Value, provider_metadata_key: &str) -> Option<Usage> {
    let object = usage.as_object()?;
    let token = |field: &str| object.get(field).and_then(Value::as_u64);
    let input_tokens_raw = token("inputTokens");
    let output_tokens = token("outputTokens");
    let total = token("totalTokens");
    let cache_read_input_tokens = token("cacheReadInputTokens");
    let cache_write_input_tokens = token("cacheWriteInputTokens");
    let input_tokens = shared::sum_tokens(&[input_tokens_raw, cache_read_input_tokens, cache_write_input_tokens]);
    let mut metadata = Map::new();
    for (key, value) in [
        ("inputTokens", input_tokens_raw),
        ("outputTokens", output_tokens),
        ("totalTokens", total),
        ("cacheReadInputTokens", cache_read_input_tokens),
        ("cacheWriteInputTokens", cache_write_input_tokens),
    ] {
        if let Some(value) = value {
            metadata.insert(key.to_string(), Value::Number(Number::from(value)));
        }
    }
    Some(Usage {
        context_tokens: None,
        input_tokens,
        output_tokens,
        non_cached_input_tokens: input_tokens_raw,
        cache_read_input_tokens,
        cache_write_input_tokens,
        reasoning_tokens: None,
        total_tokens: shared::total_tokens(input_tokens, output_tokens, total),
        cost: None,
        provider_metadata: Some(provider_metadata(provider_metadata_key, metadata)),
    })
}

fn encode_redacted_content(chunks: &[Vec<u8>]) -> String {
    BASE64_STANDARD.encode(chunks.concat())
}

fn invalid_event(event: &Value) -> AiError {
    shared::event_error(ROUTE, "Invalid bedrock/bedrock-converse stream event", Some(crate::json::encode_body(event)))
}

struct State {
    provider_metadata_key: String,
    tools: tool_stream::State,
    finish_reason: Option<FinishReasonDetails>,
    usage: Option<Usage>,
    has_tool_calls: bool,
    lifecycle: lifecycle::State,
    reasoning_signatures: BTreeMap<i64, String>,
    reasoning_redacted_content: BTreeMap<i64, Vec<Vec<u8>>>,
}

impl State {
    fn reasoning_metadata(&self, index: i64) -> Option<ProviderMetadata> {
        if let Some(signature) = self.reasoning_signatures.get(&index).filter(|signature| !signature.is_empty()) {
            return Some(provider_metadata(
                &self.provider_metadata_key,
                single_entry("signature", Value::String(signature.clone())),
            ));
        }
        let chunks = self.reasoning_redacted_content.get(&index)?;
        Some(provider_metadata(
            &self.provider_metadata_key,
            single_entry("redactedData", Value::String(encode_redacted_content(chunks))),
        ))
    }
}

impl ProtocolState for State {
    fn step(&mut self, frame: Frame) -> Result<Vec<LlmEvent>, AiError> {
        let event: Value = match serde_json::from_str(&frame) {
            Ok(event) => event,
            Err(_) => {
                return Err(shared::event_error(ROUTE, "Invalid bedrock/bedrock-converse stream event", Some(frame)));
            }
        };
        let Some(object) = event.as_object() else {
            return Err(invalid_event(&event));
        };
        let content_block_delta = object.get("contentBlockDelta").and_then(Value::as_object);

        if let Some(start) = object.get("contentBlockStart").and_then(Value::as_object)
            && let Some(tool_use) = start.get("start").and_then(|start| start.get("toolUse")).and_then(Value::as_object)
        {
            let Some(index) = start.get("contentBlockIndex").and_then(Value::as_i64) else {
                return Err(invalid_event(&event));
            };
            let Some(id) = tool_use.get("toolUseId").and_then(Value::as_str) else {
                return Err(invalid_event(&event));
            };
            let Some(name) = tool_use.get("name").and_then(Value::as_str) else {
                return Err(invalid_event(&event));
            };
            let mut events = Vec::new();
            self.lifecycle = lifecycle::step_start(self.lifecycle.clone(), &mut events);
            tool_stream::start(
                &mut self.tools,
                index,
                tool_stream::PendingTool {
                    id: id.to_string(),
                    name: name.to_string(),
                    namespace: None,
                    input: String::new(),
                    provider_executed: None,
                    provider_metadata: None,
                },
            );
            events.push(LlmEvent::ToolInputStart {
                id: id.to_string(),
                name: name.to_string(),
                namespace: None,
                provider_executed: None,
                provider_metadata: None,
            });
            return Ok(events);
        }

        if let Some(delta) = content_block_delta.and_then(|delta| delta.get("delta")).and_then(Value::as_object)
            && let Some(text) = delta.get("text").and_then(Value::as_str).filter(|text| !text.is_empty())
        {
            let index = content_block_delta
                .and_then(|delta| delta.get("contentBlockIndex"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let mut events = Vec::new();
            self.lifecycle =
                lifecycle::text_delta(self.lifecycle.clone(), &mut events, &format!("text-{index}"), text, None);
            return Ok(events);
        }

        if let Some(delta) = content_block_delta.and_then(|delta| delta.get("delta")).and_then(Value::as_object)
            && let Some(reasoning) = delta.get("reasoningContent").and_then(Value::as_object)
        {
            let index = content_block_delta
                .and_then(|delta| delta.get("contentBlockIndex"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let redacted_chunk = match reasoning.get("redactedContent") {
                Some(Value::String(encoded)) => Some(BASE64_STANDARD.decode(encoded).map_err(|_| {
                    shared::event_error(
                        ROUTE,
                        "Bedrock Converse reasoningContent.redactedContent contains invalid base64 data",
                        None,
                    )
                })?),
                _ => None,
            };
            let mut events = Vec::new();
            if let Some(chunk) = redacted_chunk.as_ref() {
                self.reasoning_redacted_content.entry(index).or_default().push(chunk.clone());
            }
            let signature =
                reasoning.get("signature").and_then(Value::as_str).filter(|signature| !signature.is_empty());
            let metadata = if let Some(signature) = signature {
                Some(provider_metadata(
                    &self.provider_metadata_key,
                    single_entry("signature", Value::String(signature.to_string())),
                ))
            } else if redacted_chunk.is_none() {
                reasoning.get("data").and_then(Value::as_str).map(|data| {
                    provider_metadata(
                        &self.provider_metadata_key,
                        single_entry("redactedData", Value::String(data.to_string())),
                    )
                })
            } else {
                None
            };
            let text = reasoning.get("text").and_then(Value::as_str);
            if text.is_some() || metadata.is_some() {
                self.lifecycle = lifecycle::reasoning_delta(
                    self.lifecycle.clone(),
                    &mut events,
                    &format!("reasoning-{index}"),
                    text.unwrap_or_default(),
                    metadata,
                );
            } else if redacted_chunk.is_some() {
                self.lifecycle = lifecycle::reasoning_start(
                    self.lifecycle.clone(),
                    &mut events,
                    &format!("reasoning-{index}"),
                    None,
                );
            }
            if let Some(signature) = signature {
                self.reasoning_signatures.insert(index, signature.to_string());
            }
            if reasoning.get("data").is_some() && redacted_chunk.is_none() {
                self.reasoning_redacted_content.remove(&index);
            }
            return Ok(events);
        }

        if let Some(delta) = content_block_delta
            && let Some(tool_use) = delta.get("delta").and_then(|delta| delta.get("toolUse")).and_then(Value::as_object)
        {
            let index = delta.get("contentBlockIndex").and_then(Value::as_i64).unwrap_or(0);
            let input = tool_use.get("input").and_then(Value::as_str).unwrap_or_default();
            let Some(outcome) = tool_stream::append(&mut self.tools, index, input) else {
                return Ok(Vec::new());
            };
            let mut events = Vec::new();
            let lifecycle = if outcome.events.is_empty() {
                self.lifecycle.clone()
            } else {
                lifecycle::step_start(self.lifecycle.clone(), &mut events)
            };
            events.extend(outcome.events);
            self.lifecycle = lifecycle;
            return Ok(events);
        }

        if let Some(stop) = object.get("contentBlockStop").and_then(Value::as_object) {
            let index = stop.get("contentBlockIndex").and_then(Value::as_i64).unwrap_or(0);
            let outcome = tool_stream::finish(ROUTE, &mut self.tools, index, None)?;
            let result_events = outcome.events;
            let mut events = Vec::new();
            let lifecycle = if result_events.is_empty() {
                let metadata = self.reasoning_metadata(index);
                lifecycle::reasoning_end(
                    lifecycle::text_end(self.lifecycle.clone(), &mut events, &format!("text-{index}"), None, None),
                    &mut events,
                    &format!("reasoning-{index}"),
                    metadata,
                    None,
                )
            } else {
                lifecycle::step_start(self.lifecycle.clone(), &mut events)
            };
            if result_events
                .iter()
                .any(|event| matches!(event, LlmEvent::ToolCall { .. } | LlmEvent::ToolInputError { .. }))
            {
                self.has_tool_calls = true;
            }
            events.extend(result_events);
            self.lifecycle = lifecycle;
            self.reasoning_signatures.remove(&index);
            self.reasoning_redacted_content.remove(&index);
            return Ok(events);
        }

        if let Some(stop) = object.get("messageStop").and_then(Value::as_object) {
            let Some(stop_reason) = stop.get("stopReason").and_then(Value::as_str) else {
                return Err(invalid_event(&event));
            };
            if stop_reason == "malformed_model_output" || stop_reason == "malformed_tool_use" {
                let mut payload = object.clone();
                payload.remove("rawBody");
                return Err(shared::event_error(
                    ROUTE,
                    format!("Bedrock Converse stopped with {stop_reason}"),
                    Some(crate::json::encode_body(&Value::Object(payload))),
                ));
            }
            self.finish_reason = Some(FinishReasonDetails {
                normalized: map_finish_reason(stop_reason),
                raw: Some(stop_reason.to_string()),
                category: None,
                explanation: None,
            });
            return Ok(Vec::new());
        }

        if let Some(metadata) = object.get("metadata").and_then(Value::as_object) {
            if let Some(usage) = metadata.get("usage").and_then(|usage| map_usage(usage, &self.provider_metadata_key)) {
                self.usage = Some(usage);
            }
            return Ok(Vec::new());
        }

        if let Some(exception) = object.get("exception").and_then(Value::as_object) {
            let details = exception.get("details").and_then(Value::as_object);
            let message = details
                .and_then(|details| details.get("message"))
                .and_then(Value::as_str)
                .or_else(|| details.and_then(|details| details.get("originalMessage")).and_then(Value::as_str))
                .unwrap_or("Bedrock Converse stream error")
                .to_string();
            let status = details
                .and_then(|details| details.get("originalStatusCode"))
                .and_then(Value::as_u64)
                .and_then(|status| u16::try_from(status).ok());
            let mut payload = object.clone();
            payload.remove("rawBody");
            let body = crate::json::encode_body(&Value::Object(payload));
            return Err(AiError {
                reason: classify_provider_failure(ProviderFailure {
                    message,
                    status,
                    raw_body: Some(body),
                    ..ProviderFailure::default()
                }),
            });
        }

        Ok(Vec::new())
    }

    fn halt(&mut self) -> Result<Vec<LlmEvent>, AiError> {
        let Some(finish_reason) = self.finish_reason.clone() else {
            return Ok(Vec::new());
        };
        let normalized = if finish_reason.normalized == FinishReason::Stop && self.has_tool_calls {
            FinishReason::ToolCalls
        } else {
            finish_reason.normalized.clone()
        };
        let mut events = Vec::new();
        for (index, chunks) in std::mem::take(&mut self.reasoning_redacted_content) {
            let metadata = match self.reasoning_signatures.get(&index).filter(|signature| !signature.is_empty()) {
                Some(signature) => single_entry("signature", Value::String(signature.clone())),
                None => single_entry("redactedData", Value::String(encode_redacted_content(&chunks))),
            };
            self.lifecycle = lifecycle::reasoning_end(
                self.lifecycle.clone(),
                &mut events,
                &format!("reasoning-{index}"),
                Some(provider_metadata(&self.provider_metadata_key, metadata)),
                None,
            );
        }
        let reason = FinishReasonDetails { normalized, ..finish_reason };
        self.lifecycle = lifecycle::finish(self.lifecycle.clone(), &mut events, reason, self.usage.clone(), None);
        Ok(events)
    }

    fn terminal(&self, _frame: &Frame) -> bool {
        false
    }
}

impl Protocol for BedrockConverse {
    fn id(&self) -> &str {
        ID
    }

    fn build_body(
        &self,
        request: &ResolvedRequest,
        model: &Model,
        _context: &ExecutionContext,
    ) -> Result<Value, AiError> {
        from_request(&request.request, model)
    }

    fn default_path(&self, model: &Model, body: &Value) -> Option<String> {
        let model_id = body.get("modelId").and_then(Value::as_str).unwrap_or(model.id.as_str());
        Some(format!("/model/{}/converse-stream", encode_component(model_id)))
    }

    fn framing(&self) -> Definition {
        bedrock_event_stream::framing(ROUTE)
    }

    fn new_state(&self, request: &ResolvedRequest, _context: &ExecutionContext) -> Box<dyn ProtocolState> {
        let model = &request.model;
        Box::new(State {
            provider_metadata_key: model.route.provider_metadata_key.clone().unwrap_or_else(|| model.provider.clone()),
            tools: tool_stream::empty(),
            finish_reason: None,
            usage: None,
            has_tool_calls: false,
            lifecycle: lifecycle::initial(),
            reasoning_signatures: BTreeMap::new(),
            reasoning_redacted_content: BTreeMap::new(),
        })
    }
}
