//! `_format_response` of the GigaAM backend: `json`, `text`, `srt`, `vtt`,
//! `verbose_json` and `diarized_json`, with Python's float arithmetic for the
//! subtitle timestamps and its `repr` for the JSON floats.
//!
//! Segments carry no speaker: a request for diarization is refused before formatting
//! (see `crate::backend`), so `speaker` is always `null` and no line gets a `[speaker]`
//! prefix.

use recog_core::py;
use recog_core::pyjson::{Map, Value};

use crate::engine::Span;
use crate::http::object;

/// A formatted answer: status 200, its content type before Starlette's charset rule,
/// and its body; or `Err` where Python raises (a NaN or an infinite time), the 500.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formatted {
    pub media_type: &'static str,
    pub body: String,
}

/// CPython's `float_floor_div` and `float_rem` (`_float_div_mod`).
fn div_mod(vx: f64, wx: f64) -> (f64, f64) {
    let mut m = vx % wx;
    let mut div = (vx - m) / wx;
    if m != 0.0 {
        if (wx < 0.0) != (m < 0.0) {
            m += wx;
            div -= 1.0;
        }
    } else {
        m = 0.0_f64.copysign(wx);
    }
    let floordiv = if div != 0.0 {
        let mut f = div.floor();
        if div - f > 0.5 {
            f += 1.0;
        }
        f
    } else {
        0.0_f64.copysign(vx / wx)
    };
    (floordiv, m)
}

/// `int(x)` of a float as decimal digits; `None` where Python raises.
fn py_int(x: f64) -> Option<String> {
    if !x.is_finite() {
        return None;
    }
    let t = x.trunc();
    Some(if t == 0.0 { "0".to_owned() } else { format!("{t:.0}") })
}

/// `f"{n:0{width}d}"` of an int given as digits.
fn pad(digits: &str, width: usize) -> String {
    let (sign, digits) = digits.strip_prefix('-').map_or(("", digits), |d| ("-", d));
    let zeros = width.saturating_sub(sign.len() + digits.len());
    format!("{sign}{}{digits}", "0".repeat(zeros))
}

/// `_ts_srt`.
fn ts_srt(seconds: f64) -> Option<String> {
    let h = py_int(div_mod(seconds, 3600.0).0)?;
    let m = py_int(div_mod(div_mod(seconds, 3600.0).1, 60.0).0)?;
    let s = py_int(div_mod(seconds, 60.0).1)?;
    let ms = py_int((seconds - seconds.trunc()) * 1000.0)?;
    Some(format!("{}:{}:{},{}", pad(&h, 2), pad(&m, 2), pad(&s, 2), pad(&ms, 3)))
}

/// `_ts_vtt`.
fn ts_vtt(seconds: f64) -> Option<String> {
    ts_srt(seconds).map(|t| t.replace(',', "."))
}

fn to_srt(segments: &[Span]) -> Option<String> {
    let mut out = Vec::new();
    for (i, s) in segments.iter().enumerate() {
        out.push(format!("{}\n{} --> {}\n{}\n", i + 1, ts_srt(s.start)?, ts_srt(s.end)?, py::strip(&s.text)));
    }
    Some(out.join("\n"))
}

fn to_vtt(segments: &[Span]) -> Option<String> {
    let mut lines = vec!["WEBVTT".to_owned(), String::new()];
    for s in segments {
        lines.push(format!("{} --> {}", ts_vtt(s.start)?, ts_vtt(s.end)?));
        lines.push(py::strip(&s.text).to_owned());
        lines.push(String::new());
    }
    Some(lines.join("\n"))
}

fn json(value: &Value) -> Option<Formatted> {
    crate::json::render(value).map(|body| Formatted { media_type: "application/json", body })
}

/// The text of all segments: `" ".join(seg.strip() for seg in segments)`.
#[must_use]
pub fn joined_text(segments: &[Span]) -> String {
    segments.iter().map(|s| py::strip(&s.text)).collect::<Vec<_>>().join(" ")
}

/// The answer in `fmt` (lowercased); an unknown format is `json`.
#[must_use]
pub fn format_response(fmt: &str, segments: &[Span], language: &str, words: Option<&[Span]>) -> Option<Formatted> {
    let text = joined_text(segments);
    match fmt {
        "text" => Some(Formatted { media_type: "text/plain", body: text }),
        "srt" => to_srt(segments).map(|body| Formatted { media_type: "application/x-subrip", body }),
        "vtt" => to_vtt(segments).map(|body| Formatted { media_type: "text/vtt", body }),
        "verbose_json" | "diarized_json" => {
            let total = segments.last().map_or(0.0, |s| s.end);
            let rows = segments
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    object([
                        ("id", Value::Int(i as i128)),
                        ("seek", Value::Int(0)),
                        ("start", Value::Float(s.start)),
                        ("end", Value::Float(s.end)),
                        ("text", Value::from(py::strip(&s.text))),
                        ("speaker", Value::Null),
                        ("tokens", Value::Array(Vec::new())),
                        ("temperature", Value::Float(0.0)),
                        ("avg_logprob", Value::Float(0.0)),
                        ("compression_ratio", Value::Float(0.0)),
                        ("no_speech_prob", Value::Float(0.0)),
                    ])
                })
                .collect();
            let mut body = Map::new();
            body.insert("task", Value::from("transcribe"));
            body.insert("language", Value::from(language));
            body.insert("duration", Value::Float(total));
            body.insert("text", Value::from(text));
            body.insert("segments", Value::Array(rows));
            if let Some(words) = words {
                let words = words
                    .iter()
                    .map(|w| {
                        object([
                            ("word", Value::from(w.text.as_str())),
                            ("start", Value::Float(w.start)),
                            ("end", Value::Float(w.end)),
                        ])
                    })
                    .collect();
                body.insert("words", Value::Array(words));
            }
            json(&Value::Object(body))
        }
        _ => json(&object([("text", Value::from(text))])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, start: f64, end: f64) -> Span {
        Span { text: text.into(), start, end }
    }

    #[test]
    fn times_past_an_hour_when_written_as_srt_then_pythons_truncation() {
        assert_eq!(ts_srt(3725.999).unwrap(), "01:02:05,998");
        assert_eq!(ts_srt(0.0).unwrap(), "00:00:00,000");
        assert_eq!(ts_srt(-1.5).unwrap(), "-1:59:58,-500");
        assert_eq!(ts_srt(f64::NAN), None);
    }

    #[test]
    fn two_segments_when_written_as_srt_and_vtt_then_blocks_joined_by_blank_lines() {
        let segments = [span(" Привет ", 0.5, 1.25), span("мир", 61.0, 62.0)];
        assert_eq!(
            to_srt(&segments).unwrap(),
            "1\n00:00:00,500 --> 00:00:01,250\nПривет\n\n2\n00:01:01,000 --> 00:01:02,000\nмир\n"
        );
        assert_eq!(
            to_vtt(&segments).unwrap(),
            "WEBVTT\n\n00:00:00.500 --> 00:00:01.250\nПривет\n\n00:01:01.000 --> 00:01:02.000\nмир\n"
        );
    }

    #[test]
    fn no_segments_when_formatted_then_empty_text_and_zero_duration() {
        let verbose = format_response("verbose_json", &[], "ru", None).unwrap();
        assert_eq!(verbose.body, r#"{"task":"transcribe","language":"ru","duration":0.0,"text":"","segments":[]}"#);
        assert_eq!(format_response("xml", &[], "ru", None).unwrap().body, r#"{"text":""}"#);
    }
}
