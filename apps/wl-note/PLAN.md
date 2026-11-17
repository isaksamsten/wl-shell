# wl-note plan

Sticky notes along a screen edge, built on the same runtime as wl-dock.
Notes look like sheets of colored paper tucked behind the edge of the
monitor. Only a sliver of each sticks out. Hovering the edge slides them
out far enough to read. Clicking one brings it forward, zoomed, to read and
edit.

What sets wl-note apart: **notes belong to applications.** A note's
frontmatter says which windows it belongs to, by app ID and title. As focus
moves between windows, the notes at the edge change with it. The browser
gets its notes, the terminal in `~/projects/wl-shell` gets that project's,
and notes that name no app are always there.

Notes are plain markdown files with TOML frontmatter in a directory you
choose, so they can be edited, synced and grepped like any other files. A
note's file says what the note is (its text, color and which apps it
belongs to). How you arrange your notes (their order, and which are
hidden) is kept in a state file, like the dock's pins.

Notes can also hold **files**. Drop documents onto a note, and they become
links on it. Drag them out again later, e.g. into an email you're writing.

Like the dock, wl-note is always on screen and runs its own loop, started
with the compositor (niri's `spawn-at-startup`). The CLI reaches it through
the daemon crate's socket code.

## Notes on disk

### The directory

`notes-dir` (default `~/Notes/wl-note`) holds one `.md` file per note.
Subdirectories are read too, so notes can be organized by hand. Files
starting with `.` and anything that isn't `.md` are ignored. The directory
is watched with inotify through a new `wl_shell_config::watch_tree` (see
[Shared code](#shared-code)), so notes created, edited, renamed or
deleted elsewhere show up at once.

### The format

```markdown
+++
title = "Release checklist"
apps  = ["firefox"]
color = "yellow"
+++
- [ ] bump the version
- [ ] tag and push
- [report.pdf](file:///home/isak/Documents/report.pdf)
```

The frontmatter is TOML between `+++` lines (Hugo's convention), at the very
start of the file. A file without it is a valid note: a global, yellow
note. All keys are optional:

| Key      | Type                     | Meaning                                                 |
| -------- | ------------------------ | ------------------------------------------------------- |
| `title`  | string                   | shown on the tab and the peek; else the first heading, else the file name |
| `apps`   | list of app ID globs     | shorthand for `[[match]]` tables with only `app`        |
| `match`  | array of tables          | see [Matching](#matching)                               |
| `tags`   | list of strings          | matched through `[tag.<name>]` in the config (phase 11) |
| `color`  | preset name or `#rrggbb` | the paper's color                                       |

Order and hidden aren't frontmatter keys: they live in the state file
(see [State](#state)).

Unknown keys are kept as they are. When wl-note changes the frontmatter
(color, attaching), it edits it with `toml_edit`, so
comments, key order and formatting survive. The body is never reformatted.
Writes are atomic (a temp file in the same directory, then a rename).

A frontmatter that doesn't parse leaves the note shown as a plain global
note, with a small warning mark on its tab and the error in its peek. The
file is never rewritten while its frontmatter is broken.

### State

How the notes are arranged lives outside them, in
`$XDG_STATE_HOME/wl-note/` (default `~/.local/state/wl-note/`), the same
way as the dock's `pinned` file. So reordering or hiding never rewrites a
note: no mtime churn, no sync conflicts, no noise in a notes repository,
and no inotify echoes to filter out.

```
~/.local/state/wl-note/order     # notes in column order, one per line
~/.local/state/wl-note/hidden    # hidden notes, one per line
```

- **Format.** One path per line, relative to `notes-dir`
  (`work/release.md`). Blank lines and `#` comments are ignored, as in
  `pinned`. Paths that name no note are kept and ignored, since a note
  may be away briefly (during a sync, or an editor's save by rename).
  Stale lines are harmless; `wl-note prune` drops them.
- **Order.** The column shows notes in the file's order. Notes not in it
  come after, newest file first, so a note made outside wl-note shows up
  right after the arranged ones. A note created in wl-note is appended to
  the file. The file is one order for all notes: each context shows its
  subset in that order, so a note shown for several apps keeps its place
  relative to the others everywhere.
- **Writing.** Atomic (a temp file, then a rename), whenever the
  arrangement changes in wl-note. Both files are watched with inotify, so
  editing them by hand or from a script works, as with the pins. A missing
  file means no order, or nothing hidden.
- **Renames.** Entries are keyed by path, so renames are followed: wl-note's
  own renames update the files directly, and an inotify
  `IN_MOVED_FROM`/`IN_MOVED_TO` pair inside `notes-dir` (the same cookie)
  moves the entries. A rename made while wl-note isn't running is lost:
  the note falls back to shown, after the ordered ones. That is the price
  of keeping the files clean, and an acceptable one.
- **Not synced.** The arrangement is per machine. Notes synced to another
  machine show there with its own order, and hidden notes show there
  until hidden there too.
- **Shared.** `wl-shell-notes` reads and writes the state (`State`), so the
  CLI and the dock's launcher use the same code. `wl-note hide` and
  `show` only edit the file and need no socket: the running wl-note
  picks the change up through inotify, as with `wl-dock pin`.

Tests (`wl-shell-notes`): parsing with comments and blank lines, notes
missing from the order going last by mtime, stale paths ignored and
pruned, and rename pairs moving entries.

### Parsing (`crates/wl-shell-notes`, pure, tested)

The format, matching and the index are a crate of their own, so wl-dock's
launcher can read notes too (see [The dock's launcher](#the-docks-launcher))
without depending on wl-note:

- `Note::parse(&str) -> Note` (frontmatter, body, the error if any) and
  `Note::set(key, value)`, which edits the frontmatter in place.
- `Matcher` and `Note::matches(&Window) -> bool`.
- `Index`: every note in the directory, loaded and updated from inotify
  events.

Tests: a missing frontmatter, a broken one, unknown keys and comments
surviving `set`, CRLF files, a `+++` inside the body, and the matching
rules below.

## Matching

### The focused window

wl-note uses the `mru-event-stream`, as wl-switcher and the dock do
(`Core::use_focus_stream`, `Program::focus_changed`). The stream gives the
focused window's ext identifier, and `Core::toplevels()` gives its app ID
and title. Together these make the **context**: the window the notes follow.

- **Title changes** of the context's window (`Program::toplevel_update`)
  re-match, so a terminal that `cd`s into a project, or a browser switching
  tabs, changes its notes.
- **Settling.** A context change only shows after it has held for
  `settle` (default 120 ms), so cycling with alt-tab or skimming through
  browser tabs doesn't make the notes flicker.
- **Things that aren't windows** don't change the context. Focus on
  wl-note's own surfaces, the dock, the launcher or a switcher (layer
  surfaces, which the stream doesn't report) keeps the last window's
  notes. This matters: clicking a note must not take that note away.
- **The context closes.** The context becomes the most recent window left
  (`Core::mru.first()`), or none.
- **No stream.** Without `mru-event-stream`, only global notes are shown,
  and a warning is printed once.

### Rules

A note is shown for the context when it isn't in the `hidden` state file
and:

- it has no `apps`, `match` or `tags` (a **global** note: shown
  everywhere, also when there is no context), or
- any of its matchers matches the context's window.

A matcher is a table with `app` and/or `title`. Both are globs (`*`, `?`,
`[…]`). Both given means both must match:

```toml
apps = ["firefox", "org.mozilla.*"]

[[match]]
app   = "foot"
title = "*wl-shell*"

[[match]]
title = "*Jira*"          # any app
```

- `app` matches the app ID as is, case-sensitively, like the dock's
  grouping key. `title` ignores case.
- A glob that doesn't parse makes that one matcher never match, with the
  warning mark on the note.
- `title-regex` is a later option if globs turn out too weak.

Tests (`wl-shell-notes`): globals, app and title globs, both required,
case rules, broken globs, and hidden notes never matching.

### Tags (phase 11)

Tags are an indirection: the config says what a tag matches, and a note
with that tag follows it. One place to say what "browser" means for all
the notes that care:

```toml
[tag.browser]
match = [{ app = "firefox" }, { app = "chromium" }, { app = "org.qutebrowser.*" }]

[tag.wl-shell]
match = [{ app = "foot", title = "*wl-shell*" }, { app = "dev.zed.Zed", title = "*wl-shell*" }]
```

A note matches if any of its own matchers or any of its tags' matchers
match. A tag the config doesn't define matches nothing. This also keeps app
IDs out of notes that should outlive a switch of browser.

## Behavior

### States

Each output that shows notes (see [Outputs](#outputs)) has one column of
notes along `edge` (default `right`). The column is in one of three
states:

```
 tucked            peeking                    focused
                                          ╭──────────────╮
      ▕█             ╭──────────▕         │ Release      │
      ▕█             │ Release  ▕         │ checklist    │
      ▕█             ╰──────────▕         │              │
      ▕█             ╭──────────▕         │ ☐ bump the   │
      ▕█             │ Groceries▕         │   version    │
                     ╰──────────▕         │ ☐ tag and    │
                                          │   push       │
                                          │ [📄 report]  │
                                          ╰──────────────╯
```

- **Tucked.** Every shown note is a sheet of paper behind the edge, with
  `tuck` (default 10 px) sticking out: the colored sliver of the paper, its
  torn or curled edge facing the screen. The slivers are stacked along the
  edge, centered, `tab-gap` apart. Nothing else is visible and nothing
  else takes input, so the rest of the screen is untouched.
- **Peeking.** The pointer resting on a sliver for `reveal-delay`
  (default 120 ms) slides the whole column out on a spring, to `peek`
  (default 180 px) of each note: enough to read its title and first lines.
  The note under the pointer comes out a little farther and lifts (a
  stronger shadow), so you can see which one a click picks. Leaving the
  column for `hide-delay` (default 400 ms) tucks it back.
- **Focused.** Clicking a note brings it forward. It grows on a spring to
  `focus-width` × up to `focus-height`, slides fully onto the screen, and
  takes the keyboard. The other notes tuck back. Escape, clicking
  outside, or clicking its sliver again tucks it back too. One note is
  focused at a time; clicking another one switches.

The column centers its slivers on the edge. With more notes than fit at
`tuck`, they overlap like a deck: they fan out while peeking, and the
column scrolls with the wheel if they still don't fit.

### When the context changes

- The notes that stay are left in place, so a note shown for several
  apps never moves.
- Notes that leave slide back behind the edge, and new ones slide out of
  it, staggered by their position on the spring. The column then closes
  up.
- A focused note stays focused if it matches the new context, which is the
  usual case since focusing a note doesn't change the context. If it
  doesn't match, it is saved and tucks away with the others.
- While peeking, the column changes in place under the pointer.

### Order

Notes follow the `order` state file (see [State](#state)). Dragging a
sliver or a peeking note along the column reorders it: a gap opens under
the pointer and the neighbors slide apart on springs, as in the dock. A
drop moves the note in the order file, just before the next note shown,
so notes not shown in this context keep their places. No note file is
touched.

The rules are wl-note's own, in `reorder.rs` (pure, tested): where the gap
goes for a pointer position along the column, and where a drop lands in
the full order. wl-dock's `drag.rs` isn't reused, since it is mostly about
pins, the Trash, folders and file drops. If the gap maths turns out the
same in both, it can move to `wl-shell-ui` then.

### Pointer and keys

| Input                          | Effect                                                    |
| ------------------------------ | --------------------------------------------------------- |
| hover a sliver                 | peek after `reveal-delay`                                 |
| click a note                   | focus it                                                  |
| click outside, Escape          | tuck back (saving)                                        |
| drag a note along the column   | reorder                                                   |
| right click a note             | its menu                                                  |
| click a checkbox               | toggle it (also when only peeking)                        |
| click a link                   | open it with `open-command`                               |
| drop files on a note           | link them in it (see [Files](#files))                     |
| drag a file chip out           | offer the file (see [Files](#files))                      |
| Ctrl+Enter, focused            | tuck back                                                 |
| Ctrl+E, focused                | open the file in `editor-command`                         |

### The menu

The dock's speech bubble, pointing at the note. It comes from the shared
menu that phase 5 extracts from wl-dock into `wl-shell-ui` (see
[Shared code](#shared-code)):

1. **Color**: a row of swatches, one per preset.
2. **Attach to <app>** (adds the context's app ID to `apps`), **Attach to
   this window** (adds `{ app, title }` with the current title, which the
   user will usually loosen by hand), **Detach from <app>**, **Show
   everywhere** (removes every matcher). Only the rows that change
   something are built.
3. **Open in editor**, **Show in folder**.
4. **Hide**, **Move to Trash** (`trash-command`, as in the dock; never a
   plain delete).

### Creating notes

- `wl-note new` (bind it to a key) creates a note attached to the context's
  app ID (or a global one with `--global`, or no context), and focuses it
  empty, ready to type. `--color`, `--title` and `--text` fill it in, and
  `--text -` reads the body from stdin.
- A **+** sliver at the end of the column, shown while peeking, does the
  same with the pointer.
- **Dropping files** on the column's empty space creates a note with them
  linked (see [Files](#files)).
- The file is named after the time (`2026-11-17-2059.md`, with `-2` and so
  on if taken). It isn't written until it has content: a note left empty
  is discarded when it tucks back.

### Hiding

Hiding keeps a note on disk but never shows it, by listing it in the
`hidden` state file. This is for notes that are done for now but worth
keeping, so that the column only holds what's current. A hidden note's
file is unchanged, so hiding is never a sync conflict.

- **Hide** in the menu, or `wl-note hide <note>`, adds it. The note
  crumples (shrinks and fades into the edge) and the column closes up.
- **Getting them back:** the CLI (`wl-note list --hidden`,
  `wl-note show <note>`) and the dock's launcher (see
  [The dock's launcher](#the-docks-launcher)). A note shown again from
  the launcher or the Hidden list is focused at once, even if it doesn't
  match the context, so the user sees where it went. `wl-note show` only
  edits the file; `--focus` also focuses it through the socket.
- A peeking column ends with a faint **Hidden (n)** sliver when any notes
  hidden from this context exist. Clicking it opens a popup listing them
  (the dock's menu look: title, color dot, first line). Picking one shows
  it again. (Phase 10, after the launcher integration, if it still seems
  needed.)

## Editing

Editing comes in two steps.

**First (phase 3): reading, with outside editing.** The focused note shows
its markdown rendered. Checkboxes and links work, and Ctrl+E (from phase
6 also the menu) opens the file with `editor-command` (default
`xdg-open {path}`, e.g.
`foot -e nvim {path}`). inotify brings the changes back.

**Then (phase 7): a built-in editor**, since quick notes shouldn't need
another window. It edits the markdown source, styled as you type
(Obsidian's live preview, simplified), and never hides the syntax from the
line the cursor is on:

- Headings are larger and bold, with `#` dimmed. `**bold**`, `*italic*`
  and `` `code` `` are styled, with their markers dimmed.
- `- [ ]` and `- [x]` show as checkboxes off the cursor's line. A checked
  item's text is struck through and dimmed.
- Lists get hanging indents. Enter continues a list (and a checkbox list
  with `- [ ] `), and Enter on an empty item ends it. Tab and Shift+Tab
  indent and outdent items.
- Links show their text, underlined, off the cursor's line. File links
  that are alone on their line show as chips (see [Files](#files)).
- The usual keys: arrows, Home/End, Ctrl+arrows by word, Shift to select,
  Ctrl+A, Ctrl+Z/Ctrl+Shift+Z, Ctrl+C/X/V, and mouse selection with
  double and triple click.
- **Clipboard:** `set_selection` exists in core already. Pasting needs the
  selection offer read (`wl_data_device.selection`), which core's
  `dnd.rs` gains.
- **IME** through `zwp_text_input_v3` is later (phase 12). Until then,
  dead keys and compose work through xkb, which core already does.

The editor is cosmic-text's `Editor` over an attributed `Buffer`, with the
markdown spans from `pulldown-cmark` (offsets kept) re-applied to the
changed paragraphs on each edit. The **focused note's text is laid out and
drawn on the main thread**, not the worker. A keystroke must show in the
next frame, and a worker roundtrip per key would add a frame. It's one note
of a few hundred words, so it is cheap. Everything else (slivers, peeks,
the paper) is the worker's.

**Saving.** Edits are written `save-delay` (default 500 ms) after the last
change, and on tucking back. A note's own writes are recognized (by the
content hash written) so they don't reload it.

**Changes on disk.** If the file changes on disk while focused:

- with no unsaved edits, the note reloads in place, keeping the cursor on
  the same line if it can;
- with unsaved edits, the editor keeps them and shows a bar on the note:
  "Changed on disk — Reload / Keep mine". Keep mine overwrites the file.

Tests (`editor.rs`, `markdown.rs`): list continuation and ending,
indenting, checkbox toggling as text edits, styles that don't hide the
cursor's line, undo grouping, and reload keeping the cursor.

## Files

A note can hold links to files, so it works as a shelf. You can drop the
documents for a task on that task's note and drag them into an email,
an upload form or a chat later.

### What's stored

Only links, in the markdown, so the note stays plain markdown and the files
stay where they are:

```markdown
Docs for the Q4 review:

- [report.pdf](file:///home/isak/Documents/report.pdf)
- [budget.ods](file:///home/isak/Documents/budget.ods)
```

A `file://` link that is alone on its line (optionally as a list item)
shows as a **chip**: the file's thumbnail or MIME icon (the dock's
`wl-shell-render::files` and `mime`), its name and a dim parent
directory. Other `file://` links show as plain links. A chip whose file is
gone shows faded, with its name struck through, and its menu offers
**Remove**.

`attach-mode = "copy"` copies dropped files into `attachments/` next to
the note instead, and links the copies, so the note keeps them whatever
happens to the originals. The default is `"link"`.

### Dropping in

wl-note is a drop target for `text/uri-list` (local `file://` URIs only),
like the dock (`Program::DATA_DEVICE`):

- **On a tucked or peeking note:** a drag reaching the column peeks it
  (no delay), and the note under the drag lifts. Dropping appends a chip
  line per file at the end of its body. If its last block is already a
  list of chips, they join it. A note that's focused and being edited
  takes the drop at the drop position instead, on its own line.
- **On the column's empty space** (or the **+** sliver): a new note,
  attached to the context, holding the files.
- Files already on the note aren't added twice.
- The drop is taken as a **copy** (`DndAction::Copy`): wl-note never asks
  the source to delete anything.

### Dragging out

- Dragging a chip starts a data-device drag (`core.start_drag`) offering
  `text/uri-list` and `text/plain` (the path), with the chip's icon as the
  drag icon, and only the copy action. The file stays linked on the note.
- **Several at once:** Ctrl+click selects chips, and dragging one of them
  drags all of them. A drag from the note's peek drags all of its chips.
- The note under a drag out doesn't tuck back until the drag ends, even if
  the pointer leaves the column. Otherwise dragging to a window under the
  notes would be impossible.
- **To check:** Thunderbird, Evolution and GTK/Qt file choosers accept
  `text/uri-list` drops with copy; and how Firefox and Chromium upload
  fields behave.

Tests (`files.rs`): what is a chip, appending to a chip list or starting
one, duplicates, the insertion point in an edited note, and building the
uri-list from a selection.

## Outputs

`output` picks where the column is:

- `"focused"` (the default): the output of the context's window, using the
  dock's `output-event-stream` (`<output> <id>` lines). The column moves
  with the focus: it tucks out on the old output and slides in on the new
  one. Without the stream, the first output.
- A name (`"DP-1"`): always there.
- `"all"`: a column on every output, the same notes on each. Focusing a
  note on one output tucks the others.

Settings can be overridden per output (`[output.<name>]`), as in the dock:
`enabled`, `edge`, `tuck`, `peek` and `focus-width`.

## Rendering

As in the dock: software rendering into shm buffers, every element a sync
subsurface scaled with `wp_viewporter` and faded with
`wp_alpha_modifier_v1`, frames driven by frame callbacks only while a
spring moves. An idle column costs nothing.

### Surfaces

```
column: layer surface per shown output (layer `top`, anchored to the edge
        and both sides, exclusive zone 0, keyboard `on-demand`)
├─ note × shown notes
│  ├─ shadow       soft, grows while lifted or focused
│  ├─ paper        the sheet (see below), sliced so it can grow
│  ├─ preview      title + first lines (worker text), while peeking
│  ├─ text         the focused note's text (main thread)
│  └─ chip × file links
├─ plus            the + sliver, while peeking
└─ hidden          the Hidden (n) sliver, while peeking (phase 10)

menu, hidden list: xdg_popups of the column with a grab (the dock's bubble)
```

- **Size.** The surface is as wide as a focused note plus its shadow, so
  focusing never resizes it. It's transparent outside the notes.
- **Input region.** Only the slivers while tucked, the column while
  peeking, and the focused note while focused. It's updated each frame
  while the springs move, so clicks go through everywhere else.
- **Keyboard.** `on-demand`, so clicking a note gives it the keyboard
  and clicking a window takes it back. Tucking back sets interactivity to
  `none` and back, as the dock does, so focus returns to the window.

### Paper (`wl-shell-render::paper`)

The worker draws each sheet with tiny-skia, at the output scale, once per
size, color and scale:

- **Color.** The preset's or the note's color, with a slight vertical
  gradient (lighter at the top, as if lit from above).
- **Grain.** A tileable value-noise texture at a few percent of contrast,
  generated once and cached. It's what makes the sheet read as paper
  rather than a rounded rectangle.
- **Edges.** Square corners, barely rounded (1–2 px). The bottom corner
  away from the edge curls: a small lighter triangle with a gradient,
  and the shadow under it pulled in, so the sheet looks lifted there.
- **Shadow.** The dock's shadow code, offset down and away from the
  edge, larger while lifted or focused.
- **The sliver** is the same sheet's edge, a little darker (it is seen
  edge-on), with a 1 px light line where the paper's thickness would
  catch the light.
- **No rotation.** Subsurfaces can't rotate. A slight per-note tilt (from
  a hash of the file name, ±1.5°) is baked into the paper and preview
  buffers while tucked or peeking, so the stack looks hand-placed. A
  focused note is always straight, since its text is drawn on top and
  must stay crisp.

Presets: `yellow`, `orange`, `pink`, `purple`, `blue`, `green`, `gray`,
`white`. Each is a paper color and a text color, redefinable in
`[appearance]`. Paper stays paper in dark mode: only the shadows and the
menu follow `[appearance.dark]`, plus a `dark-dim` (default 0.9) that
darkens the paper a little.

### Text

The **preview** (title, first lines) is drawn by the worker with
`wl-shell-render::text`, which gains a multi-line, wrapped, styled
paragraph renderer (cosmic-text `Buffer` with attributes). The focused
note's **editor** uses the same paragraph code on the main thread (see
[Editing](#editing)). The font is `font` / `font-size`. A handwriting-like
font suits the paper, but the default is the UI font, for legibility.

### Animation

- Tucking, peeking and focusing are springs on each note's offset from
  the edge, scale and shadow (`response`).
- Focusing grows the note from its peek to its focused size on one spring.
  The text lays out at the focused size from the start and is cropped,
  so it never reflows while growing.
- Context changes stagger the notes going and coming by a few frames each.
- Hiding crumples: the note shrinks toward the edge and fades.

## The CLI

The running instance listens on `wl-note-$WAYLAND_DISPLAY.sock`
(`wl_shell_daemon::listen`). Commands that only change files don't need it,
since inotify picks the changes up:

```sh
wl-note                         # run (spawn-at-startup)
wl-note new [--global] [--color c] [--title t] [--text t|-]
wl-note focus [note]            # focus a note; none: the first shown, toggles
wl-note list [--hidden|--all] [--app id] [--json]
wl-note hide <note> | show [--focus] <note>
wl-note prune                   # drop stale lines from the state files
wl-note path                    # print notes-dir
```

`<note>` is a path, a file name, or a title (the unique match, else an
error listing the candidates). `new`, `focus` and `show --focus` need the
running instance, and fail with a message without one. `hide`, `show`,
`prune` and `list` only touch the files and the state, like
`wl-dock pin`.

## The dock's launcher

Native integration, so hidden notes and notes for other apps are a few keys
away:

- A **notes source** in wl-dock's launcher behind a trigger
  (`[launcher.notes] trigger = "#"`, as files use `'`). It searches notes'
  titles and bodies (`nucleo-matcher`) through `wl-shell-notes::Index`, so
  wl-dock reads the notes directory itself and never waits on wl-note. The
  drum shows each note as a small paper swatch in its color with its title
  pill. Hidden notes are included, dimmed.
- **Enter** focuses the note through wl-note's socket (`focus <path>`),
  showing it again if hidden.
- **Tab** gives a `Subject::Note` (wl-dock's actions table): Focus, Show /
  Hide, Attach to <app the launcher was opened over>, Open in editor, Show
  in folder, Move to Trash.
- **Files on notes**: Tab on a file match gains **Add to note…**, a level
  of notes to drop the file on.

## Config

`~/.config/wl-note/config.toml`, with the same `[appearance]`,
`[appearance.dark]` and `[appearance.light]` tables as the other apps.

| Key                   | Default                | Meaning                                          |
| --------------------- | ---------------------- | ------------------------------------------------ |
| `notes-dir`           | `"~/Notes/wl-note"`    | where notes live                                 |
| `mru-event-stream`    | none                   | as in wl-switcher; without it, only global notes |
| `output-event-stream` | none                   | as in wl-dock, for `output = "focused"`          |
| `output`              | `"focused"`            | `focused`, `all`, or an output name              |
| `edge`                | `"right"`              | `left` or `right`                                |
| `layer`               | `"top"`                | `top` or `overlay`                               |
| `tuck`                | `10`                   | px of a tucked note showing                      |
| `tab-gap`             | `4`                    | px between slivers                               |
| `tab-height`          | `64`                   | px of a sliver along the edge                    |
| `peek`                | `180`                  | px of a peeking note showing                     |
| `focus-width`         | `420`                  | px                                               |
| `focus-height`        | `0.7`                  | of the output's height, at most                  |
| `reveal-delay`        | `120`                  | ms                                               |
| `hide-delay`          | `400`                  | ms                                               |
| `settle`              | `120`                  | ms a context holds before the notes change       |
| `response`            | `150`                  | ms, spring response                              |
| `tilt`                | `1.5`                  | degrees at most; 0 for straight notes            |
| `save-delay`          | `500`                  | ms after the last edit                           |
| `editor-command`      | `"xdg-open {path}"`    |                                                  |
| `open-command`        | `"xdg-open {path}"`    | links and file chips                             |
| `trash-command`       | `"gio trash -- {paths}"` | Move to Trash                                  |
| `attach-mode`         | `"link"`               | `link` or `copy` for dropped files               |
| `default-color`       | `"yellow"`             | for new notes and notes without `color`          |
| `font`, `font-size`   |                        |                                                  |
| `[tag.<name>]`        |                        | `match = [...]` (phase 11)                       |
| `[output.<name>]`     |                        | per-output overrides                             |

`[appearance]` has `paper-<preset>` and `paper-<preset>-text` for each
preset, `dark-dim`, `link-color`, `chip-background`, the `shadow-*` keys
and the menu's keys (shared with the dock).

## Shared code

Most of this is new code next to the old, so wl-switcher, wl-expose and
wl-dock don't change until they decide to use it. The one exception is
the shared menu (phase 5), which refactors wl-dock and gets a phase of
its own.

- **`crates/wl-shell-notes`** (new): the format, frontmatter editing,
  matching, the index and the state files. Used by wl-note, and by wl-dock
  from phase 9.
- **`wl-shell-render`** gains `paper` and multi-line styled paragraphs in
  `text`, as new modules and functions. The worker gains `Done` variants
  for them. wl-switcher (`app.rs`), wl-expose (`assets.rs`, `app.rs`)
  and wl-dock match `Done` exhaustively, so each gets no-op arms: about
  ten lines, all found by the compiler.
- **`wl-shell-config`** gains `watch_tree(handle, dir, f)`: it watches
  `dir` and every directory under it (adding watches as directories are
  created), and calls `f` with the events: kind (written, created,
  removed, moved from, moved to), the path relative to `dir`, and the
  move cookie, so a `MOVED_FROM`/`MOVED_TO` pair can be matched up into a
  rename. `IN_Q_OVERFLOW` is reported as `Rescan`. `watch_file` and
  `watch_dir` stay as they are, and no other app changes. Tests on a
  temporary tree: nested files, new subdirectories, renames within the
  tree as one pair, and moves in or out of it as a create or a remove.
- **`wl-shell-core`**'s `dnd.rs` gains reading the clipboard selection
  (its `selection` handler is empty today): `Core::read_selection(mime,
  f)`, like `dnd_receive`, and a defaulted `Program::selection_changed`
  hook, so other programs don't notice. Later (phase 12) core gains
  `zwp_text_input_v3`, as an optional global with defaulted hooks.
- **`wl-shell-ui`** gains the menu in phase 5 (see below). `drag.rs`
  stays in wl-dock (see [Order](#order)).
- New dependencies: `pulldown-cmark`, `toml_edit`, `globset`.

### The shared menu (phase 5)

wl-dock's menu is about 2,200 lines in two files, and only part of it is
generic:

- **`menu.rs`** has the reusable half: `Metrics`, `Layout` (rows, hit
  testing, separators), `Place` (positioner, configured tail),
  `tail_at`, `step` and `MenuKey`. It also has the dock's own: the
  `Action` enum (Pin, OpenTrash, desktop actions, …), the builders
  (`build`, `trash`, `folder`, `folder_popup`, `folder_stack`), and its
  use of `config::Position`, `dock::Item` and `strip::Key`.
- **`menuview.rs`** is typed on `Shell<WlDock>`, picks its constraint
  adjustment and tail side from the dock's `Position`, and contains the
  folder stack view (`Look::Stack`, with `stack.rs`).

The extraction:

- `wl-shell-ui::menu` gets the layout and placement half of `menu.rs`,
  with `Group` and `Entry` generic over the action type (`Entry<A>`), and
  the side as `bubble::Side` instead of the dock's `Position`. The keys
  map from core's key events, not the dock's `strip::Key`.
- `wl-shell-ui::menuview` gets the popup: bubble, rows, sliding
  highlight, open/close animation, pick flash and keyboard. It's generic
  over `P: Program` and the action type, and takes a slide axis and a
  side instead of the dock's `Position`, as `cache` was made generic in
  the dock's phase 1.
- The look stays extensible: `Look::Stack` and `stack.rs` stay in
  wl-dock, as a second look plugged in through a small trait (layout,
  timings, rendering the rows), or kept as a wl-dock wrapper around the
  shared view if a trait turns out awkward. To be decided while doing it.
- wl-dock keeps its `Action`, the builders and `drag.rs`, and uses the
  shared menu for its app menus, the folder popup and the stack.

Exit criterion: **wl-dock behaves exactly as before** (app menus, desktop
and app actions, the Trash's Empty Trash confirmation, folder popups as
list and stack, keyboard navigation, dragging files out of the popup, and
keyboard activation's popups), and all its menu, stack and placement
tests pass, moved with their code. No wl-note code in this phase.

## Phases

0. **Scaffold (done).** The crate and CLI skeleton, config, and `wl-shell-notes`:
   parsing, `set`, matching and the index over a directory, with tests.
   `wl-note list` works on the files. Exit criterion: the parser and
   matcher tests pass, and `list --app firefox` prints the right notes for
   a sample directory. `list` has `--app`, `--title` and `--json` so far;
   `--hidden` and `--all` come with the state files (phase 4). Without an
   order file, notes are newest first by creation time, so editing a note
   doesn't move it.
1. **Tucked column (done).** One layer surface on one output, the paper renderer,
   slivers for global notes, input region, inotify reload. Exit criterion:
   notes added, recolored or deleted on disk appear and change within a
   frame of the write, and an idle column uses no CPU. Until
   `watch_tree` (phase 4), every directory under `notes-dir` is watched
   with `watch_dir`, and any event rescans the tree (only changed files
   are read again; new directories get watched). The column goes on the
   output `output` names, else the first one, and doesn't take the
   keyboard yet.
2. **Context (done).** The focus stream, title updates, settling, the
   not-a-window rule, and the change animation. Exit criterion: switching
   between firefox and two terminals in different projects on niri shows
   the right notes each time, alt-tab cycling doesn't flicker them, and
   clicking a note never changes them. The context is the most recently
   focused *open* window (`Mru::most_recent` over the toplevels), so a
   focused window not announced yet keeps the old context until it is.
3. **Peek and focus, read-only.** The peek, lift and focus springs, the
   preview and the rendered focused note, checkboxes and links, Ctrl+E and
   `editor-command`, the + sliver, `wl-note new` and `focus` over the
   socket. Exit criterion: from any app, a keybinding gives a new note
   attached to it, and the round trip through `$EDITOR` shows the result
   without a restart.
4. **Order, hiding, outputs.** `watch_tree` in wl-shell-config, the state
   files (order and hidden, with inotify and rename tracking), `reorder.rs`
   and reordering by drag, hiding and showing (CLI only for now, plus
   `prune`), `output` and `output-event-stream`. Exit criterion:
   reordering or hiding never changes a note's file (its mtime stays), a
   note renamed in a file manager or by `git mv` keeps its place, and the
   column follows the focused window between two outputs.
5. **The shared menu (a wl-dock refactor).** Extract the generic menu
   from wl-dock into `wl-shell-ui` and port wl-dock to it (see
   [The shared menu](#the-shared-menu-phase-5)). Exit criterion: wl-dock
   behaves exactly as before and all its tests pass.
6. **The note menu.** Color swatches, attach and detach, Open in editor,
   Show in folder, Hide, Move to Trash. Exit criterion: color and
   matchers can be changed without an editor, a hand-written frontmatter
   keeps its comments through it, and the bubble's tail points at the
   note on left and right edges.
7. **The built-in editor.** cosmic-text editing on the main thread, live
   markdown styling, list keys, undo, clipboard (core reads the
   selection), saving, and changes on disk. Exit criterion: typing shows
   in the next frame at 2× scale, a checklist can be written without
   touching the mouse, and an edit from nvim while the note is focused is
   never lost silently.
8. **Files.** Chips, dropping onto notes and the column, dragging out
   (single and several), `attach-mode = "copy"`. Exit criterion: dragging
   a PDF from Nautilus onto a note and later from the note into a
   Thunderbird compose window attaches it, and the original file is still
   where it was.
9. **The dock's launcher.** The notes source, `Subject::Note`, Add to
   note…. Exit criterion: a hidden note is back on screen in three keys
   (`#`, a few letters, Enter).
10. **Hidden list popup**, if the launcher and CLI aren't enough in use.
11. **Tags** (`[tag.<name>]`).
12. **Polish.** IME (`zwp_text_input_v3`), the README, measurements.

## Future

- `title-regex`, and matching on the window's output or workspace.
- Notes for a **window** rather than an app, held only while that window
  lives (scratch notes for one terminal), never written to disk unless
  kept.
- Images in notes (`![](file://…)` rendered inline).
- Reminders: a `due` key that makes a note's sliver pulse.
- Syncing is out of scope; the directory works with any file sync.

## Decisions

- **Notes are the files; the arrangement is state.** Plain markdown with
  `+++` TOML frontmatter holds what a note is: its text, color, title and
  matchers. How the notes are arranged (order, hidden) is in
  `$XDG_STATE_HOME/wl-note/`, as the dock's pins are, so arranging never
  rewrites a note and never causes a sync conflict. The price: the
  arrangement is per machine, and renames made while wl-note isn't running
  lose their entries.
- **The frontmatter is edited, never regenerated** (`toml_edit`), and the
  body is never reformatted.
- **Matching is by app ID and title globs.** No app and no matchers means
  global.
- **Tags are an indirection to matchers** defined once in the config, not
  a filter. They come late (phase 11), since `apps`/`match` cover the core
  use.
- **Hiding is a state file entry**, and deleting is only Move to Trash.
  Hidden notes come back through the CLI and the dock's launcher.
- **Focus on non-windows doesn't change the context**, so the notes never
  change under the user while using them.
- **Read-only first, then a built-in editor.** The outside editor stays
  available (Ctrl+E). The built-in one shows styled markdown source, not
  a WYSIWYG document, so what is saved is exactly what was typed.
- **The focused note's text is laid out on the main thread**, for
  next-frame typing; everything else goes through the worker.
- **Unsaved edits win over a change on disk only when the user says so.**
- **Order is one list for all notes**, in the state file. Each context
  shows its subset in that order, and a reorder in one context leaves the
  others' notes where they were.
- **Files on notes are links by default**, alone on a line as chips;
  dropping and dragging out are always copies, never moves.
- **The column follows the focused window's output** by default, so the
  notes are next to the app they belong to.
- **Parsing and matching live in `wl-shell-notes`**, so wl-dock's launcher
  reads notes directly instead of asking wl-note.
- **Sharing the menu is a refactor phase of its own** (phase 5), with
  "wl-dock behaves exactly as before" as its exit criterion, and no
  wl-note code in it. It is the only change in this plan that can break
  an existing app; everything else in the shared crates is added next to
  what is there.
- **wl-dock's `drag.rs` isn't shared.** Reordering a column of notes is
  a small pure function of wl-note's own. Code moves to `wl-shell-ui`
  only once two apps turn out to need the same thing.

