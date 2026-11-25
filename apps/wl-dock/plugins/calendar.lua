-- calendar: a month calendar with an agenda of the events in Evolution Data
-- Server (EDS): the calendars of GNOME Calendar and Evolution.
--
-- Standalone it draws a calendar page as its icon, shows the date and the
-- next event as its label, and opens a popup on click with the month grid,
-- paging, and an agenda of the selected day and the days after it. Clicking
-- an event shows its details, with buttons to open its link, show it in
-- GNOME Calendar and copy it.
--
-- The calendars are read over D-Bus, with their names and colours, and
-- followed as they change: an event edited in GNOME Calendar shows here at
-- once. EDS syncs the remote ones (CalDAV, Google, Microsoft 365, webcal);
-- opening the popup asks it to refresh them.
--
-- [widget.calendar]
-- name = "Calendar"
-- calendars = ["Work", "Family"]
--
-- Options:
--   calendars     Names or EDS UIDs of the calendars to show. Default: the
--                 enabled calendars that are shown in GNOME Calendar.
--   week_start    "monday" (default), "sunday" or "saturday"
--   week_numbers  Show ISO week numbers in the grid (default false)
--   agenda_days   Days the agenda covers from the selected day (default 14, 1-366)
--   format        os.date format of the label's date (default "%A %e %B")
--   time          os.date format of times (default "%H:%M")
--   face          Draw a calendar page as the icon (standalone: on; app: off)
--   label         Show the date and the next event as the label (default true)
--   popup         Show the popup (default true)
--   open          argv run by an event's Open button; default: xdg-open on the
--                 event's link
--   calendar_app  argv run by the Calendar button (default: gnome-calendar
--                 --uuid {event}); false leaves the button out
--
-- In `open` and `calendar_app`, {title}, {url}, {uid}, {location}, {start}
-- (ISO date-time), {date} (ISO date), {calendar} (the EDS source UID) and
-- {event} (GNOME Calendar's id of the occurrence) are replaced.
--
-- Events with a TZID are read as local time (there is no time zone database
-- in the sandbox); UTC times ("...Z") are converted. Recurrences support
-- FREQ DAILY/WEEKLY/MONTHLY/YEARLY with INTERVAL, COUNT, UNTIL, BYDAY,
-- BYMONTHDAY, BYMONTH, BYSETPOS and WKST, plus EXDATE and RECURRENCE-ID.

local ui, theme = dock.ui, dock.theme
local P = { api = 1, popup_on = "click" }

-- Options --------------------------------------------------------------------

local wanted -- calendar names (lower case) and UIDs, or nil
local week_start = 1 -- 0 = Sunday ... 6 = Saturday
local week_numbers = false
local agenda_days = 14
local label_format = "%A %e %B"
local time_format = "%H:%M"
local face = dock.app == nil
local show_label, show_popup = true, true
local open_argv -- list of strings, or nil for xdg-open
local calendar_argv = { "gnome-calendar", "--uuid", "{event}" }

local PALETTE = { theme.accent, "#30d158", "#ff9f0a", "#bf5af2", "#ff453a", "#64d2ff", "#ffd60a" }
-- At most this many events in the agenda, to keep the popup's tree small.
local AGENDA_MAX = 100
-- Events parsed per callback, so a large calendar never runs one over the budget.
local CHUNK = 40

-- Days -----------------------------------------------------------------------
-- A day is an integer: days since 1970-01-01 in the local calendar.

local function days_from_civil(y, m, d)
	if m <= 2 then
		y = y - 1
	end
	local era = y // 400
	local yoe = y - era * 400
	local doy = (153 * ((m + 9) % 12) + 2) // 5 + d - 1
	local doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
	return era * 146097 + doe - 719468
end

local function civil(z)
	z = z + 719468
	local era = z // 146097
	local doe = z - era * 146097
	local yoe = (doe - doe // 1460 + doe // 36524 - doe // 146096) // 365
	local doy = doe - (365 * yoe + yoe // 4 - yoe // 100)
	local mp = (5 * doy + 2) // 153
	local d = doy - (153 * mp + 2) // 5 + 1
	local m = mp < 10 and mp + 3 or mp - 9
	local y = yoe + era * 400
	if m <= 2 then
		y = y + 1
	end
	return y, m, d
end

local function weekday(day)
	return (day + 4) % 7
end -- 0 = Sunday

local function month_len(y, m)
	local ny, nm = y, m + 1
	if nm > 12 then
		ny, nm = y + 1, 1
	end
	return days_from_civil(ny, nm, 1) - days_from_civil(y, m, 1)
end

-- The local time at which `day` starts, or at `h:mi:s` on it.
local function day_start(day, h, mi, s)
	local y, m, d = civil(day)
	return os.time({ year = y, month = m, day = d, hour = h or 0, min = mi or 0, sec = s or 0 })
end

local function day_of(t)
	local d = os.date("*t", math.floor(t))
	return days_from_civil(d.year, d.month, d.day)
end

local function today()
	return day_of(os.time())
end

-- os.date with the padding of %e and friends squeezed out.
local function fdate(format, t)
	return (os.date(format, math.floor(t)):gsub("%s+", " "):gsub("^ ", ""):gsub(" $", ""))
end

local function ftime(t)
	return fdate(time_format, t)
end

-- iCalendar parsing ----------------------------------------------------------

-- A DATE or DATE-TIME value: { day = n } or { t = epoch }.
local function parse_dt(v)
	local y, m, d = v:match("^(%d%d%d%d)(%d%d)(%d%d)")
	if not y then
		return nil
	end
	y, m, d = tonumber(y), tonumber(m), tonumber(d)
	local hh, mi, ss, z = v:match("^%d+T(%d%d)(%d%d)(%d%d)(Z?)")
	if not hh then
		return { day = days_from_civil(y, m, d) }
	end
	hh, mi, ss = tonumber(hh), tonumber(mi), tonumber(ss)
	if z == "Z" then
		return { t = days_from_civil(y, m, d) * 86400 + hh * 3600 + mi * 60 + ss, utc = true }
	end
	return { t = os.time({ year = y, month = m, day = d, hour = hh, min = mi, sec = ss }) }
end

-- Seconds in a DURATION such as "PT1H30M" or "-P1D".
local DURATION_UNITS = { W = 604800, D = 86400, H = 3600, M = 60, S = 1 }
local function parse_duration(v)
	local sign, rest = v:match("^([+-]?)P(.*)$")
	if not rest then
		return nil
	end
	local secs = 0
	for n, u in rest:gmatch("(%d+)([WDHMS])") do
		secs = secs + tonumber(n) * DURATION_UNITS[u]
	end
	return sign == "-" and -secs or secs
end

local ESCAPES = { n = "\n", N = "\n" }
local function unescape(s)
	return (s:gsub("\\(.)", function(c)
		return ESCAPES[c] or c
	end))
end

local function ints(v)
	local out = {}
	for n in v:gmatch("[+-]?%d+") do
		out[#out + 1] = math.tointeger(tonumber(n))
	end
	return out
end

local WEEKDAYS = { SU = 0, MO = 1, TU = 2, WE = 3, TH = 4, FR = 5, SA = 6 }
local FREQS = { DAILY = true, WEEKLY = true, MONTHLY = true, YEARLY = true }

local function parse_rrule(v)
	local r = { interval = 1, wkst = 1 }
	for k, val in v:gmatch("([^;=]+)=([^;]*)") do
		k = k:upper()
		if k == "FREQ" then
			r.freq = val:upper()
		elseif k == "INTERVAL" then
			r.interval = math.max(1, math.tointeger(tonumber(val)) or 1)
		elseif k == "COUNT" then
			r.count = math.tointeger(tonumber(val))
		elseif k == "UNTIL" then
			r.till = parse_dt(val)
		elseif k == "WKST" then
			r.wkst = WEEKDAYS[val:upper()] or 1
		elseif k == "BYDAY" then
			r.byday = {}
			for n, d in val:gmatch("([+-]?%d*)(%a%a)") do
				local wd = WEEKDAYS[d:upper()]
				if wd then
					r.byday[#r.byday + 1] = { ord = math.tointeger(tonumber(n)) or 0, wd = wd }
				end
			end
			if #r.byday == 0 then
				r.byday = nil
			end
		elseif k == "BYMONTHDAY" then
			r.bymonthday = ints(val)
		elseif k == "BYMONTH" then
			r.bymonth = ints(val)
		elseif k == "BYSETPOS" then
			r.bysetpos = ints(val)
		end
	end
	if not FREQS[r.freq] then
		return nil
	end
	r.plain = not (r.byday or r.bymonthday or r.bymonth or r.bysetpos)
	return r
end

-- A key for EXDATE and RECURRENCE-ID lookups.
local function dt_key(x)
	return x.day and ("d" .. x.day) or ("t" .. x.t)
end

-- Splits a content line into its upper-cased name and its value.
local function split(line)
	local colon = line:find(":", 1, true)
	if not colon then
		return nil
	end
	local quote = line:find('"', 1, true)
	if quote and quote < colon then
		-- A quoted parameter may hold a colon: take the first one outside quotes.
		local inside = false
		for i = 1, #line do
			local c = line:byte(i)
			if c == 34 then
				inside = not inside
			elseif c == 58 and not inside then
				colon = i
				break
			end
		end
	end
	local head = line:sub(1, colon - 1)
	local semi = head:find(";", 1, true)
	return (semi and head:sub(1, semi - 1) or head):upper(), line:sub(colon + 1)
end

-- Recurrence -----------------------------------------------------------------

-- The days of month `m` of year `y` that rule `r` picks; `default` is the
-- day of the month to use when the rule doesn't say.
local function month_days(y, m, r, default)
	local first, n = days_from_civil(y, m, 1), month_len(y, m)
	local last = first + n - 1
	local out = {}
	if r.bymonthday then
		for _, md in ipairs(r.bymonthday) do
			local d = md > 0 and md or n + md + 1
			if d >= 1 and d <= n then
				local day = first + d - 1
				local ok = not r.byday
				for _, b in ipairs(r.byday or {}) do
					if b.wd == weekday(day) then
						ok = true
					end
				end
				if ok then
					out[#out + 1] = day
				end
			end
		end
	elseif r.byday then
		for _, b in ipairs(r.byday) do
			if b.ord == 0 then
				for d = first + (b.wd - weekday(first)) % 7, last, 7 do
					out[#out + 1] = d
				end
			elseif b.ord > 0 then
				local d = first + (b.wd - weekday(first)) % 7 + (b.ord - 1) * 7
				if d <= last then
					out[#out + 1] = d
				end
			else
				local d = last - (weekday(last) - b.wd) % 7 + (b.ord + 1) * 7
				if d >= first then
					out[#out + 1] = d
				end
			end
		end
	elseif default <= n then
		out[1] = first + default - 1
	end
	return out
end

-- The period (day, week, month or year) that `day` falls in, as an integer.
local function unit_of(r, day)
	if r.freq == "DAILY" then
		return day
	end
	if r.freq == "WEEKLY" then
		return (day + 4 - r.wkst) // 7
	end
	local y, m = civil(day)
	if r.freq == "MONTHLY" then
		return y * 12 + m - 1
	end
	return y
end

-- The days of period `u`, sorted. `st` describes the first occurrence.
local function candidates(r, u, st)
	local out
	if r.plain and r.freq == "DAILY" then
		return { u }
	end
	if r.plain and r.freq == "WEEKLY" then
		return { u * 7 - 4 + r.wkst + (st.wd - r.wkst) % 7 }
	end
	if r.freq == "DAILY" then
		out = { u }
	elseif r.freq == "WEEKLY" then
		local ws = u * 7 - 4 + r.wkst
		out = {}
		for _, b in ipairs(r.byday or { { wd = st.wd } }) do
			out[#out + 1] = ws + (b.wd - r.wkst) % 7
		end
	elseif r.freq == "MONTHLY" then
		out = month_days(u // 12, u % 12 + 1, r, st.d)
	else
		out = {}
		for _, m in ipairs(r.bymonth or { st.m }) do
			if m >= 1 and m <= 12 then
				for _, d in ipairs(month_days(u, m, r, st.d)) do
					out[#out + 1] = d
				end
			end
		end
	end
	table.sort(out)
	local keep = {}
	for i, d in ipairs(out) do
		local ok = i == 1 or d ~= out[i - 1]
		if ok and r.bymonth and r.freq ~= "YEARLY" then
			local _, m = civil(d)
			ok = false
			for _, bm in ipairs(r.bymonth) do
				if bm == m then
					ok = true
				end
			end
		end
		if ok and r.freq == "DAILY" and r.byday then
			ok = false
			for _, b in ipairs(r.byday) do
				if b.wd == weekday(d) then
					ok = true
				end
			end
		end
		if ok and r.freq == "DAILY" and r.bymonthday then
			local y, m, dd = civil(d)
			local n = month_len(y, m)
			ok = false
			for _, md in ipairs(r.bymonthday) do
				if md == dd or n + md + 1 == dd then
					ok = true
				end
			end
		end
		if ok then
			keep[#keep + 1] = d
		end
	end
	if r.bysetpos and #keep > 0 then
		local picked = {}
		for _, pos in ipairs(r.bysetpos) do
			local d = keep[pos > 0 and pos or #keep + pos + 1]
			if d then
				picked[#picked + 1] = d
			end
		end
		table.sort(picked)
		keep = picked
	end
	return keep
end

-- The day of the last occurrence of rule `r` with a COUNT, starting on day
-- `sday`. Worked out once, while the event is read, so that expanding the
-- rule later can skip ahead instead of counting from the start every time.
local function count_end(r, sday)
	local plain = not (r.byday or r.bymonthday or r.bymonth or r.bysetpos)
	if plain and r.freq == "DAILY" then
		return sday + (r.count - 1) * r.interval
	end
	if plain and r.freq == "WEEKLY" then
		return sday + (r.count - 1) * 7 * r.interval
	end
	local _, sm, sd = civil(sday)
	local st = { m = sm, d = sd, wd = weekday(sday) }
	local su, n, last = unit_of(r, sday), 0, sday
	for p = 0, 5000 do
		for _, c in ipairs(candidates(r, su + p * r.interval, st)) do
			if c >= sday then
				n, last = n + 1, c
				if n >= r.count then
					return c
				end
			end
		end
	end
	return last
end

-- Turns a VEVENT's properties into an event, or nil. `source` is its calendar.
local function make_event(e, source)
	local s = e.DTSTART and parse_dt(e.DTSTART)
	if not s then
		return nil
	end
	if e.STATUS and e.STATUS:upper() == "CANCELLED" then
		return nil
	end
	local description = e.DESCRIPTION and unescape(e.DESCRIPTION):gsub("^%s+", ""):gsub("%s+$", "")
	if description == "" then
		description = nil
	end
	if description and #description > 4000 then
		description = description:sub(1, 4000) .. "…"
	end
	local title = e.SUMMARY and unescape(e.SUMMARY):gsub("%s+", " ")
	local location = e.LOCATION and unescape(e.LOCATION)
	local ev = {
		title = (title and title ~= "") and title or "(No title)",
		location = location ~= "" and location or nil,
		description = description,
		url = e.URL,
		uid = e.UID,
		rid = e["RECURRENCE-ID"], -- as written, for GNOME Calendar's id
		okey = source.uid .. "\n" .. (e.UID or ""), -- the overrides it shares
		source = source,
		allday = s.day ~= nil,
		utc = s.utc, -- recurs at the same UTC time rather than the same local time
	}
	-- The first day: local, or the UTC day for an event anchored in UTC.
	ev.sday = ev.allday and s.day or s.utc and s.t // 86400 or day_of(s.t)
	local en = e.DTEND and parse_dt(e.DTEND)
	local dur = e.DURATION and parse_duration(e.DURATION)
	if ev.allday then
		ev.s = s.day
		if en and en.day and en.day > s.day then
			ev.e = en.day
		elseif dur then
			ev.e = s.day + math.max(1, dur // 86400)
		else
			ev.e = s.day + 1
		end
	else
		ev.s = s.t
		if en and en.t then
			ev.e = math.max(en.t, s.t)
		elseif en and en.day then
			ev.e = math.max(day_start(en.day), s.t)
		elseif dur then
			ev.e = s.t + math.max(0, dur)
		else
			ev.e = s.t
		end
	end
	if e["RECURRENCE-ID"] then
		local r = parse_dt(e["RECURRENCE-ID"])
		ev.recur = r and dt_key(r)
	elseif e.RRULE then
		ev.rule = parse_rrule(e.RRULE)
		if ev.rule and ev.rule.count then
			ev.rule.last = count_end(ev.rule, ev.sday)
		end
	end
	if e.EXDATE then
		ev.ex = {}
		for _, v in ipairs(e.EXDATE) do
			local x = parse_dt(v)
			if x then
				ev.ex[dt_key(x)] = true
			end
		end
	end
	return ev
end

-- A line-by-line parser that calls `on_event(props)` with the properties of
-- each VEVENT it reads.
local function new_parser(on_event)
	local pending -- parts of the logical line being unfolded
	local cur, depth -- the VEVENT being read, and nesting inside it (VALARM)

	local function handle(line)
		local name, value = split(line)
		if not name then
			return
		end
		if name == "BEGIN" then
			if cur then
				depth = depth + 1
			elseif value:upper() == "VEVENT" then
				cur, depth = {}, 0
			end
		elseif name == "END" then
			if cur then
				if depth > 0 then
					depth = depth - 1
				else
					on_event(cur)
					cur = nil
				end
			end
		elseif cur and depth == 0 then
			if name == "EXDATE" then
				local list = cur.EXDATE or {}
				for v in value:gmatch("[^,]+") do
					list[#list + 1] = v
				end
				cur.EXDATE = list
			else
				cur[name] = value
			end
		end
	end

	local parser = {}
	function parser.line(l)
		l = l:gsub("[\r\n]+$", "")
		local c = l:byte(1)
		if (c == 32 or c == 9) and pending then
			pending[#pending + 1] = l:sub(2)
		else
			if pending then
				handle(table.concat(pending))
			end
			pending = { l }
		end
	end
	function parser.done()
		if pending then
			handle(table.concat(pending))
		end
		pending = nil
	end
	return parser
end

local events, overrides = {}, {} -- all events shown; RECURRENCE-ID keys by okey

-- day_start(day), remembered: expanding recurrences asks for the same few
-- days over and over.
local midnights, midnights_n = {}, 0
local function midnight(day)
	local t = midnights[day]
	if not t then
		if midnights_n >= 4096 then
			midnights, midnights_n = {}, 0
		end
		t = day_start(day)
		midnights[day], midnights_n = t, midnights_n + 1
	end
	return t
end

-- Calls emit(s, e, c) for each occurrence of `ev` that starts before day
-- `d1` and may reach day `d0`. All-day events use days, others times. `c`
-- is the local day the occurrence starts on, when it is known.
local function occurrences(ev, d0, d1, emit)
	local r = ev.rule
	if not r then
		emit(ev.s, ev.e)
		return
	end
	-- What doesn't depend on the window, worked out on first use.
	local x = ev.x
	if not x then
		local _, sm, sd = civil(ev.sday)
		local wall = not ev.allday and not ev.utc and os.date("*t", ev.s) or nil
		local dur = ev.e - ev.s
		x = {
			st = { m = sm, d = sd, wd = weekday(ev.sday) },
			wall = wall,
			tod = wall and wall.hour * 3600 + wall.min * 60 + wall.sec,
			su = unit_of(r, ev.sday),
			dur = dur,
			reach = ev.allday and dur or (dur // 86400 + 1),
		}
		ev.x = x
	end
	local sday, st, wall, tod, su, dur, reach = ev.sday, x.st, x.wall, x.tod, x.su, x.dur, x.reach
	local ex, ov = ev.ex, overrides[ev.okey]
	-- Skip straight to the periods near the window.
	local p = math.max(0, (unit_of(r, d0 - reach - 2) - su) // r.interval - 1)
	local till, last = r.till, r.last
	local stop = d1 + (ev.utc and 1 or 0)
	for _ = 1, 5000 do
		for _, c in ipairs(candidates(r, su + p * r.interval, st)) do
			if c >= sday then
				if c >= stop or (last and c > last) then
					return
				end
				local s
				if ev.allday then
					s = c
				elseif ev.utc then
					s = c * 86400 + ev.s % 86400
				else
					s = midnight(c)
					-- A day with a DST change isn't 24 hours long: let os.time work it out.
					if midnight(c + 1) - s == 86400 then
						s = s + tod
					else
						s = day_start(c, wall.hour, wall.min, wall.sec)
					end
				end
				if till then
					if till.day and c > till.day then
						return
					end
					if till.t and (ev.allday and midnight(c) or s) > till.t then
						return
					end
				end
				local skip = false
				if ex or ov then
					local kd = "d" .. c
					local kt = not ev.allday and ("t" .. s)
					skip = (ex and (ex[kd] or (kt and ex[kt]))) or (ov and (ov[kd] or (kt and ov[kt]))) or false
				end
				if not skip then
					emit(s, s + dur, wall and c)
				end
			end
		end
		p = p + 1
	end
end

-- Occurrences by day ------------------------------------------------------------

local cache, cached = {}, 0

-- A table from each day in [d0, d1) that has events to its occurrences
-- { ev, s, e, k }, unsorted (see sorted()).
local function query(d0, d1)
	local ck = d0 .. ":" .. d1
	if cache[ck] then
		return cache[ck]
	end
	local days = {}
	local t0, t1 = midnight(d0), midnight(d1)
	for _, ev in ipairs(events) do
		if ev.allday then
			if ev.rule or (ev.s < d1 and ev.e > d0) then
				occurrences(ev, d0, d1, function(s, e)
					-- All-day events sort first.
					local o = { ev = ev, s = s, e = e, k = s - 1e9 }
					for d = math.max(s, d0), math.min(e - 1, d1 - 1) do
						local list = days[d] or {}
						days[d] = list
						list[#list + 1] = o
					end
				end)
			end
		elseif ev.rule or (ev.s < t1 and ev.e >= t0) then
			occurrences(ev, d0, d1, function(s, e, c)
				if s >= t1 or e < t0 or (e == t0 and e > s) then
					return
				end
				local a = c or day_of(s)
				local b = a
				if e > midnight(a + 1) then
					b = day_of(e - 1)
				end
				local o = { ev = ev, s = s, e = e, k = s }
				for d = math.max(a, d0), math.min(b, d1 - 1) do
					local list = days[d] or {}
					days[d] = list
					list[#list + 1] = o
				end
			end)
		end
	end
	if cached >= 32 then
		cache, cached = {}, 0
	end
	cache[ck], cached = days, cached + 1
	return days
end

local function by_start(a, b)
	if a.k ~= b.k then
		return a.k < b.k
	end
	if a.e ~= b.e then
		return a.e < b.e
	end
	return a.ev.title < b.ev.title
end

-- A day's list from query(), sorted: all-day first, then by start. Only the
-- days that are shown get sorted.
local function sorted(list)
	if list and not list.sorted then
		table.sort(list, by_start)
		list.sorted = true
	end
	return list
end

-- Whether occurrence `o` is over at time `now`.
local function is_past(o, now)
	if o.ev.allday then
		return o.e <= day_of(now)
	end
	return o.e < now or (o.e == o.s and o.s < now)
end

-- The timed event happening now or next today, if any.
local function next_event()
	local t, now = today(), os.time()
	for _, o in ipairs(sorted(query(t, t + 1)[t]) or {}) do
		if not o.ev.allday and not is_past(o, now) then
			return o
		end
	end
end

-- Evolution Data Server ----------------------------------------------------------

local bus = dock.dbus.session()
local EDS = "org.gnome.evolution.dataserver."
local SOURCES = EDS .. "Sources5"
local SOURCES_PATH = "/org/gnome/evolution/dataserver/SourceManager"
local SOURCE = EDS .. "Source"
local FACTORY = EDS .. "Calendar8"
local FACTORY_PATH = "/org/gnome/evolution/dataserver/CalendarFactory"
local CALENDAR = EDS .. "Calendar"
local VIEW = EDS .. "CalendarView"
local OBJECT_MANAGER = "org.freedesktop.DBus.ObjectManager"
local PROPERTIES = "org.freedesktop.DBus.Properties"
-- Opening a remote calendar can take a while.
local TIMEOUT = 30000

-- The calendars shown, by EDS source UID: { uid, name, color, bus, path,
-- view, sub, objects, ready, failed }. `objects` holds the events by UID,
-- then RECURRENCE-ID ('' for the event itself).
local calendars = {}
local order = {} -- the same calendars, by name
local registry_loaded -- the source registry has been read
local problem -- why EDS can't be reached, or nil
local signature -- what the label and icon showed last, see tick()
local last_refresh -- os.time() remote calendars were last refreshed

local function call(dest, path, iface, method, signature_, args, done)
	bus:call({
		dest = dest,
		path = path,
		iface = iface,
		method = method,
		signature = signature_,
		args = args,
		timeout = TIMEOUT,
		on_reply = done or function() end,
	})
end

local function alive(cal)
	return calendars[cal.uid] == cal
end

local function rebuild()
	events, overrides, cache, cached = {}, {}, {}, 0
	for _, cal in ipairs(order) do
		for _, by_rid in pairs(cal.objects) do
			for _, ev in pairs(by_rid) do
				events[#events + 1] = ev
				if ev.recur then
					local o = overrides[ev.okey] or {}
					overrides[ev.okey] = o
					o[ev.recur] = true
				end
			end
		end
	end
	signature = nil
	dock.redraw()
end

-- Takes in a component from EDS: new, or replacing what it had.
local function put_object(cal, text)
	local parser = new_parser(function(props)
		local uid, rid = props.UID or "", props["RECURRENCE-ID"] or ""
		local by_rid = cal.objects[uid] or {}
		-- nil (cancelled, say) drops what was there.
		by_rid[rid] = make_event(props, cal)
		cal.objects[uid] = next(by_rid) and by_rid or nil
	end)
	for line in text:gmatch("[^\n]+") do
		parser.line(line)
	end
	parser.done()
end

-- Drops a component: `id` is its UID, a newline, and its RECURRENCE-ID
-- (empty for the whole event).
local function remove_object(cal, id)
	local uid, rid = id:match("^([^\n]*)\n?(.*)$")
	local by_rid = cal.objects[uid]
	if not by_rid then
		return
	end
	if rid == "" then
		cal.objects[uid] = nil
		return
	end
	if by_rid[rid] == nil then
		-- The same time written another way.
		local want = parse_dt(rid)
		for k in pairs(by_rid) do
			local x = k ~= "" and parse_dt(k)
			if want and x and dt_key(x) == dt_key(want) then
				rid = k
			end
		end
	end
	by_rid[rid] = nil
	if not next(by_rid) then
		cal.objects[uid] = nil
	end
end

-- Changes from the views, taken in a chunk at a time, in order.
local queue, qhead, qtail = {}, 1, 0
local pumping

local function pump()
	pumping = nil
	local n = 0
	while n < CHUNK and qhead <= qtail do
		local job = queue[qhead]
		local item = job.items[job.i]
		if item == nil or not alive(job.cal) then
			queue[qhead], qhead = nil, qhead + 1
		else
			if job.remove then
				remove_object(job.cal, item)
			else
				put_object(job.cal, item)
			end
			job.i, n = job.i + 1, n + 1
		end
	end
	if qhead <= qtail then
		pumping = dock.timer(1, pump)
	else
		rebuild()
	end
end

local function enqueue(cal, remove, items)
	if type(items) ~= "table" or #items == 0 then
		return
	end
	qtail = qtail + 1
	queue[qtail] = { cal = cal, remove = remove, items = items, i = 1 }
	if not pumping then
		pumping = dock.timer(1, pump)
	end
end

local function fail(cal, what, err)
	cal.failed = true
	print(cal.name .. ": " .. what .. ": " .. tostring(err))
	dock.redraw()
end

-- Opens calendar `cal` and a view of all its events, which sends them all
-- and then every change.
local function open_calendar(cal)
	local function start_view()
		call(cal.bus, cal.path, CALENDAR, "GetView", "s", { "#t" }, function(ret, err)
			if ret and not alive(cal) then
				call(cal.bus, ret[1], VIEW, "Dispose")
			end
			if not alive(cal) then
				return
			end
			if not ret then
				return fail(cal, "GetView", err)
			end
			cal.view = ret[1]
			call(cal.bus, cal.view, VIEW, "Start", nil, nil, function(_, e)
				if e and alive(cal) then
					fail(cal, "Start", e)
				end
			end)
			-- In case Complete never comes.
			dock.timer(10000, function()
				if alive(cal) and not cal.ready then
					cal.ready = true
					dock.redraw()
				end
			end)
		end)
	end
	call(FACTORY, FACTORY_PATH, EDS .. "CalendarFactory", "OpenCalendar", "s", { cal.uid }, function(ret, err)
		if not alive(cal) then
			return
		end
		if not ret then
			return fail(cal, "OpenCalendar", err)
		end
		cal.path, cal.bus = ret[1], ret[2]
		-- Listening before the view exists, so its first events aren't missed.
		cal.sub = bus:watch({
			sender = cal.bus,
			iface = VIEW,
			on_signal = function(args, msg)
				if not alive(cal) or msg.path ~= cal.view then
					return
				end
				local m = msg.member
				if m == "ObjectsAdded" or m == "ObjectsModified" then
					enqueue(cal, false, args[1])
				elseif m == "ObjectsRemoved" then
					enqueue(cal, true, args[1])
				elseif m == "Complete" then
					if args[2] and args[2] ~= "" then
						print(cal.name .. ": " .. args[2])
					end
					cal.ready = true
					dock.redraw()
				end
			end,
		})
		call(cal.bus, cal.path, CALENDAR, "Open", nil, nil, function(_, e)
			if not alive(cal) then
				return
			end
			if e then
				return fail(cal, "Open", e)
			end
			start_view()
		end)
	end)
end

-- Stops following `cal`; `gone`: its service went away, so don't tell it.
local function close_calendar(cal, gone)
	calendars[cal.uid] = nil
	if cal.sub then
		cal.sub:cancel()
	end
	-- Not Close: the bus connection is shared with this plugin's other
	-- widgets, and EDS closes a calendar for the whole connection.
	if cal.view and not gone then
		call(cal.bus, cal.view, VIEW, "Dispose")
	end
end

-- The source registry --------------------------------------------------------------

local KEYFILE_ESCAPES = { s = " ", n = "\n", t = "\t", r = "\r", ["\\"] = "\\" }

-- A source's settings, a key file: groups of keys.
local function keyfile(text)
	local groups, g = {}, nil
	for line in (text .. "\n"):gmatch("([^\n]*)\n") do
		local name = line:match("^%[(.+)%]%s*$")
		if name then
			g = groups[name] or {}
			groups[name] = g
		elseif g then
			local k, v = line:match("^([^=#%s][^=]-)%s*=%s*(.-)%s*$")
			if k then
				g[k] = (v:gsub("\\(.)", KEYFILE_ESCAPES))
			end
		end
	end
	return groups
end

-- The locales to look for translated names in, best first.
local LOCALES = {}
do
	local env = os.getenv("LANGUAGE") or os.getenv("LC_ALL") or os.getenv("LC_MESSAGES") or os.getenv("LANG") or ""
	for l in env:gmatch("[^:]+") do
		l = l:gsub("[.@].*$", "")
		if l ~= "" and l ~= "C" and l ~= "POSIX" then
			LOCALES[#LOCALES + 1] = l
			local lang = l:match("^(%a+)_")
			if lang then
				LOCALES[#LOCALES + 1] = lang
			end
		end
	end
end

local function display_name(ds)
	for _, l in ipairs(LOCALES) do
		local n = ds["DisplayName[" .. l .. "]"]
		if n and n ~= "" then
			return n
		end
	end
	return ds.DisplayName ~= "" and ds.DisplayName or nil
end

-- Whether source `uid` and the accounts it belongs to are enabled.
local function enabled(found, uid, depth)
	local s = found[uid]
	if not s or depth > 8 then
		return true
	end
	local ds = s["Data Source"] or {}
	if ds.Enabled == "false" then
		return false
	end
	if ds.Parent and ds.Parent ~= "" then
		return enabled(found, ds.Parent, depth + 1)
	end
	return true
end

-- Follows the registry's sources `found` (key files by UID): opens the
-- calendars to show that aren't open, closes the others.
local function sync_calendars(found)
	registry_loaded, problem = true, nil
	local list = {}
	for uid, groups in pairs(found) do
		local c = groups.Calendar
		if c and enabled(found, uid, 0) then
			local name = display_name(groups["Data Source"] or {}) or uid
			local show
			if wanted then
				show = wanted[uid] or wanted[name:lower()]
			else
				show = c.Selected ~= "false"
			end
			if show then
				list[#list + 1] = { uid = uid, name = name, color = c.Color }
			end
		end
	end
	table.sort(list, function(a, b)
		if a.name ~= b.name then
			return a.name < b.name
		end
		return a.uid < b.uid
	end)
	local keep, new = {}, {}
	order = {}
	for i, c in ipairs(list) do
		local cal = calendars[c.uid]
		if not cal then
			cal = { uid = c.uid, objects = {} }
			calendars[c.uid] = cal
			new[#new + 1] = cal
		end
		cal.name = c.name
		local color = c.color and (c.color:match("^#%x%x%x%x%x%x$") or c.color:match("^#%x%x%x%x%x%x%x%x$"))
		cal.color = color or PALETTE[(i - 1) % #PALETTE + 1]
		keep[c.uid] = true
		order[i] = cal
	end
	for uid, cal in pairs(calendars) do
		if not keep[uid] then
			close_calendar(cal)
		end
	end
	for _, cal in ipairs(new) do
		open_calendar(cal)
	end
	rebuild()
end

local function load_registry()
	call(SOURCES, SOURCES_PATH, OBJECT_MANAGER, "GetManagedObjects", nil, nil, function(ret, err)
		if not ret then
			problem = "Can't reach Evolution Data Server"
			print("GetManagedObjects: " .. tostring(err))
			dock.redraw()
			return
		end
		local found = {}
		for _, ifaces in pairs(ret[1] or {}) do
			local s = ifaces[SOURCE]
			if s and type(s.UID) == "string" and type(s.Data) == "string" then
				found[s.UID] = keyfile(s.Data)
			end
		end
		sync_calendars(found)
	end)
end

-- Reads the registry again soon, once for a burst of changes.
local reload_timer
local function reload_soon(ms)
	if reload_timer then
		reload_timer:cancel()
	end
	reload_timer = dock.timer(ms, function()
		reload_timer = nil
		load_registry()
	end)
end

local function connect()
	-- Sources added, removed or changed (a colour, a name, shown or not).
	bus:watch({
		sender = SOURCES,
		path = SOURCES_PATH,
		iface = OBJECT_MANAGER,
		on_signal = function()
			reload_soon(500)
		end,
	})
	bus:watch({
		sender = SOURCES,
		iface = PROPERTIES,
		member = "PropertiesChanged",
		on_signal = function(args)
			if args[1] == SOURCE and type(args[2]) == "table" and args[2].Data ~= nil then
				reload_soon(500)
			end
		end,
	})
	-- EDS restarted, or a backend went away: open everything again.
	bus:watch_names(EDS, function(name, owner)
		if owner then
			return
		end
		local hit = name == SOURCES or name == FACTORY
		for _, cal in pairs(calendars) do
			hit = hit or cal.bus == name
		end
		if hit then
			for _, cal in pairs(calendars) do
				close_calendar(cal, true)
			end
			order = {}
			rebuild()
			reload_soon(2000)
		end
	end)
	load_registry()
end

-- Asks EDS to sync the remote calendars, at most every few minutes.
local function refresh()
	if last_refresh and os.time() - last_refresh < 300 then
		return
	end
	last_refresh = os.time()
	for _, cal in ipairs(order) do
		-- Local calendars can't: that's fine.
		if cal.path then
			call(cal.bus, cal.path, CALENDAR, "Refresh")
		end
	end
end

local function loading()
	if not registry_loaded then
		return true
	end
	for _, cal in ipairs(order) do
		if not cal.ready and not cal.failed then
			return true
		end
	end
	return false
end

-- Popup state ------------------------------------------------------------------

local view_y, view_m -- the month shown
local selected -- the selected day; the agenda starts there
local detail -- the occurrence whose details are shown
local copied -- the occurrence last copied
local is_open, closed_at = false, nil

local function go_today()
	selected = today()
	view_y, view_m = civil(selected)
	detail = nil
	dock.redraw()
end

-- Shows the month `delta` months away, and selects today in the current
-- month or else the month's first day, so the agenda follows the page.
local function page(delta)
	local m = view_y * 12 + view_m - 1 + delta
	view_y, view_m = m // 12, m % 12 + 1
	local t = today()
	local ty, tm = civil(t)
	selected = (ty == view_y and tm == view_m) and t or days_from_civil(view_y, view_m, 1)
	detail = nil
	dock.redraw()
end

local function select_day(d)
	selected = d
	local y, m = civil(d)
	view_y, view_m = y, m
	detail = nil
	dock.redraw()
end

-- Text -------------------------------------------------------------------------

local function day_title(d, t)
	local date = fdate("%A %e %B", day_start(d))
	if d == t then
		return "Today · " .. date
	end
	if d == t + 1 then
		return "Tomorrow · " .. date
	end
	return date
end

-- When occurrence `o` happens, as seen from day `d`.
local function when_on(o, d)
	if o.ev.allday then
		local len = o.e - o.s
		if len > 1 then
			return string.format("All day · day %d of %d", d - o.s + 1, len)
		end
		return "All day"
	end
	local starts, ends = o.s >= day_start(d), o.e <= day_start(d + 1)
	if starts and ends then
		if o.e == o.s then
			return ftime(o.s)
		end
		return ftime(o.s) .. " – " .. ftime(o.e)
	elseif starts then
		return "From " .. ftime(o.s)
	elseif ends then
		return "Until " .. ftime(o.e)
	end
	return "All day"
end

-- When occurrence `o` happens, in full.
local function when_long(o)
	local long = "%A %e %B %Y"
	if o.ev.allday then
		local first = fdate(long, day_start(o.s))
		if o.e - o.s <= 1 then
			return first
		end
		return first .. " – " .. fdate(long, day_start(o.e - 1))
	end
	local first = fdate(long, o.s)
	if o.e == o.s then
		return first .. ", " .. ftime(o.s)
	end
	if day_of(o.s) == day_of(o.e - 1) then
		return first .. ", " .. ftime(o.s) .. " – " .. ftime(o.e)
	end
	return first .. ", " .. ftime(o.s) .. " – " .. fdate(long, o.e) .. ", " .. ftime(o.e)
end

local FREQ_NAMES = { DAILY = "day", WEEKLY = "week", MONTHLY = "month", YEARLY = "year" }

local function repeats(r)
	local unit = FREQ_NAMES[r.freq]
	local text = r.interval == 1 and ("Every " .. unit) or string.format("Every %d %ss", r.interval, unit)
	if r.count then
		text = text .. string.format(", %d times", r.count)
	end
	if r.till then
		local t = r.till.day and day_start(r.till.day) or r.till.t
		text = text .. ", until " .. fdate("%e %B %Y", t)
	end
	return text
end

-- The first web link in `s`.
local function find_link(s)
	if not s then
		return nil
	end
	local url = s:match("https?://[%w%-%._~:/%?#%[%]@!%$&'%(%)%*%+,;=%%]+")
	return url and (url:gsub("[%.,;:!%?%)]+$", ""))
end

local function link_of(ev)
	return ev.url or find_link(ev.location) or find_link(ev.description)
end

-- GNOME Calendar's id of occurrence `o`: the calendar's UID, the event's,
-- and for one occurrence of a recurring event its RECURRENCE-ID.
local function event_id(o)
	local ev = o.ev
	local id = ev.source.uid .. ":" .. (ev.uid or "")
	local rid = ev.rid
	if not rid and ev.rule then
		if ev.allday then
			rid = os.date("%Y%m%d", day_start(o.s))
		elseif ev.utc then
			rid = os.date("!%Y%m%dT%H%M%SZ", o.s)
		else
			rid = os.date("%Y%m%dT%H%M%S", o.s)
		end
	end
	return rid and (id .. ":" .. rid) or id
end

-- Runs `argv` with the placeholders filled in for occurrence `o`.
local function run_for(what, argv, o)
	local ev = o.ev
	local start = ev.allday and day_start(o.s) or o.s
	local values = {
		title = ev.title,
		url = link_of(ev) or "",
		uid = ev.uid or "",
		location = ev.location or "",
		start = os.date("%Y-%m-%dT%H:%M:%S", start),
		date = os.date("%Y-%m-%d", start),
		calendar = ev.source.uid,
		event = event_id(o),
	}
	local filled = {}
	for i, a in ipairs(argv) do
		filled[i] = (a:gsub("{(%w+)}", function(k)
			return values[k]
		end))
	end
	local ok, err = dock.run(filled)
	if not ok then
		print(what .. ": " .. tostring(err))
	end
end

local function open_event(o)
	local link = link_of(o.ev)
	if open_argv then
		run_for("open", open_argv, o)
	elseif link then
		run_for("xdg-open", { "xdg-open", link }, o)
	end
end

-- Popup ------------------------------------------------------------------------

local function weekday_name(wd)
	-- 2023-01-01 was a Sunday.
	return fdate("%a", day_start(days_from_civil(2023, 1, 1) + wd))
end

local function header(t)
	local ty, tm = civil(t)
	local away = ty ~= view_y or tm ~= view_m or selected ~= t
	return ui.row({
		align = "center",
		gap = 4,
		ui.text({
			fdate("%B %Y", day_start(days_from_civil(view_y, view_m, 1))),
			style = "title",
			grow = 1,
			max_lines = 1,
		}),
		away and ui.button({ key = "today", label = "Today", variant = "ghost", on_click = go_today }) or nil,
		ui.button({
			key = "prev",
			icon = "go-previous-symbolic",
			variant = "ghost",
			tooltip = "Previous month",
			on_click = function()
				page(-1)
			end,
		}),
		ui.button({
			key = "next",
			icon = "go-next-symbolic",
			variant = "ghost",
			tooltip = "Next month",
			on_click = function()
				page(1)
			end,
		}),
	})
end

local function day_cell(d, list, t)
	local _, m, dd = civil(d)
	local sel, now = d == selected, d == t
	local color = sel and theme.accent_text or now and theme.accent or m == view_m and theme.text or theme.muted
	local dots = { gap = 2, justify = "center", height = 4 }
	for i, o in ipairs(list or {}) do
		if i > 3 then
			break
		end
		dots[#dots + 1] = ui.box({
			width = 4,
			height = 4,
			radius = 2,
			background = sel and theme.accent_text or o.ev.source.color,
		})
	end
	return ui.col({
		key = "day" .. d,
		align = "center",
		gap = 2,
		padding = { 4, 0 },
		radius = 8,
		background = sel and theme.accent or nil,
		border = (now and not sel) and { width = 1, color = theme.accent } or nil,
		hover = not sel and { background = theme.hover } or nil,
		on_click = function()
			select_day(d)
		end,
		ui.text({ dd, color = color, weight = (sel or now) and "bold" or "normal" }),
		ui.row(dots),
	})
end

-- The first day the month grid shows.
local function grid_start()
	local first = days_from_civil(view_y, view_m, 1)
	return first - (weekday(first) - week_start) % 7
end

local function month_grid(t, days)
	local start = grid_start()
	local columns = {}
	if week_numbers then
		columns[1] = "auto"
	end
	for _ = 1, 7 do
		columns[#columns + 1] = "1fr"
	end
	local cells = { columns = columns, gap = 2, row_gap = 2, width = "100%" }
	local function add(n)
		cells[#cells + 1] = n
	end
	if week_numbers then
		add(ui.text({ "", style = "caption" }))
	end
	for i = 0, 6 do
		add(
			ui.text({
				weekday_name((week_start + i) % 7),
				style = "caption",
				color = theme.muted,
				align = "center",
				max_lines = 1,
			})
		)
	end
	for w = 0, 5 do
		local ws = start + w * 7
		if week_numbers then
			-- The ISO week of the row's Monday.
			local monday = ws + (1 - week_start) % 7
			add(
				ui.col({
					align = "center",
					padding = { 4, 0 },
					ui.text({ tonumber(os.date("%V", day_start(monday))), style = "caption", color = theme.muted }),
				})
			)
		end
		for i = 0, 6 do
			add(day_cell(ws + i, days[ws + i], t))
		end
	end
	return ui.grid(cells)
end

local function event_row(o, d, i, now)
	local ev = o.ev
	local sub = when_on(o, d)
	if ev.location then
		sub = sub .. " · " .. ev.location
	end
	return ui.row({
		key = "ev" .. d .. "-" .. i,
		gap = 8,
		padding = { 4, 6 },
		radius = 6,
		align = "stretch",
		opacity = is_past(o, now) and 0.5 or nil,
		hover = { background = theme.hover },
		on_click = function()
			detail, copied = o, nil
			dock.redraw()
		end,
		ui.box({ width = 3, radius = 2, background = ev.source.color }),
		ui.col({
			grow = 1,
			gap = 1,
			ui.text({ ev.title, max_lines = 1 }),
			ui.text({ sub, style = "caption", color = theme.muted, max_lines = 1 }),
		}),
	})
end

local function agenda(t, days)
	local now = os.time()
	local items = { gap = 10 }
	local shown, hidden = 0, 0
	for d = selected, selected + agenda_days - 1 do
		local list = days[d]
		if list and shown >= AGENDA_MAX then
			hidden = hidden + #list
		elseif list then
			sorted(list)
			local col = {
				gap = 2,
				ui.text({
					day_title(d, t),
					style = "caption",
					weight = "bold",
					color = d == t and theme.accent or theme.muted,
					max_lines = 1,
				}),
			}
			for i, o in ipairs(list) do
				if shown >= AGENDA_MAX then
					hidden = hidden + 1
				else
					col[#col + 1] = event_row(o, d, i, now)
					shown = shown + 1
				end
			end
			items[#items + 1] = ui.col(col)
		end
	end
	if hidden > 0 then
		items[#items + 1] = ui.text({ string.format("%d more – pick a later day", hidden), style = "muted" })
	end
	if #items == 0 then
		local text
		if problem then
			text = problem
		elseif loading() and #events == 0 then
			text = "Loading…"
		elseif #order == 0 then
			text = "No calendars. Add one in GNOME Calendar."
		elseif agenda_days == 1 then
			text = "No events"
		else
			text = string.format("No events in the next %d days", agenda_days)
		end
		items[1] = ui.text({ text, style = "muted" })
	end
	return ui.scroll({ key = "agenda", max_height = 260, ui.col(items) })
end

local function details()
	local o = detail
	local ev = o.ev
	local link = link_of(ev)
	local rows = {
		gap = 10,
		ui.row({
			gap = 6,
			align = "start",
			ui.button({
				key = "back",
				icon = "go-previous-symbolic",
				variant = "ghost",
				tooltip = "Back",
				on_click = function()
					detail = nil
					dock.redraw()
				end,
			}),
			ui.col({
				grow = 1,
				gap = 2,
				ui.text({ ev.title, style = "title", max_lines = 3 }),
				ui.text({ when_long(o), style = "muted", max_lines = 2 }),
			}),
		}),
	}
	local function line(lead, text, lines)
		rows[#rows + 1] = ui.row({
			gap = 8,
			align = "start",
			ui.stack({ width = 16, height = 16, lead }),
			ui.text({ text, grow = 1, max_lines = lines }),
		})
	end
	line(ui.box({ width = 10, height = 10, radius = 5, background = ev.source.color }), ev.source.name, 1)
	if ev.location then
		line(ui.icon({ "mark-location-symbolic", size = 16 }), ev.location, 3)
	end
	if ev.rule then
		line(ui.icon({ "media-playlist-repeat-symbolic", size = 16 }), repeats(ev.rule), 2)
	end
	if ev.description then
		rows[#rows + 1] = ui.col({
			padding = 8,
			radius = 8,
			background = theme.inset,
			ui.scroll({ key = "description", max_height = 160, ui.text({ ev.description, style = "muted" }) }),
		})
	end
	rows[#rows + 1] = ui.row({
		gap = 8,
		justify = "end",
		ui.button({
			key = "copy",
			icon = "edit-copy-symbolic",
			label = copied == o and "Copied" or "Copy",
			on_click = function()
				local text = ev.title .. "\n" .. when_long(o)
				if ev.location then
					text = text .. "\n" .. ev.location
				end
				if link then
					text = text .. "\n" .. link
				end
				dock.clipboard(text)
				copied = o
				dock.redraw()
			end,
		}),
		calendar_argv and ui.button({
			key = "calendar",
			icon = "x-office-calendar-symbolic",
			tooltip = "Show in Calendar",
			on_click = function()
				run_for("calendar", calendar_argv, o)
			end,
		}) or nil,
		(open_argv or link) and ui.button({
			key = "open",
			label = "Open",
			variant = "filled",
			icon = "external-link-symbolic",
			tooltip = link,
			on_click = function()
				open_event(o)
			end,
		}) or nil,
	})
	return ui.col(rows)
end

function P.popup()
	if not show_popup then
		return nil
	end
	local t = today()
	if not selected then
		selected = t
		view_y, view_m = civil(t)
	end
	local width = week_numbers and 330 or 300
	if detail then
		return ui.col({ padding = 14, width = width, details() })
	end
	-- One query for the grid and the agenda when they are close together.
	local gs, ae = grid_start(), selected + agenda_days
	local grid_days, list_days
	if selected >= gs - 42 and selected <= gs + 42 then
		grid_days = query(math.min(gs, selected), math.max(gs + 42, ae))
		list_days = grid_days
	else
		grid_days, list_days = query(gs, gs + 42), query(selected, ae)
	end
	return ui.col({
		padding = 14,
		gap = 10,
		width = width,
		header(t),
		month_grid(t, grid_days),
		ui.divider({}),
		agenda(t, list_days),
	})
end

function P.on_open()
	is_open = true
	-- Come back to today after the popup has been closed for a while.
	if not selected or not closed_at or os.time() - closed_at > 120 then
		selected = today()
		view_y, view_m = civil(selected)
		detail = nil
	end
	refresh()
	dock.redraw()
end

function P.on_close()
	is_open, closed_at, copied = false, os.time(), nil
end

-- Icon and label -----------------------------------------------------------------

function P.icon()
	if not face then
		return nil
	end
	local t = today()
	local y, m, d = civil(t)
	local month = fdate("%b", day_start(t)):upper()
	local list = query(t, t + 1)[t] or {}
	local now = os.time()
	local left = {}
	for _, o in ipairs(list) do
		if not is_past(o, now) and #left < 3 then
			left[#left + 1] = o.ev.source.color
		end
	end
	return ui.canvas({
		draw = function(c)
			c:rect({ x = 8, y = 8, w = 84, h = 84, radius = 18, fill = "#f8f8f8" })
			c:path({
				d = "M 8 34 L 8 26 A 18 18 0 0 1 26 8 L 74 8 A 18 18 0 0 1 92 26 L 92 34 Z",
				fill = "#ff3b30",
			})
			c:text({ month, x = 50, y = 21, size = 14, bold = true, align = "center", color = "#ffffff" })
			c:text({
				d,
				x = 50,
				y = #left > 0 and 59 or 62,
				size = 38,
				bold = true,
				align = "center",
				color = "#1c1c1e",
			})
			local x = 50 - (#left - 1) * 5
			for i, color in ipairs(left) do
				c:circle({ x = x + (i - 1) * 10, y = 82, r = 3, fill = color })
			end
		end,
	})
end

function P.label()
	if not show_label then
		return nil
	end
	local now = os.time()
	local text = fdate(label_format, now)
	local o = next_event()
	if o then
		text = text .. " · " .. (o.s <= now and "Now: " or (ftime(o.s) .. " ")) .. o.ev.title
	end
	return text
end

-- What the label and icon depend on besides the events: the day, the next
-- event, and the events of today still to come.
local function current_signature()
	local t, now = today(), os.time()
	local parts = { t }
	local o = next_event()
	parts[#parts + 1] = o and (o.s .. o.ev.title) or ""
	for _, x in ipairs(query(t, t + 1)[t] or {}) do
		parts[#parts + 1] = is_past(x, now) and "p" or "f"
	end
	return table.concat(parts, ":")
end

local function tick()
	local sig = current_signature()
	-- Redraw when something shown changed, and every minute while the popup
	-- is open so past events fade.
	if sig ~= signature or is_open then
		signature = sig
		dock.redraw()
	end
end

-- Setup ----------------------------------------------------------------------------

local WEEK_STARTS = { sunday = 0, monday = 1, saturday = 6 }

-- An argv option: a list of strings, or a string split at spaces.
local function argv_option(v)
	local argv = {}
	if type(v) == "table" then
		for _, a in ipairs(v) do
			argv[#argv + 1] = tostring(a)
		end
	elseif type(v) == "string" then
		for a in v:gmatch("%S+") do
			argv[#argv + 1] = a
		end
	end
	return #argv > 0 and argv or nil
end

function P.setup(opts)
	if type(opts.week_start) == "string" and WEEK_STARTS[opts.week_start:lower()] then
		week_start = WEEK_STARTS[opts.week_start:lower()]
	end
	if type(opts.week_numbers) == "boolean" then
		week_numbers = opts.week_numbers
	end
	if type(opts.agenda_days) == "number" then
		agenda_days = math.max(1, math.min(366, math.floor(opts.agenda_days)))
	end
	if type(opts.format) == "string" then
		label_format = opts.format
	end
	if type(opts.time) == "string" then
		time_format = opts.time
	end
	if type(opts.face) == "boolean" then
		face = opts.face
	end
	if type(opts.label) == "boolean" then
		show_label = opts.label
	end
	if type(opts.popup) == "boolean" then
		show_popup = opts.popup
	end
	if opts.open ~= nil then
		open_argv = argv_option(opts.open)
	end
	if opts.calendar_app == false then
		calendar_argv = nil
	elseif opts.calendar_app ~= nil then
		calendar_argv = argv_option(opts.calendar_app)
	end
	if opts.sources ~= nil then
		print("`sources` is no longer read: the calendars come from Evolution Data Server")
	end
	local list = opts.calendars
	if type(list) == "string" then
		list = { list }
	end
	if type(list) == "table" and #list > 0 then
		wanted = {}
		for _, n in ipairs(list) do
			n = tostring(n)
			wanted[n], wanted[n:lower()] = true, true
		end
	end

	connect()
	dock.every(30 * 1000, tick)
end

return P
