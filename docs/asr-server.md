# Optional ASR server

The ASR gateway and GigaAM backend live in `recog/asr_server/` and run
independently of the four client services. From the repository root, with
the existing server environment activated, run them in separate processes:

```bash
python -m recog.asr_server.gigaam_backend
python -m recog.asr_server.gateway
```

The backend requires CUDA, GigaAM with word-timestamp support at commit
`6e4b027`, pyannote.audio 4.x, model access and ffmpeg. The gateway forwards
GigaAM and Qwen aliases to their backend URLs. Qwen is an external service.
The existing defaults are gateway `0.0.0.0:8770`, GigaAM `127.0.0.1:9002`
and Qwen `127.0.0.1:9001`. The gateway has no authentication; run it only
on a trusted network. `GIGAAM_URL` and `QWEN_ASR_URL` override its backend URLs.

Configure your process manager to run the module commands above with the
repository as the working directory. `setup.sh` and `recog-service.sh` do not
install or start these optional servers. Existing deployments can retain
their current environment and model cache when updating the code location.

Verify `/healthz` on both services and transcribe a sample through
`/v1/audio/transcriptions` with `model=gigaam`, `response_format=verbose_json`
and `timestamp_granularities=word`; the response must contain a `words` array.
Only transcription tests model access and inference; health alone does not.
