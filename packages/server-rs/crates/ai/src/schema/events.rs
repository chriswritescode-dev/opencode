use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ids::{ContentBlockId, ToolCallId};
use super::messages::{
    AssistantMessage, CompactionPart, ContentPart, MediaAsset, MediaPart, ReasoningPart, TextPart, ToolCallPart,
    ToolOutput, ToolResultPart, ToolResultValue,
};
use super::options::ProviderMetadata;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Error,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishReasonDetails {
    pub normalized: FinishReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub non_cached_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<ProviderMetadata>,
}

impl Usage {
    pub fn visible_output_tokens(&self) -> u64 {
        self.output_tokens.unwrap_or(0).saturating_sub(self.reasoning_tokens.unwrap_or(0))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum LlmEvent {
    Compaction(CompactionPart),
    #[serde(rename_all = "camelCase")]
    StepStart {
        index: u64,
    },
    #[serde(rename_all = "camelCase")]
    TextStart {
        id: ContentBlockId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
    #[serde(rename_all = "camelCase")]
    TextDelta {
        id: ContentBlockId,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
    #[serde(rename_all = "camelCase")]
    TextEnd {
        id: ContentBlockId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
    #[serde(rename_all = "camelCase")]
    ReasoningStart {
        id: ContentBlockId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
    #[serde(rename_all = "camelCase")]
    ReasoningDelta {
        id: ContentBlockId,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
    #[serde(rename_all = "camelCase")]
    ReasoningEnd {
        id: ContentBlockId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
    #[serde(rename_all = "camelCase")]
    ToolInputStart {
        id: ToolCallId,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_executed: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
    #[serde(rename_all = "camelCase")]
    ToolInputDelta {
        id: ToolCallId,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<Value>,
    },
    #[serde(rename_all = "camelCase")]
    ToolInputEnd {
        id: ToolCallId,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
    #[serde(rename_all = "camelCase")]
    ToolInputError {
        id: ToolCallId,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        raw: String,
    },
    #[serde(rename_all = "camelCase")]
    ToolCall {
        id: ToolCallId,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        input: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_executed: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
    #[serde(rename_all = "camelCase")]
    ToolResult {
        id: ToolCallId,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        result: ToolResultValue,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<ToolOutput>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_executed: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
    #[serde(rename_all = "camelCase")]
    Media {
        media: MediaAsset,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
    #[serde(rename_all = "camelCase")]
    StepFinish {
        index: u64,
        reason: FinishReasonDetails,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
    #[serde(rename_all = "camelCase")]
    Finish {
        reason: FinishReasonDetails,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<AssistantMessage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_metadata: Option<ProviderMetadata>,
    },
    #[serde(rename_all = "camelCase")]
    Raw {
        data: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmResponse {
    pub message: AssistantMessage,
    pub events: Vec<LlmEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReasonDetails>,
}

impl LlmResponse {
    pub fn text(&self) -> String {
        join_fragments(&self.events, Fragment::Text)
    }

    pub fn reasoning(&self) -> String {
        join_fragments(&self.events, Fragment::Reasoning)
    }

    pub fn tool_calls(&self) -> Vec<&LlmEvent> {
        self.events.iter().filter(|event| matches!(event, LlmEvent::ToolCall { .. })).collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResponseFold {
    events: Vec<LlmEvent>,
    content: Vec<ContentPart>,
    usage: Option<Usage>,
    finish_reason: Option<FinishReasonDetails>,
    text_parts: HashMap<ContentBlockId, ContentAssembly>,
    reasoning_parts: HashMap<ContentBlockId, ContentAssembly>,
    tool_inputs: HashMap<ToolCallId, ToolInputAssembly>,
}

impl ResponseFold {
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            content: Vec::new(),
            usage: None,
            finish_reason: None,
            text_parts: HashMap::new(),
            reasoning_parts: HashMap::new(),
            tool_inputs: HashMap::new(),
        }
    }

    pub fn push(&mut self, event: LlmEvent) {
        if matches!(event, LlmEvent::Raw { .. }) {
            return;
        }
        self.apply_usage(&event);
        self.apply_content(&event);
        self.events.push(event);
    }

    pub fn finish(self) -> LlmResponse {
        LlmResponse {
            message: AssistantMessage { content: self.content, ..Default::default() },
            events: self.events,
            usage: self.usage,
            finish_reason: self.finish_reason,
        }
    }

    fn apply_usage(&mut self, event: &LlmEvent) {
        match event {
            LlmEvent::StepFinish { usage: Some(usage), .. } => self.usage = Some(usage.clone()),
            LlmEvent::Finish { reason, usage, .. } => {
                if let Some(usage) = usage {
                    self.usage = Some(usage.clone());
                }
                self.finish_reason = Some(reason.clone());
            }
            _ => {}
        }
    }

    fn apply_content(&mut self, event: &LlmEvent) {
        match event {
            LlmEvent::Compaction(part) => self.content.push(ContentPart::Compaction(part.clone())),
            LlmEvent::TextStart { id, provider_metadata } => {
                self.ensure_content(Fragment::Text, id, provider_metadata.clone())
            }
            LlmEvent::TextDelta { id, text, provider_metadata } => {
                self.apply_delta(Fragment::Text, id, text, provider_metadata.clone())
            }
            LlmEvent::TextEnd { id, text, provider_metadata } => {
                self.apply_end(Fragment::Text, id, text.as_deref(), provider_metadata.clone())
            }
            LlmEvent::ReasoningStart { id, provider_metadata } => {
                self.ensure_content(Fragment::Reasoning, id, provider_metadata.clone())
            }
            LlmEvent::ReasoningDelta { id, text, provider_metadata } => {
                self.apply_delta(Fragment::Reasoning, id, text, provider_metadata.clone())
            }
            LlmEvent::ReasoningEnd { id, text, provider_metadata } => {
                self.apply_end(Fragment::Reasoning, id, text.as_deref(), provider_metadata.clone())
            }
            LlmEvent::ToolInputStart { id, name, namespace, provider_metadata, .. } => {
                self.tool_inputs.insert(
                    id.clone(),
                    ToolInputAssembly {
                        name: name.clone(),
                        namespace: namespace.clone(),
                        text: String::new(),
                        provider_metadata: provider_metadata.clone(),
                    },
                );
            }
            LlmEvent::ToolInputDelta { id, name, namespace, text, .. } => {
                let input = self.tool_inputs.entry(id.clone()).or_insert_with(|| ToolInputAssembly {
                    name: name.clone(),
                    namespace: namespace.clone(),
                    text: String::new(),
                    provider_metadata: None,
                });
                input.text.push_str(text);
            }
            LlmEvent::ToolInputEnd { id, name, namespace, provider_metadata, .. } => {
                let input = self.tool_inputs.entry(id.clone()).or_insert_with(|| ToolInputAssembly {
                    name: name.clone(),
                    namespace: namespace.clone(),
                    text: String::new(),
                    provider_metadata: None,
                });
                input.name = name.clone();
                input.namespace = namespace.clone();
                if provider_metadata.is_some() {
                    input.provider_metadata = provider_metadata.clone();
                }
            }
            LlmEvent::ToolInputError { id, .. } => {
                self.tool_inputs.remove(id);
            }
            LlmEvent::ToolCall { id, name, namespace, input, provider_executed, provider_metadata } => {
                self.tool_inputs.remove(id);
                self.content.push(ContentPart::ToolCall(ToolCallPart {
                    id: id.clone(),
                    name: name.clone(),
                    namespace: namespace.clone(),
                    input: input.clone(),
                    provider_executed: *provider_executed,
                    cache: None,
                    provider_metadata: provider_metadata.clone(),
                }));
            }
            LlmEvent::ToolResult { id, name, namespace, result, provider_executed, provider_metadata, .. } => {
                self.content.push(ContentPart::ToolResult(ToolResultPart {
                    id: id.clone(),
                    name: name.clone(),
                    namespace: namespace.clone(),
                    result: result.clone(),
                    provider_executed: *provider_executed,
                    cache: None,
                    provider_metadata: provider_metadata.clone(),
                }));
            }
            LlmEvent::Media { media, provider_metadata } => {
                self.content.push(ContentPart::Media(MediaPart {
                    media: media.clone(),
                    filename: None,
                    cache: None,
                    provider_metadata: provider_metadata.clone(),
                }));
            }
            LlmEvent::StepStart { .. }
            | LlmEvent::StepFinish { .. }
            | LlmEvent::Finish { .. }
            | LlmEvent::Raw { .. } => {}
        }
    }

    fn ensure_content(&mut self, fragment: Fragment, id: &ContentBlockId, provider_metadata: Option<ProviderMetadata>) {
        let parts = match fragment {
            Fragment::Text => &mut self.text_parts,
            Fragment::Reasoning => &mut self.reasoning_parts,
        };
        if parts.contains_key(id) {
            return;
        }
        parts.insert(
            id.clone(),
            ContentAssembly {
                content_index: self.content.len(),
                text: String::new(),
                provider_metadata: provider_metadata.clone(),
            },
        );
        self.content.push(content_part(fragment, String::new(), provider_metadata));
    }

    fn apply_delta(
        &mut self,
        fragment: Fragment,
        id: &ContentBlockId,
        text: &str,
        provider_metadata: Option<ProviderMetadata>,
    ) {
        self.ensure_content(fragment, id, provider_metadata.clone());
        let parts = match fragment {
            Fragment::Text => &mut self.text_parts,
            Fragment::Reasoning => &mut self.reasoning_parts,
        };
        let Some(current) = parts.get_mut(id) else {
            return;
        };
        current.text.push_str(text);
        if provider_metadata.is_some() {
            current.provider_metadata = provider_metadata;
        }
        let content_index = current.content_index;
        let text = current.text.clone();
        let provider_metadata = current.provider_metadata.clone();
        self.content[content_index] = content_part(fragment, text, provider_metadata);
    }

    fn apply_end(
        &mut self,
        fragment: Fragment,
        id: &ContentBlockId,
        text: Option<&str>,
        provider_metadata: Option<ProviderMetadata>,
    ) {
        let parts = match fragment {
            Fragment::Text => &mut self.text_parts,
            Fragment::Reasoning => &mut self.reasoning_parts,
        };
        let Some(current) = parts.get_mut(id) else {
            return;
        };
        if let Some(text) = text {
            current.text = text.to_string();
        }
        if provider_metadata.is_some() {
            current.provider_metadata = provider_metadata;
        }
        let content_index = current.content_index;
        let text = current.text.clone();
        let provider_metadata = current.provider_metadata.clone();
        self.content[content_index] = content_part(fragment, text, provider_metadata);
    }
}

impl Default for ResponseFold {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Fragment {
    Text,
    Reasoning,
}

#[derive(Debug, Clone, PartialEq)]
struct ContentAssembly {
    content_index: usize,
    text: String,
    provider_metadata: Option<ProviderMetadata>,
}

#[derive(Debug, Clone, PartialEq)]
struct ToolInputAssembly {
    name: String,
    namespace: Option<String>,
    text: String,
    provider_metadata: Option<ProviderMetadata>,
}

fn content_part(fragment: Fragment, text: String, provider_metadata: Option<ProviderMetadata>) -> ContentPart {
    match fragment {
        Fragment::Text => ContentPart::Text(TextPart { text, cache: None, provider_metadata }),
        Fragment::Reasoning => {
            ContentPart::Reasoning(ReasoningPart { text, encrypted: None, cache: None, provider_metadata })
        }
    }
}

fn fragment_of(event: &LlmEvent, fragment: Fragment) -> Option<(&str, &str, bool)> {
    match (fragment, event) {
        (Fragment::Text, LlmEvent::TextDelta { id, text, .. }) => Some((id.as_str(), text.as_str(), false)),
        (Fragment::Text, LlmEvent::TextEnd { id, text: Some(text), .. }) => Some((id.as_str(), text.as_str(), true)),
        (Fragment::Reasoning, LlmEvent::ReasoningDelta { id, text, .. }) => Some((id.as_str(), text.as_str(), false)),
        (Fragment::Reasoning, LlmEvent::ReasoningEnd { id, text: Some(text), .. }) => {
            Some((id.as_str(), text.as_str(), true))
        }
        _ => None,
    }
}

fn join_fragments(events: &[LlmEvent], fragment: Fragment) -> String {
    let mut order: Vec<&str> = Vec::new();
    let mut parts: HashMap<&str, String> = HashMap::new();
    for event in events {
        let Some((id, text, replaces)) = fragment_of(event, fragment) else {
            continue;
        };
        if replaces {
            if !parts.contains_key(id) {
                order.push(id);
            }
            parts.insert(id, text.to_string());
            continue;
        }
        parts
            .entry(id)
            .or_insert_with(|| {
                order.push(id);
                String::new()
            })
            .push_str(text);
    }
    order.into_iter().filter_map(|id| parts.get(id).cloned()).collect::<Vec<String>>().concat()
}
