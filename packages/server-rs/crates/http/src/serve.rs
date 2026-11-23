use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;

use http_body_util::BodyExt;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::JoinSet;

use crate::app::App;
use crate::response;

pub async fn serve(listener: TcpListener, app: Arc<App>, shutdown: impl Future<Output = ()>) {
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let app = app.clone();
                connections.spawn(async move {
                    let service = service_fn(move |request| {
                        let app = app.clone();
                        async move { Ok::<_, Infallible>(dispatch(app, request).await) }
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

async fn dispatch(app: Arc<App>, request: http::Request<hyper::body::Incoming>) -> response::Response {
    let (parts, body) = request.into_parts();
    match body.collect().await {
        Ok(collected) => app.handle(http::Request::from_parts(parts, collected.to_bytes())).await,
        Err(_) => response::empty(http::StatusCode::BAD_REQUEST),
    }
}
