#![allow(clippy::result_large_err)]

pub mod clock;
pub mod id;
pub mod json;
pub mod provider_error;
pub mod route;
pub mod schema;

use std::collections::BTreeMap;
use std::sync::Arc;

use clock::{Clock, SystemClock};
use id::{IdSource, RandomIds};

pub trait Env: Send + Sync {
    fn var(&self, name: &str) -> Option<String>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

#[derive(Debug, Default, Clone)]
pub struct MapEnv {
    values: BTreeMap<String, String>,
}

impl MapEnv {
    pub fn new(values: impl IntoIterator<Item = (String, String)>) -> Self {
        Self { values: values.into_iter().collect() }
    }
}

impl FromIterator<(String, String)> for MapEnv {
    fn from_iter<T: IntoIterator<Item = (String, String)>>(iter: T) -> Self {
        Self::new(iter)
    }
}

impl Env for MapEnv {
    fn var(&self, name: &str) -> Option<String> {
        self.values.get(name).cloned()
    }
}

#[derive(Clone)]
pub struct ExecutionContext {
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdSource>,
    pub env: Arc<dyn Env>,
}

impl ExecutionContext {
    pub fn new(clock: Arc<dyn Clock>, ids: Arc<dyn IdSource>, env: Arc<dyn Env>) -> Self {
        Self { clock, ids, env }
    }

    pub fn system() -> Self {
        Self::new(Arc::new(SystemClock), Arc::new(RandomIds), Arc::new(ProcessEnv))
    }
}

impl Default for ExecutionContext {
    fn default() -> Self {
        Self::system()
    }
}
