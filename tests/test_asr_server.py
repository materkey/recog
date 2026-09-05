"""Optional server configuration and proxy tests; no models or GPU are loaded."""
import asyncio
from io import BytesIO
import runpy
import sys
from types import ModuleType, SimpleNamespace
from unittest.mock import Mock

import pytest

pytest.importorskip("multipart", reason="optional ASR server HTTP dependency")
httpx = pytest.importorskip("httpx")
pytest.importorskip("fastapi")
uvicorn = pytest.importorskip("uvicorn")
from fastapi import UploadFile


@pytest.mark.parametrize("device,expected_fp16", [("cpu", False), ("cuda:1", True)])
def test_device_and_bind_overrides_when_backend_runs_then_settings_reach_server(
    monkeypatch, device, expected_fp16
):
    monkeypatch.setenv("GIGAAM_DEVICE", device)
    monkeypatch.delenv("GIGAAM_FP16_ENCODER", raising=False)
    monkeypatch.setenv("GIGAAM_HOST", "127.0.0.2")
    monkeypatch.setenv("GIGAAM_PORT", "19002")
    monkeypatch.setenv("GIGAAM_DIARIZATION", "0")
    run = Mock()
    monkeypatch.setattr(uvicorn, "run", run)

    module = runpy.run_module("recog.asr_server.gigaam_backend", run_name="__main__")

    assert module["DEVICE"] == device
    assert module["FP16_ENCODER"] is expected_fp16
    assert module["DIARIZATION_ENABLED"] is False
    assert module["state"] == {}
    run.assert_called_once_with(
        module["app"], host="127.0.0.2", port=19002, log_level="info"
    )


@pytest.mark.parametrize("diarization", ["0", "1"])
def test_cpu_settings_when_backend_starts_then_model_loaders_receive_device(
    monkeypatch, tmp_path, diarization
):
    monkeypatch.setenv("GIGAAM_DEVICE", "cpu")
    monkeypatch.setenv("GIGAAM_FP16_ENCODER", "0")
    monkeypatch.setenv("GIGAAM_DIARIZATION", diarization)
    monkeypatch.setenv("HF_TOKEN_PATH", str(tmp_path / "absent-token"))
    fake_torch = ModuleType("torch")
    fake_torch.serialization = ModuleType("torch.serialization")
    fake_torch.serialization.add_safe_globals = Mock()
    fake_torch.torch_version = SimpleNamespace(TorchVersion=object)
    fake_torch.load = Mock()
    fake_torch.device = Mock(return_value="cpu-device")
    load_model = Mock(return_value=object())
    pipeline = Mock()
    from_pretrained = Mock(return_value=pipeline)
    monkeypatch.setitem(sys.modules, "torch", fake_torch)
    monkeypatch.setitem(sys.modules, "torch.serialization", fake_torch.serialization)
    monkeypatch.setitem(sys.modules, "gigaam", SimpleNamespace(load_model=load_model))
    monkeypatch.setitem(
        sys.modules, "pyannote.audio",
        SimpleNamespace(Pipeline=SimpleNamespace(from_pretrained=from_pretrained)),
    )
    module = runpy.run_module("recog.asr_server.gigaam_backend")

    async def exercise():
        async with module["lifespan"](module["app"]):
            assert module["state"]["model"] is load_model.return_value
            assert (module["state"]["diar"] is not None) == (diarization == "1")
        assert module["state"] == {}

    asyncio.run(exercise())
    load_model.assert_called_once_with(module["VARIANT"], device="cpu", fp16_encoder=False)
    if diarization == "1":
        from_pretrained.assert_called_once_with(module["DIAR_MODEL"])
        pipeline.to.assert_called_once_with("cpu-device")
    else:
        from_pretrained.assert_not_called()


def test_backend_urls_when_gateway_runs_then_aliases_and_timestamp_forwarding_survive(
    monkeypatch,
):
    monkeypatch.setenv("GIGAAM_URL", "http://gigaam.invalid:19002")
    monkeypatch.setenv("QWEN_ASR_URL", "http://qwen.invalid:19001")
    monkeypatch.setenv("ASR_GATEWAY_HOST", "127.0.0.1")
    monkeypatch.setenv("ASR_GATEWAY_PORT", "18770")
    run = Mock()
    monkeypatch.setattr(uvicorn, "run", run)
    module = runpy.run_module("recog.asr_server.gateway", run_name="__main__")
    run.assert_called_once_with(
        module["app"], host="127.0.0.1", port=18770, log_level="info"
    )
    assert module["MODEL_MAP"]["qwen"] == "http://qwen.invalid:19001"
    assert module["MODEL_MAP"]["whisper-1"] == "http://gigaam.invalid:19002"

    requests = []

    async def respond(request):
        requests.append(request)
        return httpx.Response(200, json={"words": [{"word": "hello", "start": 0.1, "end": 0.4}]})

    async def exercise():
        await module["client"].aclose()
        async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
            monkeypatch.setitem(module["transcribe"].__globals__, "client", client)
            response = await module["transcribe"](
                file=UploadFile(filename="sample.wav", file=BytesIO(b"audio")),
                model="gigaam-diarized", language="ru", response_format="verbose_json",
                prompt=None, temperature=None, timestamp_granularities="word",
            )
        assert response.status_code == 200
        assert b'"words"' in response.body

    asyncio.run(exercise())
    assert str(requests[0].url) == "http://gigaam.invalid:19002/v1/audio/transcriptions"
    assert b'name="timestamp_granularities"\r\n\r\nword' in requests[0].content
    assert b'name="model"\r\n\r\ngigaam-diarized' in requests[0].content
