-- MPRIS media players, on dock.dbus: a bundled module (`require 'mpris'`).
--
--   local mpris = require 'mpris'
--   local w = mpris.watch({ app = dock.app, player = opts.player }, function(p)
--     -- p: the player this widget shows now, or nil; called again when
--     -- the choice changes or the player does.
--   end)
--   w:refresh()  -- choose again (when dock.app changed: on_app)
--   w:sync()     -- ask the player where it is now
--   w:cancel()
--
-- Which player: with `player` (a bus name suffix, `*` at the end matching
-- any rest: "chromium*"), the ones it names; else with `app`, the ones
-- whose `DesktopEntry`, or bus name suffix without an `.instanceN`
-- (`org.mpris.MediaPlayer2.spotify` is `spotify`), is one of the app's
-- `id`, `desktop` or `wm_class`, ignoring case; else (standalone) all of
-- them. Of those, the one playing, else the one that changed last.
--
-- A player has `name` (its bus name), `id` (the suffix), `identity`,
-- `desktop`, `status` ('Playing', 'Paused' or 'Stopped'), `playing`,
-- `title`, `artist` (artists joined with ", "), `album`, `art` (a URL),
-- `length` and `position` (seconds; `position` as of `at`, a dock.now()
-- time), `rate`, `track`, `volume`, and `can_play`, `can_pause`,
-- `can_seek`, `can_next`, `can_previous`, `can_control` and `can_raise`.
-- `p:now()` is the position now. Methods: `play_pause`, `play`, `pause`,
-- `stop`, `next`, `previous`, `set_position(seconds)`, `seek(seconds)`
-- and `raise`.

local M = {}

local PREFIX = "org.mpris.MediaPlayer2."
local PATH = "/org/mpris/MediaPlayer2"
local ROOT = "org.mpris.MediaPlayer2"
local PLAYER = "org.mpris.MediaPlayer2.Player"
-- How often the chosen player's position is checked while it plays
-- (for players that don't say when they jump).
local RESYNC = 10000
-- A position this far off the estimate is news.
local DRIFT = 0.75

local bus = dock.dbus.session()
-- Players by bus name, and the order they came in.
local players, order = {}, {}
local watchers = {}
local names_sub

local function lower(s)
	return type(s) == "string" and s:lower() or nil
end

local Player = {}
Player.__index = Player

--- The position now, in seconds.
function Player:now()
	local pos = self.position or 0
	if self.playing and self.at then
		pos = pos + (self.rate or 1) * (dock.now() - self.at)
	end
	if self.length and self.length > 0 then
		pos = math.min(pos, self.length)
	end
	return math.max(pos, 0)
end

function Player:call(method, signature, args, iface)
	bus:call({
		dest = self.name,
		path = PATH,
		iface = iface or PLAYER,
		method = method,
		signature = signature,
		args = args,
		on_reply = function(_, err)
			if err then
				print(self.name .. " " .. method .. ": " .. err)
			end
		end,
	})
end

function Player:play_pause()
	self:call("PlayPause")
end
function Player:play()
	self:call("Play")
end
function Player:pause()
	self:call("Pause")
end
function Player:stop()
	self:call("Stop")
end
function Player:next()
	self:call("Next")
end
function Player:previous()
	self:call("Previous")
end
function Player:raise()
	self:call("Raise", nil, nil, ROOT)
end

local changed -- (below)

--- Go to `seconds` into the track.
function Player:set_position(seconds)
	seconds = math.max(0, tonumber(seconds) or 0)
	local us = math.floor(seconds * 1e6)
	if self.track and self.track ~= "" then
		self:call("SetPosition", "ox", { self.track, us })
	else
		self:call("Seek", "x", { us - math.floor(self:now() * 1e6) })
	end
	-- Where it will be (the player says so with Seeked, if it does).
	self.position, self.at = seconds, dock.now()
	changed(self, false)
end

--- Move by `seconds` (back if negative).
function Player:seek(seconds)
	self:call("Seek", "x", { math.floor((tonumber(seconds) or 0) * 1e6) })
end

-- -- choosing --------------------------------------------------------------

local function matches(w, p)
	local want = w.opts.player
	if type(want) == "string" and want ~= "" then
		want = want:lower()
		local id = p.id:lower()
		if want:sub(-1) == "*" then
			return id:sub(1, #want - 1) == want:sub(1, -2)
		end
		return id == want
	end
	local app = w.opts.app
	if app == nil then
		return true
	end
	local names = {}
	if type(app) == "table" then
		for _, k in ipairs({ "id", "desktop", "wm_class" }) do
			if app[k] then
				names[lower(app[k])] = true
			end
		end
	else
		names[lower(tostring(app))] = true
	end
	return names[p.id:lower()] or names[lower(p.desktop) or false] or false
end

local function choose(w)
	local best
	for _, name in ipairs(order) do
		local p = players[name]
		if p and p.ready and matches(w, p) then
			if not best or (p.playing and not best.playing) or (p.playing == best.playing and p.changed > best.changed) then
				best = p
			end
		end
	end
	return best
end

--- Tell watcher `w` its choice (if it changed, or `p` is it and changed).
local function update(w, p)
	local best = choose(w)
	if best ~= w.chosen or (p and best == p) then
		w.chosen = best
		w.fn(best)
	end
end

-- Player `p` changed (`news`: in a way that makes it the last to change).
changed = function(p, news)
	if news then
		p.changed = dock.now()
	end
	for _, w in pairs(watchers) do
		update(w, p)
	end
end

-- -- players ---------------------------------------------------------------

local function artists(v)
	if type(v) == "table" then
		local t = {}
		for _, a in ipairs(v) do
			t[#t + 1] = tostring(a)
		end
		return table.concat(t, ", ")
	end
	return v
end

local function sync(p)
	bus:get(p.name, PATH, PLAYER, "Position", function(v)
		if not players[p.name] then
			return
		end
		local pos = (tonumber(v) or 0) / 1e6
		local off = math.abs(pos - p:now())
		p.position, p.at = pos, dock.now()
		if off > DRIFT then
			changed(p, false)
		end
	end)
end

-- Take in Player properties; true if the position needs asking for.
local function apply(p, props)
	local resync, news = false, false
	for k, v in pairs(props) do
		if k == "PlaybackStatus" then
			if p.status ~= v then
				news = true
			end
			p.status, p.playing = v, v == "Playing"
			resync = true
		elseif k == "Metadata" then
			local m = type(v) == "table" and v or {}
			local track = m["mpris:trackid"]
			if track ~= p.track or m["xesam:title"] ~= p.title then
				news, resync = true, true
			end
			p.track = track
			p.title = m["xesam:title"]
			p.artist = artists(m["xesam:artist"])
			p.album = m["xesam:album"]
			p.art = m["mpris:artUrl"]
			if p.art == "" then
				p.art = nil
			end
			local len = tonumber(m["mpris:length"])
			p.length = len and len / 1e6 or nil
		elseif k == "Rate" then
			p.position, p.at = p:now(), dock.now()
			p.rate = tonumber(v) or 1
			resync = true
		elseif k == "Position" then
			p.position, p.at = (tonumber(v) or 0) / 1e6, dock.now()
		elseif k == "Volume" then
			p.volume = tonumber(v)
		elseif k == "CanPlay" then
			p.can_play = v
		elseif k == "CanPause" then
			p.can_pause = v
		elseif k == "CanSeek" then
			p.can_seek = v
		elseif k == "CanGoNext" then
			p.can_next = v
		elseif k == "CanGoPrevious" then
			p.can_previous = v
		elseif k == "CanControl" then
			p.can_control = v
		end
	end
	return resync, news
end

local function remove(name)
	local p = players[name]
	if not p then
		return
	end
	players[name] = nil
	for i, n in ipairs(order) do
		if n == name then
			table.remove(order, i)
			break
		end
	end
	for _, sub in ipairs(p.subs) do
		sub:cancel()
	end
	changed(p, false)
end

local function add(name)
	remove(name)
	local id = name:sub(#PREFIX + 1):gsub("%.instance[%d_]+$", "")
	local p = setmetatable({
		name = name,
		id = id,
		subs = {},
		changed = 0,
		rate = 1,
		position = 0,
		at = dock.now(),
		ready = false,
	}, Player)
	players[name] = p
	order[#order + 1] = name
	local function alive()
		return players[name] == p
	end
	local root_done, player_done = false, false
	local function done()
		if root_done and player_done and alive() then
			p.ready = true
			changed(p, false)
		end
	end
	p.subs[#p.subs + 1] = bus:watch_props(name, PATH, PLAYER, function(props, invalidated)
		if not alive() then
			return
		end
		local resync, news = apply(p, props)
		if resync then
			sync(p)
		end
		if #invalidated > 0 then
			bus:get_all(name, PATH, PLAYER, function(all)
				if alive() and all then
					apply(p, all)
					changed(p, false)
				end
			end)
		end
		if p.ready then
			changed(p, news)
		end
	end)
	p.subs[#p.subs + 1] = bus:watch({
		sender = name,
		path = PATH,
		iface = PLAYER,
		member = "Seeked",
		on_signal = function(args)
			if alive() then
				p.position, p.at = (tonumber(args[1]) or 0) / 1e6, dock.now()
				changed(p, false)
			end
		end,
	})
	bus:get_all(name, PATH, ROOT, function(props)
		if alive() and props then
			p.identity = props.Identity
			p.desktop = props.DesktopEntry
			p.can_raise = props.CanRaise
		end
		root_done = true
		done()
	end)
	bus:get_all(name, PATH, PLAYER, function(props, err)
		if alive() and props then
			apply(p, props)
			-- Not every player has Position among all of them.
			if props.Position == nil then
				sync(p)
			end
		elseif err then
			print(name .. ": " .. err)
		end
		player_done = true
		done()
	end)
end

local function start()
	if names_sub then
		return
	end
	names_sub = bus:watch_names(PREFIX, function(name, owner)
		if owner then
			add(name)
		else
			remove(name)
		end
	end)
end

-- -- watchers --------------------------------------------------------------

local Watcher = {}
Watcher.__index = Watcher

function Watcher:refresh()
	update(self)
end

function Watcher:sync()
	if self.chosen then
		sync(self.chosen)
	end
end

function Watcher:cancel()
	watchers[self] = nil
	if self.timer then
		self.timer:cancel()
		self.timer = nil
	end
end

--- Follow the player `opts` (`app`, `player`) chooses: `fn(player)`.
function M.watch(opts, fn)
	start()
	local w = setmetatable({ opts = opts or {}, fn = fn }, Watcher)
	watchers[w] = w
	w.timer = dock.every(RESYNC, function()
		if w.chosen and w.chosen.playing then
			sync(w.chosen)
		end
	end)
	update(w)
	return w
end

--- The players there are, by bus name.
function M.players()
	return players
end

return M

