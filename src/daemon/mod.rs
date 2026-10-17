//! The long-running half of jaunt: it listens for the keys it asked Hyprland
//! to bind, decides what each one means, and tells the switcher what to show.

mod apps;
mod controller;
mod ipc;
mod quickshell;
mod view;

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::compositor::Compositor as _;
use crate::compositor::Window;
use crate::compositor::integrations::hyprland::{
    self, BACK_EVENT, Binds, CANCEL_EVENT, CLOSE_EVENT, CONFIRM_EVENT, LAUNCH_EVENT, NEXT_EVENT,
    PREVIOUS_EVENT, SELECT_EVENT, SWITCH_EVENT,
};
use crate::config::{Config, Numbers, Target};
use crate::switcher::{Direction, Session};
use controller::{Controller, Effect, Event};
use view::{Absent, View};

pub(crate) use ipc::switch;

/// How long to let a configuration file settle before reading it. Editors
/// write, rename and truncate in quick succession, and Jaunt would rather
/// read the result once than read half of it three times.
const SETTLE: Duration = Duration::from_millis(60);

/// Runs the switcher until it's asked to stop.
///
/// # Errors
///
/// Returns an error if Hyprland isn't running, if another daemon already is,
/// or if the keybinds couldn't be set up.
pub(crate) fn run(path: Option<&Path>) -> Result<()> {
    let config_path = Config::path(path);
    let config = Rc::new(Config::load(path)?);
    let compositor = hyprland::Compositor;

    // Resolved here so that a stale $HYPRLAND_INSTANCE_SIGNATURE, or more than
    // one Hyprland running, is reported as itself rather than as a failure to
    // install keybinds further down.
    hyprland::instance_dir().context("jaunt's switcher only supports Hyprland")?;

    // Claimed before anything else, so a second daemon fails fast instead of
    // fighting over the keybinds.
    let instance = ipc::Instance::acquire()?;

    warn_about_missing(&config);

    let binds =
        Rc::new(Binds::install(&config).context("failed to install Jaunt's Hyprland keybinds")?);

    // Started before the first switch can arrive, so that the view is up and
    // warm by the time one is worth showing.
    let (view, connections) = View::start(&config);

    let daemon = Rc::new(Daemon {
        controller: RefCell::new(Controller::default()),
        view,
        compositor,
        binds: RefCell::new(binds),
        config: RefCell::new(config),
        config_path: config_path.clone(),
    });

    let main_loop = glib::MainLoop::new(None, false);

    watch_hyprland(&daemon, &main_loop)?;
    watch_clients(&daemon, instance.listener())?;

    if let Some(connections) = connections {
        watch_view(&daemon, connections);
    }

    if let Some(path) = &config_path {
        watch_config(&daemon, path);
    }

    quit_on_signal(&main_loop)?;

    main_loop.run();

    // Leave Hyprland the way it was found.
    daemon.binds().remove();
    daemon.view.stop();
    drop(instance);

    Ok(())
}

struct Daemon {
    controller: RefCell<Controller>,
    view: View,
    compositor: hyprland::Compositor,
    binds: RefCell<Rc<Binds>>,
    config: RefCell<Rc<Config>>,
    config_path: Option<PathBuf>,
}

impl Daemon {
    fn config(&self) -> Rc<Config> {
        Rc::clone(&self.config.borrow())
    }

    fn binds(&self) -> Rc<Binds> {
        Rc::clone(&self.binds.borrow())
    }

    /// Reads the configuration file again and takes on what changed.
    ///
    /// A file that doesn't parse leaves the daemon exactly as it was, since a
    /// half-saved file is a normal thing for an editor to leave behind for a
    /// moment.
    fn reload(self: &Rc<Self>) {
        let config = match Config::load(self.config_path.as_deref()) {
            Ok(config) => Rc::new(config),
            Err(error) => {
                eprintln!("jaunt: keeping the configuration it had: {error:#}");
                return;
            }
        };

        // A switch in progress was started under the old keys, so it ends here
        // rather than half under each.
        self.handle(Event::Cancel);

        let previous = self.binds();
        previous.remove();

        warn_about_missing(&config);

        match Binds::install(&config) {
            Ok(binds) => {
                self.view.reconfigure(&config);
                self.binds.replace(Rc::new(binds));
                self.config.replace(config);
            }
            Err(error) => {
                eprintln!("jaunt: the new configuration's keybinds were refused: {error:#}");

                // Put back the ones that were working.
                match Binds::install(&self.config()) {
                    Ok(binds) => {
                        self.binds.replace(Rc::new(binds));
                    }
                    Err(error) => eprintln!("jaunt: and its own keybinds are gone too: {error:#}"),
                }
            }
        }
    }
    fn handle(self: &Rc<Self>, event: Event) {
        let effects = self.controller.borrow_mut().handle(event);

        for effect in effects {
            self.apply(effect);
        }
    }

    fn apply(self: &Rc<Self>, effect: Effect) {
        match effect {
            Effect::ScheduleReveal { session } => {
                let daemon = Rc::clone(self);

                glib::timeout_add_local_once(self.config().switcher.delay(), move || {
                    daemon.handle(Event::Reveal { session });
                });
            }
            // The view hears about a switch here and no sooner: Fill only
            // happens once the switcher is to be on screen, so a tap quick
            // enough to skip it shows nothing and captures nothing at all.
            Effect::Fill => self.fill(),
            Effect::Highlight => {
                if let Some(session) = self.controller.borrow().session() {
                    self.view.highlight(session);
                }
            }
            Effect::Show => {
                self.view.show();
                self.binds().capture_session_keys();
            }
            Effect::Hide => {
                self.view.hide();
                self.binds().release_session_keys();
            }
            Effect::Focus(window) => {
                if let Err(error) = self.compositor.focus_window(&window) {
                    eprintln!("jaunt: {error:#}");
                }
            }
            Effect::Close(window) => {
                if let Err(error) = self.compositor.close_window(&window) {
                    eprintln!("jaunt: {error:#}");
                }
            }
            Effect::Launch(target) => self.launch(&target),
        }
    }

    /// Starts an application, and says so.
    ///
    /// Both ways of asking for one come through here: the key for something
    /// with no windows, and Ctrl with the key for another copy of something
    /// that has. Neither shows the switcher, so without this nothing would
    /// happen on screen until the application itself got round to appearing.
    fn launch(&self, target: &Target) {
        if let Err(error) = self.compositor.launch_application(&target.app) {
            eprintln!("jaunt: {error:#}");
            return;
        }

        self.view.starting(target.search(), &target.app);
    }

    /// Closes the highlighted window, telling the controller where the
    /// switch would go if that leaves its application with none.
    fn close(self: &Rc<Self>) {
        let order = {
            let controller = self.controller.borrow();
            let Some(session) = controller.session() else {
                return;
            };

            view::order(session, &triggers(&self.config(), session))
        };

        self.handle(Event::Close { order });
    }

    /// Fills the switcher from the switch in progress.
    fn fill(&self) {
        let config = self.config();
        let controller = self.controller.borrow();
        let Some(session) = controller.session() else {
            return;
        };

        self.view.fill(
            session,
            &triggers(&config, session),
            &absent(&config, session),
        );
    }

    /// A mapped key was pressed: take a snapshot of the open windows and let
    /// the controller decide what it means.
    fn trigger(self: &Rc<Self>, target: Target, direction: Direction) {
        let windows = match self.compositor.get_windows() {
            Ok(windows) => mapped(&self.config(), windows),
            Err(error) => {
                eprintln!("jaunt: {error:#}");
                return;
            }
        };

        let focused = match self.compositor.get_focused_window() {
            Ok(focused) => focused.map(Box::new),
            Err(error) => {
                eprintln!("jaunt: {error:#}");
                None
            }
        };

        self.handle(Event::Trigger {
            target,
            direction,
            windows,
            focused,
        });
    }
}

/// Follows Hyprland's event stream: the keys Jaunt asked it to bind arrive
/// here, in the order they were pressed, whatever the window happens to be
/// doing. That ordering is what makes releasing Super reliable — the release
/// can't overtake the key press that started the switch, and neither waits
/// for anything to be drawn.
fn watch_hyprland(daemon: &Rc<Daemon>, main_loop: &glib::MainLoop) -> Result<()> {
    let events = BufReader::new(hyprland::events()?);
    let (sender, receiver) = async_channel::unbounded();

    thread::spawn(move || {
        for line in events.lines() {
            let Ok(line) = line else { break };

            if sender.send_blocking(line).is_err() {
                break;
            }
        }
    });

    let daemon = Rc::clone(daemon);
    let main_loop = main_loop.clone();

    glib::MainContext::default().spawn_local(async move {
        while let Ok(line) = receiver.recv().await {
            on_hyprland_event(&daemon, &line);
        }

        // Hyprland is gone, and so is the point of running.
        main_loop.quit();
    });

    Ok(())
}

fn on_hyprland_event(daemon: &Rc<Daemon>, line: &str) {
    let Some((event, data)) = line.split_once(">>") else {
        return;
    };

    match event {
        "custom" if data == CONFIRM_EVENT => daemon.handle(Event::Confirm),
        "custom" if data == CANCEL_EVENT => daemon.handle(Event::Cancel),
        "custom" if data == CLOSE_EVENT => daemon.close(),
        "custom" if data.starts_with(SELECT_EVENT) => {
            // The windows are numbered from one, as they are on screen.
            let picked = data
                .strip_prefix(SELECT_EVENT)
                .and_then(|number| number.parse::<usize>().ok())
                .and_then(|number| number.checked_sub(1));

            let Some(index) = picked else {
                return;
            };

            daemon.handle(Event::Pick {
                index,
                confirm: daemon.config().switcher.numbers == Numbers::Switch,
            });
        }
        "custom" if data == NEXT_EVENT => daemon.handle(Event::Cycle {
            direction: Direction::Forward,
        }),
        "custom" if data == PREVIOUS_EVENT => daemon.handle(Event::Cycle {
            direction: Direction::Backward,
        }),
        "custom" if data.starts_with(LAUNCH_EVENT) => {
            let Some(key) = data.strip_prefix(LAUNCH_EVENT) else {
                return;
            };
            let config = daemon.config();
            let Some(target) = config.target(key) else {
                return;
            };

            // Asking for a new window settles what the switch was for, so the
            // switcher goes without focusing anything. Harmless when it isn't
            // open: there is then nothing to end.
            daemon.handle(Event::Cancel);

            daemon.launch(target);
        }
        "custom" => {
            let pressed = [
                (SWITCH_EVENT, Direction::Forward),
                (BACK_EVENT, Direction::Backward),
            ]
            .into_iter()
            .find_map(|(event, direction)| Some((data.strip_prefix(event)?, direction)));

            let Some((key, direction)) = pressed else {
                return;
            };
            let config = daemon.config();
            let Some(target) = config.target(key).cloned() else {
                return;
            };

            daemon.trigger(target, direction);
        }
        // A reload wipes keybinds that were added over IPC.
        "configreloaded" => daemon.binds().reinstall(),
        _ => {}
    }
}

/// Follows the daemon's own socket, where `jaunt switch` sends its requests.
fn watch_clients(daemon: &Rc<Daemon>, listener: &UnixListener) -> Result<()> {
    let listener = listener
        .try_clone()
        .context("failed to watch the daemon's socket")?;
    let (sender, receiver) = async_channel::unbounded();

    thread::spawn(move || {
        for client in listener.incoming() {
            let Ok(client) = client else { continue };
            let Some(message) = ipc::receive(client) else {
                continue;
            };

            if sender.send_blocking(message).is_err() {
                break;
            }
        }
    });

    let daemon = Rc::clone(daemon);

    glib::MainContext::default().spawn_local(async move {
        while let Ok(message) = receiver.recv().await {
            match message {
                ipc::Message::Switch { app, app_id } => {
                    daemon.trigger(Target::new(&app, app_id.as_deref()), Direction::Forward);
                }
            }
        }
    });

    Ok(())
}

/// Watches the configuration file, and the user's themes, so that saving
/// either is all it takes for the change to be in effect.
fn watch_config(daemon: &Rc<Daemon>, path: &Path) {
    let (Some(directory), Some(name)) = (path.parent(), path.file_name().map(OsString::from))
    else {
        return;
    };

    let mut inotify = match inotify::Inotify::init() {
        Ok(inotify) => inotify,
        Err(error) => {
            eprintln!("jaunt: not watching {}: {error}", path.display());
            return;
        }
    };

    // The directory rather than the file: an editor saves by writing a new
    // file and renaming it over the old one, which leaves a watch on the file
    // itself pointing at something nobody will write to again.
    let saved =
        inotify::WatchMask::CLOSE_WRITE | inotify::WatchMask::MOVED_TO | inotify::WatchMask::CREATE;

    let config = match inotify.watches().add(directory, saved) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("jaunt: not watching {}: {error}", directory.display());
            return;
        }
    };

    // Any theme, rather than the one in use: which one that is can change
    // with the configuration, and saving one that isn't costs a reload.
    // Watched only when there is a directory of them to watch.
    let themes = crate::config::themes_directory()
        .filter(|themes| themes.is_dir())
        .and_then(|themes| match inotify.watches().add(&themes, saved) {
            Ok(watch) => Some(watch),
            Err(error) => {
                eprintln!("jaunt: not watching {}: {error}", themes.display());
                None
            }
        });

    let (sender, receiver) = async_channel::unbounded();

    thread::spawn(move || {
        let mut buffer = [0; 4096];

        loop {
            let Ok(mut events) = inotify.read_events_blocking(&mut buffer) else {
                break;
            };

            let changed = events.any(|event| {
                if event.wd == config {
                    event.name == Some(&name)
                } else {
                    Some(&event.wd) == themes.as_ref()
                        && event.name.is_some_and(|name| {
                            Path::new(name).extension() == Some("toml".as_ref())
                        })
                }
            });

            if !changed {
                continue;
            }

            // Let the rest of the editor's writing land, and read it once.
            thread::sleep(SETTLE);
            let mut settled = [0; 4096];
            let _ = inotify.read_events(&mut settled);

            if sender.send_blocking(()).is_err() {
                break;
            }
        }
    });

    let daemon = Rc::clone(daemon);

    glib::MainContext::default().spawn_local(async move {
        while receiver.recv().await.is_ok() {
            daemon.reload();
        }
    });
}

/// Which key reaches each application on screen, so that the switcher can
/// show it beside the application's name.
fn triggers(config: &Config, session: &Session) -> HashMap<String, String> {
    config
        .keys
        .apps
        .iter()
        .filter_map(|(key, target)| {
            let group = session.find_group(target.search())?;

            Some((group.to_owned(), key.to_string()))
        })
        .collect()
}

/// Whether the command an application is started with is somewhere on `PATH`.
fn installed(cmd: &str) -> bool {
    // A command given as a path is its own answer.
    if cmd.contains('/') {
        return Path::new(cmd).is_file();
    }

    let Some(path) = std::env::var_os("PATH") else {
        return true;
    };

    std::env::split_paths(&path).any(|directory| directory.join(cmd).is_file())
}

/// Says which configured applications aren't installed, once, rather than
/// leaving their keys to fail quietly.
fn warn_about_missing(config: &Config) {
    for (key, target) in &config.keys.apps {
        if !installed(&target.app) {
            eprintln!(
                "jaunt: {} isn't installed, so {key} has nothing to switch to",
                target.app
            );
        }
    }
}

/// The windows of applications the user has a key for.
///
/// Everything else is left out: the switcher is a way of reaching the
/// applications that were given keys, and a row nothing reaches is noise.
/// Matched the way [`switcher::find_group`] matches, so a window is kept
/// exactly when some key would find it.
fn mapped(config: &Config, windows: Vec<Window>) -> Vec<Window> {
    windows
        .into_iter()
        .filter(|window| {
            let app_id = window.app_id.to_lowercase();

            config.keys.apps.values().any(|target| {
                let search = target.search().to_lowercase();

                app_id == search || app_id.contains(&search)
            })
        })
        .collect()
}

/// The applications that are configured but have nothing open, so the
/// switcher can show their keys too.
fn absent(config: &Config, session: &Session) -> Vec<Absent> {
    config
        .keys
        .apps
        .iter()
        .filter(|(_, target)| session.find_group(target.search()).is_none())
        // A row for something that can't start is worse than no row: its key
        // does nothing, and it takes space from the applications that work.
        .filter(|(_, target)| installed(&target.app))
        .map(|(key, target)| Absent {
            app_id: target.search().to_lowercase(),
            app: target.app.clone(),
            trigger: key.to_string(),
        })
        .collect()
}

/// Tells the view what is going on each time it connects: when it first
/// starts, and again whenever it has had to be started over.
fn watch_view(daemon: &Rc<Daemon>, connections: async_channel::Receiver<()>) {
    let daemon = Rc::clone(daemon);

    glib::MainContext::default().spawn_local(async move {
        while connections.recv().await.is_ok() {
            daemon.view.connected();
        }
    });
}

/// Stops on the signals a service manager stops things with, so the keybinds
/// are taken away rather than left behind.
fn quit_on_signal(main_loop: &glib::MainLoop) -> Result<()> {
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
    ])
    .context("failed to listen for termination signals")?;
    let (sender, receiver) = async_channel::bounded(1);

    thread::spawn(move || {
        if signals.forever().next().is_some() {
            let _ = sender.send_blocking(());
        }
    });

    let main_loop = main_loop.clone();

    glib::MainContext::default().spawn_local(async move {
        let _ = receiver.recv().await;
        main_loop.quit();
    });

    Ok(())
}
