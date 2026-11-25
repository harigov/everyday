//! Telling Parakeet which words to expect: sherpa-onnx's hotwords, which
//! raise the score of a listed word's pieces while it decodes. What the words
//! are is `karmayoga_core::speech::words`; this is how Parakeet is handed
//! them. Plain Rust, checked by plain `cargo test`; [`super::local`] does the
//! loading.
//!
//! # What sherpa-onnx needs
//!
//! Hotwords work only with its beam search (`modified_beam_search`), which
//! [`super::local::Recogniser`] therefore always uses for Parakeet: with no
//! words to expect it does what the greedy search did. Each stream is then
//! made with that turn's words ([`text`]), one word or phrase to a line.
//!
//! sherpa-onnx cuts each word into the model's pieces itself, with
//! simple-sentencepiece, which needs a vocabulary with a score for each
//! piece -- a file the Parakeet download does not include. [`vocabulary`]
//! writes one from `tokens.txt`. Parakeet's tokenizer is SentencePiece BPE
//! (merged pieces first in the order they were learned, single characters
//! last), so the scores come from each piece's place in that order. Simple
//! sentencepiece does not merge as BPE does; it picks the best-scoring cut,
//! and of the scorings tried, minus the logarithm of the place agreed with
//! BPE most often (69 of 91 names and words). A word cut differently from
//! how the model writes it is boosted only as far as the cuts agree, which
//! does no harm: sherpa takes back the boost of a word left unfinished.
//!
//! # Scores
//!
//! Each line carries its own boost ([`BOOST`]), which sherpa takes back when
//! the word is not finished. The recogniser's own `hotwords_score` is also
//! added, for good, to every piece that continues any listed word, before
//! the beam is chosen, so it is kept tiny ([`BACKGROUND`]): large, it would
//! tilt every word that starts like a name towards it.
//!
//! # What must never reach sherpa
//!
//! A word starting with `:` is read as a score, with `#` as a threshold, and
//! either fails to parse as a number by throwing -- which, through the C API,
//! aborts the process; `/` starts a new line. A modelling unit other than
//! the few it knows exits the process too, and so does a vocabulary line
//! without two fields. So [`text`] keeps only letters, digits, spaces,
//! apostrophes and hyphens, and [`vocabulary`] only lines it could parse.

// Without local speech nothing hands Parakeet anything, but these plain parts
// are still built and tested.
#![cfg_attr(not(feature = "speech"), allow(dead_code))]

/// What each listed word's pieces are raised by, in log-probability: enough
/// for "Karma" to win over "calmer" when the sound is between them, not so
/// much that a word is heard where nothing like it was said.
pub const BOOST: f32 = 2.0;

/// The recogniser's own `hotwords_score`: see this module's doc on scores.
/// Not zero, which sherpa-onnx's C API reads as "use the default" (1.5).
pub const BACKGROUND: f32 = 0.05;

/// The modelling unit sherpa-onnx is told Parakeet's pieces are.
pub const MODELING_UNIT: &str = "bpe";

/// What the vocabulary [`vocabulary`] writes is called, beside `tokens.txt`.
pub const VOCABULARY_FILE: &str = "bpe.vocab";

/// The simple-sentencepiece vocabulary for a SentencePiece BPE model whose
/// `tokens.txt` is `tokens`: one `piece score` line for each piece, in the
/// file's order. The model's special tokens (`<pad>`, `<|nospeech|>`,
/// `<blk>`...) are left out, but for `<unk>`, which simple-sentencepiece
/// looks for. See this module's doc on why the scores are what they are.
pub fn vocabulary(tokens: &str) -> String {
    let mut out = String::with_capacity(tokens.len() * 2);
    for line in tokens.lines() {
        let Some((piece, id)) = line.rsplit_once(' ') else { continue };
        let Ok(id) = id.trim().parse::<u32>() else { continue };
        if piece.is_empty() || piece.contains(char::is_whitespace) {
            continue;
        }
        if piece.starts_with('<') && piece.ends_with('>') && piece != "<unk>" {
            continue;
        }
        // The first piece's score is nought, written without a minus sign.
        let score = if id == 0 { 0.0 } else { -((id as f64) + 1.0).ln() };
        out.push_str(&format!("{piece} {score:.6}\n"));
    }
    out
}

/// The hotwords for one stream: each of `words` on a line of its own with
/// its [`BOOST`], kept to the characters sherpa-onnx reads as words. Empty
/// when none is left.
pub fn text(words: &[String]) -> String {
    let mut lines = Vec::new();
    for word in words {
        let kept: String = word
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '\'' || c == '-' { c } else { ' ' })
            .collect();
        let kept = kept
            .split_whitespace()
            // A word of only hyphens or apostrophes is no word.
            .filter(|w| w.chars().any(char::is_alphanumeric))
            .collect::<Vec<_>>()
            .join(" ");
        if !kept.is_empty() {
            lines.push(format!("{kept} :{BOOST:.1}"));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vocabulary_scores_every_piece_by_its_place_and_drops_special_tokens() {
        let tokens = "<unk> 0\n<|nospeech|> 1\n<pad> 2\nen 3\n▁s 4\nthe two 5\nbad\nŃ 6\n<blk> 7\n";
        assert_eq!(vocabulary(tokens), "<unk> 0.000000\nen -1.386294\n▁s -1.609438\nŃ -1.945910\n");
    }

    #[test]
    fn every_vocabulary_line_has_exactly_two_fields() {
        let tokens = (0..200).map(|i| format!("p{i} {i}\n")).collect::<String>();
        for line in vocabulary(&tokens).lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            assert_eq!(fields.len(), 2, "{line}");
            assert!(fields[1].parse::<f32>().is_ok(), "{line}");
        }
    }

    #[test]
    fn each_word_is_a_line_with_its_boost() {
        let words = vec!["Karma".to_string(), "Priya Shah".to_string()];
        assert_eq!(text(&words), "Karma :2.0\nPriya Shah :2.0");
    }

    #[test]
    fn nothing_sherpa_reads_as_syntax_survives() {
        let words = vec![
            ":5 Karma".to_string(),
            "C# / F#".to_string(),
            "#1 fan".to_string(),
            "O'Neil-Smith".to_string(),
            "--".to_string(),
            "\u{0}".to_string(),
        ];
        assert_eq!(text(&words), "5 Karma :2.0\nC F :2.0\n1 fan :2.0\nO'Neil-Smith :2.0");
        for line in text(&words).lines() {
            let (words, score) = line.rsplit_once(" :").unwrap();
            assert!(!words.contains([':', '#', '/', '@']), "{line}");
            assert!(score.parse::<f32>().is_ok());
        }
    }

    #[test]
    fn no_words_is_no_text() {
        assert_eq!(text(&[]), "");
        assert_eq!(text(&["  ".to_string()]), "");
    }
}
