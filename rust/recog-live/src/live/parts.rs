//! What the main loop drives, as traits: the captures, ASR and the conference bridge,
//! so the loop runs on the real ones in the service and on recorded or fake ones in a
//! replay and in tests. The Python loop reaches the same methods by duck typing
//! (`getattr(capture, "digital_silence_s", None)`).

use std::sync::Arc;

use recog_bridge::{Bridge, RefineUpdate, TranscriptLine};

use crate::asr::{AsrError, Clip, Transcription, WavoAsr};
use crate::capture::{SubprocessCapture, SystemCapture};

/// A microphone or system-audio capture as the loop uses it.
pub trait LiveCapture: Send + Sync {
    fn running(&self) -> bool;
    /// # Errors
    /// The capture could not start (the device is busy or gone).
    fn start(&self) -> Result<(), String>;
    fn stop(&self);
    /// `get_chunk(duration)`: the samples and the capture time of the first.
    fn get_chunk(&self, duration: f64) -> Option<(Vec<f32>, f64)>;
    fn clear_buffer(&self);
    /// The source epoch the latest chunk was cut in.
    fn chunk_epoch(&self) -> u64;
    /// Lost after its fast restarts (a child capture), retried once a minute.
    fn dead(&self) -> bool {
        false
    }
    /// Seconds of exact zeros so far (a child capture); None where not measured.
    fn digital_silence_s(&self) -> Option<f64> {
        None
    }
    /// A planned restart of the capture's child.
    fn request_restart(&self, _reason: &str) {}
    /// Reopens a stream that stopped capturing (an in-process device capture).
    fn reopen_if_broken(&self) -> bool {
        false
    }
    /// The child's command line (a child capture), for the log and tests.
    fn argv(&self) -> Vec<String> {
        Vec::new()
    }
}

fn io_text(e: &std::io::Error) -> String {
    e.to_string()
}

impl LiveCapture for SubprocessCapture {
    fn running(&self) -> bool {
        SubprocessCapture::running(self)
    }
    fn start(&self) -> Result<(), String> {
        SubprocessCapture::start(self).map_err(|e| io_text(&e))
    }
    fn stop(&self) {
        SubprocessCapture::stop(self);
    }
    fn get_chunk(&self, duration: f64) -> Option<(Vec<f32>, f64)> {
        SubprocessCapture::get_chunk(self, duration)
    }
    fn clear_buffer(&self) {
        SubprocessCapture::clear_buffer(self);
    }
    fn chunk_epoch(&self) -> u64 {
        SubprocessCapture::chunk_epoch(self)
    }
    fn dead(&self) -> bool {
        SubprocessCapture::dead(self)
    }
    fn digital_silence_s(&self) -> Option<f64> {
        Some(SubprocessCapture::digital_silence_s(self))
    }
    fn request_restart(&self, reason: &str) {
        SubprocessCapture::request_restart(self, reason);
    }
    fn argv(&self) -> Vec<String> {
        SubprocessCapture::argv(self)
    }
}

impl LiveCapture for SystemCapture {
    fn running(&self) -> bool {
        self.capture().running()
    }
    fn start(&self) -> Result<(), String> {
        SystemCapture::start(self).map_err(|e| io_text(&e))
    }
    fn stop(&self) {
        SystemCapture::stop(self);
    }
    fn get_chunk(&self, duration: f64) -> Option<(Vec<f32>, f64)> {
        SystemCapture::get_chunk(self, duration)
    }
    fn clear_buffer(&self) {
        self.capture().clear_buffer();
    }
    fn chunk_epoch(&self) -> u64 {
        self.capture().chunk_epoch()
    }
    fn dead(&self) -> bool {
        self.capture().dead()
    }
    fn digital_silence_s(&self) -> Option<f64> {
        Some(self.capture().digital_silence_s())
    }
    fn request_restart(&self, reason: &str) {
        self.capture().request_restart(reason);
    }
    fn argv(&self) -> Vec<String> {
        self.capture().argv()
    }
}

/// ASR as the loop calls it: the four paths of `ASRModule`/`WavoASR`.
pub trait LiveAsr: Send + Sync {
    /// `asr.available`: false while no backend can serve (the buffers are kept).
    fn available(&self) -> bool {
        true
    }
    /// `load_model(quiet=True)`.
    ///
    /// # Errors
    /// The model cannot be loaded.
    fn load_model(&self) -> Result<(), AsrError>;
    /// The live mic/mixed chunk's text, None for none.
    ///
    /// # Errors
    /// `TemporarilyUnavailable` keeps the chunk for a retry; anything else is logged.
    fn transcribe_chunk(&self, chunk: &[f32]) -> Result<Option<String>, AsrError>;
    /// One text per per-speaker clip.
    ///
    /// # Errors
    /// `TemporarilyUnavailable` holds the batch; anything else drops it.
    fn transcribe_batch(&self, clips: &[&[i16]]) -> Result<Vec<String>, AsrError>;
    /// The refine window's text and words, under the model lock; with `should_abort`
    /// (the cooperative mode) a pending live call aborts it before the decode.
    ///
    /// # Errors
    /// `Aborted` gives way to live; others as the worker handles them.
    fn transcribe_with_timestamps(
        &self,
        pcm: &[i16],
        should_abort: Option<&dyn Fn() -> bool>,
    ) -> Result<Transcription, AsrError>;
}

impl LiveAsr for WavoAsr {
    fn load_model(&self) -> Result<(), AsrError> {
        WavoAsr::load_model(self, true).map(|_| ())
    }

    fn transcribe_chunk(&self, chunk: &[f32]) -> Result<Option<String>, AsrError> {
        Ok(WavoAsr::transcribe_chunk(self, Clip::F32(chunk), crate::config::SAMPLE_RATE))
    }

    fn transcribe_batch(&self, clips: &[&[i16]]) -> Result<Vec<String>, AsrError> {
        let clips: Vec<Clip<'_>> = clips.iter().map(|c| Clip::I16(c)).collect();
        WavoAsr::transcribe_batch(self, &clips)
    }

    fn transcribe_with_timestamps(
        &self,
        pcm: &[i16],
        should_abort: Option<&dyn Fn() -> bool>,
    ) -> Result<Transcription, AsrError> {
        // `_refine_transcribe_fn` on a WavoASR: the abort check under the model lock,
        // then a decode that runs to its end.
        let guard = self.lock_model();
        if should_abort.is_some_and(|abort| abort()) {
            return Err(AsrError::Aborted);
        }
        self.transcribe_with_timestamps_locked(&guard, Clip::I16(pcm), crate::config::SAMPLE_RATE)
    }
}

/// What the loop asks of the conference bridge and sends it (the methods of
/// `KTalkBridge` that `LiveTranscriber` calls).
pub trait Conference: Send + Sync {
    fn get_dominant_speaker_for_period(&self, start_time: f64, end_time: f64) -> Option<String>;
    fn get_current_speaker(&self) -> Option<String>;
    fn is_speaker_muted(&self, speaker_name: &str) -> bool;
    fn get_sole_unmuted_remote_participant(&self) -> Option<String>;
    fn has_remote_participants(&self) -> bool;
    fn are_all_mics_muted(&self) -> bool;
    fn is_local_mic_muted(&self) -> bool;
    fn is_conference_active(&self) -> bool;
    fn send_transcript(&self, line: &TranscriptLine);
    fn send_refine(&self, updates: &[RefineUpdate], meeting_id: Option<&str>);
}

impl Conference for Arc<Bridge> {
    fn get_dominant_speaker_for_period(&self, start_time: f64, end_time: f64) -> Option<String> {
        Bridge::get_dominant_speaker_for_period(self, start_time, end_time)
    }
    fn get_current_speaker(&self) -> Option<String> {
        Bridge::get_current_speaker(self)
    }
    fn is_speaker_muted(&self, speaker_name: &str) -> bool {
        Bridge::is_speaker_muted(self, speaker_name)
    }
    fn get_sole_unmuted_remote_participant(&self) -> Option<String> {
        Bridge::get_sole_unmuted_remote_participant(self)
    }
    fn has_remote_participants(&self) -> bool {
        Bridge::has_remote_participants(self)
    }
    fn are_all_mics_muted(&self) -> bool {
        Bridge::are_all_mics_muted(self)
    }
    fn is_local_mic_muted(&self) -> bool {
        Bridge::is_local_mic_muted(self)
    }
    fn is_conference_active(&self) -> bool {
        Bridge::is_conference_active(self)
    }
    fn send_transcript(&self, line: &TranscriptLine) {
        Bridge::send_transcript(self, line);
    }
    fn send_refine(&self, updates: &[RefineUpdate], meeting_id: Option<&str>) {
        Bridge::send_refine(self, updates, meeting_id);
    }
}
