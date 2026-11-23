-- The clock: an analog clock face and the time as its label, with the
-- date in its popup.
--
-- Standalone, pinned as `@clock` (`wl-dock pin @clock`):
--
--   [widget.clock]
--   name = "Clock"
--
-- Or behind an app, as its label (and with `face = true`, its icon):
--
--   [app.<id>]
--   widget = { plugin = "clock", format = "%H:%M:%S", face = true }
--
-- `format` is an `os.date` format for the label (default "%H:%M");
-- `date` the popup's (default "%A %e %B"). `face` draws the clock as the
-- icon: the default standalone, but not on an app, whose icon it would
-- replace. `popup = false` leaves out the popup (it is off on an app
-- unless `popup = true`).

local ui, theme = dock.ui, dock.theme
local C = {}
local format, date, face, popup = "%H:%M", "%A %e %B", dock.app == nil, dock.app == nil

function C.setup(opts)
	if type(opts.format) == "string" then
		format = opts.format
	end
	if type(opts.date) == "string" then
		date = opts.date
	end
	if type(opts.face) == "boolean" then
		face = opts.face
	end
	if type(opts.popup) == "boolean" then
		popup = opts.popup
	end
	-- The dock only redraws what changed.
	dock.every(1000, dock.redraw)
end

function C.label()
	return os.date(format)
end

local function hand(c, angle, len, width, color)
	c:save()
	c:translate(50, 50)
	c:rotate(angle)
	c:line({ x1 = 0, y1 = 8, x2 = 0, y2 = -len, stroke = color, width = width, cap = "round" })
	c:restore()
end

local function draw(c)
	local t = os.date("*t")
	c:circle({ x = 50, y = 50, r = 48, fill = theme.surface })
	for i = 0, 11 do
		local major = i % 3 == 0
		c:save()
		c:translate(50, 50)
		c:rotate(i * math.pi / 6)
		c:line({
			x1 = 0,
			y1 = -42,
			x2 = 0,
			y2 = major and -35 or -39,
			stroke = theme.muted,
			width = major and 2.5 or 1.5,
		})
		c:restore()
	end
	local m = t.min + t.sec / 60
	local h = t.hour % 12 + m / 60
	hand(c, h * math.pi / 6, 24, 4, theme.text)
	hand(c, m * math.pi / 30, 36, 3, theme.text)
	hand(c, t.sec * math.pi / 30, 40, 1.5, theme.accent)
	c:circle({ x = 50, y = 50, r = 3, fill = theme.accent })
end

function C.icon(tile)
	if not face then
		return nil
	end
	return ui.canvas({ viewbox = { 100, 100 }, draw = draw })
end

function C.popup()
	if not popup then
		return nil
	end
	return ui.col({
		padding = 16,
		gap = 8,
		align = "center",
		ui.canvas({ viewbox = { 100, 100 }, width = 160, height = 160, draw = draw }),
		ui.text({ os.date(format), style = "title" }),
		ui.text({ (os.date(date):gsub("%s+", " ")), style = "muted" }),
	})
end

return C

