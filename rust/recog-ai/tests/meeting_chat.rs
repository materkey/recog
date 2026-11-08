//! The module-level contract behind tests/test_meeting_chat.py: the HTTP cases there
//! (status codes, request validation) belong to the web backend, the dialog file and
//! the prompt of the next question are here.

use std::path::Path;

use recog_ai::json::Json;
use recog_ai::meeting_chat::{ChatTurn, PROMPT_HISTORY_TURNS, append_turn, build_chat_prompt, clear_turns, read_turns};

fn turn(question: &str, answer: &str) -> ChatTurn {
    ChatTurn::new("2026-11-02T08:40:00+00:00", question, answer, "Fake (model-x)", "defaults", false)
}

fn questions(meeting: &Path) -> Vec<String> {
    read_turns(meeting).unwrap().into_iter().map(|turn| turn.question).collect()
}

#[test]
fn answered_question_when_meeting_reopened_then_dialog_is_still_there() {
    let dir = tempfile::tempdir().unwrap();
    append_turn(dir.path(), &turn("Что сказал Пётр?", "Пётр за синюю кнопку [11:30:14].")).unwrap();
    let turns = read_turns(dir.path()).unwrap();
    assert_eq!(turns, [turn("Что сказал Пётр?", "Пётр за синюю кнопку [11:30:14].")]);
}

#[test]
fn saved_dialog_when_searched_on_disk_then_russian_text_is_readable_and_python_formatted() {
    let dir = tempfile::tempdir().unwrap();
    append_turn(dir.path(), &turn("Что сказал Пётр?", "Да.")).unwrap();
    let text = std::fs::read_to_string(dir.path().join("chat.jsonl")).unwrap();
    assert_eq!(
        text,
        "{\"asked_at\": \"2026-11-02T08:40:00+00:00\", \"question\": \"Что сказал Пётр?\", \"answer\": \"Да.\", \"provider\": \"Fake (model-x)\", \"settings_revision\": \"defaults\", \"transcript_truncated\": false}\n"
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(dir.path().join("chat.jsonl")).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn follow_up_question_when_prompted_then_earlier_dialog_comes_before_it() {
    let history = [turn("Что сказал Пётр?", "Пётр за синюю кнопку [11:30:14].")];
    let prompt = build_chat_prompt("Инструкции\n{transcript}\n\n", &history, "А он уверен?");
    assert_eq!(
        prompt,
        "Инструкции\n{transcript}\n\nПредыдущие вопросы и ответы этого разговора:\n\nВопрос: Что сказал Пётр?\nОтвет: Пётр за синюю кнопку [11:30:14].\n\nВопрос пользователя:\nА он уверен?"
    );
}

#[test]
fn first_question_when_prompted_then_no_history_section() {
    assert_eq!(build_chat_prompt("Инструкции  ", &[], "Вопрос?"), "Инструкции\n\nВопрос пользователя:\nВопрос?");
}

#[test]
fn long_dialog_when_prompted_then_only_recent_turns_reach_the_model() {
    let history: Vec<ChatTurn> = (0..10).map(|number| turn(&format!("Вопрос номер {number}."), "Ответ")).collect();
    let prompt = build_chat_prompt("I", &history, "Последний");
    assert!(!prompt.contains("Вопрос номер 1."));
    assert!(prompt.contains("Вопрос номер 2.") && prompt.contains("Вопрос номер 9."));
    assert_eq!(prompt.matches("Вопрос номер").count(), PROMPT_HISTORY_TURNS);
}

#[test]
fn new_conversation_when_requested_twice_then_dialog_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    append_turn(dir.path(), &turn("Q", "A")).unwrap();
    clear_turns(dir.path()).unwrap();
    clear_turns(dir.path()).unwrap();
    assert!(read_turns(dir.path()).unwrap().is_empty());
}

#[test]
fn damaged_line_when_reading_dialog_then_the_rest_survives() {
    let dir = tempfile::tempdir().unwrap();
    append_turn(dir.path(), &turn("Что сказал Пётр?", "Да.")).unwrap();
    let mut damaged = String::from("{\"question\": \"оборв\n[1, 2]\n{\"question\": \"нет ответа\"}\n");
    let fields = r#""asked_at": "t", "provider": "p", "settings_revision": "r", "transcript_truncated": false"#;
    damaged.push_str(&format!("{{{fields}, \"question\": \"без ответа\", \"answer\": null}}\n"));
    damaged.push_str(&format!("{{{fields}, \"question\": null, \"answer\": \"без вопроса\"}}\n"));
    damaged.push_str(&format!("{{{fields}, \"question\": \"q\", \"answer\": \"a\", \"extra\": 1}}\n"));
    let path = dir.path().join("chat.jsonl");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(&damaged);
    text.push_str("\u{ff}not utf-8 is replaced\n");
    std::fs::write(&path, text).unwrap();
    append_turn(dir.path(), &turn("Второй", "Ответ")).unwrap();
    assert_eq!(questions(dir.path()), ["Что сказал Пётр?", "Второй"]);
}

#[test]
fn foreign_field_types_when_read_then_kept_as_python_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let line = r#"{"asked_at": 5, "question": "q", "answer": "a", "provider": null, "settings_revision": [1], "transcript_truncated": "yes"}"#;
    std::fs::write(dir.path().join("chat.jsonl"), format!("{line}\n")).unwrap();
    let turns = read_turns(dir.path()).unwrap();
    assert_eq!(turns[0].asked_at, Json::Int(5));
    assert_eq!(turns[0].transcript_truncated, Json::from("yes"));
}

#[test]
fn meeting_deleted_when_appending_then_not_found_and_not_recreated() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("gone");
    let error = append_turn(&gone, &turn("Q", "A")).unwrap_err();
    assert!(matches!(&error, recog_ai::AiError::Io(io) if io.kind() == std::io::ErrorKind::NotFound), "{error:?}");
    assert!(!gone.exists());
}

#[test]
fn no_dialog_file_when_read_then_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert!(read_turns(dir.path()).unwrap().is_empty());
}
