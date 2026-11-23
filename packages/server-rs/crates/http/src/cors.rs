use http::{HeaderMap, HeaderValue, Request, StatusCode, header};
use url::Url;

use crate::response;

const ALLOW_METHODS: &str = "GET, HEAD, PUT, PATCH, POST, DELETE";
const MAX_AGE: &str = "86400";

pub fn is_allowed_cors_origin(origin: Option<&str>, extra: &[String]) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    if origin.starts_with("http://localhost:")
        || origin.starts_with("http://127.0.0.1:")
        || origin.starts_with("oc://renderer")
        || matches!(origin, "tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost")
        || is_opencode_origin(origin)
    {
        return true;
    }
    extra.iter().any(|allowed| allowed == origin)
}

pub fn is_allowed_request_origin(origin: Option<&str>, host: Option<&str>, extra: &[String]) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    if origin.is_empty() {
        return true;
    }
    if host.is_some_and(|host| same_host(origin, host)) {
        return true;
    }
    is_allowed_cors_origin(Some(origin), extra)
}

pub fn preflight<B>(request: &Request<B>, extra: &[String]) -> response::Response {
    let mut response = response::empty(StatusCode::NO_CONTENT);
    if let Some(origin) = allowed_origin(request.headers(), extra) {
        response.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
    }
    response.headers_mut().insert(header::VARY, HeaderValue::from_static("Origin"));
    response.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static(ALLOW_METHODS));
    response.headers_mut().insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static(MAX_AGE));
    let requested = request.headers().get(header::ACCESS_CONTROL_REQUEST_HEADERS);
    let allow_headers = requested.cloned().unwrap_or_else(|| HeaderValue::from_static(""));
    response.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_HEADERS, allow_headers);
    if requested.is_some() {
        response::vary_with(&mut response, "Access-Control-Request-Headers");
    }
    response
}

pub fn apply<B>(request: &Request<B>, mut response: response::Response, extra: &[String]) -> response::Response {
    if let Some(origin) = allowed_origin(request.headers(), extra) {
        response.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
    }
    response::vary_with(&mut response, "Origin");
    response
}

fn allowed_origin<'a>(headers: &'a HeaderMap, extra: &[String]) -> Option<&'a HeaderValue> {
    let origin = headers.get(header::ORIGIN)?;
    origin.to_str().is_ok_and(|value| is_allowed_cors_origin(Some(value), extra)).then_some(origin)
}

fn is_opencode_origin(origin: &str) -> bool {
    let Some(rest) = origin.strip_prefix("https://") else {
        return false;
    };
    let Some(labels) = rest.strip_suffix("opencode.ai") else {
        return false;
    };
    if labels.is_empty() {
        return true;
    }
    let Some(labels) = labels.strip_suffix('.') else {
        return false;
    };
    labels.split('.').all(|label| {
        !label.is_empty()
            && label.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    })
}

fn same_host(origin: &str, host: &str) -> bool {
    let Ok(url) = Url::parse(origin) else {
        return false;
    };
    let Some(hostname) = url.host() else {
        return false;
    };
    let hostname = match hostname {
        url::Host::Ipv6(address) => format!("[{address}]"),
        url::Host::Ipv4(address) => address.to_string(),
        url::Host::Domain(domain) => domain.to_owned(),
    };
    match url.port() {
        Some(port) => format!("{hostname}:{port}") == host,
        None => hostname == host,
    }
}
