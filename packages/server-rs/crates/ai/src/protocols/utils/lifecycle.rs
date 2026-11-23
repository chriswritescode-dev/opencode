use crate::schema::{FinishReasonDetails, LlmEvent, ProviderMetadata, Usage};

#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub step_started: bool,
    pub text: Vec<String>,
    pub reasoning: Vec<String>,
}

pub fn initial() -> State {
    State { step_started: false, text: Vec::new(), reasoning: Vec::new() }
}

pub fn step_start(state: State, events: &mut Vec<LlmEvent>) -> State {
    if state.step_started {
        return state;
    }
    events.push(LlmEvent::StepStart { index: 0 });
    State { step_started: true, ..state }
}

pub fn text_start(
    state: State,
    events: &mut Vec<LlmEvent>,
    id: &str,
    provider_metadata: Option<ProviderMetadata>,
) -> State {
    if state.text.iter().any(|open| open == id) {
        return state;
    }
    let mut state = step_start(state, events);
    events.push(LlmEvent::TextStart { id: id.to_string(), provider_metadata });
    state.text.push(id.to_string());
    state
}

pub fn text_delta(
    state: State,
    events: &mut Vec<LlmEvent>,
    id: &str,
    text: &str,
    provider_metadata: Option<ProviderMetadata>,
) -> State {
    let state = text_start(state, events, id, provider_metadata.clone());
    events.push(LlmEvent::TextDelta { id: id.to_string(), text: text.to_string(), provider_metadata });
    state
}

pub fn reasoning_start(
    state: State,
    events: &mut Vec<LlmEvent>,
    id: &str,
    provider_metadata: Option<ProviderMetadata>,
) -> State {
    if state.reasoning.iter().any(|open| open == id) {
        return state;
    }
    let mut state = step_start(state, events);
    events.push(LlmEvent::ReasoningStart { id: id.to_string(), provider_metadata });
    state.reasoning.push(id.to_string());
    state
}

pub fn reasoning_delta(
    state: State,
    events: &mut Vec<LlmEvent>,
    id: &str,
    text: &str,
    provider_metadata: Option<ProviderMetadata>,
) -> State {
    let state = reasoning_start(state, events, id, provider_metadata.clone());
    events.push(LlmEvent::ReasoningDelta { id: id.to_string(), text: text.to_string(), provider_metadata });
    state
}

pub fn reasoning_end(
    state: State,
    events: &mut Vec<LlmEvent>,
    id: &str,
    provider_metadata: Option<ProviderMetadata>,
    text: Option<String>,
) -> State {
    if !state.reasoning.iter().any(|open| open == id) {
        return state;
    }
    let mut state = step_start(state, events);
    events.push(LlmEvent::ReasoningEnd { id: id.to_string(), text, provider_metadata });
    state.reasoning.retain(|open| open != id);
    state
}

pub fn text_end(
    state: State,
    events: &mut Vec<LlmEvent>,
    id: &str,
    provider_metadata: Option<ProviderMetadata>,
    text: Option<String>,
) -> State {
    if !state.text.iter().any(|open| open == id) {
        return state;
    }
    let mut state = step_start(state, events);
    events.push(LlmEvent::TextEnd { id: id.to_string(), text, provider_metadata });
    state.text.retain(|open| open != id);
    state
}

pub fn finish(
    state: State,
    events: &mut Vec<LlmEvent>,
    reason: FinishReasonDetails,
    usage: Option<Usage>,
    provider_metadata: Option<ProviderMetadata>,
) -> State {
    let mut state = close_open_blocks(step_start(state, events), events);
    events.push(LlmEvent::StepFinish {
        index: 0,
        reason: reason.clone(),
        usage: usage.clone(),
        provider_metadata: provider_metadata.clone(),
    });
    events.push(LlmEvent::Finish { reason, usage, message: None, provider_metadata });
    state.step_started = false;
    state
}

fn close_open_blocks(mut state: State, events: &mut Vec<LlmEvent>) -> State {
    for id in std::mem::take(&mut state.reasoning) {
        events.push(LlmEvent::ReasoningEnd { id, text: None, provider_metadata: None });
    }
    for id in std::mem::take(&mut state.text) {
        events.push(LlmEvent::TextEnd { id, text: None, provider_metadata: None });
    }
    state
}
