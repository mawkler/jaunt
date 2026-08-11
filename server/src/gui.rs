use std::cell::RefCell;
use std::io::{BufRead, BufReader};
use std::os::unix::net;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result};
use gtk4::gdk;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use raisin::compositor::{ActiveCompositor, Compositor, Window};
use raisin::ipc::Message;
use raisin::picker::{self, Picker};

use crate::input;
use crate::wayland;

fn flat_row_index(picker: &Picker) -> i32 {
    let mut flat = 0;

    for key in picker.groups.keys() {
        if key == &picker.current_group_name {
            break;
        }
        flat += 1 + picker.groups[key].len();
    }
    flat += 1 + picker.current_window_idx;

    i32::try_from(flat).expect("row index exceeds i32 range")
}

fn cycle_and_select(
    picker: &Rc<RefCell<Picker>>,
    list_box: &gtk4::ListBox,
    direction: picker::Direction,
) {
    let mut picker = picker.borrow_mut();
    picker.cycle_window(direction);
    let flat_idx = flat_row_index(&picker);
    if let Some(row) = list_box.row_at_index(flat_idx) {
        list_box.select_row(Some(&row));
        row.grab_focus();
    }
}

fn populate_list_box(picker: &Picker, list_box: &gtk4::ListBox) {
    while let Some(row) = list_box.first_child() {
        list_box.remove(&row);
    }

    for (app_id, windows) in &picker.groups {
        let header = gtk4::Label::new(Some(app_id));
        header.add_css_class("group-header");
        header.set_halign(gtk4::Align::Start);
        let header_row = gtk4::ListBoxRow::new();
        header_row.set_child(Some(&header));
        header_row.set_selectable(false);
        header_row.set_focusable(false);
        list_box.append(&header_row);

        for window in windows {
            let label = gtk4::Label::new(Some(&window.title));
            label.add_css_class("window-entry");
            label.set_halign(gtk4::Align::Start);
            let entry_row = gtk4::ListBoxRow::new();
            entry_row.set_child(Some(&label));
            entry_row.set_focusable(true);
            list_box.append(&entry_row);
        }
    }
}

fn select_row(picker: &Picker, list_box: &gtk4::ListBox) {
    let idx = flat_row_index(picker);
    if let Some(row) = list_box.row_at_index(idx) {
        list_box.select_row(Some(&row));
        row.grab_focus();
    }
}

fn create_list_box() -> gtk4::ListBox {
    let list_box = gtk4::ListBox::new();
    list_box.set_activate_on_single_click(false);
    list_box.set_selection_mode(gtk4::SelectionMode::Single);
    list_box
}

fn create_overlay_window() -> gtk4::Window {
    let window = gtk4::Window::new();
    window.init_layer_shell();
    window.set_namespace(Some("raisin"));
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);
    window.set_margin(Edge::Left, 200);
    window.set_margin(Edge::Right, 200);
    window.set_anchor(Edge::Top, true);
    window.set_margin(Edge::Top, 80);
    window.set_default_size(400, 300);
    window.set_anchor(Edge::Bottom, true);
    window.set_margin(Edge::Bottom, 80);
    window.set_css_classes(&["raisin-window"]);
    window
}

fn load_css() -> Result<()> {
    let provider = gtk4::CssProvider::new();
    provider.load_from_data(
        ".raisin-window { background-color: rgba(35, 35, 35, 0.96); }
         .header { font-size: 18px; font-weight: bold; padding: 8px; color: #ffffff; }
         .footer { font-size: 14px; padding: 8px; color: #aaaaaa; }
         .group-header { font-size: 16px; font-weight: bold; padding: 4px 8px; color: #ffffff; }
         .window-entry { padding: 4px 8px; color: #dddddd; }
         .window-entry:selected { background-color: rgba(80, 120, 220, 0.6); }",
    );
    let display = gdk::Display::default().context("failed to load display")?;
    let priority = gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION;

    gtk4::style_context_add_provider_for_display(&display, &provider, priority);
    Ok(())
}

fn create_header_label() -> gtk4::Label {
    let label = gtk4::Label::new(None);
    label.add_css_class("header");
    label
}

fn create_footer_label() -> gtk4::Label {
    let label = gtk4::Label::new(Some("Release Super to switch \u{00b7} Esc to cancel"));
    label.add_css_class("footer");
    label
}

fn build_layout(
    window: &gtk4::Window,
    list_box: &gtk4::ListBox,
    header_label: &gtk4::Label,
    footer_label: &gtk4::Label,
) {
    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_child(Some(list_box));
    scrolled.set_vexpand(true);

    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.append(header_label);
    vbox.append(&scrolled);
    vbox.append(footer_label);

    window.set_child(Some(&vbox));
}

struct GuiState {
    picker: Option<Rc<RefCell<Picker>>>,
    selected_window: Option<Window>,
}

struct Gui {
    window: gtk4::Window,
    list_box: gtk4::ListBox,
    header_label: gtk4::Label,
    state: Rc<RefCell<GuiState>>,
    modifier_observer: Option<wayland::ModifierObserver>,
    compositor: Rc<ActiveCompositor>,
    listener: net::UnixListener,
}

impl Gui {
    fn handle_switch(&self, app: &str, app_id: Option<&str>, direction: picker::Direction) {
        let search = app_id.unwrap_or(app).to_lowercase();
        log::info!("handling switch for '{search}' ({direction:?})");

        let active_picker = self.state.borrow().picker.clone();

        let Some(active_picker) = active_picker else {
            self.start_session(app, &search);
            return;
        };

        let same_group = {
            let picker = active_picker.borrow();
            picker::group_name_search(&picker.groups, &search) == Some(&picker.current_group_name)
        };

        if same_group {
            log::info!("same window group, cycling");
            cycle_and_select(&active_picker, &self.list_box, direction);
        } else {
            log::info!("different window group, restarting session");
            self.end_session();
            self.start_session(app, &search);
        }
    }

    fn start_session(&self, app: &str, search: &str) {
        let all_windows = match self.compositor.get_windows() {
            Ok(windows) => windows,
            Err(err) => {
                log::error!("failed to get windows: {err}");
                return;
            }
        };
        let focused_app_id = all_windows.first().map(|w| w.app_id.to_lowercase());
        let groups = picker::build_groups(all_windows);

        let Some(current_group_name) = picker::group_name_search(&groups, search) else {
            log::info!("no windows found for '{search}', launching {app}");
            match self.compositor.launch_application(app) {
                Ok(()) => {}
                Err(err) => log::error!("failed to launch application '{app}': {err}"),
            }
            return;
        };

        let current_group_name = current_group_name.clone();
        let current_window_idx = picker::initial_window_idx(
            &groups[&current_group_name],
            &current_group_name,
            focused_app_id.as_deref(),
        );

        let picker = Rc::new(RefCell::new(Picker {
            groups,
            current_group_name,
            current_window_idx,
        }));

        populate_list_box(&picker.borrow(), &self.list_box);
        self.header_label
            .set_text(&format!("Switch to {}", picker.borrow().current_group_name));
        select_row(&picker.borrow(), &self.list_box);

        self.state.borrow_mut().picker = Some(picker);

        if let Some(observer) = self.modifier_observer.as_ref() {
            observer.arm();
        }

        self.window.present();
        if let Some(display) = gdk::Display::default() {
            display.sync();
        }
        while gtk4::glib::MainContext::default().iteration(false) {
            // drain pending events to establish the keyboard grab
        }

        // The user might have released Super while the grab was being
        // established (the release event fires during the drain above).
        if let Some(window) = self.state.borrow_mut().selected_window.take() {
            log::info!("super was released during grab setup, ending session");
            self.end_session();
            self.focus(&window);
        } else if let Some(observer) = self.modifier_observer.as_ref() {
            // The compositor mandates a `modifiers` event right after the
            // grab's `enter`; waiting for it is the only reliable way to see
            // whether Super was still held when the grab went live.
            let mods = observer.wait_for_grab_modifiers();
            match mods {
                Some(0) => {
                    log::info!(
                        "super was already released when the grab was established \
                         (mods_depressed=0x{:x}), ending session",
                        observer.depressed_mods()
                    );
                    self.on_super_released();
                }
                Some(_) => {
                    log::debug!(
                        "grab established with super held (mods_depressed=0x{:x})",
                        observer.depressed_mods()
                    );
                }
                None => {
                    log::info!("no modifiers event observed after the grab, ending session");
                    self.on_super_released();
                }
            }
        }
    }

    fn end_session(&self) {
        self.window.set_visible(false);
        while gtk4::glib::MainContext::default().iteration(false) {
            // drain pending events to unmap the layer surface
        }
        self.state.borrow_mut().picker = None;
    }

    fn cancel_session(&self) {
        let mut state = self.state.borrow_mut();
        state.picker = None;
        state.selected_window = None;
        self.window.set_visible(false);
    }

    fn on_super_released(&self) {
        let window = {
            let state = self.state.borrow();
            let Some(picker) = state.picker.as_ref() else {
                return;
            };
            let picker = picker.borrow();
            let Some(window) = picker
                .current_group_windows()
                .get(picker.current_window_idx)
            else {
                log::error!(
                    "could not find any window with index {} in current group",
                    picker.current_window_idx
                );
                return;
            };
            window.clone()
        };

        self.end_session();
        self.focus(&window);
    }

    fn focus(&self, window: &Window) {
        let Window { id, app_id, .. } = window;
        log::info!("focusing window {id} ({app_id})");
        if let Err(err) = self.compositor.focus_window(window) {
            log::error!("failed to focus window: {err}");
        }
    }
}

fn setup_socket_handler(gui: &Rc<Gui>) {
    let listener = gui.listener.try_clone().expect("failed to clone listener");
    let gui_for_cb = gui.clone();

    let _ = gtk4::glib::source::timeout_add_local(Duration::from_millis(50), move || {
        let (mut stream, _) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                return gtk4::glib::ControlFlow::Continue;
            }
            Err(err) => {
                log::error!("listener accept failed: {err}");
                return gtk4::glib::ControlFlow::Break;
            }
        };

        let line = BufReader::new(&mut stream)
            .lines()
            .next()
            .and_then(Result::ok)
            .unwrap_or_default();

        let message = match serde_json::from_str::<Message>(&line) {
            Ok(message) => message,
            Err(err) => {
                if !line.is_empty() {
                    log::warn!("failed to parse message '{line}': {err}");
                }
                return gtk4::glib::ControlFlow::Continue;
            }
        };

        match message {
            Message::Forward { app, app_id } => {
                gui_for_cb.handle_switch(&app, app_id.as_deref(), picker::Direction::Forward);
            }
            Message::Backward { app, app_id } => {
                gui_for_cb.handle_switch(&app, app_id.as_deref(), picker::Direction::Backward);
            }
        }

        gtk4::glib::ControlFlow::Continue
    });
}

fn setup_key_handlers(gui: &Rc<Gui>) {
    let controller = gtk4::EventControllerKey::new();

    let gui_for_esc = gui.clone();
    controller.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            gui_for_esc.cancel_session();
            return gtk4::glib::Propagation::Stop;
        }
        gtk4::glib::Propagation::Proceed
    });

    let gui_for_super = gui.clone();
    controller.connect_key_released(move |_, key, _, _| {
        if input::is_super_key(key) {
            gui_for_super.on_super_released();
        }
    });

    gui.window.add_controller(controller);
}

pub(crate) fn run(compositor: ActiveCompositor, listener: net::UnixListener) -> Result<()> {
    let window = create_overlay_window();
    load_css().context("failed to load CSS")?;
    let list_box = create_list_box();
    let header_label = create_header_label();
    let footer_label = create_footer_label();
    build_layout(&window, &list_box, &header_label, &footer_label);

    let display = gdk::Display::default().context("failed to get the default display")?;
    let modifier_observer = match wayland::ModifierObserver::new(&display) {
        Ok(observer) => Some(observer),
        Err(err) => {
            log::warn!(
                "failed to observe keyboard modifiers, falling back to release-only switching: {err}"
            );
            None
        }
    };

    let gui = Rc::new(Gui {
        window,
        list_box,
        header_label,
        modifier_observer,
        state: Rc::new(RefCell::new(GuiState {
            picker: None,
            selected_window: None,
        })),
        compositor: Rc::new(compositor),
        listener,
    });

    setup_socket_handler(&gui);
    setup_key_handlers(&gui);

    gtk4::glib::MainLoop::new(None, false).run();

    Ok(())
}
