//! The Quickshell process that draws the switcher, and the socket the daemon
//! talks to it over.
//!
//! The daemon only ever writes to it, one line of JSON at a time, and never
//! waits for anything to come back: the writing happens on a thread of its
//! own, so a view that is slow, stuck or starting up again can only make the
//! switcher late on screen, never a switch late.

use std::cell::Cell;
use std::io::{self, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Command, ExitStatus};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rustix::process::{Pid, Signal};

use super::ipc;

/// The switcher's QML, built into the binary so that there is nothing to
/// install alongside it.
const SHELL: &[(&str, &str)] = &[
    ("shell.qml", include_str!("../../shell/shell.qml")),
    ("Theme.qml", include_str!("../../shell/Theme.qml")),
    ("Switcher.qml", include_str!("../../shell/Switcher.qml")),
    ("AppBar.qml", include_str!("../../shell/AppBar.qml")),
    ("Sheet.qml", include_str!("../../shell/Sheet.qml")),
    ("WindowPage.qml", include_str!("../../shell/WindowPage.qml")),
    ("WindowCard.qml", include_str!("../../shell/WindowCard.qml")),
    ("Keycap.qml", include_str!("../../shell/Keycap.qml")),
    ("AppIcon.qml", include_str!("../../shell/AppIcon.qml")),
    ("Splash.qml", include_str!("../../shell/Splash.qml")),
];

/// A directory to run the shell from instead of the built-in one, for working
/// on it: Quickshell reloads it every time a file in it is saved.
const SHELL_OVERRIDE: &str = "JAUNT_SHELL";

/// How the view finds the socket the daemon talks to it on.
const SOCKET_VARIABLE: &str = "JAUNT_VIEW_SOCKET";

/// How long the view has to stay up to count as having started properly,
/// rather than as one more failure in a row.
const STEADY: Duration = Duration::from_secs(10);

/// The longest the daemon waits before starting a view that keeps failing.
const PATIENCE: Duration = Duration::from_secs(30);

/// What the writing thread is told.
enum Outgoing {
    /// The view connected, again or for the first time: what follows goes to
    /// it.
    Connected(UnixStream),
    /// One message, newline and all.
    Line(String),
}

pub(crate) struct Quickshell {
    program: PathBuf,
    shell: PathBuf,
    socket: PathBuf,
    outgoing: mpsc::Sender<Outgoing>,
    pid: Cell<Option<Pid>>,
    started: Cell<Instant>,
    /// How many times in a row the view has died soon after starting, which
    /// is how long to wait before starting it again.
    failures: Cell<u32>,
    stopping: Cell<bool>,
}

impl Quickshell {
    /// Starts the view, and says each time it connects: whatever it was told
    /// before that, it has to be told again.
    pub(crate) fn start() -> Result<(Rc<Self>, async_channel::Receiver<()>)> {
        let program = ["qs", "quickshell"]
            .into_iter()
            .find_map(on_path)
            .context("it is drawn by Quickshell, and neither `qs` nor `quickshell` is on PATH")?;
        let shell = shell().context("failed to write out the switcher's QML")?;

        let socket = ipc::runtime_path("-view.sock");
        // Only one daemon runs per display, so anything at this path was left
        // behind by one that is gone.
        let _ = std::fs::remove_file(&socket);
        let listener = UnixListener::bind(&socket)
            .with_context(|| format!("failed to listen on {}", socket.display()))?;

        let (outgoing, lines) = mpsc::channel();
        thread::spawn(move || write(&lines));

        let (connected, connections) = async_channel::unbounded();
        let forward = outgoing.clone();

        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };

                // The writer hears about the connection first, so that
                // whatever the daemon says once it hears about it lands on
                // this connection rather than the last one.
                if forward.send(Outgoing::Connected(stream)).is_err()
                    || connected.send_blocking(()).is_err()
                {
                    break;
                }
            }
        });

        let quickshell = Rc::new(Self {
            program,
            shell,
            socket,
            outgoing,
            pid: Cell::new(None),
            started: Cell::new(Instant::now()),
            failures: Cell::new(0),
            stopping: Cell::new(false),
        });

        quickshell.spawn();

        Ok((quickshell, connections))
    }

    /// Hands a message to the view, or to nobody if it isn't connected.
    pub(crate) fn send(&self, mut line: String) {
        line.push('\n');
        let _ = self.outgoing.send(Outgoing::Line(line));
    }

    /// Takes the view down for good, for a daemon that is stopping.
    pub(crate) fn stop(&self) {
        self.stopping.set(true);

        if let Some(pid) = self.pid.take() {
            let _ = rustix::process::kill_process(pid, Signal::TERM);
        }

        let _ = std::fs::remove_file(&self.socket);
    }

    /// Runs the view.
    ///
    /// Always on the main thread, which matters: the signal a child is sent
    /// when its parent dies is sent when the thread that started it ends, not
    /// the process.
    fn spawn(self: &Rc<Self>) {
        let mut command = Command::new(&self.program);
        command
            .arg("--path")
            .arg(&self.shell)
            .env(SOCKET_VARIABLE, &self.socket);

        // The view goes when the daemon does, however the daemon goes: a
        // switcher with nothing behind it would sit there forever.
        //
        // SAFETY: the closure runs between fork and exec, and does nothing but
        // make one system call.
        unsafe {
            command.pre_exec(|| {
                rustix::process::set_parent_process_death_signal(Some(Signal::TERM))
                    .map_err(io::Error::from)
            });
        }

        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                eprintln!("jaunt: failed to start {}: {error}", self.program.display());
                self.again();
                return;
            }
        };

        #[allow(clippy::cast_possible_wrap)]
        let raw = child.id() as i32;
        self.pid.set(Pid::from_raw(raw));
        self.started.set(Instant::now());

        let quickshell = Rc::clone(self);
        glib::child_watch_add_local(glib::Pid(raw), move |_, status| {
            quickshell.exited(ExitStatus::from_raw(status));
        });
    }

    fn exited(self: &Rc<Self>, status: ExitStatus) {
        self.pid.set(None);

        if self.stopping.get() {
            return;
        }

        if self.started.get().elapsed() >= STEADY {
            self.failures.set(0);
        }

        eprintln!("jaunt: the switcher's Quickshell stopped ({status}), so it's starting again");
        self.again();
    }

    /// Starts the view again after a while, longer each time it has failed
    /// in a row.
    fn again(self: &Rc<Self>) {
        let failures = self.failures.get();
        self.failures.set(failures + 1);

        let delay = Duration::from_secs(1 << failures.min(5)).min(PATIENCE);
        let quickshell = Rc::clone(self);

        glib::timeout_add_local_once(delay, move || {
            if !quickshell.stopping.get() {
                quickshell.spawn();
            }
        });
    }
}

/// Passes each message on to whichever view connected last, dropping any that
/// arrive while none is.
fn write(lines: &mpsc::Receiver<Outgoing>) {
    let mut stream: Option<UnixStream> = None;

    for outgoing in lines {
        match outgoing {
            Outgoing::Connected(connected) => stream = Some(connected),
            Outgoing::Line(line) => {
                let Some(open) = &mut stream else { continue };

                if open.write_all(line.as_bytes()).is_err() {
                    stream = None;
                }
            }
        }
    }
}

/// Where the shell runs from: the directory named by `JAUNT_SHELL`, or the
/// built-in one, written out fresh.
fn shell() -> Result<PathBuf> {
    if let Some(directory) = std::env::var_os(SHELL_OVERRIDE) {
        return Ok(PathBuf::from(directory));
    }

    let directory = ipc::runtime_path("-shell");

    // Fresh each time, so that nothing from another version of Jaunt lingers
    // in it for Quickshell to pick up.
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;

    for (name, text) in SHELL {
        let path = directory.join(name);
        std::fs::write(&path, text)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }

    Ok(directory)
}

/// Where a program is on `PATH`, if it is.
fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;

    std::env::split_paths(&path)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}
