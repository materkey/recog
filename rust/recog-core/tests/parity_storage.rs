//! Parity with Python for the meeting storage and the WAV writer: scripted sequences
//! of operations run through `recog/meeting_storage.py` (and `IncrementalWavWriter`
//! of `recog/live.py`) and through the port, each in its own directory with the same
//! fixed clock, zone and meeting ids. Every answer, every printed line and every file
//! — bytes and mode — must be equal. The `[web] external_meetings_paths` entries
//! split the same way too.
//!
//! One legitimate difference is normalised: Python's title executor is made inline
//! by the harness, while the port's title thread is drained after each operation, so
//! the lines an operation prints are compared as a set, not in order.
#![forbid(unsafe_code)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use common::{assert_none, assert_same, difference, from_json, python, run_python, snapshot, to_json, unroot};
use md5::Digest as _;
use recog_core::meeting_storage::{
    self, GeneratedTitle, MeetingStorage, NewSegment, SegmentUpdate, SessionMode, StorageError, StorageOptions, Stream,
    TitleGenerator, TitleRequest,
};
use recog_core::pyjson::Value;
use recog_core::time::Clock;
use recog_core::wav::IncrementalWavWriter;
use serde_json::{Value as Json, json};

struct FakeClock {
    micros: AtomicI64,
    offset_s: i64,
}

impl Clock for FakeClock {
    fn now_micros(&self) -> i64 {
        self.micros.load(Ordering::SeqCst)
    }
    fn utc_offset(&self, _utc_micros: i64) -> i64 {
        self.offset_s
    }
}

/// The harness's fake `generate_title`, with the provenance its fake settings give.
struct FakeTitles;

impl TitleGenerator for FakeTitles {
    fn generate(&self, r: &TitleRequest) -> Result<Option<GeneratedTitle>, String> {
        if r.transcript.contains("boom") {
            return Err("boom".into());
        }
        if r.transcript.contains("untitled") {
            return Ok(None);
        }
        Ok(Some(GeneratedTitle {
            title: format!("T{}|{}|{}|{}", r.transcript.chars().count(), r.user_name, r.participants, r.meeting_title),
            provider: "claude".into(),
            model: Value::from("m1"),
            settings_revision: Value::from("r1"),
        }))
    }
}

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
}

fn write(path: &str, text: &str) -> Json {
    json!({"op": "write_file", "path": path, "base64": b64(text)})
}

fn error_json(e: &StorageError) -> Json {
    let kind = match e {
        StorageError::Json(_) => "Json",
        StorageError::Unicode(_) => "Unicode",
        StorageError::Value(_) => "Value",
        StorageError::Key(_) => "Key",
        StorageError::Type(_) => "Type",
        StorageError::Attribute(_) => "Attribute",
        StorageError::Os(_) => "Os",
    };
    json!({"error": [kind, e.to_string()]})
}

fn path_json(path: Option<PathBuf>) -> Json {
    path.map_or(Json::Null, |p| json!(p))
}

struct Runner {
    root: PathBuf,
    clock: Arc<FakeClock>,
    count: Arc<AtomicI64>,
    logs: Arc<Mutex<Vec<(Stream, String)>>>,
    aliases: Vec<(String, String)>,
    storage: Option<MeetingStorage>,
    results: Vec<Json>,
}

impl Runner {
    fn new(root: &Path, offset_s: i64, aliases: &Json) -> Self {
        let aliases = aliases
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned())).collect())
            .unwrap_or_default();
        Self {
            root: root.to_path_buf(),
            clock: Arc::new(FakeClock { micros: AtomicI64::new(0), offset_s }),
            count: Arc::new(AtomicI64::new(0)),
            logs: Arc::new(Mutex::new(Vec::new())),
            aliases,
            storage: None,
            results: Vec::new(),
        }
    }

    fn reference(&self, value: &Json) -> String {
        match value.as_str() {
            Some(s) if s.starts_with('@') => {
                self.results[s[1..].parse::<usize>().unwrap()].as_str().unwrap().to_owned()
            }
            Some(s) => s.to_owned(),
            None => panic!("not an id: {value}"),
        }
    }

    fn storage(&self) -> &MeetingStorage {
        self.storage.as_ref().expect("no storage: a script starts with new")
    }

    fn options(&self, external: &[(PathBuf, Option<String>)]) -> StorageOptions {
        let mut options = StorageOptions::new(&self.root.join("home"));
        options.external_paths = external.to_vec();
        options.source_aliases.clone_from(&self.aliases);
        options.external_index_cache_dir = self.root.join("cache");
        options.clock = self.clock.clone();
        let count = Arc::clone(&self.count);
        options.meeting_suffix = Arc::new(move || format!("{:08x}", count.fetch_add(1, Ordering::SeqCst) + 1));
        options.title_generator = Some(Arc::new(FakeTitles));
        let logs = Arc::clone(&self.logs);
        options.log = Arc::new(move |stream, line| logs.lock().unwrap().push((stream, line.to_owned())));
        options
    }

    fn run(&mut self, op: &Json) -> Json {
        let value = |r: Result<Value, StorageError>| r.map_or_else(|e| error_json(&e), |v| to_json(&v));
        let meeting_arg = |me: &Self| {
            op.get("meeting").filter(|m| !m.is_null()).map(|m| me.storage().base_path().join(me.reference(m)))
        };
        match op["op"].as_str().unwrap() {
            "set_time" => {
                self.clock.micros.store(op["micros"].as_i64().unwrap(), Ordering::SeqCst);
                Json::Null
            }
            "advance" => {
                self.clock.micros.fetch_add(op["micros"].as_i64().unwrap(), Ordering::SeqCst);
                Json::Null
            }
            "write_file" => {
                let path = self.root.join(op["path"].as_str().unwrap());
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(
                    path,
                    base64::engine::general_purpose::STANDARD.decode(op["base64"].as_str().unwrap()).unwrap(),
                )
                .unwrap();
                Json::Null
            }
            "touch" => {
                let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs_f64(op["mtime"].as_f64().unwrap());
                let file = fs::File::options().write(true).open(self.root.join(op["path"].as_str().unwrap())).unwrap();
                file.set_times(fs::FileTimes::new().set_accessed(at).set_modified(at)).unwrap();
                Json::Null
            }
            "chmod" => {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = u32::try_from(op["mode"].as_u64().unwrap()).unwrap();
                fs::set_permissions(self.root.join(op["path"].as_str().unwrap()), fs::Permissions::from_mode(mode))
                    .unwrap();
                Json::Null
            }
            "new" => {
                let external: Vec<(PathBuf, Option<String>)> = op
                    .get("external")
                    .and_then(Json::as_array)
                    .map(|list| {
                        list.iter()
                            .map(|e| (self.root.join(e[0].as_str().unwrap()), e[1].as_str().map(str::to_owned)))
                            .collect()
                    })
                    .unwrap_or_default();
                self.storage = None;
                self.storage = Some(MeetingStorage::new(&self.root.join("meetings"), self.options(&external)).unwrap());
                Json::Null
            }
            "start_session" => {
                let a = &op["args"];
                let flag = |k: &str| a.get(k).and_then(Json::as_bool).unwrap_or(false);
                let mode = SessionMode {
                    mic: flag("mic"),
                    system_audio: flag("system_audio"),
                    ktalk: flag("ktalk"),
                    screencapture: flag("screencapture"),
                    system_audio_backend: a
                        .get("system_audio_backend")
                        .and_then(Json::as_str)
                        .unwrap_or("none")
                        .to_owned(),
                    chunk_duration_ms: a.get("chunk_duration_ms").and_then(Json::as_i64).unwrap_or(3000),
                    ktalk_only: flag("ktalk_only"),
                };
                self.storage().start_session(&mode).map_or_else(|e| error_json(&e), |id| json!(id))
            }
            "join" => self
                .storage()
                .on_meeting_join(
                    op["url"].as_str().unwrap(),
                    op.get("title").and_then(Json::as_str),
                    op["platform"].as_str().unwrap(),
                )
                .map_or_else(|e| error_json(&e), |id| json!(id)),
            "ktalk_join" => self
                .storage()
                .on_ktalk_join(op["url"].as_str().unwrap(), op.get("title").and_then(Json::as_str))
                .map_or_else(|e| error_json(&e), |id| json!(id)),
            "leave" => self
                .storage()
                .on_ktalk_leave(op["reason"].as_str().unwrap())
                .map_or_else(|e| error_json(&e), |id| json!(id)),
            "split" => self.storage().split_on_silence_timeout().map_or_else(|e| error_json(&e), |id| json!(id)),
            "end_session" => self.storage().end_session().map_or_else(|e| error_json(&e), |id| json!(id)),
            "append" => {
                let a = &op["args"];
                let text = |k: &str| a.get(k).and_then(Json::as_str).unwrap_or("").to_owned();
                let flag = |k: &str| a.get(k).and_then(Json::as_bool).unwrap_or(false);
                let segment = NewSegment {
                    speaker: text("speaker"),
                    speaker_id: text("speaker_id"),
                    text: text("text"),
                    is_local: flag("is_local"),
                    is_continuation: flag("is_continuation"),
                    uncertain: flag("uncertain"),
                    uncertain_reason: text("uncertain_reason"),
                    audio_offset_ms: a.get("audio_offset_ms").and_then(Json::as_i64).unwrap_or(0),
                    captured_at: a.get("captured_at").and_then(Json::as_f64),
                };
                self.storage().append_segment(&segment).map_or_else(|e| error_json(&e), |id| json!(id))
            }
            "update_segments" => {
                let updates: Vec<SegmentUpdate> = op["updates"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|u| SegmentUpdate {
                        segment_id: u["segment_id"].as_i64().unwrap(),
                        speaker: u.get("speaker").and_then(Json::as_str).map(str::to_owned),
                        uncertain: u.get("uncertain").and_then(Json::as_bool),
                        uncertain_reason: u.get("uncertain_reason").and_then(Json::as_str).map(str::to_owned),
                        text: u.get("text").and_then(Json::as_str).map(str::to_owned),
                    })
                    .collect();
                let target = meeting_arg(self);
                self.storage()
                    .update_segments(&updates, target.as_deref())
                    .map_or_else(|e| error_json(&e), |()| Json::Null)
            }
            "update_title" => self
                .storage()
                .update_title(op["title"].as_str().unwrap())
                .map_or_else(|e| error_json(&e), |()| Json::Null),
            "update_participants" => {
                let participants = from_json(&op["participants"]);
                self.storage()
                    .update_participants(participants.as_object().unwrap())
                    .map_or_else(|e| error_json(&e), |()| Json::Null)
            }
            "current_transcript" => value(self.storage().get_current_transcript().map(Value::Array)),
            "list_meetings" => value(
                self.storage()
                    .list_meetings(op["include_active"].as_bool().unwrap(), op["refresh"].as_bool().unwrap())
                    .map(Value::Array),
            ),
            "list_active" => value(self.storage().list_active_meetings().map(Value::Array)),
            "get_meeting" => {
                value(self.storage().get_meeting(&self.reference(&op["id"])).map(|m| m.unwrap_or(Value::Null)))
            }
            "find_meeting_path" => path_json(self.storage().find_meeting_path(&self.reference(&op["id"]))),
            "get_audio_paths" => self
                .storage()
                .get_audio_paths(&self.reference(&op["id"]))
                .map_or_else(|e| error_json(&e), |(m, s)| json!([path_json(m), path_json(s)])),
            "set_recording_active" => self
                .storage()
                .set_recording_active(op["active"].as_bool().unwrap())
                .map_or_else(|e| error_json(&e), |()| Json::Null),
            "set_audio_paths" => {
                let target = meeting_arg(self);
                self.storage()
                    .set_audio_paths(
                        op.get("mic").and_then(Json::as_str),
                        op.get("system").and_then(Json::as_str),
                        target.as_deref(),
                    )
                    .map_or_else(|e| error_json(&e), |()| Json::Null)
            }
            "create_backup" => self
                .storage()
                .create_transcript_backup(&self.reference(&op["id"]))
                .map_or_else(|e| error_json(&e), path_json),
            "restore_backup" => self
                .storage()
                .restore_transcript_backup(&self.reference(&op["id"]))
                .map_or_else(|e| error_json(&e), |b| json!(b)),
            "cleanup" => self
                .storage()
                .cleanup_old_audio(op["hours"].as_f64().unwrap())
                .map_or_else(|e| error_json(&e), |n| json!(n)),
            "delete" => {
                self.storage().delete_meeting(&self.reference(&op["id"])).map_or_else(|e| error_json(&e), |b| json!(b))
            }
            "set_diarization" => self
                .storage()
                .set_diarization_result(
                    &self.reference(&op["id"]),
                    op["track"].as_str().unwrap(),
                    op["speakers"].as_i64().unwrap(),
                )
                .map_or_else(|e| error_json(&e), |()| Json::Null),
            "is_conference_source" => json!(self.storage().is_conference_source(op["source"].as_str())),
            "current_id" => json!(self.storage().current_meeting_id()),
            other => panic!("unknown op {other}"),
        }
    }

    fn run_all(&mut self, ops: &[Json]) -> Vec<Json> {
        let mut logs = Vec::new();
        for op in ops {
            let result = self.run(op);
            self.results.push(result);
            if let Some(storage) = &self.storage {
                storage.drain_titles();
            }
            let lines = std::mem::take(&mut *self.logs.lock().unwrap());
            let pick = |s: Stream| {
                let mut l: Vec<String> = lines.iter().filter(|(x, _)| *x == s).map(|(_, l)| l.clone()).collect();
                l.sort();
                l
            };
            logs.push(json!({"stdout": pick(Stream::Stdout), "stderr": pick(Stream::Stderr)}));
        }
        logs
    }
}

fn sorted_logs(logs: &Json) -> Vec<Json> {
    logs.as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            let sort = |k: &str| {
                let mut lines: Vec<String> =
                    entry[k].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_owned()).collect();
                lines.sort();
                lines
            };
            json!({"stdout": sort("stdout"), "stderr": sort("stderr")})
        })
        .collect()
}

/// Run `ops` on both sides and compare answers, printed lines and files.
fn check_script(name: &str, ops: &[Json], tz: &str, offset_s: i64, aliases: &Json) {
    let Some(python) = python() else { return };
    let root = tempfile::tempdir().unwrap();
    let (py_root, rs_root) = (root.path().join("python"), root.path().join("rust"));
    fs::create_dir_all(&py_root).unwrap();
    fs::create_dir_all(&rs_root).unwrap();
    let request = json!({"root": py_root, "ops": ops, "aliases": aliases});
    let answer = run_python(&python, "meeting_storage", &request, &root.path().join("sandbox"), tz);
    let mut runner = Runner::new(&rs_root, offset_s, aliases);
    let logs = runner.run_all(ops);
    drop(runner.storage.take());
    let mut failures = Vec::new();
    let py_logs = sorted_logs(&answer["logs"]);
    for (i, op) in ops.iter().enumerate() {
        let expected = unroot(&answer["results"][i], &[&py_root]);
        let got = unroot(&runner.results[i], &[&rs_root]);
        failures.extend(difference(&format!("{name} op {i} {op}"), &expected, &got));
        let (expected, got) = (unroot(&py_logs[i], &[&py_root]), unroot(&logs[i], &[&rs_root]));
        failures.extend(difference(&format!("{name} op {i} output {op}"), &expected, &got));
    }
    // The external index cache is named by an md5 of the absolute directory, which
    // differs between the sides: each side's names are mapped back to the directory.
    let externals: Vec<(String, String)> = ops
        .iter()
        .filter_map(|op| op.get("external").and_then(Json::as_array))
        .flatten()
        .map(|e| (e[0].as_str().unwrap().to_owned(), e[1].as_str().unwrap_or("None").to_owned()))
        .collect();
    let files = |side: &Path| -> Json {
        let names: Vec<(String, String)> = externals
            .iter()
            .map(|(p, s)| {
                let digest = md5::Md5::digest(format!("{}/{p}:{s}", side.display()).as_bytes());
                let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
                (format!("cache/{}.json", &hex[..8]), format!("cache/<{p}:{s}>.json"))
            })
            .collect();
        snapshot(side)
            .into_iter()
            .map(|(k, v)| {
                (names.iter().find(|(hashed, _)| *hashed == k).map_or(k, |(_, named)| named.clone()), json!(v))
            })
            .collect()
    };
    failures.extend(difference(&format!("{name} files"), &files(&py_root), &files(&rs_root)));
    assert_none(&failures);
}

const T0: i64 = 1_762_596_000_123_456; // 2025-11-08T10:00:00.123456Z
const SECOND: i64 = 1_000_000;
const MINUTE: i64 = 60 * SECOND;

fn meta(id: &str, extra: &str) -> String {
    format!(
        r#"{{"id": "{id}", "version": 1, "created_at": "2025-11-07T09:00:00+00:00", "ended_at": null,
            "updated_at": "2025-11-07T09:30:00.5+00:00", "source": "ktalk", "end_reason": null, "mode": {{}},
            "conference": {{"live_url": "https://x.ktalk.ru/room", "live_id": "room", "host": null}},
            "title": "Old", "title_source": "ktalk", "prev_meeting_id": null, "next_meeting_id": null,
            "participants": {{"p": {{"name": "Пётр", "is_local": false}}}},
            "stats": {{"segment_count": 3, "duration_seconds": 0, "words_count": 7}}{extra}}}"#
    )
}

fn segment(args: Json) -> Json {
    json!({"op": "append", "args": args})
}

#[test]
fn a_day_of_meetings_leaves_the_same_files_and_answers_as_python() {
    let ops = vec![
        json!({"op": "set_time", "micros": T0}),
        write("meetings/2025-11-07_090000_stale/meta.json", &meta("2025-11-07_090000_stale", "")),
        write("meetings/2025-11-07_090000_stale/transcript.jsonl", "{\"id\": 1}\n"),
        write(
            "meetings/2025-11-07_080000_empty/meta.json",
            &meta("2025-11-07_080000_empty", "").replace("\"segment_count\": 3", "\"segment_count\": 0"),
        ),
        write("meetings/broken/meta.json", "{\"id\": "),
        write("meetings/nocreated/meta.json", r#"{"id": "nocreated", "ended_at": null}"#),
        write(
            "meetings/naive/meta.json",
            r#"{"id": "naive", "ended_at": "2025-11-07T09:00:00", "created_at": "2025-11-07T08:00:00", "stats": {"segment_count": 1}}"#,
        ),
        write("meetings/notes.txt", "not a meeting"),
        write(
            "meetings/2025-11-06_100000_old/meta.json",
            &meta("2025-11-06_100000_old", r#", "audio_mic_path": "m", "audio_system_path": "s""#)
                .replace("\"ended_at\": null", "\"ended_at\": \"2025-11-06T11:00:00+00:00\""),
        ),
        write("meetings/2025-11-06_100000_old/audio_mic.wav", "RIFF old"),
        write("meetings/2025-11-06_100000_old/audio_system.wav", "RIFF fresh"),
        json!({"op": "touch", "path": "meetings/2025-11-06_100000_old/audio_mic.wav", "mtime": 1_762_400_000.25}),
        write("meetings/nometa/audio_system.wav", "RIFF"),
        json!({"op": "touch", "path": "meetings/nometa/audio_system.wav", "mtime": 1_762_400_000.0}),
        json!({"op": "new"}),
        json!({"op": "start_session", "args": {"mic": true, "system_audio": true, "ktalk": true, "screencapture": true,
                "system_audio_backend": "screencapture", "chunk_duration_ms": 3000}}),
        segment(json!({"speaker": "Me", "speaker_id": "mic", "text": "до встречи  ", "is_local": true})),
        json!({"op": "advance", "micros": 5 * SECOND}),
        json!({"op": "join", "url": "https://example.ktalk.ru/abcd1234wxyz?x=1", "title": "Daily", "platform": "ktalk"}),
        json!({"op": "update_participants", "participants": {"p1": {"name": "Anna"}, "me": {"name": "Vyacheslav", "is_local": true},
                "p2": {"is_local": false}, "self2": {"name": "Other local", "is_local": true}}}),
        segment(
            json!({"speaker": "Anna", "speaker_id": "p1", "text": "привет\tвсем \u{a0}тут", "audio_offset_ms": 1500}),
        ),
        segment(
            json!({"speaker": "Me", "speaker_id": "mic", "text": "да", "is_local": true, "uncertain": true, "uncertain_reason": "local"}),
        ),
        segment(
            json!({"speaker": "Anna K", "speaker_id": "p1", "text": "ещё \"кавычки\" и \\ слэш", "captured_at": 1_762_596_000.000_001_5}),
        ),
        segment(
            json!({"speaker": "Bob", "speaker_id": "p3", "text": "", "is_continuation": true, "captured_at": 1_762_595_990.5}),
        ),
        json!({"op": "update_segments", "updates": [
            {"segment_id": 2, "text": "исправленный текст здесь"},
            {"segment_id": 3, "speaker": "Bob", "uncertain": false, "uncertain_reason": ""},
            {"segment_id": 99, "text": "nothing"},
            {"segment_id": 4, "text": "a"}, {"segment_id": 4, "text": "b c"}]}),
        json!({"op": "update_title", "title": "Joining the meeting"}),
        json!({"op": "set_recording_active", "active": true}),
        json!({"op": "set_audio_paths", "mic": "/x/audio_mic.wav", "system": ""}),
        json!({"op": "current_transcript"}),
        json!({"op": "list_active"}),
        json!({"op": "list_meetings", "include_active": true, "refresh": false}),
        json!({"op": "current_id"}),
        json!({"op": "advance", "micros": MINUTE + 1}),
        json!({"op": "leave", "reason": "user_left"}),
        json!({"op": "advance", "micros": 2 * MINUTE}),
        json!({"op": "ktalk_join", "url": "https://example.ktalk.ru/abcd1234wxyz", "title": "Other title"}),
        json!({"op": "leave", "reason": "call_ended"}),
        json!({"op": "advance", "micros": 11 * MINUTE}),
        json!({"op": "join", "url": "https://example.ktalk.ru/abcd1234wxyz", "platform": "ktalk"}),
        json!({"op": "join", "url": "https://element.example.org/#/room/#recog-spike:example.org", "platform": "matrix"}),
        segment(json!({"speaker": "Eve", "speaker_id": "@eve:example.org:DEV", "text": "matrix words here"})),
        json!({"op": "split"}),
        json!({"op": "leave", "reason": "page_closed"}),
        segment(json!({"speaker": "Me", "speaker_id": "mic", "text": "один", "is_local": true})),
        json!({"op": "advance", "micros": 11 * MINUTE}),
        json!({"op": "split"}),
        json!({"op": "update_title", "title": "Joining the meeting"}),
        json!({"op": "update_title", "title": "Real title"}),
        json!({"op": "update_title", "title": "Too late"}),
        json!({"op": "join", "url": "https://meet.google.com/abc-defg-hij;params", "platform": "gmeet"}),
        segment(json!({"speaker": "Gina", "speaker_id": "g", "text": "meet"})),
        json!({"op": "end_session"}),
        json!({"op": "split"}),
        json!({"op": "update_segments", "updates": [{"segment_id": 1, "text": "x"}]}),
        json!({"op": "update_segments", "meeting": "@51", "updates": [{"segment_id": 1, "text": "changed after the end"}]}),
        json!({"op": "list_meetings", "include_active": false, "refresh": false}),
        json!({"op": "get_meeting", "id": "@18"}),
        json!({"op": "get_meeting", "id": "2025-11-06_100000_old"}),
        json!({"op": "get_meeting", "id": "missing"}),
        json!({"op": "get_audio_paths", "id": "2025-11-06_100000_old"}),
        json!({"op": "find_meeting_path", "id": "@18"}),
        json!({"op": "create_backup", "id": "@18"}),
        json!({"op": "create_backup", "id": "@18"}),
        json!({"op": "set_diarization", "id": "@18", "track": "mic", "speakers": 2}),
        json!({"op": "restore_backup", "id": "@18"}),
        json!({"op": "restore_backup", "id": "@18"}),
        json!({"op": "delete", "id": "missing"}),
        json!({"op": "delete", "id": "nocreated"}),
        json!({"op": "delete", "id": "@39"}),
        json!({"op": "cleanup", "hours": 0.5}),
        json!({"op": "is_conference_source", "source": "ktalk"}),
        json!({"op": "is_conference_source", "source": "old-meet"}),
        json!({"op": "is_conference_source", "source": "orphan"}),
        json!({"op": "is_conference_source", "source": null}),
    ];
    check_script("day", &ops, "Etc/GMT-3", 3 * 3600, &json!({"old-meet": "jitsi"}));
}

#[test]
fn ended_meetings_get_generated_titles_and_failures_print_like_python() {
    let ops = vec![
        json!({"op": "set_time", "micros": T0}),
        json!({"op": "new"}),
        json!({"op": "start_session", "args": {"ktalk": true}}),
        segment(json!({"speaker": "Me", "speaker_id": "mic", "text": "plain words", "is_local": true})),
        json!({"op": "split"}),
        json!({"op": "advance", "micros": 11 * MINUTE}),
        segment(json!({"speaker": "Me", "speaker_id": "mic", "text": "this goes boom", "is_local": true})),
        json!({"op": "split"}),
        segment(json!({"speaker": "Me", "speaker_id": "mic", "text": "stays untitled", "is_local": true})),
        json!({"op": "leave", "reason": "user_left"}),
        json!({"op": "join", "url": "https://zoom.us/j/123", "platform": "zoom"}),
        json!({"op": "update_participants", "participants": {"a": {"name": "Ann", "is_local": true}, "b": {"name": "Ben"}}}),
        segment(json!({"speaker": "Ben", "speaker_id": "b", "text": "zoom talk"})),
        json!({"op": "advance", "micros": 30 * SECOND}),
        json!({"op": "leave", "reason": "call_ended"}),
        json!({"op": "advance", "micros": 30 * SECOND}),
        json!({"op": "join", "url": "https://zoom.us/j/123", "platform": "zoom"}),
        segment(json!({"speaker": "Ben", "speaker_id": "b", "text": "back again"})),
        json!({"op": "join", "url": "https://other.example.org/daily", "platform": "jitsi", "title": "Jitsi"}),
        json!({"op": "join", "url": "https://element.example.org/#/room//#/room/!r:x?y", "platform": "matrix"}),
        json!({"op": "join", "url": "https://element.example.org/", "platform": "matrix"}),
        json!({"op": "end_session"}),
        json!({"op": "list_meetings", "include_active": true, "refresh": false}),
    ];
    check_script("titles", &ops, "UTC", 0, &json!({}));
}

#[test]
fn a_rejoin_after_local_midnight_starts_a_new_meeting_like_python() {
    // 20:59 UTC is 23:59 at UTC+3: the rejoin two minutes later is on the next local day.
    let ops = vec![
        json!({"op": "set_time", "micros": 1_762_635_540_000_000_i64}),
        json!({"op": "new"}),
        json!({"op": "start_session", "args": {"ktalk": true, "ktalk_only": true}}),
        json!({"op": "list_active"}),
        json!({"op": "join", "url": "https://team.ktalk.ru/room", "platform": "ktalk"}),
        segment(json!({"speaker": "A", "speaker_id": "a", "text": "late"})),
        json!({"op": "leave", "reason": "user_left"}),
        json!({"op": "advance", "micros": 2 * MINUTE}),
        json!({"op": "join", "url": "https://team.ktalk.ru/room", "platform": "ktalk"}),
        segment(json!({"speaker": "A", "speaker_id": "a", "text": "early", "captured_at": 1_762_635_600.0})),
        json!({"op": "chmod", "path": "meetings/2025-11-08_205900_00000002/meta.json", "mode": 0o600}),
        json!({"op": "set_recording_active", "active": false}),
        json!({"op": "list_active"}),
        json!({"op": "end_session"}),
    ];
    check_script("midnight", &ops, "Etc/GMT-3", 3 * 3600, &json!({}));
}

#[test]
fn external_meeting_directories_are_listed_and_cached_like_python() {
    let ext_meta = |id: &str, source: &str| {
        format!(
            r#"{{"id": "{id}", "created_at": "2025-11-0{}T10:00:00+00:00", "ended_at": "x", "source": "{source}",
                "stats": {{"duration_seconds": 5}}, "ktalk": {{"live_url": "https://k"}}, "participants": {{"x": {{}}, "y": {{"name": "Y"}}}}}}"#,
            &id[id.len() - 1..]
        )
    };
    let ops = vec![
        json!({"op": "set_time", "micros": T0}),
        write("ext/a/meetings/m1/meta.json", &ext_meta("m1", "old-meet")),
        write("ext/a/meetings/m2/meta.json", &ext_meta("m2", "ktalk")),
        write("ext/a/meetings/bad/meta.json", "[1, 2"),
        write("ext/a/meetings/noid/meta.json", "{}"),
        write("ext/b/m3/meta.json", &ext_meta("m3", "ktalk")),
        write(
            "meetings/index.json",
            r#"{"meetings": [{"id": "z", "created_at": "2025-11-05T00:00:00+00:00", "source": "old-meet"}, {"id": "y"}]}"#,
        ),
        json!({"op": "new", "external": [["ext/a/meetings", "phone"], ["ext/b", null], ["ext/missing", "x"]]}),
        json!({"op": "list_meetings", "include_active": true, "refresh": false}),
        json!({"op": "list_meetings", "include_active": false, "refresh": false}),
        write("ext/a/meetings/m4/meta.json", &ext_meta("m4", "ktalk")),
        json!({"op": "list_meetings", "include_active": false, "refresh": true}),
        json!({"op": "get_meeting", "id": "m3"}),
        json!({"op": "find_meeting_path", "id": "m1"}),
        json!({"op": "delete", "id": "m3"}),
        json!({"op": "get_audio_paths", "id": "m3"}),
    ];
    check_script("external", &ops, "UTC", 0, &json!({"old-meet": "jitsi"}));
}

#[test]
fn malformed_files_fail_or_are_skipped_as_python_does() {
    let ops = vec![
        json!({"op": "set_time", "micros": T0}),
        write(
            "meetings/index.json",
            r#"{"meetings": [{"id": "a", "created_at": null}, {"id": "b", "created_at": "2025"}]}"#,
        ),
        json!({"op": "new"}),
        json!({"op": "list_meetings", "include_active": false, "refresh": false}),
        write("meetings/index.json", "{\"meetings\": 5}"),
        json!({"op": "start_session", "args": {}}),
        segment(json!({"speaker": "S", "speaker_id": "s", "text": "local mode"})),
        json!({"op": "current_transcript"}),
        json!({"op": "end_session"}),
        write("meetings/index.json", "{\"meetings\": []}"),
        write(
            "meetings/active/meta.json",
            r#"{"id": "active", "created_at": "bad date", "participants": {"p": {"name": "P"}}}"#,
        ),
        write("meetings/active2/meta.json", r#"{"id": "active2", "created_at": 5, "participants": []}"#),
        json!({"op": "list_active"}),
        write(
            "meetings/active2/meta.json",
            r#"{"id": "active2", "created_at": "2025-11-08T09:00:00Z", "participants": {"p": {}}}"#,
        ),
        json!({"op": "list_active"}),
        write("meetings/active2/meta.json", "\u{feff}{}"),
        json!({"op": "list_active"}),
        json!({"op": "delete", "id": "active"}),
        write(
            "meetings/rows/meta.json",
            r#"{"id": "rows", "ended_at": "2025-11-08T09:00:00Z", "stats": {"words_count": "4"}}"#,
        ),
        write(
            "meetings/rows/transcript.jsonl",
            "\r\n {\"id\": 1, \"text\": \"a b\"}\r\nnot json\n{\"id\": 2.0, \"text\": 5}\n\n{\"id\": true}\n",
        ),
        json!({"op": "update_segments", "meeting": "rows", "updates": [{"segment_id": 1, "text": "one"}, {"segment_id": 2, "text": "x y z"}]}),
        json!({"op": "get_meeting", "id": "rows"}),
        write("meetings/rows/transcript.jsonl", "[1]\n"),
        json!({"op": "update_segments", "meeting": "rows", "updates": [{"segment_id": 1, "text": "one"}]}),
        write(
            "meetings/stale/meta.json",
            r#"{"id": "stale", "ended_at": null, "created_at": "2025-11-08T09:00:00", "updated_at": "2025-11-08T09:10:00+00:00", "stats": {}}"#,
        ),
        json!({"op": "start_session", "args": {}}),
        json!({"op": "end_session"}),
    ];
    check_script("malformed", &ops, "UTC", 0, &json!({}));
}

#[test]
fn wav_writes_match_python_byte_for_byte() {
    // given
    let Some(python) = python() else { return };
    let root = tempfile::tempdir().unwrap();
    let (py_root, rs_root) = (root.path().join("python"), root.path().join("rust"));
    fs::create_dir_all(&py_root).unwrap();
    fs::create_dir_all(&rs_root).unwrap();
    let samples: Vec<Json> =
        (0..40).map(|i| json!((f64::from(i) * 0.37).sin() * if i % 7 == 0 { 1.2 } else { 0.9 })).collect();
    let mut tricky: Vec<Json> = [0.0, -0.0, 1.0, -1.0, 0.999_99, -0.999_99, 1.5, -1.5, 3.0, 1e-9, 0.5 / 32767.0]
        .iter()
        .map(|x| json!(x))
        .collect();
    tricky.extend([json!("nan"), json!("inf"), json!("-inf")]);
    let script = |path: &Path| {
        vec![
            json!({"op": "new", "path": path.join("a/audio_mic.wav"), "sample_rate": 10, "channels": 1, "overlap_seconds": 0.5, "max_data_bytes": 200}),
            json!({"op": "write", "samples": samples}),
            json!({"op": "open", "continuation": false}),
            json!({"op": "write", "samples": samples}),
            json!({"op": "write", "samples": &samples[..3]}),
            json!({"op": "write", "samples": tricky}),
            json!({"op": "mark_discontinuity"}),
            json!({"op": "write", "samples": &samples[..20]}),
            json!({"op": "offset"}),
            json!({"op": "write", "samples": samples}),
            json!({"op": "write", "samples": samples}),
            json!({"op": "finalize"}),
            json!({"op": "finalize"}),
            json!({"op": "new", "path": path.join("a/audio_mic.wav"), "sample_rate": 10, "channels": 1, "overlap_seconds": 0.5, "max_data_bytes": 2_147_483_648_u64}),
            json!({"op": "open", "continuation": true}),
            json!({"op": "offset"}),
            json!({"op": "write", "samples": samples}),
            json!({"op": "new", "path": path.join("b/short.wav"), "sample_rate": 16000, "channels": 2, "overlap_seconds": 0.25, "max_data_bytes": 2_147_483_648_u64}),
            json!({"op": "open", "continuation": true}),
            json!({"op": "write", "samples": samples}),
        ]
    };
    fs::create_dir_all(py_root.join("b")).unwrap();
    fs::write(py_root.join("b/short.wav"), "short").unwrap();
    fs::create_dir_all(rs_root.join("b")).unwrap();
    fs::write(rs_root.join("b/short.wav"), "short").unwrap();
    // when
    let answer = run_python(&python, "wav", &json!({"ops": script(&py_root)}), &root.path().join("sandbox"), "UTC");
    let mut writer: Option<IncrementalWavWriter> = None;
    let mut results = Vec::new();
    for op in script(&rs_root) {
        let result = match op["op"].as_str().unwrap() {
            "new" => {
                writer = Some(IncrementalWavWriter::with_settings(
                    Path::new(op["path"].as_str().unwrap()),
                    u32::try_from(op["sample_rate"].as_u64().unwrap()).unwrap(),
                    u16::try_from(op["channels"].as_u64().unwrap()).unwrap(),
                    op["overlap_seconds"].as_f64().unwrap(),
                    op["max_data_bytes"].as_u64().unwrap(),
                ));
                Json::Null
            }
            "open" => {
                writer.as_mut().unwrap().open(op["continuation"].as_bool().unwrap()).unwrap();
                Json::Null
            }
            "write" => {
                #[allow(clippy::cast_possible_truncation)]
                let chunk: Vec<f32> = op["samples"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| match x.as_str() {
                        Some("nan") => f32::NAN,
                        Some("inf") => f32::INFINITY,
                        Some("-inf") => f32::NEG_INFINITY,
                        _ => x.as_f64().unwrap() as f32,
                    })
                    .collect();
                json!(writer.as_mut().unwrap().write_chunk(&chunk).unwrap())
            }
            "mark_discontinuity" => {
                writer.as_mut().unwrap().mark_discontinuity();
                Json::Null
            }
            "offset" => json!(writer.as_ref().unwrap().offset_ms()),
            "finalize" => {
                writer.as_mut().unwrap().finalize().unwrap();
                Json::Null
            }
            other => panic!("unknown op {other}"),
        };
        results.push(result);
    }
    drop(writer);
    // then
    let files = |side: &Path| -> Json { snapshot(side).into_iter().map(|(k, v)| (k, json!(v))).collect() };
    let mut failures = Vec::new();
    failures.extend(difference("results", &answer["results"], &Json::Array(results)));
    failures.extend(difference("files", &files(&py_root), &files(&rs_root)));
    assert_none(&failures);
}

#[test]
fn external_meeting_paths_split_like_python() {
    // given
    let Some(python) = python() else { return };
    let cases: Vec<Vec<&str>> = vec![
        vec![
            "/a/meetings:a",
            "/b/meetings",
            " /c/x : src ",
            "meetings",
            "/",
            "//x/y/",
            "a//b/./c",
            "x:",
            ":y",
            "/a:b:c",
        ],
        vec![],
    ];
    // when
    let answer =
        run_python(&python, "external_paths", &json!({"cases": cases}), tempfile::tempdir().unwrap().path(), "UTC");
    // then
    for (i, entries) in cases.iter().enumerate() {
        let entries: Vec<String> = entries.iter().map(|s| (*s).to_owned()).collect();
        let got: Vec<Json> =
            meeting_storage::external_paths(&entries).into_iter().map(|(path, source)| json!([path, source])).collect();
        assert_same(&format!("{entries:?}"), &answer["results"][i], &Json::Array(got));
    }
}
