use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderValue, Request, StatusCode, header};
use opencode_contract::generated::{ServerInfo, ServerInfoCapabilities, ServerInfoPaths};

use crate::auth;
use crate::response;
use crate::router;
use crate::status::{State, Status};

pub const IMPLEMENTED: &[&str] = &["server.info"];

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
}

impl App {
    pub fn new(options: Options) -> Arc<App> {
        Arc::new(App { options, status: Status::new() })
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub async fn handle(&self, request: Request<Bytes>) -> response::Response {
        let state = self.status.current();
        let ready = state == State::Ready;
        if request.method() == http::Method::GET && request.uri().path() == "/api/info" && !ready {
            if !auth::authorized(&auth::credential(&request), &self.options.password) {
                return auth::unauthorized(&request, "Authentication required");
            }
            return info(&self.options, state);
        }
        if !exempt(&request, ready) && !auth::authorized(&auth::credential(&request), &self.options.password) {
            return auth::unauthorized(&request, "Authentication required");
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
        if found.route.operation_id == "server.info" {
            return server_info(&self.options);
        }
        let body = serde_json::json!({
            "_tag": "UnknownError",
            "message": format!("opencode-server does not implement {} yet", found.route.identifier),
        });
        response::json(StatusCode::NOT_IMPLEMENTED, &body)
    }
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
