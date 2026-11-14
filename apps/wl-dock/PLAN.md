# wl-dock plan

A macOS-style dock for Wayland, built on the same runtime as wl-expose and
wl-switcher. It shows a row of app icons on a translucent panel with a shadow.
The icons near the pointer magnify together. Pinned apps stay in the dock.
Running apps get an icon too, with every window with the same `app_id` sharing
it. Hovering an app with several windows opens a strip of live previews and
titles.

Unlike the other apps, the dock is always on screen. It has no client/show
cycle and doesn't use `wl-shell-daemon`. `wl-dock` runs its own event loop and
is started with the compositor (e.g. niri's `spawn-at-startup`).

## Behavior

### Items

Rules for which items are shown (`src/dock.rs`, unit tested):

- Pinned apps come first, in their pinned order, whether or not they're
  running.
- Then the running apps that aren't pinned, in the order their first window
  appeared. An app keeps its place while it has windows, so icons don't
  move around when focus changes.
- Every window with the same `app_id` belongs to one item. App IDs are
  matched as is, as in wl-switcher. Windows without an app ID share the item
  `""`, which gets a generic icon.
- A pinned app whose last window closes stays, and loses its running
  indicator. An unpinned one disappears; its neighbors slide together with
  a spring, and the icon shrinks away.

By default every dock shows every window. ext-foreign-toplevel doesn't say
which output a window is on, so `windows = "output"` gets that from a script
instead (see [Which windows a dock shows](#which-windows-a-dock-shows)).

### Pointer

| Input                       | Effect                                                      |
| --------------------------- | ----------------------------------------------------------- |
| hover                       | magnification follows the pointer                           |
| hover an app, `hover-delay` | the strip (≥ `strip-min-windows`) or the app's name         |
| click, one window           | `activate-command` with that window's ID                    |
| click, several windows      | activate the most recent one (MRU), else the first          |
| click, not running          | launch it (see Launching)                                   |
| click a strip card          | activate that window                                        |
| middle click                | the app's `middle-click` action, else a new instance        |
| right click                 | the menu (see [Menu](#menu))                                |
| scroll on an app            | the app's `scroll-up`/`scroll-down` actions, if any         |
| drag an icon                | reorder; drop a pinned icon off the dock to unpin it        |
| drop files on the dock      | (future) see [Folders and files](#future-folders-and-files) |

The dock surface never takes keyboard focus (`KeyboardInteractivity::None`).
Only the open menu does (see [Menu](#menu)).

### Activating windows

The dock doesn't focus windows itself. It runs a configured command with
`sh -c`, the same way wl-switcher leaves focusing to a script:

```toml
activate-command = "niri msg action focus-window --id {id}"
close-command    = "niri msg action close-window --id {id}"    # optional
```

`{id}` is the ext toplevel identifier, which is the window's IPC ID on niri and
umbriel. It is shell-quoted when substituted. Commands run detached (their own
process group, with stdout and stderr going to ours, prefixed), and the dock
never waits for them. Without `activate-command`, clicking a running app prints
a warning once. Menu entries that need `close-command` are hidden when it isn't
set.

To know the most recent window, the dock uses the same `mru-event-stream` as
wl-switcher (`Core::mru`, `Core::use_focus_stream`). It is optional, and with
it the dock can also mark the focused app (`active-indicator`).

### Launching

Launching uses the desktop entry found for the app ID (the worker already loads
entries for icons and names). Its `Exec=` is split into argv, with the field
codes (`%u %f %F %U %i %c %k`) expanded: files dropped on the icon fill
`%f %F %u %U` (future), and otherwise those codes are removed. The command is
spawned detached with `setsid`.
`Terminal=true` entries run under `terminal` (config). A different launcher can
be configured:

```toml
launch-command = "uwsm app -- {desktop-id}"   # or "niri msg action spawn -- {exec}"
```

Placeholders: `{exec}` (the cleaned Exec line), `{desktop-id}` and `{app-id}`.

While an app is launching, its icon **bounces**. A spring keeps it bouncing
until a window with that app ID appears, or for at most `launch-timeout`
(default 10 s).

### Pinning

Pins live in a state file, not in the config, so the dock never rewrites your
TOML:

```
$XDG_STATE_HOME/wl-dock/pinned      (default ~/.local/state/wl-dock/pinned)
```

The file has one item per line, in dock order. Blank lines and `#` comments
are ignored. A line that starts with `/` or `~/` is a path (a folder or a
file, see Future); anything else is an app ID. App IDs never start with
either, so the format stays the same when folders arrive. The file is written
atomically (a temp file, then a rename) whenever pins change in the dock. The
dock watches the directory with inotify and reloads the file when it changes,
so editing it by hand or from a script works. A missing file means no pins.

The CLI only edits the file and needs no socket, because the dock picks the
change up through inotify:

```sh
wl-dock pin firefox
wl-dock unpin firefox
wl-dock pins            # print them
```

## Outputs

By default there is one dock per output (`outputs = "all"`). Settings can be
global and overridden per output, matched by its `wl_output` name:

```toml
position = "bottom"          # bottom | left | right

[output.eDP-1]
position = "left"

[output.HDMI-A-1]
enabled = false              # no dock on this output
```

To only show the dock on some outputs, use `outputs = ["DP-1", "eDP-1"]`.
Override tables accept the layout keys: `enabled`, `position`, `icon-size`,
`magnification`, `reserve`, `autohide`, `margin` and `strip-direction`. Colors
stay global.

This needs output hooks in `wl-shell-core`: `Program::output_new`,
`output_update` (name, scale or mode changed) and `output_removed`. Today the
shell's `OutputHandler` ignores these events. A dock is created when its output
has a name (wl_output v4) and is destroyed with its output.

### Which windows a dock shows

`windows = "all" | "output"` (can be set per output). With `all`, every dock
shows every window. With `output`, a dock only shows the windows on its own
output. The compositor knows which output each window is on, so a script
reports it, the same way `mru-event-stream` reports focus:

```toml
output-event-stream = "~/.config/wl-dock/output-stream.sh"
```

The command runs with `sh -c` for as long as the dock runs and prints
`<output-name> <id>` lines, one whenever a window opens on or moves to an
output. Everything after the first space is the ID, so IDs may contain
spaces. Printing the whole state again is harmless, so a script that only
has snapshots can print every window on every change. Closed windows are
forgotten when the toplevel list reports them closed.

It runs the same way as the focus stream: in its own process group, with
stdout read without blocking, stderr prefixed, restarts with backoff, and a
restart when the command changes. `focus.rs` becomes a general `LineStream`
in core, which both streams use.

Rules (in `dock.rs`, unit tested):

- A dock with `windows = "output"` shows the windows the stream placed on its
  output.
- A window the stream hasn't reported yet is held back for
  `output-grace` (250 ms), so a new window doesn't flash on every dock first.
  After that it shows on all docks until it's reported. If the stream isn't
  running (not configured, or crashed and waiting to restart), every dock
  shows every window.
- Pinned apps show on every dock. A pinned app with windows only on other
  outputs shows a hollow indicator. Clicking it activates its most recent
  window, wherever that is.
- Unpinned apps only show where they have windows.

niri (a sketch, to be checked in phase 4). The script keeps the
workspace→output map and the window→workspace map, and prints every window
whose output may have changed:

```sh
#!/bin/sh
niri msg -j event-stream | jq --unbuffered -r '
  foreach . as $e ({ws: {}, win: {}, out: []};
    if $e.WorkspacesChanged then
      .ws = ($e.WorkspacesChanged.workspaces
             | map({key: (.id | tostring), value: .output}) | from_entries)
      | .out = (.win | keys)
    elif $e.WindowsChanged then
      .win = ($e.WindowsChanged.windows
              | map({key: (.id | tostring), value: (.workspace_id | tostring)})
              | from_entries)
      | .out = (.win | keys)
    elif $e.WindowOpenedOrChanged then
      $e.WindowOpenedOrChanged.window as $w
      | .win[$w.id | tostring] = ($w.workspace_id | tostring)
      | .out = [$w.id | tostring]
    else .out = [] end;
    . as $s | $s.out[] | select($s.ws[$s.win[.]]) | "\($s.ws[$s.win[.]]) \(.)")'
```

The umbriel version depends on whether its `windows` snapshots include the
output.

## Rendering

The same approach as wl-switcher: everything is drawn in software into shm
buffers, with no GPU. Every element is a **sync** subsurface, scaled with
`wp_viewporter` and faded with `wp_alpha_modifier_v1`. Each frame moves and
resizes the subsurfaces and then commits the parent once. Frames are driven
by frame callbacks, and no more are requested once every spring has
settled, so an idle dock costs nothing.

### Surfaces

Each output gets:

```
dock: layer surface (layer `top`, anchored to its edge + both sides)
├─ panel        rounded rect + shadow, 3 slices (ends + stretched middle) so it widens
├─ indicator × running apps
├─ icon × items             (with their shadow baked in, as in wl-switcher)
└─ label                    hovered app's name (worker pill), when no strip

strip: an xdg_popup of the dock surface, only while hovering a multi-window
       app; anchored to the hovered icon
├─ card shadow × windows
├─ preview × windows        (capture buffers, placeholder until the first frame)
└─ title × windows          (worker pills)

menu: an xdg_popup of the dock surface with a grab, while open
├─ bubble      rounded body + tail + border + shadow + separators, one buffer
├─ highlight   rounded rect under the hovered row, slides on a spring
└─ row × entries (worker text, optional icon)
```

- **Dock size.** The surface is as tall (for a bottom dock) as the fully
  magnified icons plus the label and margins. It spans the output's width, so
  magnifying never resizes it. Resizing it would flicker.
- **Reserve.** With `reserve = true`, the exclusive zone is the resting dock's
  height plus `margin`, so windows don't go under the panel. Magnified icons
  overlap windows, as on macOS. With `reserve = false` the zone is 0.
  Autohide forces it to 0.
- **Input region.** The input region covers only the panel and the
  magnified icons, so clicks go through everywhere else. It is updated every
  frame while the magnification moves.
- **Icon sharpness.** Icons are rendered once at `icon-size ×
magnification × scale` and scaled down by the viewport. If that turns out
  to look soft at rest, a second copy is kept at the resting size and the
  closer of the two is used.
- **Popups.** The strip and the menu are xdg_popups of the dock surface
  (`zwlr_layer_surface_v1.get_popup`). The positioner anchors them to the
  icon, and the compositor flips or slides them to keep them on the output,
  so the dock doesn't clamp them itself. They are `reactive`, so they follow
  the icon as the magnification moves it. The menu takes a grab, so a click
  anywhere else dismisses it (`popup_done`). The strip has no grab, since
  hovering mustn't take input. Core gains `xdg_wm_base` and popup events for
  this. If a compositor's popups turn out unusable for the strip, a separate
  layer surface positioned with margins is the fallback.

### Magnification (`src/zoom.rs`, pure, tested)

The icons near the pointer grow together, on a smooth curve. For the item `i`
whose center rests at `cᵢ`, with the pointer at `p` (along the dock's axis, in
resting-layout coordinates):

```
d    = |cᵢ − p| / icon-size
f(d) = cos²(π·d / (2·range))      for d < range, else 0
sᵢ   = icon-size · (1 + (magnification − 1) · f(d) · hover)
```

- `range` is `magnification-range`, in icons (default 3). `hover` is a spring
  from 0 to 1, so magnification grows in as the pointer enters and shrinks
  away as it leaves.
- **Placement.** The magnified icons are laid out back to back with the
  gaps between them. The row is then shifted so the point under the pointer
  stays under the pointer: the resting point `p` maps to the same screen
  position. The hovered icon never slides away from the cursor, and the dock
  grows on both sides. At the ends it grows outward.
- The position along the dock is a function of `p` alone, so it is
  continuous and follows the pointer without lag, as on macOS. Only
  `hover` animates.
- The panel stays as thick as at rest and grows along the dock with the
  row's total length. Icons rise out of it, anchored to the panel's
  inner edge.
- Tests: the row is at rest when `hover = 0`; the layout is continuous in
  `p`; the hovered icon stays under the pointer; icons never overlap;
  the layout is symmetric for a centered pointer; `magnification = 1`
  changes nothing.

While a strip or the menu is open, the magnification freezes at the
hovered item (macOS behaves the same).

### The strip

- It opens after the pointer has rested on an app with at least
  `strip-min-windows` windows (default 2) for `hover-delay`. Below that, the
  app's name label shows instead.
- **Direction.** `strip-direction = "auto" | "vertical" | "horizontal"`.
  `auto` stacks the cards away from the dock edge: a vertical column above a
  bottom dock, a horizontal row beside a side dock. The configured direction
  is used as given otherwise, e.g. a row of cards above a bottom dock.
- **Cards.** The capture's aspect ratio, a fixed height (`strip-card-height`
  of the output for a vertical strip; for a horizontal one, `strip-max-width`
  limits the row's length), and a title pill. The strip is centered on the
  icon and clamped to the output. If it doesn't fit, it scrolls with the
  wheel. A card zooms slightly (`zoom`) when hovered.
- **Opening.** The cards grow out of the icon, staggered by their distance
  from it, using the morph from wl-switcher.
- **Moving the pointer from the icon to the strip.** Leaving the dock starts a
  `close-grace` timer (150 ms). Entering the strip cancels it. Leaving the
  strip starts it again.
- **Captures.** On hover start (before the strip opens, so it never shows
  placeholders), a static capture of the app's windows starts. When the strip
  opens they switch to live, and they are destroyed when it closes.
  `previews = "live" | "static" | "off"`.

### Autohide

`autohide = false | true`. A hidden dock slides off its edge on a spring and
keeps a thin trigger region (`autohide-trigger`, default 2 px) as its input
region. It comes back after the pointer rests on the edge for `reveal-delay`,
and hides `hide-delay` after the pointer leaves, unless a strip or menu is
open. The surface stays mapped the whole time, so revealing is just the
spring. "Intellihide" (hiding only when a window overlaps) needs window
geometry, which the protocols don't provide; see Future.

### Menu

Right-clicking an item opens its menu. The entries, separated into groups,
top to bottom (as on macOS):

1. **Windows.** One row per window title, with the focused one marked.
   Clicking a row activates that window. These rows are left out when the
   app has a single window.
2. **Desktop actions.** The entry's `[Desktop Action …]` groups, e.g. "New
   Private Window", read from the app's `.desktop` file (see below). They run
   like a launch.
3. **App actions** from the config (see below).
4. **Dock.** Keep in Dock / Remove from Dock, New Window (when running), and
   Close / Close All Windows (needs `close-command`).

An empty group is dropped along with its separator. The pointer highlights
rows. The menu also takes keyboard focus while open: Up/Down move, Enter
picks, Escape closes. Picking runs the entry and closes the menu. While the
menu is open, the magnification and the hovered item freeze, and autohide
waits. A menu is a pure list of entries built from the item and config
(`menu.rs`, unit tested); the popup only draws it.

### Menu design

The menu should look like it belongs to the dock: a **speech bubble** that
points at the icon it was opened from.

```
   ╭──────────────────────────╮
   │ • Inbox — Thunderbird    │
   │   Compose                │
   │                          │
   │   New Message            │
   │   Address Book           │
   │   ────────────────────   │
   │   Keep in Dock           │
   │   Close All Windows      │
   ╰───────────╮  ╭───────────╯
                ╲╱
              [icon]
```

- **Shape.** One path: a rounded rectangle (`menu-radius`, default 12 px)
  with a tail (`menu-tail`, default 10 px wide at its base, 7 px tall) on the
  side facing the dock. The tail's base joins the body with small concave
  curves and its tip is slightly rounded, so the outline reads as one smooth
  shape, not a rectangle with a triangle stuck on. The tail points down for a
  bottom dock and sideways for a side dock.
- **Tail position.** The tail points at the icon's center. The compositor may
  slide the popup to keep it on the output, so the tail's offset is computed
  from the popup's configured position relative to the icon, not assumed to
  be centered. It is clamped so it never runs into a rounded corner (at least
  `menu-radius` + 4 px from the end). If the offset changes (a reposition),
  the bubble buffer is redrawn.
- **Surface.** The bubble, its 1 px inner border (`menu-border`, a faint light
  edge as on the panel), the separators and the shadow are drawn into one
  buffer by the worker with tiny-skia, at the output scale, once per open.
  The surface includes the shadow's padding; the input region is the bubble
  path only, so the corners and the shadow aren't clickable. The background
  is translucent (`menu-background`). Where `ext-background-effect-v1` is
  available, the bubble's region is blurred behind it (future).
- **Rows.** `menu-row-height` (default 28 px), `menu-padding` (default 6 px
  around the rows, 12 px of horizontal text inset). An optional 16 px icon
  sits in a fixed column, so the labels of a group line up whether or not
  their rows have icons; the column is dropped when no row in the menu has an
  icon. Long window titles are ellipsized at `menu-max-width` (default 320
  px). The focused window has a small dot in the icon column. Rows that
  can't run (e.g. Close without a window) are not built at all, rather than
  shown dimmed.
- **Natural separators.** Groups are separated mainly by space, not lines:
  extra vertical space (`menu-group-gap`, default 8 px) between groups. Where
  a visible line helps (before the Dock group), it is a hairline inset from
  both sides of the bubble, in a low-contrast `menu-separator` color, fading
  out over its last few pixels at each end so it doesn't hit the edges. A
  separator is never drawn first, last, or twice in a row; that falls out of
  dropping empty groups in `menu.rs`, which emits groups, not separator
  entries.
- **Highlight.** A rounded rect (`menu-radius` − `menu-padding`, so its
  corners follow the bubble's) inset by `menu-padding`, in `menu-highlight`.
  It slides between rows and resizes on a fast spring instead of jumping;
  the hovered row's text switches to `menu-highlight-text`.
- **Opening and closing.** The bubble grows out of the tail's tip: scale from
  0.9 to 1 and fade in on a spring (~120 ms), with the tip fixed, so the menu
  looks like it comes out of the icon. Closing is a quick fade (~80 ms).
  Picking an entry flashes its highlight once before the fade, as macOS
  does.
- **Colors** follow `[appearance.dark]` / `[appearance.light]` like the
  rest of the dock, with defaults chosen so the menu matches the panel.

The bubble shape (path, tail clamping) and the row layout are pure functions
in `menu.rs`, tested: the tail stays inside the straight part of its edge
for any icon position, separators only appear between non-empty groups, and
the menu's size is the same for the same entries at any scale.

### Menu entries from the desktop file

The desktop action rows come from the app's `.desktop` file. It is found
the same way as for the icon and name: `freedesktop-desktop-entry` matches
the app ID against the loaded entries (StartupWMClass, desktop file ID,
Name, Exec, Snap name; case-insensitive). The lookup only needs the app ID,
so it works for a pinned app that isn't running too. When nothing matches,
the group is empty and dropped.

From the matched entry:

- `Actions=` lists the action IDs. The rows follow that order, and IDs
  without a matching `[Desktop Action <id>]` group are skipped.
- Each group gives the row's text from its localized `Name=` (same locales
  as the app name), its icon from `Icon=` (a theme name or a path, resolved
  like app icons; without it the row has no icon), and its command from
  `Exec=`. A group without `Name=` or `Exec=` is skipped.
- Picking a row runs its `Exec=` like a launch: split into argv, field codes
  expanded or removed, `Terminal=true` from the main entry honored, spawned
  detached. `launch-command` is used with `{exec}` set to the action's
  command, and the icon bounces until a new window appears.
- An `[app.<id>]` table can hide them with `desktop-actions = false`.

The worker reads the entries (it already loads them for icons and names) and
returns the actions with the item's icon and name, so opening a menu never
touches the disk. They are reloaded with the entries when an applications
directory changes. `menu.rs` gets them as plain data (name, icon, exec),
which keeps it unit testable with made-up entries.

### App actions

Per-app settings live in `[app."<app-id>"]` tables:

```toml
[app.spotify]
actions = [
  { name = "Play/Pause", command = "playerctl -p spotify play-pause", icon = "media-playback-start" },
  { name = "Next",       command = "playerctl -p spotify next",       icon = "media-skip-forward" },
  { name = "Previous",   command = "playerctl -p spotify previous",   icon = "media-skip-backward" },
]
middle-click = "playerctl -p spotify play-pause"
scroll-up    = "playerctl -p spotify next"
scroll-down  = "playerctl -p spotify previous"

[app."org.gnome.Nautilus"]
icon = "~/.local/share/icons/files.svg"   # an icon name or a path
name = "Files"
launch-command = "nautilus --new-window"
```

- `actions` are the menu's app action rows. `when = "running"` or
  `"always"` (the default) decides whether a row shows for a pinned app that
  isn't running.
- `middle-click`, `scroll-up` and `scroll-down` bind those inputs on the
  icon to a command. Scrolling is rate-limited to one command per
  `scroll-step` (a wheel notch, or about 50 px of touchpad scroll).
- `icon`, `name` and `launch-command` replace what the desktop entry gives.
  `desktop-actions = false` drops the entry's actions from the menu.
- Commands run like the activate command: with `sh -c`, detached, with the
  placeholders `{app-id}`, `{id}` (the most recent window, if any) and
  `{ids}` (all of the app's windows, shell-quoted and separated by spaces).
- Future: `label-command`, whose output (e.g. the current track) becomes the
  row's text when the menu opens.

## Shared code: `crates/wl-shell-ui`

Code that wl-switcher and the dock both use lives in this crate instead of
being copied. Phase 1 moved it there from wl-switcher:

- `anim`: `Spring`, `lerp` (wl-switcher's `anim.rs`, unchanged).
- `geom`: `Rect` (was in wl-switcher's `bar.rs`) and `IRect`, the rounding
  for the protocol (was in `overlay.rs`).
- `sub`: `Sub`, a sync subsurface with a viewport and an alpha modifier
  (was in `overlay.rs`).
- `cache`: the shm buffer cache for icons, labels and shapes, and
  `shm_buffer`/`color_buffer`. Generic over `P: Program` instead of being
  tied to `WlSwitcher`.
- `shapes`: `ShapeKey` (panel with shadow, rounded rect, dots, card shadow
  sides) and their placement: `place_panel` shows a panel in three slices
  along either axis (horizontal for a bottom dock, vertical for a side
  dock), and `place_card_shadow` scales a card's shadow with the card.
- `card`: a preview subsurface with its shadow and title pill, and its
  capture's lifecycle (static, then live, then paused). It moves when the
  dock's strip needs it (phase 7). wl-switcher's `strip.rs` layout is
  centered on the bar and only vertical, so the dock writes its own layout,
  unless a generalized one stays simple.

Core gains:

- `LineStream`: `focus.rs`'s process handling and line reader, with a line
  callback, shared by `mru-event-stream` and `output-event-stream`.
- `spawn`: detached commands with `setsid` and prefixed output, for activating,
  launching and actions.
- Output hooks (see Outputs), xdg popups, and later `wl_data_device` for drag
  and drop.

Desktop entry `Exec=` and desktop action lookup (`Actions=` and the
`[Desktop Action …]` groups' `Name=`, `Icon=` and `Exec=`) go into
`wl-shell-render::icons`, next to `name()`, with unit tests on sample
`.desktop` files.

## Config

`~/.config/wl-dock/config.toml` uses the same `[appearance]`,
`[appearance.dark]` and `[appearance.light]` tables as the other apps.

| Key                   | Default             | Meaning                                       |
| --------------------- | ------------------- | --------------------------------------------- |
| `outputs`             | `"all"`             | `"all"` or a list of output names             |
| `position`            | `"bottom"`          | `bottom`, `left`, `right`                     |
| `layer`               | `"top"`             | `top` or `overlay`                            |
| `icon-size`           | `48`                | px at rest                                    |
| `icon-gap`            | `6`                 | px                                            |
| `margin`              | `6`                 | px from the screen edge                       |
| `padding`             | `6`                 | px inside the panel                           |
| `magnification`       | `1.8`               | hovered icon's scale; 1 disables              |
| `magnification-range` | `3`                 | icons on each side that grow                  |
| `reserve`             | `true`              | exclusive zone for the resting dock           |
| `autohide`            | `false`             |                                               |
| `reveal-delay`        | `150`               | ms                                            |
| `hide-delay`          | `500`               | ms                                            |
| `hover-delay`         | `300`               | ms before the strip or label                  |
| `strip-min-windows`   | `2`                 | fewer windows: the label instead              |
| `strip-direction`     | `"auto"`            | `auto`, `vertical`, `horizontal`              |
| `strip-card-height`   | `0.18`              | of the output height                          |
| `strip-max-width`     | `0.6`               | of the output width                           |
| `previews`            | `"live"`            | `live`, `static`, `off`                       |
| `zoom`                | `1.04`              | hovered card                                  |
| `indicator`           | `"dot"`             | `dot`, `dots` (one per window, max 3), `none` |
| `active-indicator`    | `true`              | mark the focused app (needs the stream)       |
| `labels`              | `true`              | app name on hover                             |
| `response`            | `150`               | ms, spring response                           |
| `activate-command`    | none                | `{id}`                                        |
| `close-command`       | none                | `{id}`                                        |
| `launch-command`      | none                | `{exec}`, `{desktop-id}`, `{app-id}`          |
| `terminal`            | `"foot"`            | for `Terminal=true` entries                   |
| `launch-timeout`      | `10000`             | ms of bouncing at most                        |
| `mru-event-stream`    | none                | as in wl-switcher                             |
| `output-event-stream` | none                | `<output> <id>` lines (see Outputs)           |
| `windows`             | `"all"`             | `all` or `output`                             |
| `output-grace`        | `250`               | ms a new, unplaced window is held back        |
| `open-command`        | `"xdg-open {path}"` | (future) opening files and folders            |
| `[app.<app-id>]`      |                     | per-app actions and overrides (see Menu)      |
| `font`, `font-size`   |                     |                                               |
| `[output.<name>]`     |                     | per-output overrides (see Outputs)            |

`[appearance]` has `panel-color`, `panel-border` (a thin highlight around the
panel, as on macOS), `panel-radius`, `indicator-color`, `label-color`,
`label-background`, `title-color`, `title-background`, `placeholder`, the
`shadow-*` and `icon-shadow-*` keys, `icon-theme`, and the menu's keys (see
[Menu design](#menu-design)): `menu-background`, `menu-border`,
`menu-radius`, `menu-tail`, `menu-text`, `menu-highlight`,
`menu-highlight-text`, `menu-separator`, `menu-row-height`,
`menu-padding`, `menu-group-gap`, `menu-max-width` and `menu-shadow-*`.

The config and the pins file are watched with inotify and reloaded when they
change. A config error keeps the previous config and prints a warning.

## Phases

Every phase only adds to the previous ones. No phase brings in a stand-in that
a later one has to remove, and a phase never takes away a feature an earlier
one shipped.

0. **Scaffold (done).** The crate, CLI, config, the item model (`dock.rs`:
   running apps grouped by app ID), and the program that keeps the items up
   to date from the toplevel list. It has 7 tests. No pinning.
1. **Shared crate (done).** Extract `wl-shell-ui` (`anim`, `geom`, `cache`,
   `shapes`, `sub`) and port wl-switcher to it. Exit criterion: wl-switcher
   behaves the same and all its tests pass.
2. **A static dock (done).** Output hooks in core; one layer surface per output,
   with per-output config; the panel, shadow, icons of the running apps and
   their indicators; `reserve`; config and theme reload. Exit criterion: a
   correct dock on every output that survives hotplug and scale changes.
3. **Magnification (done).** `zoom.rs`, the hover spring, the input region, the
   frame loop, the hover label, and animating items as they appear and
   disappear. Exit criterion: smooth at the monitor's refresh rate.
4. **Actions.** `spawn` and `LineStream` in core, `activate-command`, MRU and
   the active indicator, and `output-event-stream` with `windows = "output"`.
   Exit criterion: daily-drivable as a switcher on niri, with per-monitor
   docks.
5. **Pinning and launching.** Pinned items in `dock.rs` (pinned first, kept
   when not running, on every dock, with the hollow indicator for windows on
   other outputs); the pins state file with inotify; the `pin`/`unpin`/`pins`
   CLI; launching from desktop entries, `launch-command`, the bounce and
   middle click. Exit criterion: daily-drivable as a launcher too.
6. **Menu and app actions.** Xdg popups in core, `menu.rs`, the speech
   bubble menu (shape, tail, separators, sliding highlight, open/close
   animation), keyboard navigation, desktop actions read from the `.desktop`
   file, `[app.<id>]` (actions, overrides, middle click and scroll), and Keep
   in Dock / Remove from Dock. Exit criterion: the menu looks finished on
   bottom and side docks, at scale 1 and 2, and its tail points at the icon
   even when the compositor slides it.
7. **The strip.** Hover delay, the strip popup, cards and titles, captures
   (static then live), direction, the close grace period, and the cards
   growing out of the icon. Move `card` into `wl-shell-ui`. Exit criterion:
   hovering an app with 10 windows feels instant.
8. **Dragging icons.** Reorder, and drag off the dock to unpin, with the
   neighbors making room on springs. This uses pointer grabs on the dock
   surface; `wl_data_device` isn't needed yet.
9. **Autohide.** Trigger region, delays and the slide.
10. **Polish.** README, measurements (idle CPU, frame times while
    magnifying), and an icon sharpness pass.

## Future: folders and files

Folders can be pinned like apps. They come after a separator at the end of
the dock, as on macOS.

- **Adding a folder.** Drop it on the dock, or add its path to the pins file
  (by hand, or with `wl-dock pin ~/Downloads`). Its options go in the config,
  keyed by path the same way apps are:

  ```toml
  [folder."~/Downloads"]
  sort  = "modified"   # modified | added | name
  count = 20           # entries shown
  view  = "stack"      # stack (fan) | grid | list
  icon  = "folder-download"
  ```

- **The icon** is the folder's theme icon (`folder-download` and so on for
  the XDG user dirs, else `inode-directory`). Optionally it shows the newest
  files' icons stacked.
- **Clicking** opens a popup with the `count` most recent entries (by mtime),
  using the same popup machinery as the strip and menu. Entries show a
  thumbnail from the freedesktop thumbnail cache
  (`$XDG_CACHE_HOME/thumbnails/{normal,large}`; the dock only reads it and
  never generates thumbnails) or the mime type's theme icon, and the name.
  The worker does the listing, inotify keeps it current, and the popup
  ends with "Open in file manager".
- **Opening** a file or folder runs `open-command` (default `xdg-open
{path}`).
- **Drag and drop** needs `wl_data_device` in core, with `text/uri-list` both
  ways:
  - Dragging out of the folder popup offers the file (copy or move, as
    negotiated with the drop target), with its icon as the drag icon.
  - Dropping on an app icon opens the files with that app (the `%f %F %u %U`
    of its Exec, or `launch-command` with `{files}`). The icon highlights
    while a drop is accepted. A running app could instead get the files as
    an app action (`[app.x] drop = "..."`).
  - Dropping on a folder copies the files into it, or moves them, as
    negotiated.
  - Dropping between icons pins a dropped folder or `.desktop` file, with the
    neighbors making room on springs.
- **Later:** a Recent Files item (from `recently-used.xbel`) and a Trash item
  (drop to trash with `gio trash`, click to open it).

## Future

- **Intellihide.** Hide only when a window overlaps the dock. This needs
  window geometry from the compositor's IPC, through a line stream like
  `output-event-stream`.
- Scrolling on an icon to cycle through its windows (when no `scroll-*`
  actions are bound), badges, and a `top` position.

## Decisions

- **No daemon crate.** The dock is always on screen and runs its own loop.
  Pinning goes through a file and inotify, so there is no socket.
- **The compositor's IPC does the focusing.** `activate-command` with the
  ext identifier, as wl-switcher's scripts do.
- **Pins live in a state file**, in `$XDG_STATE_HOME/wl-dock/pinned`, never
  in the config.
- **One pin list for all docks.** There is no pinning per output. Docks
  differ only in which running windows they show (`windows`).
- **`reserve` is a boolean.** True means the zone is exactly the resting
  dock's height plus `margin`.
- **Which output a window is on comes from a script** (`output-event-stream`),
  the same way focus order does. Without one, every dock shows every window.
- **The strip and the menu are xdg_popups** of the dock surface, so the
  compositor positions them and dismisses the menu.
- **Per-item settings are keyed by app ID or path** (`[app.<id>]`,
  `[folder.<path>]`). The pins file holds only the order.
- **Grouping key.** `app_id` as is, the same as wl-switcher.
- **Shared code goes into `wl-shell-ui`.** It is never copied between apps.
