//! Talks to Hyprland over its two IPC sockets: one that answers requests, one
//! that streams events.
//!
//! Going through the sockets rather than spawning `hyprctl` keeps a switch off
//! the process-spawning path entirely, which is what lets a key press turn
//! into a focused window in about a millisecond.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};

use crate::compositor::{self, Window};
use crate::config::{Config, Key, Keys};

/// Everything Jaunt asks Hyprland to emit is prefixed with this, which is
/// also how Jaunt recognises its own keybinds among everyone else's.
const EVENT_PREFIX: &str = "jaunt:";

/// What Hyprland counts each modifier as.
fn modmask(modifier: &str) -> u32 {
    match modifier.to_uppercase().as_str() {
        "SHIFT" => 1,
        "CAPS" => 2,
        "CTRL" | "CONTROL" => 4,
        "ALT" | "MOD1" => 8,
        "MOD2" => 16,
        "MOD3" => 32,
        "SUPER" | "WIN" | "LOGO" | "MOD4" | "META" => 64,
        "MOD5" => 128,
        _ => 0,
    }
}

/// The event Hyprland emits when a mapped key is pressed, followed by the
/// letter that was pressed.
pub(crate) const SWITCH_EVENT: &str = "jaunt:switch:";

/// The same, for Shift and a mapped key, which goes the other way.
pub(crate) const BACK_EVENT: &str = "jaunt:back:";

/// The same, for Ctrl and a mapped key, which starts another copy of the
/// application rather than switching to one that is already running.
pub(crate) const LAUNCH_EVENT: &str = "jaunt:launch:";

/// The events the keys that steer a switch in progress emit.
pub(crate) const NEXT_EVENT: &str = "jaunt:next";
pub(crate) const PREVIOUS_EVENT: &str = "jaunt:previous";

/// The event Hyprland emits when Super is released.
pub(crate) const CONFIRM_EVENT: &str = "jaunt:confirm";

/// The event Hyprland emits when Escape is pressed while the switcher is on
/// screen.
pub(crate) const CANCEL_EVENT: &str = "jaunt:cancel";

/// Emitted by the key that closes the highlighted window.
pub(crate) const CLOSE_EVENT: &str = "jaunt:close";

/// Emitted by a window's own number key, with the number after it.
pub(crate) const SELECT_EVENT: &str = "jaunt:select:";

/// The windows a number reaches: the ones the digits across the top of the
/// keyboard can name, which is as many as are worth numbering.
pub(crate) const NUMBERED: std::ops::RangeInclusive<usize> = 1..=9;

/// The Super keys a switch can be confirmed with.
const SUPER_KEYS: [&str; 2] = ["Super_L", "Super_R"];

pub struct Compositor;

/// A window, the way Hyprland's `clients` and `activewindow` describe one.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Client {
    address: String,
    class: String,
    title: String,
    mapped: bool,
    #[serde(rename = "focusHistoryID")]
    focus_history_id: i32,
    /// Width and height on screen.
    #[serde(default)]
    size: [i32; 2],
    #[serde(default)]
    initial_title: String,
}

impl From<Client> for Window {
    fn from(client: Client) -> Self {
        Self {
            id: client.address,
            app_id: client.class,
            title: client.title,
            initial_title: client.initial_title,
            #[allow(clippy::cast_sign_loss)]
            size: match client.size {
                [width, height] if width > 0 && height > 0 => Some((width as u32, height as u32)),
                _ => None,
            },
        }
    }
}

/// Where Hyprland keeps a running instance's sockets.
///
/// `$HYPRLAND_INSTANCE_SIGNATURE` says which instance a client belongs to, but
/// a terminal outlives the session it was opened in, and the directory of a
/// session that has gone stays behind. The variable is therefore a preference
/// rather than an answer: Jaunt follows it while something is listening
/// there, and otherwise looks for the Hyprland that is.
pub(crate) fn instance_dir() -> Result<PathBuf> {
    static RESOLVED: OnceLock<PathBuf> = OnceLock::new();

    if let Some(resolved) = RESOLVED.get() {
        return Ok(resolved.clone());
    }

    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("`$XDG_RUNTIME_DIR` is not set")?;
    let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok();
    let instance = resolve(&PathBuf::from(runtime).join("hypr"), signature.as_deref())?;

    let _ = RESOLVED.set(instance.clone());

    Ok(instance)
}

/// Which of the instances under `hypr` to talk to.
fn resolve(hypr: &Path, signature: Option<&str>) -> Result<PathBuf> {
    if let Some(named) = signature.map(|signature| hypr.join(signature))
        && listening(&named)
    {
        return Ok(named);
    }

    let mut running: Vec<_> = std::fs::read_dir(hypr)
        .with_context(|| format!("failed to look in {}; is Hyprland running?", hypr.display()))?
        .filter_map(Result::ok)
        .map(|instance| instance.path())
        .filter(|instance| listening(instance))
        .collect();
    running.sort();

    match running.len() {
        0 => anyhow::bail!("no running Hyprland found in {}", hypr.display()),
        1 => {
            if signature.is_some() {
                eprintln!(
                    "jaunt: $HYPRLAND_INSTANCE_SIGNATURE names a Hyprland that has gone \
                     (an old terminal, most likely); using the one that is running"
                );
            }

            Ok(running.remove(0))
        }
        _ => anyhow::bail!(
            "more than one Hyprland is running, and $HYPRLAND_INSTANCE_SIGNATURE doesn't name \
             any of them; set it to the one jaunt should use: {}",
            running
                .iter()
                .filter_map(|instance| instance.file_name()?.to_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Whether an instance's directory belongs to a Hyprland that is still there.
fn listening(instance: &Path) -> bool {
    UnixStream::connect(instance.join(".socket.sock")).is_ok()
}

/// Sends `command` to Hyprland and returns what it answered.
pub(crate) fn request(command: &str) -> Result<String> {
    let path = instance_dir()?.join(".socket.sock");

    let mut socket = UnixStream::connect(&path)
        .with_context(|| format!("failed to connect to Hyprland at {}", path.display()))?;

    socket
        .write_all(command.as_bytes())
        .context("failed to send a command to Hyprland")?;
    let _ = socket.shutdown(std::net::Shutdown::Write);

    let mut response = String::new();
    socket
        .read_to_string(&mut response)
        .context("failed to read Hyprland's response")?;

    Ok(response)
}

/// Connects to Hyprland's event stream, which reports one event per line.
pub(crate) fn events() -> Result<UnixStream> {
    let path = instance_dir()?.join(".socket2.sock");

    UnixStream::connect(&path).with_context(|| {
        format!(
            "failed to connect to Hyprland's events at {}",
            path.display()
        )
    })
}

/// Which parser Hyprland's configuration is written for. Keybinds added at
/// runtime are spelled differently for each, and each parser refuses the
/// other's spelling.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConfigLanguage {
    Legacy,
    Lua,
}

/// Asks Hyprland which parser it's using, by sending it a `keyword` command
/// too short to do anything: the Lua parser refuses the command itself, the
/// legacy one gets as far as complaining about the arguments.
pub(crate) fn config_language() -> ConfigLanguage {
    static LANGUAGE: OnceLock<ConfigLanguage> = OnceLock::new();

    *LANGUAGE.get_or_init(|| match request("keyword") {
        Ok(response) if response.contains("non-legacy") => ConfigLanguage::Lua,
        _ => ConfigLanguage::Legacy,
    })
}

/// Whether Jaunt owns a key outright, or shares it with whatever else is
/// bound to it.
///
/// The distinction matters because Hyprland removes keybinds by key, not by
/// bind: unbinding a key Jaunt shares would take the user's binding with it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ownership {
    /// Jaunt's alone while it runs. Anything else bound to the key is
    /// removed, because both firing at once is worse than either: the other
    /// binding would act on a window while Jaunt is still asking which one.
    Exclusive,
    /// Jaunt's bind joins whatever is already bound to the key, and only
    /// Jaunt's is taken away again.
    ///
    /// This is what keeps a `Super` tap bound to a launcher working. Hyprland
    /// shadows that binding by itself as soon as another bind has fired while
    /// Super was held, so it stays quiet exactly when the user was switching
    /// windows rather than tapping Super.
    Shared,
}

/// One of the keybinds the daemon installs.
struct Bind {
    /// Identifies the bind to Hyprland's Lua state across daemon restarts,
    /// e.g. `switch:t`.
    name: String,
    /// The modifiers that have to be held, as Hyprland names them.
    mods: Vec<String>,
    /// The key itself, e.g. `i`, `Super_L` or `Escape`.
    key: String,
    /// The event Hyprland emits for it.
    event: String,
    /// What it does, for when Jaunt has to explain itself.
    role: String,
    flags: Flags,
    ownership: Ownership,
    /// Whether something else was bound to this key when Jaunt started. A
    /// shared key that was occupied can't have Jaunt's bind taken off it
    /// again without taking the other one too, so Jaunt leaves its own
    /// behind: it consumes nothing and nobody is listening for its event.
    occupied: bool,
}

/// The ways a bind can depart from "fires when the key goes down, and the
/// application never sees it".
#[derive(Default, Clone, Copy)]
struct Flags {
    /// Fires when the key is let go rather than pressed.
    release: bool,
    /// Survives another bind having fired while the key was held. Hyprland
    /// otherwise shadows the bind, which would silence the release of Super
    /// every time the user actually picked a window with it.
    transparent: bool,
    /// Leaves the key to the focused application as well.
    non_consuming: bool,
    /// Fires whatever else is being held down at the time.
    ignore_mods: bool,
}

impl Bind {
    /// The commands that put the bind in place.
    fn add(&self, language: ConfigLanguage) -> Vec<String> {
        match language {
            ConfigLanguage::Legacy => {
                let add = format!(
                    "keyword bind{} {},event,{}",
                    self.flags.letters(),
                    self.keys(language),
                    self.event
                );

                match self.ownership {
                    // Replace whatever is on the key, Jaunt's own leftovers
                    // included.
                    Ownership::Exclusive => {
                        vec![format!("keyword unbind {}", self.keys(language)), add]
                    }
                    Ownership::Shared => vec![add],
                }
            }
            // Lua keybinds are objects, so Jaunt keeps hold of its own and
            // leaves every other binding alone. The table outlives the daemon,
            // which is what lets a restart reuse a bind instead of stacking a
            // second one on the same key.
            ConfigLanguage::Lua => {
                let bind = format!(
                    r#"hl.bind("{}", hl.dsp.event("{}"){})"#,
                    self.keys(language),
                    self.event,
                    self.flags.options()
                );

                let body = match self.ownership {
                    // `hl.unbind` destroys every bind on the key, so anything
                    // Jaunt is still holding for it has to go first and by
                    // handle. A handle whose bind was destroyed under it is
                    // not inert: calling it takes the whole compositor down.
                    Ownership::Exclusive => format!(
                        r#"if e then e.bind:remove() __jaunt["{name}"] = nil end for n, v in pairs(__jaunt) do if v.keys == "{keys}" then v.bind:remove() __jaunt[n] = nil end end hl.unbind("{keys}") __jaunt["{name}"] = {{ bind = {bind}, shared = false, keys = "{keys}" }}"#,
                        keys = self.keys(language),
                        name = self.lua_name(),
                    ),
                    Ownership::Shared => format!(
                        r#"if e then e.bind:set_enabled(true) else __jaunt["{name}"] = {{ bind = {bind}, shared = true, keys = "{keys}" }} end"#,
                        keys = self.keys(language),
                        name = self.lua_name(),
                    ),
                };

                vec![format!(
                    r#"eval __jaunt = __jaunt or {{}} do local e = __jaunt["{}"] {body} end"#,
                    self.lua_name()
                )]
            }
        }
    }

    /// The commands that take it away again, leaving everything else bound.
    fn remove(&self, language: ConfigLanguage) -> Vec<String> {
        match (language, self.ownership) {
            (ConfigLanguage::Legacy, Ownership::Shared) if self.occupied => vec![],
            (ConfigLanguage::Legacy, _) => vec![format!("keyword unbind {}", self.keys(language))],
            (ConfigLanguage::Lua, Ownership::Exclusive) => vec![format!(
                r#"eval do local e = __jaunt["{name}"] if e then e.bind:remove() __jaunt["{name}"] = nil end end"#,
                name = self.lua_name()
            )],
            // Disabling is the only way to stop a shared Lua bind firing
            // without removing the key's other bindings. It stops matching
            // keys and stops consuming them; the next run enables it again.
            (ConfigLanguage::Lua, Ownership::Shared) => vec![format!(
                r#"eval do local e = __jaunt["{}"] if e then e.bind:set_enabled(false) end end"#,
                self.lua_name()
            )],
        }
    }

    /// The key, spelled the way the configuration language in use spells it:
    /// `SUPER,i` for the legacy parser, `SUPER + i` for the Lua one.
    fn keys(&self, language: ConfigLanguage) -> String {
        match language {
            ConfigLanguage::Legacy => format!("{},{}", self.mods.join(" "), self.key),
            ConfigLanguage::Lua if self.mods.is_empty() => self.key.clone(),
            ConfigLanguage::Lua => format!("{} + {}", self.mods.join(" + "), self.key),
        }
    }

    /// What the bind is called in Hyprland's Lua state.
    ///
    /// The definition is part of the name, so changing a key or a flag makes a
    /// new bind rather than re-enabling the one the last run left behind. The
    /// options are named in full rather than by their legacy letters: not
    /// every one of them has a letter, and a bind whose options changed but
    /// whose name didn't would be silently kept as it was.
    fn lua_name(&self) -> String {
        format!(
            "{}@{}#{}",
            self.name,
            self.keys(ConfigLanguage::Lua),
            self.flags.set().join("+")
        )
    }

    fn modmask(&self) -> u32 {
        self.mods.iter().map(|modifier| modmask(modifier)).sum()
    }

    /// The key itself, as something to compare binds by: two binds in the same
    /// slot are two binds on the same key.
    fn slot(&self) -> (u32, String) {
        (self.modmask(), self.key.to_lowercase())
    }

    /// The key as a person writes it.
    fn spelled(&self) -> String {
        self.keys(ConfigLanguage::Lua)
    }
}

impl Flags {
    /// The letters the legacy parser spells these with, as in `bindrtn`.
    fn letters(self) -> String {
        let letters = [
            (self.release, 'r'),
            (self.transparent, 't'),
            (self.non_consuming, 'n'),
            (self.ignore_mods, 'i'),
        ];

        letters
            .into_iter()
            .filter_map(|(set, letter)| set.then_some(letter))
            .collect()
    }

    /// Which of them are set, by the name the Lua parser knows them by.
    fn set(self) -> Vec<&'static str> {
        let options = [
            (self.release, "release"),
            (self.transparent, "transparent"),
            (self.non_consuming, "non_consuming"),
            (self.ignore_mods, "ignore_mods"),
        ];

        options
            .into_iter()
            .filter_map(|(set, option)| set.then_some(option))
            .collect()
    }

    /// The same, as the table the Lua parser takes.
    fn options(self) -> String {
        let options: Vec<_> = self
            .set()
            .into_iter()
            .map(|option| format!("{option} = true"))
            .collect();

        if options.is_empty() {
            String::new()
        } else {
            format!(", {{ {} }}", options.join(", "))
        }
    }
}

/// The binds for one steering key.
///
/// Super is held for the whole of an ordinary switch, so the key is bound with
/// it; a switch started from `jaunt switch` has nobody holding anything, so
/// it's bound without it too. Hyprland matches the modifiers exactly, so only
/// one of the two ever fires, and `SHIFT + Tab` stays distinct from `Tab`.
fn session_binds(name: &str, key: &Key, event: &str) -> Vec<Bind> {
    [("", Vec::new()), (":super", vec!["SUPER".to_owned()])]
        .into_iter()
        .map(|(suffix, mut mods)| {
            mods.extend(key.mods.iter().cloned());

            Bind {
                name: format!("{name}{suffix}"),
                mods,
                key: key.key.clone(),
                event: event.to_owned(),
                role: name.to_owned(),
                // These keys are only bound while the switcher is on screen,
                // and there they belong to it: passing them on as well would
                // walk the switcher and type into the window behind it at the
                // same time. A daemon that dies holding them leaves them
                // behind, which the next run clears.
                flags: Flags::default(),
                ownership: Ownership::Shared,
                occupied: false,
            }
        })
        .collect()
}

/// The binds that close the switcher.
///
/// Cancelling means the same thing whatever is held down, so the key is bound
/// once and matched loosely — unless a steering key uses it too, in which case
/// the modifiers are what tells them apart and have to be matched exactly.
fn cancel_binds(keys: &Keys) -> Vec<Bind> {
    let steering = [
        keys.next.as_ref(),
        keys.previous.as_ref(),
        keys.close.as_ref(),
    ];
    let shared_with_steering = steering
        .into_iter()
        .flatten()
        .any(|key| key.key == keys.cancel.key);

    if shared_with_steering || !keys.cancel.mods.is_empty() {
        return session_binds("cancel", &keys.cancel, CANCEL_EVENT);
    }

    vec![Bind {
        name: "cancel".to_owned(),
        mods: Vec::new(),
        key: keys.cancel.key.clone(),
        event: CANCEL_EVENT.to_owned(),
        role: "cancel".to_owned(),
        flags: Flags {
            ignore_mods: true,
            ..Flags::default()
        },
        ownership: Ownership::Shared,
        occupied: false,
    }]
}

/// A keybind Hyprland already had, as `binds` reports it.
#[derive(serde::Deserialize)]
struct ExistingBind {
    key: String,
    modmask: u32,
    #[serde(default)]
    arg: String,
}

/// What else is bound to the keys Jaunt is about to take.
struct Occupied {
    /// How many binds that aren't Jaunt's are on each key.
    others: HashMap<(u32, String), usize>,
}

impl Occupied {
    fn scan(language: ConfigLanguage) -> Self {
        let mut others: HashMap<(u32, String), usize> = HashMap::new();

        let Ok(response) = request("j/binds") else {
            return Self { others };
        };
        let Ok(existing) = serde_json::from_str::<Vec<ExistingBind>>(&response) else {
            return Self { others };
        };

        for bind in existing {
            // The legacy parser keeps Jaunt's own events in plain sight.
            if bind.arg.starts_with(EVENT_PREFIX) {
                continue;
            }

            *others
                .entry((bind.modmask, bind.key.to_lowercase()))
                .or_default() += 1;
        }

        // The Lua parser doesn't: its binds carry a reference rather than a
        // name, so Jaunt has to ask which ones are its own.
        for (slot, count) in jaunt_owned(language) {
            if let Some(total) = others.get_mut(&slot) {
                *total = total.saturating_sub(count);
            }
        }

        Self { others }
    }

    fn others(&self, bind: &Bind) -> usize {
        self.others.get(&bind.slot()).copied().unwrap_or(0)
    }
}

/// Whether Jaunt's own bind is already on this key from an earlier run.
///
/// Only shared keys can be: an exclusive one clears the key before binding it.
fn already_there(bind: &Bind, installed: &HashSet<(u32, String)>) -> bool {
    bind.ownership == Ownership::Shared && installed.contains(&bind.slot())
}

/// The keys that already carry a bind of Jaunt's.
///
/// A shared key that something else is bound to keeps Jaunt's bind when the
/// daemon stops, because taking it off would take the other one with it. The
/// next run has to recognise it rather than add a second one.
fn installed_by_jaunt(language: ConfigLanguage) -> HashSet<(u32, String)> {
    // Lua binds are kept by name in a table of Jaunt's own, which can't hold
    // the same one twice.
    if language != ConfigLanguage::Lua {
        return request("j/binds")
            .ok()
            .and_then(|response| serde_json::from_str::<Vec<ExistingBind>>(&response).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|bind| bind.arg.starts_with(EVENT_PREFIX))
            .map(|bind| (bind.modmask, bind.key.to_lowercase()))
            .collect();
    }

    HashSet::new()
}

/// The keys Jaunt already holds in this compositor, as the Lua state knows
/// them. Binds an earlier run left behind are Jaunt's own, not a conflict.
fn jaunt_owned(language: ConfigLanguage) -> HashMap<(u32, String), usize> {
    let mut owned = HashMap::new();

    if language != ConfigLanguage::Lua {
        return owned;
    }

    let listing = r#"repl local out = {} for _, e in pairs(__jaunt or {}) do out[#out+1] = e.bind.display_key end return table.concat(out, "\n")"#;
    let Ok(response) = request(listing) else {
        return owned;
    };

    for line in response.lines() {
        if let Some(slot) = parse_keys(line) {
            *owned.entry(slot).or_default() += 1;
        }
    }

    owned
}

/// `SUPER + Super_L`, the way both Hyprland and Jaunt spell a key, as the
/// modifiers and the key it stands for.
fn parse_keys(keys: &str) -> Option<(u32, String)> {
    let mut parts: Vec<_> = keys
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    let key = parts.pop()?;

    Some((
        parts.iter().map(|part| modmask(part)).sum(),
        key.to_lowercase(),
    ))
}

/// The keybinds that make the switcher work, for as long as the daemon runs.
///
/// Hyprland forgets them when its configuration is reloaded, so the daemon
/// puts them back when it sees that happen, and takes them away when it exits.
pub(crate) struct Binds {
    /// Installed for the daemon's lifetime.
    binds: Vec<Bind>,
    /// Installed only while the switcher is on screen.
    session: Vec<Bind>,
    language: ConfigLanguage,
}

impl Binds {
    /// Installs a binding for every mapped letter — one for each direction,
    /// and one that starts another copy of the application — plus the ones
    /// that confirm a switch when Super is released.
    pub(crate) fn install(config: &Config) -> Result<Self> {
        let switches = config.keys.apps.iter().flat_map(|(key, target)| {
            let id = key.id();
            let mut mods = vec!["SUPER".to_owned()];
            mods.extend(key.mods.iter().cloned());

            let forward = Bind {
                name: format!("switch:{id}"),
                mods,
                key: key.key.clone(),
                event: format!("{SWITCH_EVENT}{id}"),
                role: format!("the key for {}", target.app),
                flags: Flags::default(),
                ownership: Ownership::Exclusive,
                occupied: false,
            };

            // Shift and the same key walks the group the other way — but only
            // for a key that doesn't already carry modifiers of its own, since
            // there would be nowhere left to put the Shift.
            let backward = key.mods.is_empty().then(|| Bind {
                name: format!("back:{id}"),
                mods: vec!["SUPER".to_owned(), "SHIFT".to_owned()],
                key: key.key.clone(),
                event: format!("{BACK_EVENT}{id}"),
                role: format!("the key for {}, backwards", target.app),
                flags: Flags::default(),
                ownership: Ownership::Exclusive,
                occupied: false,
            });

            // Ctrl and the same key runs the application's command again,
            // for when the window you want doesn't exist yet.
            let mut launch_mods = vec!["SUPER".to_owned(), "CTRL".to_owned()];
            launch_mods.extend(key.mods.iter().cloned());

            let launch = Bind {
                name: format!("launch:{id}"),
                mods: launch_mods,
                key: key.key.clone(),
                event: format!("{LAUNCH_EVENT}{id}"),
                role: format!("the key for a new {}", target.app),
                flags: Flags::default(),
                ownership: Ownership::Exclusive,
                occupied: false,
            };

            std::iter::once(forward).chain(backward).chain([launch])
        });

        let confirms = SUPER_KEYS.iter().map(|key| Bind {
            name: format!("confirm:{key}"),
            mods: vec!["SUPER".to_owned()],
            key: (*key).to_owned(),
            event: CONFIRM_EVENT.to_owned(),
            role: "confirming a switch".to_owned(),
            flags: Flags {
                release: true,
                transparent: true,
                non_consuming: true,
                // Shift is held whenever the user was walking a group
                // backwards, and letting go of Super still means confirm.
                ignore_mods: true,
            },
            ownership: Ownership::Shared,
            occupied: false,
        });

        let steering = [
            ("next", config.keys.next.as_ref(), NEXT_EVENT),
            ("previous", config.keys.previous.as_ref(), PREVIOUS_EVENT),
            ("close", config.keys.close.as_ref(), CLOSE_EVENT),
        ];
        let mut session: Vec<_> = steering
            .into_iter()
            .filter_map(|(name, key, event)| Some((name, key?, event)))
            .flat_map(|(name, key, event)| session_binds(name, key, event))
            .collect();

        session.extend(cancel_binds(&config.keys));

        // A window's own number. These are the only keys Jaunt binds without
        // being told to, which it can afford because they are its own only
        // while the switcher is on screen.
        session.extend(NUMBERED.flat_map(|number| {
            let key = Key {
                mods: Vec::new(),
                key: number.to_string(),
            };

            session_binds(
                &format!("select:{number}"),
                &key,
                &format!("{SELECT_EVENT}{number}"),
            )
        }));

        // A key configured with Shift of its own lands on the same key as
        // some other key's backwards bind. Both would be installed, and
        // whichever went second would unbind the first while Jaunt still
        // held it. The configured key wins; the derived one is dropped.
        // A key the user configured beats one Jaunt derived from another
        // key, whichever way round they were listed.
        let derived =
            |bind: &Bind| bind.name.starts_with("back:") || bind.name.starts_with("launch:");
        let (forwards, backwards): (Vec<_>, Vec<_>) =
            switches.chain(confirms).partition(|bind| !derived(bind));
        let taken: HashMap<_, _> = forwards
            .iter()
            .map(|bind| (bind.slot(), bind.role.clone()))
            .collect();
        let (dropped, backwards): (Vec<_>, Vec<_>) = backwards
            .into_iter()
            .partition(|bind| taken.contains_key(&bind.slot()));

        for bind in &dropped {
            eprintln!(
                "jaunt: {} is {}, so it can't also be {}",
                bind.spelled(),
                taken[&bind.slot()],
                bind.role,
            );
        }

        let binds = forwards.into_iter().chain(backwards).collect();

        let mut binds = Self {
            language: config_language(),
            binds,
            session,
        };

        binds.warn_about_conflicts(config);
        binds.forget_stale_lua_binds();
        binds.add()?;

        // The keys that steer a switch swallow what they're bound to, so they
        // belong to the switcher only while it's on screen. A run that ended
        // without giving them back — a crash, most likely — would otherwise
        // leave them taken until the next switch happened to end tidily.
        binds.release_session_keys();

        Ok(binds)
    }

    /// Says what Jaunt is about to do to keys that are already spoken for,
    /// and to keys it has asked for twice itself. Silence means every key it
    /// binds is its own and means one thing.
    fn warn_about_conflicts(&mut self, config: &Config) {
        if config.keys.apps.is_empty() {
            eprintln!(
                "jaunt: no applications are configured, so no keys are bound; \
                 add some under [keys.apps]"
            );
        }

        let mut roles: HashMap<(u32, String), String> = HashMap::new();

        for bind in self.binds.iter().chain(&self.session) {
            match roles.entry(bind.slot()) {
                Entry::Occupied(taken) => eprintln!(
                    "jaunt: {} is {} and {} at once; only one of them will happen",
                    bind.spelled(),
                    taken.get(),
                    bind.role
                ),
                Entry::Vacant(free) => {
                    free.insert(bind.role.clone());
                }
            }
        }

        let occupied = Occupied::scan(self.language);
        let mut mentioned = HashMap::new();

        for bind in self.binds.iter_mut().chain(&mut self.session) {
            bind.occupied = occupied.others(bind) > 0;

            // Every bind on a key fires, so one of the user's own on the key
            // that closes a window would do whatever it does as well: likely
            // close the focused window, which is a different one.
            if bind.occupied && bind.name == "close:super" {
                eprintln!(
                    "jaunt: {} is also bound in your Hyprland configuration, and would fire too \
                     when it closes a window in the switcher; have yours do nothing while the \
                     jaunt layer is on screen",
                    bind.spelled()
                );
            }

            // A shared key isn't worth mentioning: Jaunt's bind joins what is
            // there and nothing of the user's stops working. Only a key Jaunt
            // takes for itself is news.
            if !bind.occupied
                || bind.ownership == Ownership::Shared
                || mentioned.insert(bind.slot(), ()).is_some()
            {
                continue;
            }

            eprintln!(
                "jaunt: {} is already bound in your Hyprland configuration; jaunt uses it for \
                 {} while it runs, and `hyprctl reload` gives it back",
                bind.spelled(),
                bind.role
            );
        }
    }

    /// Drops binds an earlier run left in Hyprland's Lua state that this run
    /// has no use for: a letter that is no longer mapped, or a key or flag
    /// that has changed since.
    ///
    /// A shared bind can only be disabled, never removed, because removing it
    /// would take the key's other bindings with it.
    fn forget_stale_lua_binds(&self) {
        if self.language != ConfigLanguage::Lua {
            return;
        }

        let keep: Vec<_> = self
            .binds
            .iter()
            .chain(&self.session)
            .map(|bind| format!(r#"["{}"] = true"#, bind.lua_name()))
            .collect();

        let command = format!(
            r"eval __jaunt = __jaunt or {{}} do local keep = {{ {} }} for name, e in pairs(__jaunt) do if not keep[name] then if e.shared then e.bind:set_enabled(false) else e.bind:remove() end __jaunt[name] = nil end end end",
            keep.join(", ")
        );

        let _ = request(&command);
    }

    fn add(&self) -> Result<()> {
        let installed = installed_by_jaunt(self.language);

        for bind in &self.binds {
            if already_there(bind, &installed) {
                continue;
            }

            for command in bind.add(self.language) {
                let response = request(&command)?;
                anyhow::ensure!(
                    response.trim() == "ok",
                    "Hyprland refused the keybind for {}: {}",
                    bind.keys(self.language),
                    response.trim()
                );
            }
        }

        Ok(())
    }

    /// Binds the keys that steer a switch, for as long as the switcher is on
    /// screen to use them. They belong to the applications underneath the rest
    /// of the time.
    pub(crate) fn capture_session_keys(&self) {
        let installed = installed_by_jaunt(self.language);

        for bind in &self.session {
            if already_there(bind, &installed) {
                continue;
            }

            for command in bind.add(self.language) {
                if let Err(error) = request(&command) {
                    eprintln!("jaunt: failed to bind {}: {error:#}", bind.key);
                }
            }
        }
    }

    /// Gives them back.
    pub(crate) fn release_session_keys(&self) {
        for bind in &self.session {
            for command in bind.remove(self.language) {
                let _ = request(&command);
            }
        }
    }

    /// Puts the keybinds back after Hyprland reloaded its configuration.
    pub(crate) fn reinstall(&self) {
        if let Err(error) = self.add() {
            eprintln!("jaunt: failed to restore keybinds after a config reload: {error:#}");
        }
    }

    /// Takes the keybinds away, leaving Hyprland as it was found.
    pub(crate) fn remove(&self) {
        for bind in &self.binds {
            for command in bind.remove(self.language) {
                let _ = request(&command);
            }
        }

        self.release_session_keys();
    }
}

impl Compositor {
    /// Asks a window to close, the way its own close button would: it may
    /// still ask about unsaved work.
    pub(crate) fn close_window(&self, window: &Window) -> Result<()> {
        let id = &window.id;
        // The window is always named: without one, Hyprland closes whichever
        // window is focused instead.
        let command = match config_language() {
            ConfigLanguage::Legacy => format!("dispatch closewindow address:{id}"),
            ConfigLanguage::Lua => {
                format!(r#"dispatch hl.dsp.window.close({{ window = 'address:{id}' }})"#)
            }
        };

        let response = request(&command)?;
        anyhow::ensure!(
            response.trim().starts_with("ok"),
            "Hyprland refused to close window {id}: {}",
            response.trim()
        );

        Ok(())
    }
}

impl compositor::Compositor for Compositor {
    fn name(&self) -> &'static str {
        "hyprland"
    }

    fn get_windows(&self) -> Result<Vec<Window>> {
        let response = request("j/clients")?;
        let mut clients: Vec<Client> =
            serde_json::from_str(&response).context("failed to parse Hyprland's window list")?;

        clients.retain(|client| client.mapped && !client.class.is_empty());
        clients.sort_by_key(|client| client.focus_history_id);

        Ok(clients.into_iter().map(Window::from).collect())
    }

    fn get_focused_window(&self) -> Result<Option<Window>> {
        let response = request("j/activewindow")?;
        let client: serde_json::Value =
            serde_json::from_str(&response).context("failed to parse Hyprland's focused window")?;

        // Hyprland answers with an empty object when nothing is focused.
        if client.get("address").is_none() {
            return Ok(None);
        }

        let client: Client =
            serde_json::from_value(client).context("failed to parse Hyprland's focused window")?;

        Ok(Some(client.into()))
    }

    fn focus_window(&self, window: &Window) -> Result<()> {
        let id = &window.id;
        let command = match config_language() {
            ConfigLanguage::Legacy => format!("dispatch focuswindow address:{id}"),
            ConfigLanguage::Lua => {
                format!(r#"dispatch hl.dsp.focus({{ window = 'address:{id}' }})"#)
            }
        };

        let response = request(&command)?;
        anyhow::ensure!(
            response.trim().starts_with("ok"),
            "Hyprland refused to focus window {id}: {}",
            response.trim()
        );

        Ok(())
    }

    /// Lets Hyprland start the application, so it outlives the daemon and gets
    /// the same treatment as anything else the user launches.
    fn launch_application(&self, cmd: &str) -> Result<()> {
        let command = match config_language() {
            ConfigLanguage::Legacy => format!("dispatch exec {cmd}"),
            ConfigLanguage::Lua => format!(r#"dispatch hl.dsp.exec_cmd("{cmd}")"#),
        };

        let response = request(&command)?;
        anyhow::ensure!(
            response.trim().starts_with("ok"),
            "Hyprland refused to launch '{cmd}': {}",
            response.trim()
        );

        Ok(())
    }

    fn is_running(&self) -> bool {
        instance_dir().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::Compositor as _;

    fn letter_bind() -> Bind {
        Bind {
            name: "switch:t".to_owned(),
            mods: vec!["SUPER".to_owned()],
            key: "t".to_owned(),
            event: "jaunt:switch:t".to_owned(),
            role: "the key for ghostty".to_owned(),
            flags: Flags::default(),
            ownership: Ownership::Exclusive,
            occupied: false,
        }
    }

    fn confirm_bind(occupied: bool) -> Bind {
        Bind {
            name: "confirm:Super_L".to_owned(),
            mods: vec!["SUPER".to_owned()],
            key: "Super_L".to_owned(),
            event: "jaunt:confirm".to_owned(),
            role: "confirming a switch".to_owned(),
            flags: Flags {
                release: true,
                transparent: true,
                non_consuming: true,
                ..Flags::default()
            },
            ownership: Ownership::Shared,
            occupied,
        }
    }

    /// A directory of instances, some of them still listening.
    fn instances(
        name: &str,
        listening: &[&str],
        gone: &[&str],
    ) -> (PathBuf, Vec<std::os::unix::net::UnixListener>) {
        let hypr = std::env::temp_dir().join(format!("jaunt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&hypr);

        let sockets = listening
            .iter()
            .map(|instance| {
                let directory = hypr.join(instance);
                std::fs::create_dir_all(&directory).expect("failed to make an instance");

                std::os::unix::net::UnixListener::bind(directory.join(".socket.sock"))
                    .expect("failed to listen")
            })
            .collect();

        for instance in gone {
            std::fs::create_dir_all(hypr.join(instance)).expect("failed to make an instance");
        }

        (hypr, sockets)
    }

    #[test]
    fn the_instance_named_by_the_environment_is_the_one_used() {
        let (hypr, _sockets) = instances("named", &["live", "other"], &[]);

        assert_eq!(resolve(&hypr, Some("live")).unwrap(), hypr.join("live"));
    }

    #[test]
    fn a_signature_left_over_from_a_session_that_has_gone_falls_back() {
        // A terminal opened before the session restarted still names the old
        // instance, whose directory is still there.
        let (hypr, _sockets) = instances("stale", &["live"], &["gone"]);

        assert_eq!(resolve(&hypr, Some("gone")).unwrap(), hypr.join("live"));
    }

    #[test]
    fn nothing_running_says_so() {
        let (hypr, _sockets) = instances("none", &[], &["gone"]);
        let error = resolve(&hypr, Some("gone")).expect_err("nothing is listening");

        assert!(error.to_string().contains("no running Hyprland"), "{error}");
    }

    #[test]
    fn several_running_and_no_way_to_tell_asks_which() {
        let (hypr, _sockets) = instances("several", &["one", "two"], &[]);
        let error = resolve(&hypr, Some("gone")).expect_err("two are listening");

        assert!(error.to_string().contains("more than one"), "{error}");
        assert!(error.to_string().contains("one, two"), "{error}");
    }

    #[test]
    fn a_letter_replaces_whatever_was_bound_to_it() {
        assert_eq!(
            letter_bind().add(ConfigLanguage::Legacy),
            [
                "keyword unbind SUPER,t",
                "keyword bind SUPER,t,event,jaunt:switch:t"
            ]
        );
        assert_eq!(
            letter_bind().remove(ConfigLanguage::Legacy),
            ["keyword unbind SUPER,t"]
        );
    }

    #[test]
    fn a_shared_key_is_never_unbound() {
        let commands = confirm_bind(false).add(ConfigLanguage::Legacy);

        assert_eq!(
            commands,
            ["keyword bindrtn SUPER,Super_L,event,jaunt:confirm"]
        );
        assert!(!commands.iter().any(|command| command.contains("unbind")));
    }

    #[test]
    fn a_shared_key_someone_else_uses_keeps_jaunts_bind_rather_than_taking_theirs() {
        assert_eq!(
            confirm_bind(true).remove(ConfigLanguage::Legacy),
            [] as [String; 0]
        );
        assert_eq!(
            confirm_bind(false).remove(ConfigLanguage::Legacy),
            ["keyword unbind SUPER,Super_L"]
        );
    }

    #[test]
    fn lua_keeps_hold_of_its_own_binds() {
        let add = confirm_bind(false).add(ConfigLanguage::Lua).join(" ");

        assert!(add.contains(r#"hl.bind("SUPER + Super_L""#), "{add}");
        assert!(add.contains("set_enabled(true)"), "{add}");
        assert!(!add.contains("unbind"), "{add}");

        // Disabling is what leaves the user's binding on the key alone.
        let remove = confirm_bind(false).remove(ConfigLanguage::Lua).join(" ");

        assert!(remove.contains("set_enabled(false)"), "{remove}");
        assert!(!remove.contains("unbind"), "{remove}");
    }

    #[test]
    fn lua_letters_are_replaced_and_removed_by_handle() {
        let add = letter_bind().add(ConfigLanguage::Lua).join(" ");

        assert!(add.contains(r#"hl.unbind("SUPER + t")"#), "{add}");
        assert!(add.contains("shared = false"), "{add}");

        let remove = letter_bind().remove(ConfigLanguage::Lua).join(" ");

        assert!(remove.contains("e.bind:remove()"), "{remove}");
        assert!(!remove.contains("hl.unbind"), "{remove}");
    }

    #[test]
    fn a_key_and_its_modifiers_are_read_back_the_way_they_were_written() {
        assert_eq!(
            parse_keys("SUPER + Super_L"),
            Some((64, "super_l".to_owned()))
        );
        assert_eq!(parse_keys("SUPER + SHIFT + t"), Some((65, "t".to_owned())));
        assert_eq!(parse_keys("Escape"), Some((0, "escape".to_owned())));
        assert_eq!(parse_keys(""), None);

        // Two binds are on the same key when they land in the same slot,
        // however each was spelled.
        assert_eq!(
            parse_keys("SUPER + SHIFT + t"),
            parse_keys("shift + super + T")
        );
    }

    #[test]
    fn flags_are_spelled_for_both_parsers() {
        let flags = Flags {
            release: true,
            transparent: true,
            non_consuming: true,
            ignore_mods: false,
        };

        assert_eq!(flags.letters(), "rtn");
        assert_eq!(
            flags.options(),
            ", { release = true, transparent = true, non_consuming = true }"
        );
        assert_eq!(Flags::default().letters(), "");
        assert_eq!(Flags::default().options(), "");
    }

    /// Checks the parsing against whatever Hyprland actually answers, which no
    /// fixture can keep up with. Ignored by default: it needs a running
    /// Hyprland, and only reads from it.
    ///
    /// Run it with `cargo test -- --ignored`.
    #[test]
    #[ignore = "needs a running Hyprland"]
    fn a_running_hyprland_can_be_read() {
        let windows = Compositor
            .get_windows()
            .expect("failed to read the windows");

        assert!(
            windows.iter().all(|window| !window.app_id.is_empty()),
            "every window should have an app_id: {windows:#?}"
        );

        Compositor
            .get_focused_window()
            .expect("failed to read the focused window");

        println!(
            "{} windows, config language: {:?}",
            windows.len(),
            config_language()
        );
    }
}
