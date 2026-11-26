-- Drawing animated canvases: a bundled module (`require 'anim'`).
--
--   local anim = require 'anim'
--   ui.canvas({ key = "busy", frames = 24, loop = 1.2, draw = anim.spinner({}) })
--   ui.canvas({ key = "done", frames = 60, value = p, transition = 300, draw = anim.ring({}) })
--   ui.canvas({ key = "rec", frames = 16, loop = 0.8, mirror = true, draw = anim.pulse({}) })
--   ui.canvas({ key = "wait", frames = 24, loop = 1.6, mirror = true, draw = anim.bar({}) })
--
-- Each returns a `draw(c, t)` for a canvas with `frames` (PLUGINS.md,
-- Animation): the dock samples it at `frames` values of `t` and plays them,
-- so it costs nothing after that. A still canvas (no `frames`) calls it with
-- `t` nil, drawn as 0. The draw functions can be called from a plugin's own,
-- to draw more around them.
--
-- Options, all optional, in viewbox units (the viewbox is 100 × 100 unless
-- the canvas says otherwise):
--
--   spinner  An arc turning once per loop: `x`, `y`, `r` (50, 50, 40),
--            `width` (10), `color` (theme.accent), `track` (theme.track; false:
--            none), `fill` (a disc under it; none), `sweep` (the arc's length,
--            in turns: 0.25).
--   ring     An arc from `from` (in turns from 12 o'clock: 0) clockwise to `t`,
--            the whole ring at 1: the spinner's options but `sweep`.
--   pulse    A disc breathing between `from` × `r` (0.4) and `r` as `t` goes
--            from 0 to 1 (mirror the loop to breathe in and out): `x`, `y`,
--            `r`, `color`, `fill` (a disc under it).
--   bar      A pill along `x`, `y`, `w`, `h` (10, 45, 80, 10): its track, and
--            a segment a third as long sweeping along it (`segment`, 1/3); with
--            `fill = true`, filled to `t` instead. `color`, `track`.

local M = {}

local TAU = 2 * math.pi
local TOP = -math.pi / 2

local function clamp(t)
	return math.max(0, math.min(t or 0, 1))
end

-- The round shapes' options, with their defaults.
local function round(o)
	local theme = dock.theme
	local track = o.track
	if track == nil then
		track = theme.track
	end
	return {
		x = o.x or 50,
		y = o.y or 50,
		r = o.r or 40,
		width = o.width or 10,
		color = o.color or theme.accent,
		track = track,
		fill = o.fill,
	}
end

-- The disc and track under a round shape.
local function under(c, s)
	if s.fill then
		c:circle({ x = s.x, y = s.y, r = s.r + s.width / 2, fill = s.fill })
	end
	if s.track then
		c:circle({ x = s.x, y = s.y, r = s.r, stroke = s.track, width = s.width })
	end
end

function M.spinner(o)
	o = o or {}
	local s = round(o)
	local sweep = (o.sweep or 0.25) * TAU
	return function(c, t)
		under(c, s)
		local from = TOP + (t or 0) * TAU
		c:arc({
			x = s.x,
			y = s.y,
			r = s.r,
			from = from,
			to = from + sweep,
			stroke = s.color,
			width = s.width,
			cap = "round",
		})
	end
end

function M.ring(o)
	o = o or {}
	local s = round(o)
	local from = TOP + (o.from or 0) * TAU
	return function(c, t)
		t = clamp(t)
		under(c, s)
		if t >= 1 then
			c:circle({ x = s.x, y = s.y, r = s.r, stroke = s.color, width = s.width })
		elseif t > 0 then
			c:arc({
				x = s.x,
				y = s.y,
				r = s.r,
				from = from,
				to = from + t * TAU,
				stroke = s.color,
				width = s.width,
				cap = "round",
			})
		end
	end
end

function M.pulse(o)
	o = o or {}
	local s = round(o)
	local from = o.from or 0.4
	return function(c, t)
		if s.fill then
			c:circle({ x = s.x, y = s.y, r = s.r, fill = s.fill })
		end
		local r = s.r * (from + (1 - from) * clamp(t))
		c:circle({ x = s.x, y = s.y, r = r, fill = s.color })
	end
end

function M.bar(o)
	o = o or {}
	local theme = dock.theme
	local x, y, w, h = o.x or 10, o.y or 45, o.w or 80, o.h or 10
	local color = o.color or theme.accent
	local track = o.track
	if track == nil then
		track = theme.track
	end
	local segment = math.max(h, w * (o.segment or 1 / 3))
	return function(c, t)
		t = clamp(t)
		if track then
			c:rect({ x = x, y = y, w = w, h = h, radius = h / 2, fill = track })
		end
		if o.fill then
			if t > 0 then
				c:rect({ x = x, y = y, w = math.max(h, w * t), h = h, radius = h / 2, fill = color })
			end
		else
			c:rect({ x = x + (w - segment) * t, y = y, w = segment, h = h, radius = h / 2, fill = color })
		end
	end
end

return M
