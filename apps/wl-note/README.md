# wl-note

Sticky notes for Wayland, tucked behind the edge of your screen, that follow
the app you're working in. Your browser gets its notes, the terminal gets its notes, and notes that name no app are always there.

Notes are plain Markdown files in a folder you choose, so you can edit, sync,
grep and version them like any other files.

## Getting started

From the repository root:

```sh
cargo install --path apps/wl-note
wl-note
```

Add `wl-note` to your compositor's startup commands, for example on niri:

```kdl
spawn-at-startup "wl-note"
```

Build dependencies are a Rust toolchain, a C toolchain, pkg-config, libgbm and
libxkbcommon development packages. Your compositor needs layer-shell,
viewporter, `ext-foreign-toplevel-list-v1`,
`ext-foreign-toplevel-image-capture-source-v1` and `ext-image-copy-capture-v1`.

To make notes follow the focused window, wl-note needs to be told which window
has focus; see [Following the focused window](#following-the-focused-window).
Without it, you only see global notes.

Bind a key to make a note for the app you're in:

```kdl
Mod+N { spawn "wl-note" "new"; }
```

## Using it

### The column

Each note is a sheet of colored paper along the screen edge (the right one by
default). How far the notes come out depends on where the pointer is:

- **Away from the edge**, each note is a small colored marker, quiet enough to
  leave on screen all day.
- **At the edge**, every note slides out a little and shows its title in the
  margin.
- **Resting on a note** slides its whole sheet out to read. It doesn't take
  the keyboard, so browsing never steals focus from your window. Move along
  the column to read the others.
- **Clicking a note** opens it to edit. It stays open until you close it,
  whatever you do elsewhere: click its margin, or press Escape or Ctrl+Enter.
  Clicking another window just gives that window the keyboard back.

The column is split into groups, a little apart: global notes first, then the
notes for the focused app, then scratch notes for the focused window.

Drag a note's margin along the column to reorder it. Drag it from the app group
into the global group to show it everywhere, or the other way to attach it to
the focused app.

### Writing

An open note is a Markdown editor that styles the text as you type, while
keeping the syntax visible on the line you're on:

- `# Headings`, `**bold**`, `*italic*` and `` `code` ``.
- `- [ ]` checklists: click a box to tick it, also on a sheet you're only
  reading. Enter continues a list, Enter on an empty item ends it, and Tab and
  Shift+Tab indent and outdent.
- Links: click to open them.
- The usual keys: Ctrl+Z / Ctrl+Shift+Z, Ctrl+C/X/V, Ctrl+A, word movement
  with Ctrl, Shift to select, double and triple click.

A note's title is its first line if it is a heading, otherwise its file name.

Changes are saved half a second after you stop typing, on Ctrl+S, and when the
note closes. Ctrl+E opens the note in your own editor (`editor.command`). If
the file changes on disk while you have unsaved edits, the note asks whether
to reload or keep yours, so nothing is lost silently.

### The bottom bar

An open note has a row of icons along its bottom:

| Icon        | Does                                                                     |
| ----------- | ------------------------------------------------------------------------ |
| Clock       | Set when it's due: in an hour, tonight, tomorrow, next Monday, or clear  |
| Color dot   | Pick another paper color                                                 |
| Trash       | Move it to the Trash; click twice to confirm                             |
| Archive     | Hide it (see [Hiding](#hiding-and-undo))                                 |
| Pin / unpin | Attach it to the focused app (Shift+click: to this window), or detach it |

### Making notes

- **`+`**, the round button after the last note, makes a note for the focused
  app. Shift+click makes a scratch note for the focused window.
- **Rest on `+`** and a row of buttons slides out beside it, showing only the
  ones that have something to do:

  | Button    | Does                                                    |
  | --------- | ------------------------------------------------------- |
  | Undo      | Undo the last hide or trash, for a few seconds after it |
  | Window    | A scratch note for the focused window                   |
  | Global    | A note shown everywhere                                 |
  | Paste     | A note from the clipboard: text, a URL, or copied files |
  | Templates | A note from a template                                  |
  | Hidden    | Find and bring back hidden notes                        |
  | Due       | Notes with a due time, overdue ones first               |

- **`wl-note new`** from a key binding (see [The CLI](#the-cli)).
- **Drop files** on the column's empty space or on `+`.

A new note opens right away. One you close without writing anything is
discarded.

### Files on notes

Drop files from your file manager onto a note and they become links in it, with
icons in the note's bottom bar. Click an icon to open the file, or drag it out
again later, into an email, a chat or an upload form. Ctrl+click selects
several icons to drag together. Files are never copied or moved: the note only
holds `file://` links. A link whose file is gone shows struck through, and
hovering it offers a `×` to remove it.

### Scratch notes

A scratch note belongs to one window rather than an app: the scratch pad for
one particular terminal. It shows only while that window has focus, and is
white by default, so it stands out from kept notes. When the window closes,
its scratch notes go to the Trash (empty ones are deleted). On a scratch
note, the pin icon is **Keep**: it turns the note into an ordinary note for
the app. You can also drag it into the app or global group.

### Hiding and undo

The **archive** icon hides a note: the file stays as it is, but the note no
longer shows. It's for notes that are done for now but worth keeping. Bring
them back from the **Hidden** button beside `+`: type to filter, Enter to show
the note again (Ctrl+Enter to bring back several), Tab to switch between the
notes hidden here and all of them.

Right after you hide or trash a note, an **Undo** button shows beside `+` for
five seconds. Trashing only really happens once that time is over.

### Reminders

Give a note a due time with its clock icon or `wl-note due`. Once it passes,
the note's marker and margin turn red and pulse once, so the note becomes a
reminder. This works across suspend. Clear the due time, hide the note or
trash it when you're done. The **Due** button beside `+` lists every note
with a due time, hidden ones included.

### Templates

Put Markdown files in `~/Notes/wl-note/.templates/` and a **Templates** button
appears beside `+`. Picking one makes a new note from it. `{date}`, `{time}`,
`{app}` and `{window}` (the window title) are filled in:

```markdown
+++
color = "blue"
+++

# Meeting {date}

- [ ]
```

Unlike `+`, a template doesn't attach its note to the focused app. The note
is global unless the template names `apps`, `match` or `tags`. To attach it
to whatever is focused, use the placeholders there:

```markdown
+++
apps = ["{app}"]

[[match]]
title = "*{window}*"
+++
```

Drop files on a template in the open Templates list to make a note from it
with the files linked.

## Notes on disk

Notes live in `~/Notes/wl-note` (`notes-dir`), one `.md` file per note.
Subfolders are read too, so you can organize them by hand. Changes made
elsewhere, by your editor, a sync tool or `git pull`, show up immediately.

A note is Markdown with optional TOML frontmatter between `+++` lines:

```markdown
+++
apps  = ["firefox"]
color = "yellow"
due   = 2026-11-20T09:00
+++

# Release checklist

- [ ] bump the version
- [ ] tag and push
- [report.pdf](file:///home/isak/Documents/report.pdf)
```

Every key is optional. A file without frontmatter is a yellow note shown
everywhere.

| Key     | Meaning                                                                              |
| ------- | ------------------------------------------------------------------------------------ |
| `apps`  | App IDs the note belongs to; globs like `org.mozilla.*` work                         |
| `match` | Finer matching on the app ID and the window title (see below)                        |
| `color` | `yellow`, `orange`, `pink`, `purple`, `blue`, `green`, `gray`, `white`, or `#rrggbb` |
| `due`   | A date (`2026-11-20`) or local datetime (`2026-11-20T09:00`)                         |

To match a window's title, for example a terminal in a project or a browser
tab, add `[[match]]` tables. Both `app` and `title` are globs; given both,
both must match. Titles ignore case.

```toml
[[match]]
app   = "foot"
title = "*wl-shell*"

[[match]]
title = "*Jira*"          # In any app
```

A note is shown when any of its `apps` or `match` entries matches the focused
window. A note with none is global. Use `wl-note list --app <id>` to check
what a window would show; your compositor can tell you a window's app ID (for
example `niri msg windows`).

When wl-note changes a note (color, pin, due), it edits only that key, so your
comments and formatting are kept. The text is never reformatted. If the
frontmatter has a mistake, the note shows the error at the top and is shown
everywhere until you fix it.

## Following the focused window

wl-note learns which window has focus from a command you give it, which prints
one focused window ID per line. This is the same stream wl-switcher and
wl-dock use, so you can share one script. On niri, save this as
`~/.config/wl-note/focus-stream.sh` and make it executable:

```sh
#!/bin/sh
niri msg -j event-stream | jq --unbuffered -r '
  if .WindowFocusChanged then .WindowFocusChanged.id // empty
  elif .WindowsChanged then (.WindowsChanged.windows[] | select(.is_focused) | .id)
  else empty end'
```

Then add it to `~/.config/wl-note/config.toml`:

```toml
[integration]
mru-event-stream = "~/.config/wl-note/focus-stream.sh"
```

The IDs must match the compositor's `ext-foreign-toplevel` identifiers, as
niri's do. Focusing the dock, a launcher or wl-note itself keeps the last
window's notes, so clicking a note never takes it away.

### Multiple monitors

By default the column is on the monitor of the focused window, and moves with
it. For that, wl-note also needs a command printing `<output> <id>` whenever a
window opens on or moves to an output. On niri:

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

```toml
[integration]
output-event-stream = "~/.config/wl-note/output-stream.sh"
```

Without it, the column stays on one monitor. See [Outputs](#outputs) to pick
monitors yourself.

## The CLI

`new`, `focus`, `keep` and `show --focus` talk to the running wl-note. The
other commands work on the files directly, and the running wl-note picks up
their changes.

```sh
wl-note                          # Run (from your compositor's startup)
wl-note new                      # A note for the focused app, opened to edit
wl-note new --global             # ...shown everywhere
wl-note new --window             # ...a scratch note for the focused window
wl-note new --title "Call Bob" --color pink --due 17:30
echo "milk, eggs" | wl-note new --global --title Groceries --text -
wl-note focus [NOTE]             # Open a note; without one, the first, or close the open one
wl-note list                     # All notes but the hidden ones
wl-note list --app firefox --title "GitHub"  # What that window would show
wl-note list --hidden | --all | --scratch | --json
wl-note hide NOTE                # Hide a note
wl-note show [--focus] NOTE      # Show it again (and open it)
wl-note due NOTE WHEN|none       # Set or clear when it's due
wl-note keep NOTE                # Keep a scratch note as an app note
wl-note prune                    # Drop stale entries from the order and hidden files
wl-note path                     # Print the notes folder
wl-note outputs                  # List monitors and what the config makes of them
```

`NOTE` is a path, a file name or a title; if several notes match, you get the
candidates. `WHEN` is a date or datetime (`2026-11-20`, `2026-11-20T09:00`),
a time (`17:30`, the next one), a delay (`+30m`, `+2h`, `+3d`, `+1w`), `today`
(this evening) or `tomorrow` (in the morning).

## Configuration

Edit `~/.config/wl-note/config.toml` (or
`$XDG_CONFIG_HOME/wl-note/config.toml`), or pass `--config PATH`. Every setting
is optional. To start from a template listing every setting with its default:

```sh
mkdir -p ~/.config/wl-note
cp apps/wl-note/config.example.toml ~/.config/wl-note/config.toml
```

Sizes are pixels and times are milliseconds. The most useful settings:

```toml
notes-dir = "~/Notes/wl-note"
output = "focused"                 # focused, all, or an output pattern
edge = "right"                     # left or right
layer = "top"                      # top or overlay

[column]
reveal-delay = 120                 # Resting on a note before its sheet slides out
hide-delay = 400                   # After the pointer leaves
reserve-edge-band = false          # Keep windows off the 2 px strip at the edge

[sheet]
width = 300
height = 220
tilt = 1.5                         # Degrees; 0 for straight notes
font = "sans-serif"
font-size = 13
default-color = "yellow"

[editor]
command = "xdg-open {path}"        # Ctrl+E, e.g. "foot nvim {path}"

[files]
open-command = "xdg-open {path}"
trash-command = "gio trash -- {paths}"

[scratch]
color = "white"
on-close = "trash"                 # trash, delete, or keep as an app note

[due]
morning = "09:00"                  # "Tomorrow" and "next week"
evening = "18:00"                  # "Tonight"
everywhere = false                 # Also show overdue notes in every app

[row]
buttons = ["undo", "window", "global", "paste", "templates", "hidden", "due"]
undo-time = 5000                   # 0 disables undo
```

The `[appearance]` table sets the paper and text color of each preset
(`paper-yellow`, `paper-yellow-text`, …), shadows, the overdue red and more.
Paper stays paper in dark mode, only a little dimmer (`dark-dim`). Put keys in
`[appearance.dark]` or `[appearance.light]` to override them for one color
scheme. Commands run through `sh -c`; install `gio` for the default Trash
command. Paste can read another app's clipboard without focus only if the
compositor supports `ext-data-control`.

### Outputs

`output = "focused"` (the default) puts the column on the focused window's
monitor; an open note stays where it is until closed. `output = "all"` puts a
column on every monitor. A pattern puts it on one monitor only. A pattern is a
connector name (`eDP-1`), a monitor's description or `<make> <model>`
(`"Dell Inc. DELL U2720Q"`), or a glob over those (`"Dell*"`, `"*"` for all).
Run `wl-note outputs` to see what to write for yours.

`[outputs.<pattern>]` tables set `enabled`, `edge` and `layer` per monitor.
Every matching table applies, least specific first. For example, to keep notes
off the laptop screen and on the left of a particular monitor:

```toml
[outputs."eDP-1"]
enabled = false

[outputs."Dell Inc. DELL U2720Q*"]
edge = "left"
```
