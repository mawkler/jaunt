//! The switcher's pure logic: how windows are grouped, which one starts out
//! highlighted, and how cycling moves the highlight.
//!
//! Nothing here talks to a compositor or to GTK, so every rule the
//! specification pins down can be exercised by `cargo test`.

use std::collections::BTreeMap;

use crate::compositor::Window;

/// Every open window, keyed by lowercased `app_id`.
///
/// A `BTreeMap` keeps the groups sorted alphabetically, which is the order they
/// are listed in. Windows inside a group keep the order the compositor returned
/// them in: most recently used first.
pub(crate) type Groups = BTreeMap<String, Vec<Window>>;

/// Groups `windows` by their `app_id`, preserving their relative order.
pub(crate) fn group_windows(windows: Vec<Window>) -> Groups {
    windows
        .into_iter()
        .fold(Groups::new(), |mut groups, window| {
            groups
                .entry(window.app_id.to_lowercase())
                .or_default()
                .push(window);
            groups
        })
}

/// Finds the group `search` refers to: an exact `app_id` match if there is one,
/// otherwise the first group whose name contains `search`.
pub(crate) fn find_group<'a>(groups: &'a Groups, search: &str) -> Option<&'a str> {
    let search = search.to_lowercase();

    groups
        .keys()
        .find(|name| *name == &search)
        .or_else(|| groups.keys().find(|name| name.contains(&search)))
        .map(String::as_str)
}

/// Which way through a group the highlight moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Forward,
    Backward,
}

/// The name a group's windows opened under, for an application whose desktop
/// entry the switcher couldn't find.
///
/// Applications put their own name in a window's title before they have a
/// document to name instead, so this is usually the application's own name —
/// but only usually: a window that was still loading may have said something
/// useless, and two windows of one application may disagree. The name most of
/// them opened under wins, and the `app_id` is the last resort.
fn group_name<'a>(app_id: &'a str, windows: &'a [Window]) -> &'a str {
    let names = windows
        .iter()
        .map(|window| window.initial_title.as_str())
        .filter(|name| !name.is_empty());

    let mut best: Option<(&str, usize)> = None;

    for name in names.clone() {
        let votes = names.clone().filter(|other| *other == name).count();

        // Windows come most recently used first, so an earlier one keeps the
        // name when the vote is tied.
        if best.is_none_or(|(_, most)| votes > most) {
            best = Some((name, votes));
        }
    }

    best.map_or(app_id, |(name, _)| name)
}

/// One line of the switcher's list.
#[derive(Debug, PartialEq)]
pub(crate) enum Row<'a> {
    /// A group: the `app_id` everything about it is keyed by, and what to call
    /// it on screen.
    Group { app_id: &'a str, name: &'a str },
    /// A window, and whether it's the one that would be focused right now.
    Window { window: &'a Window, selected: bool },
}

/// An in-progress switch: all open windows, and the one currently highlighted.
#[derive(Debug)]
pub(crate) struct Session {
    groups: Groups,
    group: String,
    index: usize,
    label: String,
}

impl Session {
    /// Starts a session highlighting a window of `group`.
    ///
    /// `focused` is the window the compositor had focused when the session
    /// started, if any. `label` is the application as the user asked for it,
    /// e.g. `brave`, which reads better than the window class does.
    /// `direction` is the way the user asked to go through the group.
    ///
    /// # Panics
    ///
    /// Panics if `group` isn't a non-empty group of `groups`.
    pub(crate) fn new(
        groups: Groups,
        group: &str,
        focused: Option<&Window>,
        label: &str,
        direction: Direction,
    ) -> Self {
        let index = initial_index(&groups[group], focused, direction);

        Self {
            groups,
            group: group.to_owned(),
            index,
            label: label.to_owned(),
        }
    }

    /// The group being switched within, e.g. `"brave-browser"`.
    pub(crate) fn group(&self) -> &str {
        &self.group
    }

    /// The application being switched to, as the user named it.
    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    /// The window that gets focused if the user confirms right now.
    pub(crate) fn selected_window(&self) -> &Window {
        &self.groups[&self.group][self.index]
    }

    /// Moves the highlight one window along the group, wrapping around.
    pub(crate) fn cycle(&mut self, direction: Direction) {
        let windows = &self.groups[&self.group];
        self.index = step(self.index, windows.len(), direction);
    }

    /// Points the session at another application's group. Cycles instead if
    /// it's the group already shown.
    ///
    /// Coming back to the group of the window that was focused highlights
    /// that window: having looked elsewhere, the way back is to where you
    /// were. Any other group gets the pre-selection rule.
    pub(crate) fn switch_to_group(
        &mut self,
        group: &str,
        focused: Option<&Window>,
        label: &str,
        direction: Direction,
    ) {
        if group == self.group {
            self.cycle(direction);
            return;
        }

        let windows = &self.groups[group];
        let focused_index = focused.and_then(|focused| windows.iter().position(|w| w == focused));

        self.index = focused_index.unwrap_or_else(|| initial_index(windows, focused, direction));
        self.group = group.to_owned();
        self.label = label.to_owned();
    }

    /// Takes the highlighted window out of the session, for a window that is
    /// being closed. The highlight stays where it was, on the window after
    /// it, or on the last when it was last.
    ///
    /// A group left without windows goes, and the session moves to the group
    /// after it in `order`, the groups as they are shown, or else the one
    /// before it. Returns `false` when there is no group left to move to.
    pub(crate) fn remove_selected(&mut self, order: &[&str]) -> bool {
        let Some(windows) = self.groups.get_mut(&self.group) else {
            return false;
        };

        windows.remove(self.index);

        if !windows.is_empty() {
            self.index = self.index.min(windows.len() - 1);
            return true;
        }

        self.groups.remove(&self.group);

        let remains = |group: &&&str| self.groups.contains_key(**group);
        let neighbour = match order.iter().position(|group| *group == self.group) {
            Some(at) => order[at + 1..]
                .iter()
                .find(remains)
                .or_else(|| order[..at].iter().rev().find(remains))
                .map(|group| (*group).to_owned()),
            None => None,
        };
        let Some(group) = neighbour.or_else(|| self.groups.keys().next().cloned()) else {
            return false;
        };

        self.label = group_name(&group, &self.groups[&group]).to_owned();
        self.group = group;
        self.index = 0;

        true
    }

    /// Looks up the group `search` refers to, among this session's windows.
    pub(crate) fn find_group(&self, search: &str) -> Option<&str> {
        find_group(&self.groups, search)
    }

    /// Every line to display, in order: each group's label followed by its
    /// windows.
    pub(crate) fn rows(&self) -> impl Iterator<Item = Row<'_>> {
        self.groups.iter().flat_map(move |(app_id, windows)| {
            let group = std::iter::once(Row::Group {
                app_id,
                name: group_name(app_id, windows),
            });
            let windows = windows
                .iter()
                .enumerate()
                .map(move |(index, window)| Row::Window {
                    window,
                    selected: app_id == &self.group && index == self.index,
                });

            group.chain(windows)
        })
    }
}

/// Which window of `windows` starts out highlighted.
///
/// Moving off the focused window takes precedence: if it's one of `windows`,
/// and there's somewhere else to go, its neighbour in `direction` is
/// highlighted. Otherwise going forwards starts at the group's most recently
/// used window and going backwards at its least recently used one, the way
/// Alt-Tab and Alt-Shift-Tab start at opposite ends of the list.
fn initial_index(windows: &[Window], focused: Option<&Window>, direction: Direction) -> usize {
    if windows.len() < 2 {
        return 0;
    }

    let focused_index = focused.and_then(|focused| windows.iter().position(|w| w == focused));

    match (focused_index, direction) {
        (Some(index), direction) => step(index, windows.len(), direction),
        (None, Direction::Forward) => 0,
        (None, Direction::Backward) => windows.len() - 1,
    }
}

/// One step along a list of `length` windows, wrapping at either end.
fn step(index: usize, length: usize, direction: Direction) -> usize {
    match direction {
        Direction::Forward => (index + 1) % length,
        Direction::Backward => (index + length - 1) % length,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: &str, app_id: &str, title: &str) -> Window {
        Window {
            id: id.to_owned(),
            app_id: app_id.to_owned(),
            title: title.to_owned(),
            size: None,
            initial_title: String::new(),
        }
    }

    /// Two ghostty windows and one brave window, most recently used first.
    fn windows() -> Vec<Window> {
        vec![
            window("1", "com.mitchellh.ghostty", "ghostty: jaunt"),
            window("2", "brave-browser", "Hyprland Wiki"),
            window("3", "com.mitchellh.ghostty", "ghostty: notes"),
        ]
    }

    fn session(focused: Option<&Window>) -> Session {
        let groups = group_windows(windows());
        Session::new(
            groups,
            "com.mitchellh.ghostty",
            focused,
            "ghostty",
            Direction::Forward,
        )
    }

    #[test]
    fn groups_are_alphabetical_and_keep_windows_in_most_recently_used_order() {
        let groups = group_windows(windows());

        assert_eq!(
            groups.keys().collect::<Vec<_>>(),
            ["brave-browser", "com.mitchellh.ghostty"]
        );
        assert_eq!(
            groups["com.mitchellh.ghostty"]
                .iter()
                .map(|w| w.id.as_str())
                .collect::<Vec<_>>(),
            ["1", "3"]
        );
    }

    #[test]
    fn group_is_found_by_exact_app_id_before_substring() {
        let groups = group_windows(vec![
            window("1", "brave-browser", "Hyprland Wiki"),
            window("2", "brave", "Brave, the other one"),
        ]);

        assert_eq!(find_group(&groups, "brave"), Some("brave"));
        assert_eq!(find_group(&groups, "ghostty"), None);
    }

    #[test]
    fn group_is_found_by_substring_and_ignores_case() {
        let groups = group_windows(windows());

        assert_eq!(
            find_group(&groups, "ghostty"),
            Some("com.mitchellh.ghostty")
        );
        assert_eq!(
            find_group(&groups, "GHOSTTY"),
            Some("com.mitchellh.ghostty")
        );
    }

    #[test]
    fn most_recently_used_window_starts_highlighted() {
        let brave = window("2", "brave-browser", "Hyprland Wiki");

        assert_eq!(session(Some(&brave)).selected_window().id, "1");
        assert_eq!(session(None).selected_window().id, "1");
    }

    #[test]
    fn a_focused_window_hands_over_to_the_next_one() {
        let focused = window("1", "com.mitchellh.ghostty", "ghostty: jaunt");

        assert_eq!(session(Some(&focused)).selected_window().id, "3");
    }

    #[test]
    fn the_only_window_of_a_group_stays_highlighted() {
        let groups = group_windows(windows());
        let focused = window("2", "brave-browser", "Hyprland Wiki");
        let mut session = Session::new(
            groups,
            "brave-browser",
            Some(&focused),
            "brave",
            Direction::Forward,
        );

        assert_eq!(session.selected_window().id, "2");

        session.cycle(Direction::Forward);

        assert_eq!(session.selected_window().id, "2");
    }

    #[test]
    fn cycling_wraps_from_the_last_window_back_to_the_first() {
        let mut session = session(None);

        assert_eq!(session.selected_window().id, "1");

        session.cycle(Direction::Forward);
        assert_eq!(session.selected_window().id, "3");

        session.cycle(Direction::Forward);
        assert_eq!(session.selected_window().id, "1");
    }

    #[test]
    fn switching_to_another_group_re_runs_the_pre_selection_rule() {
        let focused = window("2", "brave-browser", "Hyprland Wiki");
        let mut session = session(Some(&focused));

        assert_eq!(session.selected_window().id, "1");

        session.switch_to_group("brave-browser", None, "brave", Direction::Forward);
        session.switch_to_group(
            "com.mitchellh.ghostty",
            None,
            "ghostty",
            Direction::Backward,
        );

        assert_eq!(session.selected_window().id, "3");
    }

    #[test]
    fn switching_back_to_the_focused_group_highlights_the_focused_window() {
        let focused = window("1", "com.mitchellh.ghostty", "ghostty: jaunt");
        let mut session = session(Some(&focused));

        assert_eq!(session.selected_window().id, "3");

        session.switch_to_group("brave-browser", Some(&focused), "brave", Direction::Forward);

        assert_eq!(session.group(), "brave-browser");
        assert_eq!(session.selected_window().id, "2");

        for direction in [Direction::Forward, Direction::Backward] {
            session.switch_to_group(
                "com.mitchellh.ghostty",
                Some(&focused),
                "ghostty",
                direction,
            );
            assert_eq!(session.selected_window().id, "1");

            session.switch_to_group("brave-browser", Some(&focused), "brave", direction);
        }
    }

    #[test]
    fn switching_to_the_group_already_shown_cycles_instead() {
        let mut session = session(None);

        session.switch_to_group("com.mitchellh.ghostty", None, "ghostty", Direction::Forward);

        assert_eq!(session.selected_window().id, "3");
    }

    #[test]
    fn going_backwards_starts_at_the_least_recently_used_window() {
        let groups = group_windows(windows());
        let session = Session::new(
            groups,
            "com.mitchellh.ghostty",
            None,
            "ghostty",
            Direction::Backward,
        );

        assert_eq!(session.selected_window().id, "3");
    }

    #[test]
    fn going_backwards_from_a_focused_window_picks_the_one_before_it() {
        let focused = window("1", "com.mitchellh.ghostty", "ghostty: jaunt");
        let groups = group_windows(windows());
        let session = Session::new(
            groups,
            "com.mitchellh.ghostty",
            Some(&focused),
            "ghostty",
            Direction::Backward,
        );

        assert_eq!(session.selected_window().id, "3");
    }

    #[test]
    fn cycling_backwards_wraps_the_other_way() {
        let mut session = session(None);

        assert_eq!(session.selected_window().id, "1");

        session.cycle(Direction::Backward);
        assert_eq!(session.selected_window().id, "3");

        session.cycle(Direction::Backward);
        assert_eq!(session.selected_window().id, "1");
    }

    #[test]
    fn closing_a_window_highlights_the_one_after_it() {
        let mut session = session(None);
        let order = ["com.mitchellh.ghostty", "brave-browser"];

        assert!(session.remove_selected(&order));
        assert_eq!(session.selected_window().id, "3");
        assert_eq!(session.group(), "com.mitchellh.ghostty");
    }

    #[test]
    fn closing_a_groups_last_window_moves_to_the_group_after_it_or_else_before_it() {
        let groups = group_windows(vec![
            window("1", "a", "a"),
            window("2", "b", "b"),
            window("3", "c", "c"),
        ]);
        let order = ["a", "b", "c"];
        let mut session = Session::new(groups, "b", None, "b", Direction::Forward);

        assert!(session.remove_selected(&order));
        assert_eq!(session.group(), "c");

        assert!(session.remove_selected(&order));
        assert_eq!(session.group(), "a");

        assert!(!session.remove_selected(&order));
    }

    #[test]
    fn a_group_is_named_by_what_most_of_its_windows_opened_as() {
        let named = |initial_title: &str| Window {
            initial_title: initial_title.to_owned(),
            ..window("1", "com.mitchellh.ghostty", "a document")
        };

        // What most of them opened as wins, however they are titled now.
        assert_eq!(
            group_name(
                "com.mitchellh.ghostty",
                &[named("~/notes"), named("Ghostty"), named("Ghostty")]
            ),
            "Ghostty"
        );

        // A tie goes to the most recently used window, which comes first.
        assert_eq!(
            group_name("brave-browser", &[named("Brave"), named("New Tab")]),
            "Brave"
        );

        // A window that never said falls back to the app_id.
        assert_eq!(group_name("neovide", &[named("")]), "neovide");
    }

    #[test]
    fn rows_list_every_window_under_its_group() {
        let session = session(None);
        let rows: Vec<_> = session.rows().collect();

        assert_eq!(
            rows,
            [
                Row::Group {
                    app_id: "brave-browser",
                    name: "brave-browser"
                },
                Row::Window {
                    window: &windows()[1],
                    selected: false
                },
                Row::Group {
                    app_id: "com.mitchellh.ghostty",
                    name: "com.mitchellh.ghostty"
                },
                Row::Window {
                    window: &windows()[0],
                    selected: true
                },
                Row::Window {
                    window: &windows()[2],
                    selected: false
                },
            ]
        );
    }
}
