"""Profile updates keep future mic labels and Copilot identity in agreement."""

import asyncio
import json
import time
from unittest.mock import Mock

import pytest

from recog.cli import build_parser
from recog.copilot_channel import BridgeLink, _format_channel_event, _handle_inbound
from recog.ktalk_bridge import KTalkBridge
from recog.live import LiveTranscriber, SourceState
from recog.user_profile import UserProfileStore


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
def runtime(tmp_path, monkeypatch):
    monkeypatch.setattr('recog.live.get_data_dir', lambda: tmp_path)
    monkeypatch.setattr('recog.live.MIC_SPEAKER_NAME', 'Legacy')
    monkeypatch.setattr('recog.live.build_asr', Mock())
    store = UserProfileStore(tmp_path)

    def create(override=None):
        transcriber = LiveTranscriber(mic_speaker_name=override)
        bridge = KTalkBridge()
        bridge.configure_user_profile(
            transcriber._user_identity,
            transcriber._read_user_identity,
            transcriber._apply_user_identity,
        )
        transcriber._ktalk_bridge = bridge
        return transcriber, bridge

    return store, create


@pytest.mark.parametrize('argument,expected', [(None, None), ('One run', 'One run')])
def test_cli_given_optional_override_when_parsed_then_absence_is_distinct(argument, expected):
    argv = ['live']
    if argument is not None:
        argv += ['--mic-speaker-name', argument]
    assert build_parser().parse_args(argv).mic_speaker_name == expected


@pytest.mark.parametrize('saved,override,expected', [
    (False, None, 'Legacy'), (True, None, 'Александр'),
    (True, 'One run', 'One run'), (False, 'One run', 'One run'),
])
def test_live_given_profile_and_override_when_started_then_both_labels_agree(runtime, saved, override, expected):
    store, create = runtime
    if saved:
        store.write('Александр', ['Саша', 'Alex'])
    transcriber, bridge = create(override)
    identity = bridge.user_identity_snapshot()
    assert transcriber.mic_speaker_name == identity['local_user_name'] == expected
    assert identity['local_user_profile_revision'] == store.read()['revision']
    if saved:
        assert 'Александр' in identity['local_user_aliases']
        assert 'Legacy' not in identity['local_user_aliases']


@pytest.mark.asyncio
async def test_subscriber_given_stale_revision_when_refreshed_then_latest_applies_before_ack(runtime):
    store, create = runtime
    old = store.write('Александр', ['Саша'])
    transcriber, bridge = create()
    saved = store.write('Александр', ['Alex'])
    channel = Client()
    bridge._copilot_channel_clients.add(channel)
    bridge._manual_copilot_active = True

    def check_applied(frame):
        if frame['type'] == 'user_profile_applied':
            assert transcriber.mic_speaker_name == 'Александр'
            assert transcriber._user_identity == bridge.user_identity_snapshot() == frame['identity']

    subscriber = Client(check_applied)
    bridge._subscriber_clients.add(subscriber)
    await bridge._process_message({
        'type': 'user_profile_refresh', 'revision': old['revision'],
        'name': 'Untrusted', 'aliases': ['Untrusted'],
    }, subscriber)
    assert subscriber.frames[0]['revision'] == saved['revision']
    event = channel.frames[0]
    assert event['type'] == 'user_profile_updated'
    assert event['local_user_aliases'] == ['Александр', 'Alex']
    assert event['local_user_profile_revision'] == saved['revision']
    assert bridge._copilot_tick_state is None
    await bridge._process_message({'type': 'copilot_chat_message', 'request_id': 'manual-1', 'text': 'Hi'}, subscriber)
    assert channel.frames[-1]['type'] == 'copilot_user_message'
    assert channel.frames[-1]['meeting_id'] is None
    for key, value in bridge.user_identity_snapshot().items():
        assert channel.frames[-1][key] == value


@pytest.mark.asyncio
async def test_extension_given_saved_change_when_refresh_requested_then_identity_is_unchanged(runtime):
    store, create = runtime
    transcriber, bridge = create()
    store.write('Александр', [])
    extension = Client()
    bridge._connected_clients.add(extension)
    await bridge._process_message({'type': 'user_profile_refresh'}, extension)
    assert transcriber.mic_speaker_name == 'Legacy'
    assert extension.frames == []


@pytest.mark.asyncio
@pytest.mark.parametrize('failure', ['read', 'apply'])
async def test_profile_given_failure_when_refreshed_then_error_without_success(runtime, failure):
    store, create = runtime
    transcriber, bridge = create()
    before = bridge.user_identity_snapshot()
    subscriber = Client()
    bridge._subscriber_clients.add(subscriber)
    if failure == 'read':
        store.path.write_text('{broken')
    else:
        store.write('Александр', [])
        bridge._apply_mic_identity = Mock(side_effect=RuntimeError('private path/name'))
    await bridge._process_message({'type': 'user_profile_refresh'}, subscriber)
    assert [frame['type'] for frame in subscriber.frames] == ['user_profile_error']
    assert 'private' not in subscriber.frames[0]['message']
    assert bridge.user_identity_snapshot() == before
    assert transcriber.mic_speaker_name == 'Legacy'


@pytest.mark.asyncio
async def test_profile_given_new_name_when_mic_chunk_arrives_then_history_is_preserved(runtime):
    store, create = runtime
    transcriber, bridge = create()
    transcriber._meeting_storage = Mock()
    transcriber._meeting_storage.append_segment.side_effect = [1, 2]
    state = SourceState()
    transcriber._handle_mic_chunk('Before', '12:00:00', time.time(), 0, state)
    previous = transcriber.transcript_lines[0]
    store.write('Александр', ['Саша'])
    subscriber = Client()
    bridge._subscriber_clients.add(subscriber)
    await bridge._process_message({'type': 'user_profile_refresh'}, subscriber)
    transcriber._handle_mic_chunk('After', '12:00:01', time.time(), 0, state)
    assert transcriber.transcript_lines[0] == previous
    assert '**Legacy**' in previous
    assert '**Александр**' in transcriber.transcript_lines[1]
    rows = transcriber._meeting_storage.append_segment.call_args_list
    assert [call.kwargs['speaker'] for call in rows] == ['Legacy', 'Александр']
    assert all(call.kwargs['is_local'] and call.kwargs['speaker_id'] == 'local' for call in rows)


@pytest.mark.asyncio
async def test_tick_given_queued_old_identity_when_profile_changes_then_events_use_latest(runtime):
    store, create = runtime
    store.write('Александр', ['Саша'])
    transcriber, bridge = create()
    channel = Client()
    bridge._copilot_channel_clients.add(channel)
    bridge._loop = asyncio.get_running_loop()
    bridge._copilot_tick_state = {
        'meeting_id': 'm-1', 'meeting_title': 'Meeting', 'seg_count': 0,
        'last_seg_count': 0, 'last_emit_ts': 0,
    }
    # Queue on the audio path, then apply before the event loop dispatches it.
    bridge._maybe_emit_transcript_tick()
    updated = store.write('Александр', ['Alex'])
    subscriber = Client()
    bridge._subscriber_clients.add(subscriber)
    await bridge._process_message({'type': 'user_profile_refresh'}, subscriber)
    await asyncio.sleep(0.01)
    await bridge._emit_lifecycle({'type': 'meeting_started', 'meeting_id': 'm-1'})
    await bridge._process_message({'type': 'get_recent_transcript_request', 'request_id': 'snap'}, subscriber)
    relevant = [frame for frame in channel.frames if frame['type'] in {'transcript_tick', 'meeting_started'}]
    assert {frame['type'] for frame in relevant} == {'transcript_tick', 'meeting_started'}
    relevant.append(subscriber.frames[-1])
    for frame in relevant:
        assert frame['local_user_aliases'] == ['Александр', 'Alex']
        assert frame['local_user_profile_revision'] == updated['revision']
        assert frame['local_user_identity_known'] is True


@pytest.mark.asyncio
async def test_clear_given_profile_when_refreshed_then_removed_aliases_leave_manual_session(runtime):
    store, create = runtime
    store.write('Александр', ['Саша'])
    transcriber, bridge = create()
    subscriber, channel = Client(), Client()
    bridge._subscriber_clients.add(subscriber)
    bridge._copilot_channel_clients.add(channel)
    cleared = store.clear()
    await bridge._process_message({'type': 'user_profile_refresh'}, subscriber)
    assert transcriber.mic_speaker_name == 'Legacy'
    assert channel.frames[0]['local_user_aliases'] == ['Legacy']
    assert channel.frames[0]['local_user_profile_revision'] == cleared['revision']


@pytest.mark.asyncio
@pytest.mark.parametrize('event_type', [
    'user_profile_updated', 'meeting_started', 'transcript_tick', 'copilot_user_message',
])
async def test_channel_given_profile_event_when_received_then_aliases_survive_and_no_reply_is_requested(event_type):
    event = {
        'type': event_type, 'local_user_name': 'Александр',
        'local_user_aliases': ['Александр', 'Саша'],
        'local_user_profile_revision': 'rev-2', 'local_user_identity_known': True,
    }
    link = BridgeLink()
    await _handle_inbound(json.dumps(event), link)
    assert link.event_queue.get_nowait() == event
    content, meta = _format_channel_event(event)
    assert json.loads(meta['local_user_aliases']) == event['local_user_aliases']
    assert meta['local_user_profile_revision'] == 'rev-2'
    assert json.loads(meta['local_user_identity_known']) is True
    if event_type == 'user_profile_updated':
        assert 'Do not send a reply' in content
