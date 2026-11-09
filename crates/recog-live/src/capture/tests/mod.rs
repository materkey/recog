//! The Python capture tests, ported: tests/test_audio_capture.py, the capture cases of
//! tests/test_live_buffering.py, tests/test_system_audio.py, the decoder and argv cases
//! of tests/test_mic_child.py and the backend cases of tests/test_system_audio_cli.py.

mod buffer;
mod fakes;
mod framed;
mod pulse;
mod reader;
mod sck;
