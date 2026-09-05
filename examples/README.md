# Local examples

Keep sample audio/video and generated transcripts here. These files are
gitignored; a fresh clone contains only this README.

`tests/e2e/prepare_fixtures.py` reads `examples/example_5min.wav` and optionally
`examples/vlad_sample.wav` to prepare the audio fixtures under `tests/e2e/fixtures/`.

Actual meeting recordings and speaker databases belong in the Recog data
directory (`~/.recog/` by default), not here.
