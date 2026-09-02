use std::io::Write;
use std::os::unix::net::{self};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    PathBuf::from(runtime).join(format!("raisin-{display}.sock"))
}

/// A command sent from the raisin client to the daemon.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Forward { app: String, app_id: Option<String> },
    Backward { app: String, app_id: Option<String> },
    Confirm,
}

/// Sends a message to the daemon, if one is running.
///
/// Returns `Ok(false)` if no daemon is listening.
///
/// # Errors
///
/// Returns an error if the message could not be serialized or written to the
/// socket.
pub fn send(message: &Message) -> Result<bool> {
    let path = socket_path();

    if !path.exists() {
        return Ok(false);
    }

    match net::UnixStream::connect(&path) {
        Ok(mut stream) => {
            let payload = serde_json::to_string(message).context("failed to serialize message")?;
            writeln!(stream, "{payload}").context("failed to write to Unix socket")?;
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => Ok(false),
        Err(err) => anyhow::bail!("failed to connect to Unix socket: {err}"),
    }
}

/// Returns `true` if a daemon is currently listening on the socket.
#[must_use]
pub fn is_running() -> bool {
    let path = socket_path();

    if !path.exists() {
        return false;
    }

    match net::UnixStream::connect(&path) {
        Ok(_) => true,
        Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => false,
        Err(_) => false,
    }
}

/// Binds the socket listener used by the daemon.
///
/// # Errors
///
/// Returns an error if the stale socket could not be removed or if the socket
/// could not be bound or made non-blocking.
pub fn start_listener() -> Result<net::UnixListener> {
    let path = socket_path();

    if path.exists() {
        std::fs::remove_file(&path).context("failed to remove stale socket")?;
    }

    let listener = net::UnixListener::bind(&path).context("failed to bind Unix socket")?;
    listener
        .set_nonblocking(true)
        .context("failed to make socket listener non-blocking")?;

    Ok(listener)
}
