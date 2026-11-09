//! The model behind the GigaAM backend: what `model.transcribe_longform(path,
//! word_timestamps=…)` gives the Python backend, segments and words in float seconds.
//!
//! [`WavoTranscriber`] runs GigaAM v3 on the wavo crate (Q8_0 GGUF, the GPU through
//! wgpu), as `rust/recog-live` does. GigaAM's longform mode cuts the audio at pyannote
//! VAD's speech segments; wavo has no VAD, so the audio is cut with `wavo::split` at its
//! quietest moments into pieces of at most the model's window (25 s), and each piece
//! that has text is one segment, from its first to its last sample. Words come from the
//! tokens as GigaAM's `frames_to_words` makes them (`recog/wavo_backend.py`): a word runs
//! from its first token's frame to the end of its last token's 40 ms frame.
//!
//! Known difference: segment boundaries are the cuts, not speech onsets and offsets, so
//! a segment can start or end in silence; the text is wavo's, not PyTorch's.

use recog_core::py;

/// GigaAM's encoder frame, in ms: wavo gives each token's start as its frame times 40.
const FRAME_MS: u64 = 40;

/// The word-start mark of SentencePiece pieces.
const WORD_START: char = '\u{2581}';

/// A piece of text with its time span in seconds: a segment or a word.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// `LongformTranscriptionResult`: the segments, and the words when asked for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Longform {
    pub segments: Vec<Span>,
    pub words: Vec<Span>,
}

/// What the backend needs of the model.
pub trait Transcriber: Send + Sync {
    /// The segments of 16 kHz mono samples in [-1, 1], and their words when
    /// `word_timestamps` is set.
    ///
    /// # Errors
    /// The model's failure, which the backend answers with a 500.
    fn transcribe_longform(&self, pcm: &[f32], word_timestamps: bool) -> Result<Longform, String>;
}

/// The wavo hub name of a `GIGAAM_VARIANT` (`gigaam.load_model`'s names).
#[must_use]
pub fn hub_name(variant: &str) -> Option<&'static str> {
    match variant {
        "v3_e2e_rnnt" => Some("gigaam-v3"),
        "v3_e2e_ctc" => Some("gigaam-v3-e2e-ctc"),
        "v3_rnnt" => Some("gigaam-v3-rnnt"),
        "v3_ctc" => Some("gigaam-v3-ctc"),
        _ => None,
    }
}

/// Words from `(piece, start_ms)` tokens, their times shifted by `offset_ms`.
#[must_use]
pub fn words_from_tokens(tokens: &[(String, u32)], offset_ms: u64) -> Vec<Span> {
    let mut words = Vec::new();
    let mut pieces = String::new();
    let mut starts: Vec<u64> = Vec::new();
    let mut commit = |pieces: &mut String, starts: &mut Vec<u64>| {
        let text = py::strip(pieces);
        if let (false, Some(&first), Some(&last)) = (text.is_empty(), starts.first(), starts.last()) {
            words.push(Span {
                text: text.to_owned(),
                start: (first + offset_ms) as f64 / 1000.0,
                end: (last + FRAME_MS + offset_ms) as f64 / 1000.0,
            });
        }
        pieces.clear();
        starts.clear();
    };
    for (piece, start) in tokens {
        let mut piece = piece.as_str();
        if let Some(rest) = piece.strip_prefix(WORD_START) {
            commit(&mut pieces, &mut starts);
            piece = rest;
        }
        pieces.push_str(piece);
        starts.push(u64::from(*start));
    }
    commit(&mut pieces, &mut starts);
    words
}

/// What one piece gave: its text and tokens, token times from the piece's start.
pub type PieceAnswer = (String, Vec<(String, u32)>);

/// The longform result of pieces cut at `ranges` (sample ranges of 16 kHz audio).
#[must_use]
pub fn assemble(ranges: &[std::ops::Range<usize>], answers: Vec<PieceAnswer>, word_timestamps: bool) -> Longform {
    let mut out = Longform::default();
    for (range, (text, tokens)) in ranges.iter().zip(answers) {
        if py::strip(&text).is_empty() {
            continue;
        }
        out.segments.push(Span { text, start: range.start as f64 / 16_000.0, end: range.end as f64 / 16_000.0 });
        if word_timestamps {
            out.words.extend(words_from_tokens(&tokens, range.start as u64 / 16));
        }
    }
    out
}

/// GigaAM on wavo's GPU backend.
pub struct WavoTranscriber {
    model: wavo::Model,
}

impl WavoTranscriber {
    /// The variant's model from the Hugging Face cache, downloaded there first when it
    /// is missing, loaded on the GPU.
    ///
    /// # Errors
    /// A variant wavo does not have, or the download's or the load's error.
    pub fn load(variant: &str) -> Result<Self, String> {
        let name = hub_name(variant).ok_or_else(|| {
            format!("GIGAAM_VARIANT {variant} is not available on wavo (v3_e2e_rnnt, v3_e2e_ctc, v3_rnnt, v3_ctc)")
        })?;
        let model = wavo::Model::load_with(&model_path(name)?, wavo::Backend::Gpu).map_err(|e| e.to_string())?;
        Ok(Self { model })
    }

    /// The GPU it runs on, for the log.
    #[must_use]
    pub fn device(&self) -> String {
        self.model.device().to_string()
    }
}

/// The model's file in the Hugging Face cache, downloaded there first when missing.
fn model_path(name: &str) -> Result<std::path::PathBuf, String> {
    let hub = wavo::hub::Hub::new();
    hub.find(name)
        .map_or_else(|| hub.pull(name, |_| std::ops::ControlFlow::Continue(())).map_err(|e| e.to_string()), Ok)
}

/// The longform result of the pieces of `pcm` cut at `ranges`, each transcribed by
/// `piece`; the first failure is the result.
///
/// # Errors
/// A piece's failure.
fn transcribe_pieces(
    pcm: &[f32],
    ranges: &[std::ops::Range<usize>],
    word_timestamps: bool,
    piece: impl FnMut(&[f32]) -> Result<PieceAnswer, String>,
) -> Result<Longform, String> {
    let answers = ranges.iter().map(|range| &pcm[range.clone()]).map(piece).collect::<Result<Vec<_>, _>>()?;
    Ok(assemble(ranges, answers, word_timestamps))
}

impl WavoTranscriber {
    fn transcribe_piece(&self, pcm: &[f32]) -> Result<PieceAnswer, String> {
        let transcript = self.model.transcribe(pcm).map_err(|e| e.to_string())?;
        Ok((transcript.text, transcript.tokens.into_iter().map(|t| (t.piece, t.start_ms)).collect()))
    }
}

impl Transcriber for WavoTranscriber {
    fn transcribe_longform(&self, pcm: &[f32], word_timestamps: bool) -> Result<Longform, String> {
        let ranges = wavo::split(pcm, self.model.max_audio_ms().unwrap_or(25_000));
        transcribe_pieces(pcm, &ranges, word_timestamps, |piece| self.transcribe_piece(piece))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(pairs: &[(&str, u32)]) -> Vec<(String, u32)> {
        pairs.iter().map(|(p, s)| ((*p).to_owned(), *s)).collect()
    }

    #[test]
    fn tokens_of_a_later_piece_when_made_words_then_seconds_from_the_audio_start() {
        let words = words_from_tokens(&tokens(&[("▁При", 0), ("вет", 40), ("▁мир", 200)]), 20_000);
        assert_eq!(
            words,
            vec![
                Span { text: "Привет".into(), start: 20.0, end: 20.08 },
                Span { text: "мир".into(), start: 20.2, end: 20.24 },
            ]
        );
    }

    #[test]
    fn a_silent_piece_when_assembled_then_no_segment_and_the_others_keep_their_span() {
        let ranges = [0..160_000, 160_000..400_000];
        let answers = vec![(" ".to_owned(), Vec::new()), ("Да.".to_owned(), tokens(&[("▁Да", 80), (".", 120)]))];
        let longform = assemble(&ranges, answers, true);
        assert_eq!(longform.segments, vec![Span { text: "Да.".into(), start: 10.0, end: 25.0 }]);
        assert_eq!(longform.words, vec![Span { text: "Да.".into(), start: 10.08, end: 10.16 }]);
    }

    #[test]
    fn words_not_asked_for_when_assembled_then_none() {
        let ranges = [0..16_000, 16_000..32_000];
        let answers = vec![("x".into(), tokens(&[("▁x", 0)])), ("y".into(), tokens(&[("▁y", 0)]))];
        let longform = assemble(&ranges, answers, false);
        assert_eq!(longform.segments.len(), 2);
        assert!(longform.words.is_empty());
    }

    #[test]
    fn pieces_when_transcribed_then_each_gets_its_samples_and_the_answers_are_assembled() {
        let pcm: Vec<f32> = (0..48_000u16).map(f32::from).collect();
        let mut seen = Vec::new();
        let longform = transcribe_pieces(&pcm, &[0..16_000, 16_000..48_000], true, |piece| {
            seen.push((piece[0], piece.len()));
            Ok((format!("p{}", seen.len()), tokens(&[("▁w", 40)])))
        })
        .unwrap();
        assert_eq!(seen, [(0.0, 16_000), (16_000.0, 32_000)]);
        assert_eq!(
            longform.segments,
            vec![Span { text: "p1".into(), start: 0.0, end: 1.0 }, Span { text: "p2".into(), start: 1.0, end: 3.0 }]
        );
        assert_eq!(
            longform.words,
            vec![Span { text: "w".into(), start: 0.04, end: 0.08 }, Span { text: "w".into(), start: 1.04, end: 1.08 }]
        );
    }

    #[test]
    fn a_failing_piece_when_transcribed_then_its_error_and_no_later_piece() {
        let mut calls = 0;
        let result = transcribe_pieces(&[0.0; 32_000], &[0..16_000, 16_000..32_000], false, |_| {
            calls += 1;
            Err("device lost".to_owned())
        });
        assert_eq!(result, Err("device lost".to_owned()));
        assert_eq!(calls, 1);
    }

    #[test]
    fn a_variant_wavo_lacks_when_loaded_then_the_message_names_the_ones_it_has() {
        let Err(error) = WavoTranscriber::load("v2_rnnt") else { panic!("v2_rnnt loaded") };
        assert_eq!(error, "GIGAAM_VARIANT v2_rnnt is not available on wavo (v3_e2e_rnnt, v3_e2e_ctc, v3_rnnt, v3_ctc)");
    }

    #[test]
    fn the_python_variants_when_mapped_then_wavos_gigaam_v3_models() {
        assert_eq!(hub_name("v3_e2e_rnnt"), Some("gigaam-v3"));
        assert_eq!(hub_name("v2_rnnt"), None);
    }
}
