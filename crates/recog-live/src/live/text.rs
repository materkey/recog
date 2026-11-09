//! The pure helpers of `recog/live.py`: word normalisation and the last-words dedup,
//! the silence test of a chunk and of a participant's PCM, and the system-audio spec.
//!
//! Known difference: `str.isalnum()` is taken as "general category L* or N*" from
//! Unicode 15.0 (Python 3.12's database). Python's `isnumeric` also counts a few
//! characters of other categories with a numeric value, none of which ASR writes.

use unicode_general_category::{GeneralCategory, get_general_category};

use crate::config::VAD_RMS_THRESHOLD;

/// `str.isalnum()` of one character.
fn is_alnum(c: char) -> bool {
    use GeneralCategory as G;
    matches!(
        get_general_category(c),
        G::UppercaseLetter
            | G::LowercaseLetter
            | G::TitlecaseLetter
            | G::ModifierLetter
            | G::OtherLetter
            | G::DecimalNumber
            | G::LetterNumber
            | G::OtherNumber
    )
}

/// `normalize_word`: punctuation stripped and lowercased, for the dedup comparison.
#[must_use]
pub fn normalize_word(word: &str) -> String {
    word.chars().filter(|c| is_alnum(*c)).flat_map(char::to_lowercase).collect()
}

/// `deduplicate(text, prev_words)`: the text without its leading overlap with the end
/// of `prev_words` (at most six words, compared normalised); None when all of it is a
/// repeat. An empty text or no previous words give the text back as it is.
#[must_use]
pub fn deduplicate(text: &str, prev_words: &[String]) -> Option<String> {
    if prev_words.is_empty() || text.is_empty() {
        return Some(text.to_owned());
    }
    let words: Vec<&str> = recog_core::py::split_whitespace(text).collect();
    if words.is_empty() {
        return Some(text.to_owned());
    }
    let norm_words: Vec<String> = words.iter().map(|w| normalize_word(w)).collect();
    let norm_prev: Vec<String> = prev_words.iter().map(|w| normalize_word(w)).collect();
    let max_overlap = words.len().min(prev_words.len()).min(6);
    for overlap in (1..=max_overlap).rev() {
        if norm_words[..overlap] == norm_prev[norm_prev.len() - overlap..] {
            let result = words[overlap..].join(" ");
            return if recog_core::py::strip(&result).is_empty() { None } else { Some(result) };
        }
    }
    Some(text.to_owned())
}

/// The last `n` words of `text` (`text.split()[-n:]`).
#[must_use]
pub fn last_words(text: &str, n: usize) -> Vec<String> {
    let words: Vec<&str> = recog_core::py::split_whitespace(text).collect();
    words[words.len().saturating_sub(n)..].iter().map(|w| (*w).to_owned()).collect()
}

/// numpy's pairwise summation of a contiguous float32 array (`np.add.reduce`), in
/// float32 as numpy accumulates a float32 array.
fn pairwise_sum_f32(a: &[f32]) -> f32 {
    const BLOCK: usize = 128;
    let n = a.len();
    if n < 8 {
        let mut res = 0.0f32;
        for &v in a {
            res += v;
        }
        res
    } else if n <= BLOCK {
        let mut r = [a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]];
        let mut i = 8;
        while i < n - (n % 8) {
            for (k, acc) in r.iter_mut().enumerate() {
                *acc += a[i + k];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        pairwise_sum_f32(&a[..n2]) + pairwise_sum_f32(&a[n2..])
    }
}

/// `np.sqrt(np.mean(x ** 2))` of float32 samples, in float32 as numpy computes it; NaN
/// for no samples (numpy's mean of an empty array).
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn rms_f32(samples: &[f32]) -> f64 {
    let squares: Vec<f32> = samples.iter().map(|v| v * v).collect();
    let mean = (0.0f32 + pairwise_sum_f32(&squares)) / squares.len() as f32;
    f64::from(mean.sqrt())
}

/// `pcm_is_silent`: a participant's Int16 PCM below the mixed path's silence threshold.
#[must_use]
pub fn pcm_is_silent(pcm: &[i16]) -> bool {
    if pcm.is_empty() {
        return true;
    }
    let samples: Vec<f32> = pcm.iter().map(|&v| f32::from(v) / 32768.0).collect();
    rms_f32(&samples) < VAD_RMS_THRESHOLD
}

/// `normalize_system_audio`: one system-audio spec from the new flag and the old ones,
/// `--system-audio` over `--screencapture` over `--system-device`.
#[must_use]
pub fn normalize_system_audio(system_audio: Option<&str>, system_device: Option<&str>, screencapture: bool) -> String {
    if let Some(spec) = system_audio.filter(|s| !s.is_empty()) {
        return spec.to_owned();
    }
    if screencapture {
        return "screencapture".to_owned();
    }
    if let Some(device) = system_device {
        return format!("device:{device}");
    }
    crate::live::NO_SYSTEM_AUDIO.to_owned()
}

/// `parse_system_audio_device`: the device of a `device:<id>` spec (its text; Python
/// turns a numeric one into an int), None for the other backends.
#[must_use]
pub fn parse_system_audio_device(spec: &str) -> Option<&str> {
    spec.strip_prefix("device:")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn a_word_with_punctuation_normalize_word_strips_it() {
        assert_eq!(normalize_word("hello,"), "hello");
        assert_eq!(normalize_word("world!"), "world");
        assert_eq!(normalize_word("\"quoted\""), "quoted");
    }

    #[test]
    fn mixed_case_normalize_word_lowercases() {
        assert_eq!(normalize_word("Hello"), "hello");
        assert_eq!(normalize_word("WORLD"), "world");
    }

    #[test]
    fn punctuation_and_case_normalize_word_does_both() {
        assert_eq!(normalize_word("Hello,"), "hello");
    }

    #[test]
    fn empty_or_only_punctuation_normalize_word_gives_empty() {
        assert_eq!(normalize_word(""), "");
        assert_eq!(normalize_word("..."), "");
        assert_eq!(normalize_word("—"), "");
    }

    #[test]
    fn cyrillic_normalize_word_keeps_letters_lowercased() {
        assert_eq!(normalize_word("Привет,"), "привет");
        assert_eq!(normalize_word("МИР!"), "мир");
        assert_eq!(normalize_word("ёлка"), "ёлка");
    }

    #[test]
    fn leading_overlap_deduplicate_removes_it() {
        assert_eq!(deduplicate("world foo bar", &words("hello world")).as_deref(), Some("foo bar"));
    }

    #[test]
    fn everything_repeated_deduplicate_gives_none() {
        assert_eq!(deduplicate("hello world", &words("hello world")), None);
    }

    #[test]
    fn no_overlap_deduplicate_gives_the_text() {
        assert_eq!(deduplicate("foo bar", &words("hello world")).as_deref(), Some("foo bar"));
    }

    #[test]
    fn no_previous_words_or_empty_text_deduplicate_gives_the_text() {
        assert_eq!(deduplicate("foo bar", &[]).as_deref(), Some("foo bar"));
        assert_eq!(deduplicate("", &words("hello")).as_deref(), Some(""));
    }

    #[test]
    fn overlap_in_other_case_and_punctuation_deduplicate_still_removes_it() {
        assert_eq!(deduplicate("World foo", &words("hello world")).as_deref(), Some("foo"));
        assert_eq!(deduplicate("world, foo", &words("hello world.")).as_deref(), Some("foo"));
    }

    #[test]
    fn seven_repeated_words_deduplicate_removes_at_most_six() {
        let prev = words("a b c d e f g");
        assert_eq!(deduplicate("b c d e f g h", &prev).as_deref(), Some("h"));
        assert_eq!(deduplicate("a b c d e f g h", &prev).as_deref(), Some("a b c d e f g h"));
    }

    #[test]
    fn whitespace_only_text_deduplicate_gives_it_back() {
        assert_eq!(deduplicate("   ", &words("a")).as_deref(), Some("   "));
    }

    #[test]
    fn a_long_text_last_words_takes_the_last_five() {
        assert_eq!(last_words("a b c d e f g", 5), words("c d e f g"));
        assert_eq!(last_words("a  b", 5), words("a b"));
    }

    #[test]
    fn silence_and_speech_pcm_is_silent_tells_them_apart() {
        assert!(pcm_is_silent(&[]));
        assert!(pcm_is_silent(&[0; 1600]));
        assert!(pcm_is_silent(&[100; 1600]));
        assert!(!pcm_is_silent(&[1000; 1600]));
    }

    #[test]
    fn constant_signal_rms_f32_is_its_level() {
        assert!((rms_f32(&[0.5; 48_000]) - 0.5).abs() < 1e-6);
        assert!(rms_f32(&[]).is_nan());
    }

    #[test]
    fn system_audio_flags_normalize_system_audio_prefers_the_new_one() {
        assert_eq!(normalize_system_audio(Some("pulse"), Some("3"), true), "pulse");
        assert_eq!(normalize_system_audio(None, Some("3"), true), "screencapture");
        assert_eq!(normalize_system_audio(None, Some("3"), false), "device:3");
        assert_eq!(normalize_system_audio(None, None, false), "none");
        assert_eq!(normalize_system_audio(Some(""), None, false), "none");
    }

    #[test]
    fn a_device_spec_parse_system_audio_device_gives_its_id() {
        assert_eq!(parse_system_audio_device("device:5"), Some("5"));
        assert_eq!(parse_system_audio_device("device:BlackHole 2ch"), Some("BlackHole 2ch"));
        assert_eq!(parse_system_audio_device("pulse"), None);
    }
}
