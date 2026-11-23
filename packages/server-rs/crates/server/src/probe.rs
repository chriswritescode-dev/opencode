use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use base64::Engine;
use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::header;
use hyper_util::rt::TokioIo;
use opencode_local::registration::Registration;
use tokio::net::TcpStream;

pub const TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Ready,
    Waiting,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    Missing,
    TimedOut,
    Incompatible,
    Answered(State),
}

pub async fn probe(registration: &Registration, timeout: Duration) -> Probe {
    match tokio::time::timeout(timeout, fetch(registration)).await {
        Ok(probe) => probe,
        Err(_) => Probe::TimedOut,
    }
}

pub async fn incumbent(file: &Path, url: &str, timeout: Duration) -> Option<Registration> {
    let found = opencode_local::registration::read(file)?;
    let mut candidate = found.clone();
    candidate.url = url.to_owned();
    match probe(&candidate, timeout).await {
        Probe::Answered(_) => Some(found),
        _ => None,
    }
}

async fn fetch(registration: &Registration) -> Probe {
    let Ok(uri) = hyper::Uri::from_str(&format!("{}/api/info", registration.url.trim_end_matches('/'))) else {
        return Probe::Missing;
    };
    if uri.scheme_str() != Some("http") {
        return Probe::Missing;
    }
    let Some(host) = uri.host() else {
        return Probe::Missing;
    };
    let Some(authority) = uri.authority() else {
        return Probe::Missing;
    };
    let Some(path) = uri.path_and_query() else {
        return Probe::Missing;
    };
    let Ok(target) = hyper::Uri::from_str(path.as_str()) else {
        return Probe::Missing;
    };
    let port = uri.port_u16().unwrap_or(80);
    let connect_host = host.strip_prefix('[').and_then(|host| host.strip_suffix(']')).unwrap_or(host);
    let Ok(stream) = TcpStream::connect((connect_host, port)).await else {
        return Probe::Missing;
    };
    let Ok((mut sender, connection)) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await else {
        return Probe::Missing;
    };
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut request =
        hyper::Request::builder().method(hyper::Method::GET).uri(target).header(header::HOST, authority.as_str());
    if let Some(password) = &registration.password {
        let credentials = base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"));
        request = request.header(header::AUTHORIZATION, format!("Basic {credentials}"));
    }
    let Ok(request) = request.body(Empty::<Bytes>::new()) else {
        return Probe::Missing;
    };
    let Ok(response) = sender.send_request(request).await else {
        return Probe::Missing;
    };
    let status = response.status();
    if status == hyper::StatusCode::NOT_FOUND {
        return Probe::Incompatible;
    }
    let Ok(body) = response.into_body().collect().await else {
        return Probe::Missing;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body.to_bytes()) else {
        return Probe::Missing;
    };
    let Some(version) = value.get("version").and_then(serde_json::Value::as_str) else {
        return Probe::Missing;
    };
    let Some(pid) = value
        .get("pid")
        .and_then(serde_json::Value::as_f64)
        .filter(|pid| pid.fract() == 0.0 && *pid >= 0.0 && *pid <= f64::from(u32::MAX))
    else {
        return Probe::Missing;
    };
    if pid as u32 != registration.pid {
        return Probe::Missing;
    }
    if registration.version.as_deref().is_some_and(|expected| version != expected) {
        return Probe::Missing;
    }
    if status.is_success() {
        return Probe::Answered(State::Ready);
    }
    if status == hyper::StatusCode::INTERNAL_SERVER_ERROR {
        return Probe::Answered(State::Failed);
    }
    Probe::Answered(State::Waiting)
}
