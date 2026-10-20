//! The switcher on screen, as the daemon describes it to the Quickshell
//! process that draws it.
//!
//! The daemon says what the switcher shows and never waits to hear that it
//! has: both of the switcher's timing rules belong to the controller, and the
//! view is only ever told about a switch once there is something to show.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::process::Command;
use std::rc::Rc;

use serde::Serialize;

use super::apps::Apps;
use crate::compositor::integrations::hyprland;
use super::quickshell::Quickshell;
use crate::compositor::Window;
use crate::config::{self, Config};
use crate::switcher::{Row, Session};
use crate::theme::Palette;

/// The shape a tile assumes a window has when the compositor won't say how big
/// it is: the shape a landscape window usually has.
const RATIO: f32 = 1.6;

/// What is shown for an application the icon theme has nothing for, where
/// something has to appear or the animation says nothing at all.
const FALLBACK_ICON: &str = "application-x-executable";

/// An application that is configured but has no windows open. It still gets a
/// place on screen, so its key is somewhere to be seen rather than only in the
/// configuration file.
pub(crate) struct Absent {
    /// What the `[names]` table would call it, keyed the way groups are.
    pub(crate) app_id: String,
    /// What to call it when the table doesn't.
    pub(crate) app: String,
    pub(crate) trigger: String,
}

/// One thing the view is told.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum Message {
    Config(Settings),
    /// Everything a switch shows, sent when it is first filled and each time
    /// it moves to another application.
    Session(Scene),
    /// The highlight moved within the application being switched to.
    Select {
        id: String,
        subject: String,
    },
    Show,
    Hide,
    /// An application was started: show its icon for a moment.
    Starting {
        icons: Vec<String>,
    },
}

/// The parts of the configuration the view needs.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Settings {
    width: Length,
    max_height: Length,
    preview_height: u32,
    previews: bool,
    icons: bool,
    background_opacity: f32,
    foreground_opacity: f32,
    /// The theme's colours, which the view works the rest out from.
    palette: Palette,
    cancel_key: String,
    /// The key that closes the highlighted window, when there is one.
    close_key: Option<String>,
    /// What the view dispatches to put the switcher away itself, when a
    /// click misses the panel.
    cancel_dispatch: String,
    /// The font GTK applications use, so that the switcher reads like them
    /// rather than like whatever Qt falls back to.
    font: Option<String>,
    /// And their monospaced one, which keys are written in so that they are
    /// all one width.
    mono: Option<String>,
}

/// A length, which the view resolves against the screen it is on.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
enum Length {
    Pixels(i32),
    Portion(f32),
}

impl From<config::Size> for Length {
    fn from(size: config::Size) -> Self {
        match size {
            config::Size::Pixels(pixels) => Self::Pixels(pixels),
            config::Size::Portion(portion) => Self::Portion(portion),
        }
    }
}

/// A switch, as it is shown.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Scene {
    /// The application being switched to, as it is named on screen.
    name: String,
    /// The window a release of Super would land on.
    subject: String,
    /// The row of the application being switched to, by its `app_id`.
    target: String,
    /// The window a release of Super would land on, by its id.
    selected: String,
    /// The key the switch is on, which walks the windows of the application
    /// being switched to.
    cycle_key: Option<String>,
    /// The applications with windows open, in the order of their keys.
    rows: Vec<AppRow>,
    /// The applications with nothing open, in the order of their keys.
    absent: Vec<Chip>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppRow {
    app_id: String,
    name: String,
    /// The names its icon might go by, best guess first.
    icons: Vec<String>,
    key: Option<String>,
    windows: Vec<Tile>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Tile {
    id: String,
    title: String,
    /// How wide the window is against its height, which is the shape its
    /// thumbnail and its marker both take.
    aspect: f32,
}

/// An application with nothing open: its key and its name.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Chip {
    name: String,
    icons: Vec<String>,
    key: String,
}

/// The switcher on screen, or the daemon's side of it.
pub(crate) struct View {
    /// The process drawing it. `None` when Quickshell isn't installed, which
    /// leaves the keys switching without anything on screen.
    shell: Option<Rc<Quickshell>>,
    apps: RefCell<Apps>,
    settings: RefCell<Settings>,
    fonts: Fonts,
    /// The switch the view is showing, or about to, for a view that connects
    /// partway through one.
    scene: RefCell<Option<Scene>>,
    shown: Cell<bool>,
}

impl View {
    /// Starts the view, and hands back the connections it makes: each of them
    /// wants telling what is going on, through [`View::connected`].
    pub(crate) fn start(config: &Config) -> (Self, Option<async_channel::Receiver<()>>) {
        let apps = Apps::new(config.names.clone());
        let fonts = Fonts::read();

        let (shell, connections) = match Quickshell::start() {
            Ok((shell, connections)) => (Some(shell), Some(connections)),
            Err(error) => {
                eprintln!("jaunt: the switcher can't be shown: {error:#}");
                (None, None)
            }
        };

        let view = Self {
            shell,
            apps: RefCell::new(apps),
            settings: RefCell::new(settings(config, &fonts)),
            fonts,
            scene: RefCell::new(None),
            shown: Cell::new(false),
        };

        (view, connections)
    }

    /// Catches a view that just connected up with everything it has missed.
    pub(crate) fn connected(&self) {
        self.send(&Message::Config(self.settings.borrow().clone()));

        if let Some(scene) = self.scene.borrow().clone() {
            self.send(&Message::Session(scene));
        }

        if self.shown.get() {
            self.send(&Message::Show);
        }
    }

    /// Takes on a configuration that changed while the daemon was running.
    pub(crate) fn reconfigure(&self, config: &Config) {
        self.apps.borrow_mut().rename(config.names.clone());

        let settings = settings(config, &self.fonts);
        self.send(&Message::Config(settings.clone()));
        self.settings.replace(settings);
    }

    /// Lays out every open window, each application's key beside its name,
    /// with the switch's own application named at the top.
    pub(crate) fn fill(
        &self,
        session: &Session,
        triggers: &HashMap<String, String>,
        absent: &[Absent],
    ) {
        let scene = scene(&self.apps.borrow(), session, triggers, absent);

        self.send(&Message::Session(scene.clone()));
        self.scene.replace(Some(scene));
    }

    /// Marks the window that a release of Super would focus.
    pub(crate) fn highlight(&self, session: &Session) {
        let window = session.selected_window();
        let id = window.id.clone();
        let subject = title(window).to_owned();

        if let Some(scene) = self.scene.borrow_mut().as_mut() {
            scene.selected.clone_from(&id);
            scene.subject.clone_from(&subject);
        }

        self.send(&Message::Select { id, subject });
    }

    pub(crate) fn show(&self) {
        self.shown.set(true);
        self.send(&Message::Show);
    }

    pub(crate) fn hide(&self) {
        self.shown.set(false);
        self.scene.replace(None);
        self.send(&Message::Hide);
    }

    /// Says that an application has been started, by showing its icon for
    /// about as long as it takes to notice.
    pub(crate) fn starting(&self, app_id: &str, cmd: &str) {
        let mut icons = self.apps.borrow().icons(app_id, Some(cmd));
        icons.push(FALLBACK_ICON.to_owned());

        self.send(&Message::Starting { icons });
    }

    /// Takes the view down, for a daemon that is stopping.
    pub(crate) fn stop(&self) {
        if let Some(shell) = &self.shell {
            shell.stop();
        }
    }

    fn send(&self, message: &Message) {
        let Some(shell) = &self.shell else {
            return;
        };

        match serde_json::to_string(message) {
            Ok(line) => shell.send(line),
            Err(error) => eprintln!("jaunt: failed to describe the switcher: {error}"),
        }
    }
}

fn settings(config: &Config, fonts: &Fonts) -> Settings {
    Settings {
        width: config.switcher.width.into(),
        max_height: config.switcher.max_height.into(),
        preview_height: config.previews.height,
        previews: config.previews.enabled,
        icons: config.switcher.icons,
        background_opacity: config.switcher.background_opacity.get(),
        foreground_opacity: config.switcher.foreground_opacity.get(),
        palette: config.palette.clone(),
        cancel_key: keycap_name(&config.keys.cancel),
        close_key: config.keys.close.as_ref().map(keycap_name),
        cancel_dispatch: hyprland::cancel_dispatch(),
        font: fonts.text.clone(),
        mono: fonts.mono.clone(),
    }
}

/// Everything a switch shows.
fn scene(
    apps: &Apps,
    session: &Session,
    triggers: &HashMap<String, String>,
    absent: &[Absent],
) -> Scene {
    let mut rows: Vec<AppRow> = Vec::new();

    for row in session.rows() {
        match row {
            Row::Group { app_id, name } => rows.push(AppRow {
                app_id: app_id.to_owned(),
                name: apps.label(app_id, None, name),
                // The icon is still looked up by `app_id`: it is what the
                // desktop entry is named after, not what the window calls
                // itself.
                icons: apps.icons(app_id, None),
                key: triggers.get(app_id).cloned(),
                windows: Vec::new(),
            }),
            Row::Window { window, .. } => {
                if let Some(row) = rows.last_mut() {
                    row.windows.push(Tile {
                        id: window.id.clone(),
                        title: title(window).to_owned(),
                        aspect: aspect(window.size),
                    });
                }
            }
        }
    }

    rows.sort_by_key(|row| place(row.key.as_deref()));

    let mut absent: Vec<Chip> = absent
        .iter()
        .map(|application| Chip {
            name: apps.label(
                &application.app_id,
                Some(&application.app),
                &application.app,
            ),
            icons: apps.icons(&application.app_id, Some(&application.app)),
            key: application.trigger.clone(),
        })
        .collect();
    absent.sort_by_key(|chip| chip.key.to_lowercase());

    let selected = session.selected_window();

    Scene {
        name: apps.label(session.group(), None, session.label()),
        subject: title(selected).to_owned(),
        target: session.group().to_owned(),
        selected: selected.id.clone(),
        cycle_key: triggers.get(session.group()).cloned(),
        rows,
        absent,
    }
}

/// The open applications by group, in the order the switcher shows them.
pub(crate) fn order(session: &Session, triggers: &HashMap<String, String>) -> Vec<String> {
    let mut groups: Vec<&str> = session
        .rows()
        .filter_map(|row| match row {
            Row::Group { app_id, .. } => Some(app_id),
            Row::Window { .. } => None,
        })
        .collect();
    groups.sort_by_key(|group| place(triggers.get(*group).map(String::as_str)));

    groups.into_iter().map(str::to_owned).collect()
}

/// Where an application with the key `key` goes among the others: in the
/// order of the keys that reach them, which is the order they are learned in.
/// One with no key of its own can only be reached by walking, so it goes last.
fn place(key: Option<&str>) -> (bool, String) {
    (key.is_none(), key.unwrap_or_default().to_lowercase())
}

/// What to call a window: its title, or its application for a window without
/// one, which is better than an empty tile.
fn title(window: &Window) -> &str {
    if window.title.is_empty() {
        &window.app_id
    } else {
        &window.title
    }
}

/// How wide a window is against its height. A window the compositor won't
/// measure falls back to the shape a landscape window usually has.
fn aspect(size: Option<(u32, u32)>) -> f32 {
    match size {
        #[allow(clippy::cast_precision_loss)]
        Some((width, height)) if height > 0 => width as f32 / height as f32,
        _ => RATIO,
    }
}

/// A key as it reads on a keycap rather than in a configuration file.
fn keycap_name(key: &config::Key) -> String {
    key.to_string().replace("Escape", "Esc")
}

/// The fonts GTK applications are set in, read once: asking costs a process
/// each time.
struct Fonts {
    text: Option<String>,
    mono: Option<String>,
}

impl Fonts {
    fn read() -> Self {
        Self {
            text: gtk_font("font-name"),
            mono: gtk_font("monospace-font-name"),
        }
    }
}

/// The family of one of the fonts GTK applications are set in, if there is a
/// setting to say so.
fn gtk_font(setting: &str) -> Option<String> {
    let output = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", setting])
        .output()
        .ok()?;

    family(&String::from_utf8_lossy(&output.stdout))
}

/// The family out of a font description like `'Noto Sans,  10'`.
fn family(description: &str) -> Option<String> {
    let description = description.trim().trim_matches('\'');
    let family = match description.rsplit_once(' ') {
        Some((family, size)) if size.parse::<f32>().is_ok() => family,
        _ => description,
    };
    let family = family.trim().trim_end_matches(',').trim();

    (!family.is_empty()).then(|| family.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::switcher::{Direction, group_windows};

    fn window(id: &str, app_id: &str, title: &str, size: Option<(u32, u32)>) -> Window {
        Window {
            id: id.to_owned(),
            app_id: app_id.to_owned(),
            title: title.to_owned(),
            size,
            initial_title: String::new(),
        }
    }

    fn session() -> Session {
        let windows = vec![
            window("0x1", "com.mitchellh.ghostty", "jaunt", Some((1600, 1000))),
            window("0x2", "brave-browser", "Hyprland Wiki", Some((1000, 1000))),
            window("0x3", "com.mitchellh.ghostty", "", None),
            window("0x4", "zen", "Notes", None),
        ];

        Session::new(
            group_windows(windows),
            "com.mitchellh.ghostty",
            None,
            "ghostty",
            Direction::Forward,
        )
    }

    fn triggers() -> HashMap<String, String> {
        [("com.mitchellh.ghostty", "T"), ("brave-browser", "W")]
            .into_iter()
            .map(|(app_id, key)| (app_id.to_owned(), key.to_owned()))
            .collect()
    }

    fn apps() -> Apps {
        Apps::new(
            [("brave-browser".to_owned(), "Brave".to_owned())]
                .into_iter()
                .collect(),
        )
    }

    #[test]
    fn rows_follow_their_keys_and_keyless_ones_go_last() {
        let scene = scene(&apps(), &session(), &triggers(), &[]);
        let order: Vec<&str> = scene.rows.iter().map(|row| row.app_id.as_str()).collect();

        assert_eq!(order, ["com.mitchellh.ghostty", "brave-browser", "zen"]);
        assert_eq!(scene.rows[1].name, "Brave");
        assert_eq!(scene.rows[2].key, None);
    }

    #[test]
    fn the_scene_names_the_switch_and_where_it_would_land() {
        let scene = scene(&apps(), &session(), &triggers(), &[]);

        assert_eq!(scene.target, "com.mitchellh.ghostty");
        assert_eq!(scene.selected, "0x1");
        assert_eq!(scene.subject, "jaunt");
        assert_eq!(scene.cycle_key.as_deref(), Some("T"));
    }

    #[test]
    fn a_window_without_a_title_or_a_size_still_has_both() {
        let scene = scene(&apps(), &session(), &triggers(), &[]);
        let ghostty = &scene.rows[0].windows;

        assert!((ghostty[0].aspect - 1.6).abs() < f32::EPSILON);
        assert_eq!(ghostty[1].title, "com.mitchellh.ghostty");
        assert!((ghostty[1].aspect - RATIO).abs() < f32::EPSILON);
        assert!((scene.rows[1].windows[0].aspect - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_order_is_the_order_the_rows_are_shown_in() {
        assert_eq!(
            order(&session(), &triggers()),
            ["com.mitchellh.ghostty", "brave-browser", "zen"]
        );
    }

    #[test]
    fn absent_applications_follow_their_keys_too() {
        let absent = [
            Absent {
                app_id: "spotify".to_owned(),
                app: "spotify".to_owned(),
                trigger: "S".to_owned(),
            },
            Absent {
                app_id: "nautilus".to_owned(),
                app: "nautilus".to_owned(),
                trigger: "E".to_owned(),
            },
        ];
        let scene = scene(&apps(), &session(), &triggers(), &absent);
        let keys: Vec<&str> = scene.absent.iter().map(|chip| chip.key.as_str()).collect();

        assert_eq!(keys, ["E", "S"]);
    }

    #[test]
    fn messages_are_tagged_by_type() {
        let select = Message::Select {
            id: "0x1".to_owned(),
            subject: "jaunt".to_owned(),
        };

        assert_eq!(
            serde_json::to_string(&select).unwrap(),
            r#"{"type":"select","id":"0x1","subject":"jaunt"}"#
        );
        assert_eq!(
            serde_json::to_string(&Message::Show).unwrap(),
            r#"{"type":"show"}"#
        );
        assert_eq!(
            serde_json::to_value(Length::Portion(0.5)).unwrap(),
            serde_json::json!({ "portion": 0.5 })
        );
    }

    #[test]
    fn the_configuration_carries_the_themes_colours() {
        let fonts = Fonts {
            text: None,
            mono: None,
        };
        let message = Message::Config(settings(&Config::default(), &fonts));
        let message = serde_json::to_value(message).unwrap();

        assert_eq!(message["type"], "config");
        assert_eq!(message["palette"]["background"], "#15171c");
        assert_eq!(message["palette"]["accent"], "#6b8cff");
    }

    #[test]
    fn a_font_description_comes_apart_into_its_family() {
        assert_eq!(family("'Noto Sans,  10'\n").as_deref(), Some("Noto Sans"));
        assert_eq!(family("'Cantarell 11'").as_deref(), Some("Cantarell"));
        assert_eq!(family("'Adwaita Mono 11'").as_deref(), Some("Adwaita Mono"));
        assert_eq!(family("''").as_deref(), None);
    }
}
