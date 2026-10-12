# Jaunt

Run-or-raise for [Hyprland](https://hyprland.org) and [Niri](https://github.com/YaLTeR/niri): one
key per application, which launches it if it isn't running and otherwise focuses it, cycling
through its windows if it has several.

```sh
jaunt ghostty                        # bind to e.g. Super + t
jaunt ghostty com.mitchellh.ghostty  # when the window class isn't the command
```

## The switcher (Hyprland)

![The switcher open on Brave: the applications in a bar, each with its key and a dot per open window, and Brave in a tab joined to the sheet holding its window, highlighted as the one it would switch to](docs/switcher.png)

`jaunt daemon` adds an Alt-Tab style switcher drawn by [Quickshell](https://quickshell.org). It
binds `Super` + a letter for each application in your configuration, so there are no Hyprland
keybinds to write.

- **Tap** `Super` + letter to focus the application without showing anything.
- **Hold** `Super` and the switcher appears: your applications in a bar, with live previews of the
  selected one's windows.
- While it's open:
  - the same letter cycles through the windows, `Shift` + letter goes back
  - another letter switches to that application
  - `Ctrl` + letter opens a new window of the application
  - the `close` key, if you set one, closes the highlighted window
  - releasing `Super` focuses the selected window, `Esc` cancels
- Applications with nothing open are shown faintly, and their key launches them, flashing up
  their icon to say so.

Start the daemon once, from your Hyprland startup configuration or with the [NixOS
module](#nixos). `jaunt switch <app>` triggers a switch from a keybind of your own.

While it runs, Jaunt takes its letters over from your Hyprland configuration (`hyprctl reload`
gives them back) and warns about clashes. `Super` and `Esc` are shared, so your own binds on them
keep working. Without Quickshell the keys still switch, with nothing on screen.

To blur what shows through the panel, add a layer rule (Lua configuration shown):

```lua
hl.layer_rule({ match = { namespace = "^jaunt$" }, blur = true, ignore_alpha = 0.1 })
```

`ignore_alpha` keeps the corners outside the rounded panel sharp, and has to stay below
`background_opacity`, or nothing is blurred.

## Configuration

`~/.config/jaunt/config.toml`, or `jaunt daemon --config <path>`. Everything is optional, but
nothing is bound until `[keys.apps]` has entries. Saving the file applies it straight away; a
mistake keeps the running configuration and logs what's wrong.

```toml
[keys]
next = "Tab"             # only bound while the switcher is open; Super is implied
previous = "SHIFT + Tab"
cancel = "Escape"
close = "q"              # close the highlighted window; unset by default

[keys.apps]              # Super + key
t = "ghostty"
s = "spotify"
w = { cmd = "brave", app_id = "brave-browser" } # when the window class isn't the command
"SHIFT + a" = "teams"    # a key may carry modifiers, but then has no Shift to go back with

[switcher]
delay = 90                # milliseconds Super is held before the switcher appears
width = "50%"             # of the screen, or pixels
max_height = "40%"        # previews shrink to fit
icons = true              # icons rather than initials
background_opacity = 0.92 # the panel
foreground_opacity = 1    # the sheet holding the windows
theme = "default"

[previews]
enabled = true
height = 150              # pixels

[names]                   # by window class; otherwise the desktop entry's name
brave-browser = "Brave"
```

Hyprland fires every bind on a key, so if you bind `Super` + your `close` key yourself, have it do
nothing while the switcher is up, or it closes a second window:

```lua
hl.bind("SUPER + Q", function()
    if #hl.get_layers({ namespace = "jaunt" }) == 0 then
        hl.dispatch(hl.dsp.window.close())
    end
end)
```

### Themes

Built in: `default`, `light`, `catppuccin-latte`, `catppuccin-frappe`, `catppuccin-macchiato`,
`catppuccin-mocha`, `nord`, `one-dark` and `tokyo-night`.

Your own go in `~/.config/jaunt/themes/<name>.toml`, chosen with `theme = "<name>"` or a path.
They set six colours, and Jaunt derives the rest; anything left out comes from `default`. Saving
one applies it straight away. [`themes/`](themes) has the built-in ones to start from.

```toml
background = "#15171c" # the panel
surface = "#20232b"    # the sheet and its tab
accent = "#6b8cff"     # the selected window
text = "#c4cad8"       # window titles and keys
bright = "#eef1f7"     # the heading and the selected window's title
muted = "#78819a"      # secondary text and hints
```

## Install

Coming from Raisin? Stop its daemon, then move `~/.config/raisin` to `~/.config/jaunt`.

### Nix

`nix run github:mawkler/jaunt -- <app>`

### NixOS

```nix
{
  inputs.jaunt.url = "github:mawkler/jaunt";

  # In your configuration:
  imports = [ inputs.jaunt.nixosModules.default ];

  services.jaunt = {
    enable = true;
    settings.keys.apps = {
      t = "ghostty";
      w = { cmd = "brave"; app_id = "brave-browser"; };
    };
  };
}
```

This runs the daemon as a user service, which needs the session in systemd, e.g.
`programs.hyprland.withUWSM = true`. Without `settings` it reads `~/.config/jaunt/config.toml`,
which you can edit without rebuilding.

### Cargo

`cargo install --git https://github.com/mawkler/jaunt`

The switcher also needs Quickshell's `qs` on your `PATH`.

## Development

The switcher's QML in `shell/` is built into the binary. `JAUNT_SHELL=$PWD/shell jaunt daemon`
runs it from the checkout instead, reloading on every save.
