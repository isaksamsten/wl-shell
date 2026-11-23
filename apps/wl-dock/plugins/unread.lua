-- Add a count badge to an app that publishes Unity LauncherEntry updates.
-- Set widget = "unread" on the app. The optional desktop-entry option
-- overrides the matched desktop ID (with or without the .desktop suffix).

local ui = dock.ui
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
local source_uri, sender, count, visible

local function target_uri()
	local app = dock.app
	local id = desktop_entry or (app and (app.desktop or app.id))
	if not id or id == "" then
		return nil
	end
	return "application://" .. id:gsub("%.desktop$", "") .. ".desktop"
end

local function badge()
	if visible and count and count > 0 and source_uri == target_uri() then
		return count > 99 and "99+" or tostring(count)
	end
end

local function update(args, message)
	local uri, properties = args[1], args[2]
	if type(uri) ~= "string" or uri ~= target_uri() or type(properties) ~= "table" then
		return
	end
	local next_count = type(properties.count) == "integer" and properties.count
	local next_visible = properties["count-visible"]
	if not next_count and type(next_visible) ~= "boolean" then
		return
	end

	local before = badge()
	if source_uri ~= uri or sender ~= message.sender then
		source_uri, sender, count, visible = uri, message.sender, nil, nil
	end
	if next_count then
		count = next_count
	end
	if type(next_visible) == "boolean" then
		visible = next_visible
	end
	if before ~= badge() then
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
				local before = badge()
				source_uri, sender, count, visible = nil, nil, nil, nil
				if before then
					dock.redraw()
				end
			end
		end,
	})
end

function P.icon()
	local unread = badge()
	return ui.stack({
		align = position,
		ui.app_icon({}),
		ui.box({ variant = "badge", ui.text({ unread, style = "caption" }) }),
	})
end

return P
