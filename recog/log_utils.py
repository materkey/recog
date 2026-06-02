"""Rate-limited stderr-логирование для горячих путей (live ASR loop).

Контракт: первое сообщение по ключу эмитится сразу; повторы внутри interval_s
подавляются и считаются; следующая эмиссия добавляет "(+N suppressed)".
"""

import sys
import threading
import time
from typing import Callable, Dict, Optional, Tuple

_lock = threading.Lock()
# key -> (monotonic время последней эмиссии, счётчик подавленных с тех пор)
_state: Dict[str, Tuple[Optional[float], int]] = {}


def log_rate_limited(
    key: str,
    msg: str,
    interval_s: float = 30.0,
    _now_fn: Callable[[], float] = time.monotonic,
) -> None:
    """Печатает msg в stderr не чаще раза в interval_s на каждый key."""
    now = _now_fn()
    with _lock:
        last, suppressed = _state.get(key, (None, 0))
        if last is not None and now - last < interval_s:
            _state[key] = (last, suppressed + 1)
            return
        _state[key] = (now, 0)
    suffix = f" (+{suppressed} suppressed)" if suppressed else ""
    print(f"{msg}{suffix}", file=sys.stderr)


def _reset_for_tests() -> None:
    """Сбрасывает состояние (только для unit-тестов)."""
    with _lock:
        _state.clear()
