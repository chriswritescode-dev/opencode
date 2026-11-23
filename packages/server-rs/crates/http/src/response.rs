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
