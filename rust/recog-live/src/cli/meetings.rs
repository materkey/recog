//! `recog meetings [meeting_id] [--json] [--refresh-cache]`: the saved meetings, or one
//! meeting's transcript in the web UI's "Copy all" format (`recog/cli.py`).

use recog_core::meeting_storage::MeetingStorage;
use recog_core::pyjson::{self, Value};
use recog_core::time::{Clock, fromisoformat, hms};

use crate::cli::args::MeetingsArgs;

/// What the command printed and its exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Printed {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

fn field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object().and_then(|m| m.get(key))
}

fn text(value: Option<&Value>) -> String {
    value.map(Value::py_str).unwrap_or_default()
}

fn pad_left(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width { s.to_owned() } else { format!("{s}{}", " ".repeat(width - n)) }
}

fn pad_right(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width { s.to_owned() } else { format!("{}{s}", " ".repeat(width - n)) }
}

/// `ts_to_local`: an ISO time as local `HH:MM:SS`, empty when it does not parse.
fn ts_to_local(ts: &str, zone: &dyn Clock) -> String {
    fromisoformat(ts).map(|t| hms(t.local_wall(zone))).unwrap_or_default()
}

fn uncertain_suffix(segment: &Value) -> &'static str {
    if !field(segment, "uncertain").is_some_and(Value::truthy) {
        return "";
    }
    if field(segment, "uncertain_reason").and_then(Value::as_str) == Some("mics_off") { " (🔇?)" } else { " (?)" }
}

/// One meeting as `recog meetings <id>` prints it.
#[must_use]
pub fn format_meeting(meeting: &Value, zone: &dyn Clock) -> String {
    let empty = Value::Object(recog_core::pyjson::Map::new());
    let meta = field(meeting, "meta").unwrap_or(&empty);
    let mut out = String::new();
    let line = "=".repeat(60);
    out.push_str(&format!("{line}\n"));
    out.push_str(&format!(" Meeting: {}\n", text(field(meta, "id"))));
    out.push_str(&format!(" Source: {}\n", text(field(meta, "source"))));
    if let Some(title) = field(meta, "title").filter(|t| t.truthy()) {
        out.push_str(&format!(" Title: {}\n", title.py_str()));
    }
    let conference = field(meta, "conference").or_else(|| field(meta, "ktalk")).unwrap_or(&empty);
    if let Some(url) = field(conference, "live_url").filter(|u| u.truthy()) {
        out.push_str(&format!(" Conference URL: {}\n", url.py_str()));
    }
    out.push_str(&format!(" Created: {}\n", text(field(meta, "created_at"))));
    let stats = field(meta, "stats").unwrap_or(&empty);
    out.push_str(&format!(" Duration: {}s\n", text(field(stats, "duration_seconds"))));
    out.push_str(&format!(" Segments: {}\n", text(field(stats, "segment_count"))));
    out.push_str(&format!("{line}\n\n"));
    let segments = match field(meeting, "segments") {
        Some(Value::Array(segments)) => segments.as_slice(),
        _ => &[],
    };
    let lines: Vec<String> = segments
        .iter()
        .map(|segment| {
            let speaker = format!("{}{}", text(field(segment, "speaker")), uncertain_suffix(segment));
            let local = ts_to_local(field(segment, "ts").and_then(Value::as_str).unwrap_or(""), zone);
            let time = if local.is_empty() { text(field(segment, "ts_display")) } else { local };
            format!("**{speaker}** *[{time}]*: {}", text(field(segment, "text")))
        })
        .collect();
    out.push_str(&lines.join("\n\n"));
    out.push('\n');
    out
}

/// The meeting list as `recog meetings` prints it.
#[must_use]
pub fn format_list(meetings: &[Value]) -> String {
    if meetings.is_empty() {
        return "No meetings found.\n".into();
    }
    let mut out = format!(
        "{} {} {}  {}  Title\n{}\n",
        pad_left("ID", 35),
        pad_left("Source", 12),
        pad_right("Segments", 8),
        pad_right("Duration", 10),
        "-".repeat(85)
    );
    for m in meetings {
        let duration = match field(m, "duration_seconds") {
            Some(d) if d.truthy() => format!("{}s", d.py_str()),
            _ => "-".into(),
        };
        let mut title = field(m, "title").filter(|t| t.truthy()).map_or_else(|| "(untitled)".to_owned(), Value::py_str);
        if title.chars().count() > 25 {
            title = format!("{}...", title.chars().take(22).collect::<String>());
        }
        let mut source = text(field(m, "source"));
        if field(m, "_external").is_some_and(Value::truthy) {
            source.push('*');
        }
        out.push_str(&format!(
            "{} {} {}  {}  {title}\n",
            pad_left(&text(field(m, "id")), 35),
            pad_left(&source, 12),
            pad_right(&text(field(m, "segment_count")), 8),
            pad_right(&duration, 10),
        ));
    }
    out
}

/// Runs the command against `storage`.
#[must_use]
pub fn run(storage: &MeetingStorage, args: &MeetingsArgs, zone: &dyn Clock) -> Printed {
    let ok = |stdout: String| Printed { stdout, stderr: String::new(), code: 0 };
    let failed = |e: &dyn std::fmt::Display| Printed { stdout: String::new(), stderr: format!("{e}\n"), code: 1 };
    if let Some(id) = &args.meeting_id {
        return match storage.get_meeting(id) {
            Ok(Some(meeting)) if args.json => ok(format!("{}\n", pyjson::dumps(&meeting, Some(2)))),
            Ok(Some(meeting)) => ok(format_meeting(&meeting, zone)),
            Ok(None) => Printed { stdout: String::new(), stderr: format!("Meeting not found: {id}\n"), code: 1 },
            Err(e) => failed(&e),
        };
    }
    match storage.list_meetings(true, args.refresh_cache) {
        Ok(meetings) if args.json => ok(format!("{}\n", pyjson::dumps(&Value::Array(meetings), Some(2)))),
        Ok(meetings) => ok(format_list(&meetings)),
        Err(e) => failed(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Utc;
    impl Clock for Utc {
        fn now_micros(&self) -> i64 {
            0
        }
        fn utc_offset(&self, _utc_micros: i64) -> i64 {
            0
        }
    }

    fn json(text: &str) -> Value {
        pyjson::loads(text).unwrap()
    }

    #[test]
    fn a_meeting_with_uncertain_rows_format_meeting_prints_the_copy_all_text() {
        let meeting = json(
            r#"{"meta": {"id": "m1", "source": "ktalk", "title": "Синк", "conference": {"live_url": "https://x/r"},
                "created_at": "2026-01-01T10:00:00+00:00", "stats": {"duration_seconds": 61, "segment_count": 3}},
                "segments": [
                  {"speaker": "Alice", "ts": "2026-01-01T10:00:05+00:00", "text": "привет"},
                  {"speaker": "Bob", "ts": "bad", "ts_display": "10:00:07", "text": "да", "uncertain": true, "uncertain_reason": "mics_off"},
                  {"speaker": "Unknown", "ts": "2026-01-01T10:00:09+00:00", "text": "э", "uncertain": true}
                ]}"#,
        );
        let text = format_meeting(&meeting, &Utc);
        assert_eq!(
            text,
            "============================================================\n Meeting: m1\n Source: ktalk\n Title: Синк\n \
             Conference URL: https://x/r\n Created: 2026-01-01T10:00:00+00:00\n Duration: 61s\n Segments: 3\n\
             ============================================================\n\n**Alice** *[10:00:05]*: привет\n\n\
             **Bob (🔇?)** *[10:00:07]*: да\n\n**Unknown (?)** *[10:00:09]*: э\n"
        );
    }

    #[test]
    fn a_list_with_a_long_title_and_an_external_meeting_format_list_truncates_and_marks() {
        let meetings = vec![
            json(
                r#"{"id": "a", "source": "ktalk", "segment_count": 5, "duration_seconds": 120, "title": "Очень длинное название встречи"}"#,
            ),
            json(
                r#"{"id": "b", "source": "import", "segment_count": 0, "duration_seconds": 0, "title": null, "_external": true}"#,
            ),
        ];
        let text = format_list(&meetings);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], format!("{:<35} {:<12} {:>8}  {:>10}  Title", "ID", "Source", "Segments", "Duration"));
        assert_eq!(lines[1], "-".repeat(85));
        assert_eq!(
            lines[2],
            format!("{:<35} {:<12} {:>8}  {:>10}  Очень длинное название...", "a", "ktalk", 5, "120s")
        );
        assert_eq!(lines[3], format!("{:<35} {:<12} {:>8}  {:>10}  (untitled)", "b", "import*", 0, "-"));
        assert_eq!(format_list(&[]), "No meetings found.\n");
    }
}
