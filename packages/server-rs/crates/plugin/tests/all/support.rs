use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use opencode_location::location::{LocationInfo, LocationProject};
use opencode_plugin::generation::{ActivateContext, Activation, Activator, Generation, Source};

pub struct RecordingActivator {
    pub id: String,
    pub revision: String,
    pub log: Arc<Mutex<Vec<String>>>,
    pub fail: bool,
    pub gate: Option<Arc<tokio::sync::Notify>>,
}

impl Activator for RecordingActivator {
    fn activate(&self, _context: ActivateContext) -> BoxFuture<'static, Result<Box<dyn Activation>, String>> {
        let id = self.id.clone();
        let revision = self.revision.clone();
        let log = self.log.clone();
        let fail = self.fail;
        let gate = self.gate.clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            log.lock().expect("the test log is never poisoned").push(format!("activate:{id}:{revision}"));
            if fail {
                return Err(format!("{id}@{revision} failed"));
            }
            Ok(Box::new(RecordingActivation { id, revision, log }) as Box<dyn Activation>)
        })
    }
}

struct RecordingActivation {
    id: String,
    revision: String,
    log: Arc<Mutex<Vec<String>>>,
}

impl Activation for RecordingActivation {
    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), String>> {
        let log = self.log.clone();
        let entry = format!("close:{}:{}", self.id, self.revision);
        Box::pin(async move {
            log.lock().expect("the test log is never poisoned").push(entry);
            Ok(())
        })
    }
}

pub fn generation(id: &str, revision: &str, activator: Arc<dyn Activator>) -> Generation {
    Generation {
        id: id.to_string(),
        revision: revision.to_string(),
        source: Some(Source::Builtin),
        features: None,
        gui: None,
        options: serde_json::Map::new(),
        activator,
    }
}

pub fn recorder(id: &str, revision: &str, log: &Arc<Mutex<Vec<String>>>, fail: bool) -> Arc<RecordingActivator> {
    Arc::new(RecordingActivator {
        id: id.to_string(),
        revision: revision.to_string(),
        log: log.clone(),
        fail,
        gate: None,
    })
}

pub fn location(dir: &str) -> LocationInfo {
    LocationInfo {
        directory: dir.to_string(),
        workspace_id: None,
        project: LocationProject { id: "p".to_string(), directory: dir.to_string(), canonical: dir.to_string() },
        vcs: None,
    }
}

pub fn entries(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    log.lock().expect("the test log is never poisoned").clone()
}

pub fn log_line(log: &Arc<Mutex<Vec<String>>>) -> String {
    entries(log).join(", ")
}
