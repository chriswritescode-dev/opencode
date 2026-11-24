use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderValue, Request, StatusCode, header};
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

pub const IMPLEMENTED: &[&str] =
    &["server.info", "server.pair", "server.connect", "event.subscribe", "experimental.session.log"];

pub struct Options {
    pub password: String,
    pub version: String,
    pub cors: Vec<String>,
    pub tmp: PathBuf,
    pub hostname: Option<String>,
    pub urls: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    pub state: Option<Arc<crate::state::ServerState>>,
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

    pub async fn handle<B>(&self, request: Request<B>) -> response::Response {
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

    async fn dispatch<B>(&self, request: &Request<B>) -> response::Response {
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
        match found.route.operation_id {
            "server.info" => server_info(&self.options),
            "server.pair" => response::bytes(
                StatusCode::OK,
                "application/json",
                opencode_contract::codec::encode_json(&self.pairing.issue()),
            ),
            "server.connect" => self.connect(request, &found),
            "event.subscribe" => match &self.options.state {
                Some(state) => crate::handlers::event::subscribe(
                    state,
                    &RawRequest {
                        path_params: found.params.clone(),
                        query: request.uri().query().unwrap_or_default().to_owned(),
                        headers: Vec::new(),
                        body: Vec::new(),
                    },
                ),
                None => not_implemented(found.route.identifier),
            },
            "experimental.session.log" => match &self.options.state {
                Some(state) => crate::handlers::session_log::log(
                    state,
                    &RawRequest {
                        path_params: found.params.clone(),
                        query: request.uri().query().unwrap_or_default().to_owned(),
                        headers: Vec::new(),
                        body: Vec::new(),
                    },
                ),
                None => not_implemented(found.route.identifier),
            },
            _ => not_implemented(found.route.identifier),
        }
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
