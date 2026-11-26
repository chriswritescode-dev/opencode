use std::collections::BTreeMap;
use std::sync::Arc;

use opencode_ai::{FinishReason, FinishReasonDetails, LlmEvent, ProviderMetadata, ToolResultValue, Usage};
use opencode_bus::{Bus, BusError, Definition, PublishOptions};
use opencode_contract::codec::{Encode, JsonMap, encode_json};
use opencode_contract::generated::{
    AgentID, LocationRef, ModelRef, SessionID, SessionMessageID, SessionMessageNative, SessionReasoningDeltaData,
    SessionReasoningEndedData, SessionReasoningStartedData, SessionStepEndedDataFinish, SessionStepFirstTokenData,
    SessionStepStartedData, SessionStepStreamedData, SessionStructuredError, SessionTextDeltaData,
    SessionTextEndedData, SessionTextStartedData, SessionToolCalledData, SessionToolFailedData,
    SessionToolInputEndedData, SessionToolInputStartedData, SessionToolProgressData, SessionToolSuccessData,
    ToolContent, ToolFileContent, ToolFileContentType, ToolTextContent, ToolTextContentType,
};
use serde_json::Value;

use opencode_tool::{Content, ToolOutput};

const INPUT_EXCERPT_LIMIT: usize = 2_048;

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

#[derive(Clone)]
struct InputExcerpt {
    text: String,
    length: usize,
}

struct ToolState {
    name: String,
    called: bool,
    settled: bool,
    provider_executed: bool,
    input_open: bool,
    input: Vec<String>,
    excerpt: Option<InputExcerpt>,
    malformed: bool,
}

#[derive(Clone)]
pub struct ToolProgress {
    bus: Arc<Bus>,
    session_id: String,
    assistant_message_id: String,
    location: Option<LocationRef>,
}

impl ToolProgress {
    pub fn new(bus: Arc<Bus>, session_id: &str, assistant_message_id: &str, location: Option<LocationRef>) -> Self {
        ToolProgress {
            bus,
            session_id: session_id.to_owned(),
            assistant_message_id: assistant_message_id.to_owned(),
            location,
        }
    }

    pub fn publish(&self, id: &str, metadata: JsonMap<Value>) -> Result<(), BusError> {
        let data = SessionToolProgressData {
            session_id: SessionID(self.session_id.clone()),
            assistant_message_id: SessionMessageID(self.assistant_message_id.clone()),
            id: id.to_owned(),
            metadata,
        };
        let definition = Definition::ephemeral("session.tool.progress");
        let value = serde_json::from_str::<Value>(&encode_json(&data))
            .map_err(|error| invalid_event("session.tool.progress", &error.to_string()))?;
        self.bus
            .publish(&definition, value, PublishOptions { location: self.location.clone(), ..Default::default() })
            .map(|_| ())
    }
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
    tools: BTreeMap<String, ToolState>,
    streamed_published: bool,
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
            tools: BTreeMap::new(),
            streamed_published: false,
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
        if self.streamed_published {
            return Ok(());
        }
        self.streamed_published = true;
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
            LlmEvent::ToolInputStart { id, name, provider_executed, .. } => {
                self.output_started = true;
                self.start_tool_input(id, name, provider_executed == &Some(true))
            }
            LlmEvent::ToolInputDelta { id, name, text, .. } => self.append_tool_input(id, name, text),
            LlmEvent::ToolInputEnd { id, name, .. } => self.end_tool_input(id, name),
            LlmEvent::ToolInputError { id, name, raw, .. } => {
                self.output_started = true;
                self.malformed_tool_input(id, name, raw)
            }
            LlmEvent::ToolCall { id, name, input, provider_executed, provider_metadata, .. } => {
                self.output_started = true;
                let native = self.native(provider_metadata.as_ref());
                self.tool_call(id, name, input, provider_executed == &Some(true), native)
            }
            LlmEvent::ToolResult { id, name, result, provider_executed, provider_metadata, .. } => {
                let native = self.native(provider_metadata.as_ref());
                self.tool_result(id, name, result, provider_executed == &Some(true), native)
            }
            LlmEvent::Compaction(_) | LlmEvent::Media { .. } | LlmEvent::Finish { .. } | LlmEvent::Raw { .. } => Ok(()),
        }
    }

    pub fn flush(&mut self) -> Result<(), BusError> {
        self.flush_pending_deltas()?;
        self.end_open_fragment(FragmentKind::Text)?;
        self.end_open_fragment(FragmentKind::Reasoning)?;
        self.fail_malformed_tools()
    }

    pub fn flush_pending_deltas(&mut self) -> Result<(), BusError> {
        self.flush_fragment_pending(FragmentKind::Text)?;
        self.flush_fragment_pending(FragmentKind::Reasoning)
    }

    pub fn has_pending_deltas(&self) -> bool {
        let pending = |fragments: &Fragments| fragments.block.as_ref().is_some_and(|block| !block.pending.is_empty());
        pending(&self.text) || pending(&self.reasoning)
    }

    pub fn has_started(&self) -> bool {
        self.step_started
    }

    pub fn record(&self) -> Record {
        Record {
            output_started: self.output_started,
            provider_failed: self.provider_failed,
            needs_continuation: self
                .tools
                .values()
                .any(|tool| !tool.provider_executed && (tool.called || tool.settled)),
            finish: self.finish.clone(),
        }
    }

    fn start_tool_input(&mut self, id: &str, name: &str, provider_executed: bool) -> Result<(), BusError> {
        if self.tools.contains_key(id) {
            return Err(invalid_event("session.tool.input.started", &format!("Duplicate tool input start: {id}")));
        }
        self.start()?;
        self.tools.insert(
            id.to_owned(),
            ToolState {
                name: name.to_owned(),
                called: false,
                settled: false,
                provider_executed,
                input_open: true,
                input: Vec::new(),
                excerpt: None,
                malformed: false,
            },
        );
        self.publish_durable(
            "session.tool.input.started",
            &SessionToolInputStartedData {
                session_id: SessionID(self.session_id.clone()),
                assistant_message_id: SessionMessageID(self.assistant_message_id.clone()),
                id: id.to_owned(),
                name: name.to_owned(),
            },
        )
    }

    fn append_tool_input(&mut self, id: &str, name: &str, text: &str) -> Result<(), BusError> {
        let Some(tool) = self.tools.get_mut(id) else {
            return Err(invalid_event("session.tool.input.delta", &format!("Tool input delta before start: {id}")));
        };
        if tool.name != name {
            return Err(invalid_event(
                "session.tool.input.delta",
                &format!("Tool input name changed for {id}: {} -> {name}", tool.name),
            ));
        }
        tool.input.push(text.to_owned());
        Ok(())
    }

    fn end_tool_input(&mut self, id: &str, name: &str) -> Result<(), BusError> {
        let text = {
            let Some(tool) = self.tools.get_mut(id) else {
                return Err(invalid_event("session.tool.input.ended", &format!("Tool input end before start: {id}")));
            };
            if tool.name != name {
                return Err(invalid_event(
                    "session.tool.input.ended",
                    &format!("Tool input name changed for {id}: {} -> {name}", tool.name),
                ));
            }
            if !tool.input_open {
                return Err(invalid_event("session.tool.input.ended", &format!("Duplicate tool input end: {id}")));
            }
            tool.input_open = false;
            tool.input.join("")
        };
        self.tools.get_mut(id).expect("the tool was just read").excerpt = Some(excerpt(&text));
        self.publish_durable(
            "session.tool.input.ended",
            &SessionToolInputEndedData {
                session_id: SessionID(self.session_id.clone()),
                assistant_message_id: SessionMessageID(self.assistant_message_id.clone()),
                id: id.to_owned(),
                text,
            },
        )
    }

    fn malformed_tool_input(&mut self, id: &str, name: &str, raw: &str) -> Result<(), BusError> {
        if !self.tools.contains_key(id) {
            self.start_tool_input(id, name, false)?;
        }
        let open = self.tools.get(id).is_some_and(|tool| tool.input_open);
        if open {
            if let Some(tool) = self.tools.get_mut(id) {
                tool.input = vec![raw.to_owned()];
            }
            self.end_tool_input(id, name)?;
        }
        if let Some(tool) = self.tools.get_mut(id) {
            tool.excerpt = Some(excerpt(raw));
            tool.malformed = true;
        }
        Ok(())
    }

    fn tool_call(
        &mut self,
        id: &str,
        name: &str,
        input: &Value,
        provider_executed: bool,
        native: Option<SessionMessageNative>,
    ) -> Result<(), BusError> {
        if !self.tools.contains_key(id) {
            self.start_tool_input(id, name, provider_executed)?;
        }
        let open = self.tools.get(id).is_some_and(|tool| tool.input_open);
        if open {
            self.end_tool_input(id, name)?;
        }
        let Some(tool) = self.tools.get_mut(id) else {
            return Ok(());
        };
        if tool.name != name {
            return Err(invalid_event(
                "session.tool.called",
                &format!("Tool call name changed for {id}: {} -> {name}", tool.name),
            ));
        }
        if tool.called {
            return Err(invalid_event("session.tool.called", &format!("Duplicate tool call: {id}")));
        }
        tool.called = true;
        tool.provider_executed = provider_executed;
        self.publish_durable(
            "session.tool.called",
            &SessionToolCalledData {
                session_id: SessionID(self.session_id.clone()),
                assistant_message_id: SessionMessageID(self.assistant_message_id.clone()),
                id: id.to_owned(),
                input: as_record(input),
                executed: provider_executed,
                native,
            },
        )
    }

    fn tool_result(
        &mut self,
        id: &str,
        name: &str,
        result: &ToolResultValue,
        provider_executed: bool,
        native: Option<SessionMessageNative>,
    ) -> Result<(), BusError> {
        let Some(tool) = self.tools.get_mut(id) else {
            return Err(invalid_event("session.tool.success", &format!("Tool result before call: {id}")));
        };
        if tool.name != name {
            return Err(invalid_event(
                "session.tool.success",
                &format!("Tool result name changed for {id}: {} -> {name}", tool.name),
            ));
        }
        if tool.settled {
            return if matches!(result, ToolResultValue::Error { .. }) {
                Ok(())
            } else {
                Err(invalid_event("session.tool.success", &format!("Duplicate tool result: {id}")))
            };
        }
        tool.settled = true;
        let executed = provider_executed || tool.provider_executed;
        match result {
            ToolResultValue::Error { value } => self.publish_tool_failed(
                id,
                SessionStructuredError {
                    r#type: "tool.execution".to_owned(),
                    message: stringify(value),
                    status: None,
                    response: None,
                    retry_after_ms: None,
                },
                None,
                executed,
                native,
            ),
            result => self.publish_tool_success(id, hosted_content(result), None, executed, native),
        }
    }

    pub fn tool_execution(&mut self, id: &str, name: &str, result: ToolOutput) -> Result<(), BusError> {
        let Some(tool) = self.tools.get_mut(id) else {
            return Err(invalid_event("session.tool.success", &format!("Tool execution before call: {id}")));
        };
        if tool.name != name {
            return Err(invalid_event(
                "session.tool.success",
                &format!("Tool execution name changed for {id}: {} -> {name}", tool.name),
            ));
        }
        if tool.settled {
            return Err(invalid_event("session.tool.success", &format!("Duplicate tool execution: {id}")));
        }
        tool.settled = true;
        let executed = tool.provider_executed;
        let content = result.content.iter().map(contract_content).collect::<Vec<_>>();
        if content.is_empty() {
            return Err(invalid_event("session.tool.success", &format!("Tool execution has no content: {id}")));
        }
        let metadata = result.metadata.and_then(|metadata| match metadata {
            Value::Object(map) => Some(JsonMap(map.into_iter().collect())),
            _ => None,
        });
        self.publish_tool_success(id, content, metadata, executed, None)
    }

    pub fn fail_tool(
        &mut self,
        id: &str,
        error: SessionStructuredError,
        metadata: Option<JsonMap<Value>>,
    ) -> Result<bool, BusError> {
        let Some(tool) = self.tools.get_mut(id) else { return Ok(false) };
        if tool.settled {
            return Ok(false);
        }
        tool.settled = true;
        let executed = tool.provider_executed;
        self.publish_tool_failed(id, error, metadata, executed, None)?;
        Ok(true)
    }

    pub fn fail_unsettled_tools(&mut self, error: &SessionStructuredError) -> Result<bool, BusError> {
        let ids = self.tools.iter().filter(|(_, tool)| !tool.settled).map(|(id, _)| id.clone()).collect::<Vec<_>>();
        let mut failed = false;
        for id in ids {
            failed |= self.fail_tool(&id, error.clone(), None)?;
        }
        Ok(failed)
    }

    pub fn fail_incomplete_tools(&mut self) -> Result<(), BusError> {
        let ids = self
            .tools
            .iter()
            .filter(|(_, tool)| !tool.called && !tool.settled)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            let excerpt = self.tools.get(&id).and_then(|tool| tool.excerpt.as_ref()).cloned();
            self.fail_tool(&id, input_failure("incomplete", excerpt.as_ref()), None)?;
        }
        Ok(())
    }

    fn fail_malformed_tools(&mut self) -> Result<(), BusError> {
        let ids = self
            .tools
            .iter()
            .filter(|(_, tool)| tool.malformed && !tool.settled)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            let reason =
                if self.finish.as_ref().is_some_and(|finish| finish.normalized == SessionStepEndedDataFinish::Length) {
                    "length"
                } else {
                    "malformed"
                };
            let excerpt = self.tools.get(&id).and_then(|tool| tool.excerpt.as_ref()).cloned();
            self.fail_tool(&id, input_failure(reason, excerpt.as_ref()), None)?;
        }
        Ok(())
    }

    fn publish_tool_success(
        &self,
        id: &str,
        content: Vec<ToolContent>,
        metadata: Option<JsonMap<Value>>,
        executed: bool,
        native: Option<SessionMessageNative>,
    ) -> Result<(), BusError> {
        self.publish_durable(
            "session.tool.success",
            &SessionToolSuccessData {
                session_id: SessionID(self.session_id.clone()),
                assistant_message_id: SessionMessageID(self.assistant_message_id.clone()),
                id: id.to_owned(),
                content,
                metadata,
                executed,
                native,
            },
        )
    }

    fn publish_tool_failed(
        &self,
        id: &str,
        error: SessionStructuredError,
        metadata: Option<JsonMap<Value>>,
        executed: bool,
        native: Option<SessionMessageNative>,
    ) -> Result<(), BusError> {
        self.publish_durable(
            "session.tool.failed",
            &SessionToolFailedData {
                session_id: SessionID(self.session_id.clone()),
                assistant_message_id: SessionMessageID(self.assistant_message_id.clone()),
                id: id.to_owned(),
                error,
                content: None,
                metadata,
                executed,
                native,
            },
        )
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
        if self.finish.is_some() {
            return Err(invalid_event("session.step.ended", "Duplicate step finish"));
        }
        let normalized = finish_kind(&reason.normalized);
        if normalized == SessionStepEndedDataFinish::ContentFilter {
            self.provider_failed = true;
        }
        self.finish = Some(Finish { normalized, raw: reason.raw.clone(), usage: usage.cloned(), native });
        self.flush()
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

fn as_record(value: &Value) -> JsonMap<Value> {
    match value {
        Value::Object(map) => JsonMap(map.iter().map(|(key, value)| (key.clone(), value.clone())).collect()),
        value => JsonMap(vec![("value".to_owned(), value.clone())]),
    }
}

fn stringify(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    }
}

fn excerpt(value: &str) -> InputExcerpt {
    InputExcerpt { text: value.chars().take(INPUT_EXCERPT_LIMIT).collect(), length: value.chars().count() }
}

fn input_failure(reason: &str, excerpt: Option<&InputExcerpt>) -> SessionStructuredError {
    let (r#type, message) = match reason {
        "length" => (
            "tool.input-incomplete",
            "The tool call was not executed because the output token limit (stop reason: length) was reached before you \
             could complete the arguments. Break large payloads into smaller tool calls and reissue the call with \
             complete arguments.",
        ),
        "incomplete" => (
            "tool.input-incomplete",
            "Tool call arguments were not completed and were not executed. Reissue the call with complete arguments.",
        ),
        _ => (
            "tool.input-json",
            "The tool input could not be parsed as JSON, so the call was not executed. Reissue the call with valid JSON \
             arguments.",
        ),
    };
    let message = match excerpt {
        Some(excerpt) if excerpt.length > 0 => {
            let truncated = if excerpt.length > excerpt.text.chars().count() { "\n[truncated]" } else { "" };
            format!(
                "{message}\n\nInput excerpt (first {} of {} characters):\n{}{truncated}",
                excerpt.text.chars().count(),
                excerpt.length,
                excerpt.text
            )
        }
        _ => message.to_owned(),
    };
    SessionStructuredError { r#type: r#type.to_owned(), message, status: None, response: None, retry_after_ms: None }
}

fn contract_content(content: &Content) -> ToolContent {
    match content {
        Content::Text { text } => {
            ToolContent::Text(ToolTextContent { r#type: ToolTextContentType::Text, text: text.clone() })
        }
        Content::File { uri, mime, name } => ToolContent::File(ToolFileContent {
            r#type: ToolFileContentType::File,
            uri: uri.clone(),
            mime: mime.clone(),
            name: Some(name.clone()),
        }),
    }
}

fn hosted_content(result: &ToolResultValue) -> Vec<ToolContent> {
    match result {
        ToolResultValue::Content { value } if !value.is_empty() => value
            .iter()
            .map(|content| match content {
                opencode_ai::ToolContent::Text { text } => {
                    ToolContent::Text(ToolTextContent { r#type: ToolTextContentType::Text, text: text.clone() })
                }
                opencode_ai::ToolContent::File { uri, mime, name } => ToolContent::File(ToolFileContent {
                    r#type: ToolFileContentType::File,
                    uri: uri.clone(),
                    mime: mime.clone(),
                    name: Some(name.clone()),
                }),
            })
            .collect(),
        ToolResultValue::Text { value } | ToolResultValue::Json { value } => {
            vec![ToolContent::Text(ToolTextContent { r#type: ToolTextContentType::Text, text: stringify(value) })]
        }
        ToolResultValue::Content { value } => vec![ToolContent::Text(ToolTextContent {
            r#type: ToolTextContentType::Text,
            text: stringify(&Value::Array(value.iter().map(content_to_value).collect())),
        })],
        ToolResultValue::Error { value } => {
            vec![ToolContent::Text(ToolTextContent { r#type: ToolTextContentType::Text, text: stringify(value) })]
        }
    }
}

fn content_to_value(content: &opencode_ai::ToolContent) -> Value {
    match content {
        opencode_ai::ToolContent::Text { text } => serde_json::json!({ "type": "text", "text": text }),
        opencode_ai::ToolContent::File { uri, mime, name } => {
            let mut value = serde_json::json!({ "type": "file", "uri": uri, "mime": mime });
            if let Some(name) = name {
                value["name"] = Value::String(name.clone());
            }
            value
        }
    }
}

fn invalid_event(r#type: &str, message: &str) -> BusError {
    BusError::InvalidDurableEvent { r#type: r#type.to_string(), message: message.to_string() }
}
