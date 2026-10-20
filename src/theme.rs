//! Themes: the six colours the switcher is drawn in, from which it works out
//! every other.
//!
//! A theme is a file of colours, picked by name: one of the user's own from
//! `~/.config/jaunt/themes`, or one of those Jaunt comes with. A colour a
//! theme leaves out is the default theme's.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Each theme by name, with the text of `themes/<name>.toml`.
///
/// A macro rather than a function, because `include_str!` needs its path
/// before any function runs.
macro_rules! themes {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("../themes/", $name, ".toml")))),*]
    };
}

/// The theme a colour falls back to, and what `theme` means when the
/// configuration doesn't say.
const DEFAULT: &str = "default";

/// The themes Jaunt comes with, by name.
const BUILT_IN: &[(&str, &str)] = themes![
    // keep-sorted start
    "catppuccin-frappe",
    "catppuccin-latte",
    "catppuccin-macchiato",
    "catppuccin-mocha",
    "default",
    "light",
    "nord",
    "one-dark",
    "tokyo-night",
    // keep-sorted end
];

/// The six colours the switcher is drawn in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Palette {
    /// The panel everything lies on.
    background: Colour,
    /// The sheet the windows lie on, and the tab joining it to their
    /// application.
    surface: Colour,
    /// The window a switch would land on.
    accent: Colour,
    /// Window titles, and the keys.
    text: Colour,
    /// The application in the heading, the selected window's title and its
    /// dot.
    bright: Colour,
    /// The window in the heading, what the keys do, and the other windows'
    /// dots.
    muted: Colour,
}

impl Default for Palette {
    fn default() -> Self {
        let (_, text) = BUILT_IN
            .iter()
            .find(|(name, _)| *name == DEFAULT)
            .expect("Jaunt ships a theme called 'default'");

        toml::from_str(text).expect("Jaunt's default theme sets every colour")
    }
}

/// A theme as its file writes it, which needn't set every colour.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Theme {
    background: Option<Colour>,
    surface: Option<Colour>,
    accent: Option<Colour>,
    text: Option<Colour>,
    bright: Option<Colour>,
    muted: Option<Colour>,
}

impl Theme {
    /// The colours this theme sets, and `base`'s where it doesn't.
    fn over(self, base: Palette) -> Palette {
        Palette {
            background: self.background.unwrap_or(base.background),
            surface: self.surface.unwrap_or(base.surface),
            accent: self.accent.unwrap_or(base.accent),
            text: self.text.unwrap_or(base.text),
            bright: self.bright.unwrap_or(base.bright),
            muted: self.muted.unwrap_or(base.muted),
        }
    }
}

/// A colour, written `#rrggbb`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub(crate) struct Colour(String);

impl TryFrom<String> for Colour {
    type Error = String;

    fn try_from(colour: String) -> Result<Self, Self::Error> {
        let digits = colour.strip_prefix('#').unwrap_or_default();

        if digits.len() != 6 || !digits.chars().all(|digit| digit.is_ascii_hexdigit()) {
            return Err(format!(
                "'{colour}' isn't a colour: write it as #rrggbb, like #6b8cff"
            ));
        }

        Ok(Self(colour.to_lowercase()))
    }
}

/// The colours of the theme called `name`: the user's own by that name, from
/// `directory`, or else the one Jaunt comes with. A name with a `/` in it is
/// the path to a theme file instead.
///
/// # Errors
///
/// Returns an error if there's no theme by that name, or if its file can't be
/// read or isn't a theme.
pub(crate) fn load(name: &str, directory: Option<&Path>) -> Result<Palette> {
    let (text, source) = find(name, directory)?;
    let theme: Theme = toml::from_str(&text).with_context(|| format!("{source} isn't valid"))?;

    Ok(theme.over(Palette::default()))
}

/// The text of the theme called `name`, and what to call where it came from.
fn find(name: &str, directory: Option<&Path>) -> Result<(String, String)> {
    let path = if name.contains('/') {
        Some(PathBuf::from(name))
    } else {
        directory.map(|directory| directory.join(format!("{name}.toml")))
    };

    if let Some(path) = &path {
        match std::fs::read_to_string(path) {
            Ok(text) => return Ok((text, path.display().to_string())),
            // Not one of the user's own, but it may be one Jaunt comes with.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !name.contains('/') => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read the theme {}", path.display()));
            }
        }
    }

    let (_, text) = BUILT_IN
        .iter()
        .find(|(built_in, _)| *built_in == name)
        .with_context(|| {
            let built_in: Vec<_> = BUILT_IN.iter().map(|(name, _)| *name).collect();
            let looked = path.map_or_else(String::new, |path| {
                format!("there's no {}, and ", path.display())
            });

            format!(
                "there's no theme called '{name}': {looked}Jaunt's own are {}",
                built_in.join(", ")
            )
        })?;

    Ok(((*text).to_owned(), format!("jaunt's {name} theme")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of themes for one test, emptied first.
    fn themes(test: &str, files: &[(&str, &str)]) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("jaunt-themes-{test}"));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("failed to make a themes directory");

        for (name, text) in files {
            std::fs::write(directory.join(name), text).expect("failed to write a theme");
        }

        directory
    }

    fn colour(colour: &str) -> Colour {
        colour.to_owned().try_into().expect("a colour")
    }

    #[test]
    fn the_themes_jaunt_comes_with_set_every_colour() {
        for (name, text) in BUILT_IN {
            toml::from_str::<Palette>(text).unwrap_or_else(|error| panic!("{name}: {error}"));
        }
    }

    #[test]
    fn a_theme_is_found_by_name_without_any_of_the_users_own() {
        assert_eq!(load("default", None).unwrap(), Palette::default());

        let light = load("light", None).unwrap();
        assert_ne!(light, Palette::default());
        assert_eq!(load("light", Some(&themes("none", &[]))).unwrap(), light);
    }

    #[test]
    fn the_users_own_theme_comes_first_and_falls_back_colour_by_colour() {
        let directory = themes("own", &[("light.toml", "accent = \"#FF0000\"")]);
        let palette = load("light", Some(&directory)).unwrap();

        assert_eq!(palette.accent, colour("#ff0000"));
        // The rest is the default theme's, not the built-in light theme's.
        assert_eq!(palette.background, Palette::default().background);
    }

    #[test]
    fn a_theme_may_be_a_path() {
        let directory = themes("path", &[("mine.toml", "muted = \"#123456\"")]);
        let path = directory.join("mine.toml");

        let palette = load(path.to_str().unwrap(), None).unwrap();
        assert_eq!(palette.muted, colour("#123456"));

        let error = load(directory.join("missing.toml").to_str().unwrap(), None)
            .expect_err("a path that isn't there is an error");
        assert!(format!("{error:#}").contains("missing.toml"), "{error:#}");
    }

    #[test]
    fn a_theme_that_isnt_there_says_where_jaunt_looked() {
        let directory = themes("missing", &[]);
        let error = load("dracula", Some(&directory)).expect_err("there's no dracula");
        let error = format!("{error:#}");

        assert!(error.contains("dracula.toml"), "{error}");

        // However the list is ordered, it names the ones there are.
        for name in ["default", "light", "catppuccin-mocha", "nord"] {
            assert!(error.contains(name), "{name} is missing from: {error}");
        }
    }

    #[test]
    fn a_typo_in_a_theme_is_an_error() {
        let directory = themes(
            "typo",
            &[
                ("key.toml", "backgorund = \"#000000\""),
                ("colour.toml", "background = \"black\""),
                ("short.toml", "background = \"#000\""),
            ],
        );

        let error = format!("{:#}", load("key", Some(&directory)).unwrap_err());
        assert!(error.contains("backgorund"), "{error}");

        let error = format!("{:#}", load("colour", Some(&directory)).unwrap_err());
        assert!(error.contains("#rrggbb"), "{error}");

        assert!(load("short", Some(&directory)).is_err());
    }
}
