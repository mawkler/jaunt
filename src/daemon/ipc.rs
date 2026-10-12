//! The socket the daemon listens on. It keeps a second daemon from starting,
//! and lets `jaunt switch` reach a running one.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// What a `jaunt` invocation asks the running daemon to do.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub(crate) enum Message {
    Switch { app: String, app_id: Option<String> },
}

fn socket_path() -> PathBuf {
    runtime_path(".sock")
}

/// Somewhere in the runtime directory that belongs to this daemon: one per
/// Wayland display, since that is how many daemons there can be.
pub(crate) fn runtime_path(suffix: &str) -> PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_owned());
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_owned());

    PathBuf::from(runtime_dir).join(format!("jaunt-{display}{suffix}"))
}

/// The one running daemon. Dropping it takes its socket away.
pub(crate) struct Instance {
    listener: UnixListener,
    path: PathBuf,
}

impl Instance {
    /// Claims the socket, or fails if another daemon already holds it.
    pub(crate) fn acquire() -> Result<Self> {
        let path = socket_path();

        anyhow::ensure!(
            !is_listening(&path),
            "a Jaunt daemon is already running ({})",
            path.display()
        );

        // Nothing answered, so any socket left at that path is from a daemon
        // that didn't get to clean up after itself.
        let _ = std::fs::remove_file(&path);

        let listener = UnixListener::bind(&path)
            .with_context(|| format!("failed to listen on {}", path.display()))?;

        Ok(Self { listener, path })
    }

    pub(crate) fn listener(&self) -> &UnixListener {
        &self.listener
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn is_listening(path: &Path) -> bool {
    path.exists() && UnixStream::connect(path).is_ok()
}

/// Reads one message from a client, if it sent a well-formed one.
pub(crate) fn receive(stream: UnixStream) -> Option<Message> {
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;

    // A daemon starting up connects and hangs up again to find out whether
    // one is already running, which isn't worth a word.
    if line.trim().is_empty() {
        return None;
    }

    match serde_json::from_str(&line) {
        Ok(message) => Some(message),
        Err(error) => {
            eprintln!("jaunt: ignoring an unreadable message: {error}");
            None
        }
    }
}

/// Asks the running daemon to switch to `app`.
///
/// # Errors
///
/// Returns an error if no daemon is running, or if it couldn't be reached.
pub(crate) fn switch(app: &str, app_id: Option<&str>) -> Result<()> {
    let path = socket_path();

    let mut daemon = UnixStream::connect(&path).map_err(|error| {
        anyhow::anyhow!(
            "no jaunt daemon is running ({error}); start one with `jaunt daemon`, \
             for example from your Hyprland startup configuration"
        )
    })?;

    let message = Message::Switch {
        app: app.to_owned(),
        app_id: app_id.map(str::to_owned),
    };
    let message = serde_json::to_string(&message).context("failed to encode the message")?;

    writeln!(daemon, "{message}").context("failed to send the message to the Jaunt daemon")
}
