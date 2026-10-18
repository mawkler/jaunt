//! The configuration file: which key targets which application, which keys
//! steer the switcher once it's up, and the few knobs it exposes.
//!
//! Everything has a default, so Jaunt runs without a configuration file at
//! all. Writing one replaces the defaults it mentions and leaves the rest.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::theme::{self, Palette};

/// Jaunt's own corner of the user's configuration.
fn directory() -> Option<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;

    Some(config_home.join("jaunt"))
}

/// The configuration Jaunt ships: every setting, written out at its default,
/// with what each one does beside it.
const EXAMPLE: &str = include_str!("../config.example.toml");

/// Hands the user that file, to print or to start their own from.
///
/// # Errors
///
/// Returns an error if there's nowhere to write one, if one is already there,
/// or if writing fails.
pub(crate) fn scaffold(write: bool) -> Result<()> {
    if !write {
        print!("{EXAMPLE}");

        return Ok(());
    }

    let path = default_path().context(
        "there's nowhere to put a configuration: neither $XDG_CONFIG_HOME nor $HOME is set",
    )?;

    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("failed to create {}", directory.display()))?;
    }

    // Created rather than written, so that a configuration already there is
    // never overwritten by a race or by a second run.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path);

    match file {
        Ok(mut file) => {
            file.write_all(EXAMPLE.as_bytes())
                .with_context(|| format!("failed to write {}", path.display()))?;

            println!("wrote {}", path.display());

            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(anyhow::anyhow!(
            "{} is already there, and this would write over it",
            path.display()
        )),
        Err(error) => Err(error).with_context(|| format!("failed to create {}", path.display())),
    }
}

/// Where the configuration lives unless `--config` says otherwise.
pub(crate) fn default_path() -> Option<PathBuf> {
    Some(directory()?.join("config.toml"))
}

/// Where the user's own themes are, wherever the configuration is: a
/// configuration in the Nix store can still use them.
pub(crate) fn themes_directory() -> Option<PathBuf> {
    Some(directory()?.join("themes"))
}

#[derive(Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// Every key Jaunt binds.
    #[serde(default)]
    pub(crate) keys: Keys,
    /// How the switcher behaves and how big it is.
    #[serde(default)]
    pub(crate) switcher: Switcher,
    /// The thumbnails of the windows being switched between.
    #[serde(default)]
    pub(crate) previews: Previews,
    /// What to call an application on screen, by the `app_id` its windows
    /// carry. Without an entry a group is named after the title its windows
    /// opened under, which is usually the application's own name but not
    /// always the one worth showing: Brave opens windows called `New Tab -
    /// Brave`.
    #[serde(default, deserialize_with = "lowercased_keys")]
    pub(crate) names: BTreeMap<String, String>,
    /// The colours of the theme the switcher is drawn in, read along with
    /// the file that names it.
    #[serde(skip)]
    pub(crate) palette: Palette,
}

/// `app_id`s are compared in lowercase, the same way windows are grouped by
/// them, so a name written any other way still finds its application.
fn lowercased_keys<'de, D>(deserializer: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(BTreeMap::<String, String>::deserialize(deserializer)?
        .into_iter()
        .map(|(app_id, name)| (app_id.to_lowercase(), name))
        .collect())
}

impl Config {
    /// The file [`Self::load`] reads, which is also the file to watch for
    /// changes.
    pub(crate) fn path(explicit: Option<&Path>) -> Option<PathBuf> {
        explicit.map(Path::to_owned).or_else(default_path)
    }

    /// Reads the configuration, falling back to the defaults when there isn't
    /// one to read.
    ///
    /// # Errors
    ///
    /// Returns an error if `path` was given but can't be read, if the file
    /// doesn't parse, or if the theme it names can't be had.
    pub(crate) fn load(path: Option<&Path>) -> Result<Self> {
        let required = path.is_some();

        let Some(path) = Self::path(path) else {
            return Ok(Self::default());
        };

        let file = match std::fs::read_to_string(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        };

        let mut config: Self =
            toml::from_str(&file).with_context(|| format!("{} isn't valid", path.display()))?;
        config.palette = theme::load(&config.switcher.theme, themes_directory().as_deref())?;

        Ok(config)
    }

    /// The application the key Jaunt named `id` targets, if any.
    pub(crate) fn target(&self, id: &str) -> Option<&Target> {
        self.keys
            .apps
            .iter()
            .find(|(key, _)| key.id() == id)
            .map(|(_, target)| target)
    }
}

/// An application the switcher can target.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "Entry")]
pub(crate) struct Target {
    /// The command that launches it, e.g. `ghostty`.
    pub(crate) app: String,
    /// The window class its windows carry, when that isn't the command name.
    pub(crate) app_id: Option<String>,
}

impl Target {
    pub(crate) fn new(app: &str, app_id: Option<&str>) -> Self {
        Self {
            app: app.to_owned(),
            app_id: app_id.map(str::to_owned),
        }
    }

    /// What to match the open windows against.
    pub(crate) fn search(&self) -> &str {
        self.app_id.as_deref().unwrap_or(&self.app)
    }
}

/// An application as the configuration file spells it: either the bare command
/// to run, or a table when its windows carry a different class.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Entry {
    Command(String),
    Detailed {
        cmd: String,
        #[serde(default)]
        app_id: Option<String>,
    },
}

impl From<Entry> for Target {
    fn from(entry: Entry) -> Self {
        match entry {
            Entry::Command(app) => Self { app, app_id: None },
            Entry::Detailed { cmd, app_id } => Self { app: cmd, app_id },
        }
    }
}

/// Every key Jaunt binds: one per application, plus the ones that steer a
/// switch already in progress.
///
/// The steering keys are bound only while the switcher is on screen, so they
/// belong to the applications underneath the rest of the time.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Keys {
    /// Which key targets which application. `Super` and the key starts a
    /// switch; for a key with no modifiers of its own, holding `Shift` as well
    /// walks the group the other way.
    ///
    /// A key may carry further modifiers, spelled as `next` and `previous`
    /// are: `"SHIFT + a"`.
    #[serde(default)]
    pub(crate) apps: BTreeMap<Key, Target>,
    /// Moves the highlight to the next window.
    #[serde(default)]
    pub(crate) next: Option<Key>,
    /// Moves it back to the previous one.
    #[serde(default)]
    pub(crate) previous: Option<Key>,
    /// Closes the highlighted window, leaving the switcher open on the rest.
    #[serde(default)]
    pub(crate) close: Option<Key>,
    /// Closes the switcher without switching.
    #[serde(default = "default_cancel")]
    pub(crate) cancel: Key,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            apps: BTreeMap::new(),
            next: None,
            previous: None,
            close: None,
            cancel: default_cancel(),
        }
    }
}

/// A key, as the configuration file spells it: `Tab`, or `SHIFT + Tab`.
///
/// Super is held throughout a switch, so it's implied and needn't be written.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct Key {
    /// Modifiers besides Super, as Hyprland names them.
    pub(crate) mods: Vec<String>,
    /// The key itself, as libxkbcommon names it.
    pub(crate) key: String,
}

impl FromStr for Key {
    type Err = String;

    fn from_str(key: &str) -> Result<Self, Self::Err> {
        let mut parts: Vec<_> = key
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect();

        let Some(key) = parts.pop() else {
            return Err(format!("'{key}' doesn't name a key"));
        };

        Ok(Self {
            mods: parts.iter().map(|part| part.to_uppercase()).collect(),
            key: key.to_owned(),
        })
    }
}

impl TryFrom<String> for Key {
    type Error = String;

    fn try_from(key: String) -> Result<Self, Self::Error> {
        key.parse()
    }
}

impl Key {
    /// The key as one word, for the event Jaunt asks Hyprland to emit when
    /// it's pressed: `shift+a`, or just `a`.
    pub(crate) fn id(&self) -> String {
        let mut id: Vec<_> = self.mods.iter().map(|m| m.to_lowercase()).collect();
        id.sort();
        id.push(self.key.to_lowercase());

        id.join("+")
    }
}

impl fmt::Display for Key {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for modifier in &self.mods {
            write!(formatter, "{modifier} + ")?;
        }

        formatter.write_str(&self.key)
    }
}

/// How the switcher behaves, and how big it is.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Switcher {
    /// How long Super has to stay held, in milliseconds, before the switcher
    /// appears. Long enough that a tap never puts anything on screen, short
    /// enough to feel immediate when the user does mean to look.
    #[serde(default = "default_delay")]
    pub(crate) delay: u64,
    /// How wide the switcher is. Windows that don't fit scroll.
    #[serde(default = "default_width")]
    pub(crate) width: Size,
    /// How tall it may be. Thumbnails are made smaller to fit, if they have
    /// to.
    #[serde(default = "default_max_height")]
    pub(crate) max_height: Size,
    /// Whether to show each application's icon, rather than its initials.
    #[serde(default = "enabled")]
    pub(crate) icons: bool,
    /// How solid the panel is, which the rest of the switcher lies on.
    #[serde(default = "default_background_opacity")]
    pub(crate) background_opacity: Opacity,
    /// How solid the sheet the windows lie on is, and the tab joining it to
    /// their application.
    #[serde(default = "default_foreground_opacity")]
    pub(crate) foreground_opacity: Opacity,
    /// The colours it's drawn in: the name of a theme, either the user's own
    /// in `~/.config/jaunt/themes` or one Jaunt comes with, like `default`,
    /// `light` or `catppuccin-mocha`. Or the path to a theme file.
    #[serde(default = "default_theme")]
    pub(crate) theme: String,
    /// The numbers beside an application's windows.
    #[serde(default)]
    pub(crate) numbers: Numbers,
}

/// The numbers beside an application's windows, which reach a window by its
/// place in the row.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Numbers {
    /// What pressing one does.
    #[serde(default = "default_action")]
    pub(crate) action: Action,
}

impl Default for Numbers {
    fn default() -> Self {
        Self {
            action: default_action(),
        }
    }
}

/// What a window's number key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Action {
    /// Moves the highlight to that window, which releasing Super then
    /// switches to, the way every other key in the switcher works.
    Select,
    /// Switches to it there and then, without waiting for Super.
    Switch,
}

impl Switcher {
    pub(crate) fn delay(&self) -> Duration {
        Duration::from_millis(self.delay)
    }
}

impl Default for Switcher {
    fn default() -> Self {
        Self {
            delay: default_delay(),
            width: default_width(),
            max_height: default_max_height(),
            icons: enabled(),
            background_opacity: default_background_opacity(),
            foreground_opacity: default_foreground_opacity(),
            theme: default_theme(),
            numbers: Numbers::default(),
        }
    }
}

/// How much of what is behind something it hides: 1 for all of it, 0 for
/// none.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(try_from = "f64")]
pub(crate) struct Opacity(f32);

impl Opacity {
    pub(crate) fn get(self) -> f32 {
        self.0
    }
}

impl TryFrom<f64> for Opacity {
    type Error = String;

    fn try_from(opacity: f64) -> Result<Self, Self::Error> {
        if !(0.0..=1.0).contains(&opacity) {
            return Err(format!(
                "{opacity} is not an opacity: it goes from 0, for none at all, to 1, for solid"
            ));
        }

        #[allow(clippy::cast_possible_truncation)]
        Ok(Self(opacity as f32))
    }
}

/// A length, written either in pixels or as a portion of the screen: `900` or
/// `"60%"`.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(try_from = "Measure")]
pub(crate) enum Size {
    Pixels(i32),
    Portion(f32),
}

/// A size as the configuration file writes it.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Measure {
    Pixels(i32),
    Portion(String),
}

impl TryFrom<Measure> for Size {
    type Error = String;

    fn try_from(measure: Measure) -> Result<Self, Self::Error> {
        let portion = match measure {
            Measure::Pixels(pixels) => return Ok(Self::Pixels(pixels)),
            Measure::Portion(portion) => portion,
        };

        let percent = portion
            .trim()
            .strip_suffix('%')
            .and_then(|percent| percent.trim().parse::<f32>().ok())
            .ok_or_else(|| {
                format!("'{portion}' is neither a number of pixels nor a percentage of the screen")
            })?;

        Ok(Self::Portion(percent / 100.0))
    }
}

/// Thumbnails of the windows of the application being switched to.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Previews {
    /// Whether to capture them at all.
    #[serde(default = "enabled")]
    pub(crate) enabled: bool,
    /// How tall a thumbnail is, in pixels. Every thumbnail is this tall and
    /// as wide as the window's own proportions make it.
    #[serde(default = "default_preview_height")]
    pub(crate) height: u32,
}

impl Default for Previews {
    fn default() -> Self {
        Self {
            enabled: enabled(),
            height: default_preview_height(),
        }
    }
}

fn default_cancel() -> Key {
    Key {
        mods: Vec::new(),
        key: "Escape".to_owned(),
    }
}

fn default_delay() -> u64 {
    90
}

fn default_width() -> Size {
    Size::Portion(0.5)
}

fn default_max_height() -> Size {
    Size::Portion(0.4)
}

fn default_background_opacity() -> Opacity {
    Opacity(0.92)
}

fn default_foreground_opacity() -> Opacity {
    Opacity(1.0)
}

fn default_theme() -> String {
    "default".to_owned()
}

fn default_action() -> Action {
    Action::Select
}

fn enabled() -> bool {
    true
}

fn default_preview_height() -> u32 {
    150
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(file: &str) -> Config {
        toml::from_str(file).expect("failed to parse")
    }

    #[test]
    fn an_application_is_either_a_command_or_a_command_and_a_class() {
        let config = config(
            r#"
            [keys.apps]
            t = "ghostty"
            i = { cmd = "brave", app_id = "brave-browser" }
            "#,
        );

        assert_eq!(config.target("t"), Some(&Target::new("ghostty", None)));
        assert_eq!(
            config.target("i"),
            Some(&Target::new("brave", Some("brave-browser")))
        );
        assert_eq!(config.target("q"), None);
    }

    #[test]
    fn what_the_file_leaves_out_keeps_its_default() {
        let config = config(
            r#"
            [switcher]
            delay = 150
            "#,
        );

        assert_eq!(config.switcher.delay, 150);
        assert_eq!(config.switcher.width, default_width());
        assert_eq!(config.keys.cancel.key, "Escape");
    }

    #[test]
    fn nothing_is_bound_until_the_file_says_so() {
        assert!(Config::default().keys.apps.is_empty());
        assert!(config("").keys.apps.is_empty());
    }

    #[test]
    fn an_application_key_may_carry_modifiers_too() {
        let config = config(
            r#"
            [keys.apps]
            a = "audacity"
            "SHIFT + a" = "teams-for-linux"
            "#,
        );

        assert_eq!(config.target("a"), Some(&Target::new("audacity", None)));
        assert_eq!(
            config.target("shift+a"),
            Some(&Target::new("teams-for-linux", None))
        );
    }

    #[test]
    fn a_key_names_itself_the_same_way_however_it_was_written() {
        let spelled: Key = "SHIFT + A".to_owned().try_into().expect("a key");
        let differently: Key = "shift+a".to_owned().try_into().expect("a key");

        assert_eq!(spelled.id(), "shift+a");
        assert_eq!(spelled.id(), differently.id());
    }

    #[test]
    fn a_key_may_carry_modifiers() {
        let config = config(
            r#"
            [keys]
            next = "Tab"
            previous = "SHIFT + Tab"
            "#,
        );

        assert_eq!(
            config.keys.next,
            Some(Key {
                mods: vec![],
                key: "Tab".to_owned()
            })
        );
        assert_eq!(
            config.keys.previous,
            Some(Key {
                mods: vec!["SHIFT".to_owned()],
                key: "Tab".to_owned()
            })
        );
        assert_eq!(config.keys.previous.unwrap().to_string(), "SHIFT + Tab");
    }

    #[test]
    fn a_typo_is_an_error_rather_than_a_setting_that_quietly_does_nothing() {
        let error = toml::from_str::<Config>(
            r#"
            [switcher]
            dealy = 150
            "#,
        )
        .expect_err("unknown fields should be refused");

        assert!(error.to_string().contains("dealy"), "{error}");
    }

    #[test]
    fn a_size_is_pixels_or_a_share_of_the_screen() {
        let config = config(
            r#"
            [switcher]
            width = "60%"
            max_height = 300
            "#,
        );

        assert_eq!(config.switcher.width, Size::Portion(0.6));
        assert_eq!(config.switcher.max_height, Size::Pixels(300));
    }

    #[test]
    fn opacities_default_to_a_little_see_through_and_solid() {
        let config = config("");

        assert_eq!(config.switcher.background_opacity, Opacity(0.92));
        assert_eq!(config.switcher.foreground_opacity, Opacity(1.0));
    }

    #[test]
    fn an_opacity_is_a_number_from_none_to_solid() {
        let config = config(
            r#"
            [switcher]
            background_opacity = 0.5
            foreground_opacity = 1
            "#,
        );

        assert_eq!(config.switcher.background_opacity, Opacity(0.5));
        assert_eq!(config.switcher.foreground_opacity, Opacity(1.0));

        let error = toml::from_str::<Config>(
            r#"
            [switcher]
            background_opacity = 1.5
            "#,
        )
        .expect_err("1.5 is more than solid");

        assert!(error.to_string().contains("opacity"), "{error}");
    }

    #[test]
    fn the_switcher_is_drawn_in_jaunts_own_theme_unless_the_file_names_another() {
        assert_eq!(config("").switcher.theme, "default");
        assert_eq!(config("").palette, Palette::default());

        let config = config(
            r#"
            [switcher]
            theme = "light"
            "#,
        );

        assert_eq!(config.switcher.theme, "light");
    }

    /// The file Jaunt hands out has to say what Jaunt would have done
    /// anyway, or it teaches the user something untrue the first time a
    /// default changes.
    #[test]
    fn the_configuration_jaunt_ships_sets_everything_to_its_default() {
        assert_eq!(config(EXAMPLE), Config::default());
    }

    #[test]
    fn a_number_key_selects_its_window_unless_the_file_asks_it_to_switch() {
        assert_eq!(config("").switcher.numbers.action, Action::Select);

        let config = config(
            r#"
            [switcher.numbers]
            action = "switch"
            "#,
        );

        assert_eq!(config.switcher.numbers.action, Action::Switch);

        let error = toml::from_str::<Config>(
            r#"
            [switcher.numbers]
            action = "jump"
            "#,
        )
        .expect_err("'jump' is not one of them");

        assert!(error.to_string().contains("select"), "{error}");
    }

    #[test]
    fn a_size_that_is_neither_says_so() {
        let error = toml::from_str::<Config>(
            r#"
            [switcher]
            width = "wide"
            "#,
        )
        .expect_err("'wide' is not a size");

        assert!(error.to_string().contains("percentage"), "{error}");
    }

    #[test]
    fn previews_can_be_turned_off_without_touching_anything_else() {
        let config = config(
            r#"
            [previews]
            enabled = false
            "#,
        );

        assert!(!config.previews.enabled);
        assert_eq!(config.previews.height, default_preview_height());
        assert!(config.switcher.icons);
    }
}
