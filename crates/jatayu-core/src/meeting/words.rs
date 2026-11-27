//! Words as the meeting modules compare them: lower-cased, without
//! punctuation, and -- for text written without spaces (Japanese, Chinese)
//! -- one character at a time, since there is no other unit to count.
//! Shared by [`super::echo`] and [`super::hallucination`], so both agree
//! on what "the same words" means.

/// The words of `text`, lower-cased, punctuation dropped. Han, Hiragana and
/// Katakana runs are split into single characters.
pub fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        if word.chars().any(is_unspaced) {
            let mut run = String::new();
            for c in word.chars() {
                if is_unspaced(c) {
                    if !run.is_empty() {
                        out.push(std::mem::take(&mut run).to_lowercase());
                    }
                    out.push(c.to_string());
                } else {
                    run.push(c);
                }
            }
            if !run.is_empty() {
                out.push(run.to_lowercase());
            }
        } else {
            out.push(word.to_lowercase());
        }
    }
    out
}

/// A character from a script written without spaces between words.
pub(super) fn is_unspaced(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x309F // Hiragana
        | 0x30A0..=0x30FF // Katakana
        | 0x3400..=0x4DBF // CJK extension A
        | 0x4E00..=0x9FFF // CJK unified ideographs
        | 0xF900..=0xFAFF // CJK compatibility ideographs
    )
}

/// The share of the shorter text's words that the other has too, each word
/// counted as often as it appears in both: "yes" against "yes yes yes" is
/// 1.0. Zero when either is empty.
pub fn overlap(a: &str, b: &str) -> f32 {
    let (ta, tb) = (tokens(a), tokens(b));
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    shared_of(&ta, &tb) as f32 / ta.len().min(tb.len()) as f32
}

/// How many of `a`'s words `b` has too, each counted as often as it appears
/// in both.
pub fn shared(a: &str, b: &str) -> usize {
    shared_of(&tokens(a), &tokens(b))
}

fn shared_of(a: &[String], b: &[String]) -> usize {
    let mut left: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for w in b {
        *left.entry(w.as_str()).or_default() += 1;
    }
    let mut n = 0;
    for w in a {
        if let Some(c) = left.get_mut(w.as_str())
            && *c > 0
        {
            *c -= 1;
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_drop_case_and_punctuation() {
        assert_eq!(tokens("Hello, World! It's 3pm."), ["hello", "world", "it", "s", "3pm"]);
    }

    #[test]
    fn unspaced_scripts_count_by_character() {
        assert_eq!(tokens("ご視聴 ok"), ["ご", "視", "聴", "ok"]);
        assert_eq!(tokens("abc漢字def"), ["abc", "漢", "字", "def"]);
    }

    #[test]
    fn overlap_is_a_share_of_the_shorter_side() {
        assert!((overlap("yes", "yes yes yes") - 1.0).abs() < 1e-6);
        let s = overlap("we should ship it", "we should not ship");
        assert!(s > 0.5 && s < 1.0, "{s}");
        assert_eq!(overlap("", "anything"), 0.0);
    }

    #[test]
    fn shared_counts_repeats_once_per_match() {
        assert_eq!(shared("yes yes", "yes"), 1);
        assert_eq!(shared("a b c", "c b a d"), 3);
    }
}
