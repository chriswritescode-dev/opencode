use std::convert::Infallible;
use std::io;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

type Handler = dyn Fn(Request) -> (u16, &'static str, String) + Send + Sync;

pub struct Request {
    pub method: String,
    pub path: String,
    pub body: Bytes,
}

pub struct CallbackServer {
    port: u16,
    task: JoinHandle<()>,
}

impl CallbackServer {
    pub async fn bind(
        host: &str,
        port: u16,
        handler: impl Fn(Request) -> (u16, &'static str, String) + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind((host, port)).await?;
        let port = listener.local_addr()?.port();
        let handler: Arc<Handler> = Arc::new(handler);
        let task = tokio::spawn(accept(listener, handler));
        Ok(Self { port, task })
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for CallbackServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn accept(listener: TcpListener, handler: Arc<Handler>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let handler = Arc::clone(&handler);
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let handler = Arc::clone(&handler);
                async move { Ok::<_, Infallible>(respond(handler.as_ref(), request).await) }
            });
            let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
        });
    }
}

async fn respond(handler: &Handler, request: hyper::Request<Incoming>) -> hyper::Response<Full<Bytes>> {
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let Ok(collected) = BodyExt::collect(Limited::new(request.into_body(), 64 * 1024)).await else {
        return reply(413, "text/plain", "Payload too large".to_owned());
    };
    let (status, content_type, body) = handler(Request { method, path, body: collected.to_bytes() });
    reply(status, content_type, body)
}

fn reply(status: u16, content_type: &str, body: String) -> hyper::Response<Full<Bytes>> {
    let status = hyper::StatusCode::from_u16(status).unwrap_or(hyper::StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = hyper::Response::builder().status(status).header(hyper::header::CONNECTION, "close");
    if !content_type.is_empty() {
        builder = builder.header(hyper::header::CONTENT_TYPE, content_type);
    }
    let Ok(response) = builder.body(Full::new(Bytes::from(body))) else {
        let mut fallback = hyper::Response::new(Full::new(Bytes::new()));
        *fallback.status_mut() = status;
        fallback.headers_mut().insert(hyper::header::CONNECTION, hyper::header::HeaderValue::from_static("close"));
        return fallback;
    };
    response
}
