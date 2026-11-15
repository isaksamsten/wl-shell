# wl-dock

A Wayland dock for pinned and running apps, with magnifying icons, window
previews, pinned folders and a Trash. Apps are grouped by their Wayland app ID.

## Getting started

From the repository root:

```sh
cargo install --path apps/wl-dock
wl-dock
```

Add `wl-dock` to your compositor's startup commands. Build dependencies are a
Rust toolchain, a C toolchain, pkg-config, libgbm and libxkbcommon development
packages. Your compositor needs layer-shell, viewporter,
`ext-foreign-toplevel-list-v1`, `ext-foreign-toplevel-image-capture-source-v1`
and `ext-image-copy-capture-v1` (including when previews are off).

Click an app to launch it or select its windows; middle-click launches another
instance. Right-click for its menu. Drag icons to reorder pins.

```sh
wl-dock pin firefox
wl-dock pin ~/Downloads
wl-dock unpin firefox
wl-dock pins
```

Pins live in `$XDG_STATE_HOME/wl-dock/pinned` (normally
`~/.local/state/wl-dock/pinned`), one app ID or absolute/`~/` folder path per
line. You can edit this file too; changes are picked up automatically. Folders
appear after apps; individual files cannot be pinned.

## Configuration

Edit `~/.config/wl-dock/config.toml` (or
`$XDG_CONFIG_HOME/wl-dock/config.toml`). Use `wl-dock --config PATH` for another
file. Every setting is optional. The reference below shows defaults; commented
commands are optional examples, not enabled defaults. Sizes are pixels and
times are milliseconds.

### Dock and windows

```toml
outputs = "all"                 # Or ["DP-1", "eDP-1"]
position = "bottom"             # bottom, left, right
layer = "top"                   # top, overlay
icon-size = 48                  # 16–256
icon-gap = 6
margin = 6                      # Distance from the screen edge
padding = 6                     # Space inside the panel
magnification = 1.8             # 1–3; 1 disables it
magnification-range = 3.0       # Neighbours on each side; 1–10
reserve = true                  # Keep windows out of the resting dock's area
autohide = false
reveal-delay = 150
hide-delay = 500
autohide-trigger = 2            # Edge reveal band; 1–50
hover-delay = 300
labels = true
indicator = "dot"               # dot, dots (up to three), none
active-indicator = true         # Needs mru-event-stream
response = 150                  # Animation spring response; 1–10000
font = "sans-serif"
font-size = 13

strip = true                    # Click an app to show its windows
strip-min-windows = 2           # Below this, activate its most recent window
strip-max-columns = 3           # Along the dock; 0 means unlimited
strip-max-rows = 3              # Away from the dock; 0 means unlimited
strip-card-height = 0.18        # Fraction of output height
strip-max-width = 0.6           # Fraction of output length; also caps card width
previews = "live"               # live, static (one frame), off
zoom = 1.04                     # Highlighted card scale; 1 disables it
windows = "all"                 # all, output (needs output-event-stream)
output-grace = 250              # Wait for the stream to place new windows
```

### Commands and desktop integration

Window activation and closing use your compositor's IPC. Set commands that
accept its `ext-foreign-toplevel` window IDs (translate them if necessary).
Without `close-command`, the menu has no Close entries. Commands run through
`sh -c`.

```toml
# activate-command = "niri msg action focus-window --id {id}"
# close-command = "niri msg action close-window --id {id}"
# launch-command = "uwsm app -- {desktop-id}"
terminal = "foot"               # Command for Terminal=true desktop entries
launch-timeout = 10000          # Maximum launch bounce; 0–60000
# mru-event-stream = "~/.config/wl-dock/focus-stream.sh"
# output-event-stream = "~/.config/wl-dock/output-stream.sh"
```

- `activate-command` and `close-command`: `{id}` is the window ID.
- `launch-command`: `{exec}`, `{desktop-id}` and `{app-id}` are available.
  By default, apps launch using their desktop entry's `Exec=`.
- `terminal`: the app's command is appended; include any needed flag, such as
  `"alacritty -e"`.
- `mru-event-stream`: a long-running command printing one focused window ID per
  line. Enables focus ordering and the active indicator.
- `output-event-stream`: a long-running command printing `<output> <id>` when
  a window opens on or moves to an output. Used by `windows = "output"`.

### Files, folders and Trash

```toml
drag-threshold = 6              # Pixels before an icon starts dragging; 1–100
trash = true
open-command = "xdg-open {path}" # Trash uses trash:/// as its path
trash-empty-command = "gio trash --empty"
trash-command = "gio trash -- {paths}"
copy-command = "cp -rn -- {paths} {dest}"
move-command = "mv -n -- {paths} {dest}"
```

The last three commands handle files dropped onto the Trash or a folder.
`{paths}` is the source list; `{dest}` is the destination folder. Install `gio`
for the default Trash commands.

### Per-output settings

An output table inherits global settings. These are **all** the keys it can
override; `enabled` can also include or exclude an output regardless of `outputs`.

```toml
[output."eDP-1"]
enabled = true
position = "bottom"
icon-size = 48
magnification = 1.8
reserve = true
autohide = false
margin = 6
windows = "all"
strip-max-columns = 3
strip-max-rows = 3
```

### Per-app settings

Use the app's Wayland app ID. All keys below are optional: name, icon and launch
command normally come from its desktop entry; custom actions, scroll commands
and drop commands are unset by default.

```toml
[app."org.gnome.Nautilus"]
name = "Files"
icon = "system-file-manager"    # Icon theme name or file path
launch-command = "nautilus --new-window" # Overrides the global launch command
desktop-actions = true          # Show the desktop entry's actions (default)
middle-click = "nautilus --new-window" # Default: launch another instance
scroll-up = "echo up"
scroll-down = "echo down"
drop = "nautilus {files}"       # Dropped files; also accepts {app-id}
actions = [
  { name = "New window", command = "nautilus --new-window", icon = "window-new", when = "always" },
]
```

Each action requires `name` and `command`; `icon` is optional. `when` is
`"always"` (default) or `"running"`. Action commands, middle-click and scroll
commands can use `{app-id}`, `{id}` (most recent window) and `{ids}` (all of
that app's windows).

### Per-folder settings

This customizes a pinned folder; it does not pin it. Paths accept `~/`.

```toml
[folder."~/Downloads"]
sort = "modified"              # modified (newest first), added (newest), name
count = 20                     # Maximum entries; 1–100
view = "list"                  # Currently the only view
# icon = "folder-download"     # Optional icon name or path
```

### Appearance

Colors are quoted `RRGGBB` or `RRGGBBAA` strings, optionally prefixed with `#`.
An alpha of `00` disables the corresponding shadow. All defaults are below.
`icon-theme` is normally detected from desktop settings, falling back to hicolor.

```toml
[appearance]
mode = "auto"                  # auto (desktop preference), dark, light
# icon-theme = "Papirus"
panel-color = "262626b3"
panel-radius = 16
indicator-color = "ffffffb3"
active-indicator-color = "ffffffff"
label-color = "ffffffff"
label-background = "262626e6"
title-color = "ffffffff"
title-background = "000000b3"
placeholder = "3a3a3aff"        # Strip card before its preview arrives
separator-color = "ffffff40"   # Between apps and Trash

shadow-color = "00000066"
shadow-softness = 24
shadow-spread = 0               # Grow/shrink the shadow box; may be negative
shadow-offset = [0, 4]          # [x, y]
icon-shadow-color = "00000059"
icon-shadow-softness = 6
icon-shadow-offset = [0, 2]

menu-background = "262626eb"
menu-border = "ffffff1f"
menu-radius = 12
menu-tail = [10, 7]             # [width, height]
menu-text = "ffffffff"
menu-highlight = "ffffff2e"
menu-highlight-text = "ffffffff"
menu-separator = "ffffff26"
menu-row-height = 28
menu-padding = 6
menu-group-gap = 8
menu-max-width = 320
menu-shadow-color = "00000066"
menu-shadow-softness = 18
menu-shadow-offset = [0, 4]
```

Put any theme keys in `[appearance.dark]` or `[appearance.light]` to override
just that mode, for example:

```toml
[appearance.light]
panel-color = "eeeeeecc"
label-color = "202020"
label-background = "eeeeeeee"
```

