-- Downloads: a browser's downloads in progress as a progress disc (or bar)
-- at the corner of its icon, and the downloads in its popup, where a click
-- opens a finished one or shows it in its folder.
--
-- Put it on the browser:
--
--   [app.firefox]
--   widget = "downloads"
--
-- or pin it on its own (`wl-dock pin @downloads`), where it follows every
-- browser:
--
--   [widget.downloads]
--   name = "Downloads"
--
-- Downloads are seen in the folder: Firefox writes to `name.part` and
-- Chromium-based browsers to `name.crdownload`, and the file gets its own
-- name when it is done. A file doesn't say how big it will be, so the disc
-- turns while data arrives instead of filling, unless the browser publishes
-- its progress over D-Bus (Unity LauncherEntry, as Chromium does), which the
-- disc then fills to. A download that hasn't grown for a while stands still
-- in grey (waiting); one that finished fills the disc with a check mark for
-- a moment.
--
-- Options:
--   folders   Folders to watch, a string or a list (default: the XDG download
--             folder, else ~/Downloads)
--   browser   "firefox" (.part files), "chromium" (.crdownload files) or "all";
--             default: guessed from the app's names, "all" standalone
--   style     "ring" (default): a disc at the icon's corner, or "bar": a pill
--             along its bottom
--   keep      Finished downloads the popup lists (default 5, 0–20)
--   open      argv that opens a finished download, the path appended
--             (default ["xdg-open"])
--   interval  Least ms between looks at the folders while files change
--             (default 1000, at least 250)

local ui, theme = dock.ui, dock.theme
local anim = require("anim")
local P = {}

local HOME = os.getenv("HOME") or ""
-- A download whose file hasn't grown for this long (s) shows as waiting.
local STALL = 3
-- How long (s) the ring stays full after the last download finishes.
local DONE = 2
local PRINTF = "%s\t%T@\t%p\n"
local PARTIALS = {
	"(",
	"-name",
	"*.part",
	"-o",
	"-name",
	"*.crdownload",
	"-o",
	"-name",
	".com.google.Chrome.*",
	"-o",
	"-name",
	".org.chromium.Chromium.*",
	")",
}

local folders = {}
local style, keep, interval = "ring", 5, 1000
local open_argv = { "xdg-open" }
-- "part", "crdownload", or nil for both; `chosen`: set by the `browser` option.
local only, chosen

-- Downloads in progress, by the path of their partial file.
local active = {}
-- Finished downloads, newest first: { path, name, size, at }.
local finished = {}
-- Paths the watches reported since the last look.
local changed = {}
-- Until when (dock.now()) the disc shows a finished download.
local done_until = 0
-- What the browser publishes over D-Bus: whether progress shows, and 0–1.
local entry_visible, entry_value

local scanning, again, pending, ticker = false, false, nil, nil
local last_scan = -math.huge
local scan, request

local FIREFOX = { "firefox", "librewolf", "waterfox", "floorp", "zen", "mullvad", "torbrowser", "icecat" }
local CHROMIUM = { "chrom", "brave", "vivaldi", "edge", "opera", "yandex", "thorium" }

local function guess()
	local app = dock.app
	if not app then
		return nil
	end
	for _, name in ipairs({ app.id, app.desktop, app.wm_class, app.name }) do
		if type(name) == "string" then
			local n = name:lower()
			for _, w in ipairs(FIREFOX) do
				if n:find(w, 1, true) then
					return "part"
				end
			end
			for _, w in ipairs(CHROMIUM) do
				if n:find(w, 1, true) then
					return "crdownload"
				end
			end
		end
	end
	return nil
end

-- A partial file's final name ("" if it isn't known yet) and kind, or nil.
local function partial(name)
	local base = name:match("^(.+)%.part$")
	if base then
		return base, "part"
	end
	base = name:match("^(.+)%.crdownload$")
	if base then
		if base:match("^Unconfirmed %d+$") then
			base = ""
		end
		return base, "crdownload"
	end
	if name:match("^%.com%.google%.Chrome%.") or name:match("^%.org%.chromium%.Chromium%.") then
		return "", "crdownload"
	end
	return nil
end

local function split(path)
	return path:match("^(.*)/([^/]*)$")
end

local function expand(path)
	if path == "~" then
		path = HOME
	elseif path:sub(1, 2) == "~/" then
		path = HOME .. path:sub(2)
	end
	local function home()
		return HOME
	end
	path = path:gsub("^%$HOME", home):gsub("^%${HOME}", home)
	if #path > 1 then
		path = path:gsub("/+$", "")
	end
	return path
end

local function add_folder(path)
	path = expand(path)
	if path:sub(1, 1) ~= "/" then
		print("not an absolute folder: " .. path)
		return
	end
	for _, f in ipairs(folders) do
		if f == path then
			return
		end
	end
	folders[#folders + 1] = path
end

-- The XDG download folder from user-dirs.dirs, else ~/Downloads.
local function default_folder()
	local config = os.getenv("XDG_CONFIG_HOME")
	if not config or config == "" then
		config = HOME .. "/.config"
	end
	local text = dock.read(config .. "/user-dirs.dirs")
	local dir = text and ("\n" .. text):match('\nXDG_DOWNLOAD_DIR="([^"\n]*)"')
	if dir and dir ~= "" and expand(dir) ~= HOME then
		return dir
	end
	return "~/Downloads"
end

-- Run find with `args`, adding what it prints to `found` (path → size, mtime).
local function find(args, found, done)
	local argv = { "sh", "-c", 'exec find "$@" 2>/dev/null', "find" }
	for _, a in ipairs(args) do
		argv[#argv + 1] = a
	end
	dock.spawn(argv, {
		on_line = function(line)
			local size, mtime, path = line:match("^(%d+)\t([%d.]+)\t(.+)$")
			if path then
				found[path] = { size = tonumber(size), mtime = tonumber(mtime) }
			end
		end,
		on_exit = done,
	})
end

-- The finished file of the download `d`, which is gone: its own name, or
-- else the newest file that changed in its folder. nil: cancelled.
local function completed(d, stats, touched, taken)
	if d.base ~= "" then
		local p = d.dir .. "/" .. d.base
		local f = stats[p]
		if f and f.size > 0 and not taken[p] then
			taken[p] = true
			return p
		end
	end
	local best, at
	for p in pairs(touched) do
		local f = stats[p]
		if f and f.size > 0 and not taken[p] then
			local dir, name = split(p)
			if dir == d.dir and name:sub(1, 1) ~= "." and not partial(name) and (not at or f.mtime > at) then
				best, at = p, f.mtime
			end
		end
	end
	if best then
		taken[best] = true
	end
	return best
end

local function add_finished(path, size, now)
	for i = #finished, 1, -1 do
		if finished[i].path == path then
			table.remove(finished, i)
		end
	end
	local _, name = split(path)
	table.insert(finished, 1, { path = path, name = name, size = size, at = now })
	while #finished > keep do
		table.remove(finished)
	end
end

local function apply(partials, stats, touched)
	local now = dock.now()
	local started = {}
	for path, f in pairs(partials) do
		local dir, name = split(path)
		local base, kind = partial(name)
		if base and (only == nil or only == kind) then
			local d = active[path]
			if not d then
				d = { path = path, dir = dir, name = name, base = base, size = f.size, at = now, since = now }
				active[path] = d
				started[#started + 1] = d
			elseif f.size ~= d.size then
				local dt = now - d.at
				if dt > 0 and f.size > d.size then
					local rate = (f.size - d.size) / dt
					d.rate = d.rate and (d.rate + rate) / 2 or rate
				end
				d.size, d.at = f.size, now
			elseif now - d.at >= STALL then
				d.rate = 0
			end
		end
	end

	local taken, ended = {}, false
	for path, d in pairs(active) do
		if not partials[path] then
			active[path] = nil
			local final = completed(d, stats, touched, taken)
			if final then
				add_finished(final, stats[final].size, now)
				ended = true
			else
				-- Renamed to another partial file (Chromium names an
				-- "Unconfirmed" download once its name is known).
				for _, s in ipairs(started) do
					if s.dir == d.dir and not s.carried then
						s.since, s.rate, s.carried = d.since, d.rate, true
						break
					end
				end
			end
		end
	end
	-- Finished files since moved or deleted.
	for i = #finished, 1, -1 do
		if not stats[finished[i].path] then
			table.remove(finished, i)
		end
	end

	if next(active) then
		if not ticker then
			-- Look now and then while downloading: a stalled file changes nothing.
			ticker = dock.every(2000, request)
		end
	else
		if ticker then
			ticker:cancel()
			ticker = nil
		end
		if ended then
			done_until = now + DONE
			dock.timer(math.floor(DONE * 1000) + 50, dock.redraw)
		end
	end
	dock.redraw()
end

scan = function()
	if scanning then
		again = true
		return
	end
	if #folders == 0 then
		return
	end
	last_scan = dock.now()
	local list, seen = {}, {}
	local function want(p)
		if not seen[p] then
			seen[p] = true
			list[#list + 1] = p
		end
	end
	for p in pairs(changed) do
		if p:sub(1, 1) == "/" then
			want(p)
		end
	end
	for _, f in ipairs(finished) do
		want(f.path)
	end
	for _, d in pairs(active) do
		if d.base ~= "" then
			want(d.dir .. "/" .. d.base)
		end
	end
	local touched = changed
	changed = {}

	local partials, stats = {}, {}
	local left = #list > 0 and 2 or 1
	local function done()
		left = left - 1
		if left == 0 then
			scanning = false
			apply(partials, stats, touched)
			if again then
				again = false
				request()
			end
		end
	end

	local args = {}
	for _, f in ipairs(folders) do
		args[#args + 1] = f
	end
	for _, a in ipairs({ "-maxdepth", "1", "-mindepth", "1", "-type", "f" }) do
		args[#args + 1] = a
	end
	for _, a in ipairs(PARTIALS) do
		args[#args + 1] = a
	end
	args[#args + 1] = "-printf"
	args[#args + 1] = PRINTF

	scanning = true
	local ok, err = pcall(find, args, partials, done)
	if not ok then
		scanning = false
		print(err)
		return
	end
	if #list > 0 then
		for _, a in ipairs({ "-maxdepth", "0", "-type", "f", "-printf", PRINTF }) do
			list[#list + 1] = a
		end
		ok, err = pcall(find, list, stats, done)
		if not ok then
			-- The folders' answer finishes the look.
			left = left - 1
			print(err)
		end
	end
end

-- Look at the folders, at most once per `interval`.
request = function()
	if pending then
		return
	end
	local wait = last_scan + interval / 1000 - dock.now()
	if wait <= 0 then
		scan()
		return
	end
	pending = dock.timer(math.ceil(wait * 1000), function()
		pending = nil
		scan()
	end)
end

local function on_change(paths)
	for _, p in ipairs(paths) do
		changed[p] = true
	end
	request()
end

-- The LauncherEntry URIs of the widget's app.
local function uris()
	local set, app = {}, dock.app
	for _, n in ipairs({ app.desktop or false, app.id or false }) do
		if type(n) == "string" and n ~= "" then
			set["application://" .. (n:gsub("%.desktop$", "")) .. ".desktop"] = true
		end
	end
	return set
end

local function on_entry(args)
	local uri, props = args[1], args[2]
	if type(uri) ~= "string" or type(props) ~= "table" or not uris()[uri] then
		return
	end
	if type(props["progress-visible"]) == "boolean" then
		entry_visible = props["progress-visible"]
	end
	local v = props.progress
	if type(v) == "number" and v == v then
		entry_value = math.floor(math.max(0, math.min(v, 1)) * 100 + 0.5) / 100
	end
	dock.redraw()
end

function P.setup(opts)
	local list = opts.folders
	if type(list) == "string" then
		list = { list }
	end
	if type(list) == "table" then
		for _, f in ipairs(list) do
			if type(f) == "string" and f ~= "" then
				add_folder(f)
			end
		end
	else
		add_folder(default_folder())
	end
	if opts.style == "ring" or opts.style == "bar" then
		style = opts.style
	end
	if type(opts.keep) == "number" then
		keep = math.max(0, math.min(20, math.floor(opts.keep)))
	end
	if type(opts.interval) == "number" then
		interval = math.max(250, opts.interval)
	end
	if type(opts.open) == "table" and #opts.open > 0 then
		local argv = {}
		for _, a in ipairs(opts.open) do
			argv[#argv + 1] = tostring(a)
		end
		open_argv = argv
	end
	local browsers = { firefox = "part", chromium = "crdownload", all = false }
	if browsers[opts.browser] ~= nil then
		chosen = true
		only = browsers[opts.browser] or nil
	else
		only = guess()
	end

	local poll = false
	for _, f in ipairs(folders) do
		local w, err = dock.watch(f, on_change)
		if not w then
			print(err)
			poll = true
		end
	end
	if poll then
		-- A folder that isn't there yet: look for it now and then.
		dock.every(10000, request)
	end
	if dock.app then
		dock.dbus.session():watch({
			iface = "com.canonical.Unity.LauncherEntry",
			member = "Update",
			on_signal = on_entry,
		})
	end
	scan()
end

function P.on_app()
	if chosen then
		return
	end
	local now = guess()
	if now ~= only then
		only = now
		active = {}
		dock.redraw()
		request()
	end
end

P.on_open = function()
	request()
end

-- Downloads in progress, oldest first.
local function downloads()
	local list = {}
	for _, d in pairs(active) do
		list[#list + 1] = d
	end
	table.sort(list, function(a, b)
		if a.since ~= b.since then
			return a.since < b.since
		end
		return a.name < b.name
	end)
	return list
end

-- The progress the browser publishes, 0–1, while something downloads.
local function fraction()
	return entry_visible and entry_value or nil
end

local function bytes(n)
	if n < 1024 then
		return math.floor(n) .. " B"
	end
	local units, i = { "KB", "MB", "GB", "TB" }, 0
	repeat
		n, i = n / 1024, i + 1
	until n < 1024 or i == #units
	return string.format(n < 10 and "%.1f %s" or "%.0f %s", n, units[i])
end

-- A download's name, or "Starting…" while it isn't known.
local function title(d)
	return d.base ~= "" and d.base or "Starting…"
end

-- `45% · 2.1 GB · 12 MB/s`; while it waits, "Waiting" for the speed.
local function detail(d, p)
	local parts = {}
	if p then
		parts[#parts + 1] = string.format("%d%%", math.floor(p * 100 + 0.5))
	end
	parts[#parts + 1] = bytes(d.size)
	if d.rate and d.rate > 0 then
		parts[#parts + 1] = bytes(d.rate) .. "/s"
	elseif d.rate then
		parts[#parts + 1] = "Waiting"
	end
	return table.concat(parts, " · ")
end

local function ago(t)
	local s = dock.now() - t
	if s < 60 then
		return "Just now"
	elseif s < 3600 then
		return string.format("%d min ago", s // 60)
	elseif s < 86400 then
		return string.format("%d h ago", s // 3600)
	end
	return (os.date("%e %b", math.floor(t)):gsub("^%s+", ""))
end

local ICONS = {}
for icon, exts in pairs({
	["image-x-generic"] = { "png", "jpg", "jpeg", "gif", "webp", "avif", "svg", "bmp", "tif", "tiff", "heic" },
	["audio-x-generic"] = { "mp3", "flac", "ogg", "opus", "wav", "m4a", "aac" },
	["video-x-generic"] = { "mp4", "mkv", "webm", "mov", "avi" },
	["package-x-generic"] = { "zip", "tar", "gz", "xz", "zst", "bz2", "7z", "rar", "deb", "rpm", "appimage" },
	["application-pdf"] = { "pdf" },
	["x-office-document"] = { "doc", "docx", "odt", "rtf" },
	["x-office-spreadsheet"] = { "xls", "xlsx", "ods", "csv" },
	["x-office-presentation"] = { "ppt", "pptx", "odp" },
}) do
	for _, ext in ipairs(exts) do
		ICONS[ext] = icon
	end
end
-- Images ui.image can draw.
local THUMBS = { png = true, jpg = true, jpeg = true, webp = true, svg = true }

local function ext(name)
	return (name:match("%.([^.]+)$") or ""):lower()
end

-- A file's picture in a THUMB × THUMB frame, so images and icons line up.
local THUMB = 36
local function thumb(name, path)
	local pic
	if path and THUMBS[ext(name)] then
		pic = ui.image({ src = path, width = THUMB, height = THUMB, fit = "cover", radius = 4 })
	else
		pic = ui.icon({ ICONS[ext(name)] or "text-x-generic", size = 32 })
	end
	return ui.stack({ width = THUMB, height = THUMB, pic })
end

local function open(path)
	local argv = {}
	for _, a in ipairs(open_argv) do
		argv[#argv + 1] = a
	end
	argv[#argv + 1] = path
	local ok, err = dock.run(argv)
	if not ok then
		print(err)
	end
end

-- `path` as a file:// URI.
local function uri(path)
	local escaped = path:gsub("[^%w%-%._~/]", function(ch)
		return string.format("%%%02X", ch:byte())
	end)
	return "file://" .. escaped
end

-- Show `path` selected in the file manager (FileManager1), else open its
-- folder.
local function show(path)
	local dir = split(path) or folders[1]
	local ok, err = pcall(function()
		dock.dbus.session():call({
			dest = "org.freedesktop.FileManager1",
			path = "/org/freedesktop/FileManager1",
			iface = "org.freedesktop.FileManager1",
			method = "ShowItems",
			signature = "ass",
			args = { { uri(path) }, "" },
			on_reply = function(_, e)
				if e then
					open(dir)
				end
			end,
		})
	end)
	if not ok then
		print(err)
		open(dir)
	end
end

-- Downloads in progress have progress (0–1, eased in), are busy (data
-- arriving, size unknown), or wait (nothing arrived for STALL s); then they
-- are done for DONE s. nil: nothing to show.
local function state(list)
	if #list > 0 then
		local p = fraction()
		if p then
			return "progress", p
		end
		for _, d in ipairs(list) do
			if d.rate ~= 0 then
				return "busy"
			end
		end
		return "waiting"
	elseif dock.now() < done_until then
		return "done"
	end
end

-- The corner disc, in a 100 × 100 viewbox: a surface disc with an outline,
-- and a ring on it.
local RING = { x = 50, y = 50, r = 29, width = 11 }
local function disc(c)
	c:circle({ x = 50, y = 50, r = 45, fill = theme.surface, stroke = theme.border, width = 2 })
end
local function with(t, more)
	local out = {}
	for k, v in pairs(t) do
		out[k] = v
	end
	for k, v in pairs(more) do
		out[k] = v
	end
	return out
end

-- The disc for `kind`, `size` pixels: 42% of the tile's thickness, inset
-- 2% from its corner.
local function corner(kind, p, size)
	local c = { key = "disc", width = size, height = size }
	if kind == "progress" then
		local ring = anim.ring(RING)
		c.frames, c.value, c.transition = 60, p, 300
		c.draw = function(cv, t)
			disc(cv)
			ring(cv, t)
		end
	elseif kind == "busy" then
		local spinner = anim.spinner(RING)
		c.frames, c.loop = 24, 1.2
		c.draw = function(cv, t)
			disc(cv)
			spinner(cv, t)
		end
	elseif kind == "waiting" then
		local still = anim.spinner(with(RING, { color = theme.muted }))
		c.draw = function(cv)
			disc(cv)
			still(cv, 0)
		end
	else
		c.draw = function(cv)
			cv:circle({ x = 50, y = 50, r = 45, fill = theme.accent })
			cv:path({ d = "M 31 51 L 44 64 L 69 37", stroke = theme.accent_text, width = 9, cap = "round" })
		end
	end
	return ui.canvas(c)
end

-- The bar for `kind`: a pill along the tile's bottom, 64% of its length.
local function pill(kind, p, tile, thick)
	local inset, h, w = thick * 0.02, thick * 0.1, tile.w * 0.64
	local bw, bh = w + 2 * inset, h + 2 * inset
	local function back(cv)
		cv:rect({ x = 0, y = 0, w = bw, h = bh, radius = bh / 2, fill = theme.surface })
	end
	local track = { x = inset, y = inset, w = w, h = h }
	local c = { key = "bar", width = bw, height = bh, viewbox = { bw, bh } }
	local bar
	if kind == "progress" then
		bar = anim.bar(with(track, { fill = true }))
		c.frames, c.value, c.transition = 60, p, 300
	elseif kind == "busy" then
		bar = anim.bar(track)
		c.frames, c.loop, c.mirror = 24, 1.6, true
	elseif kind == "waiting" then
		bar = anim.bar(with(track, { color = theme.muted }))
	else
		bar = anim.bar(with(track, { fill = true }))
	end
	local t = kind == "done" and 1 or nil
	c.draw = function(cv, s)
		back(cv)
		bar(cv, s or t)
	end
	return ui.row({
		width = tile.w,
		height = tile.h,
		justify = "center",
		align = "end",
		padding = { 0, 0, thick * 0.04, 0 },
		ui.canvas(c),
	})
end

function P.icon(tile)
	tile = tile or { w = 96, h = 96 }
	local thick = math.min(tile.w, tile.h)
	local list = downloads()
	local base = dock.app and ui.app_icon({}) or ui.icon({ "folder-download", size = thick })
	local kind, p = state(list)
	if not kind then
		return not dock.app and base or nil
	end
	local shown
	if style == "bar" then
		shown = pill(kind, p, tile, thick)
	else
		shown = corner(kind, p, thick * 0.46)
	end
	local layers = { align = "bottom-right", base, shown }
	if #list > 1 then
		layers[#layers + 1] = ui.row({
			width = tile.w,
			height = tile.h,
			justify = "end",
			align = "start",
			ui.box({ variant = "badge", ui.text({ tostring(#list), style = "caption" }) }),
		})
	end
	return ui.stack(layers)
end

function P.label()
	local list = downloads()
	if #list == 1 then
		return title(list[1]) .. " — " .. detail(list[1], fraction())
	elseif #list > 1 then
		local rate = 0
		for _, d in ipairs(list) do
			rate = rate + (d.rate or 0)
		end
		local text = #list .. " downloads"
		local p = fraction()
		if p then
			text = string.format("%s — %d%%", text, math.floor(p * 100 + 0.5))
		end
		if rate > 0 then
			text = text .. " · " .. bytes(rate) .. "/s"
		end
		return text
	end
	return nil
end

-- A download in progress: its name, a bar and how far it got; a click opens
-- its folder.
local function active_row(d, p)
	local key = "active:" .. d.path
	local bar
	if p then
		bar = ui.progress({ key = "bar:" .. d.path, value = p, transition = 300, height = 4 })
	elseif d.rate == 0 then
		bar = ui.progress({ key = "bar:" .. d.path, value = 0, height = 4 })
	else
		bar = ui.progress({ key = "bar:" .. d.path, indeterminate = true, height = 4 })
	end
	local named = d.base ~= ""
	return ui.row({
		key = key,
		gap = 10,
		padding = { 4, 6 },
		radius = 6,
		align = "center",
		hover = { background = theme.hover },
		on_click = function()
			open(d.dir)
		end,
		thumb(named and d.base or ""),
		ui.col({
			grow = 1,
			gap = 4,
			ui.text({ title(d), max_lines = 1, color = not named and theme.muted or nil }),
			bar,
			ui.text({ detail(d, p), style = "caption", color = theme.muted, max_lines = 1 }),
		}),
	})
end

-- A finished download: a click opens it, its button shows it in its folder.
local function finished_row(f)
	return ui.row({
		key = "open:" .. f.path,
		gap = 10,
		padding = { 4, 6 },
		radius = 6,
		align = "center",
		hover = { background = theme.hover },
		on_click = function()
			open(f.path)
		end,
		thumb(f.name, f.path),
		ui.col({
			grow = 1,
			gap = 1,
			ui.text({ f.name, max_lines = 1 }),
			ui.text({ bytes(f.size) .. " · " .. ago(f.at), style = "caption", color = theme.muted, max_lines = 1 }),
		}),
		ui.button({
			key = "show:" .. f.path,
			icon = "folder-open-symbolic",
			variant = "ghost",
			tooltip = "Show in folder",
			on_click = function()
				show(f.path)
			end,
		}),
	})
end

-- A section's caption, lined up with the rows; `apart`: 6 px more above.
local function header(text, apart)
	return ui.row({
		padding = apart and { 6, 6, 0, 6 } or { 0, 6 },
		ui.text({ text, style = "caption", weight = "bold", color = theme.muted }),
	})
end

local function clear()
	finished = {}
	dock.redraw()
end

function P.popup()
	local list = downloads()
	if #list == 0 and #finished == 0 and dock.app then
		return nil
	end
	local p = #list == 1 and fraction() or nil
	local both = #list > 0 and #finished > 0
	local rows = { gap = 2 }
	if both then
		rows[#rows + 1] = header("In progress", false)
	end
	for _, d in ipairs(list) do
		rows[#rows + 1] = active_row(d, p)
	end
	if both then
		rows[#rows + 1] = header("Recent", true)
	end
	for _, f in ipairs(finished) do
		rows[#rows + 1] = finished_row(f)
	end
	if #list == 0 and #finished == 0 then
		rows[1] = ui.row({ padding = { 4, 6 }, ui.text({ "No downloads", style = "muted" }) })
	end
	local app = dock.app and (dock.app.name or dock.app.id)
	return ui.col({
		padding = 12,
		gap = 8,
		width = 300,
		ui.row({
			padding = { 0, 6 },
			gap = 4,
			align = "center",
			ui.text({ app and (app .. " Downloads") or "Downloads", style = "title", max_lines = 1 }),
			ui.spacer({}),
			#finished > 0 and ui.button({ key = "clear", label = "Clear", variant = "ghost", on_click = clear }) or nil,
			folders[1] and ui.button({
				key = "folder",
				icon = "folder-open-symbolic",
				variant = "ghost",
				tooltip = "Open folder",
				on_click = function()
					open(folders[1])
				end,
			}) or nil,
		}),
		ui.scroll({ key = "list", max_height = 360, ui.col(rows) }),
	})
end

return P
