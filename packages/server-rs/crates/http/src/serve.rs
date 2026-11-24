use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use rustix::io::Errno;
use tokio::net::TcpListener;
use tokio::task::JoinSet;

use crate::app::App;

const MAX_REQUEST_BODY: usize = 128 * 1024 * 1024;

pub async fn serve(listener: TcpListener, app: Arc<App>, shutdown: impl Future<Output = ()>) {
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            accepted = listener.accept() => {
                let stream = match accepted {
                    Ok((stream, _)) => stream,
                    Err(error) => {
                        if exhausted(&error) {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        continue;
                    }
                };
                let app = app.clone();
                connections.spawn(async move {
                    let service = service_fn(move |request| {
                        let app = app.clone();
                        async move { Ok::<_, Infallible>(dispatch(&app, request).await) }
                    });
                    let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
                });
            }
            () = &mut shutdown => break,
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}

async fn dispatch(app: &App, request: http::Request<Incoming>) -> crate::response::Response {
    let (parts, body) = request.into_parts();
    let declared = parts
        .headers
        .get(http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > MAX_REQUEST_BODY as u64) {
        return crate::response::empty(http::StatusCode::PAYLOAD_TOO_LARGE);
    }
    let bytes = match BodyExt::collect(Limited::new(body, MAX_REQUEST_BODY)).await {
        Ok(collected) => collected.to_bytes(),
        Err(error) if error.downcast_ref::<LengthLimitError>().is_some() => {
            return crate::response::empty(http::StatusCode::PAYLOAD_TOO_LARGE);
        }
        Err(_) => return crate::response::empty(http::StatusCode::BAD_REQUEST),
    };
    app.handle(http::Request::from_parts(parts, bytes)).await
}

fn exhausted(error: &std::io::Error) -> bool {
    error
        .raw_os_error()
        .map(Errno::from_raw_os_error)
        .is_some_and(|error| matches!(error, Errno::MFILE | Errno::NFILE | Errno::NOMEM | Errno::NOBUFS))
}
