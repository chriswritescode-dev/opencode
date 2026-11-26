use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use opencode_bus::{Bus, Definition, PublishOptions};
use opencode_contract::codec::{JsonMap, encode_json};
use opencode_contract::generated::{LocationRef, SessionID, SessionMessageInfo, SessionSyntheticData};
use opencode_fs::path;
use opencode_tool::tools::read::NearbyInstructions as NearbyInstructionsTrait;
use opencode_util::path::Flavor;
use serde_json::Value;

use crate::store::{MessagesInput, Order, SessionStore};

pub struct NearbyInstructions {
    store: SessionStore,
    bus: Arc<Bus>,
    location: Option<LocationRef>,
    root: String,
    claims: Mutex<BTreeMap<String, BTreeSet<String>>>,
}

impl NearbyInstructions {
    pub fn new(store: SessionStore, bus: Arc<Bus>, location: Option<LocationRef>, root: String) -> Arc<Self> {
        Arc::new(NearbyInstructions { store, bus, location, root, claims: Mutex::new(BTreeMap::new()) })
    }

    async fn load_paths(&self, session_id: &str, paths: Vec<String>) -> Result<(), String> {
        if paths.is_empty() {
            return Ok(());
        }
        let claimed = {
            let mut claims = self.claims.lock().expect("the instruction claim lock is not poisoned");
            let entry = claims.entry(session_id.to_owned()).or_default();
            paths.into_iter().filter(|path| entry.insert(path.clone())).collect::<Vec<_>>()
        };
        if claimed.is_empty() {
            return Ok(());
        }
        let _claim = Claim { owner: self, session_id: session_id.to_owned(), paths: claimed.clone() };
        let injected = self.injected(session_id)?;
        let paths = claimed.into_iter().filter(|path| !injected.contains(path)).collect::<Vec<_>>();
        if paths.is_empty() {
            return Ok(());
        }
        let mut readable = Vec::new();
        for path in paths {
            if let Ok(content) = tokio::fs::read_to_string(&path).await {
                readable.push((path, content));
            }
        }
        if readable.is_empty() {
            return Ok(());
        }
        let text = readable
            .iter()
            .map(|(path, content)| format!("Instructions from: {path}\n{content}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let description = format!(
            "Loaded {}",
            readable.iter().map(|(path, _)| describe(&self.root, path)).collect::<Vec<_>>().join(", ")
        );
        let injected = readable.iter().map(|(path, _)| path.clone()).collect::<Vec<_>>();
        let metadata =
            Value::Object([("instruction".to_owned(), serde_json::json!({ "paths": injected }))].into_iter().collect());
        self.publish(session_id, text, description, metadata)
    }

    fn injected(&self, session_id: &str) -> Result<BTreeSet<String>, String> {
        let messages = self
            .store
            .messages(&MessagesInput {
                session_id: session_id.to_owned(),
                limit: None,
                order: Some(Order::Asc),
                r#type: Some("synthetic".to_owned()),
                cursor: None,
            })
            .map_err(|error| error.to_string())?;
        Ok(messages
            .iter()
            .filter_map(|message| match message {
                SessionMessageInfo::Synthetic(synthetic) => synthetic.metadata.as_ref(),
                _ => None,
            })
            .flat_map(|metadata| {
                metadata
                    .0
                    .iter()
                    .find(|(key, _)| key == "instruction")
                    .and_then(|(_, value)| value.get("paths"))
                    .and_then(Value::as_array)
                    .map(|paths| paths.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>())
                    .unwrap_or_default()
            })
            .collect())
    }

    fn publish(&self, session_id: &str, text: String, description: String, metadata: Value) -> Result<(), String> {
        let definition =
            Definition::latest_durable("session.synthetic").ok_or_else(|| "session.synthetic is durable".to_owned())?;
        let data = SessionSyntheticData {
            session_id: SessionID(session_id.to_owned()),
            text,
            description: Some(description),
            metadata: match metadata {
                Value::Object(map) => Some(JsonMap(map.into_iter().collect())),
                _ => None,
            },
        };
        let value = serde_json::from_str::<Value>(&encode_json(&data)).map_err(|error| error.to_string())?;
        self.bus
            .publish(&definition, value, PublishOptions { location: self.location.clone(), ..Default::default() })
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

impl NearbyInstructionsTrait for NearbyInstructions {
    fn load<'a>(&'a self, session_id: &'a str, paths: Vec<String>) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Err(error) = self.load_paths(session_id, paths).await {
                log::warn!("failed to load nearby instructions for {session_id}: {error}");
            }
        })
    }
}

struct Claim<'a> {
    owner: &'a NearbyInstructions,
    session_id: String,
    paths: Vec<String>,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        let mut claims = self.owner.claims.lock().expect("the instruction claim lock is not poisoned");
        let Some(entry) = claims.get_mut(&self.session_id) else { return };
        for path in &self.paths {
            entry.remove(path);
        }
        if entry.is_empty() {
            claims.remove(&self.session_id);
        }
    }
}

fn describe(root: &str, path: &str) -> String {
    if path::contains(root, path) { Flavor::NATIVE.relative(root, path) } else { path.to_owned() }
}
