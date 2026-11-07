//! The two runtimes behind [`crate::Runtime`].

use crate::{Recognized, Runtime, Word};

pub fn load(name: &str, backend: &str) -> Box<dyn Runtime> {
    match name {
        #[cfg(feature = "wavo")]
        "wavo" => Box::new(wavo_rt::Wavo::load(backend)),
        #[cfg(feature = "gigastt")]
        "gigastt" => Box::new(gigastt_rt::Gigastt::load(backend)),
        _ => panic!("runtime {name} is not built in (features: wavo, gigastt)"),
    }
}

/// Words from SentencePiece tokens: `▁` starts a word; a word ends where the next one starts,
/// the last one a frame after its last token.
#[cfg(feature = "wavo")]
fn words_from_pieces(tokens: &[(String, u32)], frame_ms: u32) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    for (piece, start) in tokens {
        match piece.strip_prefix('▁') {
            Some(rest) => {
                if let Some(last) = words.last_mut() {
                    last.2 = *start;
                }
                words.push(Word(rest.to_string(), *start, *start + frame_ms));
            }
            None => match words.last_mut() {
                Some(last) => {
                    last.0.push_str(piece);
                    last.2 = start + frame_ms;
                }
                None => words.push(Word(piece.clone(), *start, *start + frame_ms)),
            },
        }
    }
    words.retain(|w| !w.0.is_empty());
    words
}

#[cfg(feature = "wavo")]
mod wavo_rt {
    use super::*;
    use std::ops::ControlFlow;

    pub struct Wavo {
        model: wavo::Model,
    }

    impl Wavo {
        pub fn load(backend: &str) -> Self {
            let hub = wavo::hub::Hub::new();
            let path = hub
                .find("gigaam-v3")
                .unwrap_or_else(|| hub.pull("gigaam-v3", |_| ControlFlow::Continue(())).expect("pull gigaam-v3"));
            let backend = match backend {
                "gpu" => wavo::Backend::Gpu,
                "ane" => {
                    wavo::Model::prepare(&path, |_| ControlFlow::Continue(())).expect("prepare for the Neural Engine");
                    wavo::Backend::NeuralEngine
                }
                _ => wavo::Backend::Auto,
            };
            Self { model: wavo::Model::load_with(&path, backend).expect("load gigaam-v3") }
        }
    }

    impl Runtime for Wavo {
        fn device(&self) -> String {
            let d = self.model.device();
            format!("{} {:?}", d.name, d.api)
        }

        fn transcribe(&mut self, pcm: &[f32]) -> Recognized {
            let t = self.model.transcribe(pcm).expect("transcribe");
            let tokens: Vec<(String, u32)> = t.tokens.iter().map(|k| (k.piece.clone(), k.start_ms)).collect();
            Recognized { text: t.text.trim().to_string(), words: words_from_pieces(&tokens, 40) }
        }

        /// wavo checks for a cancel only between the pieces of `transcribe_long`; a refine window
        /// is one piece, so a decode under way runs to its end.
        fn abort_latency_ms(&mut self, _pcm: &[f32], _after_ms: u64) -> Option<f64> {
            None
        }
    }
}

#[cfg(feature = "gigastt")]
mod gigastt_rt {
    use super::*;
    use gigastt_core::inference::{Engine, TranscribeRequest, TranscribeSource};
    use gigastt_core::model::ModelVariant;
    use gigastt_core::ExecutionProviderChoice;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    pub struct Gigastt {
        engine: Engine,
        provider: String,
    }

    impl Gigastt {
        pub fn load(backend: &str) -> Self {
            let dir = std::env::var("GIGASTT_MODEL_DIR").unwrap_or_else(|_| gigastt_core::model::default_model_dir());
            let provider = ExecutionProviderChoice::parse(backend).expect("backend");
            // gigastt runs the encoder on one thread per pooled session by default; GIGASTT_THREADS
            // gives the one session more.
            let threads = std::env::var("GIGASTT_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
            let engine = Engine::load_with_execution_provider(
                &dir,
                Some(ModelVariant::E2eRnnt),
                1,
                1,
                0,
                threads,
                None,
                provider,
            )
            .expect("load e2e_rnnt");
            Self { provider: format!("{} int8={} threads={threads}", engine.execution_provider(), engine.is_int8()), engine }
        }

        fn request(&self, pcm: &[f32], abort: Option<Arc<AtomicBool>>) -> Option<Recognized> {
            let mut guard = self.engine.pool.checkout_blocking().expect("session");
            let mut req = TranscribeRequest::new(TranscribeSource::Samples(pcm));
            req.abort = abort;
            let r = self.engine.transcribe_request(req, &mut guard).ok()?;
            let ms = |s: f64| (s * 1000.0).round() as u32;
            Some(Recognized {
                text: r.text.trim().to_string(),
                words: r.words.iter().map(|w| Word(w.word.clone(), ms(w.start), ms(w.end))).collect(),
            })
        }
    }

    impl Runtime for Gigastt {
        fn device(&self) -> String {
            self.provider.clone()
        }

        fn transcribe(&mut self, pcm: &[f32]) -> Recognized {
            self.request(pcm, None).expect("transcribe")
        }

        fn abort_latency_ms(&mut self, pcm: &[f32], after_ms: u64) -> Option<f64> {
            let flag = Arc::new(AtomicBool::new(false));
            let this = &*self;
            std::thread::scope(|s| {
                let decode = s.spawn(|| this.request(pcm, Some(flag.clone())));
                std::thread::sleep(Duration::from_millis(after_ms));
                let asked = Instant::now();
                flag.store(true, Ordering::Relaxed);
                decode.join().expect("decode thread");
                Some(asked.elapsed().as_secs_f64() * 1000.0)
            })
        }
    }
}
