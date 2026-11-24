use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Number, Value};

use crate::ExecutionContext;
use crate::protocols::shared;
use crate::protocols::utils::{lifecycle, mistral_tool_id, tool_stream};
use crate::provider_error::{ProviderFailure, classify_provider_failure};
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Frame, Protocol, ProtocolState};
use crate::schema::{
    AiError, AiErrorReason, AssistantMessage, CacheMode, CachePolicy, ContentPart, FinishReason, FinishReasonDetails,
    InvalidProviderOutputClassification, LlmEvent, LlmRequest, MediaPart, Message, ProviderMetadata, SystemPart,
    ToolChoice, ToolChoiceType, ToolContent, ToolResultValue, Usage, UserMessage,
};

pub const ID: &str = "mistral-chat";
pub const DEFAULT_BASE_URL: &str = "https://api.mistral.ai/v1";
pub const PATH: &str = "/chat/completions";

const METADATA_KEY: &str = "mistral";
const DONE: &str = "[DONE]";
const MISSING_TOOL_NAME: &str = "Mistral Chat tool call delta is missing a name";
const UNSUPPORTED_ASSISTANT: &str =
    "Mistral Chat assistant messages only support text, reasoning, and tool-call content for now";

pub struct MistralChat;

#[derive(Clone, Debug, PartialEq, Eq)]
enum ToolKey {
    Index(i64),
    Id(String),
    Position(usize),
    Missing(u64),
}

impl ToolKey {
    fn label(&self) -> String {
        match self {
            ToolKey::Index(index) => index.to_string(),
            ToolKey::Id(id) => format!("id:{id}"),
            ToolKey::Position(position) => format!("position:{position}"),
            ToolKey::Missing(generated) => format!("missing:{generated}"),
        }
    }
}

#[derive(Clone)]
struct PendingToolDelta {
    id: String,
    input: String,
}

#[derive(Clone, Copy, PartialEq)]
enum ActiveKind {
    Text,
    Reasoning,
}

struct ActiveContent {
    kind: ActiveKind,
    id: String,
    thinking: Option<Map<String, Value>>,
    thinking_units: Vec<Value>,
}

struct State {
    tools: Vec<(ToolKey, tool_stream::PendingTool)>,
    pending_tools: Vec<(ToolKey, PendingToolDelta)>,
    tool_ids: BTreeMap<String, String>,
    used_tool_ids: BTreeSet<String>,
    completed_tools: Vec<LlmEvent>,
    latest_tool_key: Option<ToolKey>,
    generated_tools: u64,
    lifecycle: lifecycle::State,
    active: Option<ActiveContent>,
    next_content: u64,
    usage: Option<Usage>,
    finish_reason: Option<FinishReasonDetails>,
}

impl State {
    fn tool(&self, key: &ToolKey) -> Option<&tool_stream::PendingTool> {
        self.tools.iter().find(|(existing, _)| existing == key).map(|(_, tool)| tool)
    }

    fn take_pending(&mut self, key: &ToolKey) -> Option<PendingToolDelta> {
        let position = self.pending_tools.iter().position(|(existing, _)| existing == key)?;
        Some(self.pending_tools.remove(position).1)
    }

    fn set_pending(&mut self, key: ToolKey, pending: PendingToolDelta) {
        match self.pending_tools.iter_mut().find(|(existing, _)| *existing == key) {
            Some(entry) => entry.1 = pending,
            None => self.pending_tools.push((key, pending)),
        }
    }

    fn ordered_tools(&self) -> Vec<&(ToolKey, tool_stream::PendingTool)> {
        let mut entries = self.tools.iter().collect::<Vec<_>>();
        entries.sort_by(|left, right| match (&left.0, &right.0) {
            (ToolKey::Index(left), ToolKey::Index(right)) => left.cmp(right),
            (ToolKey::Index(_), _) => Ordering::Less,
            (_, ToolKey::Index(_)) => Ordering::Greater,
            _ => Ordering::Equal,
        });
        entries
    }

    fn close_active(&mut self, events: &mut Vec<LlmEvent>) {
        let Some(active) = self.active.take() else {
            return;
        };
        self.lifecycle = match active.kind {
            ActiveKind::Text => lifecycle::text_end(self.lifecycle.clone(), events, &active.id, None, None),
            ActiveKind::Reasoning => {
                let mut thinking = active.thinking.unwrap_or_default();
                thinking.insert("type".to_string(), Value::String("thinking".to_string()));
                thinking.insert("thinking".to_string(), Value::Array(active.thinking_units.clone()));
                let text = thinking_text(&active.thinking_units);
                lifecycle::reasoning_end(
                    self.lifecycle.clone(),
                    events,
                    &active.id,
                    Some(thinking_metadata(Value::Object(thinking))),
                    Some(text),
                )
            }
        };
    }

    fn append_text(&mut self, events: &mut Vec<LlmEvent>, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.active.as_ref().is_some_and(|active| active.kind == ActiveKind::Text) {
            self.close_active(events);
        }
        let (id, started) = match &self.active {
            Some(active) => (active.id.clone(), false),
            None => (format!("text-{}", self.next_content), true),
        };
        self.lifecycle = lifecycle::text_delta(self.lifecycle.clone(), events, &id, text, None);
        if started {
            self.active =
                Some(ActiveContent { kind: ActiveKind::Text, id, thinking: None, thinking_units: Vec::new() });
            self.next_content += 1;
        }
    }

    fn append_thinking(&mut self, events: &mut Vec<LlmEvent>, part: &Value) {
        if !self.active.as_ref().is_some_and(|active| active.kind == ActiveKind::Reasoning) {
            self.close_active(events);
        }
        let units = thinking_units(part.get("thinking"));
        let started = self.active.is_none();
        let id = match &self.active {
            Some(active) => active.id.clone(),
            None => format!("reasoning-{}", self.next_content),
        };
        let text = thinking_text(&units);
        self.lifecycle = if text.is_empty() {
            lifecycle::reasoning_start(self.lifecycle.clone(), events, &id, None)
        } else {
            lifecycle::reasoning_delta(self.lifecycle.clone(), events, &id, &text, None)
        };
        let active = self.active.get_or_insert_with(|| ActiveContent {
            kind: ActiveKind::Reasoning,
            id,
            thinking: None,
            thinking_units: Vec::new(),
        });
        active.thinking = Some(merge_thinking(active.thinking.take(), part));
        active.thinking_units.extend(units);
        if started {
            self.next_content += 1;
        }
    }

    fn append_content(&mut self, events: &mut Vec<LlmEvent>, content: &Value) {
        match content {
            Value::String(text) => self.append_text(events, text),
            Value::Array(parts) => {
                for part in parts {
                    match part.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            self.append_text(events, part.get("text").and_then(Value::as_str).unwrap_or_default());
                        }
                        Some("thinking") => self.append_thinking(events, part),
                        _ => self.close_active(events),
                    }
                }
            }
            _ => {}
        }
    }

    fn normalize_stream_tool_id(&mut self, source: &str) -> String {
        if mistral_tool_id::valid(source) {
            self.used_tool_ids.insert(source.to_string());
            return source.to_string();
        }
        if let Some(previous) = self.tool_ids.get(source) {
            return previous.clone();
        }
        let mut attempt = 0u32;
        let mut id = mistral_tool_id::hash(source);
        while self.used_tool_ids.contains(&id) {
            attempt += 1;
            id = mistral_tool_id::hash(&format!("{source}:{attempt}"));
        }
        self.tool_ids.insert(source.to_string(), id.clone());
        self.used_tool_ids.insert(id.clone());
        id
    }

    fn append_tool_delta(&mut self, key: &ToolKey, id: &str, name: &str, text: &str) -> Result<Vec<LlmEvent>, AiError> {
        let mut tools = tool_stream::State::new();
        let position = self.tools.iter().position(|(existing, _)| existing == key);
        if let Some(position) = position {
            tools.insert(0, self.tools.remove(position).1);
        }
        let outcome = tool_stream::append_or_start(
            ID,
            &mut tools,
            0,
            tool_stream::AppendDelta { id: Some(id), name: Some(name), text, provider_metadata: None },
            MISSING_TOOL_NAME,
        )?;
        if let Some(updated) = tools.remove(&0) {
            match position {
                Some(position) => self.tools.insert(position, (key.clone(), updated)),
                None => self.tools.push((key.clone(), updated)),
            }
        }
        Ok(outcome.events)
    }

    fn finish_tools(&mut self) -> Result<Vec<LlmEvent>, AiError> {
        let ordered = self.ordered_tools().into_iter().map(|(_, tool)| tool.clone()).collect::<Vec<_>>();
        let mut tools = tool_stream::State::new();
        for (index, tool) in ordered.into_iter().enumerate() {
            tools.insert(index as i64, tool);
        }
        let finished = tool_stream::finish_all(ID, &mut tools)?;
        self.tools.clear();
        Ok(finished.events)
    }

    fn append_tools(&mut self, events: &mut Vec<LlmEvent>, deltas: &[Value]) -> Result<(), AiError> {
        if deltas.is_empty() {
            return Ok(());
        }
        self.close_active(events);
        for (position, delta) in deltas.iter().enumerate() {
            let wire_id = delta.get("id").and_then(Value::as_str).map(str::trim).filter(|id| !id.is_empty());
            let provided_id = wire_id.filter(|id| *id != "null");
            let key = match delta.get("index").and_then(Value::as_i64) {
                Some(index) => ToolKey::Index(index),
                None => match provided_id {
                    Some(id) => ToolKey::Id(id.to_string()),
                    None if deltas.len() > 1 => ToolKey::Position(position),
                    None => self.latest_tool_key.clone().unwrap_or(ToolKey::Missing(self.generated_tools)),
                },
            };
            let pending = self.take_pending(&key);
            let had_pending = pending.is_some();
            let existing = self.tool(&key).map(|tool| (tool.id.clone(), tool.name.clone()));
            let had_existing = existing.is_some();
            let id = match existing.as_ref().map(|(id, _)| id).or_else(|| pending.as_ref().map(|pending| &pending.id)) {
                Some(id) => id.clone(),
                None => {
                    let source =
                        provided_id.map(str::to_string).unwrap_or_else(|| format!("generated:{}", key.label()));
                    self.normalize_stream_tool_id(&source)
                }
            };
            let name = existing.as_ref().map(|(_, name)| name.clone()).or_else(|| {
                delta
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
            });
            let mut text = pending.map(|pending| pending.input).unwrap_or_default();
            text.push_str(&tool_text(delta));
            let Some(name) = name else {
                self.set_pending(key.clone(), PendingToolDelta { id, input: text });
                self.latest_tool_key = Some(key);
                if provided_id.is_none() && !had_pending {
                    self.generated_tools += 1;
                }
                continue;
            };
            let outcome = self.append_tool_delta(&key, &id, &name, &text)?;
            if !outcome.is_empty() {
                self.lifecycle = lifecycle::step_start(self.lifecycle.clone(), events);
            }
            events.extend(outcome);
            self.latest_tool_key = Some(key);
            if provided_id.is_none() && !had_existing && !had_pending {
                self.generated_tools += 1;
            }
        }
        Ok(())
    }

    fn finish_events(&mut self) -> Result<Vec<LlmEvent>, AiError> {
        let Some(finish_reason) = self.finish_reason.clone() else {
            return Err(AiError {
                reason: AiErrorReason::InvalidProviderOutput {
                    message: "Mistral Chat stream ended without finish_reason".to_string(),
                    body: None,
                    http: None,
                    classification: Some(InvalidProviderOutputClassification::IncompleteStream),
                    route: Some(ID.to_string()),
                },
            });
        };
        let mut events = Vec::new();
        self.close_active(&mut events);
        if !self.completed_tools.is_empty() {
            self.lifecycle = lifecycle::step_start(self.lifecycle.clone(), &mut events);
        }
        let completed_tool_call = self.completed_tools.iter().any(|event| matches!(event, LlmEvent::ToolCall { .. }));
        events.extend(std::mem::take(&mut self.completed_tools));
        let reason = if finish_reason.normalized == FinishReason::Stop && completed_tool_call {
            FinishReasonDetails { normalized: FinishReason::ToolCalls, ..finish_reason }
        } else {
            finish_reason
        };
        self.lifecycle = lifecycle::finish(self.lifecycle.clone(), &mut events, reason, self.usage.clone(), None);
        Ok(events)
    }
}

impl ProtocolState for State {
    fn step(&mut self, frame: Frame) -> Result<Vec<LlmEvent>, AiError> {
        if self.terminal(&frame) {
            return Ok(Vec::new());
        }
        let event: Value = match serde_json::from_str(&frame) {
            Ok(event) => event,
            Err(_) => return Err(shared::event_error(ID, "Invalid mistral-chat stream event", Some(frame))),
        };
        if let Some(error) = event.get("error").filter(|error| error.is_object()) {
            let message = error.get("message").and_then(Value::as_str).unwrap_or_default().to_string();
            let status = error.get("code").and_then(Value::as_u64).and_then(|code| u16::try_from(code).ok());
            let reason = classify_provider_failure(ProviderFailure {
                message,
                status,
                raw_body: Some(crate::json::encode_body(&event)),
                ..ProviderFailure::default()
            });
            return Err(AiError { reason });
        }
        let mut events = Vec::new();
        let usage = map_usage(event.get("usage")).or_else(|| self.usage.clone());
        if self.finish_reason.is_some() {
            if has_late_content(&event) {
                return Err(shared::event_error(
                    ID,
                    "Mistral Chat received content after the finish reason",
                    Some(crate::json::encode_body(&event)),
                ));
            }
            self.usage = usage;
            return Ok(events);
        }
        let choice = event.get("choices").and_then(Value::as_array).and_then(|choices| choices.first());
        if let Some(content) = choice
            .and_then(|choice| choice.get("delta"))
            .and_then(|delta| delta.get("content"))
            .filter(|content| !content.is_null())
        {
            self.append_content(&mut events, content);
        }
        let deltas = choice
            .and_then(|choice| choice.get("delta"))
            .and_then(|delta| delta.get("tool_calls"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        self.append_tools(&mut events, deltas)?;
        let raw_finish_reason = choice
            .and_then(|choice| choice.get("finish_reason"))
            .and_then(Value::as_str)
            .filter(|reason| !reason.is_empty());
        let Some(raw_finish_reason) = raw_finish_reason else {
            self.usage = usage;
            return Ok(events);
        };
        let normalized = map_finish_reason(raw_finish_reason);
        if normalized == FinishReason::Error {
            let message = format!("Mistral Chat stopped with {raw_finish_reason}");
            let body = Some(crate::json::encode_body(&event));
            let reason = if raw_finish_reason == "network_error" {
                AiErrorReason::ProviderInternal { message, body, http: None, retry_after_ms: None }
            } else {
                AiErrorReason::UnknownProvider { message, body, http: None }
            };
            return Err(AiError { reason });
        }
        let incomplete = matches!(normalized, FinishReason::Length | FinishReason::ContentFilter);
        if !incomplete && !self.pending_tools.is_empty() {
            return Err(shared::event_error(ID, MISSING_TOOL_NAME, Some(crate::json::encode_body(&event))));
        }
        if !incomplete && !self.tools.is_empty() {
            self.completed_tools = self.finish_tools()?;
        }
        self.usage = usage;
        self.finish_reason = Some(FinishReasonDetails {
            normalized,
            raw: Some(raw_finish_reason.to_string()),
            category: None,
            explanation: None,
        });
        Ok(events)
    }

    fn halt(&mut self) -> Result<Vec<LlmEvent>, AiError> {
        self.finish_events()
    }

    fn terminal(&self, frame: &Frame) -> bool {
        frame.as_str() == DONE
    }
}

impl Protocol for MistralChat {
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

    fn default_path(&self, _model: &Model, _body: &Value) -> Option<String> {
        Some(PATH.to_string())
    }

    fn framing(&self) -> Definition {
        crate::route::framing::sse_with_done()
    }

    fn new_state(&self, _request: &ResolvedRequest, _context: &ExecutionContext) -> Box<dyn ProtocolState> {
        Box::new(State {
            tools: Vec::new(),
            pending_tools: Vec::new(),
            tool_ids: BTreeMap::new(),
            used_tool_ids: BTreeSet::new(),
            completed_tools: Vec::new(),
            latest_tool_key: None,
            generated_tools: 0,
            lifecycle: lifecycle::initial(),
            active: None,
            next_content: 0,
            usage: None,
            finish_reason: None,
        })
    }
}

struct Options {
    safe_prompt: Option<bool>,
    document_image_limit: Option<Number>,
    document_page_limit: Option<Number>,
    parallel_tool_calls: Option<bool>,
    reasoning_effort: Option<String>,
    prompt_mode: Option<()>,
    prompt_cache_key: Option<String>,
}

fn decode_options(input: Option<&Map<String, Value>>) -> Result<Options, AiError> {
    let empty = Map::new();
    let input = input.unwrap_or(&empty);
    Ok(Options {
        safe_prompt: optional_bool(input.get("safePrompt"), "safePrompt")?,
        document_image_limit: optional_number(input.get("documentImageLimit"), "documentImageLimit")?,
        document_page_limit: optional_number(input.get("documentPageLimit"), "documentPageLimit")?,
        parallel_tool_calls: optional_bool(input.get("parallelToolCalls"), "parallelToolCalls")?,
        reasoning_effort: optional_string(input.get("reasoningEffort"), "reasoningEffort")?,
        prompt_mode: optional_prompt_mode(input.get("promptMode"))?,
        prompt_cache_key: optional_string(input.get("promptCacheKey"), "promptCacheKey")?,
    })
}

fn optional_bool(value: Option<&Value>, field: &str) -> Result<Option<bool>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(option_error(field)),
    }
}

fn optional_number(value: Option<&Value>, field: &str) -> Result<Option<Number>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::Number(value)) => Ok(Some(value.clone())),
        Some(_) => Err(option_error(field)),
    }
}

fn optional_string(value: Option<&Value>, field: &str) -> Result<Option<String>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(option_error(field)),
    }
}

fn optional_prompt_mode(value: Option<&Value>) -> Result<Option<()>, AiError> {
    match value {
        None => Ok(None),
        Some(Value::String(value)) if value == "reasoning" => Ok(Some(())),
        Some(_) => Err(option_error("promptMode")),
    }
}

fn option_error(field: &str) -> AiError {
    shared::invalid_request(format!("mistral-chat providerOptions {field} is invalid"))
}

pub fn from_request(request: &LlmRequest, model: &Model) -> Result<Value, AiError> {
    let options = decode_options(request.provider_options.as_ref())?;
    if options.reasoning_effort.is_some() && options.prompt_mode.is_some() {
        return Err(shared::invalid_request(
            "Mistral Chat reasoningEffort and promptMode provider options are mutually exclusive",
        ));
    }
    let tool_choice = match request.tool_choice.as_ref() {
        Some(tool_choice) => Some(lower_tool_choice(tool_choice)?),
        None => None,
    };
    let (tools, flattened) = shared::flatten_tool_request(request);
    let messages = lower_messages(&flattened)?;
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.id.clone()));
    body.insert("messages".to_string(), Value::Array(messages));
    if !tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(tools.iter().map(lower_tool).collect()));
    }
    if let Some(tool_choice) = tool_choice {
        body.insert("tool_choice".to_string(), tool_choice);
    }
    body.insert("stream".to_string(), Value::Bool(true));
    if let Some(max_tokens) = request.generation.max_tokens.clone() {
        body.insert("max_tokens".to_string(), Value::Number(max_tokens));
    }
    if let Some(seed) = request.generation.seed.clone() {
        body.insert("random_seed".to_string(), Value::Number(seed));
    }
    if let Some(temperature) = request.generation.temperature.clone() {
        body.insert("temperature".to_string(), Value::Number(temperature));
    }
    if let Some(top_p) = request.generation.top_p.clone() {
        body.insert("top_p".to_string(), Value::Number(top_p));
    }
    if let Some(frequency_penalty) = request.generation.frequency_penalty.clone() {
        body.insert("frequency_penalty".to_string(), Value::Number(frequency_penalty));
    }
    if let Some(presence_penalty) = request.generation.presence_penalty.clone() {
        body.insert("presence_penalty".to_string(), Value::Number(presence_penalty));
    }
    if let Some(stop) = request.generation.stop.as_ref() {
        body.insert("stop".to_string(), Value::Array(stop.iter().map(|stop| Value::String(stop.clone())).collect()));
    }
    if sends_prompt_cache_key(request, model)
        && let Some(key) = options.prompt_cache_key.clone().or_else(|| request.prompt_cache_key.clone())
    {
        body.insert("prompt_cache_key".to_string(), Value::String(key));
    }
    if let Some(safe_prompt) = options.safe_prompt {
        body.insert("safe_prompt".to_string(), Value::Bool(safe_prompt));
    }
    if let Some(limit) = options.document_image_limit.clone() {
        body.insert("document_image_limit".to_string(), Value::Number(limit));
    }
    if let Some(limit) = options.document_page_limit.clone() {
        body.insert("document_page_limit".to_string(), Value::Number(limit));
    }
    let parallel_tool_calls = match options.parallel_tool_calls {
        Some(value) => Some(value),
        None => request
            .tool_choice
            .as_ref()
            .and_then(|tool_choice| tool_choice.disable_parallel_tool_use)
            .filter(|disabled| *disabled)
            .map(|_| false),
    };
    if let Some(parallel_tool_calls) = parallel_tool_calls {
        body.insert("parallel_tool_calls".to_string(), Value::Bool(parallel_tool_calls));
    }
    if let Some(reasoning_effort) = options.reasoning_effort {
        body.insert("reasoning_effort".to_string(), Value::String(reasoning_effort));
    }
    if options.prompt_mode.is_some() {
        body.insert("prompt_mode".to_string(), Value::String("reasoning".to_string()));
    }
    Ok(Value::Object(body))
}

fn sends_prompt_cache_key(request: &LlmRequest, model: &Model) -> bool {
    if matches!(request.cache, Some(CachePolicy::Mode(CacheMode::None))) {
        return false;
    }
    !model
        .capabilities
        .as_ref()
        .and_then(|capabilities| capabilities.get("promptCacheKey"))
        .is_some_and(|value| value == &Value::Bool(false))
}

fn lower_tool_choice(tool_choice: &ToolChoice) -> Result<Value, AiError> {
    match tool_choice.r#type {
        ToolChoiceType::Auto => return Ok(Value::String("auto".to_string())),
        ToolChoiceType::None => return Ok(Value::String("none".to_string())),
        ToolChoiceType::Required => return Ok(Value::String("any".to_string())),
        ToolChoiceType::Tool => {}
    }
    let name = tool_choice
        .name
        .as_deref()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| shared::invalid_request("Mistral Chat tool choice requires a tool name"))?;
    let mut function = Map::new();
    function.insert("name".to_string(), Value::String(name.to_string()));
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("function".to_string()));
    object.insert("function".to_string(), Value::Object(function));
    Ok(Value::Object(object))
}

fn lower_tool(tool: &crate::schema::ToolDefinition) -> Value {
    let mut function = Map::new();
    function.insert("name".to_string(), Value::String(tool.name.clone()));
    function.insert("description".to_string(), Value::String(tool.description.clone()));
    function.insert("parameters".to_string(), tool.input_schema.clone());
    function.insert("strict".to_string(), Value::Bool(false));
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("function".to_string()));
    object.insert("function".to_string(), Value::Object(function));
    Value::Object(object)
}

fn lower_messages(request: &LlmRequest) -> Result<Vec<Value>, AiError> {
    let mut normalizer = mistral_tool_id::normalizer(request);
    let normalize: &mut dyn FnMut(&str) -> String = &mut *normalizer;
    let mut messages = Vec::new();
    if !request.system.is_empty() {
        let content = if request.system.len() == 1 {
            let SystemPart::Text { text, .. } = &request.system[0];
            Value::String(text.clone())
        } else {
            Value::Array(
                request
                    .system
                    .iter()
                    .map(|part| {
                        let SystemPart::Text { text, .. } = part;
                        text_content(text)
                    })
                    .collect(),
            )
        };
        messages.push(message_object("system", content));
    }
    for message in &request.messages {
        match message {
            Message::System(_) => {
                let update = shared::wrapped_system_update("Mistral Chat", message)?;
                messages.push(message_object("user", Value::String(update.text)));
            }
            Message::User(message) => messages.push(lower_user(message)?),
            Message::Assistant(message) => {
                if skip_assistant(message) {
                    continue;
                }
                messages.push(lower_assistant(message, normalize)?);
            }
            Message::Tool(message) => {
                for part in &message.content {
                    let ContentPart::ToolResult(part) = part else {
                        continue;
                    };
                    messages.push(lower_tool_result(part, normalize)?);
                }
            }
        }
    }
    if let Some(trailing) = messages.last_mut() {
        let assistant_without_tools =
            trailing.get("role").and_then(Value::as_str) == Some("assistant") && trailing.get("tool_calls").is_none();
        if assistant_without_tools && let Some(object) = trailing.as_object_mut() {
            object.insert("prefix".to_string(), Value::Bool(true));
        }
    }
    Ok(messages)
}

fn skip_assistant(message: &AssistantMessage) -> bool {
    let has_tool_calls = message.content.iter().any(|part| matches!(part, ContentPart::ToolCall(_)));
    let has_native_thinking = message.content.iter().any(native_thinking);
    let text = message
        .content
        .iter()
        .map(|part| match part {
            ContentPart::Text(part) => part.text.as_str(),
            _ => "",
        })
        .collect::<String>();
    !has_tool_calls && !has_native_thinking && text.trim().is_empty()
}

fn lower_user(message: &UserMessage) -> Result<Value, AiError> {
    let mut content = Vec::new();
    for part in &message.content {
        match part {
            ContentPart::Text(part) => content.push(text_content(&part.text)),
            ContentPart::Media(part) => content.push(lower_media(part)?),
            _ => {}
        }
    }
    if content.iter().all(|part| part.get("type").and_then(Value::as_str) == Some("text")) {
        let text =
            content.iter().map(|part| part.get("text").and_then(Value::as_str).unwrap_or_default()).collect::<String>();
        return Ok(message_object("user", Value::String(text)));
    }
    Ok(message_object("user", Value::Array(content)))
}

fn lower_assistant(message: &AssistantMessage, normalize: &mut dyn FnMut(&str) -> String) -> Result<Value, AiError> {
    let structured = message.content.iter().any(native_thinking);
    let mut content = Vec::new();
    let mut text = Vec::new();
    let mut tool_calls = Vec::new();
    for part in &message.content {
        match part {
            ContentPart::Text(part) => {
                if structured {
                    content.push(text_content(&part.text));
                } else {
                    text.push(part.text.clone());
                }
            }
            ContentPart::Reasoning(part) => {
                let native = shared::provider_metadata_value(part.provider_metadata.as_ref(), METADATA_KEY)
                    .and_then(|metadata| metadata.get("thinking"));
                if structured && let Some(native) = native.filter(|native| is_native_thinking_content(native)) {
                    content.push(native.clone());
                }
            }
            ContentPart::ToolCall(part) => {
                let mut function = Map::new();
                function.insert("name".to_string(), Value::String(part.name.clone()));
                function.insert("arguments".to_string(), Value::String(crate::json::encode_body(&part.input)));
                let mut tool_call = Map::new();
                tool_call.insert("id".to_string(), Value::String(normalize(&part.id)));
                tool_call.insert("type".to_string(), Value::String("function".to_string()));
                tool_call.insert("function".to_string(), Value::Object(function));
                tool_calls.push(Value::Object(tool_call));
            }
            _ => return Err(shared::invalid_request(UNSUPPORTED_ASSISTANT)),
        }
    }
    let content = if structured { Value::Array(content) } else { Value::String(text.concat()) };
    let mut object = Map::new();
    object.insert("role".to_string(), Value::String("assistant".to_string()));
    object.insert("content".to_string(), content);
    if !tool_calls.is_empty() {
        object.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }
    Ok(Value::Object(object))
}

fn lower_tool_result(
    part: &crate::schema::ToolResultPart,
    normalize: &mut dyn FnMut(&str) -> String,
) -> Result<Value, AiError> {
    let content = match &part.result {
        ToolResultValue::Content { value } => {
            let mut content = Vec::new();
            for item in value {
                match item {
                    ToolContent::Text { text } => content.push(text_content(text)),
                    ToolContent::File { .. } => {
                        if let Some(media) = shared::tool_file_media(item) {
                            content.push(lower_media(&media)?);
                        }
                    }
                }
            }
            if content.iter().all(|item| item.get("type").and_then(Value::as_str) == Some("text")) {
                Value::String(
                    content
                        .iter()
                        .map(|item| item.get("text").and_then(Value::as_str).unwrap_or_default())
                        .collect::<String>(),
                )
            } else {
                Value::Array(content)
            }
        }
        _ => Value::String(shared::tool_result_text(part)),
    };
    let mut object = Map::new();
    object.insert("role".to_string(), Value::String("tool".to_string()));
    object.insert("tool_call_id".to_string(), Value::String(normalize(&part.id)));
    object.insert("name".to_string(), Value::String(part.name.clone()));
    object.insert("content".to_string(), content);
    Ok(Value::Object(object))
}

fn lower_media(part: &MediaPart) -> Result<Value, AiError> {
    let media_type = shared::media_type(&part.media);
    let mime = media_type.to_lowercase();
    let url = match shared::media_url(&part.media) {
        Some(url) => url.to_string(),
        None => shared::require_inline_media("Mistral Chat", &part.media)?.data_url,
    };
    if mime.starts_with("image/") {
        return Ok(media_content("image_url", "image_url", url));
    }
    if mime == "application/pdf" {
        return Ok(media_content("document_url", "document_url", url));
    }
    Err(shared::invalid_request(format!("Mistral Chat does not support media type {media_type}")))
}

fn media_content(kind: &str, field: &str, url: String) -> Value {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String(kind.to_string()));
    object.insert(field.to_string(), Value::String(url));
    Value::Object(object)
}

fn text_content(text: &str) -> Value {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("text".to_string()));
    object.insert("text".to_string(), Value::String(text.to_string()));
    Value::Object(object)
}

fn message_object(role: &str, content: Value) -> Value {
    let mut object = Map::new();
    object.insert("role".to_string(), Value::String(role.to_string()));
    object.insert("content".to_string(), content);
    Value::Object(object)
}

fn native_thinking(part: &ContentPart) -> bool {
    let ContentPart::Reasoning(part) = part else {
        return false;
    };
    shared::provider_metadata_value(part.provider_metadata.as_ref(), METADATA_KEY)
        .and_then(|metadata| metadata.get("thinking"))
        .is_some_and(is_native_thinking_content)
}

fn is_native_thinking_content(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.get("type").and_then(Value::as_str) != Some("thinking") {
        return false;
    }
    object.get("thinking").and_then(Value::as_array).is_some_and(|units| units.iter().all(is_thinking_unit))
}

fn is_thinking_unit(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    matches!(object.get("type"), None | Some(Value::String(_)))
        && matches!(object.get("text"), None | Some(Value::String(_)))
}

fn thinking_units(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::String(text)) => vec![text_content(text)],
        Some(Value::Array(items)) => items.iter().filter(|item| is_thinking_unit(item)).cloned().collect(),
        _ => Vec::new(),
    }
}

fn thinking_text(units: &[Value]) -> String {
    units.iter().filter_map(|unit| unit.get("text").and_then(Value::as_str)).collect::<String>()
}

fn merge_thinking(previous: Option<Map<String, Value>>, part: &Value) -> Map<String, Value> {
    let mut merged = previous.unwrap_or_default();
    if let Some(object) = part.as_object() {
        for (key, value) in object {
            merged.insert(key.clone(), value.clone());
        }
    }
    merged.insert("type".to_string(), Value::String("thinking".to_string()));
    merged.insert("thinking".to_string(), Value::Array(Vec::new()));
    merged
}

fn thinking_metadata(content: Value) -> ProviderMetadata {
    let mut inner = Map::new();
    inner.insert("thinking".to_string(), content);
    let mut metadata = ProviderMetadata::new();
    metadata.insert(METADATA_KEY.to_string(), Value::Object(inner));
    metadata
}

fn tool_text(tool: &Value) -> String {
    match tool.get("function").and_then(|function| function.get("arguments")) {
        Some(Value::String(text)) => text.clone(),
        None | Some(Value::Null) => String::new(),
        Some(value) => crate::json::encode_body(value),
    }
}

fn has_late_content(event: &Value) -> bool {
    let delta = event
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("delta"));
    if delta.and_then(|delta| delta.get("content")).and_then(Value::as_str).is_some_and(|text| !text.is_empty()) {
        return true;
    }
    if delta.and_then(|delta| delta.get("content")).and_then(Value::as_array).is_some_and(|items| !items.is_empty()) {
        return true;
    }
    delta.and_then(|delta| delta.get("tool_calls")).and_then(Value::as_array).is_some_and(|tools| {
        tools.iter().any(|tool| {
            let id = tool.get("id").and_then(Value::as_str).is_some_and(|id| !id.is_empty());
            let name = tool
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .is_some_and(|name| !name.is_empty());
            let arguments = tool.get("function").and_then(|function| function.get("arguments")).is_some();
            id || name || arguments
        })
    })
}

fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "stop" => FinishReason::Stop,
        "length" | "model_length" => FinishReason::Length,
        "tool_calls" => FinishReason::ToolCalls,
        "content_filter" => FinishReason::ContentFilter,
        "error" | "network_error" => FinishReason::Error,
        _ => FinishReason::Unknown,
    }
}

fn map_usage(usage: Option<&Value>) -> Option<Usage> {
    let usage = usage?.as_object()?;
    let number = |key: &str| usage.get(key).and_then(Value::as_u64);
    let input = number("prompt_tokens");
    let reported = number("num_cached_tokens")
        .or_else(|| {
            usage.get("prompt_tokens_details").and_then(|details| details.get("cached_tokens")).and_then(Value::as_u64)
        })
        .or_else(|| {
            usage.get("prompt_token_details").and_then(|details| details.get("cached_tokens")).and_then(Value::as_u64)
        });
    let cached = match (input, reported) {
        (Some(input), Some(reported)) => Some(reported.min(input)),
        _ => None,
    };
    let output = number("completion_tokens");
    let mut provider_metadata = ProviderMetadata::new();
    provider_metadata.insert(METADATA_KEY.to_string(), Value::Object(usage.clone()));
    Some(Usage {
        context_tokens: None,
        input_tokens: input,
        output_tokens: output,
        non_cached_input_tokens: shared::subtract_tokens(input, cached),
        cache_read_input_tokens: cached,
        cache_write_input_tokens: None,
        reasoning_tokens: None,
        total_tokens: shared::total_tokens(input, output, number("total_tokens")),
        cost: None,
        provider_metadata: Some(provider_metadata),
    })
}
