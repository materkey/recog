"""AI settings HTTP and live-policy acknowledgement using temporary files only."""

import asyncio
import json
import threading
from types import SimpleNamespace

import httpx
import pytest

from recog.ai_settings import AiSettingsStore
from recog.web import server
from recog.web.ai_settings import AiSettingsCoordinator


@pytest.fixture
def ai_api(tmp_path, monkeypatch):
    broadcasts = []

    async def broadcast(raw):
        broadcasts.append(json.loads(raw))

    store = AiSettingsStore(tmp_path / 'data')
    coordinator = AiSettingsCoordinator(store, broadcast)
    monkeypatch.setattr(server, 'ai_settings_coordinator', coordinator)
    # ASGITransport skips lifespan: installed services and real data stay untouched.
    client = httpx.AsyncClient(transport=httpx.ASGITransport(app=server.app), base_url='http://test')
    return SimpleNamespace(store=store, coordinator=coordinator, client=client, broadcasts=broadcasts)


class FakeBridge:
    def __init__(self):
        self.sent = asyncio.Queue()

    async def send(self, raw):
        await self.sent.put(json.loads(raw))
        return True

    async def next_refresh(self):
        return await asyncio.wait_for(self.sent.get(), timeout=2)


async def connect(api):
    bridge = FakeBridge()
    await api.coordinator.connected(bridge.send)
    assert await bridge.next_refresh() == {
        'type': 'ai_settings_refresh', 'revision': api.store.read()['revision'],
    }
    return bridge


async def acknowledge(api, revision=None):
    await api.coordinator.handle_frame({
        'type': 'ai_settings_applied',
        'revision': revision or api.store.read()['revision'],
        'model_application': 'next_session',
    })


def task_payload(api, prompt='Reply in English', **values):
    return {'revision': api.store.read()['revision'], 'prompt': prompt,
            'providers': ['claude'], 'models': {}, **values}


def task_view(view, task_id='copilot'):
    return next(task for task in view['tasks'] if task['id'] == task_id)


async def test_claude_effort_given_api_save_when_reloaded_then_preserved_and_invalid_value_rejected(ai_api):
    api = ai_api
    result = await api.client.put('/api/ai-settings/tasks/copilot', json=task_payload(api, claude_effort='max'))
    assert result.status_code == 200
    assert task_view(result.json())['claude_effort'] == 'max'
    loaded = await api.client.get('/api/ai-settings')
    assert task_view(loaded.json())['claude_effort'] == 'max'
    result = await api.client.put('/api/ai-settings/tasks/copilot', json=task_payload(api, claude_effort='minimal'))
    assert result.status_code == 422
    assert api.store.resolve_task('copilot')['claude_effort'] == 'max'


async def test_removed_features_given_request_when_routed_then_unavailable(ai_api):
    client = ai_api.client
    assert (await client.post('/api/live/anchor', json={})).status_code == 404
    assert (await client.get('/api/speakers')).status_code == 404
    assert (await client.post('/api/speakers/enroll-from-segments', json={})).status_code == 404
    assert (await client.post('/api/meetings/test/analyze', json={'question': 'custom'})).status_code == 422
    assert (await client.get('/api/meetings/test/analyze-stream?question=custom')).status_code == 422


async def test_copilot_switch_given_api_save_when_reloaded_then_boolean_is_persisted(ai_api):
    api = ai_api
    saved = await api.client.put('/api/ai-settings/tasks/copilot', json=task_payload(api, enabled=False))
    assert saved.status_code == 200
    assert task_view(saved.json())['enabled'] is False
    assert task_view((await api.client.get('/api/ai-settings')).json())['enabled'] is False
    invalid = await api.client.put('/api/ai-settings/tasks/copilot', json=task_payload(api, enabled='false'))
    assert invalid.status_code == 422


async def test_absent_settings_given_get_when_resolved_then_defaults_without_persistence(ai_api):
    api = ai_api
    async with api.client:
        response = await api.client.get('/api/ai-settings')
    assert response.status_code == 200
    body = response.json()
    assert body['revision'] == 'defaults'
    assert body['copilot_application'] == {'state': 'pending', 'model_application': 'next_session'}
    assert task_view(body)['providers'] == task_view(body)['allowed_providers'] == ['claude']
    assert task_view(body)['prompt'] == task_view(body)['default_prompt']
    assert not api.store.path.exists()


async def test_disconnected_bridge_given_model_save_when_read_back_then_persisted_pending(ai_api):
    api = ai_api
    async with api.client:
        response = await api.client.put('/api/ai-settings/models', json={
            'revision': 'defaults', 'models': {'claude': 'selected-claude', 'codex': 'selected-codex'},
        })
        other_browser = await api.client.get('/api/ai-settings')
    assert response.status_code == 200
    body = response.json()
    assert body == other_browser.json()
    assert body['revision'] != 'defaults'
    assert body['models']['claude'] == 'selected-claude'
    assert task_view(body)['models']['claude'] == 'selected-claude'
    assert body['copilot_application']['state'] == 'pending'
    assert api.broadcasts[-1] == {
        'type': 'ai_settings_status', 'revision': body['revision'],
        'copilot_application': body['copilot_application'],
    }


async def test_connected_bridge_given_task_save_when_current_ack_arrives_then_applied(ai_api):
    api = ai_api
    bridge = await connect(api)
    async with api.client:
        saving = asyncio.create_task(api.client.put('/api/ai-settings/tasks/copilot', json=task_payload(api)))
        refresh = await bridge.next_refresh()
        assert refresh['revision'] == api.store.read()['revision']
        assert (await api.coordinator.view())['copilot_application']['state'] == 'pending'
        await acknowledge(api)
        response = await asyncio.wait_for(saving, timeout=2)
    assert response.status_code == 200
    assert task_view(response.json())['prompt'] == 'Reply in English'
    assert response.json()['copilot_application'] == {'state': 'applied', 'model_application': 'next_session'}
    assert api.broadcasts[-1]['copilot_application']['state'] == 'applied'


async def test_stale_browser_given_model_task_or_reset_when_written_then_conflict_preserves_settings(ai_api):
    api = ai_api
    current = api.store.write_models({'claude': 'human-edit'})
    before = api.store.path.read_bytes()
    async with api.client:
        responses = [
            await api.client.put('/api/ai-settings/models', json={'revision': 'defaults', 'models': {'claude': 'stale'}}),
            await api.client.put('/api/ai-settings/tasks/copilot', json=task_payload(api, revision='defaults')),
            await api.client.delete('/api/ai-settings/tasks/copilot', params={'revision': 'defaults'}),
        ]
    assert all(response.status_code == 409 for response in responses)
    assert api.store.path.read_bytes() == before
    assert api.store.read()['revision'] == current['revision']
    assert api.broadcasts == []


async def test_task_override_given_reset_when_deleted_then_defaults_and_global_model_restored(ai_api):
    api = ai_api
    api.store.write_models({'claude': 'global-model'})
    original = api.store.write_task('copilot', {'prompt': 'Custom policy', 'models': {'claude': 'task-model'}})
    async with api.client:
        response = await api.client.delete('/api/ai-settings/tasks/copilot', params={'revision': original['revision']})
    assert response.status_code == 200
    body = response.json()
    assert body['revision'] != original['revision']
    task = task_view(body)
    assert task['prompt'] == task['default_prompt']
    assert task['models']['claude'] == 'global-model'
    assert not task['customized']
    assert body['copilot_application']['state'] == 'pending'


@pytest.mark.parametrize('values', [
    {'providers': ['codex']}, {'providers': ['claude', 'codex']}, {'providers': []},
    {'prompt': ''}, {'prompt': 'x' * 32001}, {'models': {'claude': 'has whitespace'}},
    {'models': {'claude': '--bad-option'}}, {'models': {'unknown': 'model'}},
    {'secret': 'never-store-this'}, {'prompt': None}, {'revision': None},
])
async def test_invalid_task_given_put_when_validated_then_422_and_no_mutation(ai_api, values):
    api = ai_api
    api.store.write_task('copilot', {'prompt': 'Original'})
    before = api.store.path.read_bytes()
    async with api.client:
        response = await api.client.put('/api/ai-settings/tasks/copilot', json=task_payload(api, **values))
    assert response.status_code == 422
    assert api.store.path.read_bytes() == before
    assert api.broadcasts == []


@pytest.mark.parametrize('route,payload', [
    ('analysis.summary', {'prompt': 'Missing transcript', 'providers': ['claude']}),
    ('title', {'prompt': '{transcript} {unknown}', 'providers': ['codex']}),
    ('title', {'prompt': '{transcript}', 'providers': ['codex'], 'reasoning_effort': 'invalid'}),
    ('unknown', {'prompt': '{transcript}', 'providers': ['claude']}),
])
async def test_invalid_scenario_given_put_when_validated_then_422_without_creating_settings(ai_api, route, payload):
    api = ai_api
    async with api.client:
        response = await api.client.put(f'/api/ai-settings/tasks/{route}', json={'revision': 'defaults', **payload})
    assert response.status_code == 422
    assert not api.store.path.exists()


async def test_overlapping_saves_given_old_ack_when_current_revision_differs_then_stays_pending(ai_api):
    api = ai_api
    bridge = await connect(api)
    async with api.client:
        first = asyncio.create_task(api.client.put('/api/ai-settings/tasks/copilot', json=task_payload(api, 'First')))
        old_refresh = await bridge.next_refresh()
        second = asyncio.create_task(api.client.put('/api/ai-settings/tasks/copilot', json=task_payload(api, 'Second')))
        new_refresh = await bridge.next_refresh()
        assert old_refresh['revision'] != new_refresh['revision']
        await acknowledge(api, old_refresh['revision'])
        pending = (await api.client.get('/api/ai-settings')).json()
        assert pending['revision'] == new_refresh['revision']
        assert pending['copilot_application']['state'] == 'pending'
        await acknowledge(api, new_refresh['revision'])
        responses = await asyncio.wait_for(asyncio.gather(first, second), timeout=2)
    assert all(response.status_code == 200 for response in responses)
    # Each browser gets the revision it wrote, even when another save lands
    # while the first request is waiting for the live acknowledgement.
    assert responses[0].json()['revision'] == old_refresh['revision']
    assert task_view(responses[0].json())['prompt'] == 'First'
    assert responses[0].json()['copilot_application']['state'] == 'pending'
    assert responses[1].json()['revision'] == new_refresh['revision']
    assert task_view(responses[-1].json())['prompt'] == 'Second'
    assert responses[-1].json()['copilot_application']['state'] == 'applied'


async def test_applied_settings_given_disconnect_reconnect_when_same_revision_then_fresh_ack_required(ai_api):
    api = ai_api
    await connect(api)
    await acknowledge(api)
    assert (await api.coordinator.view())['copilot_application']['state'] == 'applied'
    await api.coordinator.disconnected()
    await acknowledge(api)
    assert (await api.coordinator.view())['copilot_application']['state'] == 'pending'
    await connect(api)
    assert (await api.coordinator.view())['copilot_application']['state'] == 'pending'
    await acknowledge(api)
    assert (await api.coordinator.view())['copilot_application']['state'] == 'applied'


async def test_old_connection_given_ack_read_in_flight_when_reconnected_then_stale_ack_rejected(ai_api, monkeypatch):
    api = ai_api
    await connect(api)
    revision = api.store.read()['revision']
    read = api.store.read
    captured = asyncio.Event()
    release = threading.Event()
    guard = threading.Lock()
    should_pause = True
    loop = asyncio.get_running_loop()

    def paused_read():
        nonlocal should_pause
        with guard:
            pause, should_pause = should_pause, False
        result = read()
        if pause:
            loop.call_soon_threadsafe(captured.set)
            assert release.wait(timeout=3), 'Old acknowledgement was not released'
        return result

    monkeypatch.setattr(api.store, 'read', paused_read)
    old_ack = asyncio.create_task(acknowledge(api, revision))
    try:
        await asyncio.wait_for(captured.wait(), timeout=2)
        await api.coordinator.disconnected()
        await connect(api)
    finally:
        release.set()
        await asyncio.wait_for(old_ack, timeout=2)
    assert (await api.coordinator.view())['copilot_application']['state'] == 'pending'
    await acknowledge(api, revision)
    assert (await api.coordinator.view())['copilot_application']['state'] == 'applied'


async def test_bridge_apply_error_given_waiting_save_when_error_arrives_then_saved_pending_without_internal_details(ai_api):
    api = ai_api
    bridge = await connect(api)
    async with api.client:
        saving = asyncio.create_task(api.client.put('/api/ai-settings/tasks/copilot', json=task_payload(api)))
        await bridge.next_refresh()
        await api.coordinator.handle_frame({'type': 'ai_settings_error', 'message': 'private runtime detail'})
        response = await asyncio.wait_for(saving, timeout=2)
    assert response.status_code == 200
    body = response.json()
    assert task_view(body)['prompt'] == 'Reply in English'
    assert body['copilot_application']['state'] == 'pending'
    assert body['copilot_application']['error']
    assert 'private runtime detail' not in response.text
    await acknowledge(api)
    assert 'error' not in (await api.coordinator.view())['copilot_application']


async def test_bridge_send_failure_given_save_when_not_delivered_then_saved_pending(ai_api):
    api = ai_api

    async def failed_send(raw):
        return False

    await api.coordinator.connected(failed_send)
    async with api.client:
        response = await api.client.put('/api/ai-settings/tasks/copilot', json=task_payload(api))
    assert response.status_code == 200
    assert task_view(response.json())['prompt'] == 'Reply in English'
    assert response.json()['copilot_application']['state'] == 'pending'


async def test_write_failure_given_put_when_filesystem_fails_then_500_without_success_broadcast(ai_api, monkeypatch):
    api = ai_api

    def fail_write(*args, **kwargs):
        raise PermissionError('private filesystem location')

    monkeypatch.setattr(api.store, 'write_models', fail_write)
    async with api.client:
        response = await api.client.put('/api/ai-settings/models', json={'revision': 'defaults', 'models': {'claude': 'new'}})
    assert response.status_code == 500
    assert 'private filesystem location' not in response.text
    assert not api.store.path.exists()
    assert api.broadcasts == []


async def test_corrupt_file_given_get_when_read_fails_then_500_without_overwriting(ai_api):
    api = ai_api
    api.store.path.parent.mkdir()
    api.store.path.write_bytes(b'broken JSON')
    async with api.client:
        response = await api.client.get('/api/ai-settings')
    assert response.status_code == 500
    assert api.store.path.read_bytes() == b'broken JSON'


@pytest.mark.parametrize('method,route,payload', [
    ('PUT', '/api/ai-settings/models', {'revision': 'defaults', 'models': {'claude': 'new-model'}}),
    ('PUT', '/api/ai-settings/tasks/copilot', {'revision': 'defaults', 'prompt': 'New policy', 'providers': ['claude']}),
    ('DELETE', '/api/ai-settings/tasks/copilot?revision=defaults', None),
])
@pytest.mark.parametrize('corrupt', [b'broken JSON', b'{"version":999,"revision":"unsupported","models":{},"tasks":{}}'])
async def test_invalid_saved_storage_given_mutation_when_read_fails_then_500_without_overwriting(ai_api, method, route, payload, corrupt):
    api = ai_api
    api.store.path.parent.mkdir()
    api.store.path.write_bytes(corrupt)
    async with api.client:
        response = await api.client.request(method, route, **({'json': payload} if payload is not None else {}))
    assert response.status_code == 500
    assert api.store.path.read_bytes() == corrupt
    assert api.broadcasts == []
