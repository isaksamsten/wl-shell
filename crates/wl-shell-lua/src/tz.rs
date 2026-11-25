//! `dock.tz`: time zones, from the system's time zone database (jiff),
//! for what `os.time` and `os.date` can only do in the local zone.
//!
//! ```lua
//! dock.tz.find(name)       -- 'Europe/Berlin', or nil
//! dock.tz.system()         -- the system's zone, e.g. 'Europe/Stockholm', or nil
//! dock.tz.time(zone, {year =, month =, day =, hour =, min =, sec =})
//!                          -- seconds since the epoch, or nil and the error
//! dock.tz.date(zone, t)    -- {year, month, day, hour, min, sec, wday, yday,
//!                          --  isdst, offset, abbrev}, or nil and the error
//! dock.tz.vtimezone(text)  -- an iCalendar VTIMEZONE's current rules as a
//!                          -- POSIX TZ string, or nil and the error
//! ```
//!
//! - A zone is an IANA name (`Europe/Stockholm`, `UTC`), a Windows one
//!   as in Outlook and Exchange invites (`W. Europe Standard Time`),
//!   either after a prefix, as libical writes them
//!   (`/freeassociation.sourceforge.net/Europe/Oslo`), or a POSIX TZ
//!   string (`CET-1CEST,M3.5.0,M10.5.0/3`). `find` gives the IANA name a
//!   zone goes by (a POSIX string is its own); `nil` or `'local'` in
//!   `time` and `date` is the system's zone.
//! - `time` takes a wall-clock time (`hour`, `min` and `sec` default to
//!   0; nothing is normalized: the 32nd is an error). A time a DST change
//!   skips is moved forward by the change, and a time that happens twice
//!   is the first: RFC 5545's rule, and `os.time`'s.
//! - `date` is `os.date('*t')` in a zone (`wday` 1 is Sunday), with the
//!   offset east of UTC in seconds and the zone's abbreviation (`CEST`).
//! - `vtimezone` is for zones only an invite defines, such as Outlook's
//!   "(UTC+01:00) Amsterdam, Berlin, ...": it takes the latest STANDARD
//!   and DAYLIGHT parts, with yearly rules by weekday as POSIX has them.
//!   Past rules are left out, so old times may be off by them.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use jiff::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use mlua::{Lua, Table, Value};

use crate::runtime::lock;
use crate::windows_zones::WINDOWS_ZONES;

/// Names looked up, kept up to this many.
const CACHE: usize = 256;

/// `name` as a zone: an IANA name, a Windows one, or either after a
/// prefix.
fn resolve(name: &str) -> Option<TimeZone> {
    let name = name.trim().trim_matches('"');
    if name.is_empty() {
        return None;
    }
    if let Ok(tz) = TimeZone::get(name) {
        return Some(tz);
    }
    let windows = |name: &str| {
        (WINDOWS_ZONES.iter())
            .find(|(w, _)| w.eq_ignore_ascii_case(name))
            .and_then(|(_, iana)| TimeZone::get(iana).ok())
    };
    if let Some(tz) = windows(name) {
        return Some(tz);
    }
    if let Ok(tz) = TimeZone::posix(name) {
        return Some(tz);
    }
    // The longest name after a slash that is one.
    (name.match_indices('/'))
        .map(|(i, _)| &name[i + 1..])
        .filter(|rest| !rest.is_empty())
        .find_map(|rest| TimeZone::get(rest).ok().or_else(|| windows(rest)))
}

/// A part of a VTIMEZONE: STANDARD or DAYLIGHT.
#[derive(Default)]
struct Part {
    start: String,
    to: Option<i32>,
    rrule: Option<String>,
    name: Option<String>,
}

/// "+0100", "-0530" or "+011212" in seconds east of UTC.
fn utc_offset(v: &str) -> Option<i32> {
    let (sign, digits) = match v.as_bytes().first()? {
        b'+' => (1, &v[1..]),
        b'-' => (-1, &v[1..]),
        _ => return None,
    };
    if !(digits.len() == 4 || digits.len() == 6) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |i: usize| digits.get(i..i + 2).map_or(Ok(0), str::parse::<i32>);
    Some(sign * (n(0).ok()? * 3600 + n(2).ok()? * 60 + n(4).ok()?))
}

/// `secs` as POSIX writes a time or an offset: h[:mm[:ss]].
fn hms(secs: i32) -> String {
    let (sign, s) = if secs < 0 { ("-", -secs) } else { ("", secs) };
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    match (m, s) {
        (0, 0) => format!("{sign}{h}"),
        (_, 0) => format!("{sign}{h}:{m:02}"),
        _ => format!("{sign}{h}:{m:02}:{s:02}"),
    }
}

/// A part's abbreviation as POSIX takes it: its TZNAME if it is letters,
/// else its offset (`<+01>`).
fn abbrev(part: &Part, offset: i32) -> String {
    match &part.name {
        Some(n) if n.len() >= 3 && n.bytes().all(|b| b.is_ascii_alphabetic()) => n.clone(),
        _ => {
            let (sign, s) = if offset < 0 {
                ('-', -offset)
            } else {
                ('+', offset)
            };
            match s % 3600 / 60 {
                0 => format!("<{sign}{:02}>", s / 3600),
                m => format!("<{sign}{:02}{m:02}>", s / 3600),
            }
        }
    }
}

/// When a part's yearly rule changes the clocks, as POSIX writes it:
/// `Mm.w.d/time`, the time in the wall-clock time before the change.
fn rule(part: &Part) -> Result<String, String> {
    let rrule = part.rrule.as_deref().ok_or("no RRULE")?;
    let (mut freq, mut month, mut byday, mut monthday) = (None, None, None, None);
    for kv in rrule.split(';') {
        match kv.split_once('=') {
            Some((k, v)) if k.eq_ignore_ascii_case("FREQ") => freq = Some(v.to_ascii_uppercase()),
            Some((k, v)) if k.eq_ignore_ascii_case("BYMONTH") => month = v.parse::<u8>().ok(),
            Some((k, v)) if k.eq_ignore_ascii_case("BYDAY") => byday = Some(v.to_ascii_uppercase()),
            Some((k, v)) if k.eq_ignore_ascii_case("BYMONTHDAY") => {
                monthday = v.split(',').next().and_then(|d| d.parse::<i8>().ok())
            }
            Some((k, v)) if k.eq_ignore_ascii_case("UNTIL") => {
                return Err(format!("the rule ended ({v})"));
            }
            _ => {}
        }
    }
    if freq.as_deref() != Some("YEARLY") {
        return Err(format!("not a yearly rule: {rrule}"));
    }
    let month = month.filter(|m| (1..=12).contains(m)).ok_or("no BYMONTH")?;
    let byday = byday.ok_or("no BYDAY")?;
    let (ord, day) = byday.split_at(byday.len().saturating_sub(2));
    let day = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"]
        .iter()
        .position(|d| *d == day)
        .ok_or_else(|| format!("BYDAY={byday}"))?;
    // The week: an ordinal (-1 the last), or the first of seven days of
    // the month ("the Sunday of the 8th to the 14th").
    let week = match (ord.parse::<i8>(), monthday) {
        (Ok(-1), _) => 5,
        (Ok(n @ 1..=5), _) => n,
        (Err(_), Some(d @ (1 | 8 | 15 | 22))) => (d + 6) / 7,
        (Err(_), Some(d)) if d >= 25 || d == -7 => 5,
        _ => return Err(format!("BYDAY={byday}")),
    };
    // DTSTART's time of day: hhmmss after the T.
    let time = (part.start.split_once('T'))
        .and_then(|(_, t)| {
            let n = |i: usize| t.get(i..i + 2)?.parse::<i32>().ok();
            Some(n(0)? * 3600 + n(2)? * 60 + n(4).unwrap_or(0))
        })
        .unwrap_or(7200);
    Ok(format!("M{month}.{week}.{day}/{}", hms(time)))
}

/// A VTIMEZONE's current rules as a POSIX TZ string.
fn vtimezone(text: &str) -> Result<String, String> {
    let unfolded = text
        .replace("\r\n ", "")
        .replace("\r\n\t", "")
        .replace("\n ", "");
    let (mut standard, mut daylight): (Vec<Part>, Vec<Part>) = (Vec::new(), Vec::new());
    let mut cur: Option<(bool, Part)> = None;
    for line in unfolded.lines() {
        let line = line.trim_end_matches('\r');
        let Some((head, value)) = line.split_once(':') else {
            continue;
        };
        let name = head.split(';').next().unwrap_or(head).to_ascii_uppercase();
        let value = value.trim();
        match (name.as_str(), &mut cur) {
            ("BEGIN", None) if value.eq_ignore_ascii_case("STANDARD") => {
                cur = Some((false, Part::default()))
            }
            ("BEGIN", None) if value.eq_ignore_ascii_case("DAYLIGHT") => {
                cur = Some((true, Part::default()))
            }
            ("END", Some(_)) => {
                let (is_daylight, part) = cur.take().unwrap();
                if is_daylight {
                    &mut daylight
                } else {
                    &mut standard
                }
                .push(part);
            }
            ("DTSTART", Some((_, p))) => p.start = value.to_string(),
            ("TZOFFSETTO", Some((_, p))) => p.to = utc_offset(value),
            ("RRULE", Some((_, p))) => p.rrule = Some(value.to_string()),
            ("TZNAME", Some((_, p))) => p.name = Some(value.to_string()),
            _ => {}
        }
    }
    // The parts in effect now: those that started last.
    let latest = |parts: Vec<Part>| parts.into_iter().max_by(|a, b| a.start.cmp(&b.start));
    let (standard, daylight) = (latest(standard), latest(daylight));
    let posix = match (&standard, &daylight) {
        (None, None) => return Err("no STANDARD or DAYLIGHT".into()),
        (Some(s), Some(d)) if rule(s).is_ok() && rule(d).is_ok() => {
            let (so, d_o) = (s.to.ok_or("no TZOFFSETTO")?, d.to.ok_or("no TZOFFSETTO")?);
            let dst = if d_o - so == 3600 {
                String::new()
            } else {
                hms(-d_o)
            };
            format!(
                "{}{}{}{dst},{},{}",
                abbrev(s, so),
                hms(-so),
                abbrev(d, d_o),
                rule(d)?,
                rule(s)?
            )
        }
        // No DST (now): the offset of the part that started last.
        _ => {
            let part = match (&standard, &daylight) {
                (Some(s), Some(d)) if d.start > s.start => d,
                (Some(s), _) => s,
                (None, Some(d)) => d,
                (None, None) => unreachable!(),
            };
            let o = part.to.ok_or("no TZOFFSETTO")?;
            format!("{}{}", abbrev(part, o), hms(-o))
        }
    };
    TimeZone::posix(&posix).map_err(|e| format!("{posix}: {e}"))?;
    Ok(posix)
}

/// The zone `name` names, remembered.
fn find(name: &str) -> Option<TimeZone> {
    static ZONES: OnceLock<Mutex<HashMap<String, Option<TimeZone>>>> = OnceLock::new();
    let zones = ZONES.get_or_init(Default::default);
    if let Some(tz) = lock(zones).get(name) {
        return tz.clone();
    }
    let tz = resolve(name);
    let mut zones = lock(zones);
    if zones.len() >= CACHE {
        zones.clear();
    }
    zones.insert(name.to_string(), tz.clone());
    tz
}

/// The zone a `zone` argument names: nil and 'local' are the system's.
fn zone(name: Option<&str>) -> Result<TimeZone, String> {
    match name {
        None | Some("local") => Ok(TimeZone::system()),
        Some(n) => find(n).ok_or_else(|| format!("unknown time zone {n:?}")),
    }
}

/// The seconds since the epoch of wall-clock time `t` in `tz`.
fn time(tz: &TimeZone, t: &Table) -> mlua::Result<Result<i64, String>> {
    let field = |k: &str, default: Option<i64>| -> mlua::Result<i64> {
        match t.get::<Option<i64>>(k)? {
            Some(v) => Ok(v),
            None => default.ok_or_else(|| mlua::Error::runtime(format!("tz.time: no {k}"))),
        }
    };
    let (year, month, day) = (
        field("year", None)?,
        field("month", None)?,
        field("day", None)?,
    );
    let (hour, min, sec) = (
        field("hour", Some(0))?,
        field("min", Some(0))?,
        field("sec", Some(0))?,
    );
    let small = |v: i64| i8::try_from(v).map_err(|_| format!("{v} is out of range"));
    Ok((|| {
        let year = i16::try_from(year).map_err(|_| format!("year {year} is out of range"))?;
        let dt = DateTime::new(
            year,
            small(month)?,
            small(day)?,
            small(hour)?,
            small(min)?,
            small(sec)?,
            0,
        )
        .map_err(|e| e.to_string())?;
        let ts = tz.to_timestamp(dt).map_err(|e| e.to_string())?;
        Ok(ts.as_second())
    })())
}

/// The wall clock in `tz` at `t`, as `os.date('*t')` gives it, and more.
fn date(lua: &Lua, tz: &TimeZone, t: f64) -> mlua::Result<Result<Table, String>> {
    if !t.is_finite() {
        return Ok(Err(format!("{t} is not a time")));
    }
    let ts = match Timestamp::from_second(t.floor() as i64) {
        Ok(ts) => ts,
        Err(e) => return Ok(Err(e.to_string())),
    };
    let z = ts.to_zoned_datetime(tz.clone());
    let info = tz.to_offset_info(ts);
    let out = lua.create_table()?;
    out.set("year", z.year())?;
    out.set("month", z.month())?;
    out.set("day", z.day())?;
    out.set("hour", z.hour())?;
    out.set("min", z.minute())?;
    out.set("sec", z.second())?;
    out.set("wday", z.weekday().to_sunday_one_offset())?;
    out.set("yday", z.day_of_year())?;
    out.set("isdst", info.dst().is_dst())?;
    out.set("offset", info.offset().seconds())?;
    out.set("abbrev", info.abbreviation())?;
    Ok(Ok(out))
}

/// `ok` as Lua's value-or-nil-and-error.
fn answer<T: mlua::IntoLua>(
    lua: &Lua,
    r: Result<T, String>,
) -> mlua::Result<(Value, Option<String>)> {
    match r {
        Ok(v) => Ok((v.into_lua(lua)?, None)),
        Err(e) => Ok((Value::Nil, Some(e))),
    }
}

/// Add `ns.tz`.
pub(crate) fn install(lua: &Lua, ns: &Table) -> mlua::Result<()> {
    let tz = lua.create_table()?;
    tz.set(
        "find",
        lua.create_function(|_, name: String| {
            Ok(find(&name).map(|tz| tz.iana_name().map_or(name.clone(), str::to_string)))
        })?,
    )?;
    tz.set(
        "system",
        lua.create_function(|_, ()| Ok(TimeZone::system().iana_name().map(str::to_string)))?,
    )?;
    tz.set(
        "time",
        lua.create_function(|lua, (name, t): (Option<String>, Table)| {
            let r = match zone(name.as_deref()) {
                Ok(tz) => time(&tz, &t)?,
                Err(e) => Err(e),
            };
            answer(lua, r)
        })?,
    )?;
    tz.set(
        "date",
        lua.create_function(|lua, (name, t): (Option<String>, f64)| {
            let r = match zone(name.as_deref()) {
                Ok(tz) => date(lua, &tz, t)?,
                Err(e) => Err(e),
            };
            answer(lua, r)
        })?,
    )?;
    tz.set(
        "vtimezone",
        lua.create_function(|lua, text: String| answer(lua, vtimezone(&text)))?,
    )?;
    ns.set("tz", tz)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lua() -> Lua {
        let lua = Lua::new();
        let ns = lua.create_table().unwrap();
        install(&lua, &ns).unwrap();
        lua.globals()
            .set("tz", ns.get::<Table>("tz").unwrap())
            .unwrap();
        lua
    }

    fn eval<T: mlua::FromLuaMulti>(lua: &Lua, code: &str) -> T {
        lua.load(code)
            .eval()
            .unwrap_or_else(|e| panic!("{code}: {e}"))
    }

    fn at(s: &str) -> i64 {
        s.parse::<Timestamp>().unwrap().as_second()
    }

    #[test]
    fn zones_are_found_by_their_names() {
        let lua = lua();
        let find = |name: &str| -> Option<String> {
            let f: mlua::Function = eval(&lua, "return tz.find");
            f.call(name).unwrap()
        };
        assert_eq!(
            find("Europe/Stockholm").as_deref(),
            Some("Europe/Stockholm")
        );
        assert_eq!(find("UTC").as_deref(), Some("UTC"));
        // Windows names, ignoring case.
        assert_eq!(
            find("W. Europe Standard Time").as_deref(),
            Some("Europe/Berlin")
        );
        assert_eq!(
            find("eastern standard time").as_deref(),
            Some("America/New_York")
        );
        // After libical's prefixes.
        assert_eq!(
            find("/freeassociation.sourceforge.net/Europe/Oslo").as_deref(),
            Some("Europe/Oslo")
        );
        assert_eq!(
            find("/citadel.org/20190914_1/America/Argentina/Buenos_Aires").as_deref(),
            Some("America/Argentina/Buenos_Aires")
        );
        assert_eq!(find("Nowhere/Else"), None);
        assert_eq!(find(""), None);
    }

    #[test]
    fn wall_clock_times_become_instants() {
        let lua = lua();
        let t: i64 = eval(
            &lua,
            "return tz.time('Europe/Stockholm', {year = 2026, month = 7, day = 1, hour = 12})",
        );
        assert_eq!(t, at("2026-07-01T10:00:00Z"));
        let t: i64 = eval(
            &lua,
            "return tz.time('Eastern Standard Time', {year = 2026, month = 1, day = 15, hour = 9, min = 30, sec = 5})",
        );
        assert_eq!(t, at("2026-01-15T14:30:05Z"));
        // Skipped by the change to summer time: moved forward by it.
        let t: i64 = eval(
            &lua,
            "return tz.time('Europe/Stockholm', {year = 2026, month = 3, day = 29, hour = 2, min = 30})",
        );
        assert_eq!(t, at("2026-03-29T01:30:00Z"));
        // Twice, in the change back: the first.
        let t: i64 = eval(
            &lua,
            "return tz.time('Europe/Stockholm', {year = 2026, month = 10, day = 25, hour = 2, min = 30})",
        );
        assert_eq!(t, at("2026-10-25T00:30:00Z"));
        // Errors are nil and the error.
        let (v, e): (Option<i64>, Option<String>) = eval(
            &lua,
            "return tz.time('Nowhere/Else', {year = 2026, month = 1, day = 1})",
        );
        assert_eq!(v, None);
        assert!(e.unwrap().contains("unknown time zone"));
        let (v, e): (Option<i64>, Option<String>) = eval(
            &lua,
            "return tz.time('UTC', {year = 2026, month = 2, day = 30})",
        );
        assert_eq!(v, None);
        assert!(e.is_some());
        // A missing field is a mistake.
        assert!(
            lua.load("return tz.time('UTC', {year = 2026, month = 2})")
                .eval::<Value>()
                .is_err()
        );
    }

    #[test]
    fn instants_have_a_wall_clock_in_each_zone() {
        let lua = lua();
        lua.globals().set("t", at("2026-07-01T10:00:00Z")).unwrap();
        let (desc, n): (String, i64) = eval(
            &lua,
            "local d = tz.date('Europe/Stockholm', t)
             return string.format('%d-%02d-%02d %02d:%02d:%02d %s %s %d',
               d.year, d.month, d.day, d.hour, d.min, d.sec, d.abbrev, tostring(d.isdst), d.offset), d.wday * 1000 + d.yday",
        );
        assert_eq!(desc, "2026-07-01 12:00:00 CEST true 7200");
        // A Wednesday (Sunday is 1), the 182nd day.
        assert_eq!(n, 4182);
        let hour: i64 = eval(&lua, "return tz.date('America/New_York', t + 0.5).hour");
        assert_eq!(hour, 6);
        // Round trips through `time`.
        let back: i64 = eval(
            &lua,
            "return tz.time('Asia/Kolkata', tz.date('Asia/Kolkata', t))",
        );
        assert_eq!(back, at("2026-07-01T10:00:00Z"));
        let (v, e): (Option<Table>, Option<String>) =
            eval(&lua, "return tz.date('Nowhere/Else', t)");
        assert!(v.is_none() && e.is_some());
    }

    /// Outlook's, as it sends it in invites.
    const OUTLOOK: &str = "BEGIN:VTIMEZONE\r\nTZID:(UTC+01:00) Amsterdam\\, Berlin\\, Bern\\, Rome\\, Stockholm\\, Vienna\r\n\
        BEGIN:STANDARD\r\nDTSTART:16010101T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\n\
        RRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=10\r\nEND:STANDARD\r\n\
        BEGIN:DAYLIGHT\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\n\
        RRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=3\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n";

    #[test]
    fn vtimezones_become_posix_zones() {
        assert_eq!(
            vtimezone(OUTLOOK).unwrap(),
            "<+01>-1<+02>,M3.5.0/2,M10.5.0/3"
        );
        // libical's, with its history: the rules in effect now, and names.
        let stockholm = "BEGIN:VTIMEZONE\nTZID:Europe/Stockholm\n\
            BEGIN:STANDARD\nTZNAME:CET\nTZOFFSETFROM:+0200\nTZOFFSETTO:+0100\nDTSTART:19961027T030000\n\
            RRULE:FREQ=YEARLY;UNTIL=20361026T010000Z;BYDAY=-1SU;BYMONTH=10\nEND:STANDARD\n\
            BEGIN:STANDARD\nTZNAME:CET\nTZOFFSETFROM:+0200\nTZOFFSETTO:+0100\nDTSTART:20371025T030000\n\
            RRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=10\nEND:STANDARD\n\
            BEGIN:DAYLIGHT\nTZNAME:CEST\nTZOFFSETFROM:+0100\nTZOFFSETTO:+0200\nDTSTART:20380328T020000\n\
            RRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=3\nEND:DAYLIGHT\nEND:VTIMEZONE\n";
        assert_eq!(
            vtimezone(stockholm).unwrap(),
            "CET-1CEST,M3.5.0/2,M10.5.0/3"
        );
        // US rules, by days of the month: the second Sunday of March.
        let us = "BEGIN:VTIMEZONE\nBEGIN:DAYLIGHT\nDTSTART:20070311T020000\nTZOFFSETTO:-0400\n\
            RRULE:FREQ=YEARLY;BYMONTH=3;BYMONTHDAY=8,9,10,11,12,13,14;BYDAY=SU\nEND:DAYLIGHT\n\
            BEGIN:STANDARD\nDTSTART:20071104T020000\nTZOFFSETTO:-0500\n\
            RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\nEND:STANDARD\nEND:VTIMEZONE\n";
        assert_eq!(vtimezone(us).unwrap(), "<-05>5<-04>,M3.2.0/2,M11.1.0/2");
        // No DST: one offset.
        let india = "BEGIN:VTIMEZONE\nBEGIN:STANDARD\nDTSTART:19700101T000000\nTZOFFSETTO:+0530\n\
            END:STANDARD\nEND:VTIMEZONE\n";
        assert_eq!(vtimezone(india).unwrap(), "<+0530>-5:30");
        // DST that ended: the offset since.
        let ended = "BEGIN:VTIMEZONE\nBEGIN:DAYLIGHT\nDTSTART:20100328T020000\nTZOFFSETTO:+0400\n\
            RRULE:FREQ=YEARLY;UNTIL=20140330T000000Z;BYDAY=-1SU;BYMONTH=3\nEND:DAYLIGHT\n\
            BEGIN:STANDARD\nDTSTART:20141026T020000\nTZOFFSETTO:+0300\nEND:STANDARD\nEND:VTIMEZONE\n";
        assert_eq!(vtimezone(ended).unwrap(), "<+03>-3");
        assert!(vtimezone("BEGIN:VTIMEZONE\nEND:VTIMEZONE\n").is_err());
    }

    #[test]
    fn posix_zones_work_as_zones() {
        let lua = lua();
        lua.globals().set("outlook", OUTLOOK).unwrap();
        let (summer, winter, abbrev): (i64, i64, String) = eval(
            &lua,
            "local z = tz.vtimezone(outlook)
             assert(tz.find(z) == z)
             return tz.time(z, {year = 2026, month = 7, day = 1, hour = 12}),
               tz.time(z, {year = 2026, month = 12, day = 1, hour = 12}),
               tz.date(z, tz.time(z, {year = 2026, month = 7, day = 1})).abbrev",
        );
        assert_eq!(summer, at("2026-07-01T10:00:00Z"));
        assert_eq!(winter, at("2026-12-01T11:00:00Z"));
        assert_eq!(abbrev, "+02");
    }
}
