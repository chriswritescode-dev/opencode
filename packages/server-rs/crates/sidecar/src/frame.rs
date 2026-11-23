//! The carrier's frames: a length (`u32`, little-endian), a kind byte and the payload. The length is checked
//! against `max_frame` before anything is allocated, so a header that says 4 GiB costs nothing. A frame cut short,
//! of an unknown kind or over its cap breaks the stream: nothing after it can be trusted, and the connection fails.

use std::fmt;
use std::io;

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::protocol::Message;

/// One JSON `Message`.
pub const JSON: u8 = 0;
/// A JSON `Message` followed by raw bytes attached to it.
pub const BINARY: u8 = 1;

/// The largest frame both sides accept unless `hello` says otherwise.
pub const DEFAULT_MAX_FRAME: usize = 16 << 20;

const HEADER: usize = 5;

/// Why a stream of frames broke.
#[derive(Debug, PartialEq, Eq)]
pub enum Broken {
    /// The stream ended inside a frame: `got` of the `want` bytes of its header and payload.
    Truncated {
        got: usize,
        want: usize,
    },
    UnknownKind(u8),
    TooLarge {
        kind: u8,
        len: usize,
    },
    BadJson(String),
    Io(String),
}

impl fmt::Display for Broken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Broken::Truncated { got, want } => write!(f, "a frame cut short: {got} of {want} bytes"),
            Broken::UnknownKind(kind) => write!(f, "a frame of unknown kind {kind}"),
            Broken::TooLarge { kind, len } => write!(f, "a frame of kind {kind} and {len} bytes, over its cap"),
            Broken::BadJson(error) => write!(f, "a frame whose JSON did not parse: {error}"),
            Broken::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Broken {}

/// Why a message could not be framed.
#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    TooLarge { len: usize, max: usize },
    Json(String),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::TooLarge { len, max } => write!(f, "a frame of {len} bytes, over its cap of {max}"),
            FrameError::Json(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// One frame as read: the message and, for `BINARY`, the bytes attached to it.
#[derive(Debug, PartialEq)]
pub struct Frame {
    pub message: Message,
    pub bytes: Option<Bytes>,
}

/// One frame as bytes, header included. The frame vector is not allocated when the payload would exceed
/// `max_frame`.
pub fn encode(message: &Message, bytes: Option<&[u8]>, max_frame: usize) -> Result<Vec<u8>, FrameError> {
    let json = serde_json::to_vec(message).map_err(|error| FrameError::Json(error.to_string()))?;
    let kind = if bytes.is_some() { BINARY } else { JSON };
    let len = json.len() + bytes.map_or(0, |bytes| bytes.len() + 4);
    let max = max_frame.min(u32::MAX as usize);
    if len > max {
        return Err(FrameError::TooLarge { len, max });
    }
    let mut out = Vec::with_capacity(len + HEADER);
    out.extend_from_slice(&(len as u32).to_le_bytes());
    out.push(kind);
    if bytes.is_some() {
        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    }
    out.extend_from_slice(&json);
    if let Some(bytes) = bytes {
        out.extend_from_slice(bytes);
    }
    Ok(out)
}

/// Reads the next frame: `Ok(None)` at a clean end of the stream (between frames).
pub async fn read<R>(reader: &mut R, max_frame: usize) -> Result<Option<Frame>, Broken>
where
    R: AsyncRead + Unpin,
{
    let mut head = [0u8; HEADER];
    let got = fill(reader, &mut head).await?;
    if got == 0 {
        return Ok(None);
    }
    if got < HEADER {
        return Err(Broken::Truncated { got, want: HEADER });
    }
    let len = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
    let kind = head[4];
    if kind != JSON && kind != BINARY {
        return Err(Broken::UnknownKind(kind));
    }
    if len > max_frame {
        return Err(Broken::TooLarge { kind, len });
    }
    let mut payload = vec![0u8; len];
    let got = fill(reader, &mut payload).await?;
    if got < len {
        return Err(Broken::Truncated { got: HEADER + got, want: HEADER + len });
    }
    if kind == JSON {
        let message = serde_json::from_slice(&payload).map_err(|error| Broken::BadJson(error.to_string()))?;
        return Ok(Some(Frame { message, bytes: None }));
    }
    let payload = Bytes::from(payload);
    if payload.len() < 4 {
        return Err(Broken::BadJson("a binary frame without its JSON length".to_owned()));
    }
    let json_len = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
    if json_len > payload.len() - 4 {
        return Err(Broken::BadJson("a binary frame whose JSON runs past its payload".to_owned()));
    }
    let message =
        serde_json::from_slice(&payload[4..4 + json_len]).map_err(|error| Broken::BadJson(error.to_string()))?;
    Ok(Some(Frame { message, bytes: Some(payload.slice(4 + json_len..)) }))
}

/// Reads until `buffer` is full or the stream ends; how many bytes it read.
async fn fill<R>(reader: &mut R, buffer: &mut [u8]) -> Result<usize, Broken>
where
    R: AsyncRead + Unpin,
{
    let mut got = 0;
    while got < buffer.len() {
        match reader.read(&mut buffer[got..]).await {
            Ok(0) => break,
            Ok(read) => got += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(Broken::Io(error.to_string())),
        }
    }
    Ok(got)
}
