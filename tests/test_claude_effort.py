"""Independent Claude effort travels from stored scenario to each launch mode."""

import shlex
from unittest.mock import AsyncMock, patch

import pytest

from recog.ai_provider import provider_for_settings
from recog.ai_settings import AiSettingsStore
from recog.copilot_supervisor import TmuxRunner


@pytest.mark.parametrize('task_id', ['analysis.increment', 'analysis.summary', 'title', 'saved:my-increment', 'copilot'])
@pytest.mark.parametrize('effort', ['', 'low', 'medium', 'high', 'xhigh', 'max'])
def test_scenario_given_claude_effort_when_saved_and_reset_then_roundtrips_independently(tmp_path, task_id, effort):
    store = AiSettingsStore(tmp_path)
    store.write_task(task_id, {'claude_effort': effort})
    assert store.resolve_task(task_id)['claude_effort'] == effort
    store.reset_task(task_id)
    assert store.resolve_task(task_id)['claude_effort'] == ''


@pytest.mark.parametrize('value', ['minimal', 'auto', '--max', 1, None])
def test_invalid_claude_effort_given_save_when_validated_then_no_file_created(tmp_path, value):
    store = AiSettingsStore(tmp_path)
    with pytest.raises(ValueError, match='Claude effort'):
        store.write_task('copilot', {'claude_effort': value})
    assert not store.path.exists()


@pytest.mark.asyncio
@pytest.mark.parametrize('stream', [False, True])
@pytest.mark.parametrize('effort', ['', 'max'])
async def test_independent_efforts_given_generation_when_spawned_then_claude_gets_only_its_own(tmp_path, stream, effort):
    store = AiSettingsStore(tmp_path)
    store.write_task('analysis.increment', {'reasoning_effort': 'low', 'claude_effort': effort})
    settings = store.resolve_task('analysis.increment')

    class Process:
        returncode = 0

        async def communicate(self):
            return b'answer', b''

        @property
        def stdout(self):
            async def lines():
                yield b'{"type":"result","result":"answer"}\n'
            return lines()

    spawn = AsyncMock(return_value=Process())
    with patch('recog.ai_provider.resolve_cli', return_value='/mock/cli'), patch('asyncio.create_subprocess_exec', spawn):
        claude = provider_for_settings(settings, task='analysis', provider='claude')
        codex = provider_for_settings(settings, task='analysis', provider='codex')
        assert codex.reasoning_effort == 'low'
        assert claude.reasoning_effort == effort
        if stream:
            assert [chunk async for chunk in claude.stream('hello')] == [('answer', True)]
        else:
            assert await claude.complete('hello') == 'answer'
    args = spawn.call_args.args
    assert '--safe-mode' in args and '--disable-slash-commands' in args
    assert '--bare' not in args
    if effort:
        assert args[args.index('--effort') + 1] == effort
    else:
        assert '--effort' not in args


def test_copilot_given_effort_change_when_next_command_built_then_new_effort_applies(tmp_path):
    store = AiSettingsStore(tmp_path)
    runner = TmuxRunner(ai_settings_store=store)
    assert '--effort' not in shlex.split(runner.build_claude_command())
    store.write_task('copilot', {'claude_effort': 'max'})
    args = shlex.split(runner.build_claude_command())
    assert args[args.index('--effort') + 1] == 'max'
    store.reset_task('copilot')
    assert '--effort' not in shlex.split(runner.build_claude_command())
