https://github.com/user-attachments/assets/852dbf5b-8bfb-4da7-adcb-e5ed236289c5

# wl-expose

A window overview (like macOS Exposé) for Wayland. It shows your windows side by
side with live previews, you pick one, and it prints that window's ID.

It doesn't focus the window itself. You pair it with your compositor's IPC in a
small script. It needs a compositor that supports the ext toplevel list and image
capture protocols. I've tested it on umbriel, and niri should work too.

## Usage

```sh
wl-expose [OPTIONS] [ID...]
```

Pass the window IDs you want to show, or nothing to show all windows. If you pick
one, its ID is printed and wl-expose exits 0. If you press Escape or click the
background, it exits 1. `wl-expose --list` prints every window's ID, app ID and
title.

For example, this script shows the windows on the current niri workspace and
focuses the one you pick:

```sh
ws=$(niri msg -j workspaces | jq '.[] | select(.is_focused) | .id')
ids=$(niri msg -j windows | jq -r ".[] | select(.workspace_id == $ws) | .id")
id=$(wl-expose $ids) && niri msg action focus-window --id "$id"
```

### Controls

- Mouse: hover to select, click to pick, click the background to cancel
- Arrow keys or `hjkl`: move between windows
- `Tab` / `Shift+Tab`: cycle through windows
- `1`–`9`: pick a window by position
- `Enter` picks, `Escape` cancels

### Options

```
  -l, --list              print identifier<TAB>app_id<TAB>title per window
  -s, --select ID         start with this window selected
      --static            capture a single frame instead of live previews
  -c, --cursors           draw cursors in the previews
  -t, --timeout MS        how long to wait for window sizes (default 500)
      --config PATH       with --daemon: use another config file
      --daemon            run the daemon that shows the overlay
```

## Daemon

The overlay is shown by a daemon, so start `wl-expose --daemon` at login. In niri:

```kdl
spawn-at-startup "wl-expose" "--daemon"
```

`wl-expose` then hands each request to the daemon; without one running it fails
with an error (`--list` works without it). Running it while the overlay is open
closes the overlay, so you can use one key binding to toggle it. The daemon
reloads the config when the file changes.

## Configuration

The config file is `~/.config/wl-expose/config.toml`. All keys are optional.
Colors are `RRGGBB` or `RRGGBBAA`. The defaults are:

```toml
gap = 24                    # minimum space between windows, px
margin = 48                 # space around the layout, px
zoom = 1.08                 # zoom on the selected window; 1 disables
zoom-duration = 150         # ms
max-scale = 1.0             # max preview scale
open-duration = 400         # opening animation, ms; 0 disables
close-duration = 150        # closing fade-out, ms; 0 disables
card-scale = 0.9            # card size relative to the screen
icon-size = 48              # px; 0 disables icons
titles = true               # show the selected window's title
font = "sans-serif"
font-size = 14
# mru-event-stream = "..."  # see "Focus order"

[appearance]
mode = "auto"               # "auto" (follow the desktop), "dark" or "light"
background = "000000b3"
# background-image = "~/Pictures/wall.png"
background-blur = 0
card-color = "303030ff"
placeholder = "3a3a3aff"    # shown for windows without a preview
shadow-color = "00000077"
shadow-softness = 30
shadow-spread = 5
shadow-offset = [0, 5]
icon-shadow-color = "00000059" # follows the icon's shape; 00000000 disables
icon-shadow-softness = 6
icon-shadow-offset = [0, 2]
title-color = "ffffffff"
title-background = "000000b3"
# icon-theme = "Adwaita"

[appearance.light]
background = "ffffff80"
card-color = "f0f0f0ff"
placeholder = "d0d0d0ff"
title-color = "000000ff"
title-background = "ffffffb3"
```

Keys in `[appearance.dark]` and `[appearance.light]` override `[appearance]` for
that color scheme. If you set a background image, `background` tints it. Set
`background` to `00000000` to show the image without a tint.

### Focus order

The daemon can track which windows were focused most recently. The windows are
then shown in that order. The focused window comes first and starts selected,
so `Tab` moves to the previously focused window, `Shift+Tab` to the oldest, and
`1`–`9` count in focus order. The layout doesn't change. Set
`mru-event-stream` to a command that prints the ID of each window that gains
focus, one per line:

```toml
mru-event-stream = "~/.config/wl-expose/focus-stream.sh"
```

For niri:

```sh
#!/bin/sh
niri msg -j event-stream | jq --unbuffered -r '
  if .WindowFocusChanged then .WindowFocusChanged.id // empty
  elif .WindowsChanged then (.WindowsChanged.windows[] | select(.is_focused) | .id)
  else empty end'
```

The daemon runs it with `sh -c` and restarts it if it exits. IDs passed on
the command line still decide which windows are shown. Windows never seen
focused come last, in the order given. Without a stream, the order is the
one given on the command line.

## Building

```sh
cargo build --release
```

You need Rust 1.88 or newer, plus `libxkbcommon` and `libgbm`.

