use std::sync::Arc;

use serde_json::{Map, Value};

use crate::protocols::open_responses;
use crate::protocols::open_responses_continuation::{self, DriverInput, Shape};
use crate::protocols::shared;
use crate::route::transport::channel::{ChannelObservation, ChannelPreparation};
use crate::route::transport::http::JsonRequestParts;
use crate::route::transport::websocket::to_websocket_url;
use crate::schema::AiError;

pub const WEBSOCKET_PROTOCOL_HEADER: &str = "responses_websockets=2026-02-06";
pub const WEBSOCKET_ROTATE_AFTER_MS: u64 = 55 * 60 * 1000;

pub type UrlPredicate = Arc<dyn Fn(&str) -> bool + Send + Sync>;
pub type UrlTransform = Arc<dyn Fn(&str) -> String + Send + Sync>;
pub type HeaderTransform = Arc<dyn Fn(Vec<(String, String)>) -> Vec<(String, String)> + Send + Sync>;

pub struct Options {
    pub id: String,
    pub name: String,
    pub rotate_after_ms: Option<u64>,
    pub enabled: Option<UrlPredicate>,
    pub url: Option<UrlTransform>,
    pub headers: Option<HeaderTransform>,
    pub continuation: Option<Shape>,
}

pub fn prepare(options: &Options, parts: &JsonRequestParts) -> Result<Option<ChannelPreparation>, AiError> {
    if options.enabled.as_ref().is_some_and(|enabled| !enabled(&parts.url)) {
        return Ok(None);
    }
    let request = message(&parts.json_body)?;
    let url = options.url.as_ref().map(|transform| transform(&parts.url)).unwrap_or_else(|| parts.url.clone());
    let url = to_websocket_url(&url)?;
    let headers = options
        .headers
        .as_ref()
        .map(|transform| transform(parts.headers.clone()))
        .unwrap_or_else(|| parts.headers.clone());
    let headers = headers.into_iter().filter(|(name, _)| !name.eq_ignore_ascii_case("content-length")).collect();
    let driver = open_responses_continuation::driver(DriverInput {
        id: options.id.clone(),
        name: options.name.clone(),
        request,
        base: Observer::new(options.id.clone(), options.name.clone()),
        continuation: options.continuation.clone(),
    });
    Ok(Some(ChannelPreparation { url, headers, rotate_after_ms: options.rotate_after_ms, driver: Box::new(driver) }))
}

fn message(body: &Value) -> Result<Map<String, Value>, AiError> {
    let Some(object) = body.as_object() else {
        return Err(shared::invalid_request("Open Responses WebSocket body must be a JSON object"));
    };
    let mut request = Map::new();
    request.insert("type".to_string(), Value::String("response.create".to_string()));
    for (key, value) in object {
        if key == "type" || key == "stream" || key == "stream_options" || key == "background" {
            continue;
        }
        request.insert(key.clone(), value.clone());
    }
    Ok(request)
}

pub struct Observer {
    id: String,
    name: String,
    response_id: Option<String>,
    terminal: bool,
}

impl Observer {
    pub fn new(id: String, name: String) -> Self {
        Observer { id, name, response_id: None, terminal: false }
    }

    pub fn observe(&mut self, event: &Value, frame: &str) -> Result<ChannelObservation, AiError> {
        let kind = event.get("type").and_then(Value::as_str).unwrap_or_default();
        if self.terminal {
            return Err(shared::event_error(
                &self.id,
                format!("{} emitted {} after a terminal event", self.name, kind),
                Some(frame.to_string()),
            ));
        }
        if kind == "error" {
            self.terminal = true;
            return Ok(ChannelObservation::ProviderFailure {
                error: open_responses::provider_failure(event, &format!("{} stream error", self.name), Some(frame)),
            });
        }
        if kind == "response.failed" {
            self.terminal = true;
            if self.response_id.is_some()
                && event_response_id(event).is_some_and(|id| !id.is_empty() && Some(id) != self.response_id.as_deref())
            {
                return Err(shared::event_error(
                    &self.id,
                    format!("{} response ID changed during execution", self.name),
                    Some(frame.to_string()),
                ));
            }
            return Ok(ChannelObservation::ProviderFailure {
                error: open_responses::provider_failure(event, &format!("{} response failed", self.name), Some(frame)),
            });
        }
        if kind == "response.created" {
            if self.response_id.is_some() {
                return Err(shared::event_error(
                    &self.id,
                    format!("{} emitted duplicate response.created", self.name),
                    Some(frame.to_string()),
                ));
            }
            let Some(created) = event_response_id(event).filter(|id| !id.is_empty()) else {
                return Err(shared::event_error(
                    &self.id,
                    format!("{} response.created is missing response.id", self.name),
                    Some(frame.to_string()),
                ));
            };
            self.response_id = Some(created.to_string());
            return Ok(ChannelObservation::Frame { frame: frame.to_string() });
        }
        if !kind.starts_with("response.") {
            return Ok(ChannelObservation::Frame { frame: frame.to_string() });
        }
        let Some(known) = self.response_id.as_deref() else {
            return Err(shared::event_error(
                &self.id,
                format!("{} emitted {} before response.created", self.name, kind),
                Some(frame.to_string()),
            ));
        };
        if let Some(current) = event_response_id(event).filter(|id| !id.is_empty())
            && current != known
        {
            return Err(shared::event_error(
                &self.id,
                format!("{} response ID changed during execution", self.name),
                Some(frame.to_string()),
            ));
        }
        if kind == "response.completed" {
            self.terminal = true;
            return Ok(ChannelObservation::Completed { frame: frame.to_string(), checkpoint: None });
        }
        if kind == "response.incomplete" {
            self.terminal = true;
            return Ok(ChannelObservation::Incomplete { frame: frame.to_string() });
        }
        Ok(ChannelObservation::Frame { frame: frame.to_string() })
    }
}

fn event_response_id(event: &Value) -> Option<&str> {
    event.get("response").and_then(|response| response.get("id")).and_then(Value::as_str)
}
