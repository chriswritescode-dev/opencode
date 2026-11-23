use std::collections::VecDeque;
use std::pin::Pin;

use futures_util::{Stream, StreamExt, stream};
use serde_json::Value;

use crate::ExecutionContext;
use crate::route::compile::{StreamOptions, compile};
use crate::route::executor::RequestExecutor;
use crate::route::framing::FrameStream;
use crate::route::model::{Model, ResolvedRequest};
use crate::route::protocol::ProtocolState;
use crate::route::transport::http::HttpPrepared;
use crate::schema::{
    AiError, AiErrorReason, HttpContext, InvalidProviderOutputClassification, LlmEvent, LlmRequest, LlmResponse,
    ResponseFold,
};

pub struct LlmClient {
    executor: RequestExecutor,
    context: ExecutionContext,
}

impl LlmClient {
    pub fn new(executor: RequestExecutor, context: ExecutionContext) -> Self {
        Self { executor, context }
    }

    pub fn stream(
        &self,
        model: &Model,
        request: LlmRequest,
        options: StreamOptions,
    ) -> Pin<Box<dyn Stream<Item = Result<LlmEvent, AiError>> + Send>> {
        let compiled = match compile(model, request, &options, &self.context) {
            Ok(compiled) => compiled,
            Err(error) => {
                let item: Result<LlmEvent, AiError> = Err(error);
                return Box::pin(stream::once(async move { item }));
            }
        };
        let include_raw = compiled.resolved.request.include_raw == Some(true);
        let stage = Stage::Execute(Box::new(ExecuteState {
            executor: self.executor.clone(),
            prepared: compiled.prepared,
            resolved: compiled.resolved,
            context: self.context.clone(),
            include_raw,
            route: format!("{}/{}", model.provider, model.route.id),
        }));
        Box::pin(stream::unfold(stage, advance))
    }

    pub async fn generate(
        &self,
        model: &Model,
        request: LlmRequest,
        options: StreamOptions,
    ) -> Result<LlmResponse, AiError> {
        let mut fold = ResponseFold::new();
        let mut events = self.stream(model, request, options);
        while let Some(event) = events.next().await {
            fold.push(event?);
        }
        Ok(fold.finish())
    }
}

enum Stage {
    Execute(Box<ExecuteState>),
    Read(Box<StreamState>),
    Done,
}

struct ExecuteState {
    executor: RequestExecutor,
    prepared: HttpPrepared,
    resolved: ResolvedRequest,
    context: ExecutionContext,
    include_raw: bool,
    route: String,
}

struct StreamState {
    frames: FrameStream,
    protocol: Box<dyn ProtocolState>,
    http: HttpContext,
    route: String,
    include_raw: bool,
    pending: VecDeque<LlmEvent>,
    finished: bool,
    frames_done: bool,
    halted: bool,
    done: bool,
}

async fn advance(stage: Stage) -> Option<(Result<LlmEvent, AiError>, Stage)> {
    match stage {
        Stage::Execute(state) => {
            let ExecuteState { executor, prepared, resolved, context, include_raw, route } = *state;
            let url = prepared.request.url.clone();
            let HttpPrepared { request, framing, middleware } = prepared;
            let response = match executor.execute(request, middleware).await {
                Ok(response) => response,
                Err(error) => return Some((Err(error), Stage::Done)),
            };
            let http = HttpContext {
                url,
                status: response.status,
                headers: response.headers.iter().map(|(name, value)| (name.to_lowercase(), value.clone())).collect(),
            };
            let frames = framing.frame(response.body);
            let protocol = resolved.model.route.protocol.new_state(&resolved, &context);
            let mut state = Box::new(StreamState {
                frames,
                protocol,
                http,
                route,
                include_raw,
                pending: VecDeque::new(),
                finished: false,
                frames_done: false,
                halted: false,
                done: false,
            });
            let item = read_next(&mut state).await?;
            Some((item, Stage::Read(state)))
        }
        Stage::Read(mut state) => {
            let item = read_next(&mut state).await?;
            Some((item, Stage::Read(state)))
        }
        Stage::Done => None,
    }
}

async fn read_next(state: &mut StreamState) -> Option<Result<LlmEvent, AiError>> {
    loop {
        if state.done {
            return None;
        }
        if let Some(event) = state.pending.pop_front() {
            if state.finished && !matches!(event, LlmEvent::Raw { .. }) {
                state.done = true;
                let message = format!("Provider emitted {} after the terminal event", event_type(&event));
                return Some(Err(stream_error(&state.route, message).attach_http(&state.http)));
            }
            if matches!(event, LlmEvent::Finish { .. }) {
                state.finished = true;
            }
            return Some(Ok(event));
        }
        if !state.frames_done {
            match state.frames.next().await {
                Some(Ok(frame)) => {
                    let terminal = state.protocol.terminal(&frame);
                    match state.protocol.step(frame.clone()) {
                        Ok(events) => {
                            if state.include_raw && is_provider_frame(&frame) {
                                state.pending.push_back(LlmEvent::Raw { data: raw_frame(&frame) });
                            }
                            state.pending.extend(events);
                            state.frames_done = terminal;
                        }
                        Err(error) => {
                            state.done = true;
                            return Some(Err(error.attach_http(&state.http)));
                        }
                    }
                }
                Some(Err(error)) => {
                    state.done = true;
                    return Some(Err(error.attach_http(&state.http)));
                }
                None => state.frames_done = true,
            }
            continue;
        }
        if !state.halted {
            state.halted = true;
            match state.protocol.halt() {
                Ok(events) => state.pending.extend(events),
                Err(error) => {
                    state.done = true;
                    return Some(Err(error.attach_http(&state.http)));
                }
            }
            continue;
        }
        state.done = true;
        if !state.finished {
            return Some(Err(incomplete_stream_error(&state.route).attach_http(&state.http)));
        }
        return None;
    }
}

fn stream_error(route: &str, message: String) -> AiError {
    AiError {
        reason: AiErrorReason::InvalidProviderOutput {
            message,
            body: None,
            http: None,
            classification: None,
            route: Some(route.to_string()),
        },
    }
}

fn incomplete_stream_error(route: &str) -> AiError {
    AiError {
        reason: AiErrorReason::InvalidProviderOutput {
            message: "The provider response ended unexpectedly.".to_string(),
            body: None,
            http: None,
            classification: Some(InvalidProviderOutputClassification::IncompleteStream),
            route: Some(route.to_string()),
        },
    }
}

fn raw_frame(frame: &str) -> Value {
    serde_json::from_str(frame).unwrap_or_else(|_| Value::String(frame.to_string()))
}

fn is_provider_frame(_frame: &str) -> bool {
    true
}

fn event_type(event: &LlmEvent) -> &'static str {
    match event {
        LlmEvent::Compaction(_) => "compaction",
        LlmEvent::StepStart { .. } => "step-start",
        LlmEvent::TextStart { .. } => "text-start",
        LlmEvent::TextDelta { .. } => "text-delta",
        LlmEvent::TextEnd { .. } => "text-end",
        LlmEvent::ReasoningStart { .. } => "reasoning-start",
        LlmEvent::ReasoningDelta { .. } => "reasoning-delta",
        LlmEvent::ReasoningEnd { .. } => "reasoning-end",
        LlmEvent::ToolInputStart { .. } => "tool-input-start",
        LlmEvent::ToolInputDelta { .. } => "tool-input-delta",
        LlmEvent::ToolInputEnd { .. } => "tool-input-end",
        LlmEvent::ToolInputError { .. } => "tool-input-error",
        LlmEvent::ToolCall { .. } => "tool-call",
        LlmEvent::ToolResult { .. } => "tool-result",
        LlmEvent::Media { .. } => "media",
        LlmEvent::StepFinish { .. } => "step-finish",
        LlmEvent::Finish { .. } => "finish",
        LlmEvent::Raw { .. } => "raw",
    }
}
