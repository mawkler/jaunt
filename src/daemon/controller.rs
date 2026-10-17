//! The switch in progress, as a state machine.
//!
//! The daemon feeds it events as they arrive and carries out the effects it
//! asks for. Keeping it free of GTK and of the compositor is what lets the
//! specification's timing rules be tested: that a fast tap never reveals the
//! overlay, and that confirming never waits for the overlay to be drawn, are
//! properties of this type, not of how fast a machine happens to draw.

use crate::compositor::Window;
use crate::config::Target;
use crate::switcher::{self, Direction, Session};

/// Everything that can happen to a switch in progress.
#[derive(Debug)]
pub(crate) enum Event {
    /// A mapped key was pressed while Super is held down.
    Trigger {
        target: Target,
        direction: Direction,
        windows: Vec<Window>,
        /// Boxed because it is the one big thing an event carries, and every
        /// other event would otherwise be as large as this one.
        focused: Option<Box<Window>>,
    },
    /// A key that steers a switch already in progress was pressed.
    Cycle { direction: Direction },
    /// Super was released.
    Confirm,
    /// Escape was pressed.
    Cancel,
    /// A window's own number key was pressed. `confirm` switches to it there
    /// and then, rather than only highlighting it.
    Pick { index: usize, confirm: bool },
    /// The key that closes the highlighted window was pressed.
    ///
    /// `order` is the applications as the switcher shows them, by group,
    /// which decides where the switch goes when it closes an application's
    /// last window.
    Close { order: Vec<String> },
    /// The delay before revealing the overlay elapsed.
    Reveal { session: u64 },
}

/// What the daemon should do about an [`Event`].
#[derive(Debug, PartialEq)]
pub(crate) enum Effect {
    /// Start the timer that reveals the overlay, tagged with the session it
    /// belongs to so a timer left over from an earlier switch is ignored.
    ScheduleReveal { session: u64 },
    /// Fill the overlay with the current session's windows.
    Fill,
    /// Move the overlay's highlight to the selected window.
    Highlight,
    /// Put the overlay on screen.
    Show,
    /// Take the overlay off screen.
    Hide,
    /// Focus the window the user settled on.
    Focus(Window),
    /// Ask a window to close.
    Close(Window),
    /// Launch an application that has no windows open.
    ///
    /// The whole target, not just its command: what is shown while it starts
    /// is its icon, and that is found from the window class rather than from
    /// the command.
    Launch(Target),
}

/// What pressing a mapped key did to a switch already in progress.
enum Retarget {
    /// The switch now points at a group, which may be the one it already
    /// pointed at.
    Group { same_group: bool },
    /// The application has no open windows.
    NoWindows,
}

/// A switch in progress, or the absence of one.
#[derive(Default)]
pub(crate) struct Controller {
    session: Option<Session>,
    /// Counts sessions, so a reveal timer can tell whether it's still wanted.
    sessions: u64,
    shown: bool,
}

impl Controller {
    pub(crate) fn handle(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::Trigger {
                target,
                direction,
                windows,
                focused,
            } => self.trigger(&target, direction, windows, focused.as_deref()),
            Event::Cycle { direction } => self.cycle(direction),
            Event::Confirm => self.confirm(),
            Event::Cancel => self.end(),
            Event::Pick { index, confirm } => self.pick(index, confirm),
            Event::Close { order } => self.close(&order),
            Event::Reveal { session } => self.reveal(session),
        }
    }

    /// The switch in progress, which is what the overlay displays.
    pub(crate) fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    fn trigger(
        &mut self,
        target: &Target,
        direction: Direction,
        windows: Vec<Window>,
        focused: Option<&Window>,
    ) -> Vec<Effect> {
        if self.session.is_none() {
            return self.start(target, direction, windows, focused);
        }

        let session = self
            .session
            .as_mut()
            .expect("a session is in progress, checked above");

        let retarget = match session.find_group(target.search()).map(str::to_owned) {
            Some(group) => {
                let same_group = group == session.group();
                session.switch_to_group(&group, focused, &target.app, direction);
                Retarget::Group { same_group }
            }
            None => Retarget::NoWindows,
        };

        match retarget {
            // The application has nothing to switch to, so it's launched
            // instead — and that ends the switch.
            Retarget::NoWindows => {
                let mut effects = self.end();
                effects.push(Effect::Launch(target.clone()));
                effects
            }
            // The selection moved, but there's nothing on screen to update.
            Retarget::Group { .. } if !self.shown => vec![],
            Retarget::Group { same_group: true } => vec![Effect::Highlight],
            Retarget::Group { same_group: false } => vec![Effect::Fill, Effect::Highlight],
        }
    }

    fn start(
        &mut self,
        target: &Target,
        direction: Direction,
        windows: Vec<Window>,
        focused: Option<&Window>,
    ) -> Vec<Effect> {
        let groups = switcher::group_windows(windows);

        let Some(group) = switcher::find_group(&groups, target.search()).map(str::to_owned) else {
            return vec![Effect::Launch(target.clone())];
        };

        self.sessions += 1;
        self.session = Some(Session::new(
            groups,
            &group,
            focused,
            &target.app,
            direction,
        ));

        vec![Effect::ScheduleReveal {
            session: self.sessions,
        }]
    }

    /// Moves the highlight without changing application, for the keys that
    /// only exist while a switch is in progress.
    fn cycle(&mut self, direction: Direction) -> Vec<Effect> {
        let Some(session) = &mut self.session else {
            return vec![];
        };

        session.cycle(direction);

        if self.shown {
            vec![Effect::Highlight]
        } else {
            vec![]
        }
    }

    fn reveal(&mut self, session: u64) -> Vec<Effect> {
        let stale = session != self.sessions;

        if stale || self.session.is_none() || self.shown {
            return vec![];
        }

        self.shown = true;

        vec![Effect::Fill, Effect::Highlight, Effect::Show]
    }

    fn confirm(&mut self) -> Vec<Effect> {
        let Some(session) = &self.session else {
            return vec![];
        };

        let window = session.selected_window().clone();
        let mut effects = self.end();
        effects.push(Effect::Focus(window));

        effects
    }

    /// Highlights the window a number key names, and switches to it when the
    /// configuration says a number means that.
    ///
    /// Only a switch on screen has numbers to press: the keys are bound with
    /// it, and the numbers are only shown there.
    fn pick(&mut self, index: usize, confirm: bool) -> Vec<Effect> {
        let picked = match &mut self.session {
            Some(session) if self.shown => session.select(index),
            _ => false,
        };

        if !picked {
            return vec![];
        }

        if confirm {
            self.confirm()
        } else {
            vec![Effect::Highlight]
        }
    }

    /// Closes the highlighted window, and carries on with the rest.
    ///
    /// Only a switch on screen closes anything: a window nobody can see
    /// being chosen is not one to close.
    fn close(&mut self, order: &[String]) -> Vec<Effect> {
        let Some(session) = &mut self.session else {
            return vec![];
        };

        if !self.shown {
            return vec![];
        }

        let window = session.selected_window().clone();
        let order: Vec<&str> = order.iter().map(String::as_str).collect();

        if session.remove_selected(&order) {
            return vec![Effect::Close(window), Effect::Fill, Effect::Highlight];
        }

        // That was the last window there was.
        let mut effects = self.end();
        effects.push(Effect::Close(window));

        effects
    }

    /// Ends the switch, leaving nothing behind for the next one.
    fn end(&mut self) -> Vec<Effect> {
        self.session = None;

        if std::mem::take(&mut self.shown) {
            vec![Effect::Hide]
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: &str, app_id: &str) -> Window {
        Window {
            id: id.to_owned(),
            app_id: app_id.to_owned(),
            title: format!("{app_id} {id}"),
            size: None,
            initial_title: String::new(),
        }
    }

    /// Two ghostty windows and one brave window, most recently used first.
    fn windows() -> Vec<Window> {
        vec![
            window("1", "com.mitchellh.ghostty"),
            window("2", "brave-browser"),
            window("3", "com.mitchellh.ghostty"),
        ]
    }

    fn trigger(app: &str) -> Event {
        towards(app, Direction::Forward)
    }

    fn towards(app: &str, direction: Direction) -> Event {
        Event::Trigger {
            target: Target::new(app, None),
            direction,
            windows: windows(),
            focused: None,
        }
    }

    /// Holds Super and presses a mapped key, without releasing Super.
    fn started() -> Controller {
        let mut controller = Controller::default();

        assert_eq!(
            controller.handle(trigger("ghostty")),
            [Effect::ScheduleReveal { session: 1 }]
        );

        controller
    }

    /// Waits long enough for the overlay to appear.
    fn opened() -> Controller {
        let mut controller = started();

        assert_eq!(
            controller.handle(Event::Reveal { session: 1 }),
            [Effect::Fill, Effect::Highlight, Effect::Show]
        );

        controller
    }

    #[test]
    fn a_fast_tap_switches_without_ever_revealing_the_overlay() {
        let mut controller = started();

        assert_eq!(
            controller.handle(Event::Confirm),
            [Effect::Focus(window("1", "com.mitchellh.ghostty"))]
        );

        // The reveal timer fires after the switch is already over.
        assert_eq!(controller.handle(Event::Reveal { session: 1 }), []);
    }

    #[test]
    fn a_reveal_left_over_from_an_earlier_switch_is_ignored() {
        let mut controller = started();
        controller.handle(Event::Confirm);
        controller.handle(trigger("ghostty"));

        assert_eq!(controller.handle(Event::Reveal { session: 1 }), []);
        assert_eq!(
            controller.handle(Event::Reveal { session: 2 }),
            [Effect::Fill, Effect::Highlight, Effect::Show]
        );
    }

    #[test]
    fn holding_super_reveals_the_overlay_and_releasing_it_focuses_the_selection() {
        let mut controller = opened();

        assert_eq!(
            controller.handle(Event::Confirm),
            [
                Effect::Hide,
                Effect::Focus(window("1", "com.mitchellh.ghostty"))
            ]
        );
    }

    #[test]
    fn pressing_the_same_key_again_cycles_the_highlight() {
        let mut controller = opened();

        assert_eq!(controller.handle(trigger("ghostty")), [Effect::Highlight]);
        assert_eq!(
            controller.handle(Event::Confirm),
            [
                Effect::Hide,
                Effect::Focus(window("3", "com.mitchellh.ghostty"))
            ]
        );
    }

    #[test]
    fn cycling_before_the_overlay_appears_still_moves_the_selection() {
        let mut controller = started();

        assert_eq!(controller.handle(trigger("ghostty")), []);
        assert_eq!(
            controller.handle(Event::Confirm),
            [Effect::Focus(window("3", "com.mitchellh.ghostty"))]
        );
    }

    #[test]
    fn pressing_another_mapped_key_swaps_to_that_application() {
        let mut controller = opened();

        assert_eq!(
            controller.handle(trigger("brave")),
            [Effect::Fill, Effect::Highlight]
        );
        assert_eq!(
            controller.handle(Event::Confirm),
            [Effect::Hide, Effect::Focus(window("2", "brave-browser"))]
        );
    }

    #[test]
    fn shift_and_the_same_key_start_the_switch_at_the_other_end() {
        let mut controller = Controller::default();

        controller.handle(towards("ghostty", Direction::Backward));

        assert_eq!(
            controller.handle(Event::Confirm),
            [Effect::Focus(window("3", "com.mitchellh.ghostty"))]
        );
    }

    #[test]
    fn the_next_and_previous_keys_move_the_highlight_both_ways() {
        let mut controller = opened();

        assert_eq!(
            controller.handle(Event::Cycle {
                direction: Direction::Forward
            }),
            [Effect::Highlight]
        );
        assert_eq!(
            controller.handle(Event::Cycle {
                direction: Direction::Backward
            }),
            [Effect::Highlight]
        );
        assert_eq!(
            controller.handle(Event::Confirm),
            [
                Effect::Hide,
                Effect::Focus(window("1", "com.mitchellh.ghostty"))
            ]
        );
    }

    #[test]
    fn steering_keys_do_nothing_outside_a_switch() {
        let mut controller = Controller::default();

        assert_eq!(
            controller.handle(Event::Cycle {
                direction: Direction::Forward
            }),
            []
        );
    }

    #[test]
    fn escape_closes_the_overlay_without_focusing_anything() {
        let mut controller = opened();

        assert_eq!(controller.handle(Event::Cancel), [Effect::Hide]);
        assert_eq!(controller.handle(Event::Confirm), []);
    }

    #[test]
    fn an_application_without_windows_is_launched_instead() {
        let mut controller = Controller::default();

        assert_eq!(
            controller.handle(trigger("spotify")),
            [Effect::Launch(Target::new("spotify", None))]
        );
        assert_eq!(controller.handle(Event::Confirm), []);
    }

    #[test]
    fn switching_to_an_application_without_windows_launches_it_and_ends_the_switch() {
        let mut controller = opened();

        assert_eq!(
            controller.handle(trigger("spotify")),
            [Effect::Hide, Effect::Launch(Target::new("spotify", None))]
        );
        assert_eq!(controller.handle(Event::Confirm), []);
    }

    #[test]
    fn releasing_super_outside_a_switch_does_nothing() {
        let mut controller = Controller::default();

        assert_eq!(controller.handle(Event::Confirm), []);
        assert_eq!(controller.handle(Event::Cancel), []);
        assert_eq!(controller.handle(Event::Reveal { session: 0 }), []);
    }

    fn close() -> Event {
        Event::Close {
            order: vec![
                "com.mitchellh.ghostty".to_owned(),
                "brave-browser".to_owned(),
            ],
        }
    }

    fn pick(index: usize, confirm: bool) -> Event {
        Event::Pick { index, confirm }
    }

    #[test]
    fn a_number_key_highlights_the_window_it_belongs_to() {
        let mut controller = opened();

        assert_eq!(controller.handle(pick(1, false)), [Effect::Highlight]);
        assert_eq!(
            controller.handle(Event::Confirm),
            [
                Effect::Hide,
                Effect::Focus(window("3", "com.mitchellh.ghostty"))
            ]
        );
    }

    #[test]
    fn a_number_key_can_switch_without_waiting_for_super() {
        let mut controller = opened();

        assert_eq!(
            controller.handle(pick(1, true)),
            [
                Effect::Hide,
                Effect::Focus(window("3", "com.mitchellh.ghostty"))
            ]
        );
        assert_eq!(controller.handle(Event::Confirm), []);
    }

    #[test]
    fn a_number_no_window_answers_to_does_nothing() {
        let mut controller = opened();

        assert_eq!(controller.handle(pick(2, false)), []);
        assert_eq!(controller.handle(pick(2, true)), []);
        // The highlight is where it was.
        assert_eq!(
            controller.handle(Event::Confirm),
            [
                Effect::Hide,
                Effect::Focus(window("1", "com.mitchellh.ghostty"))
            ]
        );
    }

    #[test]
    fn numbers_do_nothing_before_the_switcher_is_on_screen() {
        let mut controller = started();

        assert_eq!(controller.handle(pick(1, false)), []);
        assert_eq!(Controller::default().handle(pick(0, true)), []);
    }

    #[test]
    fn closing_a_window_keeps_the_switcher_open_on_the_rest() {
        let mut controller = opened();

        assert_eq!(
            controller.handle(close()),
            [
                Effect::Close(window("1", "com.mitchellh.ghostty")),
                Effect::Fill,
                Effect::Highlight
            ]
        );
        assert_eq!(
            controller.handle(Event::Confirm),
            [
                Effect::Hide,
                Effect::Focus(window("3", "com.mitchellh.ghostty"))
            ]
        );
    }

    #[test]
    fn closing_an_applications_last_window_moves_to_the_next_one_shown() {
        let mut controller = opened();
        controller.handle(close());
        controller.handle(close());

        assert_eq!(
            controller.handle(Event::Confirm),
            [Effect::Hide, Effect::Focus(window("2", "brave-browser"))]
        );
    }

    #[test]
    fn closing_the_last_window_there_is_ends_the_switch() {
        let mut controller = opened();
        controller.handle(close());
        controller.handle(close());

        assert_eq!(
            controller.handle(close()),
            [Effect::Hide, Effect::Close(window("2", "brave-browser"))]
        );
        assert_eq!(controller.handle(Event::Confirm), []);
    }

    #[test]
    fn nothing_is_closed_before_the_switcher_is_on_screen() {
        let mut controller = started();

        assert_eq!(controller.handle(close()), []);
        assert_eq!(Controller::default().handle(close()), []);
    }

    #[test]
    fn the_next_switch_starts_from_a_clean_slate() {
        let mut controller = opened();
        controller.handle(Event::Cancel);

        assert_eq!(
            controller.handle(trigger("brave")),
            [Effect::ScheduleReveal { session: 2 }]
        );
        assert_eq!(
            controller.handle(Event::Reveal { session: 2 }),
            [Effect::Fill, Effect::Highlight, Effect::Show]
        );
    }
}
