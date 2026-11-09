//! `datetime.now(timezone.utc).isoformat()` and the part of `datetime.fromisoformat`
//! the increment metadata needs, without a date-time dependency.

use std::time::{SystemTime, UNIX_EPOCH};

/// Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = i64::from(month);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Microseconds since the epoch, floored like `datetime.now()`.
pub fn now_micros() -> i64 {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    elapsed.as_secs() as i64 * 1_000_000 + i64::from(elapsed.subsec_micros())
}

/// `isoformat()` of an aware UTC datetime: microseconds only when nonzero.
pub fn utc_isoformat(micros: i64) -> String {
    let seconds = micros.div_euclid(1_000_000);
    let fraction = micros.rem_euclid(1_000_000);
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let of_day = seconds.rem_euclid(86_400);
    let (hour, minute, second) = (of_day / 3600, of_day / 60 % 60, of_day % 60);
    let fraction = if fraction == 0 { String::new() } else { format!(".{fraction:06}") };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{fraction}+00:00")
}

/// `datetime.now(timezone.utc).isoformat()`.
pub fn utc_now_isoformat() -> String {
    utc_isoformat(now_micros())
}

fn number(text: &str, start: usize, len: usize) -> Option<i64> {
    let digits = text.get(start..start + len)?;
    digits.bytes().all(|byte| byte.is_ascii_digit()).then(|| digits.parse().ok()).flatten()
}

/// Microseconds since the epoch of an ISO 8601 text that carries a UTC offset;
/// `None` for a naive or unreadable one. Reads what `isoformat()` writes and the usual
/// variants: `YYYY-MM-DD`, any one separator character, `HH[:MM[:SS[.fraction]]]`,
/// then `Z` or `±HH[:MM[:SS[.ffffff]]]` or `±HHMM`.
pub fn parse_aware_isoformat(text: &str) -> Option<i64> {
    let days = date_days(text)?;
    let mut rest = text[10..].chars();
    rest.next()?;
    let time = rest.as_str();
    let zone_at = time.find(['Z', 'z', '+', '-'])?;
    let (clock, zone) = time.split_at(zone_at);
    let (seconds, micros) = clock_seconds(clock)?;
    let offset = zone_offset(zone)?;
    Some((days * 86_400 + seconds) * 1_000_000 + micros - offset)
}

/// Days since the epoch of the `YYYY-MM-DD` a text starts with; the text must go on
/// past it.
fn date_days(text: &str) -> Option<i64> {
    if text.len() < 11 || !text.is_char_boundary(10) {
        return None;
    }
    let (year, month, day) = (number(text, 0, 4)?, number(text, 5, 2)?, number(text, 8, 2)?);
    if !date_fields_valid(text, month, day) {
        return None;
    }
    Some(days_from_civil(year, month as u32, day as u32))
}

/// Dashes between the fields, a month and a day in range.
fn date_fields_valid(text: &str, month: i64, day: i64) -> bool {
    &text[4..5] == "-" && &text[7..8] == "-" && (1..=12).contains(&month) && (1..=31).contains(&day)
}

/// `HH[:MM[:SS[.fraction]]]` → (seconds of the day, microseconds).
fn clock_seconds(clock: &str) -> Option<(i64, i64)> {
    let (main, fraction) = clock.split_once('.').map_or((clock, None), |(main, fraction)| (main, Some(fraction)));
    let values = clock_fields(main)?;
    let micros = fraction_micros(fraction, values.len())?;
    let (hour, minute, second) = (values[0], *values.get(1).unwrap_or(&0), *values.get(2).unwrap_or(&0));
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some((hour * 3600 + minute * 60 + second, micros))
}

/// One to three two-digit fields separated by colons.
fn clock_fields(main: &str) -> Option<Vec<i64>> {
    let parts: Vec<&str> = main.split(':').collect();
    if parts.is_empty() || parts.len() > 3 || parts.iter().any(|part| part.len() != 2) {
        return None;
    }
    parts.iter().map(|part| number(part, 0, 2)).collect()
}

/// The microseconds of a fraction, allowed only after seconds; digits past the sixth
/// are dropped.
fn fraction_micros(fraction: Option<&str>, fields: usize) -> Option<i64> {
    match fraction {
        None => Some(0),
        Some(fraction) if !fraction.is_empty() && fields == 3 => {
            let digits: String = fraction.chars().take(6).collect();
            if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            format!("{digits:0<6}").parse().ok()
        }
        Some(_) => None,
    }
}

/// The offset in microseconds of `Z` or `±HH[:MM[:SS[.ffffff]]]` or `±HHMM`.
fn zone_offset(zone: &str) -> Option<i64> {
    if zone.eq_ignore_ascii_case("z") {
        return Some(0);
    }
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let body = zone.get(1..)?;
    // A hand-edited or damaged offset can hold any character: slice it only at
    // character boundaries, so it is unreadable instead of a panic.
    let body = match (body.get(..2), body.get(2..)) {
        (Some(hours), Some(minutes)) if body.len() == 4 && !body.contains(':') => format!("{hours}:{minutes}"),
        _ => body.to_owned(),
    };
    let (offset_seconds, offset_micros) = clock_seconds(&body)?;
    Some(sign * (offset_seconds * 1_000_000 + offset_micros))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_four_byte_offset_with_a_multibyte_character_when_read_then_unreadable_not_a_panic() {
        assert_eq!(zone_offset("+a\u{e9}1"), None);
        assert_eq!(zone_offset("\u{e9}0100"), None);
    }

    #[test]
    fn instants_given_isoformat_when_printed_then_python_text() {
        assert_eq!(utc_isoformat(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(utc_isoformat(1_774_103_100_000_000), "2026-03-21T14:25:00+00:00");
        assert_eq!(utc_isoformat(1_774_103_100_000_042), "2026-03-21T14:25:00.000042+00:00");
    }

    #[test]
    fn written_text_given_parser_when_read_back_then_same_instant() {
        for micros in [0, 1_774_103_100_000_000, 1_774_103_100_123_456] {
            assert_eq!(parse_aware_isoformat(&utc_isoformat(micros)), Some(micros));
        }
        assert_eq!(parse_aware_isoformat("2026-03-21T17:25:00+03:00"), Some(1_774_103_100_000_000));
        assert_eq!(parse_aware_isoformat("2026-03-21x14:25Z"), Some(1_774_103_100_000_000));
        assert_eq!(parse_aware_isoformat("2026-03-21T14:25:00"), None);
        assert_eq!(parse_aware_isoformat("2026-03-21"), None);
        assert_eq!(parse_aware_isoformat("garbage"), None);
    }

    const INSTANT: i64 = 1_774_103_100_000_000;

    #[test]
    fn fractions_and_offsets_given_parser_when_read_then_instant_in_utc() {
        let cases = [
            ("2026-03-21T14:25:00.5Z", INSTANT + 500_000),
            ("2026-03-21T14:25:00.1234567+00:00", INSTANT + 123_456),
            ("2026-03-21T14:25:00z", INSTANT),
            ("2026-03-21T09:25:00-05:00", INSTANT),
            ("2026-03-21T20:00:00+0535", INSTANT),
            ("2026-03-21T19:25+05", INSTANT),
            ("2026-03-21T14:25:10+00:00:10", INSTANT),
            ("2026-03-21T14:25:00+00:00:00.5", INSTANT - 500_000),
            ("2026-03-21 14Z", INSTANT - 25 * 60 * 1_000_000),
        ];
        for (text, micros) in cases {
            assert_eq!(parse_aware_isoformat(text), Some(micros), "{text}");
        }
    }

    #[test]
    fn malformed_date_clock_or_zone_given_parser_when_read_then_none() {
        let cases = [
            "2026-03-0é1T14:25Z",
            "20x6-03-21T14:25Z",
            "2026/03/21T14:25Z",
            "2026-03/21T14:25Z",
            "2026-13-21T14:25Z",
            "2026-03-00T14:25Z",
            "2026-03-21T",
            "2026-03-21TZ",
            "2026-03-21T1:25Z",
            "2026-03-21T14:25:00:00Z",
            "2026-03-21Taa:25Z",
            "2026-03-21T14:25:00.Z",
            "2026-03-21T14:25.5Z",
            "2026-03-21T14:25:00.5xZ",
            "2026-03-21T24:00Z",
            "2026-03-21T14:60Z",
            "2026-03-21T14:25:60Z",
            "2026-03-21T14:25+5",
            "2026-03-21T14:25Zulu",
        ];
        for text in cases {
            assert_eq!(parse_aware_isoformat(text), None, "{text}");
        }
    }
}
