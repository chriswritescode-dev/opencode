use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use rustix::io::Errno;
use tokio::net::TcpListener;
use tokio::task::JoinSet;

use crate::app::App;

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
    let (parts, mut body) = request.into_parts();
    tokio::spawn(async move { while body.frame().await.is_some_and(|frame| frame.is_ok()) {} });
    app.handle(http::Request::from_parts(parts, ())).await
}

fn exhausted(error: &std::io::Error) -> bool {
    error
        .raw_os_error()
        .map(Errno::from_raw_os_error)
        .is_some_and(|error| matches!(error, Errno::MFILE | Errno::NFILE | Errno::NOMEM | Errno::NOBUFS))
}
