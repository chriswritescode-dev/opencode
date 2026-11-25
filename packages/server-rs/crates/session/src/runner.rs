use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use opencode_ai::route::executor::ReqwestSender;
use opencode_ai::{ExecutionContext, LlmClient, RequestExecutor, StreamOptions, Usage};
use opencode_bus::{Bus, BusError, Definition, PublishOptions};
use opencode_contract::codec::{Encode, encode_json};
use opencode_contract::generated::{
    LocationRef, ModelCost, ModelCostTierType, MoneyUSD, SessionID, SessionInboxDeliveredData, SessionMessageID,
    SessionRetryScheduledData, SessionStepEndedData, SessionStepEndedDataFinish, SessionStepFailedData,
    SessionStepFailedDataFinish, SessionStructuredError, SessionSyntheticData, TokenUsageInfo, TokenUsageInfoCache,
};
use opencode_storage::Database;
use serde_json::Value;
use tokio::sync::watch;

use crate::context::{self, LoadInput, Loaded};
use crate::execution::{ExecutionError, Placement};
use crate::instruction_state;
use crate::model_request::{self, AppInfo};
use crate::publish_llm_event::Publisher;
use crate::retry::{RetryPolicy, structured};
use crate::store::SessionStore;

const CONTINUE_AFTER_OUTPUT_LIMIT: &str = "Your last response hit the output token limit (stop reason: length). Do not \
                                             apologize, recap, or repeat yourself. Break the remaining work into smaller \
                                             pieces.";
const MAX_CONSECUTIVE_LENGTH: u32 = 3;
const DELTA_FLUSH_MS: u64 = 100;

enum StepOutcome {
    Done,
    Length,
}

pub struct Runner {
    store: SessionStore,
    bus: Arc<Bus>,
    database: Arc<Database>,
    placement: Arc<dyn Placement>,
    client: LlmClient,
    app: AppInfo,
}

#[allow(clippy::result_large_err)]
impl Runner {
    pub fn new(
        store: SessionStore,
        bus: Arc<Bus>,
        database: Arc<Database>,
        placement: Arc<dyn Placement>,
        app: AppInfo,
    ) -> Self {
        let client = LlmClient::new(
            RequestExecutor::new(Arc::new(ReqwestSender::new()), None, Arc::new(opencode_ai::clock::SystemClock)),
            ExecutionContext::system(),
        );
        Self { store, bus, database, placement, client, app }
    }

    pub async fn drain(&self, session_id: &str, mut cancel: watch::Receiver<bool>) -> Result<(), ExecutionError> {
        let placed = tokio::select! {
            _ = cancel.changed() => return Err(ExecutionError::Interrupted),
            placed = self.placement.place(session_id.to_owned()) => placed.map_err(ExecutionError::Placement)?,
        };
        let location = placed.location().cloned();
        let mut step = 1u64;
        let mut continuation = false;
        let mut consecutive_lengths = 0u32;
        loop {
            if *cancel.borrow() {
                return Err(ExecutionError::Interrupted);
            }
            if placed.is_closed() {
                return Ok(());
            }
            let promotable =
                if continuation { Vec::new() } else { self.store.promotable(session_id, true).map_err(bus_error)? };
            if !continuation && promotable.is_empty() {
                return Ok(());
            }
            let list = context::instruction_list(&self.store, placed.agents(), placed.builtins().clone(), session_id)
                .map_err(ExecutionError::Context)?;
            if !continuation {
                instruction_state::prepare_at(&self.database, &self.bus, &list, session_id, location.clone())
                    .map_err(bus_error)?;
                for inbox_id in promotable {
                    self.publish_durable(
                        "session.inbox.delivered",
                        &SessionInboxDeliveredData {
                            session_id: SessionID(session_id.to_owned()),
                            inbox_id: SessionMessageID(inbox_id),
                        },
                        location.clone(),
                    )?;
                }
                step = 1;
            }
            let loaded = context::load_with(
                &LoadInput {
                    database: &self.database,
                    store: &self.store,
                    catalog: placed.catalog(),
                    agents: placed.agents(),
                    env: placed.env(),
                    builtins: placed.builtins().clone(),
                },
                session_id,
                list,
            )
            .map_err(ExecutionError::Context)?;
            let assistant_id = format!("msg_{}", opencode_storage::id::ascending());
            log::debug!("running session step {step} for {session_id}");
            let outcome = self.step(session_id, &loaded, &assistant_id, location.clone(), &mut cancel).await?;
            match outcome {
                StepOutcome::Done => {
                    consecutive_lengths = 0;
                    continuation = false;
                    step += 1;
                    if self.store.promotable(session_id, true).map_err(bus_error)?.is_empty() {
                        return Ok(());
                    }
                }
                StepOutcome::Length => {
                    consecutive_lengths += 1;
                    if consecutive_lengths >= MAX_CONSECUTIVE_LENGTH {
                        return Err(ExecutionError::OutputLimit);
                    }
                    self.publish_durable(
                        "session.synthetic",
                        &SessionSyntheticData {
                            session_id: SessionID(session_id.to_owned()),
                            text: CONTINUE_AFTER_OUTPUT_LIMIT.to_owned(),
                            description: None,
                            metadata: None,
                        },
                        location.clone(),
                    )?;
                    continuation = true;
                }
            }
        }
    }

    async fn step(
        &self,
        session_id: &str,
        loaded: &Loaded,
        assistant_id: &str,
        location: Option<LocationRef>,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<StepOutcome, ExecutionError> {
        let request = model_request::prepare(loaded, &self.app);
        let started = opencode_storage::id::now_ms();
        let provider_metadata_key =
            loaded.runtime.route.provider_metadata_key.as_deref().unwrap_or(loaded.runtime.provider.as_str());
        let mut publisher = Publisher::new(
            self.bus.clone(),
            session_id,
            assistant_id,
            &loaded.agent.id.0,
            &loaded.model_ref(),
            location.clone(),
            started,
            provider_metadata_key,
        );
        let mut policy = RetryPolicy::new();
        let mut delta_deadline: Option<tokio::time::Instant> = None;
        let mut cancelled = *cancel.borrow();
        'stream: loop {
            if cancelled {
                break;
            }
            let mut stream = self.client.stream(&loaded.runtime, request.clone(), StreamOptions::default());
            let mut failure = None;
            loop {
                let next = if publisher.has_pending_deltas() {
                    let deadline = *delta_deadline
                        .get_or_insert_with(|| tokio::time::Instant::now() + Duration::from_millis(DELTA_FLUSH_MS));
                    tokio::select! {
                        _ = cancel.changed() => {
                            cancelled = true;
                            None
                        }
                        event = stream.next() => event,
                        _ = tokio::time::sleep_until(deadline) => {
                            publisher.flush_pending_deltas().map_err(bus_error)?;
                            delta_deadline = None;
                            continue;
                        }
                    }
                } else {
                    delta_deadline = None;
                    tokio::select! {
                        _ = cancel.changed() => {
                            cancelled = true;
                            None
                        }
                        event = stream.next() => event,
                    }
                };
                if cancelled {
                    break 'stream;
                }
                match next {
                    Some(Ok(event)) => publisher.publish(&event).map_err(bus_error)?,
                    Some(Err(error)) => {
                        failure = Some(error);
                        break;
                    }
                    None => break,
                }
            }
            publisher.flush().map_err(bus_error)?;
            let Some(error) = failure else { break };
            if !publisher.record().output_started
                && error.is_retryable()
                && let Some(decision) = policy.decide(&error, true)
            {
                publisher.start().map_err(bus_error)?;
                let at = opencode_storage::id::now_ms() + decision.delay.as_millis() as i64;
                self.publish_durable(
                    "session.retry.scheduled",
                    &SessionRetryScheduledData {
                        session_id: SessionID(session_id.to_owned()),
                        assistant_message_id: SessionMessageID(assistant_id.to_owned()),
                        attempt: decision.attempt as f64,
                        at: at as f64,
                        error: structured(&ExecutionError::Ai(error.clone())),
                    },
                    location.clone(),
                )?;
                tokio::select! {
                    _ = cancel.changed() => {
                        cancelled = true;
                        break 'stream;
                    }
                    _ = tokio::time::sleep(decision.delay) => {}
                }
                continue;
            }
            publisher.start().map_err(bus_error)?;
            self.publish_durable(
                "session.step.failed",
                &SessionStepFailedData {
                    session_id: SessionID(session_id.to_owned()),
                    assistant_message_id: SessionMessageID(assistant_id.to_owned()),
                    error: structured(&ExecutionError::Ai(error.clone())),
                    finish: None,
                    raw_finish: None,
                    native: None,
                    cost: None,
                    tokens: None,
                    snapshot: None,
                    files: None,
                },
                location.clone(),
            )?;
            return Err(ExecutionError::Ai(error));
        }
        if cancelled {
            publisher.flush().map_err(bus_error)?;
            return self.interrupted(session_id, assistant_id, &mut publisher, location);
        }
        publisher.streamed().map_err(bus_error)?;
        let record = publisher.record();
        if record.provider_failed {
            let (cost, tokens) = record.finish.as_ref().map_or((None, None), |finish| {
                let (cost, tokens) = usage_record(finish.usage.as_ref(), &loaded.model.cost);
                (Some(cost), Some(tokens))
            });
            self.publish_durable(
                "session.step.failed",
                &SessionStepFailedData {
                    session_id: SessionID(session_id.to_owned()),
                    assistant_message_id: SessionMessageID(assistant_id.to_owned()),
                    error: SessionStructuredError {
                        r#type: "provider.content-filter".to_owned(),
                        message: "Provider blocked the response".to_owned(),
                        status: None,
                        response: None,
                        retry_after_ms: None,
                    },
                    finish: Some(SessionStepFailedDataFinish::ContentFilter),
                    raw_finish: record.finish.as_ref().and_then(|finish| finish.raw.clone()),
                    native: record.finish.as_ref().and_then(|finish| finish.native.clone()),
                    cost,
                    tokens,
                    snapshot: None,
                    files: None,
                },
                location.clone(),
            )?;
            return Err(ExecutionError::Message("Provider blocked the response".to_owned()));
        }
        if let Some(finish) = &record.finish {
            let (cost, tokens) = usage_record(finish.usage.as_ref(), &loaded.model.cost);
            let mut usage = self.bus.subscribe(&["session.usage.updated"], None);
            self.publish_durable(
                "session.step.ended",
                &SessionStepEndedData {
                    session_id: SessionID(session_id.to_owned()),
                    assistant_message_id: SessionMessageID(assistant_id.to_owned()),
                    finish: finish.normalized,
                    raw_finish: finish.raw.clone(),
                    native: finish.native.clone(),
                    cost,
                    tokens,
                    snapshot: None,
                    files: None,
                },
                location.clone(),
            )?;
            let settled = tokio::time::timeout(Duration::from_secs(2), async {
                while let Some(event) = usage.recv().await {
                    if event.data().get("sessionID").and_then(Value::as_str) == Some(session_id) {
                        return;
                    }
                }
            })
            .await;
            if settled.is_err() {
                log::warn!("session usage did not publish after session.step.ended for {session_id}");
            }
        }
        match record.finish.as_ref().map(|finish| finish.normalized) {
            Some(SessionStepEndedDataFinish::Length) => Ok(StepOutcome::Length),
            _ => Ok(StepOutcome::Done),
        }
    }

    fn interrupted(
        &self,
        session_id: &str,
        assistant_id: &str,
        publisher: &mut Publisher,
        location: Option<LocationRef>,
    ) -> Result<StepOutcome, ExecutionError> {
        if !publisher.has_started() {
            return Err(ExecutionError::Interrupted);
        }
        publisher.streamed().map_err(bus_error)?;
        self.publish_durable(
            "session.step.failed",
            &SessionStepFailedData {
                session_id: SessionID(session_id.to_owned()),
                assistant_message_id: SessionMessageID(assistant_id.to_owned()),
                error: SessionStructuredError {
                    r#type: "aborted".to_owned(),
                    message: "Step interrupted".to_owned(),
                    status: None,
                    response: None,
                    retry_after_ms: None,
                },
                finish: None,
                raw_finish: None,
                native: None,
                cost: None,
                tokens: None,
                snapshot: None,
                files: None,
            },
            location,
        )?;
        Err(ExecutionError::Interrupted)
    }

    fn publish_durable<T: Encode>(
        &self,
        r#type: &str,
        data: &T,
        location: Option<LocationRef>,
    ) -> Result<(), ExecutionError> {
        let definition = Definition::latest_durable(r#type)
            .ok_or_else(|| ExecutionError::Message(format!("Unknown durable event type {type}")))?;
        let value = serde_json::from_str::<Value>(&encode_json(data))
            .map_err(|error| ExecutionError::Message(error.to_string()))?;
        self.bus
            .publish(&definition, value, PublishOptions { location, ..Default::default() })
            .map(|_| ())
            .map_err(bus_error)
    }
}

fn usage_record(usage: Option<&Usage>, costs: &[ModelCost]) -> (MoneyUSD, TokenUsageInfo) {
    let tokens = TokenUsageInfo {
        input: token_value(usage.and_then(|usage| usage.non_cached_input_tokens)),
        output: token_value(usage.map(Usage::visible_output_tokens)),
        reasoning: token_value(usage.and_then(|usage| usage.reasoning_tokens)),
        cache: TokenUsageInfoCache {
            read: token_value(usage.and_then(|usage| usage.cache_read_input_tokens)),
            write: token_value(usage.and_then(|usage| usage.cache_write_input_tokens)),
        },
    };
    let reported = usage.and_then(|usage| usage.cost.as_ref()).and_then(serde_json::Number::as_f64);
    let cost = match reported {
        Some(cost) if cost.is_finite() => cost,
        Some(_) => 0.0,
        None => calculate_cost(costs, &tokens),
    };
    (MoneyUSD(cost), tokens)
}

fn calculate_cost(costs: &[ModelCost], tokens: &TokenUsageInfo) -> f64 {
    let context = tokens.input + tokens.cache.read + tokens.cache.write;
    let tier = costs
        .iter()
        .filter(|cost| {
            cost.tier.as_ref().is_some_and(|tier| tier.r#type == ModelCostTierType::Context && context > tier.size)
        })
        .max_by(|a, b| tier_size(a).total_cmp(&tier_size(b)));
    let cost = tier.or_else(|| costs.iter().find(|cost| cost.tier.is_none()));
    let Some(cost) = cost else { return 0.0 };
    (tokens.input * cost.input.0
        + (tokens.output + tokens.reasoning) * cost.output.0
        + tokens.cache.read * cost.cache.read.0
        + tokens.cache.write * cost.cache.write.0)
        / 1_000_000.0
}

fn tier_size(cost: &ModelCost) -> f64 {
    cost.tier.as_ref().map_or(0.0, |tier| tier.size)
}

fn token_value(value: Option<u64>) -> f64 {
    value.map_or(0.0, |value| value as f64)
}

fn bus_error(error: BusError) -> ExecutionError {
    ExecutionError::Bus(error.to_string())
}
