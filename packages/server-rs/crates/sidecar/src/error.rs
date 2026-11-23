//! Why a sidecar operation failed, from a missing runtime to a broken frame.

use std::fmt;
use std::time::Duration;

use crate::protocol::RemoteError;

#[derive(Debug, Clone)]
pub enum SidecarError {
    BunNotFound,
    Spawn(String),
    Handshake(String),
    Exited { status: Option<i32>, signal: Option<i32>, stderr: Vec<String> },
    Unresponsive,
    MemoryExceeded { rss: u64 },
    ProtocolBroken(String),
    TooLarge { len: usize, max: usize },
    Busy,
    Cancelled,
    Remote(RemoteError),
    Unavailable { retry_in: Duration },
    Disabled { crashes: usize },
    ShuttingDown,
}

impl fmt::Display for SidecarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SidecarError::BunNotFound => write!(f, "no Bun runtime was found"),
            SidecarError::Spawn(message) => write!(f, "the sidecar could not start: {message}"),
            SidecarError::Handshake(message) => write!(f, "the sidecar handshake failed: {message}"),
            SidecarError::Exited { status, signal, stderr } => {
                write!(f, "the sidecar exited")?;
                if let Some(status) = status {
                    write!(f, " with status {status}")?;
                }
                if let Some(signal) = signal {
                    write!(f, " on signal {signal}")?;
                }
                if !stderr.is_empty() {
                    write!(f, "; its last stderr: {}", stderr.join("\n"))?;
                }
                Ok(())
            }
            SidecarError::Unresponsive => write!(f, "the sidecar stopped answering"),
            SidecarError::MemoryExceeded { rss } => write!(f, "the sidecar used {rss} bytes of memory"),
            SidecarError::ProtocolBroken(reason) => write!(f, "the sidecar protocol broke: {reason}"),
            SidecarError::TooLarge { len, max } => write!(f, "a frame of {len} bytes, over its cap of {max}"),
            SidecarError::Busy => write!(f, "the sidecar has too many calls in flight"),
            SidecarError::Cancelled => write!(f, "the call was cancelled"),
            SidecarError::Remote(error) => write!(f, "the sidecar answered {}: {}", error.tag, error.message),
            SidecarError::Unavailable { retry_in } => write!(f, "the sidecar is restarting, retry in {retry_in:?}"),
            SidecarError::Disabled { crashes } => write!(f, "the sidecar is disabled after {crashes} crashes"),
            SidecarError::ShuttingDown => write!(f, "the sidecar is shutting down"),
        }
    }
}

impl std::error::Error for SidecarError {}
