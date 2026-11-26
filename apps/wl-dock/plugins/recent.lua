-- recent: an app's recent documents, like the dock menus on macOS.
--
-- On an app, hovering its item shows the files the app opened last, newest
-- first, with their thumbnails. Clicking one opens it in that app again.
-- Standalone, it lists the recent files of every app (or of `apps`), and
-- clicking the item opens the list.
--
-- The list comes from recently-used.xbel, where GTK apps (and others using
-- the same file) record each file they open, and which app opened it. The
-- file is watched, so the list follows as apps open files. Files that no
-- longer exist are left out.
--
-- [app."org.gnome.TextEditor"]
-- widget = "recent"
--
-- [widget.recent]
-- name = "Recent Documents"
--
-- Options:
--   count       Files shown (default 10, 1-12)
--   apps        More names the app records files under, besides the ones the dock
--               knows it by (its ID, desktop entry, StartupWMClass and name), e.g.
--               ["soffice", "LibreOffice"]. Standalone: only these apps' files
--   thumbnails  Show thumbnails from the thumbnail cache, else the image itself for
--               PNG, JPEG, WebP and SVG files (default true)
--   open        "app" (default): open a file with the command the app recorded, else
--               xdg-open; "default": always xdg-open; or an argv list in which {path}
--               and {uri} are replaced
--   file        The bookmark file (default $XDG_DATA_HOME/recently-used.xbel)
--
-- Apps record themselves by name and command (`Text Editor`, `gnome-text-editor`),
-- compared with the dock's names for the app ignoring case, spaces and
-- punctuation, and by the last part of reverse-DNS IDs (`org.gnome.Nautilus`
-- matches `nautilus`). Add the ones that don't match to `apps`.

local ui, theme = dock.ui, dock.theme
local P = { api = 1, popup_on = "click" }

-- Options --------------------------------------------------------------------

local count = 10
local MAX_COUNT = 12 -- the dock keeps 16 decoded images
local thumbnails = true
local open_mode = "app" -- 'app', 'default' or 'argv'
local open_argv
local extra_names = {}
local file

-- Strings ----------------------------------------------------------------------

local ENTITIES = { amp = "&", lt = "<", gt = ">", quot = '"', apos = "'" }

local function unescape(s)
	if not s:find("&", 1, true) then
		return s
	end
	return (
		s:gsub("&(#?x?)(%w+);", function(kind, v)
			if kind == "" then
				return ENTITIES[v]
			end
			local n = tonumber(v, kind == "#x" and 16 or 10)
			if n and n < 0x110000 then
				return utf8.char(n)
			end
		end)
	)
end

local function percent_decode(s)
	return (s:gsub("%%(%x%x)", function(h)
		return string.char(tonumber(h, 16))
	end))
end

-- A path's file:// URI, escaped as GLib does (and thumbnailers hash).
local function file_uri(path)
	return "file://"
		.. path:gsub("[^%w%-._~/!$&'()*+,;=:@]", function(c)
			return string.format("%%%02X", c:byte())
		end)
end

-- The local path of a file: URI, else nil.
local function uri_path(uri)
	local rest = uri:match("^file://(.*)$")
	if not rest then
		return nil
	end
	local host, path = rest:match("^([^/]*)(/.*)$")
	if not path or (host ~= "" and host:lower() ~= "localhost") then
		return nil
	end
	path = percent_decode(path:match("^[^?#]*"))
	if path:find("\0", 1, true) then
		return nil
	end
	return path
end

local function home()
	return os.getenv("HOME") or ""
end

local function expand(path)
	if path:sub(1, 2) == "~/" then
		return home() .. path:sub(2)
	end
	return path
end

local function tilde(path)
	local h = home()
	if h ~= "" and (path == h or path:sub(1, #h + 1) == h .. "/") then
		return "~" .. path:sub(#h + 1)
	end
	return path
end

-- MD5 (RFC 1321), for thumbnail names ------------------------------------------

local S = {
	7,
	12,
	17,
	22,
	7,
	12,
	17,
	22,
	7,
	12,
	17,
	22,
	7,
	12,
	17,
	22,
	5,
	9,
	14,
	20,
	5,
	9,
	14,
	20,
	5,
	9,
	14,
	20,
	5,
	9,
	14,
	20,
	4,
	11,
	16,
	23,
	4,
	11,
	16,
	23,
	4,
	11,
	16,
	23,
	4,
	11,
	16,
	23,
	6,
	10,
	15,
	21,
	6,
	10,
	15,
	21,
	6,
	10,
	15,
	21,
	6,
	10,
	15,
	21,
}
local K = {}
for i = 1, 64 do
	K[i] = math.floor(math.abs(math.sin(i)) * 4294967296)
end
local M32 = 0xffffffff

local function md5(msg)
	local len = #msg
	msg = msg .. "\128" .. string.rep("\0", (55 - len) % 64) .. string.pack("<I8", len * 8)
	local h0, h1, h2, h3 = 0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476
	local m = {}
	for off = 1, #msg, 64 do
		for j = 0, 15 do
			m[j] = string.unpack("<I4", msg, off + j * 4)
		end
		local a, b, c, d = h0, h1, h2, h3
		for i = 0, 63 do
			local f, g
			if i < 16 then
				f, g = (b & c) | (~b & d), i
			elseif i < 32 then
				f, g = (d & b) | (~d & c), (5 * i + 1) % 16
			elseif i < 48 then
				f, g = b ~ c ~ d, (3 * i + 5) % 16
			else
				f, g = c ~ (b | ~d), (7 * i) % 16
			end
			f = (f + a + K[i + 1] + m[g]) & M32
			local s = S[i + 1]
			a, d, c = d, c, b
			b = (b + (((f << s) | (f >> (32 - s))) & M32)) & M32
		end
		h0, h1, h2, h3 = (h0 + a) & M32, (h1 + b) & M32, (h2 + c) & M32, (h3 + d) & M32
	end
	return (
		string.pack("<I4I4I4I4", h0, h1, h2, h3):gsub(".", function(c)
			return string.format("%02x", c:byte())
		end)
	)
end

local function thumbnail_dir()
	local cache = os.getenv("XDG_CACHE_HOME")
	if not cache or cache:sub(1, 1) ~= "/" then
		cache = home() .. "/.cache"
	end
	return cache .. "/thumbnails"
end

local THUMB_SIZES = { "normal", "large", "x-large", "xx-large" }

-- Times --------------------------------------------------------------------------

local function days_from_civil(y, m, d)
	y = m <= 2 and y - 1 or y
	local era = (y >= 0 and y or y - 399) // 400
	local yoe = y - era * 400
	local doy = (153 * (m + (m > 2 and -3 or 9)) + 2) // 5 + d - 1
	local doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
	return era * 146097 + doe - 719468
end

-- An ISO 8601 time (as GLib writes them, in UTC) as a Unix time; nil if it
-- isn't one.
local function parse_time(s)
	if not s then
		return nil
	end
	local y, mo, d, h, mi, sec, rest = s:match("^(%d%d%d%d)-(%d%d)-(%d%d)T(%d%d):(%d%d):(%d%d)(.*)$")
	if not y then
		return nil
	end
	local t = days_from_civil(tonumber(y), tonumber(mo), tonumber(d)) * 86400
		+ tonumber(h) * 3600
		+ tonumber(mi) * 60
		+ tonumber(sec)
	local sign, oh, om = rest:match("([+-])(%d%d):?(%d%d)$")
	if sign then
		local off = tonumber(oh) * 3600 + tonumber(om) * 60
		t = sign == "+" and t - off or t + off
	end
	return t
end

local function day_of(t)
	local d = os.date("*t", t)
	return days_from_civil(d.year, d.month, d.day)
end

-- When, for people: "Just now", "5 min ago", "Today, 14:05", "Yesterday",
-- "Tuesday", "3 Mar", "3 Mar 2023".
local function ago(t)
	local now = os.time()
	local s = now - t
	if s < 60 then
		return "Just now"
	end
	if s < 3600 then
		return (s // 60) .. " min ago"
	end
	local days = day_of(now) - day_of(t)
	if days <= 0 then
		return "Today, " .. os.date("%H:%M", t)
	end
	if days == 1 then
		return "Yesterday"
	end
	if days < 7 then
		return os.date("%A", t)
	end
	local format = os.date("%Y", t) == os.date("%Y", now) and "%e %b" or "%e %b %Y"
	return (os.date(format, t):gsub("^%s+", ""))
end

-- Matching apps --------------------------------------------------------------------

-- A name compared: lowercase, letters and digits only.
local function key(s)
	return (s:lower():gsub("%.desktop$", ""):gsub("[^%w]", ""))
end

-- Adds the keys of `name` to `set`: the name, and the last part of a
-- reverse-DNS ID.
local function add_keys(set, name)
	if type(name) ~= "string" or name == "" then
		return
	end
	local k = key(name)
	if k ~= "" then
		set[k] = true
	end
	local id = name:gsub("%.desktop$", "")
	if not id:find("%s") and select(2, id:gsub("%.", "")) >= 2 then
		local last = key(id:match("([^.]+)$"))
		if last ~= "" then
			set[last] = true
		end
	end
end

local wanted -- set of keys; nil: every app

local function compute_wanted()
	if dock.app then
		wanted = {}
		for _, f in ipairs({ "id", "desktop", "wm_class", "name" }) do
			add_keys(wanted, dock.app[f])
		end
	elseif #extra_names > 0 then
		wanted = {}
	else
		wanted = nil
	end
	if wanted then
		for _, n in ipairs(extra_names) do
			add_keys(wanted, n)
		end
	end
end

-- Whether a recorded app (name, exec) is the one wanted; cached, as the same
-- few apps recur throughout the file.
local matched = {}

local function is_wanted(name, exec)
	if not wanted then
		return true
	end
	local id = name .. "\0" .. exec
	local hit = matched[id]
	if hit == nil then
		local keys = {}
		add_keys(keys, name)
		local cmd = exec:match("^['\"%s]*([^%s'\"]+)")
		if cmd then
			add_keys(keys, cmd:match("([^/]+)$"))
		end
		hit = false
		for k in pairs(keys) do
			if wanted[k] then
				hit = true
				break
			end
		end
		matched[id] = hit
	end
	return hit
end

-- Reading the bookmarks -------------------------------------------------------------

-- Every file bookmark, newest first: { uri, path, mime, time, exec, app }.
-- `time` and `exec` are those of the wanted app that used it last.
local entries = {}
local read_error

local function parse(text)
	local out = {}
	for attrs, body in text:gmatch("<bookmark%s([^>]*)>(.-)</bookmark>") do
		local href = attrs:match('href="([^"]*)"')
		local path = href and uri_path(unescape(href))
		if path then
			local best
			for a in body:gmatch("<bookmark:application%s([^>]*)>") do
				local name = unescape(a:match('name="([^"]*)"') or "")
				local exec = unescape(a:match('exec="([^"]*)"') or "")
				if is_wanted(name, exec) then
					local t = parse_time(a:match('modified="([^"]*)"'))
						or tonumber(a:match('timestamp="(%d+)"'))
						or parse_time(attrs:match('modified="([^"]*)"'))
						or 0
					if not best or t > best.time then
						best = { time = t, exec = exec, app = name }
					end
				end
			end
			if best then
				best.uri = unescape(href)
				best.path = path
				best.mime = body:match('<mime:mime%-type%s+type="([^"]*)"')
				out[#out + 1] = best
			end
		end
	end
	table.sort(out, function(x, y)
		return x.time > y.time
	end)
	return out
end

-- What is shown ---------------------------------------------------------------------------

-- The entries shown: those that exist, at most `count`, each with `thumb`
-- (an image to show) or nil.
local shown = {}
local signature = ""
local checking, check_again = nil, false

local function update(list)
	local parts = {}
	for _, e in ipairs(list) do
		parts[#parts + 1] = e.uri .. "|" .. (e.thumb or "") .. "|" .. e.time
	end
	local sig = table.concat(parts, "\n")
	shown = list
	if sig ~= signature then
		signature = sig
		dock.redraw()
	end
end

local thumb_names = {} -- path -> the thumbnail's file name

local function thumb_name(path)
	local n = thumb_names[path]
	if not n then
		n = md5(file_uri(path)) .. ".png"
		thumb_names[path] = n
	end
	return n
end

local IMAGE_TYPES = { ["image/png"] = true, ["image/jpeg"] = true, ["image/webp"] = true, ["image/svg+xml"] = true }

-- Asks `stat` which files exist and how new their thumbnails are, then shows
-- the first `count` that exist.
local function check()
	if checking then
		check_again = true
		return
	end
	check_again = false
	local candidates = {}
	for i = 1, math.min(#entries, math.max(count * 3, 30), 100) do
		candidates[i] = entries[i]
	end
	if #candidates == 0 then
		return update({})
	end
	local argv = { "sh", "-c", 'stat -L -c "%Y %n" -- "$@" 2>/dev/null', "sh" }
	local dir = thumbnail_dir()
	for _, e in ipairs(candidates) do
		argv[#argv + 1] = e.path
		if thumbnails then
			for _, size in ipairs(THUMB_SIZES) do
				argv[#argv + 1] = dir .. "/" .. size .. "/" .. thumb_name(e.path)
			end
		end
	end
	local mtimes = {}
	local ok, handle = pcall(dock.spawn, argv, {
		on_line = function(line)
			local t, name = line:match("^(%d+) (.*)$")
			if t then
				mtimes[name] = tonumber(t)
			end
		end,
		on_exit = function()
			checking = nil
			local list = {}
			for _, e in ipairs(candidates) do
				local mtime = mtimes[e.path]
				if mtime then
					e.thumb = nil
					if thumbnails then
						for _, size in ipairs(THUMB_SIZES) do
							local t = dir .. "/" .. size .. "/" .. thumb_name(e.path)
							if mtimes[t] and mtimes[t] >= mtime then
								e.thumb = t
								break
							end
						end
						if not e.thumb and IMAGE_TYPES[e.mime] then
							e.thumb = e.path
						end
					end
					list[#list + 1] = e
					if #list >= count then
						break
					end
				end
			end
			update(list)
			if check_again then
				check()
			end
		end,
	})
	if ok then
		checking = handle
	else
		-- No way to check: show them as they are.
		print("stat: " .. tostring(handle))
		local list = {}
		for i = 1, math.min(count, #candidates) do
			list[i] = candidates[i]
		end
		update(list)
	end
end

local function reload()
	local text, err = dock.read(file)
	if not text then
		-- No file yet: nothing was opened.
		if read_error ~= err then
			read_error = err
			print(file .. ": " .. tostring(err))
		end
		entries = {}
	else
		read_error = nil
		entries = parse(text)
	end
	check()
end

-- Opening -----------------------------------------------------------------------------

-- `s` split as the shell would (quotes and backslashes; no expansions).
local function shell_words(s)
	local out, cur, any = {}, {}, false
	local i, n = 1, #s
	while i <= n do
		local ch = s:sub(i, i)
		if ch == "'" then
			local j = s:find("'", i + 1, true) or n + 1
			cur[#cur + 1] = s:sub(i + 1, j - 1)
			any, i = true, j + 1
		elseif ch == '"' then
			local j = i + 1
			while j <= n and s:sub(j, j) ~= '"' do
				if s:sub(j, j) == "\\" and j < n then
					cur[#cur + 1] = s:sub(j + 1, j + 1)
					j = j + 2
				else
					cur[#cur + 1] = s:sub(j, j)
					j = j + 1
				end
			end
			any, i = true, j + 1
		elseif ch == "\\" and i < n then
			cur[#cur + 1] = s:sub(i + 1, i + 1)
			any, i = true, i + 2
		elseif ch:find("^%s") then
			if any then
				out[#out + 1] = table.concat(cur)
			end
			cur, any, i = {}, false, i + 1
		else
			cur[#cur + 1] = ch
			any, i = true, i + 1
		end
	end
	if any then
		out[#out + 1] = table.concat(cur)
	end
	return out
end

-- The recorded command line for `e`, with its field codes filled in; nil if
-- there is none.
local function app_argv(e)
	local words = shell_words(e.exec)
	-- GLib stores the whole command line quoted: "'gedit %u'".
	if #words == 1 and words[1]:find("%s") then
		words = shell_words(words[1])
	end
	if #words == 0 then
		return nil
	end
	local argv, filled = {}, false
	for _, w in ipairs(words) do
		local v = w:gsub("%%(.)", function(c)
			if c == "u" or c == "U" then
				filled = true
				return e.uri
			end
			if c == "f" or c == "F" then
				filled = true
				return e.path
			end
			if c == "%" then
				return "%"
			end
			return ""
		end)
		if v ~= "" then
			argv[#argv + 1] = v
		end
	end
	if #argv == 0 then
		return nil
	end
	if not filled then
		argv[#argv + 1] = e.path
	end
	return argv
end

local function open(e)
	local argv
	if open_mode == "argv" then
		argv = {}
		for i, a in ipairs(open_argv) do
			argv[i] = a:gsub("{(%w+)}", function(k)
				if k == "path" then
					return e.path
				end
				if k == "uri" then
					return e.uri
				end
			end)
		end
	elseif open_mode == "app" then
		argv = app_argv(e)
	end
	argv = argv or { "xdg-open", e.path }
	local ok, err = dock.run(argv)
	if not ok then
		print(argv[1] .. ": " .. tostring(err))
	end
end

-- Popup ----------------------------------------------------------------------------------

-- An icon for a MIME type, from the names every icon theme has.
local function type_icon(mime)
	mime = (mime or ""):lower()
	if mime == "inode/directory" then
		return "folder"
	end
	local media, sub = mime:match("^([^/]+)/(.+)$")
	if not media then
		return "text-x-generic"
	end
	if media == "image" or media == "audio" or media == "video" or media == "font" then
		return media .. "-x-generic"
	end
	if sub == "calendar" then
		return "x-office-calendar"
	end
	if sub == "vcard" or sub == "x-vcard" then
		return "x-office-address-book"
	end
	if sub:find("spreadsheet") or sub:find("excel") or sub == "csv" then
		return "x-office-spreadsheet"
	end
	if sub:find("presentation") or sub:find("powerpoint") then
		return "x-office-presentation"
	end
	if
		sub == "pdf"
		or sub:find("document")
		or sub:find("word")
		or sub == "msword"
		or sub == "rtf"
		or sub:find("epub")
	then
		return "x-office-document"
	end
	if
		sub:find("zip")
		or sub:find("tar")
		or sub:find("rar")
		or sub:find("7z")
		or sub:find("xz")
		or sub:find("bzip")
		or sub:find("zstd")
		or sub:find("archive")
		or sub:find("compressed")
	then
		return "package-x-generic"
	end
	if sub == "html" or sub == "xhtml+xml" then
		return "text-html"
	end
	if sub:find("script") or sub:find("python") or sub:find("perl") or sub:find("ruby") then
		return "text-x-script"
	end
	if sub:find("executable") then
		return "application-x-executable"
	end
	return "text-x-generic"
end

local THUMB = 36

local function row(e, i)
	local name = e.path:gsub("/+$", ""):match("([^/]*)$")
	if name == "" then
		name = e.path
	end
	local folder = tilde(e.path:gsub("/+$", ""):match("^(.*)/[^/]*$") or "/")
	if folder == "" then
		folder = "/"
	end
	local preview
	if e.thumb then
		preview = ui.image({ src = e.thumb, fit = "contain", radius = 3, width = THUMB, height = THUMB })
	else
		preview = ui.icon({ type_icon(e.mime), size = 32 })
	end
	return ui.row({
		key = "file" .. i,
		gap = 10,
		padding = { 4, 6 },
		radius = 6,
		align = "left",
		hover = { background = theme.hover },
		on_click = function()
			open(e)
		end,
		ui.stack({ width = THUMB, height = THUMB, preview }),
		ui.col({
			grow = 1,
			gap = 1,
			ui.text({ name, max_lines = 1 }),
			ui.text({
				e.time > 0 and (folder .. " · " .. ago(e.time)) or folder,
				style = "caption",
				color = theme.muted,
				max_lines = 1,
			}),
		}),
	})
end

function P.popup()
	if #shown == 0 then
		-- On an app, the label shows instead.
		if dock.app then
			return nil
		end
		return ui.col({ padding = 16, ui.text({ "No recent documents", style = "muted" }) })
	end
	local items = { gap = 2 }
	for i, e in ipairs(shown) do
		items[#items + 1] = row(e, i)
	end
	return ui.col({
		padding = 12,
		gap = 8,
		width = 300,
		ui.row({
			padding = { 0, 6 },
			ui.text({ dock.app and dock.app.name or "Recent Documents", style = "title", max_lines = 1 }),
		}),
		ui.scroll({ key = "list", max_height = 420, ui.col(items) }),
	})
end

function P.on_open()
	-- Files may have gone, or gained thumbnails, since.
	check()
end

-- Standalone: a stack of pages with a clock.
function P.icon()
	if dock.app then
		return nil
	end
	return ui.canvas({
		draw = function(c)
			c:save()
			c:translate(50, 50)
			c:rotate(-0.16)
			c:rect({ x = -30, y = -38, w = 56, h = 72, radius = 6, fill = "#d8d8dc" })
			c:restore()
			c:rect({ x = 24, y = 12, w = 56, h = 72, radius = 6, fill = "#ffffff", stroke = "#c7c7cc", width = 1 })
			for i = 0, 4 do
				c:line({
					x1 = 33,
					y1 = 26 + i * 9,
					x2 = i == 4 and 56 or 71,
					y2 = 26 + i * 9,
					stroke = "#c7c7cc",
					width = 3,
					cap = "round",
				})
			end
			c:circle({ x = 70, y = 72, r = 18, fill = theme.accent })
			c:line({ x1 = 70, y1 = 72, x2 = 70, y2 = 62, stroke = "#ffffff", width = 3.5, cap = "round" })
			c:line({ x1 = 70, y1 = 72, x2 = 77, y2 = 76, stroke = "#ffffff", width = 3.5, cap = "round" })
		end,
	})
end

function P.on_app()
	compute_wanted()
	matched = {}
	reload()
end

-- Setup ----------------------------------------------------------------------------------

function P.setup(opts)
	if type(opts.count) == "number" then
		count = math.max(1, math.min(MAX_COUNT, math.floor(opts.count)))
	end
	if type(opts.thumbnails) == "boolean" then
		thumbnails = opts.thumbnails
	end
	if type(opts.apps) == "string" then
		opts.apps = { opts.apps }
	end
	if type(opts.apps) == "table" then
		for _, n in ipairs(opts.apps) do
			if type(n) == "string" and n ~= "" then
				extra_names[#extra_names + 1] = n
			end
		end
	end
	if opts.open == "default" or opts.open == "app" then
		open_mode = opts.open
	elseif type(opts.open) == "table" and #opts.open > 0 then
		open_mode, open_argv = "argv", {}
		for _, a in ipairs(opts.open) do
			open_argv[#open_argv + 1] = tostring(a)
		end
	elseif opts.open ~= nil then
		print('open: expected "app", "default" or a list; using "app"')
	end
	if type(opts.file) == "string" and opts.file ~= "" then
		file = expand(opts.file)
	else
		local data = os.getenv("XDG_DATA_HOME")
		if not data or data:sub(1, 1) ~= "/" then
			data = home() .. "/.local/share"
		end
		file = data .. "/recently-used.xbel"
	end

	compute_wanted()
	local w, err = dock.watch(file, reload)
	if not w then
		print(file .. ": not watched, polling: " .. tostring(err))
		dock.every(30 * 1000, reload)
	end
	reload()
end

return P
