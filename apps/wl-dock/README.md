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

### Launcher

`wl-dock --launcher` turns a bottom dock into a search bar for apps. Bind it to
a key, for example on niri:

```kdl
Mod+Space { spawn "wl-dock" "--launcher"; }
```

It opens on the focused output, whose dock must be at the bottom. Type to search
the installed apps by name, generic name, keywords, desktop ID and command;
acronyms work (`vsc` finds Visual Studio Code), and apps you open often rank
higher. The best matches stand on a drum above the bar: Up or Ctrl+N rolls the
next one to the front, Down or Ctrl+P rolls back, and scrolling rolls it too.
Enter activates the selected app's most recent window, or launches it if it
isn't running; Ctrl+Enter does the other. Backspace, Ctrl+W and Ctrl+U delete a
character, a word or everything. Escape clears the query, then closes the
launcher, as do a click outside it and running `--launcher` again. It exits 0
once something was opened and 1 otherwise.

Launches are recorded in `$XDG_STATE_HOME/wl-dock/history` to rank matches,
and so is the app you picked for a query: it comes first for that query (and
the start of it) next time. `launcher.history = false` turns both off.

Start the query with `'` to search files instead, as in Alfred: `'plan`
finds `PLAN.md`. There is no index; the launcher walks your home directory
while you type and the best matches stream onto the drum, each with its
thumbnail or type icon and its folder. The query uses
[nucleo's syntax](https://github.com/helix-editor/nucleo): spaces separate
terms, `^` and `$` anchor, `!` excludes, and `'` asks for an exact substring
(`''file.pdf`). Enter opens the selected file or folder with
`files.open-command`, and Ctrl+Enter opens its folder. Backspace on an empty
file query, or Shift+Tab, goes back to the apps. Hidden files, `.gitignore`d
files and `launcher.files.exclude` are skipped, symlinks aren't followed, and
the walk stays on one file system.

Tab on a file shows what you can do with it: its icon flies into a small pill
above the bar's right end, with its name and folder, and the drum fills with
Open file, Open with… and Open containing directory. Type to filter them (`ow`
finds Open with…) and press Enter to run the front one. Open with… (Enter or
Tab) lists the apps that take the file's type, from their `MimeType=` and your
`mimeapps.list`, the default first, and adds its icon to the pill; Enter opens
the file with the front one. Escape, Shift+Tab, or Backspace on an empty query
go back a step, to where you were; clicking an icon in the pill goes back to
where it was chosen.

## Configuration

Edit `~/.config/wl-dock/config.toml` (or
`$XDG_CONFIG_HOME/wl-dock/config.toml`). Use `wl-dock --config PATH` for another
file. Every setting is optional. The reference below shows defaults; commented
commands are optional examples, not enabled defaults. Sizes are pixels and
times are milliseconds.

To start from a template, copy [`config.example.toml`](config.example.toml),
where every setting is commented out with its default:

```sh
mkdir -p ~/.config/wl-dock
cp apps/wl-dock/config.example.toml ~/.config/wl-dock/config.toml
```

General dock settings stay at the root, before any table header. Feature
settings live in the tables below; the old flat spellings are not supported.

### Dock, autohide, windows and launcher

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
hover-delay = 300
labels = true
indicator = "dot"               # dot, dots (up to three), none
active-indicator = true         # Needs mru-event-stream
drag-threshold = 6              # Pixels before an icon starts dragging; 1–100

[autohide]
enabled = false
reveal-delay = 150
hide-delay = 500
trigger = 2                    # Edge reveal band; 1–50

[windows]
scope = "all"                  # all, output (needs output-event-stream)
output-grace = 250             # Wait for the stream to place new windows
strip = true                    # Click an app to show its windows
min-windows = 2                # Below this, activate its most recent window
max-columns = 3                # Along the dock; 0 means unlimited
max-rows = 3                   # Away from the dock; 0 means unlimited
card-height = 0.18             # Fraction of output height
max-width = 0.6                # Fraction of output length; also caps card width
previews = "live"               # live, static (one frame), off
zoom = 1.04                     # Highlighted card scale; 1 disables it

[launcher]
width = 0.4                    # Fraction of output width; at least the dock
icon-scale = 1.25              # Selected app's icon, times icon-size; 1–3
# font-size = 20               # Default: from icon-size
matches = 5                    # Matches on the drum; 0–12, 0 shows only one
far-scale = 0.5                # Farthest match's size; 0.25–1
enter = "activate"             # activate, launch (Ctrl+Enter does the other)
history = true                  # Record launches to rank matches

[launcher.files]
trigger = "'"                  # Typed first, searches files; "" turns it off
root = "~"                     # The directory searched
exclude = ["node_modules", "target", "__pycache__"] # Gitignore-style globs
hidden = false                 # Include hidden files and directories
gitignore = true               # Skip what .gitignore, .ignore and git exclude
max = 1000000                  # Entries searched at most; 1–100000000
```

### Commands and desktop integration

Window activation and closing use your compositor's IPC. Set commands that
accept its `ext-foreign-toplevel` window IDs (translate them if necessary).
Without `close-command`, the menu has no Close entries. Commands run through
`sh -c`.

```toml
[integration]
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
  a window opens on or moves to an output. Used by `windows.scope = "output"`.

### Files, folders and Trash

```toml
[trash]
enabled = true
command = "gio trash -- {paths}"
empty-command = "gio trash --empty"

[files]
open-command = "xdg-open {path}" # Trash uses trash:/// as its path
copy-command = "cp -rn -- {paths} {dest}"
move-command = "mv -n -- {paths} {dest}"
```

`trash.command`, `files.copy-command` and `files.move-command` handle files
dropped onto the Trash or a folder. `{paths}` is the source list; `{dest}` is
the destination folder. Install `gio` for the default Trash commands.

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
margin = 6

[output."eDP-1".autohide]
enabled = false

[output."eDP-1".windows]
scope = "all"
max-columns = 3
max-rows = 3
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
view = "list"                  # list (a menu), or stack (large icons fanned out of the folder)
# icon = "folder-download"     # Optional icon name or path
```

### Appearance

Colors are quoted `RRGGBB` or `RRGGBBAA` strings, optionally prefixed with `#`.
An alpha of `00` disables the corresponding shadow. All defaults are below.
`icon-theme` is normally detected from desktop settings, falling back to hicolor.

```toml
[appearance]
font = "sans-serif"
font-size = 13
response = 150                  # Animation spring response; 1–10000
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

launcher-background = "262626e6"
launcher-text = "ffffffff"
launcher-completion = "ffffff8c" # The rest of the selected name, and the placeholder
launcher-match = "81d0ffff" # What the query matched
launcher-caret = "ffffffcc"
```

Put any theme keys in `[appearance.dark]` or `[appearance.light]` to override
just that mode, for example:

```toml
[appearance.light]
panel-color = "eeeeeecc"
label-color = "202020"
label-background = "eeeeeeee"
```

