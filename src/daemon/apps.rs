//! What the system says about the applications Jaunt switches between: what
//! each one is called, and what its icon is called.
//!
//! Read straight off the desktop entries rather than asked of anything, so
//! that the answer never waits on a service.

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// What a desktop entry says about an application.
#[derive(Debug, Default)]
pub(crate) struct Entry {
    /// The entry's own file name, without `.desktop`.
    entry: String,
    /// The command it runs, without its path.
    runs: String,
    /// The window class it says its windows carry. This is the field made for
    /// recognising an application's windows, and the one worth trusting: Zen's
    /// entry is `zen-beta`, Beeper's is `beepertexts`, and neither is what
    /// their windows are called.
    class: String,
    name: String,
    /// The icon theme's name for it, which is again its own: Beeper's windows
    /// say `Beeper` and its icon is `beepertexts`.
    icon: String,
}

/// The desktop entries, and the names the user would rather give some of the
/// applications they describe.
pub(crate) struct Apps {
    /// What to call each application, by `app_id`, for the ones the user would
    /// rather name themselves.
    names: BTreeMap<String, String>,
    /// Every desktop entry on the system.
    ///
    /// Read once and kept: walking every entry on the system is far too much
    /// to do while a switch is waiting to appear.
    entries: OnceCell<Vec<Entry>>,
}

impl Apps {
    pub(crate) fn new(names: BTreeMap<String, String>) -> Self {
        Self {
            names,
            entries: OnceCell::new(),
        }
    }

    /// Takes on the names from a configuration that changed.
    pub(crate) fn rename(&mut self, names: BTreeMap<String, String>) {
        self.names = names;
    }

    /// The desktop entry for an application, if one of them is plainly about
    /// it.
    ///
    /// The window class it declares comes first, then its own file name, then
    /// the command it runs: a system can carry several entries running one
    /// command, and the extras tend to say less about the application.
    fn entry(&self, app_id: &str, cmd: Option<&str>) -> Option<&Entry> {
        let entries = self.entries.get_or_init(desktop_entries);
        let wanted: Vec<String> = std::iter::once(app_id)
            .chain(cmd)
            .map(str::to_lowercase)
            .collect();
        let matches = |field: &String| !field.is_empty() && wanted.contains(field);

        entries
            .iter()
            .find(|entry| matches(&entry.class))
            .or_else(|| entries.iter().find(|entry| matches(&entry.entry)))
            .or_else(|| entries.iter().find(|entry| matches(&entry.runs)))
    }

    /// What to call an application on screen.
    ///
    /// What the user called it wins. Then what its desktop entry calls it,
    /// which is the application's own name rather than anything inferred from
    /// it — `Files` rather than `org.gnome.Nautilus`, and `Gram` rather than
    /// whichever document its first window happens to have open.
    ///
    /// `otherwise` is the last resort, for an application no entry describes:
    /// the name its windows opened under when it is running, and the command
    /// that would start it when it isn't.
    pub(crate) fn label(&self, app_id: &str, cmd: Option<&str>, otherwise: &str) -> String {
        if let Some(name) = self.names.get(app_id) {
            return name.clone();
        }

        self.entry(app_id, cmd)
            .map_or_else(|| otherwise.to_owned(), |entry| entry.name.clone())
    }

    /// The names an application's icon might go by, best guess first.
    ///
    /// Which of them the icon theme actually has is for whatever draws the
    /// icon to find out: it is the one holding the theme.
    pub(crate) fn icons(&self, app_id: &str, cmd: Option<&str>) -> Vec<String> {
        // What the desktop entry names first: it knows, where the window class
        // is only a guess that happens to be right most of the time.
        let named = self
            .entry(app_id, cmd)
            .map(|entry| entry.icon.clone())
            .filter(|icon| !icon.is_empty());

        let mut icons: Vec<String> = named
            .into_iter()
            .chain(guesses(app_id))
            .chain(cmd.into_iter().flat_map(guesses))
            .collect();

        let mut seen = std::collections::HashSet::new();
        icons.retain(|icon| seen.insert(icon.clone()));

        icons
    }
}

/// The icon names a window class or a command suggests: itself, and the last
/// part of a reverse-domain name like `com.mitchellh.ghostty`.
fn guesses(name: &str) -> [String; 4] {
    let last = name.rsplit('.').next().unwrap_or(name);

    [
        name.to_owned(),
        name.to_lowercase(),
        last.to_owned(),
        last.to_lowercase(),
    ]
}

/// Every desktop entry on the system: what the entry itself is called, the
/// command it runs, and the name it gives the application.
fn desktop_entries() -> Vec<Entry> {
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    let shared =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());

    let roots = home
        .into_iter()
        .chain(shared.split(':').map(PathBuf::from))
        .map(|root| root.join("applications"));

    let mut entries = Vec::new();

    for root in roots {
        let Ok(files) = std::fs::read_dir(root) else {
            continue;
        };

        for file in files.flatten() {
            let path = file.path();

            if path.extension().and_then(|end| end.to_str()) != Some("desktop") {
                continue;
            }

            let (Some(entry), Ok(text)) = (
                path.file_stem().and_then(|stem| stem.to_str()),
                std::fs::read_to_string(&path),
            ) else {
                continue;
            };

            if let Some(mut described) = describes(&text) {
                described.entry = entry.to_lowercase();
                entries.push(described);
            }
        }
    }

    entries
}

/// What a desktop entry says, as far as the switcher cares.
fn describes(text: &str) -> Option<Entry> {
    let mut entry = Entry::default();
    let mut started = false;

    for line in text.lines() {
        // Entries carry a section per language and per action; only the first
        // one describes the application itself.
        if line.starts_with('[') {
            if started {
                break;
            }

            started = true;
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();

        match key {
            "Name" if entry.name.is_empty() => entry.name = value.to_owned(),
            "Icon" if entry.icon.is_empty() => entry.icon = value.to_owned(),
            "StartupWMClass" if entry.class.is_empty() => entry.class = value.to_lowercase(),
            "Exec" if entry.runs.is_empty() => {
                entry.runs = value
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_lowercase();
            }
            _ => {}
        }
    }

    (!entry.name.is_empty()).then_some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEEPER: &str = "\
[Desktop Entry]
Name=Beeper
Exec=/nix/store/abc-beeper/bin/beepertexts --no-sandbox %U
Icon=beepertexts
StartupWMClass=Beeper

[Desktop Action new-window]
Name=New Window
Exec=beepertexts --new-window
";

    fn apps(entries: Vec<Entry>, names: &[(&str, &str)]) -> Apps {
        let apps = Apps::new(
            names
                .iter()
                .map(|(app_id, name)| ((*app_id).to_owned(), (*name).to_owned()))
                .collect(),
        );
        let _ = apps.entries.set(entries);

        apps
    }

    fn beeper() -> Entry {
        let mut entry = describes(BEEPER).expect("a complete entry");
        entry.entry = "beepertexts".to_owned();
        entry
    }

    #[test]
    fn an_entry_is_read_from_its_first_section_only() {
        let entry = describes(BEEPER).expect("a complete entry");

        assert_eq!(entry.name, "Beeper");
        assert_eq!(entry.icon, "beepertexts");
        assert_eq!(entry.class, "beeper");
        assert_eq!(entry.runs, "beepertexts");
    }

    #[test]
    fn an_entry_without_a_name_describes_nothing() {
        assert!(describes("[Desktop Entry]\nExec=thing\n").is_none());
    }

    #[test]
    fn the_users_name_wins_then_the_entry_then_the_fallback() {
        let named = apps(vec![beeper()], &[("Beeper", "Chat")]);
        let unnamed = apps(vec![beeper()], &[]);

        assert_eq!(named.label("Beeper", None, "beeper"), "Chat");
        assert_eq!(unnamed.label("Beeper", None, "beeper"), "Beeper");
        assert_eq!(unnamed.label("ghostty", None, "Ghostty"), "Ghostty");
    }

    #[test]
    fn an_entry_is_found_by_the_command_it_runs() {
        let apps = apps(vec![beeper()], &[]);

        assert_eq!(apps.label("unknown", Some("beepertexts"), "x"), "Beeper");
    }

    #[test]
    fn icons_start_with_the_entrys_own_and_hold_no_repeats() {
        let apps = apps(vec![beeper()], &[]);

        assert_eq!(
            apps.icons("Beeper", None),
            ["beepertexts", "Beeper", "beeper"]
        );
        assert_eq!(
            apps.icons("com.mitchellh.ghostty", Some("ghostty")),
            ["com.mitchellh.ghostty", "ghostty"]
        );
    }
}
