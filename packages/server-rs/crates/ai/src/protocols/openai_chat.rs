use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::ExecutionContext;
use crate::protocols::openai_freeform::{self, Custom};
use crate::protocols::shared;
use crate::protocols::utils::cache::{self, CacheControl};
use crate::protocols::utils::open_responses_options;
use crate::protocols::utils::{lifecycle, tool_stream};
use crate::provider_error::{ProviderFailure, classify_provider_failure};
use crate::route::framing::Definition;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::{Frame, Protocol, ProtocolState};
use crate::schema::{
    AiError, AiErrorReason, AssistantMessage, CacheHint, ContentPart, FinishReason, FinishReasonDetails,
    InvalidProviderOutputClassification, LlmEvent, LlmRequest, MediaPart, Message, ProviderMetadata, ReasoningPart,
    SystemPart, ToolCallPart, ToolChoice, ToolChoiceType, ToolContent, ToolDefinition, ToolMessage, ToolResultValue,
    Usage, UserMessage,
};

pub const ID: &str = "openai-chat";
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const PATH: &str = "/chat/completions";

const ADAPTER: &str = "OpenAI Chat";
const DONE: &str = "[DONE]";
const REASONING_ID: &str = "reasoning-0";
const TEXT_ID: &str = "text-0";
const RESERVED_REASONING_FIELDS: [&str; 4] = ["role", "content", "refusal", "tool_calls"];
const MISTRAL_FAMILIES: [&str; 5] = ["mistral", "devstral", "codestral", "pixtral", "mixtral"];
const OPENAI_MODEL_PREFIX: &str = "openai/";

pub struct OpenAiChat;

#[derive(Clone)]
struct PendingToolDelta {
    id: Option<String>,
    name: Option<String>,
    input: String,
    extra_content: Option<Value>,
}

struct State {
    provider_metadata_key: String,
    tools: tool_stream::State,
    custom: BTreeMap<String, Custom>,
    custom_calls: BTreeSet<i64>,
    pending_tools: BTreeMap<i64, PendingToolDelta>,
    tool_call_events: Vec<LlmEvent>,
    usage: Option<Usage>,
    finish_reason: Option<FinishReasonDetails>,
    lifecycle: lifecycle::State,
    reasoning_field: Option<String>,
    reasoning_text_observed: bool,
    reasoning_details: Vec<Value>,
    reasoning_details_observed: bool,
    reasoning_opaque: Option<String>,
    reasoning_emitted: bool,
    latest_tool_index: Option<i64>,
    next_tool_index: i64,
    require_finish_reason: bool,
}

impl ProtocolState for State {
    fn step(&mut self, frame: Frame) -> Result<Vec<LlmEvent>, AiError> {
        if self.terminal(&frame) {
            return Ok(Vec::new());
        }
        let event: Value = match serde_json::from_str(&frame) {
            Ok(event) => event,
            Err(_) => return Err(shared::event_error(ID, "Invalid openai-chat stream event", Some(frame))),
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
        let choice = event.get("choices").and_then(Value::as_array).and_then(|choices| choices.first());
        let usage = map_usage(event.get("usage"), &self.provider_metadata_key, &event)
            .or_else(|| map_usage(choice.and_then(|choice| choice.get("usage")), &self.provider_metadata_key, &event))
            .or_else(|| self.usage.clone());
        let raw_finish_reason = choice
            .and_then(|choice| choice.get("finish_reason"))
            .and_then(Value::as_str)
            .filter(|reason| !reason.is_empty());
        let finish_reason = match raw_finish_reason {
            Some(reason) => {
                let native = choice.and_then(|choice| choice.get("native_finish_reason")).and_then(Value::as_str);
                Some(FinishReasonDetails {
                    normalized: map_finish_reason(&event, reason)?,
                    raw: Some(native.unwrap_or(reason).to_string()),
                    category: match reason {
                        "content_filter" => content_filter_category(choice),
                        _ => None,
                    },
                    explanation: None,
                })
            }
            None => self.finish_reason.clone(),
        };
        let delta = choice.and_then(|choice| choice.get("delta")).filter(|delta| delta.is_object());
        let tool_deltas: &[Value] = match delta.and_then(|delta| delta.get("tool_calls")).and_then(Value::as_array) {
            Some(tool_deltas) => tool_deltas,
            None => &[],
        };
        let mut lifecycle = self.lifecycle.clone();
        if self.finish_reason.is_some() && tool_deltas.iter().any(tool_delta_has_content) {
            return Err(shared::event_error(
                ID,
                "OpenAI Chat received content after the finish reason",
                Some(crate::json::encode_body(&event)),
            ));
        }
        let reasoning = reasoning_delta(delta, self.reasoning_field.as_deref());
        let reasoning_field =
            self.reasoning_field.clone().or_else(|| reasoning.as_ref().map(|(field, _)| field.clone()));
        let reasoning_text_observed = self.reasoning_text_observed || reasoning.is_some();
        let detail_delta = delta
            .and_then(|delta| delta.get("reasoning_details"))
            .and_then(Value::as_array)
            .map(|details| known_reasoning_details(details));
        if let Some(details) = detail_delta.as_ref() {
            append_reasoning_details(&mut self.reasoning_details, details);
        }
        let reasoning_details_observed = self.reasoning_details_observed || detail_delta.is_some();
        let reasoning_opaque = delta
            .and_then(|delta| delta.get("reasoning_opaque"))
            .and_then(Value::as_str)
            .filter(|opaque| !opaque.is_empty())
            .map(str::to_string)
            .or_else(|| self.reasoning_opaque.clone());
        let delta_metadata = reasoning_metadata(&self.provider_metadata_key, reasoning_field.as_deref(), None, None);
        let text = detail_delta
            .as_ref()
            .filter(|details| !details.is_empty())
            .and_then(|details| detail_text(details, reasoning_text_observed))
            .or_else(|| reasoning.as_ref().map(|(_, text)| text.clone()));
        if let Some(text) = text {
            lifecycle = lifecycle::reasoning_delta(lifecycle, &mut events, REASONING_ID, &text, delta_metadata);
        } else if (reasoning_details_observed || reasoning_opaque.is_some())
            && !lifecycle.reasoning.iter().any(|open| open == REASONING_ID)
            && (truthy_str(delta.and_then(|delta| delta.get("content"))).is_some()
                || truthy_str(delta.and_then(|delta| delta.get("refusal"))).is_some()
                || !tool_deltas.is_empty())
        {
            lifecycle = lifecycle::reasoning_start(lifecycle, &mut events, REASONING_ID, delta_metadata);
        }
        let reasoning_emitted = self.reasoning_emitted || lifecycle.reasoning.iter().any(|open| open == REASONING_ID);
        if let Some(content) =
            delta.and_then(|delta| delta.get("content")).and_then(Value::as_str).filter(|text| !text.is_empty())
        {
            lifecycle = lifecycle::text_delta(lifecycle, &mut events, TEXT_ID, content, None);
        }
        if let Some(refusal) =
            delta.and_then(|delta| delta.get("refusal")).and_then(Value::as_str).filter(|text| !text.is_empty())
        {
            lifecycle = lifecycle::text_delta(lifecycle, &mut events, TEXT_ID, refusal, None);
        }
        let mut latest_tool_index = self.latest_tool_index;
        let mut next_tool_index = self.next_tool_index;
        for (position, tool) in tool_deltas.iter().enumerate() {
            let tool_id = truthy_str(tool.get("id"));
            let matched = tool_index_by_id(&self.tools, &self.pending_tools, tool_id);
            let fallback =
                if tool_deltas.len() > 1 { position as i64 } else { latest_tool_index.unwrap_or(position as i64) };
            let fallback_id = self
                .tools
                .get(&fallback)
                .map(|tool| tool.id.as_str())
                .or_else(|| self.pending_tools.get(&fallback).and_then(|tool| tool.id.as_deref()));
            let index = tool.get("index").and_then(Value::as_i64).or(matched).unwrap_or_else(|| {
                let taken = match (tool_id, fallback_id) {
                    (Some(id), Some(fallback_id)) => !fallback_id.is_empty() && fallback_id != id,
                    _ => false,
                };
                if taken { next_tool_index } else { fallback }
            });
            let current = self.tools.get(&index).cloned();
            let pending = self.pending_tools.get(&index).cloned();
            let id = current
                .as_ref()
                .map(|tool| tool.id.clone())
                .or_else(|| pending.as_ref().and_then(|tool| tool.id.clone()))
                .or_else(|| tool_id.map(str::to_string));
            let name = current
                .as_ref()
                .map(|tool| tool.name.clone())
                .or_else(|| pending.as_ref().and_then(|tool| tool.name.clone()))
                .or_else(|| {
                    truthy_str(tool.get("function").and_then(|function| function.get("name"))).map(str::to_string)
                })
                .or_else(|| truthy_str(tool.get("custom").and_then(|custom| custom.get("name"))).map(str::to_string));
            let custom = object_field(tool, "custom");
            let opening = custom.is_some() && !self.custom_calls.contains(&index);
            if opening {
                self.custom_calls.insert(index);
            }
            let raw = custom.and_then(|custom| custom.get("input")).and_then(Value::as_str).unwrap_or_default();
            let fragment = match custom {
                Some(_) if opening => {
                    openai_freeform::stream_start(self.custom.get(name.as_deref().unwrap_or_default()), raw)
                }
                Some(_) => openai_freeform::stream_delta(raw),
                None => tool
                    .get("function")
                    .and_then(|function| function.get("arguments"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            };
            let text = match pending.as_ref() {
                Some(pending) => format!("{}{fragment}", pending.input),
                None => fragment.clone(),
            };
            let extra_content = pending
                .as_ref()
                .and_then(|pending| pending.extra_content.clone())
                .or_else(|| decode_tool_extra_content(tool.get("extra_content")));
            latest_tool_index = Some(index);
            next_tool_index = next_tool_index.max(index + 1);
            if current.is_none() && (id.is_none() || name.is_none()) {
                self.pending_tools.insert(index, PendingToolDelta { id, name, input: text, extra_content });
                continue;
            }
            self.pending_tools.remove(&index);
            let provider_metadata = extra_content.map(|extra_content| {
                let mut inner = Map::new();
                inner.insert("extraContent".to_string(), extra_content);
                let mut metadata = ProviderMetadata::new();
                metadata.insert(self.provider_metadata_key.clone(), Value::Object(inner));
                metadata
            });
            let outcome = tool_stream::append_or_start(
                ADAPTER,
                &mut self.tools,
                index,
                tool_stream::AppendDelta { id: id.as_deref(), name: name.as_deref(), text: &text, provider_metadata },
                "OpenAI Chat tool call delta is missing id or name",
            );
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(error) => return Err(with_event_body(error, &event)),
            };
            if !outcome.events.is_empty() {
                lifecycle = lifecycle::step_start(lifecycle, &mut events);
            }
            events.extend(outcome.events);
        }
        let incomplete_tools = finish_reason
            .as_ref()
            .is_some_and(|reason| matches!(reason.normalized, FinishReason::ContentFilter | FinishReason::Length));
        if finish_reason.is_some()
            && !incomplete_tools
            && self.finish_reason.is_none()
            && !self.pending_tools.is_empty()
        {
            return Err(shared::event_error(
                ID,
                "OpenAI Chat tool call delta is missing id or name",
                Some(crate::json::encode_body(&event)),
            ));
        }
        if finish_reason.is_some() && !incomplete_tools && self.finish_reason.is_none() && !self.tools.is_empty() {
            self.tool_call_events = self.finish_tools()?;
        }
        self.usage = usage;
        if self.finish_reason.is_none() {
            self.finish_reason = finish_reason;
        }
        self.lifecycle = lifecycle;
        self.reasoning_field = reasoning_field;
        self.reasoning_text_observed = reasoning_text_observed;
        self.reasoning_details_observed = reasoning_details_observed;
        self.reasoning_opaque = reasoning_opaque;
        self.reasoning_emitted = reasoning_emitted;
        self.latest_tool_index = latest_tool_index;
        self.next_tool_index = next_tool_index;
        Ok(events)
    }

    fn halt(&mut self) -> Result<Vec<LlmEvent>, AiError> {
        if self.finish_reason.is_none() && self.require_finish_reason {
            let reason = AiErrorReason::InvalidProviderOutput {
                message: "OpenAI Chat stream ended without finish_reason".to_string(),
                body: None,
                http: None,
                classification: Some(InvalidProviderOutputClassification::IncompleteStream),
                route: Some(ID.to_string()),
            };
            return Err(AiError { reason });
        }
        let tool_call_events = if self.finish_reason.is_none() && !self.tools.is_empty() {
            self.finish_tools()?
        } else {
            self.tool_call_events.clone()
        };
        let has_tool_calls = !tool_call_events.is_empty();
        let reason = match self.finish_reason.clone() {
            Some(mut reason) => {
                if matches!(reason.normalized, FinishReason::Stop) && has_tool_calls {
                    reason.normalized = FinishReason::ToolCalls;
                }
                reason
            }
            None => FinishReasonDetails {
                normalized: if has_tool_calls { FinishReason::ToolCalls } else { FinishReason::Stop },
                raw: None,
                category: None,
                explanation: None,
            },
        };
        let metadata = reasoning_metadata(
            &self.provider_metadata_key,
            self.reasoning_field.as_deref(),
            self.reasoning_details_observed.then_some(self.reasoning_details.as_slice()),
            self.reasoning_opaque.as_deref(),
        );
        let mut events = Vec::new();
        let started = if (self.reasoning_details_observed || self.reasoning_opaque.is_some()) && !self.reasoning_emitted
        {
            lifecycle::reasoning_start(
                self.lifecycle.clone(),
                &mut events,
                REASONING_ID,
                reasoning_metadata(&self.provider_metadata_key, self.reasoning_field.as_deref(), None, None),
            )
        } else {
            self.lifecycle.clone()
        };
        let ended = lifecycle::reasoning_end(started, &mut events, REASONING_ID, metadata, None);
        let lifecycle = if tool_call_events.is_empty() { ended } else { lifecycle::step_start(ended, &mut events) };
        events.extend(tool_call_events);
        self.lifecycle = lifecycle::finish(lifecycle, &mut events, reason, self.usage.clone(), None);
        Ok(events)
    }

    fn terminal(&self, frame: &Frame) -> bool {
        frame.as_str() == DONE
    }
}

impl State {
    fn finish_tools(&mut self) -> Result<Vec<LlmEvent>, AiError> {
        let keys = self.custom_calls.iter().copied().collect::<Vec<i64>>();
        let mut closed = openai_freeform::close(std::mem::take(&mut self.tools), keys, None);
        let finished = tool_stream::finish_all(ADAPTER, &mut closed.tools)?;
        self.tools = closed.tools;
        let mut events = closed.events;
        events.extend(finished.events);
        Ok(events)
    }
}

impl Protocol for OpenAiChat {
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

    fn new_state(&self, request: &ResolvedRequest, _context: &ExecutionContext) -> Box<dyn ProtocolState> {
        let model = &request.model;
        let provider_metadata_key = model.route.provider_metadata_key.clone().unwrap_or_else(|| model.provider.clone());
        Box::new(State {
            provider_metadata_key,
            tools: tool_stream::empty(),
            custom: openai_freeform::tools(&request.request),
            custom_calls: BTreeSet::new(),
            pending_tools: BTreeMap::new(),
            tool_call_events: Vec::new(),
            usage: None,
            finish_reason: None,
            lifecycle: lifecycle::initial(),
            reasoning_field: model.compatibility_str("reasoningField").map(str::to_string),
            reasoning_text_observed: false,
            reasoning_details: Vec::new(),
            reasoning_details_observed: false,
            reasoning_opaque: None,
            reasoning_emitted: false,
            latest_tool_index: None,
            next_tool_index: 0,
            require_finish_reason: compatibility_bool(model, "requireFinishReason").unwrap_or(true),
        })
    }
}

fn map_finish_reason(event: &Value, reason: &str) -> Result<FinishReason, AiError> {
    match reason {
        "error" => Err(AiError {
            reason: AiErrorReason::UnknownProvider {
                message: "Provider reported an error (finish_reason: error)".to_string(),
                body: Some(crate::json::encode_body(event)),
                http: None,
            },
        }),
        "network_error" => Err(AiError {
            reason: AiErrorReason::ProviderInternal {
                message: "Provider reported a network error (finish_reason: network_error)".to_string(),
                body: Some(crate::json::encode_body(event)),
                http: None,
                retry_after_ms: None,
            },
        }),
        "stop" | "end" => Ok(FinishReason::Stop),
        "length" => Ok(FinishReason::Length),
        "content_filter" => Ok(FinishReason::ContentFilter),
        "function_call" | "tool_calls" => Ok(FinishReason::ToolCalls),
        _ => Ok(FinishReason::Unknown),
    }
}

fn content_filter_category(choice: Option<&Value>) -> Option<String> {
    let annotations = choice.and_then(|choice| choice.get("delta")).and_then(|delta| delta.get("copilot_annotations"));
    let results = choice.and_then(|choice| choice.get("content_filter_results"));
    let null = Value::Null;
    shared::content_filter_category(&[annotations.unwrap_or(&null), results.unwrap_or(&null)])
}

fn map_usage(usage: Option<&Value>, provider_metadata_key: &str, event: &Value) -> Option<Usage> {
    let usage = usage.filter(|usage| usage.is_object())?;
    let number = |path: &[&str]| -> Option<u64> {
        let mut value = usage;
        for key in path {
            value = value.get(*key)?;
        }
        value.as_u64()
    };
    let input = number(&["prompt_tokens"]);
    let completion = number(&["completion_tokens"]);
    let cached = number(&["prompt_tokens_details", "cached_tokens"])
        .or_else(|| number(&["prompt_cache_hit_tokens"]))
        .or_else(|| number(&["cached_tokens"]))
        .or_else(|| number(&["cache_read_input_tokens"]));
    let cache_write =
        number(&["prompt_tokens_details", "cache_write_tokens"]).or_else(|| number(&["cache_created_input_tokens"]));
    let reasoning =
        number(&["completion_tokens_details", "reasoning_tokens"]).or_else(|| number(&["reasoning_tokens"]));
    let reported_total = usage.get("total_tokens").and_then(Value::as_u64);
    let output = match (completion, reasoning) {
        (Some(completion), Some(reasoning))
            if reported_total == shared::sum_tokens(&[input, Some(completion), Some(reasoning)]) =>
        {
            Some(completion.saturating_add(reasoning))
        }
        _ => completion,
    };
    let mut provider_metadata = ProviderMetadata::new();
    provider_metadata.insert(provider_metadata_key.to_string(), usage.clone());
    Some(Usage {
        context_tokens: None,
        input_tokens: input,
        output_tokens: output,
        non_cached_input_tokens: shared::subtract_tokens(input, shared::sum_tokens(&[cached, cache_write])),
        cache_read_input_tokens: cached,
        cache_write_input_tokens: cache_write,
        reasoning_tokens: reasoning,
        total_tokens: shared::total_tokens(input, output, reported_total),
        cost: shared::reported_billing(usage, &[event]),
        provider_metadata: Some(provider_metadata),
    })
}

fn tool_index_by_id(
    tools: &tool_stream::State,
    pending_tools: &BTreeMap<i64, PendingToolDelta>,
    id: Option<&str>,
) -> Option<i64> {
    let id = id?;
    let keys = tools.keys().chain(pending_tools.keys()).copied().collect::<BTreeSet<i64>>();
    keys.into_iter().find(|key| {
        let found = tools
            .get(key)
            .map(|tool| tool.id.as_str())
            .or_else(|| pending_tools.get(key).and_then(|tool| tool.id.as_deref()));
        found == Some(id)
    })
}

fn reasoning_delta(delta: Option<&Value>, configured_field: Option<&str>) -> Option<(String, String)> {
    let delta = delta.filter(|delta| delta.is_object())?;
    let fields = [configured_field, Some("reasoning_content"), Some("reasoning"), Some("reasoning_text")];
    for field in fields.into_iter().flatten() {
        let Some(text) = delta.get(field).and_then(Value::as_str).filter(|text| !text.is_empty()) else {
            continue;
        };
        return Some((field.to_string(), text.to_string()));
    }
    None
}

fn detail_text(details: &[Value], hide_kimi_summary: bool) -> Option<String> {
    let text = details
        .iter()
        .filter_map(|detail| {
            let body = match detail.get("type").and_then(Value::as_str) {
                Some("reasoning.text") => detail.get("text"),
                Some("reasoning.summary") => detail.get("summary"),
                Some("summary") if !hide_kimi_summary => detail.get("summary"),
                _ => None,
            };
            body.and_then(Value::as_str).filter(|text| !text.is_empty()).map(str::to_string)
        })
        .collect::<Vec<String>>();
    if text.is_empty() {
        return None;
    }
    Some(text.concat())
}

fn append_reasoning_details(result: &mut Vec<Value>, details: &[Value]) {
    for detail in details {
        match result.last().and_then(|previous| merge_reasoning_details(previous, detail)) {
            Some(merged) => {
                let last = result.len() - 1;
                result[last] = merged;
            }
            None => result.push(detail.clone()),
        }
    }
}

fn merge_reasoning_details(previous: &Value, detail: &Value) -> Option<Value> {
    if conflicting_reasoning_details(previous, detail) {
        return None;
    }
    let previous_type = previous.get("type").and_then(Value::as_str)?;
    if previous_type != detail.get("type").and_then(Value::as_str)? {
        return None;
    }
    let mut merged = previous.as_object()?.clone();
    for (field, value) in detail.as_object()? {
        merged.insert(field.clone(), value.clone());
    }
    match previous_type {
        "reasoning.text" | "reasoning.summary" => {
            let key = if previous_type == "reasoning.text" { "text" } else { "summary" };
            merged.insert(
                key.to_string(),
                Value::String(format!("{}{}", string_field(previous, key), string_field(detail, key))),
            );
            for (field, value) in merge_detail_identity(previous, detail) {
                merged.insert(field, value);
            }
        }
        "summary" => {
            merged.insert(
                "summary".to_string(),
                Value::String(format!("{}{}", string_field(previous, "summary"), string_field(detail, "summary"))),
            );
        }
        _ => return None,
    }
    Some(Value::Object(merged))
}

fn merge_detail_identity(previous: &Value, current: &Value) -> Map<String, Value> {
    let mut identity = Map::new();
    for key in ["signature", "format"] {
        if let Some(value) = merge_detail_value(previous.get(key), current.get(key)) {
            identity.insert(key.to_string(), value);
        }
    }
    identity
}

fn merge_detail_value(previous: Option<&Value>, current: Option<&Value>) -> Option<Value> {
    if let Some(value) = previous.filter(|value| shared::is_truthy(value)) {
        return Some(value.clone());
    }
    if let Some(value) = current.filter(|value| shared::is_truthy(value)) {
        return Some(value.clone());
    }
    previous.or(current).cloned()
}

fn conflicting_reasoning_details(previous: &Value, current: &Value) -> bool {
    let signature_conflict = previous
        .get("signature")
        .and_then(Value::as_str)
        .filter(|signature| !signature.is_empty())
        .zip(current.get("signature").and_then(Value::as_str).filter(|signature| !signature.is_empty()))
        .is_some_and(|(previous, current)| previous != current);
    conflicting_detail_value(previous.get("id"), current.get("id"))
        || conflicting_detail_value(previous.get("index"), current.get("index"))
        || conflicting_detail_value(previous.get("format"), current.get("format"))
        || signature_conflict
}

fn conflicting_detail_value(previous: Option<&Value>, current: Option<&Value>) -> bool {
    let Some(previous) = previous.filter(|value| !value.is_null()) else {
        return false;
    };
    let Some(current) = current.filter(|value| !value.is_null()) else {
        return false;
    };
    previous != current
}

fn reasoning_metadata(
    provider_metadata_key: &str,
    field: Option<&str>,
    details: Option<&[Value]>,
    opaque: Option<&str>,
) -> Option<ProviderMetadata> {
    let mut inner = Map::new();
    if let Some(field) = field {
        inner.insert("reasoningField".to_string(), Value::String(field.to_string()));
    }
    if let Some(details) = details {
        inner.insert("reasoningDetails".to_string(), Value::Array(details.to_vec()));
    }
    if let Some(opaque) = opaque {
        inner.insert("reasoningOpaque".to_string(), Value::String(opaque.to_string()));
    }
    let mut metadata = ProviderMetadata::new();
    metadata.insert(provider_metadata_key.to_string(), Value::Object(inner));
    Some(metadata)
}

fn tool_delta_has_content(tool: &Value) -> bool {
    truthy_str(tool.get("id")).is_some()
        || truthy_str(tool.get("function").and_then(|function| function.get("name"))).is_some()
        || truthy_str(tool.get("function").and_then(|function| function.get("arguments"))).is_some()
        || truthy_str(tool.get("custom").and_then(|custom| custom.get("input"))).is_some()
}

fn decode_tool_extra_content(value: Option<&Value>) -> Option<Value> {
    let value = value?;
    value.get("google")?.get("thought_signature")?.as_str()?;
    Some(value.clone())
}

fn object_field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key).filter(|value| value.is_object())
}

fn truthy_str(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).filter(|text| !text.is_empty())
}

fn string_field(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn with_event_body(mut error: AiError, event: &Value) -> AiError {
    if let AiErrorReason::InvalidProviderOutput { body, .. } = &mut error.reason {
        *body = Some(crate::json::encode_body(event));
    }
    error
}

pub fn from_request(request: &LlmRequest, model: &Model) -> Result<Value, AiError> {
    if model.compatibility.as_ref().and_then(|compatibility| compatibility.get("cacheFormat")).is_some() {
        let mut cache_control = CacheControl::new();
        return from_request_with(request, model, Some(&mut cache_control));
    }
    from_request_with(request, model, None)
}

pub fn from_request_with(
    request: &LlmRequest,
    model: &Model,
    cache_control: Option<&mut CacheControl>,
) -> Result<Value, AiError> {
    let reasoning_field = model.compatibility_str("reasoningField");
    if let Some(field) = reasoning_field
        && RESERVED_REASONING_FIELDS.contains(&field)
    {
        return Err(shared::invalid_request(format!(
            "{ADAPTER} reasoning field conflicts with reserved field {field}"
        )));
    }
    let provider = model.provider.as_str();
    let base_url = model.route.endpoint.base_url.as_deref();
    let max_tokens_field =
        model.compatibility_str("maxTokensField").unwrap_or(detect_max_tokens_field(provider, base_url));
    let supports_store =
        capability_bool(model, &["store"]).unwrap_or_else(|| detect_supports_store(provider, base_url));
    let supports_strict_mode =
        capability_bool(model, &["tools", "strict"]).unwrap_or_else(|| detect_supports_strict_mode(provider, base_url));
    let zai_tool_stream = compatibility_bool(model, "zaiToolStream")
        .unwrap_or_else(|| detect_zai_tool_stream(provider, base_url, &model.id));
    let has_history = has_tool_history(&request.messages);
    let flattened = shared::flatten_tool_request(request);
    let has_active_tools = !flattened.0.is_empty();
    let model_id = model.id.to_lowercase();
    let mistral = MISTRAL_FAMILIES.iter().any(|family| model_id.contains(*family));
    let provider_metadata_key = model.route.provider_metadata_key.clone().unwrap_or_else(|| model.provider.clone());
    let require_reasoning = compatibility_bool(model, "requireReasoning").unwrap_or_else(|| {
        reasoning_field.is_some()
            || provider == "deepseek"
            || base_url.is_some_and(|url| url.to_lowercase().contains("deepseek.com"))
            || model_id.contains("deepseek")
    });
    let require_assistant_after_tool = compatibility_bool(model, "requireAssistantAfterTool").unwrap_or(mistral);
    let tool_call_id = ToolCallId {
        mistral,
        claude: model_id.contains("claude"),
        openai: provider == "openai" || provider == "azure" || model_id.starts_with(OPENAI_MODEL_PREFIX),
    };
    let mut lowering = Lowering {
        cache_control,
        custom: openai_freeform::tools(request),
        provider_metadata_key,
        tool_call_id,
        require_reasoning,
        reasoning_field: reasoning_field.map(str::to_string),
        require_assistant_after_tool,
    };
    let messages = lower_messages(&flattened.1, &mut lowering)?;
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.id.clone()));
    body.insert("messages".to_string(), Value::Array(messages));
    if flattened.0.is_empty() {
        if has_history {
            body.insert("tools".to_string(), Value::Array(Vec::new()));
        }
    } else {
        let tools = flattened
            .0
            .iter()
            .map(|tool| {
                let custom = lowering.custom.get(&tool.name).cloned();
                lower_tool(tool, &mut lowering, supports_strict_mode, custom.as_ref())
            })
            .collect::<Result<Vec<Value>, AiError>>()?;
        body.insert("tools".to_string(), Value::Array(tools));
    }
    if has_active_tools && let Some(tool_choice) = request.tool_choice.as_ref() {
        body.insert("tool_choice".to_string(), lower_tool_choice(tool_choice, &lowering.custom)?);
    }
    body.insert("stream".to_string(), Value::Bool(true));
    if capability_bool(model, &["streamUsage"]).unwrap_or(true) {
        let mut stream_options = Map::new();
        stream_options.insert("include_usage".to_string(), Value::Bool(true));
        body.insert("stream_options".to_string(), Value::Object(stream_options));
    }
    if zai_tool_stream && has_active_tools {
        body.insert("tool_stream".to_string(), Value::Bool(true));
    }
    if let Some(max_tokens) = request.generation.max_tokens.clone() {
        body.insert(max_tokens_field.to_string(), Value::Number(max_tokens));
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
    if let Some(seed) = request.generation.seed.clone() {
        body.insert("seed".to_string(), Value::Number(seed));
    }
    if let Some(stop) = request.generation.stop.as_ref() {
        body.insert("stop".to_string(), Value::Array(stop.iter().map(|value| Value::String(value.clone())).collect()));
    }
    for (key, value) in lower_options(model, request, supports_store, has_active_tools) {
        body.insert(key, value);
    }
    Ok(Value::Object(body))
}

struct ToolCallId {
    mistral: bool,
    claude: bool,
    openai: bool,
}

impl ToolCallId {
    fn apply(&self, id: &str) -> String {
        if self.mistral {
            let filtered = id.chars().filter(|character| character.is_ascii_alphanumeric()).take(9).collect::<String>();
            return format!("{filtered:0<9}");
        }
        if self.claude {
            return id
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                        return character;
                    }
                    '_'
                })
                .collect();
        }
        if self.openai {
            return id.chars().take(40).collect();
        }
        id.to_string()
    }
}

struct Lowering<'a> {
    cache_control: Option<&'a mut CacheControl>,
    custom: BTreeMap<String, Custom>,
    provider_metadata_key: String,
    tool_call_id: ToolCallId,
    require_reasoning: bool,
    reasoning_field: Option<String>,
    require_assistant_after_tool: bool,
}

impl Lowering<'_> {
    fn cache(&mut self, hint: Option<&CacheHint>) -> Option<Value> {
        match self.cache_control.as_mut() {
            Some(cache_control) => cache_control.take(hint),
            None => None,
        }
    }
}

fn lower_tool(
    tool: &ToolDefinition,
    lowering: &mut Lowering<'_>,
    supports_strict_mode: bool,
    custom: Option<&Custom>,
) -> Result<Value, AiError> {
    let mut object = Map::new();
    if let Some(custom) = custom {
        object.insert("type".to_string(), Value::String("custom".to_string()));
        let mut definition = Map::new();
        definition.insert("name".to_string(), Value::String(tool.name.clone()));
        definition.insert("description".to_string(), Value::String(tool.description.clone()));
        let format = match openai_freeform::grammar(&custom.format) {
            Some(grammar) => {
                let mut format = Map::new();
                format.insert("type".to_string(), Value::String("grammar".to_string()));
                format.insert("grammar".to_string(), grammar);
                Value::Object(format)
            }
            None => {
                let mut format = Map::new();
                format.insert("type".to_string(), Value::String("text".to_string()));
                Value::Object(format)
            }
        };
        definition.insert("format".to_string(), format);
        object.insert("custom".to_string(), Value::Object(definition));
        if let Some(cache_control) = lowering.cache(tool.cache.as_ref()) {
            object.insert("cache_control".to_string(), cache_control);
        }
        return Ok(Value::Object(object));
    }
    object.insert("type".to_string(), Value::String("function".to_string()));
    let mut function = Map::new();
    function.insert("name".to_string(), Value::String(tool.name.clone()));
    function.insert("description".to_string(), Value::String(tool.description.clone()));
    function.insert("parameters".to_string(), tool.input_schema.clone());
    if supports_strict_mode {
        function.insert("strict".to_string(), Value::Bool(false));
    }
    object.insert("function".to_string(), Value::Object(function));
    if let Some(cache_control) = lowering.cache(tool.cache.as_ref()) {
        object.insert("cache_control".to_string(), cache_control);
    }
    Ok(Value::Object(object))
}

fn lower_tool_choice(tool_choice: &ToolChoice, custom: &BTreeMap<String, Custom>) -> Result<Value, AiError> {
    match tool_choice.r#type {
        ToolChoiceType::Auto => return Ok(Value::String("auto".to_string())),
        ToolChoiceType::None => return Ok(Value::String("none".to_string())),
        ToolChoiceType::Required => return Ok(Value::String("required".to_string())),
        ToolChoiceType::Tool => {}
    }
    let name = tool_choice
        .name
        .as_deref()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| shared::invalid_request(format!("{ADAPTER} tool choice requires a tool name")))?;
    let mut object = Map::new();
    if custom.contains_key(name) {
        object.insert("type".to_string(), Value::String("custom".to_string()));
        let mut nested = Map::new();
        nested.insert("name".to_string(), Value::String(name.to_string()));
        object.insert("custom".to_string(), Value::Object(nested));
        return Ok(Value::Object(object));
    }
    object.insert("type".to_string(), Value::String("function".to_string()));
    let mut nested = Map::new();
    nested.insert("name".to_string(), Value::String(name.to_string()));
    object.insert("function".to_string(), Value::Object(nested));
    Ok(Value::Object(object))
}

fn lower_media(part: &MediaPart) -> Result<Value, AiError> {
    let media_type = shared::media_type(&part.media);
    if media_type.to_lowercase() == "application/pdf" {
        let inline = shared::require_inline_media(ADAPTER, &part.media)?;
        let filename = part.filename.clone().unwrap_or_else(|| "document.pdf".to_string());
        let mut file = Map::new();
        file.insert("filename".to_string(), Value::String(filename));
        file.insert("file_data".to_string(), Value::String(inline.data_url));
        let mut object = Map::new();
        object.insert("type".to_string(), Value::String("file".to_string()));
        object.insert("file".to_string(), Value::Object(file));
        return Ok(Value::Object(object));
    }
    let kind = shared::media_kind(&media_type);
    if kind != "image" && kind != "video" {
        return Err(shared::invalid_request(format!("{ADAPTER} does not support media type {media_type}")));
    }
    let url = match shared::media_url(&part.media) {
        Some(url) => url.to_string(),
        None => shared::require_inline_media(ADAPTER, &part.media)?.data_url,
    };
    let mut nested = Map::new();
    nested.insert("url".to_string(), Value::String(url));
    let mut object = Map::new();
    if kind == "video" {
        object.insert("type".to_string(), Value::String("video_url".to_string()));
        object.insert("video_url".to_string(), Value::Object(nested));
        return Ok(Value::Object(object));
    }
    object.insert("type".to_string(), Value::String("image_url".to_string()));
    object.insert("image_url".to_string(), Value::Object(nested));
    Ok(Value::Object(object))
}

fn lower_user_message(message: &UserMessage, lowering: &mut Lowering<'_>) -> Result<Value, AiError> {
    let mut content = Vec::new();
    for part in &message.content {
        match part {
            ContentPart::Text(part) => {
                content.push(text_content(&part.text, lowering.cache(part.cache.as_ref())));
            }
            ContentPart::Media(part) => content.push(lower_media(part)?),
            _ => {}
        }
    }
    let joinable = content.iter().all(|part| {
        part.get("type").and_then(Value::as_str) == Some("text")
            && !part.as_object().is_some_and(|object| object.contains_key("cache_control"))
    });
    if joinable {
        let text =
            content.iter().map(|part| part.get("text").and_then(Value::as_str).unwrap_or_default()).collect::<String>();
        return Ok(message_value("user", Value::String(text)));
    }
    Ok(message_value("user", Value::Array(content)))
}

fn lower_assistant_message(message: &AssistantMessage, lowering: &mut Lowering<'_>) -> Result<Value, AiError> {
    let mut content = Vec::new();
    let mut reasoning: Vec<&ReasoningPart> = Vec::new();
    let mut tool_calls = Vec::new();
    for part in &message.content {
        match part {
            ContentPart::Text(part) => {
                content.push(text_content(&part.text, lowering.cache(part.cache.as_ref())));
            }
            ContentPart::Reasoning(part) => reasoning.push(part),
            ContentPart::ToolCall(part) => {
                let custom = lowering.custom.get(&part.name).cloned();
                tool_calls.push(lower_tool_call(
                    part,
                    &lowering.provider_metadata_key,
                    &lowering.tool_call_id,
                    custom.as_ref(),
                ));
            }
            _ => {
                return Err(shared::invalid_request(format!(
                    "{ADAPTER} assistant messages only support text, reasoning, and tool-call content for now"
                )));
            }
        }
    }
    let text = reasoning.iter().map(|part| part.text.as_str()).collect::<String>();
    let details = reasoning_details(&reasoning, &lowering.provider_metadata_key);
    let observed_field = reasoning.iter().find_map(|part| reasoning_field(part, &lowering.provider_metadata_key));
    let fully_structured = reasoning.iter().all(|part| {
        shared::provider_metadata_value(part.provider_metadata.as_ref(), &lowering.provider_metadata_key)
            .and_then(|value| value.get("reasoningDetails"))
            .is_some_and(Value::is_array)
    });
    let field = if let Some(configured) = lowering.reasoning_field.as_deref()
        && (lowering.require_reasoning || !reasoning.is_empty())
    {
        Some(configured.to_string())
    } else if reasoning.is_empty() {
        lowering.require_reasoning.then(|| "reasoning_content".to_string())
    } else if let Some(observed) = observed_field {
        Some(observed)
    } else if !fully_structured
        || lowering.require_reasoning
        || details.as_ref().is_some_and(|details| details.iter().any(is_kimi_detail))
    {
        Some("reasoning_content".to_string())
    } else {
        None
    };
    let opaque = reasoning_opaque(&reasoning, &lowering.provider_metadata_key);
    let reasoning_text = if reasoning.is_empty() || (opaque.is_some() && text.is_empty()) {
        lowering.require_reasoning.then(String::new)
    } else {
        Some(text)
    };
    let replays_structured =
        lowering.reasoning_field.is_none() || lowering.reasoning_field.as_deref() == Some("reasoning_details");
    let content = if content
        .iter()
        .any(|part| part.as_object().is_some_and(|object| object.contains_key("cache_control")))
    {
        Value::Array(content)
    } else if content.is_empty() && !tool_calls.is_empty() {
        Value::Null
    } else {
        Value::String(
            content.iter().map(|part| part.get("text").and_then(Value::as_str).unwrap_or_default()).collect::<String>(),
        )
    };
    let mut result = Map::new();
    result.insert("role".to_string(), Value::String("assistant".to_string()));
    result.insert("content".to_string(), content);
    if !tool_calls.is_empty() {
        result.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }
    if let Some(details) = details
        && replays_structured
    {
        result.insert("reasoning_details".to_string(), Value::Array(details));
    }
    if let Some(opaque) = opaque
        && replays_structured
    {
        result.insert("reasoning_opaque".to_string(), Value::String(opaque));
    }
    if let Some(field) = field
        && let Some(reasoning_text) = reasoning_text
        && field != "reasoning_details"
    {
        result.insert(field, Value::String(reasoning_text));
    }
    Ok(Value::Object(result))
}

fn lower_tool_messages(
    message: &ToolMessage,
    lowering: &mut Lowering<'_>,
) -> Result<(Vec<Value>, Vec<Value>), AiError> {
    let mut messages = Vec::new();
    let mut attachments = Vec::new();
    for part in &message.content {
        let ContentPart::ToolResult(part) = part else {
            continue;
        };
        let id = lowering.tool_call_id.apply(&part.id);
        let cache_control = lowering.cache(part.cache.as_ref());
        let ToolResultValue::Content { value } = &part.result else {
            messages.push(tool_message(&id, shared::tool_result_text(part), cache_control));
            continue;
        };
        let text = value
            .iter()
            .filter_map(|item| match item {
                ToolContent::Text { text } => Some(text.as_str()),
                ToolContent::File { .. } => None,
            })
            .collect::<Vec<&str>>()
            .join("\n");
        messages.push(tool_message(&id, text, cache_control));
        for item in value {
            if matches!(item, ToolContent::File { .. })
                && let Some(media) = shared::tool_file_media(item)
            {
                attachments.push(lower_media(&media)?);
            }
        }
    }
    Ok((messages, attachments))
}

fn lower_messages(request: &LlmRequest, lowering: &mut Lowering<'_>) -> Result<Vec<Value>, AiError> {
    let mut messages = Vec::new();
    let cached_system = request.system.iter().any(|part| match part {
        SystemPart::Text { cache, .. } => cache.is_some(),
    });
    if !request.system.is_empty() {
        if cached_system && lowering.cache_control.is_some() {
            let content = request
                .system
                .iter()
                .map(|part| {
                    let SystemPart::Text { text, cache, .. } = part;
                    text_content(text, lowering.cache(cache.as_ref()))
                })
                .collect::<Vec<Value>>();
            messages.push(message_value("system", Value::Array(content)));
        } else {
            let text = shared::join_text(request.system.iter().map(|part| match part {
                SystemPart::Text { text, .. } => text.clone(),
            }));
            messages.push(message_value("system", Value::String(text)));
        }
    }
    let mut pending_attachments: Vec<Value> = Vec::new();
    for message in &request.messages {
        if matches!(message, Message::User(_)) {
            bridge_tools(&mut messages, lowering.require_assistant_after_tool);
        }
        if let Message::System(_) = message {
            let part = shared::wrapped_system_update(ADAPTER, message)?;
            let cache_control = lowering.cache(part.cache.as_ref());
            if !pending_attachments.is_empty() {
                let mut content = std::mem::take(&mut pending_attachments);
                content.push(text_content(&part.text, cache_control));
                messages.push(message_value("user", Value::Array(content)));
                continue;
            }
            let previous = messages.last_mut();
            match previous
                .and_then(|previous| (previous.get("role").and_then(Value::as_str) == Some("user")).then_some(previous))
            {
                Some(previous) => {
                    let replaced = match previous.get("content") {
                        Some(Value::String(text)) => match cache_control {
                            Some(cache_control) => message_value(
                                "user",
                                Value::Array(vec![
                                    text_content(text, None),
                                    text_content(&part.text, Some(cache_control)),
                                ]),
                            ),
                            None => message_value("user", Value::String(format!("{text}\n{}", part.text))),
                        },
                        Some(Value::Array(content)) => {
                            let mut content = content.clone();
                            content.push(text_content(&part.text, cache_control));
                            message_value("user", Value::Array(content))
                        }
                        _ => message_value("user", Value::String(part.text.clone())),
                    };
                    *previous = replaced;
                }
                None => messages.push(match cache_control {
                    Some(cache_control) => {
                        message_value("user", Value::Array(vec![text_content(&part.text, Some(cache_control))]))
                    }
                    None => message_value("user", Value::String(part.text.clone())),
                }),
            }
            continue;
        }
        if let Message::Assistant(assistant) = message
            && assistant
                .content
                .iter()
                .all(|part| matches!(part, ContentPart::Text(text) if text.text.trim().is_empty()))
        {
            continue;
        }
        if let Message::Tool(message) = message {
            let (lowered, attachments) = lower_tool_messages(message, lowering)?;
            messages.extend(lowered);
            pending_attachments.extend(attachments);
            continue;
        }
        flush_attachments(&mut messages, &mut pending_attachments, lowering.require_assistant_after_tool);
        match message {
            Message::User(message) => messages.push(lower_user_message(message, lowering)?),
            Message::Assistant(message) => messages.push(lower_assistant_message(message, lowering)?),
            Message::System(_) | Message::Tool(_) => {}
        }
    }
    flush_attachments(&mut messages, &mut pending_attachments, lowering.require_assistant_after_tool);
    Ok(messages)
}

fn bridge_tools(messages: &mut Vec<Value>, require_assistant_after_tool: bool) {
    let trailing_tool = messages.last().and_then(|message| message.get("role")).and_then(Value::as_str) == Some("tool");
    if require_assistant_after_tool && trailing_tool {
        messages.push(message_value("assistant", Value::String("Done.".to_string())));
    }
}

fn flush_attachments(messages: &mut Vec<Value>, pending: &mut Vec<Value>, require_assistant_after_tool: bool) {
    if pending.is_empty() {
        return;
    }
    bridge_tools(messages, require_assistant_after_tool);
    messages.push(message_value("user", Value::Array(std::mem::take(pending))));
}

fn lower_options(
    model: &Model,
    request: &LlmRequest,
    supports_store: bool,
    has_active_tools: bool,
) -> Map<String, Value> {
    let options = open_responses_options::resolve(request);
    let parallel_tool_calls =
        if has_active_tools { open_responses_options::resolve_parallel_tool_calls(request, &options) } else { None };
    let cache_key = if capability_bool(model, &["promptCacheKey"]).unwrap_or(false) {
        shared::prompt_cache_key(request, model)
    } else {
        None
    };
    let mut output = Map::new();
    if supports_store {
        output.insert("store".to_string(), Value::Bool(options.store.unwrap_or(false)));
    }
    if let Some(cache_key) = cache_key.filter(|cache_key| !cache_key.is_empty()) {
        output.insert("prompt_cache_key".to_string(), Value::String(cache_key));
    }
    if !crate::prepare::places_markers(model, request) {
        for (key, value) in cache::openai_request_ttl(crate::prepare::request_ttl(request)) {
            output.insert(key, value);
        }
    }
    if let Some(reasoning_effort) = options.reasoning_effort.filter(|value| !value.is_empty()) {
        output.insert("reasoning_effort".to_string(), Value::String(reasoning_effort));
    }
    if let Some(verbosity) = options.text_verbosity.filter(|value| !value.is_empty()) {
        output.insert("verbosity".to_string(), Value::String(verbosity));
    }
    if let Some(service_tier) = options.service_tier.filter(|value| !value.is_empty()) {
        output.insert("service_tier".to_string(), Value::String(service_tier));
    }
    if let Some(parallel_tool_calls) = parallel_tool_calls {
        output.insert("parallel_tool_calls".to_string(), Value::Bool(parallel_tool_calls));
    }
    if let Some(metadata) = options.metadata {
        output.insert("metadata".to_string(), Value::Object(metadata));
    }
    if let Some(safety_identifier) = options.safety_identifier.filter(|value| !value.is_empty()) {
        output.insert("safety_identifier".to_string(), Value::String(safety_identifier));
    }
    output
}

fn lower_tool_call(
    part: &ToolCallPart,
    provider_metadata_key: &str,
    tool_call_id: &ToolCallId,
    custom: Option<&Custom>,
) -> Value {
    let id = tool_call_id.apply(&part.id);
    let mut object = Map::new();
    object.insert("id".to_string(), Value::String(id));
    if let Some(custom) = custom {
        object.insert("type".to_string(), Value::String("custom".to_string()));
        let mut nested = Map::new();
        nested.insert("name".to_string(), Value::String(part.name.clone()));
        nested.insert("input".to_string(), Value::String(openai_freeform::to_raw(custom, &part.input)));
        object.insert("custom".to_string(), Value::Object(nested));
        return Value::Object(object);
    }
    object.insert("type".to_string(), Value::String("function".to_string()));
    let mut function = Map::new();
    function.insert("name".to_string(), Value::String(part.name.clone()));
    function.insert("arguments".to_string(), Value::String(crate::json::encode_body(&part.input)));
    object.insert("function".to_string(), Value::Object(function));
    if let Some(extra_content) =
        decode_extra_content(shared::provider_metadata_value(part.provider_metadata.as_ref(), provider_metadata_key))
    {
        object.insert("extra_content".to_string(), extra_content);
    }
    Value::Object(object)
}

fn decode_extra_content(metadata: Option<&Value>) -> Option<Value> {
    let extra_content = metadata?.get("extraContent")?;
    extra_content.get("google")?.get("thought_signature")?.as_str()?;
    Some(extra_content.clone())
}

fn reasoning_field(part: &ReasoningPart, provider_metadata_key: &str) -> Option<String> {
    shared::provider_metadata_value(part.provider_metadata.as_ref(), provider_metadata_key)
        .and_then(|metadata| metadata.get("reasoningField"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn reasoning_details(parts: &[&ReasoningPart], provider_metadata_key: &str) -> Option<Vec<Value>> {
    let mut observed = Vec::new();
    let mut declared = false;
    for part in parts {
        let Some(details) = shared::provider_metadata_value(part.provider_metadata.as_ref(), provider_metadata_key)
            .and_then(|metadata| metadata.get("reasoningDetails"))
        else {
            continue;
        };
        let Some(details) = details.as_array() else {
            continue;
        };
        declared = true;
        observed.extend(details.iter().cloned());
    }
    if !declared {
        return None;
    }
    Some(known_reasoning_details(&observed).into_iter().map(lower_reasoning_detail).collect())
}

fn reasoning_opaque(parts: &[&ReasoningPart], provider_metadata_key: &str) -> Option<String> {
    parts.iter().rev().find_map(|part| {
        shared::provider_metadata_value(part.provider_metadata.as_ref(), provider_metadata_key)
            .and_then(|metadata| metadata.get("reasoningOpaque"))
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

fn lower_reasoning_detail(detail: Value) -> Value {
    match detail.get("type").and_then(Value::as_str) {
        Some("summary") => {
            let mut object = Map::new();
            object.insert("type".to_string(), Value::String("summary".to_string()));
            object.insert("summary".to_string(), detail.get("summary").cloned().unwrap_or(Value::Null));
            Value::Object(object)
        }
        Some("encrypted") => {
            let mut object = Map::new();
            object.insert("type".to_string(), Value::String("encrypted".to_string()));
            object.insert("encrypted".to_string(), detail.get("encrypted").cloned().unwrap_or(Value::Null));
            Value::Object(object)
        }
        _ => detail,
    }
}

fn known_reasoning_details(details: &[Value]) -> Vec<Value> {
    details.iter().filter_map(known_reasoning_detail).collect()
}

fn known_reasoning_detail(detail: &Value) -> Option<Value> {
    let object = detail.as_object()?;
    match object.get("type")?.as_str()? {
        "reasoning.text" => {
            optional_string(object, "text")?;
            openrouter_detail_fields(object)?;
            Some(detail.clone())
        }
        "reasoning.summary" => {
            optional_string(object, "summary")?;
            openrouter_detail_fields(object)?;
            Some(detail.clone())
        }
        "reasoning.encrypted" => {
            object.get("data")?.as_str()?;
            openrouter_detail_fields(object)?;
            Some(detail.clone())
        }
        "summary" => {
            let summary = object.get("summary")?.as_str()?;
            optional_number(object, "index")?;
            let mut known = Map::new();
            known.insert("type".to_string(), Value::String("summary".to_string()));
            known.insert("summary".to_string(), Value::String(summary.to_string()));
            if let Some(index) = object.get("index") {
                known.insert("index".to_string(), index.clone());
            }
            Some(Value::Object(known))
        }
        "encrypted" => {
            let encrypted = object.get("encrypted")?.as_str()?;
            optional_number(object, "index")?;
            let mut known = Map::new();
            known.insert("type".to_string(), Value::String("encrypted".to_string()));
            known.insert("encrypted".to_string(), Value::String(encrypted.to_string()));
            if let Some(index) = object.get("index") {
                known.insert("index".to_string(), index.clone());
            }
            Some(Value::Object(known))
        }
        _ => None,
    }
}

fn openrouter_detail_fields(object: &Map<String, Value>) -> Option<()> {
    optional_nullable_string(object, "id")?;
    optional_string(object, "format")?;
    optional_number(object, "index")?;
    optional_nullable_string(object, "signature")
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Option<()> {
    match object.get(key) {
        None | Some(Value::String(_)) => Some(()),
        Some(_) => None,
    }
}

fn optional_nullable_string(object: &Map<String, Value>, key: &str) -> Option<()> {
    match object.get(key) {
        None | Some(Value::Null) | Some(Value::String(_)) => Some(()),
        Some(_) => None,
    }
}

fn optional_number(object: &Map<String, Value>, key: &str) -> Option<()> {
    match object.get(key) {
        None | Some(Value::Number(_)) => Some(()),
        Some(_) => None,
    }
}

fn is_kimi_detail(detail: &Value) -> bool {
    matches!(detail.get("type").and_then(Value::as_str), Some("summary") | Some("encrypted"))
}

fn text_content(text: &str, cache_control: Option<Value>) -> Value {
    let mut object = Map::new();
    object.insert("type".to_string(), Value::String("text".to_string()));
    object.insert("text".to_string(), Value::String(text.to_string()));
    if let Some(cache_control) = cache_control {
        object.insert("cache_control".to_string(), cache_control);
    }
    Value::Object(object)
}

fn tool_message(tool_call_id: &str, text: String, cache_control: Option<Value>) -> Value {
    let content = match cache_control {
        Some(cache_control) => Value::Array(vec![text_content(&text, Some(cache_control))]),
        None => Value::String(text),
    };
    let mut object = Map::new();
    object.insert("role".to_string(), Value::String("tool".to_string()));
    object.insert("tool_call_id".to_string(), Value::String(tool_call_id.to_string()));
    object.insert("content".to_string(), content);
    Value::Object(object)
}

fn message_value(role: &str, content: Value) -> Value {
    let mut object = Map::new();
    object.insert("role".to_string(), Value::String(role.to_string()));
    object.insert("content".to_string(), content);
    Value::Object(object)
}

fn has_tool_history(messages: &[Message]) -> bool {
    messages.iter().any(|message| match message {
        Message::Tool(_) => true,
        Message::Assistant(message) => message.content.iter().any(|part| matches!(part, ContentPart::ToolCall(_))),
        _ => false,
    })
}

fn compatibility_value<'a>(model: &'a Model, key: &str) -> Option<&'a Value> {
    let value = model.compatibility.as_ref()?.get(key)?;
    (!value.is_null()).then_some(value)
}

fn compatibility_bool(model: &Model, key: &str) -> Option<bool> {
    compatibility_value(model, key).map(shared::is_truthy)
}

fn capability_value<'a>(model: &'a Model, path: &[&str]) -> Option<&'a Value> {
    let mut value = model.capabilities.as_ref()?;
    for key in path {
        value = value.get(*key)?;
    }
    (!value.is_null()).then_some(value)
}

fn capability_bool(model: &Model, path: &[&str]) -> Option<bool> {
    capability_value(model, path).map(shared::is_truthy)
}

fn detect_max_tokens_field(provider: &str, base_url: Option<&str>) -> &'static str {
    let provider = provider.to_lowercase();
    let url = base_url.unwrap_or_default().to_lowercase();
    let legacy = provider == "deepseek"
        || url.contains("deepseek.com")
        || provider == "moonshotai"
        || url.contains("api.moonshot.ai")
        || provider == "togetherai"
        || url.contains("api.together.")
        || provider == "zai"
        || provider == "zai-coding-plan"
        || provider == "zhipuai"
        || provider == "zhipuai-coding-plan"
        || url.contains("api.z.ai")
        || url.contains("open.bigmodel.cn")
        || provider == "nvidia"
        || url.contains("integrate.api.nvidia.com")
        || provider == "cerebras"
        || url.contains("cerebras.ai")
        || url.contains("llm.chutes.ai")
        || provider == "chutes"
        || provider == "cloudflare-ai-gateway"
        || url.contains("gateway.ai.cloudflare.com")
        || provider == "cloudflare-workers-ai"
        || url.contains("api.cloudflare.com");
    if legacy { "max_tokens" } else { "max_completion_tokens" }
}

fn detect_supports_store(provider: &str, base_url: Option<&str>) -> bool {
    let provider = provider.to_lowercase();
    let url = base_url.unwrap_or_default().to_lowercase();
    provider == "openai"
        || provider == "azure"
        || url.contains("api.openai.com")
        || url.contains(".openai.azure.com")
        || url.contains(".cognitiveservices.azure.com")
        || url.contains(".services.ai.azure.com")
}

fn detect_supports_strict_mode(provider: &str, base_url: Option<&str>) -> bool {
    let provider = provider.to_lowercase();
    let url = base_url.unwrap_or_default().to_lowercase();
    let moonshot = provider == "moonshotai" || provider == "moonshotai-cn" || url.contains("api.moonshot.");
    let together = provider == "togetherai" || provider == "together" || url.contains("api.together.");
    let cloudflare = provider == "cloudflare-ai-gateway" || url.contains("gateway.ai.cloudflare.com");
    let nvidia = provider == "nvidia" || url.contains("integrate.api.nvidia.com");
    !moonshot && !together && !cloudflare && !nvidia
}

fn detect_zai_tool_stream(provider: &str, base_url: Option<&str>, model_id: &str) -> bool {
    let provider = provider.to_lowercase();
    let url = base_url.unwrap_or_default().to_lowercase();
    let is_zai = provider == "zai"
        || provider == "zai-coding-plan"
        || provider == "zhipuai"
        || provider == "zhipuai-coding-plan"
        || url.contains("api.z.ai")
        || url.contains("open.bigmodel.cn");
    if !is_zai {
        return false;
    }
    let id = model_id.to_lowercase();
    !matches!(id.as_str(), "glm-4.5" | "glm-4.5-air" | "glm-4.5-flash" | "glm-4.5v")
}
