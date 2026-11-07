"""WavoASR, the ASR backend "wavo": GigaAM v3 e2e_rnnt on the Rust runtime wavo.

The Rust module (rust/wavo-asr-py) is replaced by a fake that answers with fixed
tokens, so these tests run without a GPU or the model; what they check is the
adapter: the shapes recog's live and refine paths expect.
"""

import sys
import threading
import types

import numpy as np
import pytest

from recog import remote_asr as remote_asr_mod
from recog.install_config import INSTALL_CONFIG
from recog.refine import RefineAborted


class FakeModel:
    """Answers every clip with the tokens it was given, and records what it got."""

    def __init__(self, text="Привет мир", tokens=(("▁При", 0), ("вет", 40), ("▁мир", 200))):
        self.text, self.tokens = text, list(tokens)
        self.seen = []
        self.device = "Fake GPU Metal"

    def transcribe(self, pcm):
        self.seen.append(np.frombuffer(pcm, dtype="<f4").copy())
        return self.text, self.tokens


@pytest.fixture
def fake(monkeypatch):
    model = FakeModel()
    module = types.ModuleType("wavo_asr")
    module.Model = lambda: model
    monkeypatch.setitem(sys.modules, "wavo_asr", module)
    return model


@pytest.fixture
def asr(fake):
    from recog.wavo_backend import WavoASR

    return WavoASR()


def test_tokens_with_word_starts_timestamps_give_gigaams_words(asr):
    """given: tokens where "▁" starts a word, 40 ms encoder frames
    when: a refine window is transcribed with timestamps
    then: words as GigaAM builds them, in ms: from the first token's frame to the end of the last's"""
    result = asr.transcribe_with_timestamps(np.zeros(16000, dtype=np.float32))

    assert result.text == "Привет мир"
    assert [(w.text, w.start, w.end) for w in result.words] == [
        ("Привет", 0, 80),
        ("мир", 200, 240),
    ]


def test_int16_pcm_any_path_reaches_the_model_as_floats_in_range(asr, fake):
    """given: Int16 PCM, as the extension's per-speaker stream sends it
    when: it goes through the batch path
    then: the model gets float32 in [-1, 1]"""
    asr.transcribe_batch([np.array([0, 16384, -32768], dtype=np.int16)])

    np.testing.assert_array_equal(fake.seen[0], np.array([0.0, 0.5, -1.0], dtype=np.float32))


def test_batch_of_clips_transcribe_batch_answers_each_in_order(asr, fake):
    """given: three clips
    when: they are transcribed as one batch
    then: one text per clip, in the input order (wavo runs them one after another)"""
    texts = iter(["один", "два", "три"])
    fake.transcribe = lambda pcm: (next(texts), [])

    assert asr.transcribe_batch([np.zeros(10, dtype=np.float32)] * 3) == [
        "один",
        "два",
        "три",
    ]
    assert asr.transcribe_batch([]) == []


@pytest.mark.parametrize("text,expected", [(" слово ", "слово"), ("", None), ("   ", None)])
def test_live_chunk_transcribe_chunk_strips_or_gives_none(asr, fake, text, expected):
    """given: the model's text for a live chunk
    when: the chunk is transcribed
    then: the text stripped, None when there is nothing, as ASRModule answers"""
    fake.text = text

    assert asr.transcribe_chunk(np.zeros(10, dtype=np.float32)) == expected


def test_model_error_transcribe_chunk_answers_none(asr, fake):
    """given: a model that fails on a chunk
    when: the live path transcribes it
    then: None, like ASRModule's best-effort contract, not an exception into the loop"""

    def fail(pcm):
        raise RuntimeError("device lost")

    fake.transcribe = fail

    assert asr.transcribe_chunk(np.zeros(10, dtype=np.float32)) is None


def test_other_sample_rate_transcribe_with_timestamps_raises_value_error(asr):
    """given: audio at 48 kHz
    when: it is transcribed
    then: ValueError: wavo takes 16 kHz only and recog never resamples here"""
    with pytest.raises(ValueError, match="16000"):
        asr.transcribe_with_timestamps(np.zeros(10, dtype=np.float32), sample_rate=48000)


def test_backend_wavo_build_asr_returns_wavo_asr(monkeypatch, fake):
    """given: RECOG_ASR_BACKEND=wavo
    when: build_asr()
    then: a WavoASR, without loading the model yet"""
    from recog.wavo_backend import WavoASR

    monkeypatch.setattr(INSTALL_CONFIG.asr, "backend", "wavo")

    asr = remote_asr_mod.build_asr()

    assert isinstance(asr, WavoASR)
    assert fake.seen == []


@pytest.mark.parametrize("abort_before", [False, True])
def test_wavo_refine_live_pending_aborts_only_before_the_decode(asr, abort_before):
    """given: the wavo backend, which cannot stop a decode under way
    when: refine runs while live may be waiting
    then: it gives way before starting, else it runs to the end under the model lock"""
    from recog.live import LiveTranscriber

    t = LiveTranscriber.__new__(LiveTranscriber)
    t.asr = asr
    seen_lock = []
    original = asr.transcribe_with_timestamps

    def spy(pcm):
        seen_lock.append(asr._model_lock._is_owned())
        return original(pcm)

    asr.transcribe_with_timestamps = spy
    pcm = np.zeros(16000, dtype=np.int16)

    if abort_before:
        with pytest.raises(RefineAborted):
            t._refine_transcribe_fn(pcm, should_abort=lambda: True)
        assert seen_lock == []
    else:
        result = t._refine_transcribe_fn(pcm, should_abort=lambda: False)
        assert result.text == "Привет мир"
        assert seen_lock == [True]
    assert not asr._model_lock._is_owned()


def test_two_threads_load_model_loads_once(monkeypatch):
    """given: two threads asking for the model at once
    when: both call load_model
    then: one model is made and both get it"""
    made = []
    module = types.ModuleType("wavo_asr")
    module.Model = lambda: made.append(1) or FakeModel()
    monkeypatch.setitem(sys.modules, "wavo_asr", module)
    from recog.wavo_backend import WavoASR

    asr = WavoASR()
    got = []
    threads = [
        threading.Thread(target=lambda: got.append(asr.load_model(quiet=True))) for _ in range(2)
    ]
    for th in threads:
        th.start()
    for th in threads:
        th.join()

    assert made == [1]
    assert got[0] is got[1]


@pytest.mark.parametrize("importable", [True, False])
def test_backend_wavo_readiness_needs_the_rust_module(tmp_path, importable):
    """given: RECOG_ASR_BACKEND=wavo, with and without an importable wavo_asr
    when: the readiness check runs before services start
    then: it passes only with the module, else names it and setup.sh
          (the live service would fail on its first model load)"""
    import os
    import subprocess
    from pathlib import Path

    root = Path(__file__).resolve().parent.parent
    shadow = tmp_path / "shadow" / "wavo_asr"
    shadow.mkdir(parents=True)
    (shadow / "__init__.py").write_text("" if importable else "raise ImportError('not built')\n")
    data = tmp_path / "data"
    data.mkdir()

    result = subprocess.run(
        [
            "bash",
            "-c",
            'source "$1"; deps_asr_config_check',
            "--",
            str(root / "scripts/deps.sh"),
        ],
        env={
            **os.environ,
            "DEPS_PYTHON": sys.executable,
            "RECOG_DATA_DIR": str(data),
            "RECOG_ASR_BACKEND": "wavo",
            "PYTHONPATH": str(tmp_path / "shadow"),
        },
        capture_output=True,
        text=True,
    )

    assert (result.returncode == 0) is importable, result.stdout + result.stderr
    if not importable:
        assert "wavo_asr" in result.stdout and "setup.sh" in result.stdout


def test_backend_wavo_live_process_imports_no_torch(tmp_path):
    """given: RECOG_ASR_BACKEND=wavo
    when: a fresh interpreter imports the live pipeline and builds its ASR
    then: torch is not imported: with wavo it is dead weight, ~190 MB and a second of startup"""
    import os
    import subprocess

    shadow = tmp_path / "wavo_asr"
    shadow.mkdir()
    (shadow / "__init__.py").write_text("class Model:\n    pass\n")
    code = (
        "import sys\n"
        "import recog.live\n"
        "from recog.remote_asr import build_asr\n"
        "build_asr()\n"
        "print(sorted(m for m in ('torch', 'torchaudio', 'gigaam') if m in sys.modules))\n"
    )
    env = {
        **os.environ,
        "RECOG_ASR_BACKEND": "wavo",
        "PYTHONPATH": str(tmp_path),
        "RECOG_DATA_DIR": str(tmp_path),
    }

    result = subprocess.run([sys.executable, "-c", code], env=env, capture_output=True, text=True)

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "[]"
