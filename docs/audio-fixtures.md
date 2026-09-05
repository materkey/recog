# Local examples

Keep sample audio/video and generated transcripts in `.local/examples/`.
This directory is gitignored; create it locally when preparing audio fixtures.

`tests/e2e/prepare_fixtures.py` reads `.local/examples/example_5min.wav` and optionally
`.local/examples/vlad_sample.wav` to prepare the audio fixtures under `tests/e2e/fixtures/`.

Actual meeting recordings and speaker databases belong in the Recog data
directory (`~/.recog/` by default), not here.
