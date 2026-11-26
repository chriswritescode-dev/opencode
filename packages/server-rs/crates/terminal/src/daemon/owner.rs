use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, oneshot, watch};

use crate::daemon::protocol::{Registration, Request, Response};
use crate::daemon::{DaemonError, transport};

struct OwnerRequest {
    request: Request,
    reply: oneshot::Sender<Result<Response, DaemonError>>,
}

pub struct Owner {
    requests: mpsc::Sender<OwnerRequest>,
    closed: watch::Receiver<bool>,
}

impl Clone for Owner {
    fn clone(&self) -> Self {
        Owner { requests: self.requests.clone(), closed: self.closed.clone() }
    }
}

impl Owner {
    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    pub async fn exchange(&self, request: Request) -> Result<Response, DaemonError> {
        let (reply, response) = oneshot::channel();
        if self.requests.send(OwnerRequest { request, reply }).await.is_err() {
            return Err(DaemonError::connect("PTY ownership connection closed"));
        }
        match response.await {
            Ok(result) => result,
            Err(_) => Err(DaemonError::connect("PTY ownership connection closed")),
        }
    }
}

pub async fn open(registration: &Registration, ticket: Option<&str>) -> Result<Owner, DaemonError> {
    let mut stream =
        UnixStream::connect(&registration.socket).await.map_err(|error| DaemonError::connect(error.to_string()))?;
    let request = Request::Own { instance_id: registration.instance_id.clone(), ticket: ticket.map(str::to_owned) };
    transport::write_frame(&mut stream, &registration.token, &request)
        .await
        .map_err(|error| DaemonError::connect(error.to_string()))?;
    let frame = transport::read_frame(&mut stream).await.map_err(|error| DaemonError::connect(error.to_string()))?;
    let Some(frame) = frame else {
        return Err(DaemonError::connect("PTY ownership connection closed"));
    };
    let response: Response = serde_json::from_slice(&frame).map_err(|error| DaemonError::connect(error.to_string()))?;
    match response {
        Response::Error { message } => return Err(DaemonError::response(message)),
        Response::Owned => {}
        _ => return Err(DaemonError::response("PTY daemon does not support server ownership")),
    }
    let (read, write) = stream.into_split();
    let (requests, receiver) = mpsc::channel(8);
    let (closed, closed_rx) = watch::channel(false);
    tokio::spawn(serve(read, write, receiver, closed, registration.token.clone()));
    Ok(Owner { requests, closed: closed_rx })
}

async fn serve(
    mut read: OwnedReadHalf,
    mut write: OwnedWriteHalf,
    mut requests: mpsc::Receiver<OwnerRequest>,
    closed: watch::Sender<bool>,
    token: String,
) {
    while let Some(incoming) = next_request(&mut read, &mut requests).await {
        let result = exchange(&mut read, &mut write, &token, &incoming.request).await;
        let failed = result.is_err();
        if incoming.reply.send(result).is_err() || failed {
            break;
        }
    }
    let _ = closed.send(true);
}

async fn next_request(read: &mut OwnedReadHalf, requests: &mut mpsc::Receiver<OwnerRequest>) -> Option<OwnerRequest> {
    tokio::select! {
        request = requests.recv() => request,
        frame = transport::read_frame(read) => {
            let _ = frame;
            None
        }
    }
}

async fn exchange(
    read: &mut OwnedReadHalf,
    write: &mut OwnedWriteHalf,
    token: &str,
    request: &Request,
) -> Result<Response, DaemonError> {
    let payload = transport::encode(token, request).map_err(|error| DaemonError::response(error.to_string()))?;
    write.write_all(&payload).await.map_err(|error| DaemonError::response(error.to_string()))?;
    let frame = transport::read_frame(read).await.map_err(|error| DaemonError::response(error.to_string()))?;
    let Some(frame) = frame else {
        return Err(DaemonError::response("PTY daemon closed its ownership connection"));
    };
    let response: Response =
        serde_json::from_slice(&frame).map_err(|error| DaemonError::response(error.to_string()))?;
    if let Response::Error { message } = response {
        return Err(DaemonError::response(message));
    }
    Ok(response)
}
