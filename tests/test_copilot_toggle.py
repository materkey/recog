"""The persisted switch stops running sessions and gates all subsequent starts."""

import asyncio
import json

import pytest

from recog.ai_settings import AiSettingsStore
from recog.copilot_supervisor import Supervisor, State, Spawn, TmuxRunner
from recog.ktalk_bridge import KTalkBridge


class Runner:
    def __init__(self):
        self.running = False
        self.spawns = []
        self.kills = 0

    async def alive(self):
        return self.running

    async def spawn(self, meeting_id):
        self.running = True
        self.spawns.append(meeting_id)

    async def kill(self):
        self.running = False
        self.kills += 1


class Client:
    def __init__(self, handler=None):
        self.frames = []
        self.handler = handler

    async def send(self, raw):
        frame = json.loads(raw)
        self.frames.append(frame)
        if self.handler:
            await self.handler(frame)


def test_disabled_copilot_given_prompt_edit_and_reset_when_saved_then_switch_stays_off(tmp_path):
    store = AiSettingsStore(tmp_path)
    store.write_task('copilot', {'enabled': False})
    store.write_task('copilot', {'prompt': 'New instructions'})
    assert store.resolve_task('copilot')['enabled'] is False
    store.reset_task('copilot')
    assert store.resolve_task('copilot')['enabled'] is False
    store.write_task('copilot', {'enabled': True})
    assert AiSettingsStore(tmp_path).resolve_task('copilot')['enabled'] is True


@pytest.mark.parametrize('value', ['false', 0, None])
def test_invalid_switch_given_save_when_validated_then_rejected(tmp_path, value):
    store = AiSettingsStore(tmp_path)
    with pytest.raises(ValueError):
        store.write_task('copilot', {'enabled': value})
    with pytest.raises(ValueError):
        store.write_task('title', {'enabled': False})


@pytest.mark.asyncio
@pytest.mark.parametrize('manual', [False, True])
async def test_running_session_given_disable_when_refreshed_then_stops_and_cannot_respawn(tmp_path, manual):
    store = AiSettingsStore(tmp_path)
    runner = Runner()
    supervisor = Supervisor(runner=runner, ai_settings_store=store, autospawn=True)
    event = {'type': 'copilot_spawn_request'} if manual else {'type': 'meeting_started', 'meeting_id': 'meeting'}
    await supervisor.handle_frame(event)
    assert runner.running
    store.write_task('copilot', {'enabled': False})
    await supervisor.handle_frame({'type': 'copilot_settings_changed'})
    assert runner.kills == 1 and not runner.running
    assert supervisor.lifecycle.state == State.IDLE
    await supervisor.handle_frame(event)
    await supervisor.on_session_died()
    await supervisor._execute((Spawn('queued-before-disable'),))
    assert len(runner.spawns) == 1
    assert supervisor._respawn_task is None


@pytest.mark.asyncio
async def test_disabled_startup_given_existing_process_when_refreshed_then_reaped(tmp_path):
    store = AiSettingsStore(tmp_path)
    store.write_task('copilot', {'enabled': False})
    runner = Runner()
    runner.running = True
    supervisor = Supervisor(runner=runner, ai_settings_store=store)
    assert not await supervisor._refresh_enabled()
    assert not runner.running and runner.kills == 1
    with pytest.raises(RuntimeError, match='выключен'):
        TmuxRunner(ai_settings_store=store).build_claude_command()


@pytest.mark.asyncio
async def test_disabled_during_meeting_given_enable_when_refreshed_then_active_conference_starts(tmp_path):
    store = AiSettingsStore(tmp_path)
    store.write_task('copilot', {'enabled': False})
    runner = Runner()
    supervisor = Supervisor(runner=runner, ai_settings_store=store, autospawn=True)
    await supervisor.handle_frame({'type': 'meeting_started', 'meeting_id': 'current'})
    assert runner.spawns == []
    store.write_task('copilot', {'enabled': True})
    await supervisor._refresh_enabled()
    assert runner.spawns == ['current']


@pytest.mark.asyncio
async def test_pending_retry_given_disable_when_refreshed_then_retry_cancelled(tmp_path):
    store = AiSettingsStore(tmp_path)
    supervisor = Supervisor(runner=Runner(), ai_settings_store=store, autospawn=True)
    await supervisor.handle_frame({'type': 'meeting_started', 'meeting_id': 'meeting'})
    await supervisor.on_session_died()
    retry = supervisor._respawn_task
    assert retry is not None
    store.write_task('copilot', {'enabled': False})
    await supervisor._refresh_enabled()
    await asyncio.gather(retry, return_exceptions=True)
    assert retry.cancelled()
    assert supervisor._respawn_task is None


@pytest.mark.asyncio
async def test_bridge_given_disabled_policy_when_chat_start_or_late_reply_arrives_then_no_delivery(tmp_path):
    store = AiSettingsStore(tmp_path)
    runner = Runner()
    supervisor = Supervisor(runner=runner, ai_settings_store=store, autospawn=True)
    await supervisor.handle_frame({'type': 'copilot_spawn_request'})
    bridge = KTalkBridge(ai_settings_store=store)
    subscriber, channel = Client(), Client()
    relay = Client(supervisor.handle_frame)
    bridge._subscriber_clients.add(subscriber)
    bridge._copilot_channel_clients.add(channel)
    bridge._supervisor_clients.add(relay)
    bridge._manual_copilot_active = True
    saved = store.write_task('copilot', {'enabled': False})
    await bridge._process_message({'type': 'ai_settings_refresh'}, subscriber)
    assert not runner.running and not bridge._manual_copilot_active
    assert subscriber.frames[-1]['revision'] == saved['revision']
    channel.frames.clear()
    relay.frames.clear()
    for kind in ('copilot_spawn_request', 'copilot_chat_message'):
        await bridge._process_message({'type': kind, 'request_id': kind, 'text': 'Hello'}, subscriber)
        assert subscriber.frames[-1]['status'] == 'error'
        assert 'выключен' in subscriber.frames[-1]['message']
    count = len(subscriber.frames)
    await bridge._process_message({'type': 'copilot_channel_reply', 'request_id': 'late', 'text': 'Late'}, channel)
    assert len(subscriber.frames) == count
    assert channel.frames == relay.frames == []
