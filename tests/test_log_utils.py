"""Тесты rate-limited stderr-логгера (recog/log_utils.py)."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent))

from recog.log_utils import log_rate_limited, _reset_for_tests


def test_first_message_emitted_immediately(capsys):
    """given: чистое состояние логгера
    when: log_rate_limited вызывается один раз
    then: сообщение сразу попадает в stderr"""
    _reset_for_tests()
    log_rate_limited("key-a", "boom", interval_s=30.0)
    assert "boom" in capsys.readouterr().err


def test_repeat_within_interval_suppressed_and_counted(capsys):
    """given: сообщение с key-b уже было залогировано
    when: то же key логируется повторно внутри interval, потом по истечении
    then: повторы подавлены; следующая эмиссия содержит счётчик подавленных"""
    _reset_for_tests()
    fake_now = [1000.0]
    log_rate_limited("key-b", "первое", interval_s=30.0, _now_fn=lambda: fake_now[0])
    fake_now[0] += 1.0
    log_rate_limited("key-b", "второе", interval_s=30.0, _now_fn=lambda: fake_now[0])
    fake_now[0] += 1.0
    log_rate_limited("key-b", "третье", interval_s=30.0, _now_fn=lambda: fake_now[0])
    err = capsys.readouterr().err
    assert "первое" in err
    assert "второе" not in err and "третье" not in err

    fake_now[0] += 30.0
    log_rate_limited("key-b", "четвёртое", interval_s=30.0, _now_fn=lambda: fake_now[0])
    err = capsys.readouterr().err
    assert "четвёртое" in err
    assert "+2 suppressed" in err


def test_different_keys_do_not_interfere(capsys):
    """given: key-c уже залогирован
    when: логируется key-d
    then: key-d эмитится сразу (отдельный счётчик)"""
    _reset_for_tests()
    log_rate_limited("key-c", "c-msg", interval_s=30.0)
    log_rate_limited("key-d", "d-msg", interval_s=30.0)
    err = capsys.readouterr().err
    assert "c-msg" in err and "d-msg" in err
