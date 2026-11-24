use std::io;

use bytes::Bytes;
use futures_util::stream::unfold;
use http::StatusCode;
use opencode_bus::EventId;
use opencode_bus::feed::{self, FeedSubscription};
use opencode_bus::sse;
use opencode_contract::generated::operations::event_subscribe;
use opencode_contract::request::RawRequest;
use tokio::time::{Instant, Interval, interval_at};

use crate::handlers::invalid;
use crate::response;
use crate::state::ServerState;

pub fn subscribe(state: &ServerState, raw: &RawRequest) -> response::Response {
    let query = match event_subscribe::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    let types = query.types.flatten().map(|items| {
        items
            .into_iter()
            .map(|item| {
                let encoded = opencode_contract::codec::encode_json(&item);
                serde_json::from_str::<String>(&encoded).expect("an event type encodes as a JSON string")
            })
            .collect::<Vec<_>>()
    });
    let subscription = state.feed.subscribe(types);
    let connected = feed::frame(&serde_json::json!({
        "id": EventId::create(),
        "type": "server.connected",
        "data": {},
    }));
    let interval = interval_at(Instant::now() + state.heartbeat, state.heartbeat);
    let stream =
        unfold(StreamState { subscription, interval, connected: Some(connected), heartbeat_sent: false }, step);
    response::stream(
        StatusCode::OK,
        "text/event-stream",
        &[
            ("cache-control", "no-cache, no-transform"),
            ("x-accel-buffering", "no"),
            ("x-content-type-options", "nosniff"),
        ],
        stream,
    )
}

struct StreamState {
    subscription: FeedSubscription,
    interval: Interval,
    connected: Option<String>,
    heartbeat_sent: bool,
}

async fn step(mut state: StreamState) -> Option<(Result<Bytes, io::Error>, StreamState)> {
    if let Some(connected) = state.connected.take() {
        return Some((Ok(Bytes::from(connected)), state));
    }
    if !state.heartbeat_sent {
        state.heartbeat_sent = true;
        return Some((Ok(Bytes::from(sse::HEARTBEAT)), state));
    }
    let item = tokio::select! {
        item = state.subscription.recv() => Some(item),
        _ = state.interval.tick() => None,
    };
    match item {
        None => Some((Ok(Bytes::from(sse::HEARTBEAT)), state)),
        Some(Some(Ok(frame))) => Some((Ok(Bytes::from(frame)), state)),
        Some(Some(Err(error))) => Some((Err(io::Error::other(error)), state)),
        Some(None) => None,
    }
}
