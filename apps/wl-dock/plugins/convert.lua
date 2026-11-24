-- Convert image: a launcher action on image files. Tab on an image in the
-- launcher, pick Convert image…, then the format; the copy is written
-- next to the original (`photo.webp`, or `photo-2.webp` if that is taken)
-- with ImageMagick.
--
-- Load it with a plugin table:
--
--   [plugin.convert]
--   formats = ["png", "jpg", "webp", "avif"]   # the choices, in order
--   command = ["magick"]                        # or ["convert"] for ImageMagick 6
--   notify = true                               # notify-send when done or failed
--
-- `command` is run as `command... <input> <output>`, so any converter that
-- takes those two arguments works (`["vips", "copy"]`, say). The image's
-- own format isn't offered.

local formats = { "png", "jpg", "webp", "avif", "gif" }
local command = { "magick" }
local notify = true

-- The formats an image already is, by its type.
local own = {
	["image/png"] = "png",
	["image/jpeg"] = "jpg",
	["image/webp"] = "webp",
	["image/avif"] = "avif",
	["image/gif"] = "gif",
	["image/tiff"] = "tiff",
	["image/bmp"] = "bmp",
	["image/heic"] = "heic",
	["image/jxl"] = "jxl",
}
local same = { jpeg = "jpg", tif = "tiff" }

-- Picks a free name, converts, and says how it went. Arguments: notify,
-- input, output without its extension, extension, then the command.
local script = [[
notify=$1 in=$2 base=$3 ext=$4
shift 4
out="$base.$ext" n=2
while [ -e "$out" ]; do out="$base-$n.$ext" n=$((n + 1)); done
say() { [ "$notify" = 1 ] && command -v notify-send >/dev/null && notify-send -a wl-dock "$@"; }
if err=$("$@" "$in" "$out" 2>&1); then
	say -i "$out" "Converted ${in##*/}" "${out##*/}"
else
	echo "convert ${in##*/}: $err" >&2
	say -u critical "Couldn't convert ${in##*/}" "$err"
fi
exit 0
]]

local function strings(v)
	if type(v) == "string" then
		return { v }
	end
	if type(v) ~= "table" or #v == 0 then
		return nil
	end
	local out = {}
	for _, s in ipairs(v) do
		if type(s) ~= "string" then
			return nil
		end
		out[#out + 1] = s
	end
	return out
end

local function setup(opts)
	local f = strings(opts.formats)
	if f then
		formats = {}
		for _, ext in ipairs(f) do
			ext = ext:lower():gsub("^%.", "")
			if ext:match("^[%w]+$") then
				formats[#formats + 1] = same[ext] or ext
			end
		end
	end
	command = strings(opts.command) or command
	if type(opts.notify) == "boolean" then
		notify = opts.notify
	end
end

local function choices(s)
	local is = own[s.mime[1] or ""]
	local out = {}
	for _, ext in ipairs(formats) do
		if ext ~= is then
			out[#out + 1] = { id = ext, name = ext:upper() }
		end
	end
	return out
end

local function run(s, choice)
	local stem = s.name:match("^(.+)%.[^.]+$") or s.name
	local base = s.path:sub(1, #s.path - #s.name) .. stem
	local argv = { "sh", "-c", script, "convert", notify and "1" or "0", s.path, base, choice.id }
	for _, a in ipairs(command) do
		argv[#argv + 1] = a
	end
	dock.run(argv)
end

return {
	api = 1,
	setup = setup,
	actions = {
		{
			id = "convert",
			name = "Convert image…",
			icon = "image-x-generic",
			when = { kind = "file", dir = false, mime = "image/*" },
			choices = choices,
			run = run,
		},
	},
}
