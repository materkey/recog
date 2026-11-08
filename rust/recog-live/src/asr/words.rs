//! Words from wavo's tokens, the way GigaAM's `frames_to_words` makes them
//! (`words_from_tokens` of `recog/wavo_backend.py`).

use recog_core::py;

use super::Word;

/// GigaAM's encoder frame: wavo gives each token's start as its frame times 40 ms.
pub const FRAME_MS: i64 = 40;

/// The word-start mark of SentencePiece pieces.
const WORD_START: char = '\u{2581}';

/// Words from `(piece, start_ms)` tokens.
///
/// A piece starting with "▁" starts a word; a word runs from its first token's frame to
/// the end of its last token's frame. A word whose text is empty after Python's
/// `str.strip` is left out.
#[must_use]
pub fn words_from_tokens<S: AsRef<str>>(tokens: &[(S, u32)]) -> Vec<Word> {
    let mut words = Vec::new();
    let mut pieces = String::new();
    let mut starts: Vec<i64> = Vec::new();

    let mut commit = |pieces: &mut String, starts: &mut Vec<i64>| {
        let text = py::strip(pieces);
        if let (false, Some(&first), Some(&last)) = (text.is_empty(), starts.first(), starts.last()) {
            words.push(Word { text: text.to_owned(), start: first, end: last + FRAME_MS });
        }
        pieces.clear();
        starts.clear();
    };

    for (piece, start) in tokens {
        let mut piece = piece.as_ref();
        if let Some(rest) = piece.strip_prefix(WORD_START) {
            commit(&mut pieces, &mut starts);
            piece = rest;
        }
        pieces.push_str(piece);
        starts.push(i64::from(*start));
    }
    commit(&mut pieces, &mut starts);
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, start: i64, end: i64) -> Word {
        Word { text: text.into(), start, end }
    }

    #[test]
    fn tokens_with_word_starts_words_from_tokens_gives_gigaams_words() {
        let tokens = [("▁При", 0), ("вет", 40), ("▁мир", 200)];
        assert_eq!(words_from_tokens(&tokens), vec![w("Привет", 0, 80), w("мир", 200, 240)]);
    }

    #[test]
    fn leading_pieces_without_a_word_start_words_from_tokens_makes_them_the_first_word() {
        let tokens = [("а", 0), ("б", 40), ("▁в", 80)];
        assert_eq!(words_from_tokens(&tokens), vec![w("аб", 0, 80), w("в", 80, 120)]);
    }

    #[test]
    fn whitespace_only_word_words_from_tokens_leaves_it_out() {
        let tokens = [("▁", 0), ("▁ ", 40), ("▁x", 80), ("▁\u{3000}", 120)];
        assert_eq!(words_from_tokens(&tokens), vec![w("x", 80, 120)]);
    }

    #[test]
    fn no_tokens_words_from_tokens_gives_no_words() {
        let tokens: [(&str, u32); 0] = [];
        assert!(words_from_tokens(&tokens).is_empty());
    }
}
