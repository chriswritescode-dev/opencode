use std::sync::Arc;

use futures_util::future::BoxFuture;

use crate::generated::builtins;
use crate::generation::{ActivateContext, Activation, Activator, Generation, Source};

pub struct NoopActivator;

impl Activator for NoopActivator {
    fn activate(&self, _context: ActivateContext) -> BoxFuture<'static, Result<Box<dyn Activation>, String>> {
        Box::pin(async { Ok(Box::new(NoopActivation) as Box<dyn Activation>) })
    }
}

struct NoopActivation;

impl Activation for NoopActivation {
    fn close(self: Box<Self>) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// The active built-in catalog: a later task replaces a no-op with the native port of that id, and nothing else may
/// list built-ins.
pub fn builtins() -> (Vec<Generation>, Vec<Generation>) {
    (generations(builtins::PRE), generations(builtins::POST))
}

fn generations(ids: &[&str]) -> Vec<Generation> {
    ids.iter()
        .map(|id| Generation {
            id: (*id).to_string(),
            revision: "internal".to_string(),
            source: Some(Source::Builtin),
            features: None,
            gui: None,
            options: serde_json::Map::new(),
            activator: Arc::new(NoopActivator),
        })
        .collect()
}

pub fn guarded(id: &str) -> bool {
    builtins::GUARDED.contains(&id)
}
