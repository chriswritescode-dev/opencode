use std::future::Future;
use std::sync::{Arc, RwLock};

use opencode_ai::route::executor::HttpSender;
use opencode_config::seams::KvStore;
use opencode_contract::generated::{
    ConfigEntryDocumentInfo, ConfigEntryDocumentInfoWebsearch, ConfigEntryDocumentInfoWebsearchObjectProvider,
    ConnectionInfo, ConnectionStatus, WebSearchID,
};
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;

use super::Snapshot;
use super::artifacts::ConsoleArtifacts;
use super::content::{ConsoleInstruction, ConsoleSkill};
use super::watches::{WatchEvent, WatchTarget, WatchTools, Watches};
use super::websearch::ConsoleWebSearch;
use crate::integration::IntegrationService;
use crate::provider_plugin::hooks::HookRegistration;

const WEB_SEARCH_PROVIDER: &str = "opencode";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ConsoleSkills {
    pub owner: String,
    pub items: Vec<ConsoleSkill>,
}

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

#[derive(Clone)]
struct ArtifactDependencies {
    integration: Arc<IntegrationService>,
    http: Arc<dyn HttpSender>,
    kv: Arc<dyn KvStore>,
}

#[derive(Clone)]
struct ArtifactsCurrent {
    connection: ConnectionInfo,
    wakes: bool,
}

pub struct ConsoleHandle {
    pub(crate) snapshot: RwLock<Snapshot>,
    pub(crate) integrated: RwLock<Option<ConfigEntryDocumentInfo>>,
    pub(crate) allowed: RwLock<Allowed>,
    pub(crate) reported: RwLock<Option<ConnectionStatus>>,
    pub(crate) explainers: std::sync::Mutex<Vec<HookRegistration>>,
    pub(crate) loading: Mutex<()>,
    instructions: RwLock<Vec<ConsoleInstruction>>,
    console_skills: RwLock<ConsoleSkills>,
    skill_download: std::sync::Mutex<Option<JoinHandle<()>>>,
    websearch: RwLock<Option<WebSearchDependencies>>,
    artifact_dependencies: RwLock<Option<ArtifactDependencies>>,
    artifacts: RwLock<Option<ArtifactsCurrent>>,
    pub(crate) watches: Watches,
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
            instructions: RwLock::new(Vec::new()),
            console_skills: RwLock::new(ConsoleSkills::default()),
            skill_download: std::sync::Mutex::new(None),
            websearch: RwLock::new(None),
            artifact_dependencies: RwLock::new(None),
            artifacts: RwLock::new(None),
            watches: Watches::default(),
            revision: watch::channel(0).0,
        }
    }
}

impl Drop for ConsoleHandle {
    fn drop(&mut self) {
        self.abort_skill_download();
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

    pub fn artifacts(&self) -> Option<ConsoleArtifacts> {
        let current = self.artifacts.read().expect("lock the Console artifacts").clone()?;
        let dependencies =
            self.artifact_dependencies.read().expect("lock the Console artifact dependencies").clone()?;
        Some(ConsoleArtifacts::new(
            dependencies.integration,
            dependencies.http,
            dependencies.kv,
            current.connection,
            current.wakes,
        ))
    }

    pub(crate) fn set_artifact_dependencies(
        &self,
        integration: Arc<IntegrationService>,
        http: Arc<dyn HttpSender>,
        kv: Arc<dyn KvStore>,
    ) {
        *self.artifact_dependencies.write().expect("lock the Console artifact dependencies") =
            Some(ArtifactDependencies { integration, http, kv });
    }

    /// Offers the artifact provider through `connection`, or removes it when it is undefined.
    pub(crate) fn sync_artifacts(&self, connection: Option<ConnectionInfo>, wakes: bool) {
        let next = connection.map(|connection| ArtifactsCurrent { connection, wakes });
        {
            let current = self.artifacts.read().expect("lock the Console artifacts");
            let same = match (current.as_ref(), next.as_ref()) {
                (Some(current), Some(next)) => {
                    crate::integration::key(&current.connection) == crate::integration::key(&next.connection)
                        && current.wakes == next.wakes
                }
                (None, None) => true,
                _ => false,
            };
            if same {
                return;
            }
        }
        *self.artifacts.write().expect("lock the Console artifacts") = next;
    }

    pub fn allowed(&self) -> Allowed {
        *self.allowed.read().expect("lock the Console allowed flags")
    }

    pub fn instructions(&self) -> Vec<ConsoleInstruction> {
        self.instructions.read().expect("lock the Console instructions").clone()
    }

    pub fn skills(&self) -> Vec<ConsoleSkill> {
        let owner = skill_owner(&self.snapshot());
        let skills = self.console_skills.read().expect("lock the Console skills");
        if skills.owner != owner {
            return Vec::new();
        }
        skills.items.clone()
    }

    pub(crate) fn set_instructions(&self, value: Vec<ConsoleInstruction>) {
        *self.instructions.write().expect("lock the Console instructions") = value;
    }

    pub(crate) fn set_skills(&self, owner: String, items: Vec<ConsoleSkill>) {
        *self.console_skills.write().expect("lock the Console skills") = ConsoleSkills { owner, items };
    }

    pub(crate) fn clear_skills(&self) {
        self.abort_skill_download();
        *self.console_skills.write().expect("lock the Console skills") = ConsoleSkills::default();
    }

    pub(crate) fn restart_skill_download(&self, download: impl Future<Output = ()> + Send + 'static) {
        self.abort_skill_download();
        *self.skill_download.lock().expect("lock the Console skill download") = Some(tokio::spawn(download));
    }

    pub(crate) fn abort_skill_download(&self) {
        if let Some(download) = self.skill_download.lock().expect("lock the Console skill download").take() {
            download.abort();
        }
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

    pub(crate) fn sync_watches(self: &Arc<Self>, target: Option<WatchTarget>) {
        self.watches.sync(self, target);
    }

    pub(crate) fn watch_leader(&self) -> bool {
        self.watches.is_leader()
    }

    pub fn watch_tools(&self) -> Option<WatchTools> {
        self.watches.tools()
    }

    pub(crate) async fn forget_watched(&self, session_id: &str) {
        self.watches.forget(session_id).await;
    }

    pub fn watched_events(&self) -> Vec<WatchEvent> {
        self.watches.delivered()
    }
}

pub(crate) fn skill_owner(snapshot: &Snapshot) -> String {
    let connection = snapshot.connection.as_ref().map(crate::integration::key).unwrap_or_default();
    format!("{connection}:{}", snapshot.organization.as_deref().unwrap_or_default())
}
