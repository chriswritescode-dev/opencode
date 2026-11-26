use std::sync::{Arc, RwLock};

use opencode_ai::route::executor::HttpSender;
use opencode_contract::generated::{
    ConfigEntryDocumentInfo, ConfigEntryDocumentInfoWebsearch, ConfigEntryDocumentInfoWebsearchObjectProvider,
    ConnectionStatus, WebSearchID,
};
use tokio::sync::{Mutex, watch};

use super::Snapshot;
use super::websearch::ConsoleWebSearch;
use crate::integration::IntegrationService;
use crate::provider_plugin::hooks::HookRegistration;

const WEB_SEARCH_PROVIDER: &str = "opencode";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Allowed {
    pub watches: bool,
    pub lists: bool,
    pub artifacts: bool,
}

#[derive(Clone)]
struct WebSearchDependencies {
    integration: Arc<IntegrationService>,
    http: Arc<dyn HttpSender>,
}

pub struct ConsoleHandle {
    pub(crate) snapshot: RwLock<Snapshot>,
    pub(crate) integrated: RwLock<Option<ConfigEntryDocumentInfo>>,
    pub(crate) allowed: RwLock<Allowed>,
    pub(crate) reported: RwLock<Option<ConnectionStatus>>,
    pub(crate) explainers: std::sync::Mutex<Vec<HookRegistration>>,
    pub(crate) loading: Mutex<()>,
    websearch: RwLock<Option<WebSearchDependencies>>,
    revision: watch::Sender<u64>,
}

impl Default for ConsoleHandle {
    fn default() -> Self {
        Self {
            snapshot: RwLock::new(Snapshot::default()),
            integrated: RwLock::new(None),
            allowed: RwLock::new(Allowed::default()),
            reported: RwLock::new(None),
            explainers: std::sync::Mutex::new(Vec::new()),
            loading: Mutex::new(()),
            websearch: RwLock::new(None),
            revision: watch::channel(0).0,
        }
    }
}

impl ConsoleHandle {
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.revision.subscribe()
    }

    pub fn web_search(&self) -> Option<ConsoleWebSearch> {
        let snapshot = self.snapshot();
        let connection = snapshot.connection?;
        let selected = match snapshot.remote.as_ref()?.info.websearch.as_ref()? {
            ConfigEntryDocumentInfoWebsearch::Object(websearch) => &websearch.provider,
            ConfigEntryDocumentInfoWebsearch::Bool(_) => return None,
        };
        let expected =
            ConfigEntryDocumentInfoWebsearchObjectProvider::String2(WebSearchID(WEB_SEARCH_PROVIDER.to_owned()));
        if *selected != expected {
            return None;
        }
        let dependencies = self.websearch.read().expect("lock the Console web search dependencies").clone()?;
        Some(ConsoleWebSearch::new(dependencies.integration, dependencies.http, connection))
    }

    pub(crate) fn set_web_search_dependencies(&self, integration: Arc<IntegrationService>, http: Arc<dyn HttpSender>) {
        *self.websearch.write().expect("lock the Console web search dependencies") =
            Some(WebSearchDependencies { integration, http });
    }

    pub fn allowed(&self) -> Allowed {
        *self.allowed.read().expect("lock the Console allowed flags")
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        self.snapshot.read().expect("lock the Console snapshot").clone()
    }

    pub(crate) fn set_snapshot(&self, value: Snapshot) {
        *self.snapshot.write().expect("lock the Console snapshot") = value;
    }

    pub(crate) fn integrated(&self) -> Option<ConfigEntryDocumentInfo> {
        self.integrated.read().expect("lock the Console integrated document").clone()
    }

    pub(crate) fn set_integrated(&self, value: Option<ConfigEntryDocumentInfo>) {
        *self.integrated.write().expect("lock the Console integrated document") = value;
    }

    pub(crate) fn set_allowed(&self, value: Allowed) {
        *self.allowed.write().expect("lock the Console allowed flags") = value;
    }

    pub(crate) fn reported(&self) -> Option<ConnectionStatus> {
        self.reported.read().expect("lock the Console reported status").clone()
    }

    pub(crate) fn set_reported(&self, value: Option<ConnectionStatus>) {
        *self.reported.write().expect("lock the Console reported status") = value;
    }

    pub(crate) fn set_explainers(&self, value: Vec<HookRegistration>) {
        *self.explainers.lock().expect("lock the Console explainer hooks") = value;
    }

    pub(crate) fn bump(&self) {
        self.revision.send_modify(|revision| *revision += 1);
    }
}
