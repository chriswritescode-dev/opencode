use std::sync::Arc;

use opencode_ai::{FinishReason, FinishReasonDetails, LlmEvent, ProviderMetadata, Usage};
use opencode_bus::{Bus, BusError, Definition, PublishOptions};
use opencode_contract::codec::{Encode, JsonMap, encode_json};
use opencode_contract::generated::{
    AgentID, LocationRef, ModelRef, SessionID, SessionMessageID, SessionMessageNative, SessionReasoningDeltaData,
    SessionReasoningEndedData, SessionReasoningStartedData, SessionStepEndedDataFinish, SessionStepFirstTokenData,
    SessionStepStartedData, SessionStepStreamedData, SessionTextDeltaData, SessionTextEndedData,
    SessionTextStartedData,
};
use serde_json::Value;

#[derive(Clone)]
pub struct Finish {
    pub normalized: SessionStepEndedDataFinish,
    pub raw: Option<String>,
    pub usage: Option<Usage>,
    pub native: Option<SessionMessageNative>,
}

pub struct Record {
    pub output_started: bool,
    pub provider_failed: bool,
    pub needs_continuation: bool,
    pub finish: Option<Finish>,
}

#[derive(Default)]
struct Fragments {
    block: Option<Block>,
    next_ordinal: f64,
}

struct Block {
    id: String,
    ordinal: f64,
    values: Vec<String>,
    pending: String,
}

#[derive(Clone, Copy)]
enum FragmentKind {
    Text,
    Reasoning,
}

impl FragmentKind {
    fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Reasoning => "reasoning",
        }
    }

    fn event_type(self, suffix: &str) -> String {
        format!("session.{}.{suffix}", self.name())
    }
}

pub struct Publisher {
    bus: Arc<Bus>,
    session_id: String,
    assistant_message_id: String,
    agent: String,
    model: ModelRef,
    location: Option<LocationRef>,
    started: f64,
    provider_metadata_key: String,
    step_started: bool,
    first_token_published: bool,
    output_started: bool,
    provider_failed: bool,
    text: Fragments,
    reasoning: Fragments,
    finish: Option<Finish>,
}

impl Publisher {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bus: Arc<Bus>,
        session_id: &str,
        assistant_message_id: &str,
        agent: &str,
        model: &ModelRef,
        location: Option<LocationRef>,
        started: i64,
        provider_metadata_key: &str,
    ) -> Self {
        Self {
            bus,
            session_id: session_id.to_string(),
            assistant_message_id: assistant_message_id.to_string(),
            agent: agent.to_string(),
            model: model.clone(),
            location,
            started: started as f64,
            provider_metadata_key: provider_metadata_key.to_string(),
            step_started: false,
            first_token_published: false,
            output_started: false,
            provider_failed: false,
            text: Fragments::default(),
            reasoning: Fragments::default(),
            finish: None,
        }
    }

    pub fn start(&mut self) -> Result<(), BusError> {
        if self.step_started {
            return Ok(());
        }
        self.step_started = true;
        self.publish_durable(
            "session.step.started",
            &SessionStepStartedData {
                session_id: SessionID(self.session_id.clone()),
                assistant_message_id: SessionMessageID(self.assistant_message_id.clone()),
                agent: AgentID(self.agent.clone()),
                model: self.model.clone(),
                snapshot: None,
                started: self.started,
            },
        )
    }

    pub fn streamed(&mut self) -> Result<(), BusError> {
        self.start()?;
        self.publish_durable(
            "session.step.streamed",
            &SessionStepStreamedData {
                session_id: SessionID(self.session_id.clone()),
                assistant_message_id: SessionMessageID(self.assistant_message_id.clone()),
            },
        )
    }

    pub fn publish(&mut self, event: &LlmEvent) -> Result<(), BusError> {
        if begins_output(event) {
            self.first_token()?;
        }
        match event {
            LlmEvent::StepStart { .. } => self.start(),
            LlmEvent::TextStart { id, .. } => {
                self.flush_pending_deltas()?;
                self.output_started = true;
                let ordinal = self.start_fragment(FragmentKind::Text, id)?;
                self.publish_fragment_started(FragmentKind::Text, ordinal, None)
            }
            LlmEvent::TextDelta { id, text, .. } => self.append_fragment(FragmentKind::Text, id, text),
            LlmEvent::TextEnd { id, text, provider_metadata, .. } => {
                self.end_fragment(FragmentKind::Text, id, text.as_deref(), self.native(provider_metadata.as_ref()))
            }
            LlmEvent::ReasoningStart { id, provider_metadata, .. } => {
                self.flush_pending_deltas()?;
                self.output_started = true;
                let ordinal = self.start_fragment(FragmentKind::Reasoning, id)?;
                self.publish_fragment_started(FragmentKind::Reasoning, ordinal, self.native(provider_metadata.as_ref()))
            }
            LlmEvent::ReasoningDelta { id, text, .. } => self.append_fragment(FragmentKind::Reasoning, id, text),
            LlmEvent::ReasoningEnd { id, text, provider_metadata, .. } => {
                self.end_fragment(FragmentKind::Reasoning, id, text.as_deref(), self.native(provider_metadata.as_ref()))
            }
            LlmEvent::StepFinish { reason, usage, provider_metadata, .. } => {
                self.step_finish(reason, usage.as_ref(), self.native(provider_metadata.as_ref()))
            }
            LlmEvent::Compaction(_)
            | LlmEvent::ToolInputStart { .. }
            | LlmEvent::ToolInputDelta { .. }
            | LlmEvent::ToolInputEnd { .. }
            | LlmEvent::ToolInputError { .. }
            | LlmEvent::ToolCall { .. }
            | LlmEvent::ToolResult { .. }
            | LlmEvent::Media { .. }
            | LlmEvent::Finish { .. }
            | LlmEvent::Raw { .. } => Ok(()),
        }
    }

    pub fn flush(&mut self) -> Result<(), BusError> {
        self.flush_pending_deltas()?;
        self.end_open_fragment(FragmentKind::Text)?;
        self.end_open_fragment(FragmentKind::Reasoning)
    }

    pub fn flush_pending_deltas(&mut self) -> Result<(), BusError> {
        self.flush_fragment_pending(FragmentKind::Text)?;
        self.flush_fragment_pending(FragmentKind::Reasoning)
    }

    pub fn has_pending_deltas(&self) -> bool {
        let pending = |fragments: &Fragments| fragments.block.as_ref().is_some_and(|block| !block.pending.is_empty());
        pending(&self.text) || pending(&self.reasoning)
    }

    pub fn record(&self) -> Record {
        Record {
            output_started: self.output_started,
            provider_failed: self.provider_failed,
            needs_continuation: false,
            finish: self.finish.clone(),
        }
    }

    fn first_token(&mut self) -> Result<(), BusError> {
        if self.first_token_published {
            return Ok(());
        }
        self.first_token_published = true;
        self.start()?;
        self.publish_durable(
            "session.step.first-token",
            &SessionStepFirstTokenData {
                session_id: SessionID(self.session_id.clone()),
                assistant_message_id: SessionMessageID(self.assistant_message_id.clone()),
            },
        )
    }

    fn step_finish(
        &mut self,
        reason: &FinishReasonDetails,
        usage: Option<&Usage>,
        native: Option<SessionMessageNative>,
    ) -> Result<(), BusError> {
        self.flush()?;
        if self.finish.is_some() {
            return Err(invalid_event("session.step.ended", "Duplicate step finish"));
        }
        let normalized = finish_kind(&reason.normalized);
        if normalized == SessionStepEndedDataFinish::ContentFilter {
            self.provider_failed = true;
        }
        self.finish = Some(Finish { normalized, raw: reason.raw.clone(), usage: usage.cloned(), native });
        Ok(())
    }

    fn fragment(&self, kind: FragmentKind) -> &Fragments {
        match kind {
            FragmentKind::Text => &self.text,
            FragmentKind::Reasoning => &self.reasoning,
        }
    }

    fn fragment_mut(&mut self, kind: FragmentKind) -> &mut Fragments {
        match kind {
            FragmentKind::Text => &mut self.text,
            FragmentKind::Reasoning => &mut self.reasoning,
        }
    }

    fn start_fragment(&mut self, kind: FragmentKind, id: &str) -> Result<f64, BusError> {
        let fragments = self.fragment_mut(kind);
        if fragments.block.is_some() {
            return Err(invalid_event(&kind.event_type("started"), &format!("{} start before end: {id}", kind.name())));
        }
        let ordinal = fragments.next_ordinal;
        fragments.next_ordinal += 1.0;
        fragments.block = Some(Block { id: id.to_string(), ordinal, values: Vec::new(), pending: String::new() });
        Ok(ordinal)
    }

    fn append_fragment(&mut self, kind: FragmentKind, id: &str, value: &str) -> Result<(), BusError> {
        let Some(block) = self.fragment_mut(kind).block.as_mut() else {
            return Err(invalid_event(&kind.event_type("delta"), &format!("{} delta before start: {id}", kind.name())));
        };
        if block.id != id {
            return Err(invalid_event(&kind.event_type("delta"), &format!("{} delta before start: {id}", kind.name())));
        }
        block.values.push(value.to_string());
        block.pending.push_str(value);
        Ok(())
    }

    fn end_fragment(
        &mut self,
        kind: FragmentKind,
        id: &str,
        value: Option<&str>,
        native: Option<SessionMessageNative>,
    ) -> Result<(), BusError> {
        let open = self.fragment(kind).block.as_ref().is_some_and(|block| block.id == id);
        if !open {
            return Err(invalid_event(&kind.event_type("ended"), &format!("{} end before start: {id}", kind.name())));
        }
        self.flush_fragment_pending(kind)?;
        let Some(block) = self.fragment_mut(kind).block.take() else {
            return Ok(());
        };
        let text = match value {
            Some(text) => text.to_string(),
            None => block.values.join(""),
        };
        self.publish_fragment_ended(kind, block.ordinal, &text, native)
    }

    fn end_open_fragment(&mut self, kind: FragmentKind) -> Result<(), BusError> {
        self.flush_fragment_pending(kind)?;
        let Some(block) = self.fragment_mut(kind).block.take() else {
            return Ok(());
        };
        self.publish_fragment_ended(kind, block.ordinal, &block.values.join(""), None)
    }

    fn flush_fragment_pending(&mut self, kind: FragmentKind) -> Result<(), BusError> {
        let pending =
            self.fragment_mut(kind).block.as_mut().and_then(|block| {
                (!block.pending.is_empty()).then(|| (block.ordinal, std::mem::take(&mut block.pending)))
            });
        let Some((ordinal, delta)) = pending else {
            return Ok(());
        };
        self.publish_fragment_delta(kind, ordinal, &delta)
    }

    fn publish_fragment_started(
        &self,
        kind: FragmentKind,
        ordinal: f64,
        native: Option<SessionMessageNative>,
    ) -> Result<(), BusError> {
        let session_id = SessionID(self.session_id.clone());
        let assistant_message_id = SessionMessageID(self.assistant_message_id.clone());
        match kind {
            FragmentKind::Text => self.publish_durable(
                "session.text.started",
                &SessionTextStartedData { session_id, assistant_message_id, ordinal },
            ),
            FragmentKind::Reasoning => self.publish_durable(
                "session.reasoning.started",
                &SessionReasoningStartedData { session_id, assistant_message_id, ordinal, native },
            ),
        }
    }

    fn publish_fragment_delta(&self, kind: FragmentKind, ordinal: f64, delta: &str) -> Result<(), BusError> {
        let session_id = SessionID(self.session_id.clone());
        let assistant_message_id = SessionMessageID(self.assistant_message_id.clone());
        let delta = delta.to_string();
        match kind {
            FragmentKind::Text => self.publish_event(
                Definition::ephemeral("session.text.delta"),
                &SessionTextDeltaData { session_id, assistant_message_id, ordinal, delta },
            ),
            FragmentKind::Reasoning => self.publish_event(
                Definition::ephemeral("session.reasoning.delta"),
                &SessionReasoningDeltaData { session_id, assistant_message_id, ordinal, delta },
            ),
        }
    }

    fn publish_fragment_ended(
        &self,
        kind: FragmentKind,
        ordinal: f64,
        text: &str,
        native: Option<SessionMessageNative>,
    ) -> Result<(), BusError> {
        let session_id = SessionID(self.session_id.clone());
        let assistant_message_id = SessionMessageID(self.assistant_message_id.clone());
        let text = text.to_string();
        match kind {
            FragmentKind::Text => self.publish_durable(
                "session.text.ended",
                &SessionTextEndedData { session_id, assistant_message_id, ordinal, text, native },
            ),
            FragmentKind::Reasoning => self.publish_durable(
                "session.reasoning.ended",
                &SessionReasoningEndedData { session_id, assistant_message_id, ordinal, text, native },
            ),
        }
    }

    fn native(&self, metadata: Option<&ProviderMetadata>) -> Option<SessionMessageNative> {
        let value = metadata?.get(&self.provider_metadata_key)?;
        let object = value.as_object()?;
        Some(JsonMap(object.iter().map(|(key, value)| (key.clone(), value.clone())).collect()))
    }

    fn publish_durable<T: Encode>(&self, r#type: &str, data: &T) -> Result<(), BusError> {
        let definition = Definition::latest_durable(r#type)
            .ok_or_else(|| invalid_event(r#type, &format!("Unknown durable event type {}", r#type)))?;
        self.publish_event(definition, data)
    }

    fn publish_event<T: Encode>(&self, definition: Definition, data: &T) -> Result<(), BusError> {
        let value = serde_json::from_str::<Value>(&encode_json(data))
            .map_err(|error| invalid_event(definition.r#type, &error.to_string()))?;
        let options = PublishOptions { location: self.location.clone(), ..Default::default() };
        self.bus.publish(&definition, value, options).map(|_| ())
    }
}

fn begins_output(event: &LlmEvent) -> bool {
    matches!(
        event,
        LlmEvent::TextStart { .. }
            | LlmEvent::ReasoningStart { .. }
            | LlmEvent::ToolInputStart { .. }
            | LlmEvent::ToolInputError { .. }
            | LlmEvent::ToolCall { .. }
    )
}

fn finish_kind(reason: &FinishReason) -> SessionStepEndedDataFinish {
    match reason {
        FinishReason::Stop => SessionStepEndedDataFinish::Stop,
        FinishReason::Length => SessionStepEndedDataFinish::Length,
        FinishReason::ToolCalls => SessionStepEndedDataFinish::ToolCalls,
        FinishReason::ContentFilter => SessionStepEndedDataFinish::ContentFilter,
        FinishReason::Error => SessionStepEndedDataFinish::Error,
        FinishReason::Unknown => SessionStepEndedDataFinish::Unknown,
    }
}

fn invalid_event(r#type: &str, message: &str) -> BusError {
    BusError::InvalidDurableEvent { r#type: r#type.to_string(), message: message.to_string() }
}
