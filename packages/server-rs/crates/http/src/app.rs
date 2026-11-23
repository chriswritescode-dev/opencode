use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderValue, Request, StatusCode, header};
use opencode_contract::generated::{PairingSession, ServerInfo, ServerInfoCapabilities, ServerInfoPaths};

use crate::auth;
use crate::compression;
use crate::cors;
use crate::pairing::Pairing;
use crate::request_site;
use crate::response;
use crate::router;
use crate::status::{State, Status};
use crate::ticket::TicketStore;

pub const IMPLEMENTED: &[&str] = &["server.info", "server.pair", "server.connect"];

pub struct Options {
    pub password: String,
    pub version: String,
    pub cors: Vec<String>,
    pub tmp: PathBuf,
    pub hostname: Option<String>,
    pub urls: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
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

    pub async fn handle(&self, request: Request<Bytes>) -> response::Response {
        if request.method() == http::Method::OPTIONS {
            return cors::preflight(&request, &self.options.cors);
        }
        let response = if request_site::rejected(&request, &self.options.cors) {
            forbidden()
        } else {
            self.dispatch(&request).await
        };
        let response = cors::apply(&request, response, &self.options.cors);
        let accept_encoding = request.headers().get(header::ACCEPT_ENCODING).and_then(|value| value.to_str().ok());
        compression::apply(accept_encoding, response)
    }

    async fn dispatch(&self, request: &Request<Bytes>) -> response::Response {
        let state = self.status.current();
        let ready = state == State::Ready;
        if request.method() == http::Method::GET && request.uri().path() == "/api/info" && !ready {
            if !self.authorized(request) {
                return auth::unauthorized(request, "Authentication required");
            }
            return info(&self.options, state);
        }
        if !exempt(request, ready) && !self.authorized(request) {
            return auth::unauthorized(request, "Authentication required");
        }
        if !ready {
            return unavailable(state);
        }
        if request.method() == http::Method::GET && request.uri().path() == "/openapi.json" {
            return response::bytes(
                StatusCode::OK,
                "application/json",
                Bytes::from_static(opencode_contract::SERVED_OPENAPI_JSON.as_bytes()),
            );
        }
        let Some(found) = router::find(request.method(), request.uri().path()) else {
            return response::empty(StatusCode::NOT_FOUND);
        };
        match found.route.operation_id {
            "server.info" => server_info(&self.options),
            "server.pair" => response::bytes(
                StatusCode::OK,
                "application/json",
                opencode_contract::codec::encode_json(&self.pairing.issue()),
            ),
            "server.connect" => self.connect(request, &found),
            _ => {
                let body = serde_json::json!({
                    "_tag": "UnknownError",
                    "message": format!("opencode-server does not implement {} yet", found.route.identifier),
                });
                response::json(StatusCode::NOT_IMPLEMENTED, &body)
            }
        }
    }

    fn authorized(&self, request: &Request<Bytes>) -> bool {
        auth::authorized(&auth::credential(request), &self.options.password)
            || auth::authorized_cookie(request, &self.options.password)
    }

    fn connect(&self, request: &Request<Bytes>, matched: &router::Match) -> response::Response {
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

fn forbidden() -> response::Response {
    let body = serde_json::json!({
        "_tag": "ForbiddenError",
        "message": "Cross-site requests to the API are not allowed",
    });
    response::json(StatusCode::FORBIDDEN, &body)
}

fn exempt(request: &Request<Bytes>, ready: bool) -> bool {
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

fn ticketed(request: &Request<Bytes>) -> bool {
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
    let hostname = hostname();
    let info = ServerInfo {
        version: options.version.clone(),
        pid: f64::from(std::process::id()),
        hostname: (!hostname.is_empty()).then_some(Some(hostname)),
        urls: (options.urls)(),
        paths: ServerInfoPaths { tmp: options.tmp.to_string_lossy().into_owned() },
        capabilities: Some(Some(ServerInfoCapabilities { persistent_pty: Some(Some(false)) })),
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

#[cfg(unix)]
fn hostname() -> String {
    rustix::system::uname().nodename().to_string_lossy().into_owned()
}

#[cfg(not(unix))]
fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}
