//! When a note is due: the frontmatter's `due`, the presets that set it,
//! and `<when>` as the command line takes it. Pure but for reading the
//! clock and the local time zone (behind [`Zone`], so tests don't depend
//! on either), unit tested.
//!
//! `due` is a TOML date or datetime:
//!
//! - `due = 2026-11-20`: due at the start of that day, local time.
//! - `due = 2026-11-20T09:00`: local time.
//! - `due = 2026-11-20T09:00+01:00` (or `Z`): that very moment.
//!
//! A note is overdue from its due time on. Nothing else is accepted (not
//! a string, not a time of day alone): the note gets a warning instead.

use std::fmt;

use toml_edit::{Date, Datetime, Offset, Time};

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A time of day (`due.morning`, `due.evening`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeOfDay {
    pub hour: u8,
    pub minute: u8,
}

impl TimeOfDay {
    pub const fn new(hour: u8, minute: u8) -> Self {
        Self { hour, minute }
    }

    /// `HH:MM` (`H:MM` too).
    pub fn parse(s: &str) -> Result<Self, String> {
        let wrong = || format!("time '{s}': expected HH:MM");
        let (h, m) = s.trim().split_once(':').ok_or_else(wrong)?;
        let digits = |p: &str, n: usize| p.len() <= n && p.bytes().all(|b| b.is_ascii_digit());
        if !digits(h, 2) || m.len() != 2 || !digits(m, 2) {
            return Err(wrong());
        }
        let (hour, minute) = (
            h.parse::<u8>().map_err(|_| wrong())?,
            m.parse::<u8>().map_err(|_| wrong())?,
        );
        match hour < 24 && minute < 60 {
            true => Ok(Self { hour, minute }),
            false => Err(wrong()),
        }
    }
}

impl fmt::Display for TimeOfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

/// A date and time of day on the calendar, without a time zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Civil {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// Days since 1970-01-01 of a date on the proleptic Gregorian calendar.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date `days` after 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month as u8, day as u8)
}

impl Civil {
    pub const fn new(year: i32, month: u8, day: u8, hour: u8, minute: u8) -> Self {
        Self {
            year,
            month,
            day,
            hour,
            minute,
            second: 0,
        }
    }

    /// Days since 1970-01-01 of its date.
    pub fn days(&self) -> i64 {
        days_from_civil(self.year.into(), self.month.into(), self.day.into())
    }

    /// Its seconds since 1970-01-01 as if it were UTC.
    pub fn as_utc(&self) -> i64 {
        self.days() * 86_400
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
    }

    /// The date and time `secs` seconds after 1970-01-01 would be in UTC.
    pub fn from_utc(secs: i64) -> Self {
        let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
        let rest = secs.rem_euclid(86_400);
        Self {
            year: year as i32,
            month,
            day,
            hour: (rest / 3600) as u8,
            minute: (rest % 3600 / 60) as u8,
            second: (rest % 60) as u8,
        }
    }

    /// 0 for Monday to 6 for Sunday.
    pub fn weekday(&self) -> u8 {
        (self.days() + 3).rem_euclid(7) as u8
    }

    /// The same time of day, `n` days on the calendar later.
    pub fn plus_days(&self, n: i64) -> Self {
        let (year, month, day) = civil_from_days(self.days() + n);
        Self {
            year: year as i32,
            month,
            day,
            ..*self
        }
    }

    /// The same day, at `t`.
    pub fn at(&self, t: TimeOfDay) -> Self {
        Self {
            hour: t.hour,
            minute: t.minute,
            second: 0,
            ..*self
        }
    }

    /// The same day, at midnight.
    pub fn midnight(&self) -> Self {
        self.at(TimeOfDay::new(0, 0))
    }

    /// The same moment, without its seconds.
    fn minute_floor(&self) -> Self {
        Self { second: 0, ..*self }
    }
}

/// Between civil time and Unix time: a time zone.
pub trait Zone {
    /// The moment `civil` is in this zone (an hour that does not exist or
    /// happens twice is any of the moments near it).
    fn to_unix(&self, civil: Civil) -> i64;
    /// The civil time at the moment `unix`.
    fn to_civil(&self, unix: i64) -> Civil;
}

/// The system's local time zone.
#[derive(Clone, Copy, Debug, Default)]
pub struct Local;

impl Zone for Local {
    fn to_unix(&self, c: Civil) -> i64 {
        // SAFETY: mktime only reads and normalizes the tm it is given.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            tm.tm_year = c.year - 1900;
            tm.tm_mon = i32::from(c.month) - 1;
            tm.tm_mday = c.day.into();
            tm.tm_hour = c.hour.into();
            tm.tm_min = c.minute.into();
            tm.tm_sec = c.second.into();
            tm.tm_isdst = -1;
            libc::mktime(&mut tm) as i64
        }
    }

    fn to_civil(&self, unix: i64) -> Civil {
        // SAFETY: localtime_r only writes to the tm it is given.
        let tm = unsafe {
            let t = unix as libc::time_t;
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&t, &mut tm);
            tm
        };
        Civil {
            year: tm.tm_year + 1900,
            month: (tm.tm_mon + 1) as u8,
            day: tm.tm_mday as u8,
            hour: tm.tm_hour as u8,
            minute: tm.tm_min as u8,
            second: tm.tm_sec.min(59) as u8,
        }
    }
}

/// A zone a fixed number of minutes east of UTC.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fixed(pub i32);

impl Zone for Fixed {
    fn to_unix(&self, c: Civil) -> i64 {
        c.as_utc() - i64::from(self.0) * 60
    }

    fn to_civil(&self, unix: i64) -> Civil {
        Civil::from_utc(unix + i64::from(self.0) * 60)
    }
}

/// Now, in Unix seconds.
pub fn now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// A note's `due`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    /// A date: the start of that day, local time.
    Day(Civil),
    /// A local date and time.
    Local(Civil),
    /// A moment, as an offset datetime gave it.
    At(i64),
}

impl Due {
    /// What a frontmatter value says: a date or a datetime with a date.
    pub fn from_datetime(dt: &Datetime) -> Result<Self, String> {
        let wrong = || "due: expected a date or datetime, such as 2026-11-20 or 2026-11-20T09:00";
        let Some(date) = dt.date else {
            return Err(wrong().into());
        };
        let civil = |time: Option<Time>| Civil {
            year: date.year.into(),
            month: date.month,
            day: date.day,
            hour: time.map_or(0, |t| t.hour),
            minute: time.map_or(0, |t| t.minute),
            second: time.and_then(|t| t.second).map_or(0, |s| s.min(59)),
        };
        match (dt.time, dt.offset) {
            (None, None) => Ok(Due::Day(civil(None))),
            (Some(_), None) => Ok(Due::Local(civil(dt.time))),
            (Some(_), Some(off)) => {
                let minutes = match off {
                    Offset::Z => 0,
                    Offset::Custom { minutes } => i64::from(minutes),
                };
                Ok(Due::At(civil(dt.time).as_utc() - minutes * 60))
            }
            (None, Some(_)) => Err(wrong().into()),
        }
    }

    /// The moment it is due.
    pub fn at(&self, zone: &dyn Zone) -> i64 {
        match self {
            Due::Day(c) | Due::Local(c) => zone.to_unix(*c),
            Due::At(t) => *t,
        }
    }

    /// Whether it is due at `now`, or was.
    pub fn overdue(&self, zone: &dyn Zone, now: i64) -> bool {
        self.at(zone) <= now
    }

    /// Only a date was given.
    pub fn is_day(&self) -> bool {
        matches!(self, Due::Day(_))
    }

    /// As frontmatter writes it: a date, or a local datetime (an offset
    /// datetime is shown in local time).
    pub fn datetime(&self, zone: &dyn Zone) -> Datetime {
        let civil = match self {
            Due::Day(c) | Due::Local(c) => *c,
            Due::At(t) => zone.to_civil(*t),
        };
        let date = Some(Date {
            year: civil.year.clamp(0, 9999) as u16,
            month: civil.month,
            day: civil.day,
        });
        let time = (!self.is_day()).then_some(Time {
            hour: civil.hour,
            minute: civil.minute,
            second: (civil.second != 0).then_some(civil.second),
            nanosecond: None,
        });
        Datetime {
            date,
            time,
            offset: None,
        }
    }

    /// "Thu 20 Nov 09:00" (just "Thu 20 Nov" for a date), with the year
    /// when it isn't `now`'s. Absolute, so it never changes as time goes
    /// by.
    pub fn when(&self, zone: &dyn Zone, now: i64) -> String {
        let c = match self {
            Due::Day(c) | Due::Local(c) => *c,
            Due::At(t) => zone.to_civil(*t),
        };
        let mut out = format!(
            "{} {} {}",
            WEEKDAYS[c.weekday() as usize],
            c.day,
            MONTHS[(c.month as usize).clamp(1, 12) - 1]
        );
        if c.year != zone.to_civil(now).year {
            out.push_str(&format!(" {}", c.year));
        }
        if !self.is_day() {
            out.push_str(&format!(" {:02}:{:02}", c.hour, c.minute));
        }
        out
    }

    /// "Due Thu 20 Nov 09:00".
    pub fn label(&self, zone: &dyn Zone, now: i64) -> String {
        format!("Due {}", self.when(zone, now))
    }
}

/// The earliest moment after `now` that one of `dues` falls on, for the
/// timer to wait for.
pub fn next_due(dues: impl IntoIterator<Item = Due>, zone: &dyn Zone, now: i64) -> Option<i64> {
    dues.into_iter()
        .map(|d| d.at(zone))
        .filter(|&t| t > now)
        .min()
}

/// The ready-made choices for setting a due time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// In an hour.
    Hour,
    /// This evening, at `due.evening`.
    Tonight,
    /// The next day, at `due.morning`.
    Tomorrow,
    /// The next Monday, at `due.morning`.
    NextWeek,
}

impl Preset {
    pub const ALL: [Preset; 4] = [
        Preset::Hour,
        Preset::Tonight,
        Preset::Tomorrow,
        Preset::NextWeek,
    ];

    /// What it is called in full.
    pub fn name(self) -> &'static str {
        match self {
            Preset::Hour => "In an hour",
            Preset::Tonight => "Tonight",
            Preset::Tomorrow => "Tomorrow",
            Preset::NextWeek => "Next week",
        }
    }

    /// What it is called in the row of choices.
    pub fn short(self) -> &'static str {
        match self {
            Preset::Hour => "1h",
            Preset::Tonight => "Tonight",
            Preset::Tomorrow => "Tomorrow",
            Preset::NextWeek => "Mon",
        }
    }
}

/// The presets that can be chosen at `now`, with the time each sets. A
/// preset that would already be overdue (tonight, once the evening has
/// come) is left out.
pub fn presets(
    now: i64,
    zone: &dyn Zone,
    morning: TimeOfDay,
    evening: TimeOfDay,
) -> Vec<(Preset, Due)> {
    let today = zone.to_civil(now);
    let mut out = Vec::new();
    for preset in Preset::ALL {
        let civil = match preset {
            Preset::Hour => zone.to_civil(now + 3600).minute_floor(),
            Preset::Tonight => today.at(evening),
            Preset::Tomorrow => today.plus_days(1).at(morning),
            Preset::NextWeek => {
                let days = 7 - i64::from(today.weekday());
                today.plus_days(days).at(morning)
            }
        };
        let due = Due::Local(civil);
        if due.at(zone) > now {
            out.push((preset, due));
        }
    }
    out
}

/// A moment as the command line gives it (`wl-note due <note> <when>`):
///
/// - a TOML date or datetime (`2026-11-20`, `2026-11-20T09:00`, …);
/// - `HH:MM`: the next time the clock shows it;
/// - `+30m`, `+2h`, `+3d`, `+1w`: that long from now (days and weeks on
///   the calendar, at the same time of day);
/// - `today`: this evening, at `evening`; `tomorrow`: at `morning`.
pub fn parse_when(
    s: &str,
    now: i64,
    zone: &dyn Zone,
    morning: TimeOfDay,
    evening: TimeOfDay,
) -> Result<Due, String> {
    let s = s.trim();
    let today = zone.to_civil(now);
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "today" => {
            let due = Due::Local(today.at(evening));
            return match due.at(zone) > now {
                true => Ok(due),
                false => Err(format!("{evening} has passed today")),
            };
        }
        "tomorrow" => return Ok(Due::Local(today.plus_days(1).at(morning))),
        _ => {}
    }
    if let Some(rest) = lower.strip_prefix('+') {
        let wrong = || format!("'{s}': expected + a number and m, h, d or w, such as +2h");
        let unit = rest.chars().last().ok_or_else(wrong)?;
        let n: i64 = rest[..rest.len() - unit.len_utf8()]
            .parse()
            .map_err(|_| wrong())?;
        if !(0..=100_000).contains(&n) {
            return Err(wrong());
        }
        return match unit {
            'm' => Ok(Due::Local(zone.to_civil(now + n * 60).minute_floor())),
            'h' => Ok(Due::Local(zone.to_civil(now + n * 3600).minute_floor())),
            'd' => Ok(Due::Local(today.minute_floor().plus_days(n))),
            'w' => Ok(Due::Local(today.minute_floor().plus_days(n * 7))),
            _ => Err(wrong()),
        };
    }
    if !s.contains('-')
        && let Ok(t) = TimeOfDay::parse(s)
    {
        let due = Due::Local(today.at(t));
        return Ok(match due.at(zone) > now {
            true => due,
            false => Due::Local(today.plus_days(1).at(t)),
        });
    }
    let dt: Datetime = s.parse().map_err(|_| {
        format!(
            "'{s}': expected a date or datetime (2026-11-20, 2026-11-20T09:00), HH:MM, +30m, +2h, +3d, today or tomorrow"
        )
    })?;
    Due::from_datetime(&dt).map_err(|_| format!("'{s}': a time needs a date as well"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const UTC: Fixed = Fixed(0);
    const MORNING: TimeOfDay = TimeOfDay::new(9, 0);
    const EVENING: TimeOfDay = TimeOfDay::new(18, 0);

    fn dt(s: &str) -> Datetime {
        s.parse().unwrap()
    }

    fn civil(y: i32, mo: u8, d: u8, h: u8, mi: u8) -> Civil {
        Civil::new(y, mo, d, h, mi)
    }

    /// Unix time of a UTC date and time.
    fn utc(y: i32, mo: u8, d: u8, h: u8, mi: u8) -> i64 {
        civil(y, mo, d, h, mi).as_utc()
    }

    #[test]
    fn calendar_arithmetic() {
        assert_eq!(utc(1970, 1, 1, 0, 0), 0);
        assert_eq!(utc(2000, 3, 1, 0, 0), 951_868_800);
        assert_eq!(utc(2026, 11, 20, 9, 0), 1_795_165_200);
        for t in [
            0,
            86_399,
            951_868_800,
            1_795_165_200,
            -86_400,
            4_102_444_800,
        ] {
            assert_eq!(Civil::from_utc(t).as_utc(), t);
        }
        // Thursday, the 1st of January 1970; a leap day.
        assert_eq!(civil(1970, 1, 1, 0, 0).weekday(), 3);
        assert_eq!(civil(2026, 11, 20, 0, 0).weekday(), 4);
        assert_eq!(
            civil(2024, 2, 28, 12, 0).plus_days(2),
            civil(2024, 3, 1, 12, 0)
        );
        assert_eq!(
            civil(2026, 12, 31, 23, 0).plus_days(1),
            civil(2027, 1, 1, 23, 0)
        );
        assert_eq!(
            civil(2026, 3, 1, 0, 0).plus_days(-1),
            civil(2026, 2, 28, 0, 0)
        );
    }

    #[test]
    fn the_three_forms() {
        let day = Due::from_datetime(&dt("2026-11-20")).unwrap();
        assert_eq!(day, Due::Day(civil(2026, 11, 20, 0, 0)));
        assert!(day.is_day());
        // The start of that day, local time.
        assert_eq!(day.at(&Fixed(60)), utc(2026, 11, 19, 23, 0));
        let local = Due::from_datetime(&dt("2026-11-20T09:00")).unwrap();
        assert_eq!(local, Due::Local(civil(2026, 11, 20, 9, 0)));
        assert_eq!(local.at(&UTC), utc(2026, 11, 20, 9, 0));
        assert_eq!(local.at(&Fixed(-300)), utc(2026, 11, 20, 14, 0));
        let seconds = Due::from_datetime(&dt("2026-11-20 09:00:30")).unwrap();
        assert_eq!(seconds.at(&UTC), utc(2026, 11, 20, 9, 0) + 30);
        // An offset names the moment, whatever the zone.
        let offset = Due::from_datetime(&dt("2026-11-20T09:00:00+02:00")).unwrap();
        assert_eq!(offset.at(&Fixed(-300)), utc(2026, 11, 20, 7, 0));
        let zulu = Due::from_datetime(&dt("2026-11-20T09:00Z")).unwrap();
        assert_eq!(zulu.at(&Fixed(600)), utc(2026, 11, 20, 9, 0));
    }

    #[test]
    fn bad_values() {
        // A time of day alone is no due time.
        assert!(Due::from_datetime(&dt("09:00")).is_err());
        for bad in ["tomorrow", "2026-13-01", "2026-11-31x", ""] {
            assert!(bad.parse::<Datetime>().is_err(), "{bad}");
        }
    }

    #[test]
    fn overdue_from_the_moment_it_is_due() {
        let due = Due::Local(civil(2026, 11, 20, 9, 0));
        let at = utc(2026, 11, 20, 9, 0);
        assert!(!due.overdue(&UTC, at - 1));
        assert!(due.overdue(&UTC, at));
        assert!(due.overdue(&UTC, at + 86_400));
        // A date is due at the start of the day.
        let day = Due::Day(civil(2026, 11, 20, 0, 0));
        assert!(!day.overdue(&UTC, utc(2026, 11, 19, 23, 59)));
        assert!(day.overdue(&UTC, utc(2026, 11, 20, 0, 0)));
    }

    #[test]
    fn labels_are_absolute() {
        let now = utc(2026, 11, 1, 12, 0);
        let due = Due::Local(civil(2026, 11, 20, 9, 5));
        assert_eq!(due.label(&UTC, now), "Due Fri 20 Nov 09:05");
        assert_eq!(due.label(&UTC, now + 86_400 * 30), "Due Fri 20 Nov 09:05");
        assert_eq!(
            Due::Day(civil(2026, 11, 20, 0, 0)).label(&UTC, now),
            "Due Fri 20 Nov"
        );
        // Another year says so.
        assert_eq!(
            Due::Local(civil(2027, 1, 4, 18, 30)).when(&UTC, now),
            "Mon 4 Jan 2027 18:30"
        );
        // An offset datetime reads in the zone's time.
        let at = Due::from_datetime(&dt("2026-11-20T09:00Z")).unwrap();
        assert_eq!(at.when(&Fixed(60), now), "Fri 20 Nov 10:00");
    }

    #[test]
    fn written_as_frontmatter() {
        assert_eq!(
            Due::Day(civil(2026, 11, 20, 0, 0))
                .datetime(&UTC)
                .to_string(),
            "2026-11-20"
        );
        assert_eq!(
            Due::Local(civil(2026, 11, 20, 9, 0))
                .datetime(&UTC)
                .to_string(),
            "2026-11-20T09:00"
        );
        let at = Due::from_datetime(&dt("2026-11-20T09:00Z")).unwrap();
        assert_eq!(at.datetime(&Fixed(60)).to_string(), "2026-11-20T10:00");
    }

    #[test]
    fn the_next_due_among_notes() {
        let now = utc(2026, 11, 20, 12, 0);
        let past = Due::Local(civil(2026, 11, 20, 9, 0));
        let soon = Due::Local(civil(2026, 11, 20, 13, 0));
        let later = Due::Day(civil(2026, 11, 22, 0, 0));
        let at = |d: &[Due]| next_due(d.iter().copied(), &UTC, now);
        assert_eq!(at(&[]), None);
        assert_eq!(at(&[past]), None);
        assert_eq!(at(&[later, past, soon]), Some(utc(2026, 11, 20, 13, 0)));
        // Due this very second: overdue, nothing to wait for.
        assert_eq!(at(&[Due::At(now)]), None);
    }

    #[test]
    fn presets_around_the_day() {
        let at = |now| presets(now, &UTC, MORNING, EVENING);
        let due = |list: &[(Preset, Due)], p| {
            list.iter()
                .find(|(q, _)| *q == p)
                .map(|(_, d)| d.datetime(&UTC).to_string())
        };
        // A Thursday afternoon.
        let list = at(utc(2026, 11, 19, 14, 20) + 17);
        assert_eq!(
            due(&list, Preset::Hour).as_deref(),
            Some("2026-11-19T15:20")
        );
        assert_eq!(
            due(&list, Preset::Tonight).as_deref(),
            Some("2026-11-19T18:00")
        );
        assert_eq!(
            due(&list, Preset::Tomorrow).as_deref(),
            Some("2026-11-20T09:00")
        );
        assert_eq!(
            due(&list, Preset::NextWeek).as_deref(),
            Some("2026-11-23T09:00")
        );
        // Past the evening: no tonight; the hour crosses midnight.
        let list = at(utc(2026, 11, 30, 23, 30));
        assert_eq!(due(&list, Preset::Tonight), None);
        assert_eq!(
            due(&list, Preset::Hour).as_deref(),
            Some("2026-12-01T00:30")
        );
        assert_eq!(
            due(&list, Preset::Tomorrow).as_deref(),
            Some("2026-12-01T09:00")
        );
        // Just after midnight it is tonight that comes first.
        let list = at(utc(2026, 12, 1, 0, 5));
        assert_eq!(
            due(&list, Preset::Tonight).as_deref(),
            Some("2026-12-01T18:00")
        );
        // On a Monday, next week is the Monday after.
        let list = at(utc(2026, 11, 23, 8, 0));
        assert_eq!(
            due(&list, Preset::NextWeek).as_deref(),
            Some("2026-11-30T09:00")
        );
        // On a Sunday it is the next day.
        let list = at(utc(2026, 11, 22, 8, 0));
        assert_eq!(
            due(&list, Preset::NextWeek).as_deref(),
            Some("2026-11-23T09:00")
        );
        // All of them are in the future.
        for t in [0, 3600 * 5, 86_400 * 3 + 40_000] {
            assert!(at(t).iter().all(|(_, d)| d.at(&UTC) > t));
        }
    }

    #[test]
    fn presets_follow_the_zone() {
        // 23:30 UTC is already the next day in Berlin.
        let now = utc(2026, 11, 19, 23, 30);
        let list = presets(now, &Fixed(60), MORNING, EVENING);
        let tomorrow = list.iter().find(|(p, _)| *p == Preset::Tomorrow).unwrap();
        assert_eq!(
            tomorrow.1.datetime(&Fixed(60)).to_string(),
            "2026-11-21T09:00"
        );
    }

    #[test]
    fn when_on_the_command_line() {
        let now = utc(2026, 11, 19, 14, 20) + 40;
        let parse = |s: &str| {
            parse_when(s, now, &UTC, MORNING, EVENING).map(|d| d.datetime(&UTC).to_string())
        };
        assert_eq!(parse("2026-11-20").unwrap(), "2026-11-20");
        assert_eq!(parse("2026-11-20T09:00").unwrap(), "2026-11-20T09:00");
        assert_eq!(parse("2026-11-20T09:00+01:00").unwrap(), "2026-11-20T08:00");
        // The next time the clock shows it.
        assert_eq!(parse("16:45").unwrap(), "2026-11-19T16:45");
        assert_eq!(parse("9:30").unwrap(), "2026-11-20T09:30");
        assert_eq!(parse("14:20").unwrap(), "2026-11-20T14:20");
        // Relative.
        assert_eq!(parse("+30m").unwrap(), "2026-11-19T14:50");
        assert_eq!(parse("+2h").unwrap(), "2026-11-19T16:20");
        assert_eq!(parse("+3d").unwrap(), "2026-11-22T14:20");
        assert_eq!(parse("+1w").unwrap(), "2026-11-26T14:20");
        assert_eq!(parse(" +0m ").unwrap(), "2026-11-19T14:20");
        // Words.
        assert_eq!(parse("today").unwrap(), "2026-11-19T18:00");
        assert_eq!(parse("Tomorrow").unwrap(), "2026-11-20T09:00");
        let late = utc(2026, 11, 19, 19, 0);
        assert!(parse_when("today", late, &UTC, MORNING, EVENING).is_err());
        for bad in [
            "",
            "soon",
            "+",
            "+h",
            "+2x",
            "+-2h",
            "25:00",
            "12:60",
            "2026-11-31",
            "09:00:00",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn times_of_day() {
        assert_eq!(TimeOfDay::parse("09:00").unwrap(), MORNING);
        assert_eq!(TimeOfDay::parse(" 7:05 ").unwrap(), TimeOfDay::new(7, 5));
        assert_eq!(MORNING.to_string(), "09:00");
        for bad in [
            "", "9", "24:00", "09:60", "09:5", "ab:cd", "009:00", "-1:00",
        ] {
            assert!(TimeOfDay::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_local_zone_round_trips() {
        // Whatever the zone, a time in the middle of the day is itself
        // again after going to Unix time and back.
        for c in [civil(2026, 1, 15, 12, 0), civil(2026, 7, 15, 12, 30)] {
            assert_eq!(Local.to_civil(Local.to_unix(c)), c);
        }
        let t = now();
        assert_eq!(Local.to_unix(Local.to_civil(t)), t);
    }
}
