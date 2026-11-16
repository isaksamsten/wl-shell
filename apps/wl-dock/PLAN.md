# wl-dock plan

A macOS-style dock for Wayland, built on the same runtime as wl-expose and
wl-switcher. It shows a row of app icons on a translucent panel with a shadow.
The icons near the pointer magnify together. Pinned apps stay in the dock.
Running apps get an icon too, with every window with the same `app_id` sharing
it. Clicking an app with several windows opens a strip of live previews and
titles, laid out next to the icon so every window is a short move away.

Besides apps, the dock holds folders, and a Trash for removing
items from the dock by dragging. An Applications item opens a grid of every
installed app, one page per category, that you swipe between. And
`wl-dock --launcher` turns a bottom dock into a Quicksilver/Alfred-style
search bar for apps, with the best matches rolling around the dock.

Unlike the other apps, the dock is always on screen. It has no client/show
cycle and doesn't run under `wl-shell-daemon`'s loop. `wl-dock` runs its own
event loop and is started with the compositor (e.g. niri's
`spawn-at-startup`). It only borrows the daemon's socket code, so that
`wl-dock --launcher`, `--apps` and `--activate` reach the running dock (see
[Talking to the dock](#talking-to-the-dock)).

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

| Input                       | Effect                                                        |
| --------------------------- | ------------------------------------------------------------- |
| hover                       | magnification follows the pointer                             |
| hover an app, `hover-delay` | the app's name                                                |
| click, one window           | `activate-command` with that window's ID                      |
| click, several windows      | open the strip (`strip = false`: activate the MRU window)     |
| ctrl+click                  | activate the most recent window (MRU), else the first         |
| click, not running          | launch it (see Launching)                                     |
| click a strip card          | activate that window and close the strip                      |
| click the icon, strip open  | close the strip                                               |
| middle click                | the app's `middle-click` action, else a new instance          |
| right click                 | the menu (see [Menu](#menu))                                  |
| scroll on an app            | the app's `scroll-up`/`scroll-down` actions, if any           |
| drag an icon                | reorder (see [Dragging](#dragging-and-the-trash))             |
| drop an icon on the Trash   | remove it from the dock                                       |
| drop files on the dock      | open, pin or trash them (see [Drag and drop](#drag-and-drop)) |
| click Applications          | the app grid (see [Applications](#applications))              |

The dock surface's keyboard interactivity is `on-demand`, only so that it
can see Ctrl on a click (see [Ctrl+click](#ctrlclick)); it handles no keys
itself. The open strip and menu take the keyboard (see [Menu](#menu)).

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
`%f %F %u %U` (see [Drag and drop](#drag-and-drop)), and otherwise those
codes are removed. The command is spawned detached with `setsid`.
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
are ignored. A line that starts with `/` or `~/` is a folder (see
[Folders](#folders)); anything else is an app ID. App IDs never start with
either, so the format holds both. Only folders are pinned: a path line that
names a file is skipped with a warning, and `wl-dock pin` refuses files.
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
`magnification`, `reserve`, `autohide`, `margin`, `strip-max-columns` and
`strip-max-rows`. Colors stay global.

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

niri. The script keeps the workspace→output map and the window→workspace
map, and prints every window whose output may have changed. Checked against
sample events of the niri IPC's shape; note `jq -n` with `inputs`, since
without `-n` jq runs the program once per event and keeps no state:

```sh
#!/bin/sh
niri msg -j event-stream | jq -n --unbuffered -r '
  foreach inputs as $e ({ws: {}, win: {}, out: []};
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

umbriel's `windows` snapshots include the output, so printing every
snapshot whole is enough:

```sh
#!/bin/sh
umbriel subscribe windows | jq --unbuffered -r '.data[] | "\(.output) \(.id)"'
```

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
├─ separator                between the apps and the folders and Trash
├─ dragged icon             while dragging (phase 9)
├─ badge × numbered items   during keyboard activation (worker pills)
└─ label                    hovered app's name (worker pill), when no strip

strip: an xdg_popup of the dock surface with a grab, while an app's strip
       is open; anchored to the icon
├─ card shadow × windows
├─ preview × windows        (capture buffers, placeholder until the first frame)
└─ title × windows          (worker pills)

menu: an xdg_popup of the dock surface with a grab, while open
├─ bubble      rounded body + tail + border + shadow + separators, one buffer
├─ highlight   rounded rect under the hovered row, slides on a spring
└─ row × entries (worker text, optional icon)

launcher: one layer surface in all, on the focused output, kept only if
          that output's dock is at the bottom (layer `overlay`, exclusive
          keyboard, the whole output, transparent outside the bar and the
          matches), while open
├─ bar         rounded rect + shadow, morphs from the dock's panel
├─ match × top matches  icons on the drum; the front one is the selection,
│                       and one on the drum's back is stacked below the bar
└─ query, completion, caret

activate: one 1×1 layer surface in all, on the output the compositor
          picks (exclusive keyboard, empty input region, nothing drawn),
          during keyboard activation

apps: an xdg_popup of the dock surface with a grab, while open; anchored
      to the Applications item
├─ background  rounded rect + shadow
├─ header × categories, and the header highlight
├─ highlight   under the selected cell
└─ cell × apps on the visible pages (icon + name)
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
  the icon as the magnification moves it. The strip and the menu take a
  grab, so a click anywhere else dismisses them (`popup_done`). Core gains
  `xdg_wm_base` and popup events for
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

Phase 7 opened the strip on hover and, by default, stacked its cards in a
column above a bottom dock. Neither worked in use. Moving across the dock
opened strips nobody wanted, and keeping one open meant keeping the pointer
on a path from the icon to it. With cards 18% of the output tall, the
fourth window of a column was about 70% of the screen away from the icon.
Phase 8 replaces that: the strip opens on click only, and its cards form a
grid next to the icon.

#### Opening and closing

- Clicking an app with at least `strip-min-windows` windows (default 2)
  opens its strip. With `strip = false`, the click activates the most
  recent window instead, and the strip never opens.
- Ctrl+click always activates the most recent window directly.
- Hovering never opens the strip. It only shows the app's name label, for
  multi-window apps too.
- Clicking the icon again, clicking outside, or Escape closes the strip.
  Clicking another icon closes it and acts on that icon as usual
  (activate, launch, or open its strip): the popup grab still delivers
  clicks on the client's own surfaces to them.
- Clicks act on the button's release, as now, so a press that turns into a
  drag (phase 9) never opens the strip.

#### Grab and keyboard

The strip is a grabbing popup, like the menu. The pointer can wander
anywhere without closing it, so there is no grace timer and no path to keep
(phase 7's `close-grace` goes away). It also takes keyboard focus: the arrow
keys move a highlight between the cards, Enter activates, `1`–`9` pick a
card, and Escape closes. The highlighted card zooms (`zoom`) like a hovered
one, and the pointer and the keys move the same highlight. It starts on the
most recent window that isn't focused, so click and then Enter switches to
the app's previous window.

#### Ctrl+click

`wl_pointer.button` carries no modifiers; they only come to a surface with
keyboard focus (`wl_keyboard.modifiers`, sent after `enter`). So the dock's
layer surface becomes `on-demand`: clicking it gives it keyboard focus, and
the modifiers arrive with that. Since clicks act on release, the modifiers
are known by then. Focus is not kept, though: after acting on a click that
doesn't open a popup, the dock sets its interactivity to `none` (which
drops focus) and back to `on-demand` when the pointer next enters. The
click usually activates or launches a window, which takes focus anyway.
This has to be checked on niri: that the keyboard `enter` and `modifiers`
arrive before the release, and that dropping focus returns it to the
previous window. If modifiers can't be seen, Ctrl+click acts as a plain
click; the menu's window rows still reach any window.

#### Layout

The pointer starts on the icon, so what matters is how far the cards are
from it. A column away from the dock adds a card height of travel per
window. One row along the dock puts the far ends half a row away. A grid
next to the icon keeps the farthest card closest, growing with about √n.
(Arcs around the icon and ordering by recency were considered and
rejected. Cards of different aspect ratios don't tile on an arc, it runs
off the output near the edges, and the titles don't line up. Ordering by
recency reshuffles the cards on every open, so their positions can't be
learned.)

Rows and columns are relative to the dock, not the screen:

- A **row** runs along the dock's edge. The first row is next to the dock
  and centered on the icon, and later rows stack away from the dock. Above
  a bottom dock rows are horizontal. Beside a left or right dock they are
  vertical, and the second row is farther from the edge than the first.
- **Columns** are the cards per row.
- `strip-max-columns` (default 3) and `strip-max-rows` (default 3) cap the
  grid, and 0 means no limit. `strip-max-rows = 1` gives a single line along
  the dock, and `strip-max-columns = 1` a single line away from it (phase
  7's `vertical` above a bottom dock). These replace `strip-direction`.
- **Filling.** Cards keep the window order (the order the windows appeared,
  as items do). They fill a row up to `strip-max-columns`, then the next row
  out from the dock, so n cards make ⌈n/columns⌉ rows. The last, partial
  row is centered. When n is more than columns × rows, the row cap wins:
  the rows hold ⌈n/rows⌉ cards each, and the columns cap gives way.
- **Size.** All cards share one height, `strip-card-height` of the output
  at most. A card's width follows the capture's aspect ratio, and each card
  has a title pill. Cards shrink (the existing bisection, down to
  `MIN_CARD`) to fit `strip-max-width` along the dock and the space between
  the dock and the output's far edge across it. If they still don't fit,
  the strip scrolls with the wheel: along the dock when the rows are capped,
  else away from it.
- **Opening.** The cards grow out of the icon, staggered by their distance
  from it, using the morph from wl-switcher.
- **Captures.** When the pointer enters a multi-window icon, a static
  capture of its windows starts, so a click usually finds the frames
  ready. A card whose frame isn't there yet shows the placeholder and
  fades the frame in. When the strip opens the captures switch to live,
  and they are destroyed when it closes.
  `previews = "live" | "static" | "off"`.

Tests (in `strip.rs`): the fill order and row counts for given caps; the
row cap winning over the columns cap; 0 meaning no limit; the first row
touching the dock side, centered on the icon, on bottom, left and right
docks; the last row centered; zoomed cards never overlapping; scrolling in
the right direction; and hit testing following the grid, also while
scrolled.

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
  command (only if it uses `{exec}`; see phase 6), and the icon bounces
  until a new window appears.
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

## Dragging and the Trash

### Reordering

- A press on an icon that moves more than `drag-threshold` (default 6 px)
  before its release starts a drag; a shorter one stays a click. Clicks
  act on release, so nothing has run when a drag starts.
- The icon lifts out of the dock and follows the pointer, as a sync
  subsurface. While the button is held, the compositor keeps sending
  motion to the dock surface even outside it (the implicit grab; to be
  checked on niri). The icon's slot closes, and a gap opens where it would
  land, with the neighbors sliding apart on springs. Magnification keeps
  following the pointer while it is over the dock.
- Only pinned items have an order to change. A pinned icon moves the gap
  among the pinned items; past the last one, the gap stays at their end.
  A running app that isn't pinned, dragged among the pinned items, gets
  pinned there (as on macOS). Dropped back among the running apps, it
  returns to its place. Folders reorder among the folders, between the
  separator and the Trash.
- Dropping writes the new order to the pins file. Releasing off the dock,
  or Escape (the press gave the dock keyboard focus, see
  [Ctrl+click](#ctrlclick)), cancels: the icon flies back to its slot on a
  spring. **Dragging off the dock never unpins**, so a slip never loses an
  icon; that's what the Trash is for.
- A drag closes an open strip or menu. No label shows during a drag, and
  autohide waits until it ends.
- The rules are pure, in `drag.rs` (tested): where the gap is for a
  pointer position in the magnified layout, what a drop does (move, pin
  at, unpin, nothing), and that cancelling leaves the pins as they were.

### The Trash

The Trash is the dock's last item, after a separator, as on macOS
(`trash = false` hides it). It isn't in the pins file.

- **The separator** is a thin line across the panel (`separator-color`).
  It takes a fixed length along the dock and doesn't magnify, so `zoom.rs`
  gets slots of different lengths. It is shown when there are folders or
  a Trash after it.
- **The icon** is the theme's `user-trash` or `user-trash-full`, following
  whether `$XDG_DATA_HOME/Trash/files` is empty (watched with inotify).
- **Clicking** runs `open-command` with `trash:///`. Its menu has Open and
  Empty Trash (`trash-empty-command`, default `gio trash --empty`). Empty
  Trash asks first: picking it turns the row into "Empty Trash: click
  again", and only a second pick runs it.
- **Dropping a dock item** on it removes the item from the dock. A pinned
  app is unpinned, and stays among the running apps if it has windows,
  else shrinks away. A pinned folder is unpinned; **the folder itself is
  never touched.** A running app that isn't pinned has nothing
  to remove, so the Trash doesn't accept it and dropping there cancels.
- **Dropping files from outside the dock** (a file manager, or the folder
  popup) moves them to the trash with `trash-command` (default
  `gio trash -- {paths}`). This needs `wl_data_device` (phase 11).
- **Feedback.** While the Trash accepts what is dragged, it grows a little
  and its label says what a drop does: "Remove from Dock" or "Move to
  Trash".

## Drag and drop

Files come in through `wl_data_device` (phase 11). Core gains the data
device manager, a device per seat, offers and sources. The dock accepts
`text/uri-list` (local `file://` URIs only). As a drag moves over the
dock, it magnifies as for the pointer, and the target under it says what
a drop does:

| Target        | Drop                                                          |
| ------------- | ------------------------------------------------------------- |
| between icons | folders only: a gap opens and they are pinned there           |
| an app        | open the files with it (see below)                            |
| a folder      | copy or move the files into it, as negotiated with the source |
| the Trash     | move the files to the trash (`trash-command`)                 |

- **Only folders are pinned.** A drag whose paths are all folders opens a
  gap between the folders and pins them there on a drop. Files are never
  pinned: they go into a pinned folder or open with an app. A drag that
  holds any file opens no gap, the space between icons doesn't accept it,
  and dropping there does nothing (the drag goes back to its source).
  `.desktop` files are files too, so dropping one doesn't pin its app.
- **Opening with an app.** The files fill `%f %F %u %U` in the app's
  `Exec=` (one launch per file for `%f` and `%u`), or `launch-command`
  with `{files}` (shell-quoted, separated by spaces). An app whose `Exec=`
  has no file field code doesn't accept the drop. `[app.<id>] drop =
"..."` overrides this, with `{files}`.
- **Moving into a folder.** A pinned folder accepts files and folders. The
  dock offers copy and move and takes the action the source picks
  (`wl_data_offer.set_actions`, preferring move on the same file
  system), then runs `copy-command` or `move-command` (`cp -rn` and
  `mv -n` by default, with `{paths}` and `{dest}`: `gio copy` can't copy
  folders, and `-n` never overwrites). A folder doesn't accept itself,
  one of its parents, or what is in it already.
- **Feedback.** The target under the drag says what a drop does in the
  label: "Keep in Dock", "Open with <app>", "Move to <folder>" (or "Copy
  to"), "Move to Trash". A target that doesn't accept the drag shows no
  label and doesn't grow.
- **Autohide.** A hidden dock reveals when a drag reaches its trigger
  band, and stays shown until the drag leaves or drops.
- **The dock's own drags stay on the implicit grab** (decided in phase
  11). Data-device drags would let the icon leave the dock surface and a
  pinned folder be dropped into a file manager, but they trade the
  pointer events the magnification and gap follow for data-device ones,
  and the drop target, not the dock, would decide what dragging off the
  dock does; it must never do anything.
  Files dragged out of a folder's popup are data-device drags.

## Folders

Folders can be pinned like apps; files can't. Folders come after the
separator, before the Trash, as on macOS. A file reaches the dock only by
being dropped: into a pinned folder, onto an app, or onto the Trash (see
[Drag and drop](#drag-and-drop)).

- **Adding one.** Drop a folder on the dock, or add its path to the pins
  file (by hand, or with `wl-dock pin ~/Downloads`). Folder options go in
  the config, keyed by path the same way apps are:

  ```toml
  [folder."~/Downloads"]
  sort  = "modified"   # modified | added | name
  count = 20           # entries shown
  view  = "list"       # list | stack | grid (later)
  icon  = "folder-download"
  ```

- **Icons.** A folder shows its theme icon (`folder-download` and so on for
  the XDG user dirs, else `inode-directory`); optionally the newest files'
  icons stacked. A folder that no longer exists (or is no longer a
  directory) shows a faded icon, and its menu offers Remove from Dock.
- **Entry icons.** Files in the folder popup show their thumbnail, else
  their mime type's icon. Thumbnails come from the freedesktop thumbnail
  cache (`$XDG_CACHE_HOME/thumbnails/{normal,large}`); the dock only reads
  it and never generates thumbnails.
- **Clicking a folder** opens a popup with the `count` most recent entries
  (by mtime), using the same popup machinery as the strip and menu.
  Entries show a thumbnail or mime icon, and the name. The worker does the
  listing, inotify keeps it current, and the popup ends with "Open in file
  manager". Clicking an entry opens it; dragging one out offers the file
  (copy or move, as negotiated with the drop target), with its icon as the
  drag icon. Dropped on the Trash, it is trashed.
- **Stack view** (`view = "stack"`, stack.rs). The entries as icons of
  the dock's icon size, with the dock icons' shadow, each with its name
  in a pill, on a transparent popup. Above a bottom dock they rise from
  the folder in a column, newest nearest the dock, bending to the right
  along a large circle (a fan, without macOS's tilt: subsurfaces can't
  rotate), names on the left. Beside a side dock it is a straight column
  centered on the icon, names away from the dock. Only as many entries
  as fit on the output are shown, and Open in File Manager (with the
  folder's icon) is always last. Opening, the items fly out of the
  folder's icon, growing as they go and staggered from the dock outward;
  closing, they fly back in. The hovered name's pill takes the highlight
  color, Up and Down move along the stack, and clicking and dragging out
  work as in the list.
- **Later:** a Recent Files item (from `recently-used.xbel`).

## Launcher

`wl-dock --launcher` turns the dock into a search bar for apps, like
Quicksilver or Alfred. Bind it to a key:

```kdl
Mod+Space { spawn "wl-dock" "--launcher"; }
```

It opens on the focused output, and only if the dock there is at the
bottom. The bar grows out of the panel and the matches roll around the
dock, and neither works along a side, so a side dock disables the
launcher (see [Where](#the-bar)).

### Talking to the dock

The running dock listens on a Unix socket, `$XDG_RUNTIME_DIR/wl-dock.sock`,
with the same path scheme and line protocol as the daemons
(`wl_shell_daemon::ipc`). The listener is factored out of
`wl_shell_daemon::run` so the dock's own loop can use it. `--launcher`
sends `launcher` and waits, like the other apps' clients: it exits 0 when
something was opened and 1 when cancelled or unavailable. A second
`--launcher` while the launcher is open closes it, so one key toggles.
`--apps` works the same way. `--activate` doesn't wait (see
[Keyboard activation](#keyboard-activation)). Without a running dock, all
three fail with a message; they don't start one. Pins stay in their file.

### The bar

```
     [·]                               5th match: the farthest, smallest
     [ ]                               4th
    [  ]                               3rd
    [   ]                              2nd: Up brings it to the front
   ╭─────────────────────────────────────────────────╮
 [ICON]  fir|efox                                    │
   ╰─────────────────────────────────────────────────╯
```

- **Where.** The launcher is its own layer surface (see
  [Surfaces](#surfaces)), created without an output, so the compositor
  puts it on the focused output (as niri does). It covers the output,
  transparent outside the bar and the matches, so a click outside closes
  it. It maps fully transparent, and once `wl_surface.enter` says which
  output it is on, the dock there hands over and the bar sits where its
  panel is. If that dock is on a side, or the output has no dock, the
  launcher closes before drawing anything, and `--launcher` says the
  launcher needs a bottom dock and exits 1. With every dock on a side,
  there is no launcher.
- **The icon** of the selected match, at `launcher-icon-scale` (default
  1.25) × `icon-size`, so it's slightly larger than the dock's icons and
  rises out of the bar as a magnified icon does. It is the front of the
  drum (see [The matches](#the-matches)). With an empty query it is
  `system-search`.
- **The query** follows, in a larger font (`launcher-font-size`), with a
  caret. When the query is a prefix of the selected match's name, the
  rest of the name completes it inline in a dim color (`fir` + `efox`);
  otherwise the name shows dimmed at the bar's end. This is the only
  place a match's name shows. Empty, it shows a placeholder ("Search
  apps").
- **Width.** `launcher-width` (default 0.4 of the output), at least the
  resting dock's length.

### The matches

The matches aren't a list. The best `launcher-matches` (default 5) ride
a virtual cylinder: the dock is a drum lying along the bottom edge, and
the matches sit on its face, around it. The selected match is at the
front, in the bar. The others stand above it in rank order, each further
around the drum, so they are higher up and smaller; the last looks
farthest away, and each grows as it comes closer to the front. Each
shows its icon and, beside it, its name in a small pill of the bar's
color: dim, with the characters the query matched in the text color
(`search.rs` gives them). The front's name is in the bar, so its pill
fades in as it rolls up out of the front.

- **Geometry (`drum.rs`, pure, tested).** A match at ring position `t`
  (0 the front, 1 the next, …) sits at the angle `φ = t · step` on the
  drum (`step` about 20°, so the last of 5 is near the top). Its depth
  is `d = (1 − cos φ) / (1 − cos φmax)`, 0 at the front and 1 at the
  last slot, its size is the front's × `1 − d · (1 − launcher-far-scale)`
  (default 0.5), and it dims a little with `d`. The matches are stacked
  tightly in the front icon's column: the rise at `t` is the integral of
  the size from 0 to `t` (closed form), plus 8%, so neighbors are a small
  gap apart (at least 8% of the smaller) and never overlap. The slots
  don't depend on how many matches there are.
- **The ring is closed.** Past the last slot, the drum's back runs over
  the top and down behind the bar to the front. A match on that stretch
  (`t` between n − 1 and n) is stacked below the bar and faded out, so
  it disappears into the bar and reappears over the top.
- **Moving.** Up (or Ctrl+N, or scrolling) rolls the drum one step
  toward you: every match moves a slot down and grows, the second comes
  to the front and becomes the selection, and the old front rolls down
  into the bar, around the back, and comes over the top into the last
  slot. Down rolls it back the other way. The roll is a spring on `t`
  (`response`), so quick presses add up and it never jumps. The ring
  order is always the rank order, rotated so the selection is in front.
- **Typing** re-ranks. The drum then shows the new best match in front,
  without rolling, and each slot whose match changed cross-fades its
  icon.
- **Fewer matches.** With n below `launcher-matches`, only the first n
  slots are used, and the way back is longer. With one match, or
  `launcher-matches = 1`, only the front shows. An empty query shows only
  `system-search`.
- **Pointer.** Clicking a match opens it, and scrolling rolls the drum.
- **Rendering.** Each match is a subsurface with its icon rendered once
  at the front size and scaled by its viewport, as the dock's icons are,
  and its name likewise (a worker pill, scaled with the icon). A match
  going behind the bar is restacked below it (`place_below`), and back
  above as it comes over the top; its name hides while it is behind.

### The transformation

- **Opening.** The dock's icons shrink and fade toward the bar's center,
  while the panel's rectangle and radius move to the bar's on a spring
  (`response`). The icon and the query fade in as it settles, and the
  other matches roll up onto the drum from behind the bar. The keyboard
  is the launcher's from the first frame, so nothing typed early is lost.
  An autohidden dock rises from its edge as part of it. Magnification and
  labels are off while the launcher is open.
- **Closing** runs it backwards: the matches roll back behind the bar,
  and the bar becomes the panel. When an app is opened, its icon flies
  from the front of the drum to its slot in the dock (or where its new
  icon appears) and starts bouncing there.

### Matching (`search.rs`, pure, tested)

- **Candidates** are the app index (see [Applications](#applications)).
  Each is matched by Name, GenericName, Keywords, its desktop ID (without
  `.desktop` and a reverse-DNS prefix) and its `Exec=` program, with Name
  weighted most.
- **Scoring.** A fuzzy subsequence score (`nucleo-matcher`, as in helix),
  ignoring case and accents, with bonuses for a match at the start of the
  name and at word starts, so acronyms work (`vsc`: Visual Studio Code).
- **History.** Every launch from the dock, the grid or the launcher is
  recorded in `$XDG_STATE_HOME/wl-dock/history` (app ID, count, last
  time), and frecency (decaying with a half-life of a week) adds to the
  score. The launcher also learns which app a query meant: the app last
  picked for a query, or a prefix of it, comes first for it (phase 15).
  `history = false` turns both off.
- **Ties** go to the shorter name, then alphabetically, and the order is
  stable, so the results don't reshuffle while typing. Matching runs on
  the main thread (a few hundred apps); only the text goes to the worker.
- **Tests:** prefix beats subsequence, acronyms, accents, history order,
  learned queries winning, and stable ties.

### Keys

| Key                       | Effect                                                  |
| ------------------------- | ------------------------------------------------------- |
| typing                    | edits the query; the matches update on every key        |
| `'`                       | search files (see [Files](#files-phase-14))             |
| Backspace, Ctrl+W, Ctrl+U | delete a character, a word, everything                  |
| Enter                     | open the selected app: activate if running, else launch |
| Ctrl+Enter                | launch a new instance                                   |
| Up, Ctrl+N                | the next match: the drum rolls one step toward you      |
| Down, Ctrl+P              | the previous match: the drum rolls back                 |
| Tab                       | the selected app's actions; Shift+Tab goes back         |
| Escape                    | clear the query; with an empty query, close             |

`launcher-enter = "launch"` makes Enter always launch, and Ctrl+Enter
activate.

### Files (phase 14)

A query that starts with `'` searches files instead of apps, as in
Alfred: `'file.pdf` searches for `file.pdf`. There is no index: each
search walks the disk while the launcher is open, and the best matches
stream onto the drum as they are found, so it feels as fast as the apps.

```
     [·]                               README.md  wl-dock
     [ ]                               PLAN.md  wl-switcher
    [  ]                               PLAN.md  wl-shell
   ╭─────────────────────────────────────────────────╮
 [ICON]  ~ ▸ plan|  PLAN.md    ~/projects/…/wl-dock  │
   ╰─────────────────────────────────────────────────╯
```

- **The trigger.** `files-trigger` (default `"'"`) is the character that
  switches to files when typed first. A space is never a trigger, so it
  stays a plain character in app queries. The trigger switches the scope
  (no token: the search is simply over the files), and the rest is the
  query. Backspace on an empty query, or Shift+Tab, goes back to the apps
  with the query empty. With an empty file query, the bar shows a folder
  icon and the placeholder "Search files", and the drum is empty.
- **The walk** starts when the trigger is typed, not when the launcher
  opens, so app searches never touch the disk. It walks `files-root`
  (default `~`) with the `ignore` crate's parallel walker (as ripgrep
  and helix do), on a few threads of their own, off the main loop:
  - `files-exclude` is a list of gitignore-style globs relative to the
    root (default `["node_modules", "target", "__pycache__"]`), turned
    into overrides, so excluded directories are never entered.
  - Hidden files and directories are skipped unless `files-hidden`.
  - `.gitignore`, `.ignore` and git's global excludes are honored unless
    `files-gitignore = false`.
  - Symlinks aren't followed, the walk stays on the root's file system
    (no network mounts or `/proc`), and it stops after `files-max`
    entries (default 1,000,000), so a huge tree can't eat the memory.
  - Directories are results too; the walk still descends into them.
- **Matching (`nucleo`).** Every entry the walker finds is pushed into a
  `nucleo::Nucleo` through its injector, as helix's file picker does. The
  single column is the path relative to the root, and the item holds
  nothing else (the path is the root joined with it), so an entry costs
  about its path's length. The matcher uses `match_paths()` (bonuses
  after `/` and on the file name), ignores case and accents, and the
  query uses nucleo's pattern syntax: spaces separate atoms (`dock plan`),
  `^` and `$` anchor, `!` excludes, and `'` asks for an exact substring
  (the trigger is stripped first, so `''file.pdf` is an exact search).
  Typing re-parses the pattern; appending to it only rematches the
  previous matches, so narrowing stays cheap at any size. Nucleo matches
  and sorts on its own thread pool, never on the main loop.
- **Streaming.** Nucleo's notify callback (new items, or a match pass
  done) pings the dock's calloop. On the next frame the main thread
  calls `tick(0)`, which never blocks, and takes the top
  `launcher-matches` from the snapshot. Only if they changed is the drum
  updated, as typing does: the slots whose match changed cross-fade.
  This happens at most once a frame, however fast results arrive.
- **Stable selection.** Until the drum is rolled, the front is always the
  current best match. Once the user rolls to a match, it stays selected
  and in front while results keep arriving, as long as it is in the
  results; the others update around it. Typing resets this.
- **Ties.** Nucleo breaks equal scores by insertion order, which is the
  walk's order and not meaningful. Whether shorter paths need a small
  score bonus to come first (e.g. `PLAN.md` over a deep namesake) is to
  be decided on real trees.
- **The matches** show each file's thumbnail, or else its MIME type's
  icon (`wl-shell-render::files` and `mime`, as in the folder popup),
  rendered by the worker at the front size. Until it arrives, a generic
  icon for files or folders stands in, so streaming never waits on the
  worker. A pill shows the file name and, dimmed after it, its parent
  directory's name, since names repeat across a tree. The characters the
  query matched are highlighted in both (`Matcher::fuzzy_indices` on the
  main thread, for the shown matches only).
- **The bar** holds two lines for files. The first is the query and the
  front's file name: inline after the caret, dim, if the query is the
  start of it (`plan` + `.md`), else apart after an arrow (`dock plan
  → PLAN.md`) with the matched characters brighter. The second, smaller
  and dimmer, is the front's full path (`~` for home), the matched
  characters bright and the name brighter than its directories.
  Directories the query didn't match are elided (`~/…/wl-dock/PLAN.md`)
  to fit, by an estimate of the font's width. Nothing sits at the bar's
  end.
- **Progress.** While nothing is selected, the second line shows a count
  of entries seen (with `…` while the walk runs). With the walk done and
  nothing matching, it shows "No files".
- **Opening.** Enter opens the front match with `open-command` (a folder
  in the file manager, a file with its default app). Ctrl+Enter shows it
  in its folder: `open-command` on the parent directory. Both close the
  launcher and answer `picked <path>`. Picks aren't recorded in the
  history. (Later: Tab for file actions, see [Future](#future).)
- **Lifetime.** The walk and its items live for the launcher's session:
  going back to the apps and returning reuses them, so a second `'` is
  instant. Closing the launcher cancels the walk (its visitors quit on a
  flag) and drops the items, so nothing stays in memory and the next
  search sees the disk as it is then.
- **Pure parts, tested:** the trigger and token handling (in launcher.rs:
  `'file.pdf`, `''` for an exact atom, a leading space staying an app
  query, Backspace back to the apps), and
  `filesearch.rs` on a temporary tree: excludes, hidden files,
  `.gitignore`, `files-max`, no symlinks followed, and that cancelling
  stops the walk. Ranking is nucleo's, checked with a few paths.

### Actions (phase 15)

- Tab puts the selected app's actions on the drum instead of the apps:
  the same entries as its dock menu (`menu.rs`: windows, desktop actions,
  app actions, Keep in Dock, Close). The app becomes a token in the field
  (`Firefox ▸`), and the query now filters the actions; the drum holds
  the best `launcher-matches` of them and rolls the same way.
- Actions have no icons of their own, so each is a pill with its name
  (worker text, with the action's icon if it has one), scaled like an
  icon by its depth. (Open question: whether the far pills stay
  readable; if not, only the front one gets its name.)
- Enter runs the front action. Shift+Tab, or Backspace on an empty
  query, goes back to the apps.

## Keyboard activation

Holding Super, the number keys reach the first ten dock items, as on the
Windows taskbar. Bind them to `wl-dock --activate <n>`:

```kdl
Mod+1 { spawn "wl-dock" "--activate" "1"; }
Mod+2 { spawn "wl-dock" "--activate" "2"; }
// …
Mod+0 { spawn "wl-dock" "--activate" "10"; }
```

### Numbers

- Items are numbered 1–9, then 0 for the tenth, in dock order: the apps,
  then the folders. The Applications item and the Trash have no
  number, and neither do items past the tenth. The numbers are those of
  the dock on the chosen output, so with `windows = "output"` they can
  differ between docks.
- While the mode is on, each numbered icon shows its number in a small
  badge at its corner (`badges = true`), and the dock reveals if it is
  autohidden. The item being acted on magnifies, as if the pointer were
  on it.

### The mode

`--activate n` sends `activate n` over the socket and exits as soon as the
dock has it; unlike `--launcher`, it doesn't wait. The dock acts on item n
at once, then keeps listening for as long as the modifier is held:

| Item n                      | First press of n                      | Pressing n again                    |
| --------------------------- | ------------------------------------- | ----------------------------------- |
| one window                  | activate it; the mode ends            | (another `--activate n`: the same)  |
| `strip-min-windows` or more | open the strip on the previous window | highlight the next one in MRU order |
| not running                 | launch it (with the bounce)           | nothing (it is launching)           |
| a folder                    | its popup, first entry highlighted    | highlight the next entry            |
| a file                      | open it; the mode ends                |                                     |

- **Releasing the modifier** activates the highlighted window (or opens
  the highlighted entry) and closes the popup. Escape closes it without
  activating, and Enter activates at once. The arrow keys move the
  highlight too, and Shift+n moves it back.
- **Another number** m, while the modifier is held, closes the open popup
  and acts on item m as in the table.
- **When the mode ends after activating directly**, the window takes the
  keyboard. A later Super+n reaches the compositor's binding again, so it
  is simply another `--activate`.
- **The highlight follows the MRU order**, as an Alt+Tab switcher does.
  It starts on the most recent window that isn't focused (as a click
  does), so Super+n and letting go switches to the app's previous window.
  Each further press of n moves it to the next window in MRU order,
  wrapping, while the cards stay where they are. Without
  `mru-event-stream`, the MRU order is unknown, and the highlight starts on
  the first card and steps in card order.
- `--activate` without a number only shows the badges while the modifier
  is held, for compositors that can bind the modifier on its own. A
  number pressed then acts as above.

### Keyboard and modifiers

The press that started it went to the compositor's binding, so the dock
never saw it. To see the next presses and the release, the dock maps the
activate surface (see [Surfaces](#surfaces)): created without an output,
so it lands on the focused output, with exclusive keyboard. Its
`wl_surface.enter` picks the dock that acts, the same way as the
launcher's.

- The modifiers held when the surface gets the keyboard (the first
  `wl_keyboard.modifiers`) are the ones to watch. The mode ends when none
  of them is held any more, as wl-switcher does for its hold.
- If none is held at that point (a quick tap, released before the focus
  arrived), an open strip stays open as if clicked, with the keyboard:
  Enter, Escape, the arrows and the numbers work, and nothing activates
  by itself.
- The strip and the folder popup open **without a grab** in this mode.
  `xdg_popup.grab` needs the serial of an input event the dock received,
  and the starting press went to the compositor. The keys come through
  the activate surface instead. A click on a card still works; a click
  elsewhere isn't seen, so the mode ends on the keys alone. (To check on
  niri: whether a later key press's serial is enough to take a grab
  after all.)

The rules are pure, in `activate.rs` (tested): the numbering (skipping
the Applications item and the Trash, 0 for the tenth), what a press does
given the item and the mode's state (activate, launch, open, next card,
switch item), and what releasing the modifiers does.

## Applications

An Applications item opens a grid of every installed app. It isn't a
menu: the apps lie in a fixed grid, with one page per category, and you
swipe between the categories.

```
 ╭──────────────────────────────────────────────────╮
 │  All   Accessories  [Development]  Graphics  …   │
 │                                                  │
 │   [ ]     [ ]     [ ]     [ ]     [ ]     [ ]    │
 │  Code   Emacs   GitG   Meld   Neovim  Zed        │
 │   [ ]     [ ]     …                              │
 ╰──────────────────────────────────────────────────╯
                 [Applications]
```

- **The item.** `applications = "start" | "end" | "none"` puts an
  Applications icon (`applications-icon`, default `view-app-grid`) first
  in the dock, or last before the separator. Clicking it opens or closes
  the grid. `wl-dock --apps` does the same from a keybinding, on the
  output the compositor picks (see [Launcher](#launcher)).
- **The popup** is an xdg_popup of the dock surface with a grab, anchored
  to the Applications item, like the strip. Its size comes from the grid
  alone, so it never resizes between categories.
- **Apps.** Every desktop entry of type Application with an `Exec=`, that
  isn't `NoDisplay` or `Hidden`, passes `OnlyShowIn`/`NotShowIn` for
  `$XDG_CURRENT_DESKTOP`, and whose `TryExec` exists. Sorted by localized
  name, ignoring case and accents. A cell is the icon (`apps-icon-size`,
  default 64) with the name under it, on one line, ellipsized.
- **Categories.** The headers along the top: All first, then the
  freedesktop main categories that have apps, in a fixed order, with
  built-in English names (AudioVideo is "Multimedia", Network "Internet",
  Utility "Accessories", and so on), then Other for apps without a main
  category. An app with several main categories is in each.
  `[apps.categories]` in the config can reorder, rename and hide them, or
  define new ones from `Categories=` values.
- **The grid** is `apps-columns` × `apps-rows` (default 7 × 5) for every
  category. A category with fewer apps leaves cells empty; one with more
  scrolls.
- **Switching categories.** A two-finger horizontal swipe on a touchpad
  (horizontal axis events from a finger source) moves the pages with the
  fingers. On `axis_stop`, they settle on a spring at the nearest
  category, or the next one if the swipe was fast. A horizontal wheel
  notch moves one category (rate-limited), and so does clicking a header,
  Tab or Shift+Tab, and Ctrl+Left or Ctrl+Right. The header highlight
  slides like the menu's, and the header row scrolls to keep it in view.
- **Scrolling.** Vertical scrolling moves the current category's rows,
  clamped, with a short rubber band at the ends and kinetic scrolling
  after a touchpad's `axis_stop` (the compositor doesn't do it). A
  gesture locks to the axis it starts along, over its first ~10 px, so a
  diagonal swipe either pages or scrolls. Each category keeps its scroll
  position while the grid is open.
- **Picking.** Clicking a cell, or Enter, acts like clicking the app in
  the dock: activate its most recent window if it runs, else launch it
  (with the bounce on its dock icon). Middle click launches a new
  instance, and right click opens its menu (`menu.rs`). Picking closes
  the grid.
- **Keyboard.** The arrows move a highlight (Left and Right wrap into the
  next row), Page Up/Down move a page of rows, Home/End go to the first
  and last app, Escape closes. Typing a printable character switches to
  the launcher with it as the query.
- **Animation.** The grid grows out of the Applications icon (scale 0.9 to
  1 and fade in), with its cells staggered by their distance from the
  icon, as the strip's cards are. The pages slide when switching, and
  closing fades.
- **Rendering.** The worker renders the icons and names of the visible
  page and its neighbors only, into the shared cache.
- **Pure parts, tested:** the app index (filtering, categories, sorting;
  in `wl-shell-render::icons`, with sample `.desktop` files), and
  `grid.rs`: cell layout, paging, scroll clamping, swipe settling, axis
  locking and hit testing while scrolled.
- **Later:** dragging an app from the grid onto the dock to pin it.

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
  and drop (phase 11).
- The socket listener, factored out of `wl-shell-daemon`, for `--launcher`,
  `--apps` and `--activate`.

Desktop entry `Exec=` and desktop action lookup (`Actions=` and the
`[Desktop Action …]` groups' `Name=`, `Icon=` and `Exec=`) go into
`wl-shell-render::icons`, next to `name()`, with unit tests on sample
`.desktop` files.

The app index for the grid and the launcher (every visible application
entry, with its GenericName, Keywords and categories) goes there too,
loaded by the worker with the entries. Scroll physics (rubber band, kinetic
scrolling, settling on pages) goes into `wl-shell-ui`, shared by the grid
and the strip.

## Config

`~/.config/wl-dock/config.toml` uses the same `[appearance]`,
`[appearance.dark]` and `[appearance.light]` tables as the other apps.

| Key                   | Default                        | Meaning                                         |
| --------------------- | ------------------------------ | ----------------------------------------------- |
| `outputs`             | `"all"`                        | `"all"` or a list of output names               |
| `position`            | `"bottom"`                     | `bottom`, `left`, `right`                       |
| `layer`               | `"top"`                        | `top` or `overlay`                              |
| `icon-size`           | `48`                           | px at rest                                      |
| `icon-gap`            | `6`                            | px                                              |
| `margin`              | `6`                            | px from the screen edge                         |
| `padding`             | `6`                            | px inside the panel                             |
| `magnification`       | `1.8`                          | hovered icon's scale; 1 disables                |
| `magnification-range` | `3`                            | icons on each side that grow                    |
| `reserve`             | `true`                         | exclusive zone for the resting dock             |
| `autohide`            | `false`                        |                                                 |
| `reveal-delay`        | `150`                          | ms                                              |
| `hide-delay`          | `500`                          | ms                                              |
| `hover-delay`         | `300`                          | ms before the label                             |
| `strip`               | `true`                         | click on a multi-window app opens its strip     |
| `strip-min-windows`   | `2`                            | fewer windows: a click activates                |
| `strip-max-columns`   | `3`                            | cards per row along the dock; 0: no limit       |
| `strip-max-rows`      | `3`                            | rows stacked away from the dock; 0: no limit    |
| `strip-card-height`   | `0.18`                         | of the output height                            |
| `strip-max-width`     | `0.6`                          | of the output width                             |
| `previews`            | `"live"`                       | `live`, `static`, `off`                         |
| `zoom`                | `1.04`                         | hovered card                                    |
| `indicator`           | `"dot"`                        | `dot`, `dots` (one per window, max 3), `none`   |
| `active-indicator`    | `true`                         | mark the focused app (needs the stream)         |
| `labels`              | `true`                         | app name on hover                               |
| `response`            | `150`                          | ms, spring response                             |
| `activate-command`    | none                           | `{id}`                                          |
| `close-command`       | none                           | `{id}`                                          |
| `launch-command`      | none                           | `{exec}`, `{desktop-id}`, `{app-id}`            |
| `terminal`            | `"foot"`                       | for `Terminal=true` entries                     |
| `launch-timeout`      | `10000`                        | ms of bouncing at most                          |
| `mru-event-stream`    | none                           | as in wl-switcher                               |
| `output-event-stream` | none                           | `<output> <id>` lines (see Outputs)             |
| `windows`             | `"all"`                        | `all` or `output`                               |
| `output-grace`        | `250`                          | ms a new, unplaced window is held back          |
| `autohide-trigger`    | `2`                            | px band along the edge that reveals the dock    |
| `open-command`        | `"xdg-open {path}"`            | opening folders, their entries and the Trash    |
| `drag-threshold`      | `6`                            | px a press moves before it drags                |
| `trash`               | `true`                         | the Trash at the end of the dock                |
| `trash-command`       | `"gio trash -- {paths}"`       | moving dropped files to the trash               |
| `trash-empty-command` | `"gio trash --empty"`          | Empty Trash                                     |
| `copy-command`        | `"gio copy -- {paths} {dest}"` | files dropped on a folder                       |
| `move-command`        | `"gio move -- {paths} {dest}"` | files dropped on a folder                       |
| `applications`        | `"start"`                      | the Applications item: `start`, `end`, `none`   |
| `applications-icon`   | `"view-app-grid"`              | an icon name or a path                          |
| `apps-columns`        | `7`                            | grid columns                                    |
| `apps-rows`           | `5`                            | grid rows                                       |
| `apps-icon-size`      | `64`                           | px                                              |
| `[apps.categories]`   | built in                       | order, names, hidden and extra categories       |
| `launcher-width`      | `0.4`                          | of the output width                             |
| `launcher-icon-scale` | `1.25`                         | the bar's icon, × `icon-size`                   |
| `launcher-font-size`  |                                | the query's font size                           |
| `launcher-matches`    | `5`                            | matches on the drum; 0 shows only the selection |
| `launcher-far-scale`  | `0.5`                          | the farthest match's size, × the front's        |
| `launcher-enter`      | `"activate"`                   | `activate` or `launch` on Enter                 |
| `files-trigger`       | `"'"`                          | first character that switches to file search    |
| `files-root`          | `"~"`                          | the directory file search walks                 |
| `files-exclude`       | `["node_modules", …]`          | gitignore-style globs, relative to the root     |
| `files-hidden`        | `false`                        | include hidden files and directories            |
| `files-gitignore`     | `true`                         | honor `.gitignore`, `.ignore` and git excludes  |
| `files-max`           | `1000000`                      | entries walked at most                          |
| `history`             | `true`                         | record launches for ranking                     |
| `badges`              | `true`                         | numbers on the icons during keyboard activation |
| `[app.<app-id>]`      |                                | per-app actions and overrides (see Menu)        |
| `font`, `font-size`   |                                |                                                 |
| `[output.<name>]`     |                                | per-output overrides (see Outputs)              |

`[appearance]` has `panel-color`, `panel-border` (a thin highlight around the
panel, as on macOS), `panel-radius`, `indicator-color`,
`active-indicator-color`, `label-color`,
`label-background`, `title-color`, `title-background`, `placeholder`, the
`shadow-*` and `icon-shadow-*` keys, `icon-theme`, and the menu's keys (see
[Menu design](#menu-design)): `menu-background`, `menu-border`,
`menu-radius`, `menu-tail`, `menu-text`, `menu-highlight`,
`menu-highlight-text`, `menu-separator`, `menu-row-height`,
`menu-padding`, `menu-group-gap`, `menu-max-width` and `menu-shadow-*`.
The newer parts add `separator-color`, the grid's `apps-background`,
`apps-header`, `apps-header-highlight` and `apps-highlight`, and the
`apps-header`, `apps-header-highlight` and `apps-highlight`, the
launcher's `launcher-background`, `launcher-text`, `launcher-completion`
and `launcher-caret`, and `badge-color` and `badge-background`.

The config and the pins file are watched with inotify and reloaded when they
change. A config error keeps the previous config and prints a warning.

## Phases

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
4. **Actions (done).** `spawn` and `LineStream` in core, `activate-command`, MRU and
   the active indicator, and `output-event-stream` with `windows = "output"`.
   Exit criterion: daily-drivable as a switcher on niri, with per-monitor
   docks.
5. **Pinning and launching (done).** Pinned items in `dock.rs` (pinned first, kept
   when not running, on every dock, with the hollow indicator for windows on
   other outputs); the pins state file with inotify; the `pin`/`unpin`/`pins`
   CLI; launching from desktop entries, `launch-command`, the bounce and
   middle click. Exit criterion: daily-drivable as a launcher too.
6. **Menu and app actions (done; the look still needs checking on
   screen, see below).** `menu.rs` (rows, layout, tail clamping, popup
   placement; unit tested), the speech bubble (`wl-shell-render::bubble`,
   rendered by the worker: shape, tail, border, separators, shadow),
   `menuview.rs` (the popup, sliding highlight, open/close animation and
   the pick flash), keyboard navigation, desktop actions read from the
   `.desktop` file, `[app.<id>]` (actions, overrides, middle click and
   scroll), `close-command`, and Keep in Dock / Remove from Dock. Exit
   criterion: the menu looks finished on bottom and side docks, at scale
   1 and 2, and its tail points at the icon even when the compositor
   slides it. The placement math is tested; the look is still to be
   checked on niri at both scales.

   Notes from building it:
   - The menu opens in steps, all asynchronous: the rows' texts first
     (the bubble is as wide as the widest), then the popup, then the
     bubble for where the compositor put it. It shows once that bubble is
     there, so the tail never jumps.
   - The positioner only slides along the dock (`slide_x` for a bottom
     dock, `slide_y` for a side dock); flipping or sliding across it
     would take the tail off the icon.
   - A desktop action uses `launch-command` only if it has `{exec}`
     (anything else, e.g. `{desktop-id}`, would launch the app instead);
     an app's own `launch-command` is for the app only.
   - Up and Down wrap around. The hovered item doesn't freeze while the
     menu is open (only the magnification does), so a click on another
     icon acts on that icon; no label shows while the menu is open.
   - Widths are measured at the output's scale; `same_size_at_any_scale`
     checks that the layout itself is scale-free and whole-pixel.

7. **The strip (done).** Hover delay, the strip popup, cards and titles, captures
   (live), direction, the close grace period, and the cards
   growing out of the icon. Move `card` into `wl-shell-ui`.
8. **The strip on click (done).** Opening on click (`strip`) instead of hover,
   Ctrl+click to activate the MRU window (with the `on-demand` focus
   handling), the strip's grab and keyboard navigation (the highlight
   starting on the previous window), and the grid layout relative to the
   dock with `strip-max-columns` and `strip-max-rows`. Removes the hover
   strip, `close-grace` and `strip-direction`.
9. **Dragging icons and the Trash (done; the feel still needs checking
   on screen).** Reordering pinned items by dragging,
   pinning a running app by dragging it among the pinned ones, and the
   Trash with its separator: dropping a dock item on it removes it from
   the dock, clicking opens the trash, Empty Trash asks first, and the
   icon follows whether the trash is empty. `drag.rs` holds the rules
   (tested), and `zoom.rs` gains slots of different lengths for the
   separator. Drags use the press's implicit grab on the dock surface;
   `wl_data_device` isn't needed yet. Exit criterion: reordering feels
   like macOS on bottom and side docks, a slip off the dock never loses
   an icon, and the pins file has the new order after every drop. The
   rules are tested; that niri keeps sending motion and the release to
   the dock surface off it (the implicit grab) is still to be checked.

   Notes from building it:
   - The separator and the Trash are items (`dock::Kind`) with keys no
     app ID can have (they contain a NUL), so slots, labels, the menu
     and the hover treat them like apps where that fits. The Trash's
     icon key is named after `user-trash`/`user-trash-full`, so a theme
     without them gets a letter tile.
   - A gap is a cell of its own with a spring, anchored before the
     pinned app it precedes (or after the last one), so it stays put
     while windows come and go. Moving it closes the old gap and opens a
     new one. Back home, the dragged item's own slot opens instead of a
     gap, so dropping where it came from changes nothing.
   - Where the gap goes is decided on the layout as shown, gap included:
     it opens under the pointer, so its neighbors move away and the
     choice holds still. A running app is pinned up to half an icon past
     the last pinned one (or the gap after it).
   - On a drop, the gap's spring becomes the slot's and app.rs rebuilds
     the items at once, so the new order takes over without a jump; the
     icon then flies from the pointer to its slot (or back home when
     cancelled). Into the Trash it just goes, and its slot shrinks away
     or reappears among the running apps.
   - The dragged icon is kept on the dock surface: whether subsurfaces
     outside a layer surface are shown differs between compositors.
   - Escape, or a `leave` (the grab broke), cancels. A press on the
     Trash that moves stays a click.
   - The Trash's directories are created if missing, so there is
     something to watch (`wl_shell_config::watch_dir`). Empty Trash is
     only offered when it's full; its row is measured with its "click
     again" text, so the bubble doesn't change size when it asks.

10. **Autohide (done).** Trigger region, delays and
    the slide (`autohide.rs`: when to show or hide, and how far to slide;
    unit tested). Exit criterion: the dock slides away and comes back
    smoothly on bottom and side docks, and never hides under an open strip
    or menu.

    Notes from building it:
    - An autohiding dock starts hidden and reserves nothing, whatever
      `reserve` says.
    - Everything (panel, icons, indicators, label) slides by the dock's
      reach (panel, margin and shadow, or the icons' shadow if that reaches
      farther) on a spring 1.5× `response`, and is cleared once out of
      sight, so nothing lingers past the edge.
    - Hidden, the input region is the trigger band, as long as the resting
      panel (not the whole edge, so the bottom pixels of windows elsewhere
      stay clickable). Shown, the panel's input region reaches down to the
      edge, so moving the pointer into the margin isn't leaving.
    - The reveal delay starts when the pointer enters the band and isn't
      restarted by moving along it; leaving the band cancels it. Once
      revealed under a resting pointer, it magnifies where the pointer is.
    - Pointer leaving with a strip or menu open doesn't start the hide
      delay; it starts when the popup closes.
    - Clicks on the band (or between icons) give the keyboard focus back.

11. **Drag and drop (done).**
    `wl_data_device` in core, and the dock as a drop
    target for `text/uri-list`: files dropped on an app open with it, on
    the Trash go to the trash, and folders dropped between icons get
    pinned. Files are never pinned: a drag holding any file opens no gap
    and isn't accepted between icons. Drop rules (what each target
    accepts for folders, files and mixes) go in `drag.rs`, tested.
    Autohide reveals for a drag. Decide whether the dock's own drags move
    to the data device. Exit criterion: dragging from Nautilus and one
    other file manager to each target does the right thing on niri, and a
    file dragged between icons is refused.
12. **Folders (done).** Folder
    pins after the separator (file paths in the pins
    file are skipped, `wl-dock pin` refuses them), their theme icons,
    opening with `open-command`, the folder popup (`[folder.<path>]`)
    with the worker's listing kept current by inotify and entry
    thumbnails or mime icons, moving or copying dropped files into
    folders, and dragging files out of the popup. Exit criterion:
    `~/Downloads` in the dock is as useful as on macOS.

    Notes from building phases 11 and 12 together:
    - Core's `dnd.rs` is only set up for programs that ask
      (`Program::DATA_DEVICE`). A drag's `text/uri-list` is read as soon
      as it enters a dock (sources allow reading before the drop), so the
      dock knows what it holds before anything is accepted. Reads never
      block; data a program offers is written on a thread, so a drop on
      the program itself can't deadlock.
    - app.rs decides what takes a drag (`Offer`: the takers and their
      labels) and accepts or refuses it to match what the dock shows; the
      geometry (which item, its middle or edge, the folders' zone) is
      drag.rs's `drop_target`. Folders go into a folder only from the
      middle half of its icon; nearer its edges, they open the gap.
    - Opening with an app needs its desktop entry (prefetched for every
      item); `%u`/`%U` get `file://` URIs, `%f`/`%F` paths. A launch
      command is only used for drops if it has `{files}`.
    - Opening with an app, the Trash and pinning take the drop as a copy
      (the dock does the work, the source keeps its files); folders offer
      both and prefer moving on the same file system.
    - Folder keys are `\0folder\0<pin>`. A folder's icon is
      `[folder.<path>] icon`, else the XDG user directory's
      (`user-dirs.dirs`), else `folder`, rendered under the key `folder`
      so a missing icon falls back to it.
    - The popup is the menu with file rows (32 px icons: the thumbnail,
      else the type's icons from the shared MIME database, with
      fallbacks), listed by the worker on each click. Not done: inotify
      keeping an open popup current (it is listed fresh each time it
      opens), `view = "grid"`, and the stacked newest-files icon.
    - `view = "stack"` is the folder popup's rows in another look
      (menuview.rs `Look::Stack`, laid out by stack.rs), so picking,
      keys and dragging out are shared with the list.
    - A file dragged out of the popup is a data-device drag from the
      popup's surface; the popup fades but lives until the drag ends.

13. **Launcher (done).** The socket listener and `wl-dock --launcher`, the
    launcher surface on the focused output and refusing an output
    without a bottom dock, the morph from the dock and back, the query
    field, the app index (in `wl-shell-render::icons`, shared with the
    grid later), `search.rs` and `drum.rs` (tested), the drum and its
    rolling, Enter and Ctrl+Enter, and the launch history. Exit
    criterion: the bar is there a frame or two after the key press, no
    typed character is lost, a few letters find the app you meant, and
    rolling through the matches with quick presses never jumps. The
    ranking, the drum's geometry and the keys are tested; the rest is
    still to be checked on niri and umbriel.

    Notes from building it:
    - The listener lives in `wl_shell_daemon::listen` (not core, which
      can't depend on the protocol); `run` uses it too. The socket is
      `wl-dock-$WAYLAND_DISPLAY.sock`, the daemons' scheme. The client
      gets `picked <desktop-id>`, `cancelled`, or an error.
    - Core gained `Program::surface_enter`. Should no enter arrive within
      500 ms, the launcher takes the first bottom dock.
    - The bar is the panel's thickness and radius, so the morph only
      moves and stretches it along the dock. The launcher draws it from
      the hand-over on (the dock hides its panel), cross-fading from the
      panel's color to `launcher-background`; the dock's icons shrink
      into its center on a spring of the same response.
    - Closing gives up the keyboard and the input at once; an app picked
      is run after a roundtrip, so it keeps the focus, and the client is
      answered then.
    - Text is worker lines (the query, the inline completion or the name
      at the bar's end); a line keeps its last text until the new one
      arrives. The query is sent between zero-width spaces, so its own
      spaces at the ends count for the caret. The caret is always at the
      end.
    - Accents: the query is folded (lowercase, no marks) before
      `nucleo-matcher`, which folds the names itself.
    - At first the drum's radius was the smallest that kept neighbors
      apart, so five matches stood about 7 front icons tall, mostly
      gaps. Now they are stacked a small gap apart and shrink to half,
      so five stand about 3.5 front icons tall.
    - The history is keyed by desktop ID (`<count> <last> <id>` lines),
      so dock launches and launcher picks add up. Activating a running
      app from the launcher counts too.
    - A picked app that the dock doesn't know yet gets the index's
      desktop entry under its `StartupWMClass` (else its desktop ID), so
      launching never waits for the worker.

14. **Launcher file search (done; the feel still needs checking on
    niri).** The `'` trigger and the root token,
    `filesearch.rs` (the `ignore` walker with `files-exclude`, hidden
    files, gitignore, one file system and `files-max`, feeding a
    `nucleo::Nucleo` through its injector), the calloop ping from
    nucleo's notify and the per-frame `tick(0)`, streaming the top
    `launcher-matches` onto the drum with a stable selection, thumbnails
    and MIME icons with stand-ins, the pills with directory and
    highlights, the progress count, Enter and Ctrl+Enter, and cancelling
    the walk and freeing the items on close. New dependencies: `nucleo`
    and `ignore`. Exit criterion: in a home of a few hundred thousand
    files, the first matches show a frame or two after the trigger on a
    warm cache and keep refining while the walk runs, typing never drops
    a frame, rolling never jumps while results stream in, and closing
    stops the walk at once and gives the memory back. The trigger and
    the walk's filters are tested; the feel is to be checked on niri.

    Notes from building it:
    - The settings are `[launcher.files]`: `trigger`, `root`, `exclude`,
      `hidden`, `gitignore` and `max` (the `files-*` keys above).
      `trigger = ""` turns file search off.
    - The trigger and the scope live in launcher.rs's `Query` (not
      search.rs): the trigger only counts typed (or pasted) into an empty
      app query; Backspace or Ctrl+W on an empty file query, or Shift+Tab,
      go back to the apps. Escape clears a file query, then closes.
    - `.git` is always excluded, hidden or not. `.gitignore` only counts
      inside a git repository (the `ignore` crate's default), and so do
      the parent directories' ignore files. Paths that aren't UTF-8 are
      left out.
    - Nucleo calls its notify for every pushed item; a flag makes only
      the first news since the last frame ping the loop. The ping kicks a
      launcher frame, which takes the news in before drawing.
    - After typing, the snapshot is the old query's until nucleo has
      matched the new one, so the drum keeps its matches until
      `FileSearch::current` says the snapshot is the query's.
    - A kept (rolled-to) match that drops out of the top stays, in the
      last slot, if it is among the first 20,000 matches. A roll under
      way when results change carries on.
    - A file's icons are looked up by the worker (`Assets::file_icons`:
      thumbnail, else the type's icons) and rendered as named icons; the
      stand-in is the generic file or folder icon, swapped without a fade.
    - Equal scores keep the walk's order: `plan` puts `planets/` and
      `PLAN.md` together. Whether a bonus for the file name is needed is
      still to be decided on real trees.
    - The progress reads `12,345 items…` while walking, `12,345 items`
      when done, and `No files` once everything walked was matched
      without a match.
15. **Launcher actions.** Tab for the app's actions from `menu.rs` on the
    drum, with filtering, and learning which app a query meant. Exit
    criterion: with the keyboard only, any desktop action of any app is a
    few keys away.
16. **Keyboard activation.** `wl-dock --activate [n]` over the socket,
    the activate surface and its modifier tracking, the numbering and
    badges, activating a single window, the strip without a grab with the
    highlight stepping on repeated presses and activating on release,
    switching items, launching, and folders' popups. `activate.rs`
    (tested). Exit criterion: holding Super and tapping 2, 2, 2 then
    letting go lands on the second item's fourth most recent window (the
    focused one counts as the first), with no key
    lost on niri, and a quick tap of Super+2 leaves a usable strip.
17. **Applications.** `grid.rs` and the shared scroll physics (tested),
    the Applications item and popup, the headers, swiping between
    categories, scrolling, the keyboard, launching, typing switching to
    the launcher, and the animations. `wl-dock --apps`. Exit criterion:
    swiping between categories follows the fingers and settles without a
    dropped frame, and a long category scrolls like a native list.
18. **Polish.** README, measurements (idle CPU, frame times while
    magnifying), and an icon sharpness pass.

## Future

- **Intellihide.** Hide only when a window overlaps the dock. This needs
  window geometry from the compositor's IPC, through a line stream like
  `output-event-stream`.
- Scrolling on an icon to cycle through its windows (when no `scroll-*`
  actions are bound), badges, and a `top` position.
- **More for files in the launcher.** Tab for file actions: Open, Open
  With (the apps whose `MimeType=` matches), Show in Folder, Copy Path,
  Move to Trash. Tab on a folder could make it the root. Recent files
  (`recently-used.xbel`) could rank first, and with an empty file query
  fill the drum. Several roots.
- **More launcher sources:** windows by title (activate any window),
  a calculator, web searches, and commands from the config. They would be
  providers behind one interface, with apps the first.

## Decisions

- **No daemon crate, but a socket.** The dock is always on screen and runs
  its own loop. Pinning goes through a file and inotify. Only `--launcher`,
  `--apps` and `--activate`, which must act at once, go through a socket,
  using the daemon crate's IPC code.
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
- **The strip opens on click, never on hover.** Ctrl+click activates the
  most recent window directly. `strip = false` makes a plain click do that.
- **The strip is a grid relative to the dock**, capped by
  `strip-max-columns` and `strip-max-rows`, and its cards keep a stable
  order so their positions can be learned.
- **The Trash removes from the dock; dragging off doesn't.** A slip off
  the dock never loses an icon. A dock item dropped on the Trash only
  leaves the dock; only files from outside the dock are moved to the
  trash.
- **Only folders are pinned, never files.** A file dragged to the dock is
  moved (or copied) into a pinned folder, opened with an app, or trashed;
  the space between icons refuses it. This keeps the dock to apps and
  places, with files living in folders.
- **The grid has a fixed size**, from `apps-columns` and `apps-rows`, so
  the popup never resizes between categories, and cells stay where they
  were learned.
- **The launcher is part of the dock.** It shares the app index, icons,
  launch history and menu entries, and morphs from the dock's panel,
  instead of being a separate app.
- **The launcher needs a bottom dock.** The bar grows out of the panel
  and the matches roll around the dock, which doesn't work along a side,
  so a side dock disables the launcher instead of moving it elsewhere.
- **The launcher's matches are a drum, not a list.** Icons with names, the
  selection in front and the rest receding over the top, so the bar
  stays a bar and moving through the matches is a roll, not a highlight
  jumping down a menu.
- **File search has no index.** Each search walks the disk anew
  (`ignore`) and matches as it goes (`nucleo`), streaming the top matches
  to the drum. Nothing runs, and nothing is stored, while the launcher
  isn't searching files, results are never stale, and there is no
  `plocate` or daemon to set up. The price is a cold first walk, which
  streaming hides.
- **Files are behind a trigger, not mixed with apps.** A leading `'`, as
  in Alfred, so app searches stay instant and never touch the disk, and
  the drum never mixes two rankings.
- **Keyboard activation is held, not toggled.** The compositor binds
  Super+n to `--activate n`; the dock then holds the keyboard until the
  modifier is released, which activates the highlighted window, as an
  Alt+Tab switcher does.
