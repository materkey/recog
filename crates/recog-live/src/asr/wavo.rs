//! ASR backend "wavo": GigaAM on the wavo crate, in place of
//! `recog/wavo_backend.py` (which reaches the same crate through `rust/wavo-asr-py`).
//!
//! [`WavoAsr`] has `WavoASR`'s methods and answers: the live chunk's text
//! ([`WavoAsr::transcribe_chunk`]), one text per per-speaker clip
//! ([`WavoAsr::transcribe_batch`], run one after another: wavo has no batch), and the
//! refine window's text with its words in ms ([`WavoAsr::transcribe_with_timestamps`]).
//! A decode under way cannot be aborted; a refine (60-100 ms on the Mac) runs to its end.
//!
//! The model sits behind [`Engine`], so tests answer with a fake and never load or
//! download it. [`WavoEngine`] is the real one.
//!
//! The model lock. Python holds an `RLock` around every `model.transcribe`, and the
//! refine path takes the same lock around a whole refine call (its abort check and the
//! decode), so live waits for at most one refine. Rust's `Mutex` is not reentrant:
//! [`WavoAsr::lock_model`] gives the guard, and the `*_locked` methods run under a
//! guard the caller already holds, which is what the refine path calls; the other
//! methods take the lock themselves. Loading the model takes a lock of its own (Python
//! loads under the model lock), so a call holding the model lock can still trigger the
//! first load.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use recog_core::install_config::AsrModel;
use recog_core::py;

use super::gigaam_v4;
use super::words::words_from_tokens;
use super::{AsrError, Clip, Transcription};
use crate::config::SAMPLE_RATE;
use crate::log::Logger;

/// A model's answer: the text, and its tokens as `(piece, start_ms)`.
pub type EngineAnswer = (String, Vec<(String, u32)>);

/// What [`WavoAsr`] needs of the model.
pub trait Engine: Send + Sync {
    /// The text of 16 kHz mono float samples in [-1, 1], and its tokens.
    ///
    /// # Errors
    /// [`AsrError::InvalidInput`] for audio the model refuses (too long, wavo's
    /// `ValueError`), [`AsrError::Failed`] for anything else (`RuntimeError`).
    fn transcribe(&self, pcm: &[f32]) -> Result<EngineAnswer, AsrError>;

    /// The device it runs on, as the Python module's `device` (`Apple M3 Pro Metal`).
    fn device(&self) -> String;
}

/// Makes the engine on the first call that needs it.
pub type Loader = Box<dyn Fn() -> Result<Arc<dyn Engine>, AsrError> + Send + Sync>;

/// GigaAM on wavo's GPU backend, as `wavo_asr.Model` loads it.
pub struct WavoEngine {
    inner: wavo::Model,
}

fn engine_error(e: &wavo::Error) -> AsrError {
    match e {
        wavo::Error::AudioTooLong { .. } => AsrError::InvalidInput(e.to_string()),
        _ => AsrError::Failed(e.to_string()),
    }
}

impl WavoEngine {
    /// Loads the GGUF at `path` on the GPU.
    ///
    /// # Errors
    /// wavo's load error, as [`AsrError::Failed`].
    pub fn load(path: &Path) -> Result<Self, AsrError> {
        let inner = wavo::Model::load_with(path, wavo::Backend::Gpu).map_err(|e| engine_error(&e))?;
        Ok(Self { inner })
    }

    /// Find the selected model in the Hugging Face cache without downloading.
    ///
    /// # Errors
    /// An I/O error while checking the v4 weights.
    pub fn cached_path(model: AsrModel) -> Result<Option<PathBuf>, AsrError> {
        let hub = wavo::hub::Hub::new();
        match model {
            AsrModel::V4 => gigaam_v4::cached(hub.root()).map_err(|e| AsrError::Failed(e.to_string())),
            AsrModel::V3 => Ok(hub.find(model.name())),
        }
    }

    /// Return the selected model's path, calling `on_miss` before fetching missing weights.
    ///
    /// # Errors
    /// A cache, download or integrity check failure.
    pub fn fetch(model: AsrModel, on_miss: impl FnOnce()) -> Result<PathBuf, AsrError> {
        if let Some(path) = Self::cached_path(model)? {
            return Ok(path);
        }
        on_miss();
        let hub = wavo::hub::Hub::new().prepare(false);
        match model {
            AsrModel::V4 => gigaam_v4::download(hub.root()).map_err(|e| AsrError::Failed(e.to_string())),
            AsrModel::V3 => {
                hub.pull(model.name(), |_| std::ops::ControlFlow::Continue(())).map_err(|e| engine_error(&e))
            }
        }
    }

    /// Load the selected model from the cache, downloading it first if needed.
    ///
    /// # Errors
    /// The download's or the load's error, as [`AsrError::Failed`].
    pub fn from_hub(model: AsrModel) -> Result<Self, AsrError> {
        let path = Self::fetch(model, || {})?;
        Self::load(&path)
    }
}

impl Engine for WavoEngine {
    fn transcribe(&self, pcm: &[f32]) -> Result<EngineAnswer, AsrError> {
        let transcript = self.inner.transcribe(pcm).map_err(|e| engine_error(&e))?;
        let tokens = transcript.tokens.into_iter().map(|t| (t.piece, t.start_ms)).collect();
        Ok((transcript.text, tokens))
    }

    fn device(&self) -> String {
        let d = self.inner.device();
        format!("{} {:?}", d.name, d.api)
    }
}

/// The model lock held: proof for the `*_locked` methods.
pub struct ModelGuard<'a> {
    owner: &'a WavoAsr,
    _guard: MutexGuard<'a, ()>,
}

/// GigaAM on wavo with the methods recog's live and refine paths call.
pub struct WavoAsr {
    selected: AsrModel,
    loader: Loader,
    model: Mutex<Option<Arc<dyn Engine>>>,
    device: Mutex<String>,
    model_lock: Mutex<()>,
    logger: Logger,
}

impl WavoAsr {
    /// An ASR whose engine `loader` makes on first use; lines go to `logger`.
    #[must_use]
    pub fn new(selected: AsrModel, loader: Loader, logger: Logger) -> Self {
        Self {
            selected,
            loader,
            model: Mutex::new(None),
            device: Mutex::new("wavo".into()),
            model_lock: Mutex::new(()),
            logger,
        }
    }

    /// The selected local model, downloaded on first use if missing, with logs on stderr.
    #[must_use]
    pub fn from_hub(model: AsrModel) -> Self {
        Self::new(
            model,
            Box::new(move || Ok(Arc::new(WavoEngine::from_hub(model)?) as Arc<dyn Engine>)),
            Logger::stderr(),
        )
    }

    /// Always true: wavo has no remote side that can be down.
    #[must_use]
    pub fn available(&self) -> bool {
        true
    }

    /// `"wavo"` until the model loads, then `"wavo <device>"`.
    #[must_use]
    pub fn device(&self) -> String {
        self.device.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The model, loaded once whichever thread asks first.
    ///
    /// # Errors
    /// The loader's error; the next call tries again.
    pub fn load_model(&self, quiet: bool) -> Result<Arc<dyn Engine>, AsrError> {
        let mut model = self.model.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(engine) = model.as_ref() {
            return Ok(engine.clone());
        }
        if !quiet {
            self.logger.line(&format!("Загрузка модели {} (wavo)...", self.selected.name()));
        }
        let engine = (self.loader)()?;
        *self.device.lock().unwrap_or_else(PoisonError::into_inner) = format!("wavo {}", engine.device());
        *model = Some(engine.clone());
        Ok(engine)
    }

    /// Holds the model lock: live calls wait until the guard is dropped.
    pub fn lock_model(&self) -> ModelGuard<'_> {
        ModelGuard { owner: self, _guard: self.model_lock.lock().unwrap_or_else(PoisonError::into_inner) }
    }

    fn check_guard(&self, guard: &ModelGuard<'_>) {
        assert!(std::ptr::eq(guard.owner, self), "a model guard of another WavoAsr");
    }

    fn transcribe_raw_locked(
        &self,
        guard: &ModelGuard<'_>,
        audio: Clip<'_>,
        sample_rate: u32,
    ) -> Result<EngineAnswer, AsrError> {
        self.check_guard(guard);
        if sample_rate != SAMPLE_RATE {
            return Err(AsrError::InvalidInput(format!("wavo takes {SAMPLE_RATE} Hz audio, got {sample_rate}")));
        }
        let pcm = audio.to_f32();
        let model = self.load_model(true)?;
        model.transcribe(&pcm)
    }

    fn transcribe_raw(&self, audio: Clip<'_>, sample_rate: u32) -> Result<EngineAnswer, AsrError> {
        let guard = self.lock_model();
        self.transcribe_raw_locked(&guard, audio, sample_rate)
    }

    /// The live mic/mixed chunk's text, stripped; None when empty or on any error (logged,
    /// rate-limited).
    pub fn transcribe_chunk(&self, audio: Clip<'_>, sample_rate: u32) -> Option<String> {
        match self.transcribe_raw(audio, sample_rate) {
            Ok((text, _)) => {
                let text = py::strip(&text);
                (!text.is_empty()).then(|| text.to_owned())
            }
            Err(e) => {
                self.logger.rate_limited(
                    "asr-transcribe-chunk",
                    &format!("[WavoASR] transcribe_chunk failed: {}: {e}", e.python_name()),
                );
                None
            }
        }
    }

    /// One text per per-speaker clip, in order, unstripped as the model gives it.
    ///
    /// # Errors
    /// The first clip's failure; no texts then, as the Python list comprehension raises.
    pub fn transcribe_batch(&self, clips: &[Clip<'_>]) -> Result<Vec<String>, AsrError> {
        clips.iter().map(|clip| self.transcribe_raw(*clip, SAMPLE_RATE).map(|(text, _)| text)).collect()
    }

    /// The text and its words with `start`/`end` in ms, for refine.
    ///
    /// # Errors
    /// [`AsrError::InvalidInput`] for another sample rate or audio too long, else the
    /// model's failure.
    pub fn transcribe_with_timestamps(&self, audio: Clip<'_>, sample_rate: u32) -> Result<Transcription, AsrError> {
        let guard = self.lock_model();
        self.transcribe_with_timestamps_locked(&guard, audio, sample_rate)
    }

    /// [`Self::transcribe_with_timestamps`] under a model lock the caller holds (the
    /// refine path, which checks for a waiting live call under the same lock first).
    ///
    /// # Errors
    /// As [`Self::transcribe_with_timestamps`].
    ///
    /// # Panics
    /// When `guard` is another `WavoAsr`'s.
    pub fn transcribe_with_timestamps_locked(
        &self,
        guard: &ModelGuard<'_>,
        audio: Clip<'_>,
        sample_rate: u32,
    ) -> Result<Transcription, AsrError> {
        let (text, tokens) = self.transcribe_raw_locked(guard, audio, sample_rate)?;
        let words = words_from_tokens(&tokens);
        Ok(Transcription { text, words })
    }

    /// The refine window as `_refine_transcribe_fn` runs it on a `WavoASR`: the abort
    /// check under the model lock (cooperative mode, `should_abort` given), then a
    /// decode that runs to its end.
    ///
    /// # Errors
    /// [`AsrError::Aborted`] when a live call is waiting, else as
    /// [`Self::transcribe_with_timestamps`].
    pub fn refine(&self, pcm: &[i16], should_abort: Option<&dyn Fn() -> bool>) -> Result<Transcription, AsrError> {
        let guard = self.lock_model();
        if should_abort.is_some_and(|abort| abort()) {
            return Err(AsrError::Aborted);
        }
        self.transcribe_with_timestamps_locked(&guard, Clip::I16(pcm), SAMPLE_RATE)
    }
}

impl std::fmt::Debug for WavoAsr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WavoAsr").field("device", &self.device()).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_too_long_or_another_wavo_error_when_mapped_then_an_input_error_or_a_failure() {
        // given
        let too_long = wavo::Error::AudioTooLong { ms: 40_000, max_ms: 30_000 };
        let model = wavo::Error::Model("no gigaam".into());
        // when / then
        assert_eq!(
            engine_error(&too_long),
            AsrError::InvalidInput("audio is 40000 ms, longer than the model's 30000 ms: split it at pauses".into())
        );
        assert_eq!(engine_error(&model), AsrError::Failed("invalid model: no gigaam".into()));
    }
}
