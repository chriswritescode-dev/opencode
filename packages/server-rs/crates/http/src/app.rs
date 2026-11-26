use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderValue, Request, StatusCode, header};
use hyper::upgrade::OnUpgrade;
use opencode_contract::generated::{PairingSession, ServerInfo, ServerInfoCapabilities, ServerInfoPaths};
use opencode_contract::request::RawRequest;

use crate::auth;
use crate::compression;
use crate::cors;
use crate::pairing::Pairing;
use crate::request_site;
use crate::response;
use crate::router;
use crate::status::{State, Status};
use crate::ticket::TicketStore;

pub const IMPLEMENTED: &[&str] = &[
    "server.info",
    "server.pair",
    "server.connect",
    "event.subscribe",
    "pty.list",
    "pty.create",
    "pty.get",
    "pty.update",
    "pty.remove",
    "pty.connect.token",
    "pty.connect",
    "experimental.persistentPty.read",
    "experimental.persistentPty.list",
    "experimental.persistentPty.create",
    "experimental.persistentPty.shutdown",
    "experimental.persistentPty.handoff",
    "experimental.persistentPty.get",
    "experimental.persistentPty.update",
    "experimental.persistentPty.remove",
    "experimental.persistentPty.snapshot",
    "experimental.persistentPty.connectToken",
    "experimental.persistentPty.connect",
    "experimental.session.log",
    "location.get",
    "location.reload",
    "debug.heapDump",
    "debug.location.list",
    "debug.location.evict",
    "project.list",
    "project.update",
    "vcs.init",
    "vcs.get",
    "vcs.base",
    "vcs.status",
    "vcs.branch.list",
    "vcs.diff",
    "worktree.list",
    "worktree.create",
    "worktree.remove",
    "worktree.refresh",
    "config.get",
    "config.shells",
    "experimental.config.update",
    "agent.list",
    "agent.get",
    "command.list",
    "skill.list",
    "reference.list",
    "fs.read",
    "fs.list",
    "fs.find",
    "experimental.fs.write",
    "experimental.fs.watch",
    "experimental.fs.unwatch",
    "form.list",
    "permission.request.list",
    "permission.saved.list",
    "permission.saved.update",
    "permission.saved.remove",
    "session.permission.create",
    "session.permission.list",
    "session.permission.get",
    "session.permission.reply",
    "session.form.list",
    "session.form.create",
    "session.form.get",
    "session.form.cancel",
    "session.form.reply",
    "session.list",
    "session.create",
    "session.get",
    "session.update",
    "session.remove",
    "session.prompt",
    "session.message.list",
    "session.inbox.list",
    "model.list",
    "session.switchAgent",
    "session.switchModel",
    "session.active",
    "experimental.session.wait",
    "experimental.job.list",
    "plugin.list",
    "plugin.gui",
    "plugin.check",
    "plugin.update",
];

pub struct Options {
    pub password: String,
    pub version: String,
    pub cors: Vec<String>,
    pub tmp: PathBuf,
    pub hostname: Option<String>,
    pub urls: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    pub state: Option<Arc<crate::state::ServerState>>,
    pub services: Option<Arc<crate::services::GlobalServices>>,
}

pub struct App {
    options: Options,
    status: Status,
    pairing: Pairing,
    tickets: TicketStore,
}

impl App {
    pub fn new(options: Options) -> Arc<App> {
        Arc::new(App { options, status: Status::new(), pairing: Pairing::new(), tickets: TicketStore::new() })
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn tickets(&self) -> &TicketStore {
        &self.tickets
    }

    pub fn cors(&self) -> &[String] {
        &self.options.cors
    }

    pub async fn handle(&self, request: Request<Bytes>) -> response::Response {
        self.handle_upgrade(request, None).await
    }

    pub async fn handle_upgrade(&self, request: Request<Bytes>, upgrade: Option<OnUpgrade>) -> response::Response {
        if request.method() == http::Method::OPTIONS {
            return cors::preflight(&request, &self.options.cors);
        }
        let response = if request_site::rejected(&request, &self.options.cors) {
            forbidden()
        } else {
            self.dispatch(&request, upgrade).await
        };
        let response = cors::apply(&request, response, &self.options.cors);
        let accept_encoding = request.headers().get(header::ACCEPT_ENCODING).and_then(|value| value.to_str().ok());
        compression::apply(accept_encoding, response)
    }

    pub fn admits<B>(&self, request: &Request<B>) -> bool {
        request.method() != http::Method::OPTIONS
            && !request_site::rejected(request, &self.options.cors)
            && self.gate(request).is_none()
    }

    fn gate<B>(&self, request: &Request<B>) -> Option<response::Response> {
        let state = self.status.current();
        let ready = state == State::Ready;
        if request.method() == http::Method::GET && request.uri().path() == "/api/info" && !ready {
            if !self.authorized(request) {
                return Some(auth::unauthorized(request, "Authentication required"));
            }
            return Some(info(&self.options, state));
        }
        if !exempt(request, ready) && !self.authorized(request) {
            return Some(auth::unauthorized(request, "Authentication required"));
        }
        if !ready {
            return Some(unavailable(state));
        }
        None
    }

    async fn dispatch(&self, request: &Request<Bytes>, upgrade: Option<OnUpgrade>) -> response::Response {
        if let Some(response) = self.gate(request) {
            return response;
        }
        let method = request.method();
        if (method == http::Method::GET || method == http::Method::HEAD)
            && router::matches_static(request.uri().path(), "/openapi.json")
        {
            return response::bytes(
                StatusCode::OK,
                "application/json",
                Bytes::from_static(opencode_contract::SERVED_OPENAPI_JSON.as_bytes()),
            );
        }
        let Some(found) = router::find(request.method(), request.uri().path()) else {
            return response::empty(StatusCode::NOT_FOUND);
        };
        let raw = raw_request(request, found.params.clone());
        match found.route.operation_id {
            "server.info" => server_info(&self.options),
            "server.pair" => response::bytes(
                StatusCode::OK,
                "application/json",
                opencode_contract::codec::encode_json(&self.pairing.issue()),
            ),
            "server.connect" => self.connect(request, &found),
            "event.subscribe" => match &self.options.state {
                Some(state) => crate::handlers::event::subscribe(state, &raw),
                None => not_implemented(found.route.identifier),
            },
            "pty.list" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::pty::list(&raw, lease.services()),
                Err(response) => response,
            },
            "pty.create" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::pty::create(&raw, lease.services()),
                Err(response) => response,
            },
            "pty.get" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::pty::get(&raw, lease.services()),
                Err(response) => response,
            },
            "pty.update" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::pty::update(&raw, lease.services()),
                Err(response) => response,
            },
            "pty.remove" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::pty::remove(&raw, lease.services()),
                Err(response) => response,
            },
            "pty.connect.token" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::pty::connect_token(&raw, lease.services(), self.tickets(), self.cors()),
                Err(response) => response,
            },
            "pty.connect" => crate::handlers::pty::connect(self, request, &raw, upgrade).await,
            "experimental.persistentPty.read" => match self.services() {
                Some(services) => crate::handlers::persistent_pty::read(&raw, services).await,
                None => crate::services::services_unavailable(),
            },
            "experimental.persistentPty.list" => match self.services() {
                Some(services) => crate::handlers::persistent_pty::list(&raw, services).await,
                None => crate::services::services_unavailable(),
            },
            "experimental.persistentPty.create" => match self.services() {
                Some(services) => crate::handlers::persistent_pty::create(&raw, services).await,
                None => crate::services::services_unavailable(),
            },
            "experimental.persistentPty.shutdown" => match self.services() {
                Some(services) => crate::handlers::persistent_pty::shutdown(services).await,
                None => crate::services::services_unavailable(),
            },
            "experimental.persistentPty.handoff" => match self.services() {
                Some(services) => crate::handlers::persistent_pty::handoff(services).await,
                None => crate::services::services_unavailable(),
            },
            "experimental.persistentPty.get" => match self.services() {
                Some(services) => crate::handlers::persistent_pty::get(&raw, services).await,
                None => crate::services::services_unavailable(),
            },
            "experimental.persistentPty.update" => match self.services() {
                Some(services) => crate::handlers::persistent_pty::update(&raw, services).await,
                None => crate::services::services_unavailable(),
            },
            "experimental.persistentPty.remove" => match self.services() {
                Some(services) => crate::handlers::persistent_pty::remove(&raw, services).await,
                None => crate::services::services_unavailable(),
            },
            "experimental.persistentPty.snapshot" => match self.services() {
                Some(services) => crate::handlers::persistent_pty::snapshot(&raw, services).await,
                None => crate::services::services_unavailable(),
            },
            "experimental.persistentPty.connectToken" => match self.services() {
                Some(services) => crate::handlers::persistent_pty::connect_token(&raw, services, self).await,
                None => crate::services::services_unavailable(),
            },
            "experimental.persistentPty.connect" => {
                crate::handlers::persistent_pty::connect(self, request, &raw, upgrade).await
            }
            "experimental.session.log" => match &self.options.state {
                Some(state) => crate::handlers::session_log::log(state, &raw),
                None => not_implemented(found.route.identifier),
            },
            "location.get" => crate::handlers::location::get(self, &raw).await,
            "location.reload" => match self.services() {
                Some(services) => crate::handlers::location::reload(services).await,
                None => crate::services::services_unavailable(),
            },
            "debug.heapDump" => match self.services() {
                Some(services) => crate::handlers::debug::heap_dump(services, &raw).await,
                None => crate::services::services_unavailable(),
            },
            "debug.location.list" => match self.services() {
                Some(services) => crate::handlers::debug::location_list(services).await,
                None => crate::services::services_unavailable(),
            },
            "debug.location.evict" => match self.services() {
                Some(services) => crate::handlers::debug::location_evict(services, &raw).await,
                None => crate::services::services_unavailable(),
            },
            "project.list" => match self.services() {
                Some(services) => crate::handlers::project::list(services, &raw).await,
                None => crate::services::services_unavailable(),
            },
            "project.update" => match self.services() {
                Some(services) => crate::handlers::project::update(services, &raw).await,
                None => crate::services::services_unavailable(),
            },
            "vcs.init" => crate::handlers::vcs::init(self, &raw).await,
            "vcs.get" => crate::handlers::vcs::get(self, &raw).await,
            "vcs.base" => crate::handlers::vcs::base(self, &raw).await,
            "vcs.status" => crate::handlers::vcs::status(self, &raw).await,
            "vcs.branch.list" => crate::handlers::vcs::branch_list(self, &raw).await,
            "vcs.diff" => crate::handlers::vcs::diff(self, &raw).await,
            "worktree.list" => match self.services() {
                Some(services) => crate::handlers::worktree::list(services, &raw).await,
                None => crate::services::services_unavailable(),
            },
            "worktree.create" => match self.services() {
                Some(services) => crate::handlers::worktree::create(services, &raw).await,
                None => crate::services::services_unavailable(),
            },
            "worktree.remove" => match self.services() {
                Some(services) => crate::handlers::worktree::remove(services, &raw).await,
                None => crate::services::services_unavailable(),
            },
            "worktree.refresh" => match self.services() {
                Some(services) => crate::handlers::worktree::refresh(services, &raw).await,
                None => crate::services::services_unavailable(),
            },
            "config.get" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::config::get(&raw, &lease.config),
                Err(response) => response,
            },
            "config.shells" => match self.location(&raw).await {
                Ok(_lease) => match self.services() {
                    Some(services) => crate::handlers::config::shells(services),
                    None => crate::services::services_unavailable(),
                },
                Err(response) => response,
            },
            "experimental.config.update" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::config::update(&raw, &lease.config).await,
                Err(response) => response,
            },
            "session.list" => match self.services() {
                Some(services) => crate::handlers::session::list(services, &raw),
                None => crate::services::services_unavailable(),
            },
            "session.create" => match self.services() {
                Some(services) => crate::handlers::session::create(services, &raw).await,
                None => crate::services::services_unavailable(),
            },
            "session.get" => match self.services() {
                Some(services) => crate::handlers::session::get(services, &raw),
                None => crate::services::services_unavailable(),
            },
            "session.update" => match self.services() {
                Some(services) => crate::handlers::session::update(services, &raw).await,
                None => crate::services::services_unavailable(),
            },
            "session.remove" => match self.services() {
                Some(services) => crate::handlers::session::remove(services, &raw).await,
                None => crate::services::services_unavailable(),
            },
            "session.prompt" => match self.services() {
                Some(services) => crate::handlers::session::prompt(services, &raw),
                None => crate::services::services_unavailable(),
            },
            "session.message.list" => match self.services() {
                Some(services) => crate::handlers::message::list(services, &raw),
                None => crate::services::services_unavailable(),
            },
            "session.inbox.list" => match self.services() {
                Some(services) => crate::handlers::session::inbox(services, &raw),
                None => crate::services::services_unavailable(),
            },
            "model.list" => crate::handlers::model::list(self, &raw).await,
            "session.switchAgent" => match self.services() {
                Some(services) => crate::handlers::session::switch_agent(services, &raw),
                None => crate::services::services_unavailable(),
            },
            "session.switchModel" => match self.services() {
                Some(services) => crate::handlers::session::switch_model(services, &raw),
                None => crate::services::services_unavailable(),
            },
            "session.active" => match self.services() {
                Some(services) => crate::handlers::session::active(services),
                None => crate::services::services_unavailable(),
            },
            "experimental.session.wait" => match self.services() {
                Some(services) => crate::handlers::session::wait(services, &raw).await,
                None => crate::services::services_unavailable(),
            },
            "experimental.job.list" => match self.services() {
                Some(services) => crate::handlers::job::list(services, &raw).await,
                None => crate::services::services_unavailable(),
            },
            "plugin.list" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::plugin::list(&raw, lease.services()).await,
                Err(response) => response,
            },
            "plugin.gui" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::plugin::gui(&raw, lease.services()).await,
                Err(response) => response,
            },
            "plugin.check" => match self.location(&raw).await {
                Ok(lease) => match self.services() {
                    Some(services) => {
                        crate::handlers::plugin::check(&raw, lease.services(), &services.plugins.updates).await
                    }
                    None => crate::services::services_unavailable(),
                },
                Err(response) => response,
            },
            "plugin.update" => match self.location(&raw).await {
                Ok(lease) => match self.services() {
                    Some(services) => {
                        crate::handlers::plugin::update(&raw, lease.services(), &services.plugins.updates).await
                    }
                    None => crate::services::services_unavailable(),
                },
                Err(response) => response,
            },
            "agent.list" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::registry::agent_list(&raw, lease.services()).await,
                Err(response) => response,
            },
            "agent.get" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::registry::agent_get(&raw, lease.services()).await,
                Err(response) => response,
            },
            "command.list" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::registry::command_list(&raw, lease.services()).await,
                Err(response) => response,
            },
            "skill.list" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::registry::skill_list(&raw, lease.services()).await,
                Err(response) => response,
            },
            "reference.list" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::registry::reference_list(&raw, lease.services()).await,
                Err(response) => response,
            },
            "fs.read" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::fs::read(request.method(), &raw, lease.services()).await,
                Err(response) => response,
            },
            "fs.list" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::fs::list(&raw, lease.services()).await,
                Err(response) => response,
            },
            "fs.find" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::fs::find(&raw, lease.services()).await,
                Err(response) => response,
            },
            "experimental.fs.write" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::fs::write(&raw, lease.services()).await,
                Err(response) => response,
            },
            "experimental.fs.watch" => match self.location(&raw).await {
                Ok(lease) => match self.services() {
                    Some(services) => crate::handlers::fs::watch(&raw, lease.services(), &services.file_watch).await,
                    None => crate::services::services_unavailable(),
                },
                Err(response) => response,
            },
            "experimental.fs.unwatch" => match self.location(&raw).await {
                Ok(lease) => match self.services() {
                    Some(services) => crate::handlers::fs::unwatch(&raw, lease.services(), &services.file_watch).await,
                    None => crate::services::services_unavailable(),
                },
                Err(response) => response,
            },
            "form.list" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::form::list(&raw, lease.services()),
                Err(response) => response,
            },
            "permission.request.list" => match self.location(&raw).await {
                Ok(lease) => crate::handlers::permission::request_list(&raw, lease.services()),
                Err(response) => response,
            },
            "permission.saved.list" => match self.location(&raw).await {
                Ok(lease) => match self.services() {
                    Some(services) => crate::handlers::permission::saved_list(&raw, services, lease.services()),
                    None => crate::services::services_unavailable(),
                },
                Err(response) => response,
            },
            "permission.saved.update" => match self.services() {
                Some(services) => crate::handlers::permission::saved_update(&raw, services),
                None => crate::services::services_unavailable(),
            },
            "permission.saved.remove" => match self.services() {
                Some(services) => crate::handlers::permission::saved_remove(&raw, services),
                None => crate::services::services_unavailable(),
            },
            "session.permission.create" => crate::handlers::permission::session_create(self, &raw).await,
            "session.permission.list" => crate::handlers::permission::session_list(self, &raw).await,
            "session.permission.get" => crate::handlers::permission::session_get(self, &raw).await,
            "session.permission.reply" => crate::handlers::permission::session_reply(self, &raw).await,
            "session.form.list" => crate::handlers::form::session_list(self, &raw).await,
            "session.form.create" => crate::handlers::form::session_create(self, &raw).await,
            "session.form.get" => crate::handlers::form::session_get(self, &raw).await,
            "session.form.cancel" => crate::handlers::form::session_cancel(self, &raw).await,
            "session.form.reply" => crate::handlers::form::session_reply(self, &raw).await,
            _ => not_implemented(found.route.identifier),
        }
    }

    #[allow(clippy::result_large_err)]
    pub async fn location(
        &self,
        request: &RawRequest,
    ) -> Result<opencode_location::map::Lease<crate::services::LocationServices>, response::Response> {
        let Some(services) = &self.options.services else {
            return Err(crate::services::services_unavailable());
        };
        let reference = match request.path_params.iter().find(|(name, _)| name == "sessionID") {
            Some((_, id)) => match &services.sessions {
                Some(sessions) => match opencode_location::resolve::resolve_session_location(sessions.store(), id) {
                    Ok(reference) => reference,
                    Err(error) => return Err(crate::services::session_location_error(error)),
                },
                None => opencode_location::resolve::request_ref(request, &services.start_directory),
            },
            None => opencode_location::resolve::request_ref(request, &services.start_directory),
        };
        match services.locations.get(&reference).await {
            Ok(lease) => Ok(lease),
            Err(error) => Err(crate::services::location_error(error)),
        }
    }

    #[allow(clippy::result_large_err)]
    pub async fn session_location(
        &self,
        session_id: &str,
    ) -> Result<opencode_location::map::Lease<crate::services::LocationServices>, response::Response> {
        let Some(services) = &self.options.services else {
            return Err(crate::services::services_unavailable());
        };
        let reference = session_reference(services, session_id)?;
        match services.locations.get(&reference).await {
            Ok(lease) => Ok(lease),
            Err(error) => Err(crate::services::location_error(error)),
        }
    }

    #[allow(clippy::result_large_err)]
    pub async fn session_location_live(
        &self,
        session_id: &str,
    ) -> Result<Option<Arc<crate::services::LocationServices>>, response::Response> {
        let Some(services) = &self.options.services else {
            return Err(crate::services::services_unavailable());
        };
        let reference = session_reference(services, session_id)?;
        Ok(services.locations.services(&reference).await)
    }

    pub async fn shutdown(&self) {
        if let Some(services) = &self.options.services {
            services.shutdown_executions().await;
            services.locations.shutdown_all().await;
            services.shutdown_persistent_pty().await;
        }
    }

    pub fn services(&self) -> Option<&Arc<crate::services::GlobalServices>> {
        self.options.services.as_ref()
    }

    fn authorized<B>(&self, request: &Request<B>) -> bool {
        auth::authorized(&auth::credential(request), &self.options.password)
            || auth::authorized_cookie(request, &self.options.password)
    }

    fn connect<B>(&self, request: &Request<B>, matched: &router::Match) -> response::Response {
        let browser = request
            .headers()
            .get(header::ACCEPT)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("text/html"));
        let Some((_, code)) = matched.params.iter().find(|(name, _)| name == "code") else {
            return response::empty(StatusCode::NOT_FOUND);
        };
        if !self.pairing.consume(code) {
            if browser {
                return response::bytes(
                    StatusCode::UNAUTHORIZED,
                    "text/plain",
                    "This pairing link expired or was already used. Run `opencode pair` to get a new one.",
                );
            }
            return auth::unauthorized_without_challenge("Pairing link expired or already used");
        }
        let token = auth::issue_session(&self.options.password, crate::now_ms());
        if !browser {
            return response::bytes(
                StatusCode::OK,
                "application/json",
                opencode_contract::codec::encode_json(&PairingSession { token }),
            );
        }
        let host = request.headers().get(header::HOST).and_then(|value| value.to_str().ok());
        let cookie = format!(
            "{}={token}; Max-Age={}; Path=/; HttpOnly; SameSite=Lax",
            auth::session_cookie_name(host),
            auth::SESSION_TTL_SECONDS,
        );
        let mut response = response::empty(StatusCode::FOUND);
        response.headers_mut().insert(header::LOCATION, HeaderValue::from_static("/"));
        if let Ok(cookie) = HeaderValue::from_str(&cookie) {
            response.headers_mut().insert(header::SET_COOKIE, cookie);
        }
        response
    }
}

fn raw_request(request: &Request<Bytes>, path_params: Vec<(String, String)>) -> RawRequest {
    RawRequest {
        path_params,
        query: request.uri().query().unwrap_or_default().to_owned(),
        headers: request
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str().to_owned(), String::from_utf8_lossy(value.as_bytes()).into_owned()))
            .collect(),
        body: request.body().to_vec(),
    }
}

#[allow(clippy::result_large_err)]
fn session_reference(
    services: &crate::services::GlobalServices,
    session_id: &str,
) -> Result<opencode_location::location::LocationRef, response::Response> {
    opencode_location::resolve::resolve_session_location(&services.store, session_id)
        .map_err(crate::services::session_location_error)
}

fn not_implemented(identifier: &str) -> response::Response {
    let body = serde_json::json!({
        "_tag": "UnknownError",
        "message": format!("opencode-server does not implement {identifier} yet"),
    });
    response::json(StatusCode::NOT_IMPLEMENTED, &body)
}

fn forbidden() -> response::Response {
    let body = serde_json::json!({
        "_tag": "ForbiddenError",
        "message": "Cross-site requests to the API are not allowed",
    });
    response::json(StatusCode::FORBIDDEN, &body)
}

fn exempt<B>(request: &Request<B>, ready: bool) -> bool {
    let path = request.uri().path();
    if single_segment(path, "/auth/connect/", "") {
        return true;
    }
    if !ready {
        return false;
    }
    let connect = single_segment(path, "/api/pty/", "/connect")
        || single_segment(path, "/api/experimental/persistent-pty/", "/connect")
        || path == "/api/experimental/client-mcp";
    connect && ticketed(request)
}

fn single_segment(path: &str, prefix: &str, suffix: &str) -> bool {
    let Some(rest) = path.strip_prefix(prefix).and_then(|rest| rest.strip_suffix(suffix)) else {
        return false;
    };
    !rest.is_empty() && !rest.contains('/')
}

fn ticketed<B>(request: &Request<B>) -> bool {
    url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
        .find(|(key, _)| key == "ticket")
        .is_some_and(|(_, value)| !value.is_empty())
}

fn info(options: &Options, state: State) -> response::Response {
    let status = match state {
        State::Failed => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    let body = serde_json::json!({
        "version": options.version,
        "pid": std::process::id(),
        "urls": (options.urls)(),
        "paths": { "tmp": options.tmp.to_string_lossy() },
    });
    let mut response = response::json(status, &body);
    if matches!(state, State::Starting | State::Stopping) {
        response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    }
    response
}

fn server_info(options: &Options) -> response::Response {
    let info = ServerInfo {
        version: options.version.clone(),
        pid: f64::from(std::process::id()),
        hostname: options.hostname.clone().filter(|hostname| !hostname.is_empty()).map(Some),
        urls: (options.urls)(),
        paths: ServerInfoPaths { tmp: options.tmp.to_string_lossy().into_owned() },
        capabilities: Some(Some(ServerInfoCapabilities {
            persistent_pty: Some(Some(opencode_terminal::persistent::available())),
        })),
    };
    response::bytes(StatusCode::OK, "application/json", opencode_contract::codec::encode_json(&info))
}

fn unavailable(state: State) -> response::Response {
    if state == State::Failed {
        let body = serde_json::json!({
            "code": "service_failed",
            "message": "The background service could not start.",
            "action": "Run `opencode service restart` after checking the service logs.",
        });
        return response::json(StatusCode::SERVICE_UNAVAILABLE, &body);
    }
    let code = if state == State::Stopping { "service_stopping" } else { "service_starting" };
    let body = serde_json::json!({ "code": code });
    let mut response = response::json(StatusCode::SERVICE_UNAVAILABLE, &body);
    response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http::Request;

    use super::raw_request;

    #[test]
    fn raw_request_carries_query_headers_body_and_params() {
        let request = Request::builder()
            .method("POST")
            .uri("/api/pair?x=1")
            .header("x-opencode-directory", "/tmp/x")
            .body(Bytes::from_static(br#"{"a":1}"#))
            .expect("a request");
        let raw = raw_request(&request, vec![("id".to_owned(), "one".to_owned())]);
        assert_eq!(raw.query, "x=1");
        assert_eq!(raw.body, br#"{"a":1}"#.to_vec());
        assert_eq!(raw.headers, vec![("x-opencode-directory".to_owned(), "/tmp/x".to_owned())]);
        assert_eq!(raw.path_params, vec![("id".to_owned(), "one".to_owned())]);
    }
}
