-- The player: what an MPRIS media player plays, with its cover as the
-- icon and a popup that plays, pauses, skips and seeks.
--
-- On an app, for the app's own player:
--
--   [app.spotify]
--   widget = "player"
--
--   [app.firefox]
--   widget = { plugin = "player", player = "firefox" }   # by bus name
--
-- Standalone (`wl-dock pin @nowplaying`), following whichever player
-- plays (else the one that changed last):
--
--   [widget.nowplaying]
--   plugin = "player"
--   name = "Now Playing"
--
-- Options: `player`, a bus name suffix (`org.mpris.MediaPlayer2.<suffix>`,
-- `*` at the end matching any rest) instead of matching the app by its
-- names; `cover = false` keeps the icon as it is; `label = false` keeps
-- the item's name instead of the title; `span = 1.5` or `2` widens the
-- tile on a bottom dock while there is a player, to show the title (and
-- at 2, the artist) next to the cover.

local ui, theme = dock.ui, dock.theme
local mpris = require("mpris")

local P = {}
local p, watcher, ticking
local cover, show_label = true, true
local span = 1
local standalone = dock.app == nil

-- Wide only with something to show beside the cover, and only along a
-- bottom dock.
local function respan()
	dock.span({ horizontal = p and span or 1, vertical = 1 })
end

local function clock(s)
	s = math.max(0, math.floor(s or 0))
	local h, m = s // 3600, s // 60 % 60
	if h > 0 then
		return string.format("%d:%02d:%02d", h, m, s % 60)
	end
	return string.format("%d:%02d", m, s % 60)
end

function P.setup(opts)
	if opts.cover == false then
		cover = false
	end
	if opts.label == false then
		show_label = false
	end
	if opts.span == 1.5 or opts.span == 2 then
		span = opts.span
	end
	watcher = mpris.watch({ app = dock.app, player = opts.player }, function(player)
		p = player
		respan()
		dock.redraw()
	end)
end

-- The dock knows the app by more names now: look again.
function P.on_app()
	watcher:refresh()
end

-- The cover over what shows until it arrives, in a stack with `props`.
-- The stack must be sized itself: a stack in a stack has no size.
local function cover_stack(tile, props)
	local under = standalone and ui.icon({ "audio-x-generic" }) or ui.app_icon({})
	props[1] = under
	props[2] = ui.image({ src = p.art, fit = "cover", radius = math.min(tile.h, tile.w / tile.span) * 0.2 })
	return ui.stack(props)
end

-- The square icon: the cover, else the item's own.
local function square(tile)
	if not cover or not (p and p.art) then
		if standalone then
			return ui.icon({ "audio-x-generic" })
		end
		return nil
	end
	return cover_stack(tile, {})
end

-- A wide tile: the square at the start, the title (and with room, the
-- artist) beside it.
local function wide(tile)
	local h = tile.h
	local roomy = tile.span >= 2
	local name = (p.title and p.title ~= "") and p.title or p.identity or p.id
	local artist = roomy and p.artist and p.artist ~= "" and p.artist or nil
	-- As wide as the tile is tall, whatever dock it is laid out for.
	local side = string.format("%.3f%%", 100 / tile.span)
	local sq
	if cover and p.art then
		sq = cover_stack(tile, { width = side, min_width = side, height = "100%" })
	else
		-- The item's own icon (standalone, the placeholder).
		sq = ui.stack({
			width = side,
			min_width = side,
			height = "100%",
			standalone and ui.icon({ "audio-x-generic", size = h * 0.9 }) or ui.app_icon({}),
		})
	end
	return ui.row({
		width = "100%",
		height = "100%",
		gap = h * (roomy and 0.08 or 0.06),
		sq,
		ui.col({
			grow = 1,
			min_width = 0,
			gap = h * 0.02,
			padding = { right = h * (roomy and 0.04 or 0.02) },
			ui.text({
				name,
				size = h * (roomy and 0.17 or 0.13),
				weight = "bold",
				color = theme.text,
				max_lines = (roomy and artist) and 2 or 3,
			}),
			artist and ui.text({ artist, size = h * 0.14, color = theme.muted, max_lines = 1 }) or nil,
		}),
	})
end

function P.icon(tile)
	if p and tile.span > 1 then
		return wide(tile)
	end
	return square(tile)
end

function P.label()
	if not (show_label and p and p.title and p.title ~= "") then
		return nil
	end
	if p.artist and p.artist ~= "" then
		return p.title .. " – " .. p.artist
	end
	return p.title
end

-- The time labels move once a second while the popup is open.
function P.on_open()
	if watcher then
		watcher:sync()
	end
	if not ticking then
		ticking = dock.every(1000, function()
			if p and p.playing then
				dock.redraw()
			end
		end)
	end
end

function P.on_close()
	if ticking then
		ticking:cancel()
		ticking = nil
	end
end

local function controls()
	return ui.row({
		gap = 16,
		justify = "center",
		align = "center",
		ui.button({
			key = "previous",
			icon = "media-skip-backward-symbolic",
			variant = "ghost",
			enabled = p.can_previous ~= false,
			on_click = function()
				p:previous()
			end,
		}),
		ui.button({
			key = "play",
			icon = p.playing and "media-playback-pause-symbolic" or "media-playback-start-symbolic",
			variant = "filled",
			enabled = (p.playing and p.can_pause ~= false) or (not p.playing and p.can_play ~= false),
			on_click = function()
				p:play_pause()
			end,
		}),
		ui.button({
			key = "next",
			icon = "media-skip-forward-symbolic",
			variant = "ghost",
			enabled = p.can_next ~= false,
			on_click = function()
				p:next()
			end,
		}),
	})
end

local function seekbar()
	local length = p.length or 0
	local live = p.playing and { rate = p.rate or 1, at = p.at } or nil
	return ui.col({
		gap = 2,
		ui.slider({
			key = "seek",
			value = p.position or 0,
			max = math.max(length, 1),
			enabled = p.can_seek ~= false and length > 0,
			live = live,
			on_commit = function(v)
				p:set_position(v)
			end,
		}),
		ui.row({
			ui.text({ clock(p:now()), style = "caption" }),
			ui.spacer({}),
			ui.text({ length > 0 and clock(length) or "", style = "caption" }),
		}),
	})
end

local function title()
	local text = ui.text({ p.title or p.identity or p.id, style = "title", max_lines = 1, align = "center" })
	-- Standalone, the title raises the player's app.
	if standalone and p.can_raise then
		return ui.button({
			key = "raise",
			variant = "ghost",
			tooltip = "Show " .. (p.identity or p.id),
			on_click = function()
				p:raise()
			end,
			text,
		})
	end
	return text
end

function P.popup()
	if not p then
		if standalone then
			return ui.col({ padding = 16, ui.text({ "Nothing is playing", style = "muted" }) })
		end
		return nil
	end
	local body = ui.col({
		padding = 20,
		gap = 12,
		align = "center",
		p.art and ui.image({ src = p.art, width = 220, height = 220, radius = 12, fit = "cover" }) or ui.box({
			variant = "inset",
			width = 220,
			height = 220,
			radius = 12,
			align = "center",
			justify = "center",
			ui.icon({ "audio-x-generic", size = 96, color = theme.muted }),
		}),
		ui.col({
			gap = 2,
			align = "center",
			width = 260,
			title(),
			ui.text({ p.artist or "", style = "muted", max_lines = 1, align = "center" }),
		}),
		ui.col({ width = 260, seekbar() }),
		controls(),
	})
	if not p.art then
		return ui.box({ width = 300, body })
	end
	return ui.stack({
		width = 300,
		ui.image({ src = p.art, fit = "cover", blur = 40, opacity = 0.45 }),
		body,
	})
end

return P
