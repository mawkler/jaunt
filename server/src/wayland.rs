//! Raw Wayland plumbing to observe the compositor's current modifier state.
//!
//! GDK's Wayland backend maps the compositor's xkb modifier mask onto GDK
//! modifier flags and drops everything except Shift/Lock/Control/Alt
//! ("Mod1"). Super, which the standard evdev keymap reports as xkb "Mod4",
//! is therefore never visible through GDK: `gdk_keymap_get_modifier_state`
//! returns an empty mask on such layouts.
//!
//! To read the real state we bind our own `wl_keyboard` object on GDK's own
//! Wayland connection (the events it receives are dispatched by GDK's event
//! source, since the proxy lives on the display's default queue). The
//! Wayland protocol requires the compositor to send the current modifier
//! state right after each `wl_keyboard.enter` event, so the `mods_depressed`
//! mask captured after the switcher grabs the keyboard tells us whether
//! Super was still held when the grab went live.
//!
//! The needed libwayland-client functions are resolved at runtime with
//! `dlopen`/`dlsym` rather than linked: gtk4-layer-shell interposes its own
//! copies of the `wl_proxy_*` API and only works when it is the first
//! provider of those symbols in the process, which requires libwayland-client
//! to be absent from the executable's `DT_NEEDED`. Loading the library
//! dynamically (it is already loaded by GTK at that point) sidesteps the link
//! ordering entirely.

use std::cell::Cell;
use std::ffi::CStr;
use std::fs::File;
use std::os::fd::FromRawFd;
use std::os::raw::{c_char, c_int, c_uint, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use std::{ptr, thread, time::Duration};

use anyhow::Result;
use gtk4::gdk;
use gtk4::prelude::ObjectType;

/// Opaque libwayland-client types; only ever passed between functions.
#[allow(non_camel_case_types)]
enum wl_display {}
#[allow(non_camel_case_types)]
enum wl_registry {}
#[allow(non_camel_case_types)]
enum wl_seat {}
#[allow(non_camel_case_types)]
enum wl_keyboard {}
#[allow(non_camel_case_types)]
enum wl_surface {}
#[allow(non_camel_case_types)]
enum wl_array {}

/// `struct wl_interface` as defined by wayland-util.h.
#[repr(C)]
struct wl_interface {
    name: *const c_char,
    version: c_int,
    method_count: c_int,
    methods: *const wl_message,
    event_count: c_int,
    events: *const wl_message,
}

/// `struct wl_message` as defined by wayland-util.h; never dereferenced.
#[repr(C)]
struct wl_message {
    name: *const c_char,
    signature: *const c_char,
    types: *const *const wl_interface,
}

/// `union wl_argument` as defined by wayland-util.h; only `u` and `s` are
/// used by this module.
#[repr(C)]
union wl_argument {
    i: c_int,
    u: c_uint,
    f: f32,
    s: *const c_char,
    o: *mut c_void,
    n: c_uint,
    a: *mut c_void,
    h: c_int,
}

/// Request opcodes from wayland.xml, as generated into
/// wayland-client-protocol.h.
const WL_DISPLAY_GET_REGISTRY: c_uint = 1;
const WL_REGISTRY_BIND: c_uint = 0;
const WL_SEAT_GET_KEYBOARD: c_uint = 1;

/// Listener callbacks for `wl_registry`, mirroring `wl_registry_listener`.
#[repr(C)]
struct RegistryListener {
    global: extern "C" fn(*mut c_void, *mut wl_registry, c_uint, *const c_char, c_uint),
    global_remove: extern "C" fn(*mut c_void, *mut wl_registry, c_uint),
}

/// Listener callbacks for `wl_keyboard` (version 1), mirroring
/// `wl_keyboard_listener`.
#[repr(C)]
struct KeyboardListener {
    keymap: extern "C" fn(*mut c_void, *mut wl_keyboard, c_uint, c_int, c_uint),
    enter: extern "C" fn(*mut c_void, *mut wl_keyboard, c_uint, *mut wl_surface, *mut wl_array),
    leave: extern "C" fn(*mut c_void, *mut wl_keyboard, c_uint, *mut wl_surface),
    key: extern "C" fn(*mut c_void, *mut wl_keyboard, c_uint, c_uint, c_uint, c_uint),
    modifiers: extern "C" fn(*mut c_void, *mut wl_keyboard, c_uint, c_uint, c_uint, c_uint, c_uint),
}

// SAFETY: dlopen/dlsym are standard libc functions (glibc 2.34+, no -ldl
// needed); gdk_wayland_display_get_wl_display is public, stable GDK API on
// the already-linked libgdk. RTLD_NOW has its usual POSIX value.
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn gdk_wayland_display_get_wl_display(display: *mut c_void) -> *mut wl_display;
    fn g_main_context_iteration(context: *mut c_void, may_block: c_int) -> c_int;
}

const RTLD_NOW: c_int = 2;

/// The libwayland-client API used by this module, resolved at runtime.
///
/// libwayland-client since 1.25 no longer exports the generated core
/// protocol stubs (`wl_registry_bind` and friends); the raw `wl_proxy_*`
/// marshalling API below is what the generated code itself uses. The
/// `wl_*_interface` data symbols are still exported.
// The `wl_` field prefix mirrors the C symbol names.
#[allow(clippy::struct_field_names)]
struct WaylandApi {
    wl_display_get_error: unsafe extern "C" fn(*mut c_void) -> c_int,
    wl_proxy_get_version: unsafe extern "C" fn(*mut c_void) -> c_uint,
    wl_proxy_marshal_array_constructor_versioned: unsafe extern "C" fn(
        *mut c_void,
        c_uint,
        *mut wl_argument,
        *const wl_interface,
        c_uint,
    ) -> *mut c_void,
    wl_proxy_add_listener: unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> c_int,
    wl_registry_interface: *const wl_interface,
    wl_seat_interface: *const wl_interface,
    wl_keyboard_interface: *const wl_interface,
}

impl WaylandApi {
    /// Resolves the API from `libwayland-client.so.0`, which is already
    /// loaded by GTK at this point (the handle is never closed).
    fn load() -> Option<WaylandApi> {
        // SAFETY: dlopen with a static string and standard flags.
        let handle = unsafe { dlopen(c"libwayland-client.so.0".as_ptr(), RTLD_NOW) };
        if handle.is_null() {
            return None;
        }

        macro_rules! sym {
            ($name:literal, $ty:ty) => {{
                // SAFETY: dlsym on a live handle; the looked-up symbols are
                // part of the libwayland-client ABI and never unloaded.
                let ptr = unsafe { dlsym(handle, concat!($name, "\0").as_ptr().cast()) };
                // The symbol is a plain function or a data object; function
                // pointers and raw pointers have the same size on all
                // supported platforms, and clippy has no pointer-compatible
                // `as` cast for function pointer types.
                #[allow(clippy::transmute_ptr_to_ptr)]
                unsafe {
                    std::mem::transmute::<*mut c_void, $ty>(ptr)
                }
            }};
        }

        Some(WaylandApi {
            wl_display_get_error: sym!(
                "wl_display_get_error",
                unsafe extern "C" fn(*mut c_void) -> c_int
            ),
            wl_proxy_get_version: sym!(
                "wl_proxy_get_version",
                unsafe extern "C" fn(*mut c_void) -> c_uint
            ),
            wl_proxy_marshal_array_constructor_versioned: sym!(
                "wl_proxy_marshal_array_constructor_versioned",
                unsafe extern "C" fn(
                    *mut c_void,
                    c_uint,
                    *mut wl_argument,
                    *const wl_interface,
                    c_uint,
                ) -> *mut c_void
            ),
            wl_proxy_add_listener: sym!(
                "wl_proxy_add_listener",
                unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> c_int
            ),
            wl_registry_interface: sym!("wl_registry_interface", *const wl_interface),
            wl_seat_interface: sym!("wl_seat_interface", *const wl_interface),
            wl_keyboard_interface: sym!("wl_keyboard_interface", *const wl_interface),
        })
    }
}

/// Reimplements `wl_display_get_registry` (opcode 1), which the generated
/// code emits as a `wl_proxy_marshal_*` call on the display proxy.
///
/// # Safety
///
/// `display` must be a live `wl_display` proxy.
unsafe fn wl_display_get_registry(api: &WaylandApi, display: *mut wl_display) -> *mut wl_registry {
    // The message signature is "n": a single, zeroed new-id slot that the
    // marshaller fills with the new proxy (it must not be a null pointer).
    let mut args = [wl_argument { u: 0 }];
    // SAFETY: contract above; the registry proxy is created on the display's
    // default queue.
    unsafe {
        (api.wl_proxy_marshal_array_constructor_versioned)(
            display.cast(),
            WL_DISPLAY_GET_REGISTRY,
            args.as_mut_ptr(),
            api.wl_registry_interface,
            (api.wl_proxy_get_version)(display.cast()),
        )
    }
    .cast()
}

/// Reimplements `wl_registry_bind` (opcode 0): marshals the `name`, the
/// interface name string and the version, constructing a new proxy with
/// `interface`.
///
/// # Safety
///
/// `registry` must be a live registry proxy, `interface` a pointer to a
/// static `wl_interface` whose `name` field is a NUL-terminated string valid
/// for the process lifetime, and `version` a version supported by
/// `interface`.
unsafe fn wl_registry_bind(
    api: &WaylandApi,
    registry: *mut wl_registry,
    name: c_uint,
    interface: *const wl_interface,
    version: c_uint,
) -> *mut c_void {
    let mut args = [
        wl_argument { u: name },
        // SAFETY: contract above.
        wl_argument {
            s: unsafe { (*interface).name },
        },
        wl_argument { u: version },
        // Zeroed new-id slot (message signature "usun"), filled by the
        // marshaller with the new proxy.
        wl_argument { u: 0 },
    ];
    // SAFETY: contract above.
    unsafe {
        (api.wl_proxy_marshal_array_constructor_versioned)(
            registry.cast(),
            WL_REGISTRY_BIND,
            args.as_mut_ptr(),
            interface,
            version,
        )
    }
}

/// Reimplements `wl_seat_get_keyboard` (opcode 1), constructing a
/// `wl_keyboard` proxy on `seat` at the seat's own version.
///
/// # Safety
///
/// `seat` must be a live seat proxy.
unsafe fn wl_seat_get_keyboard(api: &WaylandApi, seat: *mut wl_seat) -> *mut wl_keyboard {
    // The message signature is "n": a single, zeroed new-id slot that the
    // marshaller fills with the new proxy (it must not be a null pointer).
    let mut args = [wl_argument { u: 0 }];
    // SAFETY: contract above.
    unsafe {
        (api.wl_proxy_marshal_array_constructor_versioned)(
            seat.cast(),
            WL_SEAT_GET_KEYBOARD,
            args.as_mut_ptr(),
            api.wl_keyboard_interface,
            (api.wl_proxy_get_version)(seat.cast()),
        )
    }
    .cast()
}

/// Reimplements `wl_registry_add_listener`.
///
/// # Safety
///
/// `registry` must be a live registry proxy, `listener` a pointer to a
/// `RegistryListener` valid for the process lifetime.
unsafe fn wl_registry_add_listener(
    api: &WaylandApi,
    registry: *mut wl_registry,
    listener: *const RegistryListener,
    data: *mut c_void,
) -> c_int {
    // SAFETY: mirrors the generated wrapper, which forwards to
    // wl_proxy_add_listener.
    unsafe { (api.wl_proxy_add_listener)(registry.cast(), listener.cast(), data) }
}

/// Reimplements `wl_keyboard_add_listener`.
///
/// # Safety
///
/// `keyboard` must be a live keyboard proxy, `listener` a pointer to a
/// `KeyboardListener` valid for the process lifetime.
unsafe fn wl_keyboard_add_listener(
    api: &WaylandApi,
    keyboard: *mut wl_keyboard,
    listener: *const KeyboardListener,
    data: *mut c_void,
) -> c_int {
    // SAFETY: mirrors the generated wrapper, which forwards to
    // wl_proxy_add_listener.
    unsafe { (api.wl_proxy_add_listener)(keyboard.cast(), listener.cast(), data) }
}

/// Shared state behind the listener `data` pointers: the current
/// `mods_depressed` mask and the resolved API needed to bind further objects.
struct ObserverState {
    cell: Cell<u32>,
    /// Set once the keyboard listener is installed, so
    /// [`ModifierObserver::new`] can wait for the binding to be queued.
    bound: AtomicBool,
    /// Set on each `wl_keyboard.modifiers` event, so a wait can observe the
    /// compositor's post-grab modifier state without a roundtrip.
    mods_received: AtomicBool,
    /// Set while `arm()` is active; the next `enter` consumes it and marks
    /// the grab whose events `wait_for_grab_modifiers` should observe.
    awaiting_grab: AtomicBool,
    /// Set once the grab's `enter` has been seen since `arm()`.
    grab_entered: AtomicBool,
    api: WaylandApi,
}

/// Returns the shared state behind a listener `data` pointer.
///
/// # Safety
///
/// `data` must be the `*mut c_void` registered with [`ModifierObserver::new`],
/// pointing at a live `ObserverState`.
unsafe fn observer_state(data: *mut c_void) -> &'static ObserverState {
    // SAFETY: guaranteed by the caller (see the safety contract above).
    unsafe { &*data.cast::<ObserverState>() }
}

extern "C" fn registry_global(
    data: *mut c_void,
    registry: *mut wl_registry,
    name: c_uint,
    interface: *const c_char,
    version: c_uint,
) {
    // SAFETY: `interface` is a NUL-terminated string owned by libwayland for
    // the duration of the callback.
    let is_seat = unsafe { CStr::from_ptr(interface) }.to_bytes() == b"wl_seat";
    if !is_seat {
        return;
    }
    // SAFETY: data points to the ObserverState registered in
    // ModifierObserver::new.
    let state = unsafe { observer_state(data) };
    // SAFETY: binds a proxy for an advertised global; the resulting objects
    // are never destroyed and outlive the daemon. Version 1 keeps the
    // keyboard listener free of repeat_info.
    let seat = unsafe {
        wl_registry_bind(
            &state.api,
            registry,
            name,
            state.api.wl_seat_interface,
            version.min(1),
        )
    }
    .cast::<wl_seat>();
    // SAFETY: wl_seat_get_keyboard creates the wl_keyboard proxy for the
    // seat, at the seat's version (capped to 1 above).
    let keyboard = unsafe { wl_seat_get_keyboard(&state.api, seat) };
    if keyboard.is_null() {
        return;
    }
    // SAFETY: the listener is a static that lives for the process lifetime.
    unsafe { wl_keyboard_add_listener(&state.api, keyboard, &raw const KEYBOARD_LISTENER, data) };
    state.bound.store(true, Ordering::Release);
}

extern "C" fn registry_global_remove(
    _data: *mut c_void,
    _registry: *mut wl_registry,
    _name: c_uint,
) {
}

extern "C" fn keyboard_keymap(
    _data: *mut c_void,
    _keyboard: *mut wl_keyboard,
    _format: c_uint,
    fd: c_int,
    _size: c_uint,
) {
    // SAFETY: the compositor hands us an fd we own; closing it (via drop)
    // prevents leaking it. The keymap contents are not needed here.
    let _ = unsafe { File::from_raw_fd(fd) };
}

extern "C" fn keyboard_enter(
    data: *mut c_void,
    _keyboard: *mut wl_keyboard,
    serial: c_uint,
    _surface: *mut wl_surface,
    _keys: *mut wl_array,
) {
    // SAFETY: data points to the Cell we registered in ModifierObserver::new.
    let state = unsafe { observer_state(data) };
    state.cell.set(0);
    // This enter is the grab `arm()` is waiting for; consume the flag so
    // events from previous sessions can never satisfy the wait.
    if state.awaiting_grab.swap(false, Ordering::AcqRel) {
        state.grab_entered.store(true, Ordering::Release);
    }
    log::debug!("keyboard_enter (serial {serial})");
}

extern "C" fn keyboard_leave(
    data: *mut c_void,
    _keyboard: *mut wl_keyboard,
    _serial: c_uint,
    _surface: *mut wl_surface,
) {
    // SAFETY: data points to the Cell we registered in ModifierObserver::new.
    unsafe { observer_state(data).cell.set(0) };
    log::debug!("keyboard_leave");
}

extern "C" fn keyboard_key(
    _data: *mut c_void,
    _keyboard: *mut wl_keyboard,
    _serial: c_uint,
    _time: c_uint,
    _key: c_uint,
    _state: c_uint,
) {
}

extern "C" fn keyboard_modifiers(
    data: *mut c_void,
    _keyboard: *mut wl_keyboard,
    _serial: c_uint,
    mods_depressed: c_uint,
    _mods_latched: c_uint,
    _mods_locked: c_uint,
    _group: c_uint,
) {
    // SAFETY: data points to the Cell we registered in ModifierObserver::new.
    unsafe { observer_state(data).cell.set(mods_depressed) };
    // SAFETY: data points to the Cell we registered in ModifierObserver::new.
    unsafe { observer_state(data) }
        .mods_received
        .store(true, Ordering::Release);
    log::debug!("keyboard_modifiers (mods_depressed=0x{mods_depressed:x})");
}

static REGISTRY_LISTENER: RegistryListener = RegistryListener {
    global: registry_global,
    global_remove: registry_global_remove,
};

static KEYBOARD_LISTENER: KeyboardListener = KeyboardListener {
    keymap: keyboard_keymap,
    enter: keyboard_enter,
    leave: keyboard_leave,
    key: keyboard_key,
    modifiers: keyboard_modifiers,
};

/// Observes the compositor's current `wl_keyboard.modifiers` mask.
pub(crate) struct ModifierObserver {
    /// Shared with the Wayland callbacks, which keep a raw pointer to it.
    /// The boxed state lives as long as the daemon, which is also the
    /// lifetime of the Wayland listeners.
    state: Box<ObserverState>,
}

impl ModifierObserver {
    /// Binds a `wl_keyboard` on GDK's Wayland connection so the compositor's
    /// raw `mods_depressed` mask becomes observable.
    ///
    /// # Errors
    ///
    /// Returns an error if the display is not Wayland, libwayland-client
    /// cannot be resolved, or the registry could not be set up.
    pub(crate) fn new(display: &gdk::Display) -> Result<Self> {
        // SAFETY: gdk_wayland_display_get_wl_display is public, stable GTK
        // API; the returned wl_display is owned by GDK and valid for the
        // process lifetime (we never destroy it).
        let wl_display = unsafe { gdk_wayland_display_get_wl_display(display.as_ptr().cast()) };
        if wl_display.is_null() {
            anyhow::bail!("gdk_wayland_display_get_wl_display returned null");
        }

        let api = WaylandApi::load()
            .ok_or_else(|| anyhow::anyhow!("failed to resolve libwayland-client functions"))?;
        // Leaked on purpose: the Wayland listeners hold a raw pointer to it
        // for the rest of the process lifetime.
        let state = Box::new(ObserverState {
            cell: Cell::new(0),
            bound: AtomicBool::new(false),
            mods_received: AtomicBool::new(false),
            awaiting_grab: AtomicBool::new(false),
            grab_entered: AtomicBool::new(false),
            api,
        });
        // SAFETY: the pointer stays valid as long as `state` is leaked (see
        // above), which outlives the Wayland listeners.
        let data = (&raw const *state).cast::<c_void>().cast_mut();

        // SAFETY: returns a registry proxy on the display's default queue,
        // whose events GDK's event source dispatches.
        let registry = unsafe { wl_display_get_registry(&state.api, wl_display) };
        if registry.is_null() {
            // SAFETY: wl_display_get_error is a plain getter on the display.
            let error = unsafe { (state.api.wl_display_get_error)(wl_display.cast()) };
            anyhow::bail!("wl_display_get_registry failed (display error {error})");
        }
        // SAFETY: REGISTRY_LISTENER is a static, valid for the process
        // lifetime.
        if unsafe {
            wl_registry_add_listener(&state.api, registry, &raw const REGISTRY_LISTENER, data)
        } != 0
        {
            anyhow::bail!("wl_registry_add_listener failed");
        }
        // No roundtrip: GDK owns the read side of the connection, so let its
        // event source (iterated below) dispatch the registry globals and
        // install the keyboard listener, then wait a bounded time for the
        // binding to be queued.
        for _ in 0..100 {
            if state.bound.load(Ordering::Acquire) {
                break;
            }
            // SAFETY: iteration with `may_block = false` on the default main
            // context; the display is registered there.
            unsafe { g_main_context_iteration(ptr::null_mut(), 0) };
            thread::sleep(Duration::from_millis(10));
        }
        if !state.bound.load(Ordering::Acquire) {
            anyhow::bail!("no wl_seat global observed in time");
        }

        Ok(Self { state })
    }

    /// The compositor's current `mods_depressed` mask, as of the last
    /// `wl_keyboard.modifiers` event.
    pub(crate) fn depressed_mods(&self) -> u32 {
        self.state.cell.get()
    }

    /// Arms the modifier wait for the next grab: events from previous
    /// sessions can no longer satisfy the wait.
    pub(crate) fn arm(&self) {
        self.state.grab_entered.store(false, Ordering::Release);
        self.state.awaiting_grab.store(true, Ordering::Release);
    }

    /// Waits (bounded) for the armed grab's `enter` and the mandatory
    /// `modifiers` event that follows it on the main thread. Returns the
    /// `mods_depressed` mask, or `None` if the compositor never sent them in
    /// time.
    pub(crate) fn wait_for_grab_modifiers(&self) -> Option<u32> {
        log::debug!("waiting for grab modifiers");
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            // SAFETY: iteration with `may_block = false` on the default main
            // context dispatches GDK's Wayland events on this thread.
            while unsafe { g_main_context_iteration(ptr::null_mut(), 0) } != 0 {}
            let mods_seen = self.state.mods_received.swap(false, Ordering::Acquire);
            let grab_entered = self.state.grab_entered.swap(false, Ordering::Acquire);
            if mods_seen && grab_entered {
                let mods = self.state.cell.get();
                log::debug!("grab modifiers observed (mods_depressed=0x{mods:x})");
                return Some(mods);
            }
            if Instant::now() >= deadline {
                log::debug!("grab modifiers deadline reached");
                return None;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}
