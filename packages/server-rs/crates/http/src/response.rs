use bytes::Bytes;
use http::{HeaderValue, StatusCode, header};
use http_body_util::Full;

pub type Response = http::Response<Full<Bytes>>;

pub fn bytes(status: StatusCode, content_type: &'static str, body: impl Into<Bytes>) -> Response {
    let mut response = Response::new(Full::new(body.into()));
    *response.status_mut() = status;
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

pub fn json(status: StatusCode, body: &serde_json::Value) -> Response {
    bytes(status, "application/json", serde_json::to_string(body).unwrap_or_else(|_| "{}".to_owned()))
}

pub fn empty(status: StatusCode) -> Response {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
    response
}

pub fn vary_with(response: &mut Response, dimension: &str) {
    let current = response.headers().get(header::VARY).and_then(|value| value.to_str().ok()).map(str::to_owned);
    let next = match current {
        Some(vary) => {
            let lower = dimension.to_ascii_lowercase();
            let present = vary.split(',').any(|member| {
                let member = member.trim().to_ascii_lowercase();
                member == "*" || member == lower
            });
            if present { vary } else { format!("{vary}, {dimension}") }
        }
        None => dimension.to_owned(),
    };
    if let Ok(value) = HeaderValue::from_str(&next) {
        response.headers_mut().insert(header::VARY, value);
    }
}
