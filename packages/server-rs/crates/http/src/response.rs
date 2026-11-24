use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::Stream;
use http::{HeaderName, HeaderValue, StatusCode, header};
use http_body_util::Full;

pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, io::Error>> + Send>>;

pub enum Body {
    Full(Full<Bytes>),
    Stream(ByteStream),
}

impl Body {
    pub fn empty() -> Self {
        Body::Full(Full::new(Bytes::new()))
    }
}

impl http_body::Body for Body {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        match self.get_mut() {
            Body::Full(full) => {
                Pin::new(full).poll_frame(cx).map(|frame| frame.map(|result| result.map_err(|never| match never {})))
            }
            Body::Stream(stream) => {
                stream.as_mut().poll_next(cx).map(|frame| frame.map(|result| result.map(http_body::Frame::data)))
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Body::Full(full) => full.is_end_stream(),
            Body::Stream(_) => false,
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match self {
            Body::Full(full) => full.size_hint(),
            Body::Stream(_) => http_body::SizeHint::default(),
        }
    }
}

pub type Response = http::Response<Body>;

pub fn bytes(status: StatusCode, content_type: &'static str, body: impl Into<Bytes>) -> Response {
    let mut response = Response::new(Body::Full(Full::new(body.into())));
    *response.status_mut() = status;
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

pub fn json(status: StatusCode, body: &serde_json::Value) -> Response {
    bytes(status, "application/json", serde_json::to_string(body).unwrap_or_else(|_| "{}".to_owned()))
}

pub fn empty(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

pub fn stream(
    status: StatusCode,
    content_type: &'static str,
    headers: &[(&'static str, &'static str)],
    stream: impl Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
) -> Response {
    let mut response = Response::new(Body::Stream(Box::pin(stream)));
    *response.status_mut() = status;
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    for (name, value) in headers {
        response.headers_mut().insert(HeaderName::from_static(name), HeaderValue::from_static(value));
    }
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
