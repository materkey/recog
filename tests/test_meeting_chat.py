"""Questions about a recorded meeting, through the HTTP API on temporary data."""

import json

import httpx
import pytest

from recog.ai_settings import AiSettingsStore
from recog.meeting_storage import MeetingStorage
from recog.web import ai_provider, server


class FakeProvider:
    name = "Fake (model-x)"

    def __init__(self, answer="Пётр за синюю кнопку [11:30:14]."):
        self.answer = answer
        self.prompts = []

    async def complete(self, prompt):
        self.prompts.append(prompt)
        if isinstance(self.answer, Exception):
            raise self.answer
        return self.answer


@pytest.fixture
def chat(tmp_path, monkeypatch):
    meetings = tmp_path / "meetings"
    meeting = meetings / "m1"
    meeting.mkdir(parents=True)
    (meeting / "meta.json").write_text(
        json.dumps(
            {
                "id": "m1",
                "title": "Обзор дизайна",
                "created_at": "2026-11-02T08:30:08+00:00",
                "participants": {
                    "a": {"name": "Peter"},
                    "b": {"name": "Мария", "is_local": True},
                },
            }
        ),
        encoding="utf-8",
    )
    rows = [
        {
            "id": 1,
            "ts": "2026-11-02T08:30:14+00:00",
            "ts_display": "11:30:14",
            "speaker": "Peter",
            "speaker_id": "a",
            "text": "Я за синюю кнопку.",
        },
        {
            "id": 2,
            "ts": "2026-11-02T08:31:00+00:00",
            "ts_display": "11:31:00",
            "speaker": "Мария",
            "speaker_id": "b",
            "text": "Понял.",
        },
    ]
    (meeting / "transcript.jsonl").write_text(
        "".join(json.dumps(row, ensure_ascii=False) + "\n" for row in rows), encoding="utf-8"
    )
    (meetings / "empty").mkdir()
    (meetings / "empty" / "meta.json").write_text(
        json.dumps({"id": "empty", "created_at": "2026-11-02T08:30:08+00:00"})
    )
    (meetings / "empty" / "transcript.jsonl").write_text("")

    provider = FakeProvider()
    monkeypatch.setattr(server, "get_storage", lambda: MeetingStorage(meetings))
    monkeypatch.setattr(ai_provider, "get_data_dir", lambda: tmp_path)
    monkeypatch.setattr(server, "get_provider", lambda task, settings: provider)
    client = httpx.AsyncClient(
        transport=httpx.ASGITransport(app=server.app), base_url="http://test"
    )

    class Chat:
        pass

    Chat.provider, Chat.client, Chat.dir, Chat.store = (
        provider,
        client,
        meeting,
        AiSettingsStore(tmp_path),
    )
    return Chat


async def ask(chat, question="Что сказал Пётр?", meeting_id="m1"):
    return await chat.client.post(f"/api/meetings/{meeting_id}/chat", json={"question": question})


@pytest.mark.asyncio
async def test_question_about_a_meeting_when_asked_then_model_sees_transcript_and_question(chat):
    response = await ask(chat)

    assert response.status_code == 200
    turn = response.json()
    assert turn["question"] == "Что сказал Пётр?"
    assert turn["answer"] == "Пётр за синюю кнопку [11:30:14]."
    assert turn["provider"] == "Fake (model-x)"
    assert turn["transcript_truncated"] is False
    prompt = chat.provider.prompts[0]
    assert "[11:30:14] Peter: Я за синюю кнопку." in prompt
    assert "Обзор дизайна" in prompt and "Мария" in prompt
    assert prompt.rstrip().endswith("Что сказал Пётр?")
    assert prompt.index("Я за синюю кнопку.") < prompt.index("Что сказал Пётр?")


@pytest.mark.asyncio
async def test_answered_question_when_meeting_reopened_then_dialog_is_still_there(chat):
    await ask(chat)

    response = await chat.client.get("/api/meetings/m1/chat")

    assert response.status_code == 200
    assert [turn["question"] for turn in response.json()] == ["Что сказал Пётр?"]


@pytest.mark.asyncio
async def test_follow_up_question_when_asked_then_model_sees_earlier_dialog_before_it(chat):
    await ask(chat)
    chat.provider.answer = "Да."

    await ask(chat, "А он уверен?")

    prompt = chat.provider.prompts[1]
    assert prompt.index("Пётр за синюю кнопку [11:30:14].") < prompt.index("А он уверен?")
    assert "Что сказал Пётр?" in prompt


@pytest.mark.asyncio
async def test_long_dialog_when_asked_then_only_recent_turns_reach_the_model(chat):
    for number in range(10):
        await ask(chat, f"Вопрос номер {number}.")

    await ask(chat, "Последний")

    prompt = chat.provider.prompts[-1]
    assert "Вопрос номер 1." not in prompt
    assert "Вопрос номер 2." in prompt and "Вопрос номер 9." in prompt


@pytest.mark.asyncio
async def test_failed_model_call_when_asked_then_error_shown_and_nothing_stored(chat):
    chat.provider.answer = RuntimeError("Claude CLI error: limit")

    response = await ask(chat)

    assert response.status_code == 502
    assert "limit" in response.json()["detail"]
    assert (await chat.client.get("/api/meetings/m1/chat")).json() == []


@pytest.mark.asyncio
async def test_empty_model_answer_when_asked_then_it_counts_as_failure(chat):
    chat.provider.answer = "  \n"

    response = await ask(chat)

    assert response.status_code == 502
    assert (await chat.client.get("/api/meetings/m1/chat")).json() == []


@pytest.mark.asyncio
async def test_meeting_deleted_during_the_answer_when_finished_then_not_recreated(
    chat, monkeypatch
):
    import shutil

    async def complete(prompt):
        shutil.rmtree(chat.dir)
        return "Ответ"

    monkeypatch.setattr(chat.provider, "complete", complete)

    response = await ask(chat)

    assert response.status_code == 404
    assert not chat.dir.exists()


@pytest.mark.asyncio
async def test_transcript_over_the_limit_when_asked_then_turn_says_it_was_cut(chat, monkeypatch):
    monkeypatch.setattr(server, "CHAT_TRANSCRIPT_MAX_CHARS", 20)

    response = await ask(chat)

    assert response.json()["transcript_truncated"] is True
    assert "Понял." in chat.provider.prompts[0]


@pytest.mark.asyncio
async def test_new_conversation_when_requested_then_dialog_is_gone(chat):
    await ask(chat)

    assert (await chat.client.delete("/api/meetings/m1/chat")).status_code == 200
    assert (await chat.client.delete("/api/meetings/m1/chat")).status_code == 200

    assert (await chat.client.get("/api/meetings/m1/chat")).json() == []
    await ask(chat, "Снова")
    assert "Что сказал Пётр?" not in chat.provider.prompts[-1]


@pytest.mark.asyncio
async def test_damaged_line_when_reading_dialog_then_the_rest_survives(chat):
    await ask(chat)
    with (chat.dir / "chat.jsonl").open("a", encoding="utf-8") as stream:
        stream.write('{"question": "оборв\n[1, 2]\n{"question": "нет ответа"}\n')
    await ask(chat, "Второй")

    turns = (await chat.client.get("/api/meetings/m1/chat")).json()

    assert [turn["question"] for turn in turns] == ["Что сказал Пётр?", "Второй"]


@pytest.mark.parametrize("question", ["", "   \n", "x" * 4001], ids=["empty", "blank", "too-long"])
@pytest.mark.asyncio
async def test_unusable_question_when_asked_then_rejected_before_the_model(chat, question):
    response = await ask(chat, question)

    assert response.status_code == 422
    assert chat.provider.prompts == []


@pytest.mark.parametrize("meeting_id", ["absent", "..%5Cm1"])
@pytest.mark.parametrize("method", ["get", "delete"])
@pytest.mark.asyncio
async def test_unknown_meeting_when_chat_opened_then_not_found(chat, meeting_id, method):
    response = await getattr(chat.client, method)(f"/api/meetings/{meeting_id}/chat")

    assert response.status_code in (400, 404)


@pytest.mark.asyncio
async def test_meeting_without_speech_when_asked_then_explained_without_the_model(chat):
    response = await ask(chat, meeting_id="empty")

    assert response.status_code == 400
    assert chat.provider.prompts == []


@pytest.mark.asyncio
async def test_edited_prompt_and_model_when_asked_then_chat_uses_them(chat, monkeypatch):
    chat.store.write_task(
        "meeting_chat", {"prompt": "Мой промпт\n{transcript}", "models": {"claude": "other"}}
    )
    seen = {}

    def get_provider(task, settings):
        seen.update(settings)
        return chat.provider

    monkeypatch.setattr(server, "get_provider", get_provider)

    turn = (await ask(chat)).json()

    assert chat.provider.prompts[0].startswith("Мой промпт")
    assert seen["providers"] == ["claude"] and seen["models"]["claude"] == "other"
    assert turn["settings_revision"] == seen["revision"]
