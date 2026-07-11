//! Platform-neutral transport framing shared by the daemon and Tauri shell.
//!
//! The wire format is UTF-8 NDJSON: one complete `CommandEnvelope` or
//! `EventEnvelope` per line, with no embedded raw newlines. Frames are capped at
//! 1 MiB before allocation. Windows listeners must apply a current-user-only
//! DACL and reject remote clients when creating the pipe described by
//! [`NamedPipeSpec`].

use std::{io, marker::PhantomData};

use nectarpilot_contracts::{CommandEnvelope, EventEnvelope, PROTOCOL_VERSION};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf,
    WriteHalf, split,
};

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedPipeSpec {
    pub path: String,
    pub protocol_version: u16,
    pub max_frame_bytes: usize,
    pub reject_remote_clients: bool,
    pub current_user_acl_required: bool,
}

impl NamedPipeSpec {
    /// Produces the stable v1 pipe name for a Windows user identity. Pass the
    /// same `DOMAIN\\username` string from both processes.
    #[must_use]
    pub fn for_user_identity(identity: &str) -> Self {
        let normalized = identity.trim().to_ascii_lowercase();
        let hash = hex::encode(Sha256::digest(normalized.as_bytes()));
        Self {
            path: format!(r"\\.\pipe\nectarpilot-v1-{}", &hash[..16]),
            protocol_version: PROTOCOL_VERSION,
            max_frame_bytes: MAX_FRAME_BYTES,
            reject_remote_clients: true,
            current_user_acl_required: true,
        }
    }

    #[must_use]
    pub fn for_current_environment() -> Self {
        let domain = std::env::var("USERDOMAIN").unwrap_or_default();
        let user = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "unknown-user".into());
        Self::for_user_identity(&format!("{domain}\\{user}"))
    }
}

/// Sending half used by the Tauri shell. It is independent from the event
/// receiver and can be placed in its own async task.
pub struct CommandSender<W> {
    writer: W,
}

impl<W: AsyncWrite + Unpin> CommandSender<W> {
    pub async fn send(&mut self, command: &CommandEnvelope) -> Result<(), TransportError> {
        write_frame(&mut self.writer, command).await
    }

    pub async fn close(&mut self) -> Result<(), TransportError> {
        self.writer.shutdown().await?;
        Ok(())
    }
}

/// Subscription half used by the Tauri shell.
pub struct EventReceiver<R> {
    reader: BufReader<R>,
}

impl<R: AsyncRead + Unpin> EventReceiver<R> {
    pub async fn next(&mut self) -> Result<Option<EventEnvelope>, TransportError> {
        read_frame(&mut self.reader).await
    }
}

/// Splits any full-duplex stream (including a Tokio named-pipe client) into the
/// exact client API needed by the shell: command send + event subscription.
pub fn daemon_client<S>(stream: S) -> (CommandSender<WriteHalf<S>>, EventReceiver<ReadHalf<S>>)
where
    S: AsyncRead + AsyncWrite,
{
    let (reader, writer) = split(stream);
    (
        CommandSender { writer },
        EventReceiver {
            reader: BufReader::new(reader),
        },
    )
}

/// Server-side codec. Read commands and write events over separate handles so
/// the daemon can select between inbound commands and broadcast events.
pub struct CommandReceiver<R> {
    reader: BufReader<R>,
}

impl<R: AsyncRead + Unpin> CommandReceiver<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
        }
    }

    pub async fn next(&mut self) -> Result<Option<CommandEnvelope>, TransportError> {
        read_frame(&mut self.reader).await
    }
}

pub struct EventSender<W> {
    writer: W,
    marker: PhantomData<EventEnvelope>,
}

impl<W: AsyncWrite + Unpin> EventSender<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            marker: PhantomData,
        }
    }

    pub async fn send(&mut self, event: &EventEnvelope) -> Result<(), TransportError> {
        write_frame(&mut self.writer, event).await
    }
}

pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), TransportError>
where
    W: AsyncWrite + Unpin,
    T: serde::Serialize,
{
    let mut payload = serde_json::to_vec(value)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(TransportError::FrameTooLarge {
            size: payload.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }
    // Keep the JSON and delimiter in one OS-level write. This avoids a Windows
    // named-pipe edge case where rapid split writes can expose the delimiter as
    // an empty frame during startup reconnects.
    payload.push(b'\n');
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>, TransportError>
where
    R: AsyncBufRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if frame.is_empty() {
                return Ok(None);
            }
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |position| position + 1);
        let content_end = newline.unwrap_or(available.len());
        if frame.len() + content_end > MAX_FRAME_BYTES {
            reader.consume(consumed);
            return Err(TransportError::FrameTooLarge {
                size: frame.len() + content_end,
                maximum: MAX_FRAME_BYTES,
            });
        }
        frame.extend_from_slice(&available[..content_end]);
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    if frame.last() == Some(&b'\r') {
        frame.pop();
    }
    if frame.is_empty() {
        return Err(TransportError::EmptyFrame);
    }
    Ok(Some(serde_json::from_slice(&frame)?))
}

#[cfg(windows)]
pub fn try_connect_named_pipe(
    spec: &NamedPipeSpec,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, TransportError> {
    use tokio::net::windows::named_pipe::ClientOptions;

    // The server owns ACL enforcement; this client refuses a non-NectarPilot
    // protocol path by construction through `NamedPipeSpec`.
    let client = ClientOptions::new().open(&spec.path)?;
    Ok(client)
}

/// Connects to the per-user daemon pipe, tolerating the normal desktop/daemon
/// startup race and a temporarily busy pipe instance.
#[cfg(windows)]
pub async fn connect_named_pipe(
    spec: &NamedPipeSpec,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, TransportError> {
    const STARTUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

    let started = tokio::time::Instant::now();
    loop {
        match try_connect_named_pipe(spec) {
            Ok(client) => return Ok(client),
            Err(TransportError::Io(error))
                if matches!(error.raw_os_error(), Some(2 | 231))
                    && started.elapsed() < STARTUP_TIMEOUT =>
            {
                tokio::time::sleep(RETRY_DELAY).await;
            }
            Err(error) => return Err(error),
        }
    }
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("transport I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid transport JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("transport frame is empty")]
    EmptyFrame,
    #[error("transport frame is {size} bytes; maximum is {maximum}")]
    FrameTooLarge { size: usize, maximum: usize },
}

#[cfg(test)]
mod tests {
    use nectarpilot_contracts::{Command, CommandEnvelope, DaemonEvent, EventEnvelope};
    use tokio::io::duplex;
    use uuid::Uuid;

    use super::{CommandReceiver, EventSender, NamedPipeSpec, daemon_client};

    #[tokio::test]
    async fn client_and_daemon_exchange_ndjson_envelopes() {
        let (client_stream, daemon_stream) = duplex(8 * 1024);
        let (mut command_sender, mut event_receiver) = daemon_client(client_stream);
        let (daemon_read, daemon_write) = tokio::io::split(daemon_stream);
        let mut command_receiver = CommandReceiver::new(daemon_read);
        let mut event_sender = EventSender::new(daemon_write);

        let command = CommandEnvelope::new(Uuid::nil(), Command::GetSnapshot);
        command_sender.send(&command).await.expect("send command");
        assert_eq!(command_receiver.next().await.expect("read"), Some(command));

        let event = EventEnvelope::new(
            1,
            Uuid::nil(),
            DaemonEvent::CommandAccepted {
                request_id: Uuid::nil(),
            },
        );
        event_sender.send(&event).await.expect("send event");
        assert_eq!(event_receiver.next().await.expect("read"), Some(event));
    }

    #[test]
    fn pipe_name_is_stable_and_versioned() {
        let first = NamedPipeSpec::for_user_identity("DOMAIN\\Alice");
        let second = NamedPipeSpec::for_user_identity("domain\\alice");
        assert_eq!(first, second);
        assert!(first.path.starts_with(r"\\.\pipe\nectarpilot-v1-"));
        assert!(first.current_user_acl_required);
        assert!(first.reject_remote_clients);
    }
}
