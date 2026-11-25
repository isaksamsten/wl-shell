# wl-dock widgets and plugins

A **widget** is a Lua 5.4 plugin that draws part of the dock. It can sit
behind an app's item, where it can replace the app's label (the name shown on
hover) and icon, make the tile longer and add a popup. It can also be an item
of its own (a _standalone_ widget) that you pin, drag and remove like an app.

This document covers [using widgets](#using-widgets), the
[bundled plugins](#bundled-plugins), and
[writing your own](#writing-a-plugin), followed by a
[reference](#reference) for the hooks, the `dock` API, UI trees and D-Bus.

- [Using widgets](#using-widgets)
- [Bundled plugins](#bundled-plugins)
- [Writing a plugin](#writing-a-plugin)
- [How a plugin runs](#how-a-plugin-runs)
- [Reference](#reference)
  - [Hooks](#hooks)
  - [Launcher actions](#launcher-actions-1)
  - [Launcher sources](#launcher-sources-1)
  - [The `dock` API](#the-dock-api)
  - [UI trees](#ui-trees)
  - [Canvas](#canvas)
  - [D-Bus](#d-bus)
  - [The `mpris` module](#the-mpris-module)
- [Examples](#examples)
- [Tips and troubleshooting](#tips-and-troubleshooting)

## Using widgets

### On an app

Give an `[app.<id>]` table a `widget`, either a plugin name or a table with
`plugin` and that placement's options:

```toml
[app.foot]
widget = "clock"
# or: widget = { plugin = "clock", format = "%H:%M:%S", face = true }
popup-delay = 400              # ms of hover before the popup; default: [widgets]
```

Clicking the item still launches or activates the app, and its menu, windows
and indicator stay the same. So on an app, a plugin's popup always opens on
hover, even if the plugin asks for `popup_on = 'click'`. If the plugin fails,
the app gets its own name, icon and size back.

### Standalone

Create a `[widget.<name>]` table and pin `@<name>`:

```toml
[widget.clock]
# plugin = "clock"             # Default: the table's name
name = "Clock"                 # Label and menu title; default: the table's name
popup-delay = 600
# format = "%H:%M"             # Every other key is an option for the plugin
```

```sh
wl-dock pin @clock
```

The widget goes at the end of the pinned apps. You can drag it anywhere
among the apps, or past the separator among the folders, up to just before
the Trash. The pins file (`$XDG_STATE_HOME/wl-dock/pinned`) holds it as an
`@clock` line. A `---` line in that file stands for the separator, and
`@name` lines after it sit among the folders.

Standalone widgets have no windows and no indicator. A left click opens or
closes the popup if the plugin has `popup_on = 'click'`. Otherwise the click
calls the plugin's `on_click('left')`, or opens or closes the popup if the
plugin has no `on_click`. A middle click calls `on_click('middle')`. The
right-click menu has **Remove from Dock**, and dropping the widget on the
Trash does the same. Removing it deletes the `@clock` line but keeps
`[widget.clock]`, so you can pin it again later. Until the plugin draws an
icon, or while it is failing, the item shows a placeholder.

### Plugin tables and global settings

```toml
[plugin.clock]                 # Defaults for every placement of this plugin
# path = "~/src/clock.lua"     # Default: plugins/clock.lua next to the config, else bundled
format = "%H:%M"

[widgets]
enabled = true                 # false: no plugin runs
popup-delay = 300              # ms
budget = 1000000               # Lua instructions per hook call; 10000–1000000000
```

wl-dock looks up a plugin called `name` in this order:

1. `[plugin.<name>] path`, with `~/` expanded and relative paths taken from
   the config's folder
2. `plugins/<name>.lua` next to the config file, usually
   `~/.config/wl-dock/plugins/`
3. the bundled plugin of that name

A file in `plugins/` with the same name as a bundled plugin replaces it,
even if you create the file while the dock is running. Plugin names use
letters, digits, `-` and `_`.

A placement gets the options of its plugin table, with its own options
applied key by key on top. A plugin table also adds the plugin's
[launcher actions](#launcher-actions) and
[launcher sources](#launcher-sources), if it has any; its `setup(opts)`
gets the table's options.

## Bundled plugins

### `clock`

Shows an analog clock face as the icon, the time as the label and the date
in the popup. On an app it only sets the label by default, so the app keeps
its icon.

| Option   | Default                  | Meaning                                   |
| -------- | ------------------------ | ----------------------------------------- |
| `format` | `"%H:%M"`                | `os.date` format of the label             |
| `date`   | `"%A %e %B"`             | `os.date` format of the date in the popup |
| `face`   | standalone: on; app: off | Draw the clock face as the icon           |
| `popup`  | standalone: on; app: off | Show the popup                            |

### `player`

Shows what an MPRIS media player is playing: the cover art as the icon,
`title – artist` as the label, and a popup that can play, pause, skip and
seek.

On an app, it follows that app's own player. A player matches when its
`DesktopEntry`, or its bus name without the prefix and any `.instanceN`
suffix (`org.mpris.MediaPlayer2.spotify` becomes `spotify`), is one of the
app's ID, desktop entry or `StartupWMClass`, ignoring case. As a standalone
widget it follows whichever player is playing, or else the one that changed
last. Clicking the title in its popup raises that player's app.

```toml
[app.spotify]
widget = "player"

[app.firefox]
widget = { plugin = "player", player = "firefox" }

[widget.nowplaying]
plugin = "player"
name = "Now Playing"
span = 2               # Wide on a bottom dock: the title and artist beside the cover
```

| Option   | Default | Meaning                                                                                                                                                                           |
| -------- | ------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `player` | —       | Pick the player by its bus name suffix instead of matching the app; a trailing `*` matches any rest (`"chromium*"`)                                                               |
| `cover`  | `true`  | `false` keeps the item's icon                                                                                                                                                     |
| `label`  | `true`  | `false` keeps the item's name                                                                                                                                                     |
| `span`   | `1`     | `1.5` or `2`: while there is a player, the tile is that many icons wide on a bottom dock, with the title (at `2`, also the artist) beside the cover. Side docks keep square tiles |

### `convert`

A [launcher action](#launcher-actions) rather than a widget: Tab on an image
in the launcher's file search offers **Convert image…**, whose choices are
the formats to convert to (not the image's own). The copy is written next
to the original with ImageMagick, as `photo.webp`, or `photo-2.webp` if that
name is taken, and `notify-send` says when it is done or what went wrong.

```toml
[plugin.convert]
```

| Option    | Default                                 | Meaning                                                                                                                    |
| --------- | --------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| `formats` | `["png", "jpg", "webp", "avif", "gif"]` | The formats offered, in order, as file extensions                                                                          |
| `command` | `["magick"]`                            | The converter, run as `command… <input> <output>`: `["convert"]` for ImageMagick 6, or anything taking those two arguments |
| `notify`  | `true`                                  | Tell with `notify-send` (if installed) when a conversion is done or fails; errors also go to the dock's stderr             |

### `calc`

A [launcher source](#launcher-sources): type `=` in the empty launcher, then
an expression. The result is in front as you type, and Enter copies it.

```toml
[plugin.calc]
```

| You type                                    | You get                                                 |
| ------------------------------------------- | ------------------------------------------------------- |
| `2^10`, `2**10`, `7 // 2`, `7 mod 2`, `5!`  | 1024, 1024, 3, 1, 120                                   |
| `2(3 + 4)`, `2pi`, `-2^2`, `2^3^2`          | 14, 6.28318530718, −4, 512                              |
| `sqrt 2`, `√16`, `sin(30°)`, `sin 30 deg`   | 1.41421356237, 4, 0.5, 0.5                              |
| `log(8, 2)`, `ln e`, `round(pi, 3)`         | 3, 1, 3.142                                             |
| `15% of 80`, `80 + 15%`, `80 - 25%`, `50%`  | 12, 92, 60, 0.5                                         |
| `0xff`, `0b1010`, `0o17`, `1_000_000`       | Hexadecimal, binary, octal, separators                  |
| `0xf0 & 0x3c`, `1 << 10`, `5 xor 3`, `~0`   | 48, 1024, 6, −1: bit operations on integers (and `\|`)  |
| `255 in hex`, `0xff in bin`, `8 to oct`     | 0xFF, 0b1111 1111, 0o10                                 |
| `0.75 in frac`, `1999 in roman`, `1e5 as sci` | 3/4, MCMXCIX, 1 × 10⁵                                  |
| `10 km in mi`, `100°F to C`, `65 mph in km/h` | 6.21371192237 mi, 37.7777777778 °C, 104.60736 km/h    |
| `1 GiB in MB`, `90 min in h`, `3 cups in ml`, `2 m² in ft²` | 1073.741824 MB, 1.5 h, 709.7647095 ml, 21.5278208334 ft² |
| `5 ft`                                      | Its usual conversions, the most readable first          |
| `x = 6 * 7`, then `2x`                      | Enter keeps `x`; then 84                                |
| `ans + 1`                                   | The last result copied, plus one                        |

- **Functions:** `sqrt`, `cbrt`, `abs`, `sign`, `floor`, `ceil`, `trunc`,
  `round(x, digits)`, `exp`, `ln`, `log(x, base)` (base 10 by default),
  `log2`, `log10`, `sin`, `cos`, `tan`, `asin`, `acos`, `atan(y, x)`,
  `atan2`, `sinh`, `cosh`, `tanh`, `asinh`, `acosh`, `atanh`, `hypot`,
  `min`, `max`, `sum`, `avg`, `gcd`, `lcm`, `fact`, `gamma`, `ncr`, `npr`,
  `deg(rad)` and `rad(deg)`. Without parentheses a function takes what
  follows up to the next operator: `sqrt 4 * 2` is 4.
- **Constants:** `pi` (`π`), `tau`, `e`, `phi`, `inf`, and `deg` (`°`),
  one degree in the angle unit, so `sin(30°)` works in either mode.
- **Units:** length, mass, time, temperature, data (`B`, `kB`, `MB`…,
  `KiB`, `MiB`…, and bits: `b`, `Kb`, `Mb`, `Gb`, `bit`), speed, area,
  volume, angle, energy, pressure and power, by symbol or name (`km`,
  `miles`, `°C`, `fahrenheit`, `GiB`, `km/h`, `cups`, `kWh`, `psi`, `hp`).
  Convert with `in`, `to` or `as`.
- **Extras:** an integer from hexadecimal, binary or bit operations, or of
  256 or more, also shows in hexadecimal and binary; a fraction that is
  exact (`0.1 + 0.2` is 3/10); a quantity, its conversions. Each row copies
  its own value, without grouping or units.
- **While typing:** a half-typed expression (`2 * (3 +`) keeps the last
  result shown until it makes sense again; a mistake shows nothing.
- **History:** the empty query lists the results copied before, for as
  long as the plugin runs.

| Option      | Default | Meaning                                                                   |
| ----------- | ------- | ------------------------------------------------------------------------- |
| `precision` | `12`    | Significant digits shown and copied (1–17)                                |
| `angle`     | `"rad"` | `"deg"`: `sin`, `cos` and `tan` take degrees and `asin`… give degrees     |
| `grouping`  | `true`  | Group long numbers with commas (1,234,567); copies are never grouped      |
| `extras`    | `true`  | `false` shows only the result                                             |
| `history`   | `20`    | Results the empty query lists                                             |

### `calendar`

Shows a month calendar with an agenda of the events in Evolution Data Server
(EDS), the calendars of GNOME Calendar and Evolution. As a standalone widget
it draws a calendar page with today's date as its icon. Its label shows the
date and the event that is on now or comes next today. A left click opens
the popup. The popup has the month grid, buttons to page through months,
and an agenda that starts at the selected day. Days with events get a dot
for each event, up to three, in the colour of its calendar. Clicking a day
selects it, and paging to another month selects that month's first day, or
today in the current month. Clicking an event shows its time, place,
calendar, recurrence and description, with **Copy**, **Calendar** (show it
in GNOME Calendar) and **Open** buttons. **Open** runs `xdg-open` on the
event's `URL`, or on the first web link in its location or description,
such as a meeting link.

The plugin talks to EDS over D-Bus and needs it running (GNOME starts it).
By default it shows the enabled calendars that are shown in GNOME Calendar,
with their names and colours, and follows changes as they happen: an event
added in GNOME Calendar, or a calendar hidden there, shows at once. EDS
syncs the remote calendars (CalDAV, Google, Microsoft 365, webcal); opening
the popup asks it to refresh them, at most every five minutes. To add a
calendar, add it in GNOME Calendar or Evolution.

```toml
[widget.calendar]
name = "Calendar"
calendars = ["Work", "Family"]   # optional: only these
```

```sh
wl-dock pin @calendar
```

The plugin is [`plugins/calendar.lua`](plugins/calendar.lua). If your dock
doesn't find it by name, copy it to `~/.config/wl-dock/plugins/` or point
`[plugin.calendar] path` at it.

| Option         | Default                       | Meaning                                                                                                                       |
| -------------- | ----------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `calendars`    | those shown in GNOME Calendar | Names or EDS source UIDs of the calendars to show (enabled ones, shown in GNOME Calendar or not)                              |
| `week_start`   | `"monday"`                    | `"monday"`, `"sunday"` or `"saturday"`                                                                                        |
| `week_numbers` | `false`                       | Show ISO week numbers in the grid                                                                                             |
| `agenda_days`  | `14`                          | Days the agenda covers from the selected day (1–366)                                                                          |
| `format`       | `"%A %e %B"`                  | `os.date` format of the label's date                                                                                          |
| `time`         | `"%H:%M"`                     | `os.date` format of times                                                                                                     |
| `face`         | standalone: on; app: off      | Draw the calendar page as the icon                                                                                            |
| `label`        | `true`                        | `false` keeps the item's name                                                                                                 |
| `popup`        | `true`                        | Show the popup                                                                                                                |
| `open`         | —                             | argv for **Open**, e.g. `["gnome-calendar", "--date", "{date}"]`                                                              |
| `calendar_app` | `["gnome-calendar", "--uuid", "{event}"]` | argv for **Calendar**; `false` leaves the button out                                                              |

In `open` and `calendar_app`, `{title}`, `{url}`, `{uid}`, `{location}`,
`{start}`, `{date}`, `{calendar}` (the EDS source UID) and `{event}` (GNOME
Calendar's id of the occurrence) are replaced.

Recurring events support `FREQ` `DAILY`, `WEEKLY`, `MONTHLY` and `YEARLY`
with `INTERVAL`, `COUNT`, `UNTIL`, `BYDAY`, `BYMONTHDAY`, `BYMONTH`,
`BYSETPOS` and `WKST`, plus `EXDATE` and moved or changed occurrences
(`RECURRENCE-ID`). Cancelled events are left out. Times are shown in local
time. A time with a `TZID` is read in that zone, whether it is an IANA name,
a Windows name as Outlook and Exchange write it, or libical's, and a
repeating event keeps its time of day in its zone across DST changes. A zone
the time zone database doesn't know, such as Outlook's "(UTC+01:00)
Amsterdam, Berlin, ...", is asked of EDS, which keeps the definitions that
came with the invites. The event's details show its time in its zone when
that isn't local time. A `TZID` that nothing is known of is read as local
time. The agenda
lists at most 100 events and counts the rest.

### `recent`

Lists the files an app opened last, like the dock menus on macOS. Put it on
an app, and hovering the app's item shows a popup with its recent files,
newest first. Each file has its thumbnail (or an icon for its type), its
folder, and when it was opened. Clicking a file opens it in that app again.

```toml
[app."org.gnome.Papers"]
widget = "recent"

[app.libreoffice-writer]
widget = { plugin = "recent", apps = ["soffice"] }

[widget.recent]                # Standalone: every app's recent files
name = "Recent Documents"
```

The list comes from `~/.local/share/recently-used.xbel`. GTK apps, and
others that use the same file, record there each file they open and which
app opened it. The plugin watches the file, so the list updates as apps
open files. Files that no longer exist are left out. Thumbnails come from
the freedesktop thumbnail cache (`~/.cache/thumbnails`), as the file
manager made them. An image without a thumbnail shows itself.

An app records itself by a name and a command, such as `Document Viewer`
and `papers`. A file is the app's if either one matches one of the dock's
names for the app: its ID, desktop entry, `StartupWMClass` or name. Case,
spaces and punctuation don't count, and a reverse-DNS ID also matches by
its last part, so `org.gnome.Papers` matches `papers`. A file manager
records the files it opens in other apps under its own name, so those
files show on both items. If an app's files don't show, check the
`bookmark:application` names in the file and add them to `apps`.

As a standalone widget, a left click opens the popup, which lists the
recent files of every app. With `apps` set, it lists only those apps'
files. Each file opens in the app that used it last.

| Option       | Default                               | Meaning                                                                                                                       |
| ------------ | ------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `count`      | `10`                                  | Files shown (1–12)                                                                                                            |
| `apps`       | —                                     | More names the app records files under, e.g. `["soffice"]`. Standalone: list only these apps' files                           |
| `thumbnails` | `true`                                | `false` shows type icons only                                                                                                 |
| `open`       | `"app"`                               | `"app"`: the command the app recorded, else `xdg-open`; `"default"`: always `xdg-open`; or an argv with `{path}` and `{uri}` |
| `file`       | `$XDG_DATA_HOME/recently-used.xbel`   | The bookmark file to read                                                                                                     |

### `downloads`

Shows a browser's downloads in progress as a ring around its icon (or a
bar along its bottom), and lists finished downloads in a popup, where a
click opens one. It watches the download folder with `dock.watch`. Firefox
writes a download to `name.part` and Chromium-based browsers to
`name.crdownload`, and the file gets its real name when it is done. A
partial file doesn't say how big the download will be, so the ring turns a
step each time data arrives instead of filling. If the browser also
publishes its progress over Unity LauncherEntry, as Chromium does, the
ring fills to that. With several downloads at once, a badge counts them.
The label shows the file name, its size so far and the speed, or
_Waiting_ when nothing has arrived for a few seconds. When the last
download finishes, the ring stays full for a moment.

```toml
[app.firefox]
widget = "downloads"

[app.chromium]
widget = { plugin = "downloads", style = "bar" }

[widget.downloads]             # Standalone: every browser's downloads
name = "Downloads"
```

On an app, the plugin only follows that browser's kind of file, guessed
from the app's names (Firefox, LibreWolf, Zen… or Chromium, Chrome, Brave,
Vivaldi, Edge…). While nothing is downloading and nothing has finished
since the plugin started, the browser keeps its own icon, label and no
popup. A finished download leaves the list when its file is moved or
deleted. A cancelled download doesn't show up in the list.

| Option     | Default                                 | Meaning                                                                                 |
| ---------- | --------------------------------------- | --------------------------------------------------------------------------------------- |
| `folders`  | The XDG download folder, else `~/Downloads` | Folders to watch: a string or a list                                                |
| `browser`  | Guessed from the app; standalone `"all"` | `"firefox"` (`.part`), `"chromium"` (`.crdownload`) or `"all"`                         |
| `style`    | `"ring"`                                | `"ring"` around the icon or `"bar"` along its bottom                                    |
| `keep`     | `5`                                     | Finished downloads listed (0–20)                                                        |
| `open`     | `["xdg-open"]`                          | argv that opens a finished download, with its path added at the end                     |
| `interval` | `1000`                                  | The shortest time in ms between looks at the folders while files change (at least 250) |

The sources in [`plugins/`](plugins) are good starting points for your own
plugins.

## Writing a plugin

### A first plugin

Create `~/.config/wl-dock/plugins/hello.lua`:

```lua
local P = {}
local started = os.time()

function P.setup(opts)
  -- Update once a minute.
  dock.every(60 * 1000, dock.redraw)
end

function P.label()
  local minutes = (os.time() - started) // 60
  return 'Up for ' .. minutes .. ' min'
end

return P
```

Then put it behind an app:

```toml
[app.foot]
widget = "hello"
```

Hover over the item to see the label. The dock watches plugin files, so
saving `hello.lua` reloads the plugin right away. Errors and `print` output
go to the dock's stderr, prefixed with `wl-dock: plugin hello:`. Run
`wl-dock` from a terminal while you develop.

### Adding an icon and a popup

```lua
local ui, theme = dock.ui, dock.theme
local P = {}
local count = 0

function P.icon(tile)
  -- The app's icon with a badge in its corner.
  return ui.stack{ align = 'top-right',
    ui.app_icon{},
    count > 0 and ui.box{ variant = 'badge',
      ui.text{ tostring(count), style = 'caption' } } or nil,
  }
end

function P.popup()
  return ui.col{ padding = 16, gap = 8, width = 220,
    ui.text{ 'Counter', style = 'title' },
    ui.text{ count .. ' clicks', style = 'muted' },
    ui.row{ gap = 8,
      ui.button{ key = 'add', label = 'Add', variant = 'filled',
        on_click = function() count = count + 1; dock.redraw() end },
      ui.button{ key = 'reset', label = 'Reset', enabled = count > 0,
        on_click = function() count = 0; dock.redraw() end },
    },
  }
end

return P
```

The idea is the same for every widget: the plugin keeps its own state, and
`icon`, `label` and `popup` describe what that state looks like now. When the
state changes, call `dock.redraw()` and the dock asks the hooks again. The
dock only repaints what changed.

### Options

`setup(opts)` receives the placement's options as a Lua table, converted
from TOML: strings, numbers, booleans, arrays as lists and tables as tables.
Check the types yourself, because the user can write anything:

```lua
local interval = 5000

function P.setup(opts)
  if type(opts.interval) == 'number' and opts.interval >= 1000 then
    interval = opts.interval
  end
  dock.every(interval, refresh)
end
```

Document the options in a comment at the top of the plugin, as the bundled
plugins do.

### Standalone or on an app

`dock.app` is `nil` for a standalone widget. On an app, it is a table with
the names the dock knows the app by:

| Field      | Meaning                                            |
| ---------- | -------------------------------------------------- |
| `id`       | The dock's app ID: the `[app.<id>]` key            |
| `desktop`  | The desktop entry it matched, without `.desktop`   |
| `wm_class` | That entry's `StartupWMClass`                      |
| `name`     | The configured name, else the entry's, else the ID |

The desktop entry is looked up after the plugin starts. When the lookup
finishes, the dock updates the same `dock.app` table in place, calls the
plugin's `on_app()` hook and asks the hooks again. If the plugin matches
something by app name, as `player` does, it should check again in `on_app`.

A plugin that works in both places usually picks different defaults. The
clock, for example, only draws its face when it is standalone:

```lua
local face = dock.app == nil
```

When a hook returns `nil`, the item keeps its own label, icon or popup. On
an app that means the app's own; on a standalone widget it means the
`name` from the config, a placeholder icon, and no popup.

### Launcher actions

A plugin can add rows to what Tab shows on a launcher match: a file, an app
or a window. Give it an `actions` list and a `[plugin.<name>]` table, since
the launcher only takes actions from plugin tables (not from each widget
placement):

```lua
return {
  actions = {
    { id = 'shrink', name = 'Shrink image', icon = 'image-x-generic',
      when = { kind = 'file', mime = 'image/*' },
      choices = function(s) return { { id = '50', name = '50%' }, { id = '25', name = '25%' } } end,
      run = function(s, choice)
        dock.run{ 'magick', s.path, '-resize', choice.id .. '%', s.path .. '.small.png' }
      end },
    { id = 'logs', name = 'Follow logs', icon = 'utilities-terminal',
      when = { kind = 'app', running = true },
      run = function(s) dock.run{ 'foot', 'journalctl', '--user', '-f', '-t', s.id } end },
  },
}
```

```toml
[plugin.tools]                 # plugins/tools.lua
```

The plugin's rows come after the launcher's own, in the order the plugin
lists them (plugins by name). An action without `choices` runs when you
press Enter on it, and the launcher closes. One with `choices` is a level of
its own, like Open with…: Tab or Enter enters it, it shows _Loading…_ until
`choices(s)` answers, and Enter on a choice runs `run(s, choice)`. If the
plugin doesn't answer within 0.5 s, or fails, the level shows _No choices_
and you can go back as usual. See the [reference](#launcher-actions-1) for
the subject tables and `when`.

### Launcher sources

A plugin can also add a search of its own to the launcher, entered by typing
its trigger first, as `'` enters the file search and `@` the window search.
Give it a `sources` list and, as for actions, a `[plugin.<name>]` table:

```lua
local function hosts()
  local out = {}
  for host in ('\n' .. (dock.read('~/.ssh/config') or '')):gmatch('\nHost%s+([^%s*]+)') do
    out[#out + 1] = { id = host, title = host, icon = 'network-server' }
  end
  return out
end

return {
  sources = {
    { id = 'ssh', name = 'SSH', trigger = '>', icon = 'network-server',
      items = hosts,                                 -- a fixed list; the launcher ranks it
      run = function(item) dock.run{ 'foot', 'ssh', item.id } end },
    { id = 'calc', name = 'Calculator', trigger = '=', icon = 'accessories-calculator',
      search = function(query, emit)                 -- live results for each query
        local f = load('return ' .. query, 'calc', 't', { math = math })
        local ok, v = pcall(f or error)
        if ok and v then emit{ { id = 'result', title = tostring(v), subtitle = query } } end
      end,
      run = function(item) dock.clipboard(item.title) end },
  },
}
```

Typing `>` in the empty launcher shows the source's name in the bar and its
icon in front; what you type after it searches the source. Backspace on the
empty query, or Shift+Tab, goes back to the apps.

- **`items()`** returns the whole list. The launcher asks for it once, when
  the source is first entered, keeps it, and ranks it itself for every
  query (by title, then subtitle), so typing never waits for the plugin.
  Call `dock.invalidate()` when the list has changed (for example from a
  `dock.watch` callback); it is asked for again. Until it arrives the bar
  says _Loading…_.
- **`search(query, emit)`** is called for each query, once typing pauses
  for 80 ms (at once for an empty query). Call `emit(list)` with the
  results, while `search` runs or later (from a timer, or a process's
  `on_line`); each `emit` replaces the results shown. Results for an older
  query are dropped, however late they come, and the results shown stay
  until the new query's arrive. `search` may also return the list instead
  of emitting it.
- **Enter** calls `run(item)` with the item's table, as the plugin made it.
  Without `run`, Enter opens a file item or an app item (below), and does
  nothing on other items.
- **Tab** on a file item (`kind = 'file'`, with a `path`) offers the file's
  usual actions, Open with… and the rest; on an app item (`kind = 'app'`,
  with the app's desktop ID as `app`) the app's. Other items get the
  [launcher actions](#launcher-actions) whose `when` names their source:
  `when = { kind = 'item', source = 'ssh' }`.

A trigger already taken (by `'`, `@`, the web search's `!`, or an earlier
source; plugins by name) is ignored. See the
[reference](#launcher-sources-1) for the item tables.
The bundled [`calc`](#calc) is a source too, and its file a fuller example.

## How a plugin runs

- **One state per placement.** Each `[app.*]` widget and each pinned
  `[widget.*]` gets its own Lua state. Two placements of the same plugin
  share nothing, not even globals. A `[plugin.*]` table whose plugin has
  launcher `actions` or `sources` gets one more state for them.
- **Threads.** States run on a small thread pool. A state never runs on two
  threads at once, so a plugin needs no locking. Nothing in the API blocks:
  processes, timers and D-Bus calls all report back through callbacks on the
  plugin's thread.
- **Loading.** The file runs once and must return a table. If `setup(opts)`
  exists, it is called next. Hooks can be at the top level of the table or
  under a `widget` subtable.
- **Budget.** Each call into the plugin (a hook, a timer, a callback) may run
  `[widgets] budget` Lua instructions, 1,000,000 by default. A state may use
  64 MiB of memory. Going over either limit stops the state, and `pcall`
  can't catch it.
- **Errors.**
  - A hook that raises an error is logged (a repeated error only once), its
    answer is dropped, and the plugin keeps running.
  - A syntax error, or a file that doesn't return a table, leaves the
    plugin stopped until you save the file again.
  - A failing `setup`, an exceeded budget or running out of memory restarts
    the plugin after a backoff: 1 s, doubling to at most 60 s, and back to
    1 s once it has run for a minute.
  - While a plugin is down, its app shows its own name and icon, and a
    standalone widget shows a placeholder.
- **Deadlines.** `label()` must answer within 0.5 s and `icon()` or `popup()`
  within 1 s, or the answer is thrown away. A launcher action's `choices(s)`
  has 0.5 s, a source's `items()` 2 s, and `search` 2 s to run (what it
  emits later counts as long as its query is the latest). Do slow work,
  such as running commands or reading the network, ahead of time in timers
  or callbacks, and have the hooks only return what is already known.
- **Reloading.** Saving the plugin file starts a fresh state: timers stop,
  processes are killed, and D-Bus and file watches are cancelled. Modules
  loaded with `require` are not watched, so save the plugin file to pick up
  changes to them. Changes to the config apply too.
- **When hooks run.** The dock calls `label()` and `icon()` when the plugin
  loads and after each `dock.redraw()`. It also calls `icon()` when the tile
  changes size. `popup()` runs when the popup opens and after each
  `dock.redraw()` while it is open.

## Reference

### Hooks

All hooks are optional. A plugin needs at least one of `icon`, `label`,
`popup` or `on_click` to be a widget.

| Hook            | Called                                                          | Returns / gets                                                          |
| --------------- | --------------------------------------------------------------- | ----------------------------------------------------------------------- |
| `setup(opts)`   | Once, after the file loads                                      | `opts`: the placement's options                                         |
| `label()`       | On load and after `dock.redraw()`                               | A string (trimmed, at most 200 characters), or `nil` for the item's own |
| `icon(tile)`    | On load, after `dock.redraw()`, on tile changes                 | A [UI tree](#ui-trees), or `nil` for the item's own                     |
| `popup()`       | When the popup opens and after `dock.redraw()` while it is open | A UI tree, or `nil` for no popup                                        |
| `on_open()`     | When the popup opens                                            |                                                                         |
| `on_close()`    | When the popup closes                                           |                                                                         |
| `on_click(btn)` | A standalone widget's item is clicked (left: see `popup_on`)    | `btn`: `'left'` or `'middle'`                                           |
| `on_app()`      | `dock.app` gained names (see above)                             |                                                                         |

The plugin table also takes these keys:

| Key        | Meaning                                                                                                                                                                                     |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `span`     | The tile's length along the dock in icons: `1`, `1.5` or `2` (other values round down), or per dock direction `{ horizontal = 2, vertical = 1 }`. Change it at run time with `dock.span()`. |
| `popup_on` | What opens the popup: `'hover'` (default) or `'click'`. See [`popup()`](#popup). On an app's item the popup always opens on hover.                                                          |
| `api`      | The API version the plugin was written for. If given, it must be `1`.                                                                                                                       |

`actions` adds [launcher actions](#launcher-actions), and `sources`
[launcher sources](#launcher-sources).

#### `icon(tile)`

`tile` describes the space the icon gets:

| Field   | Meaning                                                           |
| ------- | ----------------------------------------------------------------- |
| `w`,`h` | The magnified size, in logical pixels (`w` is longer with a span) |
| `span`  | The tile's span                                                   |
| `axis`  | `'horizontal'` (bottom dock) or `'vertical'` (side dock)          |
| `scale` | The output's scale factor                                         |

The dock lays the tree out again for each dock's size when it paints, so one
tree works on every output. Use `tile` for proportions, such as a corner
radius of `math.min(tile.w, tile.h) * 0.2`, not for absolute sizes. Icon
trees are only drawn: clicks on an item go to the item, so handlers in an
icon tree are never called.

#### `popup()`

By default the popup opens after the item has been hovered for its
`popup-delay`, and the item's label doesn't show: hovering shows the popup
or the label, never both. If `popup()` returns `nil`, the label shows
instead, until the pointer moves to another item and back.

With `popup_on = 'click'`, a standalone widget shows its label on hover, and
a left click opens or closes the popup instead of calling `on_click`. The
popup stays open while the pointer moves over items without a hover popup,
and closes when the pointer leaves the dock and the popup, on another click,
or when it hands over to another widget's hover popup. On an app's item,
clicks belong to the app, so its popup opens on hover anyway.

No label shows while a popup is open. The popup is shown in a speech bubble
pointing at the item, like the menu. When the pointer moves from one open
popup to another widget, the next popup opens at once. The dock handles
hover and pressed looks, slider drags and scrolling itself. It calls the
plugin's handlers as described in [Interaction](#interaction).

### Launcher actions

`actions` is a list of tables, each with these keys:

| Key       | Meaning                                                                                       |
| --------- | --------------------------------------------------------------------------------------------- |
| `id`      | Required, unique in the plugin                                                                |
| `name`    | The row's name; default: the `id`                                                             |
| `icon`    | A theme icon name; default: a generic one                                                     |
| `when`    | Which subjects it applies to (below); default: all                                            |
| `choices` | Optional `function(s)`: a list of choices, each `{ id =, name =, icon = }` plus any fields    |
| `run`     | Required `function(s, choice)`: does the action; `choice` is `nil` for an action without them |

A choice needs an `id` or a `name`; either stands in for the other, and a
choice without an `icon` gets the action's. `run` gets the choice table back
as `choices` made it, with its other fields (but not functions), so a choice
can carry data: `{ id = 'half', name = '50%', factor = 0.5 }`. At most 500
choices are shown, and a repeated `id` only once.

The subject `s` is a plain table:

| `kind`     | Fields                                                                                                                          |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `'file'`   | `path`, `name`, `dir` (a folder), `mime` (the type and its parents, nearest first: `{'image/png', 'application/octet-stream'}`) |
| `'app'`    | `id` (desktop ID), `name`, `running` (has windows), `pinned`, `windows` (a list of `{ id =, title = }`, most recent first)      |
| `'window'` | `id`, `title`, `app_id`, `app_name`                                                                                             |
| `'item'`   | `source` (its source's `id`), `id`, `title`, `data` (the item's table, as the plugin made it)                                   |

`when` is checked by the dock, so building the Tab level never waits for a
plugin. Every key given must hold:

| Key       | Holds for                                                             |
| --------- | --------------------------------------------------------------------- |
| `kind`    | Subjects of this kind, or of one in a list: `{ 'app', 'window' }`     |
| `mime`    | Files whose type or one of its parents matches the glob (`'image/*'`) |
| `path`    | Files whose path matches the glob (`'~/notes/*.md'`; `~/` expands)    |
| `dir`     | Folders (`true`) or files (`false`)                                   |
| `app`     | Apps whose desktop ID, or windows whose app ID, match the glob        |
| `running` | Apps with windows (`true`) or without (`false`); windows always run   |
| `source`  | Items of the source with this `id` (a plugin source's)                |

In globs `*` matches any run of characters, `/` included, and `?` one
character; `mime` and `app` ignore case. A key that doesn't apply to the
subject, such as `mime` on an app, doesn't hold. An unknown key is an error
when the plugin loads.

### Launcher sources

`sources` is a list of tables, each with these keys:

| Key       | Meaning                                                                                       |
| --------- | --------------------------------------------------------------------------------------------- |
| `id`      | Required, unique in the plugin; what `when.source` names                                      |
| `name`    | Shown in the bar while nothing is typed; default: the `id`                                    |
| `icon`    | A theme icon name: the front's while nothing matches, and the items' without one of their own |
| `trigger` | Required: the one character that enters the source when typed first (not a space)             |
| `items`   | `function()`: the list of items. Give this or `search`                                        |
| `search`  | `function(query, emit)`: calls `emit(list)` with the query's results. Give this or `items`    |
| `run`     | Optional `function(item)`: what Enter does                                                    |

An item is a table with these keys, plus any fields of the plugin's own
(`run` and the actions' `s.data` get them back, without functions):

| Key        | Meaning                                                                       |
| ---------- | ----------------------------------------------------------------------------- |
| `id`       | Unique in the list; an `id` or a `title` is required, each stands in for the other |
| `title`    | Its name in the launcher                                                      |
| `subtitle` | Shown dimmed after the title, and searched too                                |
| `icon`     | A theme icon name or a path; default: the source's                            |
| `kind`     | `'item'` (default), `'file'` or `'app'`                                       |
| `path`     | A file item's path (`~/` expands); its icon is the file's thumbnail or type   |
| `app`      | An app item's desktop ID (`'org.gnome.Nautilus'`); its icon is the app's      |

`items()` keeps at most 10,000 items, and each `emit` 500; a repeated `id`
counts once. A bad item (no `id` or `title`, a file without `path`) is an
error in `items()`, or in the `emit` call.

### The `dock` API

| Function                           | Does                                                                                                                                                                |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `dock.redraw()`                    | Ask the hooks again (once the current call returns)                                                                                                                 |
| `dock.invalidate()`                | Say a [launcher source](#launcher-sources)'s `items()` changed: they are asked for again (a search source searches again)                                         |
| `dock.timer(ms, fn)`               | Call `fn()` once after `ms`. Returns a handle with `:cancel()`                                                                                                      |
| `dock.every(ms, fn)`               | Call `fn()` every `ms` (at least 10). Returns a handle with `:cancel()`                                                                                             |
| `dock.spawn(argv, opts)`           | Run a process. `opts.on_line(line)` gets each line of its stdout, `opts.on_exit(code)` its exit code (`nil` if killed by a signal). Returns a handle with `:kill()` |
| `dock.run(argv)`                   | Start a detached process. Returns `true`, or `nil, err`                                                                                                             |
| `dock.read(path)`                  | A file's text (`~/` expands, at most 16 MiB). Returns `text`, or `nil, err`                                                                                         |
| `dock.watch(path, fn, opts)`       | Call `fn(paths)` when a file, or anything in a folder, changes. Returns a handle with `:cancel()`, or `nil, err`. See [Watching files](#watching-files)             |
| `dock.json.decode(s)`              | JSON to Lua (`null` becomes `nil`). Raises an error on bad JSON                                                                                                     |
| `dock.json.encode(v)`              | Lua to JSON                                                                                                                                                         |
| `dock.clipboard(text)`             | Copy `text` to the clipboard                                                                                                                                        |
| `dock.span(n)`                     | Set the tile's span: `1`, `1.5`, `2` or `{ horizontal =, vertical = }`                                                                                              |
| `dock.now()`                       | Unix time in seconds, with a fraction. This is the clock `live.at` uses                                                                                             |
| `dock.app`                         | The app behind the widget, or `nil` (see [above](#standalone-or-on-an-app))                                                                                         |
| `dock.ui.*`                        | [UI constructors](#ui-trees)                                                                                                                                        |
| `dock.theme.*`                     | [Theme color names](#colors)                                                                                                                                        |
| `dock.dbus.session()`, `.system()` | [D-Bus clients](#d-bus)                                                                                                                                             |
| `dock.tz.*`                        | `os.time` and `os.date` in any time zone. See [Time zones](#time-zones)                                                                                            |

`argv` is either a list of arguments (`{'notmuch', 'count', 'tag:unread'}`),
which is safe for arbitrary values, or a single string run with `sh -c`.
`dock.spawn` connects stdin to `/dev/null` and sends stderr to the dock's
stderr. Processes started with `spawn` are killed when the plugin stops;
`dock.run` processes are not.

#### The sandbox

Plugins have Lua's `string`, `table`, `math`, `utf8` and `coroutine`
libraries, plus `os.time`, `os.date`, `os.clock`, `os.difftime` and
`os.getenv`. They don't have `io`, `debug`, `package`, `os.execute`,
`dofile`, `loadfile` or `string.dump`, and `load` only accepts source text.
Read files with `dock.read` and run commands with `dock.spawn` or `dock.run`.

`print(...)` writes to the dock's log.

#### Watching files

```lua
local w, err = dock.watch('~/.calendars', function(paths)
  -- paths: a list of the files and folders that changed, each once
end, { recursive = true })
w:cancel()
```

`dock.watch` calls `fn` when the file at `path` changes, or when anything in
the folder at `path` does. `~/` expands. With `opts.recursive`, a folder's
subfolders are watched too, including ones made later, except those starting
with `.` and symlinked ones.

A file is watched through its folder, so it is still seen when it is
replaced by a rename (how most editors and Evolution save), removed, or
created later. Its folder must exist; if it doesn't, `dock.watch` returns
`nil` and the error, so fall back to a timer. Changes are gathered for 200
ms from the first one and delivered as one call. If the system drops events,
`fn` gets the watched path itself.

A plugin can have at most 256 watches at a time. They end when the plugin
reloads or stops, or when you call `:cancel()`.

`require('name')` loads `name.lua` from the plugin's folder, where dots in
the name are subfolders (`require('lib.util')` loads `lib/util.lua`). If
there is no such file, it loads the bundled module of that name, currently
only [`mpris`](#the-mpris-module).

#### Time zones

`os.time` and `os.date` only know local time and UTC. `dock.tz` reads the
system's time zone database:

```lua
dock.tz.find('W. Europe Standard Time')  --> 'Europe/Berlin' (nil if unknown)
dock.tz.system()                         --> 'Europe/Stockholm'
local t = dock.tz.time('America/New_York', { year = 2026, month = 7, day = 1, hour = 9 })
local d = dock.tz.date('Asia/Tokyo', t)  --> { year, month, day, hour, min, sec,
                                         --     wday, yday, isdst, offset, abbrev }
local z = dock.tz.vtimezone(text)        --> '<+01>-1<+02>,M3.5.0/2,M10.5.0/3'
```

A zone is an IANA name (`Europe/Stockholm`, `UTC`), a Windows name as
Outlook and Exchange write them (`W. Europe Standard Time`), or either after
a prefix as libical writes them (`/freeassociation.sourceforge.net/Europe/Oslo`),
or a POSIX TZ string (`CET-1CEST,M3.5.0,M10.5.0/3`). `find` returns the IANA
name, or a POSIX string itself. In `time` and `date`, `nil` or `'local'` is
the system's zone.

`time` turns a wall-clock time into Unix seconds. `hour`, `min` and `sec`
default to 0, and nothing is normalized, so the 32nd of a month is an error.
A time skipped by a change to summer time is moved forward by the change,
and a time that happens twice is the first one, as RFC 5545 and `os.time` do
it. `date` is `os.date('*t')` in a zone (`wday` 1 is Sunday), plus `offset`
(seconds east of UTC) and `abbrev` (`CEST`). Both return `nil, err` for an
unknown zone or an invalid time. To format a time in a zone, shift it by the
offset and format it as UTC: `os.date('!%H:%M', t + d.offset)`.

`vtimezone` turns an iCalendar `VTIMEZONE` into a POSIX TZ string, for zones
that only an invite defines, such as Outlook's "(UTC+01:00) Amsterdam,
Berlin, ...". It takes the rules in effect now (the latest `STANDARD` and
`DAYLIGHT` parts, with yearly rules by weekday), so times from before the
rules last changed may be off. It returns `nil, err` for rules POSIX can't
express.

### UI trees

Build trees with the `dock.ui` constructors. Each one takes a single table:
named keys are props, and positional values are children. `nil` and `false`
children are skipped, so `cond and ui.x{} or nil` works:

```lua
local ui = dock.ui
ui.col{ gap = 8, padding = 12,
  ui.text{ 'Title', style = 'title' },
  show_more and ui.text{ 'More' } or nil,
}
```

The dock checks the tree when the hook returns. An error names the node and
prop at fault, as in `col[1].text.color: "nope" is not a color or a theme
color`. A tree may have at most 5000 nodes and be at most 64 levels deep.

#### Constructors

| Constructor            | Props                                                                                                                                                                                                                                             |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ui.col{}`             | Children top to bottom. `gap`, `padding`, `align`, `justify`                                                                                                                                                                                      |
| `ui.row{}`             | Children left to right. Same props as `col`                                                                                                                                                                                                       |
| `ui.grid{}`            | `columns`: a count (1–64) or a list of widths: pixels, `'1fr'`, `'auto'`. `gap`, `row_gap`                                                                                                                                                        |
| `ui.stack{}`           | Layers its children; the first is at the bottom. `align`: `center` (default), `stretch`, `top`, `bottom`, `left`, `right`, `top-left`, `top-right`, `bottom-left`, `bottom-right`                                                                 |
| `ui.box{}`             | A container. `variant`: `card`, `inset`, `badge`                                                                                                                                                                                                  |
| `ui.scroll{}`          | Scrolls its content vertically when it is taller than the box. Limit it with `height` or `max_height`                                                                                                                                             |
| `ui.spacer{}`          | Fixed space with `size`, or without `size` it takes up the free space in a row or column                                                                                                                                                          |
| `ui.divider{}`         | A thin line                                                                                                                                                                                                                                       |
| `ui.text{'…'}`         | `style`: `body` (default), `title`, `muted`, `caption`. `weight`: `normal`, `light`, `medium`, `bold` or 100–900. `color`, `size`, `align` (`start`/`left`, `center`, `end`/`right`), `max_lines`. Numbers become text. At most 10,000 characters |
| `ui.icon{'name'}`      | An icon from the icon theme (also `name =`). `size`, `color`. `-symbolic` icons take the text color by default                                                                                                                                    |
| `ui.app_icon{}`        | An app's icon. `app`: an app ID (default: the widget's app). `size`                                                                                                                                                                               |
| `ui.image{src = …}`    | A file path (`~/` expands), a `file://` URL or an `http(s)://` URL. `fit`: `cover`, `contain`, `fill`. `radius`, `blur` (0–100), `opacity`. Supports PNG, JPEG, WebP and SVG                                                                      |
| `ui.canvas{draw = fn}` | Vector drawing, see [Canvas](#canvas). `viewbox = {w, h}` (default `{100, 100}`)                                                                                                                                                                  |
| `ui.button{}`          | `label`, `icon`, `variant` (`plain` (default), `filled`, `ghost`), `enabled`, `selected`, `tooltip`, `on_click`. Children go after the icon and label                                                                                             |
| `ui.slider{}`          | `value`, `min` (0), `max` (1), `enabled`, `live`, `on_change`, `on_commit`                                                                                                                                                                        |
| `ui.toggle{}`          | `value` (boolean), `enabled`, `on_change`                                                                                                                                                                                                         |
| `ui.progress{}`        | `value`, `max` (1), `live`                                                                                                                                                                                                                        |

Values for the layout props:

- `padding`: `8`, `{v, h}`, `{t, r, b, l}` or `{ top =, right =, bottom =, left = }`
- `align` (col, row): how children sit across the axis: `start`, `center`,
  `end`, `stretch`
- `justify`: how children spread along the axis: `start`, `center`, `end`,
  `between`, `around`, `evenly`

#### Props every node takes

| Prop                                                 | Meaning                                                                                       |
| ---------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| `width`, `height`                                    | Pixels, or a share of the parent as `'50%'`                                                   |
| `min_width`, `min_height`, `max_width`, `max_height` | Same units                                                                                    |
| `grow`                                               | Share of the free space in a row or column (`true` is `1`)                                    |
| `background`                                         | A [color](#colors)                                                                            |
| `radius`                                             | Corner radius                                                                                 |
| `border`                                             | A width (`1`), a color (`'accent'`), or `{ width =, color = }`. The default color is `border` |
| `opacity`                                            | 0–1                                                                                           |
| `hover`                                              | `background`, `radius`, `border` and `opacity` to use while hovered                           |
| `key`                                                | A stable name for the node (see below)                                                        |

Sizes are logical pixels. Images and icons are rendered at the output's
scale.

#### Colors

A color is either one of the theme's names or a CSS color: `'#rrggbb'`,
`'#rrggbbaa'`, `'rgb(…)'`, `'red'`, `'transparent'`. The theme's names are
also available as `dock.theme.<name>`, which catches typos as Lua errors:

`text`, `muted`, `accent`, `accent_text`, `surface`, `border`, `hover`,
`pressed`, `track`, `divider`, `card`, `inset`, `badge`, `badge_text`.

Theme colors follow the dock's appearance in both light and dark mode, and
they are resolved when the tree is painted. Use them unless you need a
specific color, and a theme change won't need a new tree.

#### Interaction

Interactive nodes only respond in popups.

| Node     | Handler            | Called                                    |
| -------- | ------------------ | ----------------------------------------- |
| `button` | `on_click()`       | On click, if `enabled`                    |
| `slider` | `on_change(value)` | While dragging, at most 30 times a second |
| `slider` | `on_commit(value)` | On release                                |
| `toggle` | `on_change(on)`    | When flipped                              |

Any node can have an `on_click`, which makes it clickable.

Handlers run on the plugin's thread like other callbacks. Update your state
in them and call `dock.redraw()`. The dock shows hover, pressed and drag
feedback without waiting for the plugin.

**Keys.** The dock tells nodes apart across rebuilds by key, which is how it
finds a node's handlers and keeps its state, such as a slider being dragged
or a scroll position. A node without a `key` gets one from its position in
the tree. Give interactive and scrolling nodes an explicit `key` whenever
the tree's shape can change, for example when nodes before them appear and
disappear. Keys must be unique within a tree.

**Live sliders and progress bars.** A seek bar for a playing track would
otherwise need a redraw every frame. Instead, give it
`live = { rate = 1, at = t }`: it moves on its own from `value` at time `at`
(a `dock.now()` time) at `rate` units per second. The dock paints it on its
own layer, only as often as it moves by a pixel. Set `live = nil` (or
`false`) when it should stop. The `player` plugin uses this:

```lua
ui.slider{ key = 'seek', value = p.position, max = p.length,
  live = p.playing and { rate = p.rate, at = p.at } or nil,
  on_commit = function(v) p:set_position(v) end }
```

### Canvas

`ui.canvas{ viewbox = {w, h}, draw = fn }` calls `draw(c)` while the tree is
built and records the drawing commands. The drawing is then scaled from the
viewbox to the node's size. As an icon, a canvas fills the tile; in a popup,
give it a `width` and `height`. Coordinates are in viewbox units with y
pointing down. Angles are in radians, clockwise. A canvas can hold at most
20,000 commands.

| Method                                                | Draws                                                                                                                                                      |
| ----------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `c:line{x1, y1, x2, y2}`                              | A line                                                                                                                                                     |
| `c:rect{x, y, w, h, radius}`                          | A rectangle, optionally rounded                                                                                                                            |
| `c:circle{x, y, r}`                                   | A circle                                                                                                                                                   |
| `c:arc{x, y, r, from, to, pie}`                       | An arc from `from` to `to` (radians from 3 o'clock); with `pie = true`, closed through the center                                                          |
| `c:path{d = 'M 0 0 L 10 10 Z'}`                       | SVG path data (relative commands and arcs too)                                                                                                             |
| `c:text{'12', x, y, size, color, bold, align}`        | One line of text. Its `align` edge and vertical middle are at (`x`, `y`). `size` defaults to 12. It moves and scales with the transform but doesn't rotate |
| `c:save()`, `c:restore()`                             | Push and pop the transform                                                                                                                                 |
| `c:translate(x, y)`, `c:rotate(a)`, `c:scale(x[, y])` | Transform what comes next                                                                                                                                  |

Shapes take `fill` and `stroke` colors, a stroke `width` (default 1, in
viewbox units so it scales with the canvas) and a `cap` (`butt`, `round`,
`square`). Note that these are named fields, so call `c:line{ x1 = 0, … }`
rather than passing positional numbers.

`rotate` turns around the current origin, so translate to the center first.
A line drawn along −y (12 o'clock) and then rotated by `a` turns clockwise,
like a clock hand:

```lua
c:save()
c:translate(50, 50)
c:rotate(minutes * math.pi / 30)
c:line{ x1 = 0, y1 = 0, x2 = 0, y2 = -40, stroke = dock.theme.text, width = 3, cap = 'round' }
c:restore()
```

### D-Bus

`dock.dbus.session()` and `dock.dbus.system()` return clients for the
session and system bus. Every call is asynchronous, and replies and signals
arrive as callbacks on the plugin's thread.

```lua
local bus = dock.dbus.session()

bus:call{ dest = 'org.mpris.MediaPlayer2.spotify', path = '/org/mpris/MediaPlayer2',
  iface = 'org.mpris.MediaPlayer2.Player', method = 'SetPosition',
  signature = 'ox', args = { track, us }, timeout = 5000,
  on_reply = function(ret, err) end }        -- ret: the reply's arguments, as a list

bus:get(dest, path, iface, name, function(value, err) end)
bus:get_all(dest, path, iface, function(props, err) end)
bus:set(dest, path, iface, name, signature, value, function(err) end)

local sub = bus:watch{ sender = dest, path = path, iface = iface, member = 'Seeked',
  arg0 = nil, on_signal = function(args, msg) end }   -- msg: sender, path, iface, member
bus:watch_props(dest, path, iface, function(changed, invalidated) end)
bus:watch_names('org.mpris.MediaPlayer2.', function(name, owner) end)  -- owner nil: gone
sub:cancel()
```

- **From D-Bus:** variants are unwrapped. Integers (including `x` and `t`)
  become Lua integers and doubles become numbers. `a{..}` becomes a table
  keyed by the dict key, and other arrays and structs become lists. Object
  paths and signatures become strings, and so does `ay`.
- **To D-Bus:** `signature` gives the arguments' types, and a value that
  doesn't fit is an error on the call. For `v`, the type is guessed:
  booleans are `b`, integers `x`, numbers `d` and strings `s`; lists are
  `as`, `ax`, `ad`, `ab` or `av`; other tables are `a{sv}`. File descriptors
  (`h`) aren't supported.
- **Failures:** a call that fails or times out (after 5 s by default, at
  most 120 s) answers `on_reply(nil, err)`. If the bus can't be reached, the
  client keeps trying to connect again.
- **Watches** with a well-known `sender` only receive signals from that
  name's current owner. `watch_names(prefix, fn)` reports the names that
  already exist and each later change. All watches end when the plugin
  reloads or stops.

### The `mpris` module

`require('mpris')` provides the MPRIS logic that `player` uses, for plugins
that want to show a player their own way:

```lua
local mpris = require('mpris')
local w = mpris.watch({ app = dock.app, player = opts.player }, function(p)
  -- p: the chosen player, or nil. Called again when the choice or the player changes.
end)
w:refresh()   -- choose again (call it in on_app)
w:sync()      -- ask the player for its position now
w:cancel()
mpris.players()  -- every player, by bus name
```

Players are chosen as described for [`player`](#player). A player has:

- **Fields:** `name` (the bus name), `id` (the suffix), `identity`,
  `desktop`, `status` (`'Playing'`, `'Paused'` or `'Stopped'`), `playing`,
  `title`, `artist`, `album`, `art` (a URL), `length` and `position` (in
  seconds, with `position` measured at `at`, a `dock.now()` time), `rate`,
  `track`, `volume`, and the flags `can_play`, `can_pause`, `can_seek`,
  `can_next`, `can_previous`, `can_control` and `can_raise`.
- **Methods:** `p:now()` returns the current position. `play_pause`,
  `play`, `pause`, `stop`, `next`, `previous`, `set_position(seconds)`,
  `seek(seconds)` and `raise` control the player.

## Examples

### Unread count from a command

This plugin puts a badge with the number of unread mails on a mail app. It
runs the command in the background, and `icon()` only reads the result.

```lua
-- [app.thunderbird]
-- widget = { plugin = "unread", command = "notmuch count tag:unread", interval = 60000 }
local ui = dock.ui
local P = {}
local command, interval, unread = 'notmuch count tag:unread', 60000, 0
local running

local function refresh()
  if running then return end
  running = dock.spawn(command, {
    on_line = function(line) unread = tonumber(line) or unread end,
    on_exit = function() running = nil; dock.redraw() end,
  })
end

function P.setup(opts)
  if type(opts.command) == 'string' then command = opts.command end
  if type(opts.interval) == 'number' then interval = math.max(opts.interval, 1000) end
  refresh()
  dock.every(interval, refresh)
end

function P.icon()
  if unread == 0 then return nil end           -- the app's own icon
  return ui.stack{ align = 'top-right',
    ui.app_icon{},
    ui.box{ variant = 'badge', ui.text{ unread > 99 and '99+' or unread, style = 'caption' } },
  }
end

function P.label()
  return unread > 0 and (dock.app.name .. ' — ' .. unread .. ' unread') or nil
end

return P
```

### CPU meter

This standalone widget reads `/proc/stat` and draws a ring as its icon, with
a progress bar in its popup.

```lua
-- [widget.cpu]
-- name = "CPU"
local ui, theme = dock.ui, dock.theme
local P = {}
local last, usage = nil, 0

local function sample()
  local text = dock.read('/proc/stat')
  if not text then return end
  local nums = {}
  for n in text:match('^cpu%s+([^\n]+)'):gmatch('%d+') do nums[#nums + 1] = tonumber(n) end
  local idle = nums[4] + (nums[5] or 0)
  local total = 0
  for _, n in ipairs(nums) do total = total + n end
  if last and total > last.total then
    usage = 1 - (idle - last.idle) / (total - last.total)
  end
  last = { idle = idle, total = total }
  dock.redraw()
end

function P.setup()
  sample()
  dock.every(2000, sample)
end

function P.label()
  return string.format('CPU %d%%', math.floor(usage * 100 + 0.5))
end

function P.icon()
  return ui.canvas{ draw = function(c)
    c:circle{ x = 50, y = 50, r = 46, fill = theme.surface }
    c:arc{ x = 50, y = 50, r = 34, from = 0, to = 2 * math.pi, stroke = theme.track, width = 10 }
    c:arc{ x = 50, y = 50, r = 34, from = -math.pi / 2,
      to = -math.pi / 2 + usage * 2 * math.pi, stroke = theme.accent, width = 10, cap = 'round' }
    c:text{ math.floor(usage * 100 + 0.5), x = 50, y = 50, size = 24, bold = true,
      align = 'center', color = theme.text }
  end }
end

function P.popup()
  return ui.col{ padding = 16, gap = 8, width = 200,
    ui.text{ 'Processor', style = 'title' },
    ui.progress{ value = usage, max = 1 },
    ui.text{ string.format('%.1f%% busy', usage * 100), style = 'muted' },
  }
end

return P
```

### Volume slider

This popup controls PipeWire's default sink with `wpctl`. The slider keeps
its own value while you drag it, and the plugin only sets the volume when
you release it.

```lua
-- [app.pavucontrol]
-- widget = "volume"
local ui = dock.ui
local P = {}
local volume, muted = 0, false

local function read()
  dock.spawn({ 'wpctl', 'get-volume', '@DEFAULT_AUDIO_SINK@' }, {
    on_line = function(line)              -- "Volume: 0.45 [MUTED]"
      volume = tonumber(line:match('[%d.]+')) or volume
      muted = line:find('MUTED') ~= nil
    end,
    on_exit = function() dock.redraw() end,
  })
end

P.on_open = read

function P.label()
  return string.format('Volume %d%%%s', math.floor(volume * 100 + 0.5), muted and ' (muted)' or '')
end

function P.popup()
  return ui.col{ padding = 16, gap = 10, width = 240,
    ui.row{ align = 'center', justify = 'between',
      ui.text{ 'Output', style = 'title' },
      ui.toggle{ key = 'mute', value = not muted, on_change = function(on)
        muted = not on
        dock.run({ 'wpctl', 'set-mute', '@DEFAULT_AUDIO_SINK@', muted and '1' or '0' })
        dock.redraw()
      end },
    },
    ui.slider{ key = 'volume', value = volume, max = 1.5, enabled = not muted,
      on_commit = function(v)
        volume = v
        dock.run({ 'wpctl', 'set-volume', '@DEFAULT_AUDIO_SINK@', string.format('%.2f', v) })
        dock.redraw()
      end },
  }
end

function P.setup() read() end

return P
```

### Battery over D-Bus

This standalone widget watches UPower's display device on the system bus, so
it updates only when the battery changes instead of polling. Its icon is a
ring drawn on a canvas, with the charge in the middle and the remaining time
below it. The ring is green while charging, orange when the battery is low
and red when it is critical. The label shows the remaining time, and the
popup adds the status, power draw and energy.

The `span` option makes the tile wider on a bottom dock: 1.5 or 2 icons.
A wider tile shows the remaining time in large text next to the ring. Side
docks always use the normal size. The icon has one canvas whose viewbox is
as wide as the tile, so it scales to every dock's icon size.

```lua
-- [widget.battery]
-- name = "Battery"
-- low = 20          # % below which the ring turns orange
-- critical = 10     # % below which it turns red
-- span = 2          # Tile width on a bottom dock: 1, 1.5 or 2 icons
local ui, theme = dock.ui, dock.theme
local P = {}
local DEST = 'org.freedesktop.UPower'
local PATH = '/org/freedesktop/UPower/devices/DisplayDevice'
local IFACE = 'org.freedesktop.UPower.Device'
-- UPower's State values.
local CHARGING, DISCHARGING, FULL = 1, 2, 4
local STATES = { 'Charging', 'Discharging', 'Empty', 'Fully charged', 'Not charging', 'Discharging' }
local low, critical = 20, 10
local b = {}   -- the device's properties, by their UPower names

local function apply(props)
  for k, v in pairs(props) do b[k] = v end
  dock.redraw()
end

function P.setup(opts)
  if type(opts.low) == 'number' then low = opts.low end
  if type(opts.critical) == 'number' then critical = opts.critical end
  -- Wider along a bottom dock only; a side dock keeps square tiles.
  local span = (opts.span == 1.5 or opts.span == 2) and opts.span or 1
  dock.span({ horizontal = span, vertical = 1 })
  local bus = dock.dbus.system()
  bus:get_all(DEST, PATH, IFACE, function(props, err)
    if props then apply(props) else print(err) end
  end)
  bus:watch_props(DEST, PATH, IFACE, apply)
end

local function color()
  local pct = b.Percentage or 0
  if b.State == CHARGING then return '#30d158' end
  if pct <= critical then return '#ff453a' end
  if pct <= low then return '#ff9f0a' end
  return theme.accent
end

-- Seconds until full or empty, if UPower knows (0 means it doesn't).
local function remaining()
  if b.State == CHARGING and (b.TimeToFull or 0) > 0 then return b.TimeToFull end
  if b.State == DISCHARGING and (b.TimeToEmpty or 0) > 0 then return b.TimeToEmpty end
end

local function short(s)   -- "2:05"
  local m = s // 60
  return string.format('%d:%02d', m // 60, m % 60)
end

local function long(s)    -- "2 h 5 min"
  local m = (s + 30) // 60
  if m < 60 then return m .. ' min' end
  return string.format('%d h %d min', m // 60, m % 60)
end

-- The ring, in the 100 x 100 square at the viewbox's left. `time`: show the
-- remaining time inside it.
local function ring(c, time)
  local pct = math.max(0, math.min(100, b.Percentage or 0))
  c:circle{ x = 50, y = 50, r = 48, fill = theme.surface }
  c:circle{ x = 50, y = 50, r = 38, stroke = theme.track, width = 10 }
  if pct > 0 then
    c:arc{ x = 50, y = 50, r = 38, from = -math.pi / 2,
      to = -math.pi / 2 + pct / 100 * 2 * math.pi,
      stroke = color(), width = 10, cap = 'round' }
  end
  if b.State == CHARGING then   -- a bolt above the number
    c:path{ d = 'M 52 22 L 45 31 L 50 31 L 48 37 L 55 28 L 50 28 Z', fill = color() }
  end
  c:text{ math.floor(pct + 0.5) .. '%', x = 50, y = 50, size = 20, bold = true,
    align = 'center', color = theme.text }
  local s = remaining()
  if time and s then
    c:text{ short(s), x = 50, y = 68, size = 11, align = 'center', color = theme.muted }
  end
end

-- The ring with the time inside: the square icon, and the popup's.
local function draw(c) ring(c, true) end

function P.icon(tile)
  if not b.Percentage or b.IsPresent == false then return nil end   -- the placeholder
  if tile.span <= 1 then
    return ui.canvas{ draw = draw }
  end
  -- Wide: the ring, and the time beside it, in a viewbox as wide as the tile.
  local extra = 100 * (tile.span - 1)
  return ui.canvas{ viewbox = { 100 + extra, 100 }, draw = function(c)
    ring(c, false)
    local s = remaining()
    local x, size = 100 + extra / 2, math.min(30, extra * 0.4)
    local main = s and short(s) or (b.State == FULL and 'Full' or '–')
    local sub = s and (b.State == CHARGING and 'to full' or 'left')
    c:text{ main, x = x, y = sub and 42 or 50, size = size, bold = true,
      align = 'center', color = b.State == CHARGING and color() or theme.text }
    if sub then
      c:text{ sub, x = x, y = 42 + size, size = size * 0.6, align = 'center', color = theme.muted }
    end
  end }
end

function P.label()
  if not b.Percentage then return nil end
  local text = math.floor(b.Percentage + 0.5) .. '%'
  local s = remaining()
  if s then
    return text .. ' · ' .. long(s) .. (b.State == CHARGING and ' until full' or ' left')
  end
  if b.State == FULL then return text .. ' · Fully charged' end
  return text
end

-- Shown on hover, after the widget's popup-delay.
function P.popup()
  if not b.Percentage or b.IsPresent == false then
    return ui.col{ padding = 16, ui.text{ 'No battery found', style = 'muted' } }
  end
  local rows = { columns = { 'auto', '1fr' }, gap = 16, row_gap = 6, width = '100%' }
  local function add(name, value)
    rows[#rows + 1] = ui.text{ name, style = 'muted' }
    rows[#rows + 1] = ui.text{ value, align = 'end' }
  end
  add('Status', STATES[b.State] or 'Unknown')
  local s = remaining()
  if s then add(b.State == CHARGING and 'Full in' or 'Remaining', long(s)) end
  if (b.EnergyRate or 0) > 0 then
    add(b.State == CHARGING and 'Charging at' or 'Using', string.format('%.1f W', b.EnergyRate))
  end
  if (b.EnergyFull or 0) > 0 then
    add('Energy', string.format('%.1f / %.1f Wh', b.Energy or 0, b.EnergyFull))
  end
  return ui.col{ padding = 16, gap = 12, width = 260, align = 'center',
    ui.text{ 'Battery', style = 'title' },
    ui.canvas{ width = 120, height = 120, draw = draw },
    ui.grid(rows),
  }
end

return P
```

## Tips and troubleshooting

- **Watch the log.** Run `wl-dock` in a terminal. Load errors, hook errors,
  budget overruns, restarts and `print` output all appear there.
- **Start from a bundled plugin.** Copy one from [`plugins/`](plugins) to
  `~/.config/wl-dock/plugins/` under the same name to replace it, or under a
  new name to start your own. [`tests/plugins/kitchen-sink.lua`](tests/plugins/kitchen-sink.lua)
  uses every UI component.
- **Keep hooks cheap and pure.** They should describe the current state, not
  fetch it. Start commands and requests in `setup`, timers or `on_open`, and
  store the results.
- **Call `dock.redraw()` after changing state.** Calling it several times in
  one callback costs no more than calling it once, and the dock repaints
  only what changed.
- **Stop work nobody sees.** For example, only tick a popup's clock between
  `on_open` and `on_close`, as `player` does.
- **Use theme colors** so the widget looks right in light and dark mode.
- **Prefer argv lists to shell strings** when the command includes values
  from options or data.
- **Nothing on screen?** Check that `[widgets] enabled` isn't `false`, that
  the plugin name only uses letters, digits, `-` and `_`, that a standalone
  widget has both its `[widget.<name>]` table and its `@name` pin, and that
  the hook returned a node from `dock.ui` rather than a plain table.
