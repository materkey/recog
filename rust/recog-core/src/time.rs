//! The `datetime` operations the meeting storage relies on: `now(UTC).isoformat()`,
//! `fromisoformat`, aware/naive subtraction, `astimezone()` to the local zone,
//! `fromtimestamp(t, UTC)` with Python's rounding — behind a [`Clock`], so tests and
//! parity runs fix the time and the zone.
//!
//! Known difference: `fromisoformat` accepts the extended forms recog writes and
//! reads (`YYYY-MM-DD`, any one separator, `HH[:MM[:SS[.f…]]]`, `Z` or `±HH:MM[…]`);
//! Python 3.12 also accepts week dates and the basic `YYYYMMDD`/`HHMMSS` forms.

use chrono::{
    DateTime, Datelike as _, Local, NaiveDate, NaiveDateTime, Offset as _, TimeZone as _, Timelike as _, Utc,
};

use crate::py;

const MICROS: i64 = 1_000_000;

/// Where "now" and the local zone come from.
pub trait Clock: Send + Sync {
    /// Microseconds since the Unix epoch.
    fn now_micros(&self) -> i64;
    /// The local zone's offset from UTC, in seconds, at an instant.
    fn utc_offset(&self, utc_micros: i64) -> i64;
}

/// The system clock and the system's local zone (`TZ` included).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_micros(&self) -> i64 {
        Utc::now().timestamp_micros()
    }

    fn utc_offset(&self, utc_micros: i64) -> i64 {
        DateTime::<Utc>::from_timestamp_micros(utc_micros)
            .map_or(0, |t| i64::from(t.with_timezone(&Local).offset().fix().local_minus_utc()))
    }
}

/// A `datetime`: wall time in microseconds since 1970-01-01 of its own zone, and the
/// zone's offset when it is aware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PyDateTime {
    pub wall_micros: i64,
    pub offset_micros: Option<i64>,
}

/// A `datetime` failure: Python's `ValueError` or `TypeError` text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeError {
    Value(String),
    Type(String),
}

impl std::fmt::Display for TimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Value(m) | Self::Type(m) => f.write_str(m),
        }
    }
}

impl PyDateTime {
    /// `datetime.fromtimestamp(utc_micros, UTC)` of an exact instant.
    #[must_use]
    pub fn utc(utc_micros: i64) -> Self {
        Self { wall_micros: utc_micros, offset_micros: Some(0) }
    }

    /// The instant, for an aware datetime.
    #[must_use]
    pub fn utc_micros(&self) -> Option<i64> {
        self.offset_micros.map(|offset| self.wall_micros - offset)
    }

    /// `self - other` in microseconds.
    ///
    /// # Errors
    /// Python's `TypeError` for subtracting a naive and an aware datetime.
    pub fn minus(&self, other: &Self) -> Result<i64, TimeError> {
        match (self.utc_micros(), other.utc_micros()) {
            (Some(a), Some(b)) => Ok(a - b),
            (None, None) => Ok(self.wall_micros - other.wall_micros),
            _ => Err(TimeError::Type("can't subtract offset-naive and offset-aware datetimes".into())),
        }
    }

    /// `astimezone()` to the clock's local zone: an aware time keeps its instant, a
    /// naive one is taken as local time.
    #[must_use]
    pub fn local_wall(&self, clock: &dyn Clock) -> i64 {
        let instant = self.utc_micros().unwrap_or_else(|| {
            let guess = self.wall_micros - clock.utc_offset(self.wall_micros) * MICROS;
            self.wall_micros - clock.utc_offset(guess) * MICROS
        });
        instant + clock.utc_offset(instant) * MICROS
    }

    /// `isoformat()`.
    #[must_use]
    pub fn isoformat(&self) -> String {
        let mut text = format_wall(self.wall_micros, "%Y-%m-%dT%H:%M:%S");
        let micros = self.wall_micros.rem_euclid(MICROS);
        if micros != 0 {
            text.push_str(&format!(".{micros:06}"));
        }
        if let Some(offset) = self.offset_micros {
            let sign = if offset < 0 { '-' } else { '+' };
            let offset = offset.abs();
            let (hours, minutes) = (offset / 3_600 / MICROS, offset / 60 / MICROS % 60);
            let (seconds, fraction) = (offset / MICROS % 60, offset % MICROS);
            text.push_str(&format!("{sign}{hours:02}:{minutes:02}"));
            if seconds != 0 || fraction != 0 {
                text.push_str(&format!(":{seconds:02}"));
                if fraction != 0 {
                    text.push_str(&format!(".{fraction:06}"));
                }
            }
        }
        text
    }
}

/// strftime of a wall time in microseconds since 1970-01-01.
#[must_use]
pub fn format_wall(wall_micros: i64, format: &str) -> String {
    DateTime::<Utc>::from_timestamp_micros(wall_micros)
        .map(|t| t.naive_utc().format(format).to_string())
        .unwrap_or_default()
}

/// The calendar date of a wall time, as (year, month, day).
#[must_use]
pub fn wall_date(wall_micros: i64) -> (i32, u32, u32) {
    DateTime::<Utc>::from_timestamp_micros(wall_micros).map_or((1970, 1, 1), |t| (t.year(), t.month(), t.day()))
}

/// `datetime.now(UTC)`.
#[must_use]
pub fn now_utc(clock: &dyn Clock) -> PyDateTime {
    PyDateTime::utc(clock.now_micros())
}

/// `datetime.fromtimestamp(seconds, UTC)`: microseconds rounded half to even, as
/// CPython's `_PyTime_ObjectToTimeval` does.
#[must_use]
pub fn from_timestamp(seconds: f64) -> PyDateTime {
    let whole = seconds.trunc();
    let mut fraction = ((seconds - whole) * 1e6).round_ties_even();
    let mut whole = whole;
    if fraction >= 1e6 {
        fraction -= 1e6;
        whole += 1.0;
    } else if fraction < 0.0 {
        fraction += 1e6;
        whole -= 1.0;
    }
    #[allow(clippy::cast_possible_truncation)]
    PyDateTime::utc(whole as i64 * MICROS + fraction as i64)
}

fn invalid(text: &str) -> TimeError {
    TimeError::Value(format!("Invalid isoformat string: {}", py::repr(text)))
}

fn digits(s: &str, at: usize, n: usize) -> Option<i64> {
    let part = s.get(at..at + n)?;
    if part.bytes().all(|b| b.is_ascii_digit()) { part.parse().ok() } else { None }
}

/// Parse `HH[:MM[:SS[.f…]]]` from `s`, returning microseconds since midnight.
fn parse_time(s: &str) -> Option<(i64, i64, i64, i64)> {
    let hour = digits(s, 0, 2)?;
    let mut rest = &s[2..];
    let (mut minute, mut second, mut micro) = (0, 0, 0);
    if let Some(r) = rest.strip_prefix(':') {
        minute = digits(r, 0, 2)?;
        rest = &r[2..];
        if let Some(r) = rest.strip_prefix(':') {
            second = digits(r, 0, 2)?;
            rest = &r[2..];
            if let Some(r) = rest.strip_prefix(['.', ',']) {
                if r.is_empty() || !r.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                let kept = &r[..r.len().min(6)];
                micro = kept.parse::<i64>().ok()? * 10_i64.pow(u32::try_from(6 - kept.len()).ok()?);
                rest = "";
            }
        }
    }
    rest.is_empty().then_some((hour, minute, second, micro))
}

/// `datetime.fromisoformat(text)`.
///
/// # Errors
/// Python's `ValueError` text: `Invalid isoformat string: '…'`, or the range error of
/// a component.
pub fn fromisoformat(text: &str) -> Result<PyDateTime, TimeError> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 10 || !chars[..10].iter().all(char::is_ascii) {
        return Err(invalid(text));
    }
    let date_part: String = chars[..10].iter().collect();
    let (year, month, day) = match (
        digits(&date_part, 0, 4),
        &date_part[4..5],
        digits(&date_part, 5, 2),
        &date_part[7..8],
        digits(&date_part, 8, 2),
    ) {
        (Some(y), "-", Some(m), "-", Some(d)) => (y, m, d),
        _ => return Err(invalid(text)),
    };
    let rest: String = chars.iter().skip(11).collect();
    if !rest.is_ascii() {
        return Err(invalid(text));
    }
    let has_separator = chars.len() > 10;
    let (time, offset) = if has_separator {
        if rest.is_empty() {
            return Err(invalid(text));
        }
        let split = rest.find(['Z', '+', '-']).unwrap_or(rest.len());
        let time = parse_time(&rest[..split]).ok_or_else(|| invalid(text))?;
        let tz = &rest[split..];
        let offset = if tz.is_empty() {
            None
        } else if tz == "Z" {
            Some(0)
        } else {
            let sign = if tz.starts_with('-') { -1 } else { 1 };
            let body = &tz[1..];
            let (h, m, s, us) = if body.len() == 4 && body.bytes().all(|b| b.is_ascii_digit()) {
                (digits(body, 0, 2).unwrap_or(0), digits(body, 2, 2).unwrap_or(0), 0, 0)
            } else {
                parse_time(body).ok_or_else(|| invalid(text))?
            };
            if h > 23 || m > 59 || s > 59 {
                return Err(TimeError::Value(
                    "offset must be a timedelta strictly between -timedelta(hours=24) and timedelta(hours=24).".into(),
                ));
            }
            Some(sign * (((h * 60 + m) * 60 + s) * MICROS + us))
        };
        (time, offset)
    } else {
        ((0, 0, 0, 0), None)
    };
    let (hour, minute, second, micro) = time;
    if year < 1 {
        return Err(TimeError::Value(format!("year {year} is out of range")));
    }
    if !(1..=12).contains(&month) {
        return Err(TimeError::Value("month must be in 1..12".into()));
    }
    let date = NaiveDate::from_ymd_opt(
        i32::try_from(year).unwrap_or(1),
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(0),
    )
    .ok_or_else(|| TimeError::Value("day is out of range for month".into()))?;
    for (value, name, max) in [(hour, "hour", 23), (minute, "minute", 59), (second, "second", 59)] {
        if value > max {
            return Err(TimeError::Value(format!("{name} must be in 0..{max}")));
        }
    }
    let wall = NaiveDateTime::new(
        date,
        chrono::NaiveTime::from_hms_micro_opt(
            u32::try_from(hour).unwrap_or(0),
            u32::try_from(minute).unwrap_or(0),
            u32::try_from(second).unwrap_or(0),
            u32::try_from(micro).unwrap_or(0),
        )
        .ok_or_else(|| invalid(text))?,
    );
    Ok(PyDateTime { wall_micros: Utc.from_utc_datetime(&wall).timestamp_micros(), offset_micros: offset })
}

/// Hour, minute and second of a wall time: `strftime("%H:%M:%S")`.
#[must_use]
pub fn hms(wall_micros: i64) -> String {
    DateTime::<Utc>::from_timestamp_micros(wall_micros)
        .map(|t| format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isoformat_omits_zero_microseconds_and_prints_the_offset_like_python() {
        // given / when / then
        assert_eq!(PyDateTime::utc(1_700_000_000_000_000).isoformat(), "2023-11-14T22:13:20+00:00");
        assert_eq!(PyDateTime::utc(1_700_000_000_000_123).isoformat(), "2023-11-14T22:13:20.000123+00:00");
        let naive = fromisoformat("2026-01-02 03:04:05").unwrap();
        assert_eq!(naive.isoformat(), "2026-01-02T03:04:05");
    }

    #[test]
    fn fromisoformat_reads_offsets_fractions_and_dates() {
        // given / when
        let aware = fromisoformat("2026-11-08T10:00:00.5+03:00").unwrap();
        let zulu = fromisoformat("2026-11-08T07:00:00.500000Z").unwrap();
        // then
        assert_eq!(aware.minus(&zulu), Ok(0));
        assert_eq!(fromisoformat("2026-11-08").unwrap().isoformat(), "2026-11-08T00:00:00");
        assert_eq!(
            fromisoformat("2026-11-08T10:00:00.1234567+0100").unwrap().isoformat(),
            "2026-11-08T10:00:00.123456+01:00"
        );
    }

    #[test]
    fn malformed_or_out_of_range_dates_fail_with_python_messages() {
        // given / when / then
        assert_eq!(fromisoformat("soon"), Err(TimeError::Value("Invalid isoformat string: 'soon'".into())));
        assert_eq!(fromisoformat("2026-13-01"), Err(TimeError::Value("month must be in 1..12".into())));
        assert_eq!(fromisoformat("2026-02-30"), Err(TimeError::Value("day is out of range for month".into())));
        assert_eq!(fromisoformat("2026-02-01T25:00"), Err(TimeError::Value("hour must be in 0..23".into())));
        let naive = fromisoformat("2026-02-01T00:00").unwrap();
        assert!(matches!(PyDateTime::utc(0).minus(&naive), Err(TimeError::Type(_))));
    }

    #[test]
    fn iso_texts_python_rejects_when_parsed_then_its_value_errors() {
        // given / when / then
        for (text, message) in [
            ("2026/01/01", "Invalid isoformat string: '2026/01/01'"),
            ("2026-01-0é", "Invalid isoformat string: '2026-01-0é'"),
            ("2026-01-01Té", "Invalid isoformat string: '2026-01-01Té'"),
            ("2026-01-01T", "Invalid isoformat string: '2026-01-01T'"),
            ("2026-01-01T10:00:00.", "Invalid isoformat string: '2026-01-01T10:00:00.'"),
            ("2026-01-01T10:00:00.5x", "Invalid isoformat string: '2026-01-01T10:00:00.5x'"),
            ("2026-01-01T1", "Invalid isoformat string: '2026-01-01T1'"),
            ("2026-01-01T10:0", "Invalid isoformat string: '2026-01-01T10:0'"),
            ("2026-01-01T10:00:0", "Invalid isoformat string: '2026-01-01T10:00:0'"),
            ("2026-01-01T10:00+1", "Invalid isoformat string: '2026-01-01T10:00+1'"),
            (
                "2026-01-01T10:00+24:00",
                "offset must be a timedelta strictly between -timedelta(hours=24) and timedelta(hours=24).",
            ),
            ("0000-01-01", "year 0 is out of range"),
            ("2026-01-01T10:60", "minute must be in 0..59"),
            ("2026-01-01T10:00:60", "second must be in 0..59"),
        ] {
            assert_eq!(fromisoformat(text), Err(TimeError::Value(message.into())), "{text}");
        }
    }

    #[test]
    fn offsets_with_seconds_when_read_then_kept_and_printed_as_python() {
        // given / when
        let west = fromisoformat("2026-01-01T10:00:00,25-01:30:15.5").unwrap();
        // then
        assert_eq!(west.isoformat(), "2026-01-01T10:00:00.250000-01:30:15.500000");
        assert_eq!(fromisoformat("2026-01-01T10:00:00+00:00:30").unwrap().isoformat(), "2026-01-01T10:00:00+00:00:30");
        let naive = fromisoformat("2026-01-01T10:00").unwrap();
        assert_eq!(naive.minus(&fromisoformat("2026-01-01T09:00").unwrap()), Ok(3_600 * MICROS));
    }

    #[test]
    fn fromtimestamp_rounds_microseconds_half_to_even() {
        // given / when / then
        // Python: 1.0000005 -> .000001 and 1.0000015 -> .000001 (the fraction's own
        // binary value decides), 2.5e-06 -> .000002 (a tie, to even).
        assert_eq!(from_timestamp(1.000_000_5).isoformat(), "1970-01-01T00:00:01.000001+00:00");
        assert_eq!(from_timestamp(1.000_001_5).isoformat(), "1970-01-01T00:00:01.000001+00:00");
        assert_eq!(from_timestamp(2.5e-6).isoformat(), "1970-01-01T00:00:00.000002+00:00");
        assert_eq!(from_timestamp(-0.5).isoformat(), "1969-12-31T23:59:59.500000+00:00");
    }
}
