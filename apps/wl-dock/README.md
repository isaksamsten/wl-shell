https://github.com/user-attachments/assets/3ff2300a-503c-4ec4-87d3-740abe513767

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

### Launcher

https://github.com/user-attachments/assets/7cb773d0-a193-48e1-876c-1e897a82b2c6

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
launcher, as do a click outside it and running `--launcher` again.

Launches are recorded in `$XDG_STATE_HOME/wl-dock/history` to rank matches,
and so is the app you picked for a query: it comes first for that query (and
the start of it) next time. `launcher.history = false` turns both off.

Start the query with `'` to search files instead, as in Alfred: `'plan`
finds files matching that. The query uses
[nucleo's syntax](https://github.com/helix-editor/nucleo): spaces separate
terms, `^` and `$` anchor, `!` excludes, and `'` asks for an exact substring
(`''file.pdf`). Enter opens the selected file or folder with
`files.open-command`, and Ctrl+Enter opens its folder. Backspace on an empty
file query, or Shift+Tab, goes back to the apps. Hidden files, `.gitignore`d
files and `launcher.files.exclude` are skipped, symlinks aren't followed, and
the walk stays on one file system.

Tab on a file or application shows what you can do with it.

Start the query with `@` to search every open window by title or app name, on
any output. The drum shows each window's capture, and `@` followed by Enter goes
to the previous window. Enter activates the window with `activate-command`; Tab
offers more, such as Close Window with `close-command`.

To enable web search, copy the engines you want from `config.example.toml`.
Type `!` to search engines by name or keyword, then press Tab to enter the
selected engine.

Set `fallback` in `[launcher.web]` to an engine's keyword and a query that
matches no app becomes a web search with that engine: Enter opens it in the
browser. Alt+Enter searches the web for the query whatever is in front, and
`show = "always"` also puts the search last on the drum behind the matches.
With `fallback-suggest = true` the engine's suggestions fill the drum behind
the search, so it is off by default.

### Keyboard activation

https://github.com/user-attachments/assets/538fb920-f13e-497d-9d29-795251ed81d4

Holding Super (or another modifier), the number keys reach the first ten dock
items, as on the Windows taskbar. Bind them to `wl-dock --activate N`, for
example on niri:

```kdl
Mod+1 { spawn "wl-dock" "--activate" "1"; }
Mod+2 { spawn "wl-dock" "--activate" "2"; }
// …
Mod+0 { spawn "wl-dock" "--activate" "0"; }
```

Items are numbered 1–9, then 0, in dock order: the apps, then the folders.
Super+N acts on item N: an app with one window is picked (nothing switches
until you let go); an app with several opens its strip on its
previous window, and each further press of N moves to the next one, most
recently used first (Shift+N goes back); an app that isn't running launches; a
folder opens its popup with the first entry highlighted, and N again moves
down. Another number switches to its item. Letting go of the modifier
activates the picked or highlighted window, or opens the highlighted entry;
Escape closes without activating, Enter activates at once, and the arrow keys
move the highlight.

`wl-dock --activate` without a number only shows the numbers, for compositors
that can bind the modifier alone. The command exits as soon as the dock has the
request.

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
output = "all"                  # all, or an output pattern (see Outputs)
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

[activate]
badges = true                  # Number the items during --activate
delay = 150                    # Before anything shows; 0–2000

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

[launcher.windows]
trigger = "@"                  # Typed first, searches windows; "" turns it off
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
open-terminal-command = "cd {dir} && exec foot"  # Open terminal here; "" hides it
launch-timeout = 10000          # Maximum launch bounce; 0–60000
# mru-event-stream = "~/.config/wl-dock/focus-stream.sh"
# output-event-stream = "~/.config/wl-dock/output-stream.sh"
```

- `activate-command` and `close-command`: `{id}` is the window ID.
- `launch-command`: `{exec}`, `{desktop-id}` and `{app-id}` are available.
  By default, apps launch using their desktop entry's `Exec=`.
- `terminal`: the app's command is appended; include any needed flag, such as
  `"alacritty -e"`.
- `open-terminal-command`: the launcher's Open terminal here, with `{dir}` the
  folder (shell quoted). Run with `sh -c`; `""` removes the action.
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

### Outputs

`output = "all"` puts a dock on every enabled output. `output = "<pattern>"`
puts it only on the output the pattern matches (on the first output while that
one isn't connected). A pattern is, in order of preference:

1. a connector name, exactly: `eDP-1`
2. a monitor's description or `<make> <model>`, exactly: `"Dell Inc. DELL U2720Q"`
3. a glob (`*`, `?`, `[…]`) over those: `"Dell Inc. DELL U2720Q*"`, `"*"` for all

Run `wl-dock outputs` to list your outputs, what to write for each, and what
the config makes of them. The same patterns and tables work in every wl-shell
app that lives on outputs; see [PLAN-outputs.md](../../PLAN-outputs.md).

An `[outputs.<pattern>]` table applies to every output it matches, least
specific first, key by key, so `"*"` holds defaults. Outputs are enabled unless
a table says `enabled = false`. These are **all** the keys a table can set:

```toml
[outputs."eDP-1"]
enabled = true
position = "bottom"
icon-size = 48
magnification = 1.8
reserve = true
margin = 6

[outputs."eDP-1".autohide]
enabled = false

[outputs."eDP-1".windows]
scope = "all"
max-columns = 3
max-rows = 3
```

For example, to show the dock only on one monitor, whichever connector it is
on, disable `"*"` and enable the monitor:

```toml
# [outputs."*"]
# enabled = false
#
# [outputs."Dell Inc. DELL U2720Q*"]
# enabled = true
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

### Widgets

A widget is a Lua 5.4 plugin behind an app's item. It can replace the app's
label (its name on hover) and its icon, make its tile longer along the dock,
and show an interactive popup while the item is hovered. Clicking the item
still launches or activates the app.

```toml
[app.foot]
widget = { plugin = "clock", format = "%H:%M:%S", face = true }  # or just widget = "clock"
popup-delay = 400             # ms of hover before the widget's popup (default: [widgets])

[plugin.clock]                 # Optional: defaults for every placement
# path = "~/src/clock.lua"     # Default: plugins/clock.lua next to the config, else bundled
format = "%H:%M"

[widgets]
enabled = true                 # Default
popup-delay = 300              # Default
budget = 1000000               # Lua instructions per hook call (default)
```

A plugin is a file that returns a table of hooks:

```lua
local P, n = {}, 0
function P.setup(opts) dock.every(1000, function() n = n + 1; dock.redraw() end) end
function P.label() return 'Up ' .. n .. 's' end
return P
```

`setup(opts)` gets the plugin's options; `label()` returns a string, or `nil`
for the app's own name, and is asked again after `dock.redraw()`. Host
functions: `dock.redraw()`, `dock.timer(ms, fn)` and `dock.every(ms, fn)`
(handles with `:cancel()`), `dock.spawn(argv, { on_line = fn, on_exit = fn })`
(a handle with `:kill()`), `dock.run(argv)` (detached), `dock.read(path)`,
`dock.json.decode(s)` / `dock.json.encode(v)` and `dock.clipboard(text)`;
`argv` is a list, or a string for `sh -c`. Plugins have no `io`, `debug` or
`os.execute`; `require` loads modules next to the plugin. A hook that runs
past its budget, or a failing `setup`, restarts the plugin with backoff, and the
app shows its own name meanwhile. Plugin files reload when saved.

`icon(tile)` returns a UI tree drawn as the app's icon (or `nil` for the
app's own). `tile` has `w` and `h` (the magnified size in logical pixels),
`span`, `axis` (`'horizontal'` or `'vertical'`) and `scale`. A plugin makes its
tile longer with `span = 2` in its table, or `dock.span(1.5)` at run time
(1, 1.5 or 2 icons; `{ horizontal = 2, vertical = 1 }` per dock direction).

```lua
local ui = dock.ui
local P, unread = {}, 3
function P.icon(tile)
  return ui.stack{ align = 'top-right',
    ui.app_icon{},                      -- the app's own icon, underneath
    ui.box{ variant = 'badge', ui.text{tostring(unread), style = 'caption'} },
  }
end
return P
```

Trees are built from `dock.ui` constructors: props are named, children
positional (`nil` children are skipped).

| Constructor                                   | Props                                                                                                                                                                  |
| --------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ui.col{}`, `ui.row{}`                        | `gap`, `padding` (`8`, `{v, h}` or `{t, r, b, l}`), `align` (`start`, `center`, `end`, `stretch`), `justify` (`start`, `center`, `end`, `between`, `around`, `evenly`) |
| `ui.grid{}`                                   | `columns` (a count, or sizes: `40`, `'1fr'`, `'auto'`), `gap`, `row_gap`                                                                                               |
| `ui.stack{}`                                  | `align` (`center`, `top-right`, `bottom`, ...); the first child is the bottom layer                                                                                    |
| `ui.box{}`                                    | `variant` (`card`, `inset`, `badge`)                                                                                                                                   |
| `ui.scroll{}`                                 | `max_height`                                                                                                                                                           |
| `ui.spacer{}`, `ui.divider{}`                 | `size`                                                                                                                                                                 |
| `ui.text{'...'}`                              | `style` (`title`, `body`, `muted`, `caption`), `weight`, `color`, `size`, `align`, `max_lines`                                                                         |
| `ui.icon{'name'}`                             | `size`, `color` (`-symbolic` icons take the text color)                                                                                                                |
| `ui.app_icon{}`                               | `app` (default: the widget's), `size`                                                                                                                                  |
| `ui.image{src = path}`                        | `fit` (`cover`, `contain`, `fill`), `radius`, `blur`, `opacity`                                                                                                        |
| `ui.canvas{draw = fn}`                        | `viewbox = {w, h}` (default 100 x 100)                                                                                                                                 |
| `ui.button{}`                                 | `label`, `icon`, `variant` (`plain`, `filled`, `ghost`), `enabled`, `selected`, `on_click`                                                                             |
| `ui.slider{}`, `ui.toggle{}`, `ui.progress{}` | `value`, `min`, `max`, `enabled`, `live`, `on_change`, `on_commit`                                                                                                     |

Every node also takes `width`, `height`, `min_*`, `max_*` (pixels or `'50%'`),
`grow`, `background`, `radius`, `border`, `opacity`, `hover` (props while
hovered) and `key`. Colors are CSS colors or the theme's names in
`dock.theme` (`text`, `muted`, `accent`, `accent_text`, `surface`, `border`,
`hover`, `pressed`, `track`, `divider`, `card`, `inset`, `badge`,
`badge_text`), which follow the dock's appearance, light and dark. A canvas's
`draw(c)` runs when the tree is built; `c` has `line`, `rect`, `circle`, `arc`
(`from`, `to` in radians from 3 o'clock, `pie`), `path{d = 'M ...'}`, `text`,
`save`, `restore`, `translate`, `rotate` and `scale`, each shape with `fill`,
`stroke`, `width` and `cap`, in viewbox units.

`popup()` returns a tree for the item's popup (or `nil` for none). It opens
after the item has been hovered for its `popup-delay`, in a speech bubble
pointing at the item like the menu, and at once when the pointer moves from
one open popup to another widget. `on_open()` and `on_close()` are called as
it opens and closes, and it is asked again after `dock.redraw()`. Hover,
pressed looks, slider drags and scrolling are handled by the dock; the
plugin hears `on_click()`, `on_change(value)` (at most 30 times a second
while a slider is dragged), `on_commit(value)` (on release) and a toggle's
`on_change(on)`. A slider or progress bar with `live = { rate = 1, at =
os.time() }` moves on its own from `value` at `at` (seconds) by `rate` per
second, so a seek bar keeps up without a redraw; it is painted on its own
layer, as often as that moves it a pixel.

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

badge-color = "1f1f1fff"        # Item numbers during --activate
badge-background = "ffffffe6"
```

Put any theme keys in `[appearance.dark]` or `[appearance.light]` to override
just that mode, for example:

```toml
[appearance.light]
panel-color = "eeeeeecc"
label-color = "202020"
label-background = "eeeeeeee"
```
