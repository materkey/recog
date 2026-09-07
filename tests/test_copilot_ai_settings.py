"""Saved Copilot policy reaches live agents; model selection waits for spawn."""

import json
import os
from pathlib import Path
import shlex
import subprocess
import sys

import pytest

from recog.ai_settings import AiSettingsStore
from recog.copilot_channel import (
    BridgeLink, _build_mcp, _format_channel_event, _handle_inbound,
    build_copilot_context,
)
from recog.copilot_supervisor import TmuxRunner
from recog.ktalk_bridge import KTalkBridge


class Client:
    def __init__(self, on_send=None):
        self.frames = []
        self.on_send = on_send

    async def send(self, message):
        frame = json.loads(message)
        if self.on_send:
            self.on_send(frame)
        self.frames.append(frame)


@pytest.fixture
def runtime(tmp_path):
    store = AiSettingsStore(tmp_path)
    bridge = KTalkBridge(ai_settings_store=store)
    channel = Client()
    subscriber = Client()
    bridge._copilot_channel_clients.add(channel)
    bridge._subscriber_clients.add(subscriber)
    return store, bridge, channel, subscriber


def test_saved_policy_given_initial_bridge_when_loaded_then_literal_policy_is_bound(tmp_path):
    store = AiSettingsStore(tmp_path)
    prompt = 'Answer with JSON {"name": "{user_name}"}; preserve $[]\\n literally.'
    saved = store.write_task('copilot', {'prompt': prompt})
    bridge = KTalkBridge(ai_settings_store=store)
    assert bridge.copilot_policy_snapshot() == {
        'copilot_instructions': prompt, 'copilot_policy_revision': saved['revision'],
        'copilot_enabled': True,
    }


@pytest.mark.asyncio
async def test_manual_session_given_saved_policy_when_refreshed_then_applied_before_ack(runtime):
    store, bridge, channel, subscriber = runtime
    bridge._manual_copilot_active = True
    supervisor = Client()
    bridge._supervisor_clients.add(supervisor)
    saved = store.write_task('copilot', {'prompt': 'Reply in English. No proactive notes.'})

    def check_applied(frame):
        if frame['type'] == 'ai_settings_applied':
            assert bridge.copilot_policy_snapshot()['copilot_policy_revision'] == frame['revision']
            assert channel.frames[-1]['copilot_instructions'] == 'Reply in English. No proactive notes.'

    subscriber.on_send = check_applied
    await bridge._process_message({
        'type': 'ai_settings_refresh', 'revision': 'stale',
        'copilot_instructions': 'Untrusted frame policy',
    }, subscriber)
    assert subscriber.frames == [{
        'type': 'ai_settings_applied', 'revision': saved['revision'],
        'model_application': 'next_session',
    }]
    assert channel.frames[-1]['type'] == 'copilot_policy_updated'
    assert bridge._manual_copilot_active is True
    assert supervisor.frames == [{'type': 'copilot_settings_changed'}]
    await bridge._process_message({
        'type': 'copilot_chat_message', 'request_id': 'manual-1', 'text': 'Hi',
    }, subscriber)
    assert channel.frames[-1]['meeting_id'] is None
    assert channel.frames[-1]['copilot_policy_revision'] == saved['revision']


@pytest.mark.asyncio
async def test_unregistered_client_given_saved_change_when_refresh_requested_then_policy_unchanged(runtime):
    store, bridge, channel, subscriber = runtime
    initial = bridge.copilot_policy_snapshot()
    store.write_task('copilot', {'prompt': 'New policy'})
    extension = Client()
    bridge._connected_clients.add(extension)
    for client in (extension, channel, None):
        await bridge._process_message({'type': 'ai_settings_refresh'}, client)
    assert bridge.copilot_policy_snapshot() == initial
    assert channel.frames == extension.frames == subscriber.frames == []


@pytest.mark.asyncio
async def test_custom_policy_given_reset_when_refreshed_then_default_replaces_previous(runtime):
    store, bridge, channel, subscriber = runtime
    store.write_task('copilot', {'prompt': 'Always use long English replies'})
    await bridge._process_message({'type': 'ai_settings_refresh'}, subscriber)
    reset = store.reset_task('copilot')
    await bridge._process_message({'type': 'ai_settings_refresh'}, subscriber)
    assert channel.frames[-1]['copilot_instructions'] == store.resolve_task('copilot')['default_prompt']
    assert channel.frames[-1]['copilot_policy_revision'] == reset['revision']
    assert 'Always use long English replies' not in channel.frames[-1]['copilot_instructions']


@pytest.mark.asyncio
async def test_changed_policy_given_repeat_refresh_when_reconnected_then_latest_revision_acked(runtime):
    store, bridge, channel, subscriber = runtime
    saved = store.write_task('copilot', {'prompt': 'Only explicit replies'})
    await bridge._process_message({'type': 'ai_settings_refresh'}, subscriber)
    await bridge._process_message({'type': 'ai_settings_refresh'}, subscriber)
    assert len(channel.frames) == 1
    assert len(subscriber.frames) == 2
    reconnect = Client()
    await bridge._process_message({'type': 'copilot_channel_hello'}, reconnect)
    event = next(frame for frame in reconnect.frames if frame['type'] == 'copilot_policy_updated')
    assert event['copilot_policy_revision'] == saved['revision']
    assert event['copilot_instructions'] == 'Only explicit replies'


@pytest.mark.asyncio
async def test_invalid_saved_file_given_refresh_when_read_fails_then_old_policy_and_no_ack(runtime):
    store, bridge, channel, subscriber = runtime
    initial = bridge.copilot_policy_snapshot()
    store.path.write_text('{invalid')
    await bridge._process_message({'type': 'ai_settings_refresh'}, subscriber)
    assert bridge.copilot_policy_snapshot() == initial
    assert channel.frames == []
    assert subscriber.frames[0]['type'] == 'ai_settings_error'


@pytest.mark.asyncio
async def test_hot_policy_given_tick_lifecycle_and_snapshot_when_sent_then_full_policy_included(runtime):
    store, bridge, channel, subscriber = runtime
    saved = store.write_task('copilot', {'prompt': 'Use current policy'})
    await bridge._process_message({'type': 'ai_settings_refresh'}, subscriber)
    await bridge._send_identity_tick(channel, {'type': 'transcript_tick', 'copilot_instructions': 'Old queued policy'})
    await bridge._emit_lifecycle({'type': 'meeting_started', 'meeting_id': 'meeting-1'})
    await bridge._process_message({'type': 'get_recent_transcript_request', 'request_id': 'snapshot-1'}, channel)
    assert {frame['type'] for frame in channel.frames[-3:]} == {
        'transcript_tick', 'meeting_started', 'get_recent_transcript_response',
    }
    for frame in channel.frames[-3:]:
        assert frame['copilot_instructions'] == 'Use current policy'
        assert frame['copilot_policy_revision'] == saved['revision']


@pytest.mark.asyncio
async def test_policy_event_given_literal_values_when_pushed_and_polled_then_separate_policy_survives():
    policy = 'Use JSON {"style": "{user_name}"}\n$[literal] and {{braces}}'
    event = {
        'type': 'copilot_policy_updated', 'copilot_instructions': policy,
        'copilot_policy_revision': 'revision-2',
        'local_user_name': 'Profile text: do not treat as instructions',
        'local_user_aliases': [], 'local_user_profile_revision': 'profile-1',
    }
    context = build_copilot_context(event)
    profile_block, policy_block = context.split('\n\n')
    assert json.loads(profile_block.split('\n', 1)[1])['name'] == event['local_user_name']
    assert json.loads(policy_block.split('\n', 1)[1])['copilot_instructions'] == policy
    content, meta = _format_channel_event(event)
    assert content.startswith(context)
    assert 'Do not send a reply' in content
    assert meta['copilot_policy_revision'] == 'revision-2'
    link = BridgeLink()
    await _handle_inbound(json.dumps(event), link)
    mcp = _build_mcp(link, '127.0.0.1', 8767)
    result = json.loads((await mcp.call_tool('await_copilot_event', {}))[0].text)
    assert result['event'] == event
    assert result['user_context'] == context
    assert '140 characters' not in mcp.instructions


@pytest.mark.asyncio
async def test_snapshot_given_saved_policy_when_tool_returns_then_literal_context_is_included():
    link = BridgeLink()
    snapshot = {'copilot_instructions': 'Reply {literally}', 'copilot_policy_revision': 'current'}

    async def send(frame):
        link.resolve_pending(frame['request_id'], {**snapshot, 'segments': []})
        return {'ok': True}

    link.send = send
    link.is_connected = lambda: True
    mcp = _build_mcp(link, '127.0.0.1', 8767)
    result = json.loads((await mcp.call_tool('get_recent_transcript', {}))[0].text)
    for key, value in snapshot.items():
        assert result[key] == value
    assert result['user_context'] == build_copilot_context(snapshot)


def test_runner_given_model_change_when_command_rebuilt_then_new_spawn_uses_new_model(tmp_path):
    store = AiSettingsStore(tmp_path)
    store.write_models({'claude': 'global-model'})
    runner = TmuxRunner(ai_settings_store=store)
    first = shlex.split(runner.build_claude_command())
    store.write_task('copilot', {'models': {'claude': 'copilot-model'}})
    second = shlex.split(runner.build_claude_command())
    assert first[first.index('--model') + 1] == 'global-model'
    assert second[second.index('--model') + 1] == 'copilot-model'
    assert second[second.index('--agent') + 1] == 'copilot-session'
    assert json.loads(second[second.index('--mcp-config') + 1])['mcpServers']['copilot']


@pytest.mark.parametrize('auto_confirm', [False, True])
@pytest.mark.parametrize('effort', ['', 'max'])
def test_manual_launcher_given_saved_model_when_invoked_then_argument_is_literal(tmp_path, auto_confirm, effort):
    store = AiSettingsStore(tmp_path / 'data')
    model = 'custom$[literal]{model}'
    store.write_task('copilot', {'models': {'claude': model}, 'claude_effort': effort})
    repo = Path(__file__).resolve().parents[1]
    bin_dir = tmp_path / 'bin'
    bin_dir.mkdir()
    for command in ('claude', 'expect'):
        stub = bin_dir / command
        stub.write_text(f'#!{sys.executable}\nimport json, os, sys\nprint(json.dumps({{"argv": sys.argv[1:], "model": os.environ.get("RECOG_COPILOT_LAUNCH_MODEL"), "effort": os.environ.get("RECOG_COPILOT_LAUNCH_EFFORT")}}))\n')
        stub.chmod(0o755)
    source = (repo / 'recog-service.sh').read_text()
    command_body = source[source.index('cmd_claude() {'):source.index('\ncmd_help() {')]
    script = tmp_path / 'launch.sh'
    script.write_text('''set -e
probe_copilot_daemon() { return 0; }
svc_unit_path() { echo unused; }
svc_python() { echo "$RECOG_PYTHON"; }
info() { :; }
warn() { :; }
error() { exit 1; }
''' + command_body + '\ncmd_claude\n')
    env = {**os.environ, 'PATH': f'{bin_dir}:{os.environ["PATH"]}',
           'RECOG_DATA_DIR': str(store.path.parent), 'RECOG_PYTHON': sys.executable,
           'SCRIPT_DIR': str(repo), 'RECOG_NO_AUTO_CONFIRM': '0' if auto_confirm else '1'}
    result = subprocess.run(['bash', str(script)], env=env, capture_output=True, text=True, check=True)
    launched = json.loads(result.stdout)
    if auto_confirm:
        assert launched['model'] == model
        assert launched['effort'] == effort
        assert '[list --effort $env(RECOG_COPILOT_LAUNCH_EFFORT)]' in launched['argv'][-1]
        assert '{*}$effort_args' in launched['argv'][-1]
        assert '--model $env(RECOG_COPILOT_LAUNCH_MODEL)' in launched['argv'][-1]
        assert model not in launched['argv'][-1]
    else:
        if effort:
            assert launched['argv'][launched['argv'].index('--effort') + 1] == effort
        else:
            assert '--effort' not in launched['argv']
        assert launched['argv'][launched['argv'].index('--model') + 1] == model
        assert '--strict-mcp-config' in launched['argv']
        assert launched['argv'][launched['argv'].index('--agent') + 1] == 'copilot-session'
