use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use opencode_contract::codec::encode_json;
use opencode_contract::generated::PUBLIC_EVENT_TYPES;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc::{self, Receiver, Sender, error::TryRecvError, error::TrySendError};

use crate::bus::{Bus, ListenerHandle, lock};
use crate::event::Event;
use crate::sse;

pub const SUBSCRIBER_CAPACITY: usize = 4096;

pub type Encode = Arc<dyn Fn(&Event) -> Result<String, String> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FeedError {
    #[error("EventFeed.SubscriberOverflow")]
    SubscriberOverflow { capacity: usize },
    #[error("EventFeed.EncodingError")]
    EncodingError { event_id: String, event_type: String, cause: String },
}

pub fn frame(value: &Value) -> String {
    sse::data_frame(&encode_json(value))
}

pub struct EventFeed {
    subscribers: Arc<Mutex<Vec<(u64, Subscriber)>>>,
    next_id: AtomicU64,
    capacity: usize,
    encode: Encode,
    listener: Mutex<Option<ListenerHandle>>,
}

struct Subscriber {
    types: Option<HashSet<String>>,
    sender: Sender<String>,
    failure: Arc<Mutex<Option<FeedError>>>,
}

impl Subscriber {
    fn accepts(&self, r#type: &str) -> bool {
        self.types.as_ref().is_none_or(|types| types.contains(r#type))
    }
}

pub struct FeedSubscription {
    receiver: Receiver<String>,
    failure: Arc<Mutex<Option<FeedError>>>,
    subscribers: Arc<Mutex<Vec<(u64, Subscriber)>>>,
    id: u64,
}

impl FeedSubscription {
    pub fn try_recv(&mut self) -> Option<Result<String, FeedError>> {
        match self.receiver.try_recv() {
            Ok(frame) => Some(Ok(frame)),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => lock(&self.failure).take().map(Err),
        }
    }

    pub async fn recv(&mut self) -> Option<Result<String, FeedError>> {
        match self.receiver.recv().await {
            Some(frame) => Some(Ok(frame)),
            None => lock(&self.failure).take().map(Err),
        }
    }
}

impl Drop for FeedSubscription {
    fn drop(&mut self) {
        lock(&self.subscribers).retain(|(id, _)| *id != self.id);
    }
}

impl EventFeed {
    pub fn new(bus: &Bus, capacity: usize) -> Arc<EventFeed> {
        EventFeed::attach(bus, capacity, Arc::new(|event| Ok(frame(&Value::Object(event.payload().clone())))))
    }

    pub fn with_encoder(bus: &Bus, capacity: usize, encode: Encode) -> Arc<EventFeed> {
        EventFeed::attach(bus, capacity, encode)
    }

    fn attach(bus: &Bus, capacity: usize, encode: Encode) -> Arc<EventFeed> {
        let feed = Arc::new(EventFeed {
            subscribers: Arc::new(Mutex::new(Vec::new())),
            next_id: AtomicU64::new(0),
            capacity,
            encode,
            listener: Mutex::new(None),
        });
        let weak = Arc::downgrade(&feed);
        let listener = bus.listen(Box::new(move |event| {
            if let Some(feed) = Weak::upgrade(&weak) {
                feed.publish(event);
            }
            Ok(())
        }));
        *lock(&feed.listener) = Some(listener);
        feed
    }

    pub fn subscribe(&self, types: Option<Vec<String>>) -> FeedSubscription {
        let types = types.filter(|types| !types.is_empty()).map(|types| types.into_iter().collect::<HashSet<_>>());
        let (sender, receiver) = mpsc::channel(self.capacity);
        let failure = Arc::new(Mutex::new(None));
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        lock(&self.subscribers).push((id, Subscriber { types, sender, failure: failure.clone() }));
        FeedSubscription { receiver, failure, subscribers: self.subscribers.clone(), id }
    }

    fn publish(&self, event: &Event) {
        if !is_public(event.r#type()) {
            return;
        }
        if !lock(&self.subscribers).iter().any(|(_, subscriber)| subscriber.accepts(event.r#type())) {
            return;
        }
        let encoded = match (self.encode)(event) {
            Ok(encoded) => encoded,
            Err(cause) => {
                self.fail(FeedError::EncodingError {
                    event_id: event.id().to_string(),
                    event_type: event.r#type().to_string(),
                    cause,
                });
                return;
            }
        };
        let mut subscribers = lock(&self.subscribers);
        let mut index = 0;
        while index < subscribers.len() {
            if !subscribers[index].1.accepts(event.r#type()) {
                index += 1;
                continue;
            }
            match subscribers[index].1.sender.try_send(encoded.clone()) {
                Ok(()) => index += 1,
                Err(TrySendError::Full(_)) => {
                    let (_, subscriber) = subscribers.remove(index);
                    *lock(&subscriber.failure) = Some(FeedError::SubscriberOverflow { capacity: self.capacity });
                }
                Err(TrySendError::Closed(_)) => {
                    subscribers.remove(index);
                }
            }
        }
    }

    fn fail(&self, error: FeedError) {
        let current = std::mem::take(&mut *lock(&self.subscribers));
        for (_, subscriber) in current {
            *lock(&subscriber.failure) = Some(error.clone());
        }
    }
}

fn is_public(r#type: &str) -> bool {
    PUBLIC_EVENT_TYPES.contains(&r#type) || r#type.starts_with("rpc.")
}
