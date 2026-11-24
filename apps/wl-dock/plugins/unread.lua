-- Show what an app publishes over Unity LauncherEntry updates: a count
-- badge, a progress arc around the icon, and a "!" badge while it is
-- urgent. Set widget = "unread" on the app. The optional desktop-entry
-- option overrides the matched desktop ID (with or without the .desktop
-- suffix).

local ui, theme = dock.ui, dock.theme
local P = {}
local desktop_entry
local position = "top-right"
local positions = {
	["top-left"] = true,
	top = true,
	["top-right"] = true,
	left = true,
	center = true,
	right = true,
	["bottom-left"] = true,
	bottom = true,
	["bottom-right"] = true,
}
-- The latest update's sender and URI, and what it has published so far.
local source_uri, sender
local count, count_visible, progress, progress_visible, urgent

local function reset(uri, from)
	source_uri, sender = uri, from
	count, count_visible, progress, progress_visible, urgent = nil, nil, nil, nil, nil
end

local function target_uri()
	local app = dock.app
	local id = desktop_entry or (app and (app.desktop or app.id))
	if not id or id == "" then
		return nil
	end
	return "application://" .. id:gsub("%.desktop$", "") .. ".desktop"
end

local function badge()
	if source_uri ~= target_uri() then
		return nil
	end
	if count_visible and count and count > 0 then
		return count > 99 and "99+" or tostring(count)
	end
	if urgent then
		return "!"
	end
	return nil
end

-- The progress shown, 0–1 in whole percents (so small steps don't redraw).
local function shown_progress()
	if progress_visible and progress and source_uri == target_uri() then
		return math.floor(math.max(0, math.min(progress, 1)) * 100 + 0.5) / 100
	end
	return nil
end

-- What the icon shows, to tell whether an update changes it.
local function shown()
	return tostring(badge()) .. "|" .. tostring(shown_progress())
end

local function integer(v)
	return math.type(v) == "integer" and v or nil
end

local function number(v)
	return type(v) == "number" and v == v and v or nil
end

local function boolean(v)
	if type(v) == "boolean" then
		return v
	end
end

local function update(args, message)
	local uri, properties = args[1], args[2]
	if type(uri) ~= "string" or uri ~= target_uri() or type(properties) ~= "table" then
		return
	end
	local got = {
		count = integer(properties.count),
		count_visible = boolean(properties["count-visible"]),
		progress = number(properties.progress),
		progress_visible = boolean(properties["progress-visible"]),
		urgent = boolean(properties.urgent),
	}
	if next(got) == nil then
		return
	end

	local before = shown()
	if source_uri ~= uri or sender ~= message.sender then
		reset(uri, message.sender)
	end
	-- Each update carries only what changed.
	if got.count ~= nil then
		count = got.count
	end
	if got.count_visible ~= nil then
		count_visible = got.count_visible
	end
	if got.progress ~= nil then
		progress = got.progress
	end
	if got.progress_visible ~= nil then
		progress_visible = got.progress_visible
	end
	if got.urgent ~= nil then
		urgent = got.urgent
	end
	if before ~= shown() then
		dock.redraw()
	end
end

function P.setup(opts)
	if not dock.app then
		return
	end
	if type(opts["desktop-entry"]) == "string" and opts["desktop-entry"] ~= "" then
		desktop_entry = opts["desktop-entry"]
	end
	position = positions[opts.position] and opts.position or "top-right"

	local bus = dock.dbus.session()
	bus:watch({
		iface = "com.canonical.Unity.LauncherEntry",
		member = "Update",
		on_signal = update,
	})
	bus:watch({
		sender = "org.freedesktop.DBus",
		path = "/org/freedesktop/DBus",
		iface = "org.freedesktop.DBus",
		member = "NameOwnerChanged",
		on_signal = function(args)
			if args[1] == sender and args[3] == "" then
				local before = shown()
				reset(nil, nil)
				if before ~= shown() then
					dock.redraw()
				end
			end
		end,
	})
end

-- A thin ring along the tile's edge, filled clockwise from 12 o'clock.
local function arc(p)
	return ui.canvas({
		draw = function(c)
			c:arc({ x = 50, y = 50, r = 47, from = 0, to = 2 * math.pi, stroke = theme.track, width = 4 })
			if p > 0 then
				local top = -math.pi / 2
				c:arc({
					x = 50,
					y = 50,
					r = 47,
					from = top,
					to = top + p * 2 * math.pi,
					stroke = theme.accent,
					width = 4,
					cap = "round",
				})
			end
		end,
	})
end

function P.icon()
	local text, p = badge(), shown_progress()
	if not text and not p then
		return ui.app_icon()
	end
	local layers = { align = position, ui.app_icon({}) }
	if p then
		layers[#layers + 1] = arc(p)
	end
	if text then
		layers[#layers + 1] = ui.box({ variant = "badge", ui.text({ text, style = "caption" }) })
	end
	return ui.stack(layers)
end

return P
