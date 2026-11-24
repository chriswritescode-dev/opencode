#![allow(clippy::result_large_err)]

pub mod clock;
pub mod id;
pub mod json;
pub mod media;
pub mod modalities;
pub mod prepare;
pub mod protocols;
pub mod provider_error;
pub mod providers;
pub mod route;
pub mod schema;

pub mod experimental {
    pub use crate::modalities::extraction;
    pub use crate::modalities::realtime;
}

pub use media::generation::Generation;
pub use modalities::video::VideoClient;
pub use modalities::{DecisionClient, EmbeddingClient, ImageClient, SpeechClient, TranscriptionClient};
pub use providers::google_adc::GoogleAdc;
pub use providers::{ProviderPackages, RuntimeModel, Settings};

pub use route::auth::{Auth, Credential};
pub use route::client::LlmClient;
pub use route::compile::StreamOptions;
pub use route::executor::{HttpSender, Middleware, RequestExecutor};
pub use route::model::{Model, ModelDescriptor, ProtocolRegistry, Route};
pub use route::transport::channel::{ChannelExecutor, WebSocketInterceptor};
pub use schema::*;

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

pub trait Sleeper: Send + Sync {
    fn sleep(&self, duration: std::time::Duration) -> futures_util::future::BoxFuture<'static, ()>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TokioSleeper;

impl Sleeper for TokioSleeper {
    fn sleep(&self, duration: std::time::Duration) -> futures_util::future::BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

#[derive(Clone)]
pub struct ExecutionContext {
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn IdSource>,
    pub env: Arc<dyn Env>,
    pub sleeper: Arc<dyn Sleeper>,
}

impl ExecutionContext {
    pub fn new(clock: Arc<dyn Clock>, ids: Arc<dyn IdSource>, env: Arc<dyn Env>) -> Self {
        Self::with_sleeper(clock, ids, env, Arc::new(TokioSleeper))
    }

    pub fn with_sleeper(
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdSource>,
        env: Arc<dyn Env>,
        sleeper: Arc<dyn Sleeper>,
    ) -> Self {
        Self { clock, ids, env, sleeper }
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
