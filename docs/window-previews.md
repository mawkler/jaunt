# Window previews

How the switcher shows what each window actually looks like, rather than only its title.

The overlay is drawn by Quickshell, and the pictures come from its `ScreencopyView`: the daemon
never touches a capture. This page is what the view does with them, and the reasoning behind it.

## What it has to leave alone

The switcher's two hard timing rules come first, and previews are additive to both:

- **A fast tap must show nothing.** Capturing has to start when the overlay is revealed, not when
  the key is pressed. A tap that never reveals anything must never capture anything either.
- **Confirming must not wait for drawing.** A preview that is still arriving, or never arrives,
  must not delay or block a switch. The session state machine in `src/daemon/controller.rs` owns
  both rules: previews are a view concern, and the view shows a placeholder until a picture turns
  up.

Both hold by construction. The view hears about a switch only through `Effect::Fill`, which the
state machine emits once the switcher is to be on screen, so a tap never reaches it. And the daemon
only ever writes to the view, on a thread of its own, so nothing on the path from a key press to a
focused window can wait on it.

## How other switchers do it

| Approach | Who uses it | Why it does or doesn't fit |
| --- | --- | --- |
| Compositor-internal scene access | GNOME Shell, KDE's Present Windows, Hyprland's `hyprexpo` plugin | Direct access to each window's texture, so it's free and always correct — but only from inside the compositor. As a Hyprland plugin it would tie Jaunt to a C++ ABI that changes every release, and rules out every other compositor. |
| Portal ScreenCast (xdg-desktop-portal + PipeWire) | OBS window capture, most screen sharing | Built for sharing, not thumbnails: a consent dialog per source, a PipeWire dependency, and a session setup measured in hundreds of milliseconds. Wrong shape for something that has to appear in 90 ms. |
| `wlr-screencopy-unstable-v1`, cropped to the window | `grim`, `wayshot`, and the crop-the-screenshot trick | Captures an output, not a window. Only works for windows currently on screen, so most of a grouped list would come back blank, stale, or showing whatever is on top of them. |
| `hyprland-toplevel-export-v1` | `xdg-desktop-portal-hyprland`'s window sharing | Captures one window by address, whether or not it's visible. Exact and available today — and Hyprland-only. |
| `ext-image-capture-source-v1` + `ext-image-copy-capture-v1` | The standardised successor to both wlr-screencopy and toplevel-export | Same capability, specified rather than vendor-specific, with a capture source for a foreign toplevel. What Quickshell's `ScreencopyView` uses where the compositor offers it. |

## How the view does it

- **Finding the window.** The daemon sends each window's Hyprland address. The view looks it up
  among Quickshell's `Hyprland.toplevels` and captures `toplevel.wayland`. Hyprland's IPC writes an
  address with `0x` in front and Quickshell without, so the view drops it before comparing.
- **Only the application being switched to is live**, so a video or a terminal keeps moving in the
  switcher. Every other open application's windows wait beneath it, hidden, and are captured
  once each time the switcher appears: switching to one shows its windows as they are now
  straight away, and they turn live from there.
- **Captures start a frame late.** The switcher is drawn first, with whatever each window last
  showed, and captures start once it is on screen. Starting them with it held the first frame up
  by about 100 ms.
- **Effects go on an item around a capture.** A `ScreencopyView` that is itself made a layer,
  which every effect needs, draws nothing at all into it. Wrapped in an item that is the layer, it
  draws as usual.

## What can go wrong

| Failure | What the user sees |
| --- | --- |
| Quickshell isn't installed | No overlay at all. The keys still switch, and the daemon says why at startup. |
| A window hasn't been drawn since it was last visible | Whatever it last drew, which is what every other switcher shows too. |
| A capture takes longer than the switch | The window shows its application's icon until it arrives; the switch is unaffected. |
| A window positioned entirely off the monitor | Its application's icon instead. Hyprland copies a window only while its box overlaps the monitor being rendered. Windows on hidden workspaces usually still overlap by a sliver and do capture. |
| The view crashes | The daemon starts it again, waiting longer each time it fails in a row, and catches it up on the switch in progress when it reconnects. |
