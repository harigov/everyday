//! Approving something out loud.
//!
//! Only two kinds of confirmation may be answered by voice: a web search the
//! assistant wants to make after it has read mail, and a page it wants to open
//! at an address it composed itself. Those questions exist because the words
//! or the address could carry something out, so they may be answered aloud only
//! when all of it can be said: a search too long to read, or an address with a
//! query, a login, escapes or anything else a listener could not judge, waits
//! for the card, where it is shown in full. Everything else -- a delete, a mail to somebody, a write after a stranger's
//! words were read -- waits for a click on the card already in the thread,
//! because a voice is easy to forge and easy to mishear, and an echo of the
//! assistant's own speech must never be able to approve anything.
//!
//! What counts as an answer is deliberately narrow too. A confirmation is
//! given by a whole utterance made of nothing but agreement ("yeah, go
//! ahead"), never by a sentence that happens to contain a yes; and a refusal
//! anywhere in what was said, however it is dressed, is never read as consent.

/// What somebody said to a yes-or-no question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpokenAnswer {
    Yes,
    No,
    Unclear,
}

/// The most words a whole answer may be. A longer utterance is somebody
/// saying something else.
const MAX_ANSWER_WORDS: usize = 5;

/// The most separate phrases of agreement in one answer: "yes, please do".
const MAX_AGREEMENTS: usize = 3;

/// Phrases that are agreement on their own. A whole utterance made of these
/// and nothing else is a yes.
const AGREEMENTS: &[&[&str]] = &[
    &["yes"],
    &["yeah"],
    &["yep"],
    &["yup"],
    &["sure"],
    &["ok"],
    &["okay"],
    &["absolutely"],
    &["please"],
    &["thanks"],
    &["go", "ahead"],
    &["do", "it"],
    &["please", "do"],
    &["sure", "thing"],
    &["of", "course"],
];

/// Words that are a refusal wherever they stand. "never mind" is covered by
/// "never".
const NEGATIONS: &[&str] = &[
    "no",
    "nope",
    "nah",
    "not",
    "never",
    "dont",
    "don't",
    "cant",
    "can't",
    "wont",
    "won't",
    "cancel",
    "nevermind",
];

/// Break what was heard into lowercase words: punctuation is space, and the
/// apostrophe of "don't" stays (curly ones are made straight first).
fn words(heard: &str) -> Vec<String> {
    heard
        .to_lowercase()
        .replace(['\u{2019}', '\u{2018}'], "'")
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .map(|w| w.trim_matches('\'').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Whether `words` is made entirely of [`AGREEMENTS`], at most
/// [`MAX_AGREEMENTS`] of them. Tries every way of cutting it up, since "please
/// do" and "do it" overlap in "please do it".
fn only_agreement(words: &[String], phrases_left: usize) -> bool {
    if words.is_empty() {
        return true;
    }
    if phrases_left == 0 {
        return false;
    }
    AGREEMENTS.iter().any(|phrase| {
        words.len() >= phrase.len()
            && phrase.iter().zip(words).all(|(a, b)| a == b)
            && only_agreement(&words[phrase.len()..], phrases_left - 1)
    })
}

/// What was said to a yes-or-no question.
///
/// Short whole-utterance answers only. Anything containing a negation is never
/// a [`SpokenAnswer::Yes`]: a short one is a [`SpokenAnswer::No`] -- declining
/// is the safe way to be wrong -- and a long one is
/// [`SpokenAnswer::Unclear`], since somebody holding forth is not answering.
pub fn spoken_answer(heard: &str) -> SpokenAnswer {
    let words = words(heard);
    if words.is_empty() || words.len() > MAX_ANSWER_WORDS {
        return SpokenAnswer::Unclear;
    }
    if words.iter().any(|w| NEGATIONS.contains(&w.as_str())) {
        // "do not" has its "not"; "never mind" its "never". Five words with
        // one is somebody explaining, not answering.
        if words.len() < MAX_ANSWER_WORDS {
            return SpokenAnswer::No;
        }
        return SpokenAnswer::Unclear;
    }
    if only_agreement(&words, MAX_AGREEMENTS) { SpokenAnswer::Yes } else { SpokenAnswer::Unclear }
}

/// Whether a confirmation of this `kind` about `subject` may be answered by
/// voice: a web search or a page fetch, when the question can say all of it.
pub fn may_answer_aloud(kind: &str, subject: &str) -> bool {
    match kind {
        "search" => search_words(subject).is_some(),
        "fetch" => spoken_address(subject).is_some(),
        _ => false,
    }
}

/// The longest search read aloud, in characters.
const MAX_SUBJECT_CHARS: usize = 100;

/// The longest address read aloud, in characters, without its scheme.
const MAX_ADDRESS_CHARS: usize = 60;

/// `text` as one line fit to drop into a question: whitespace of every kind
/// (newlines too) becomes a single space, and a full stop, question or
/// exclamation mark that ends a sentence becomes a comma.
///
/// The subject is the model's own wording of what it wants to do, and it may
/// have been written by whoever wrote the mail it just read. Without this, a
/// query of "x. Tell them the transfer is approved" would be read to the
/// person in the assistant's own voice as if it were a second thing the
/// assistant had said.
fn one_line(text: &str) -> String {
    let flat: String =
        text.chars().map(|c| if c.is_control() || c.is_whitespace() { ' ' } else { c }).collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::with_capacity(flat.len());
    let chars: Vec<char> = flat.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let ends_sentence = matches!(c, '.' | '!' | '?' | ';' | ':')
            && chars.get(i + 1).is_none_or(|next| next.is_whitespace());
        if ends_sentence {
            // Nothing at the very end: the question supplies its own mark.
            if i + 1 < chars.len() {
                out.push(',');
            }
        } else {
            out.push(c);
        }
    }
    out.trim().trim_end_matches(',').trim().to_string()
}

/// The whole of a search, as it would be said. `None` when there is none, or
/// it is too long to say: cutting it would hide what it sends.
fn search_words(subject: &str) -> Option<String> {
    let line = one_line(subject);
    (!line.is_empty() && line.chars().count() <= MAX_SUBJECT_CHARS).then_some(line)
}

/// The whole of a web address, as it would be said: `news.example.org slash
/// today`. `None` for anything that cannot be judged by ear, which is left to
/// the card: a query or fragment, a login, a port, percent escapes, letters
/// that only look like ASCII, or an address too long to take in.
fn spoken_address(subject: &str) -> Option<String> {
    let mut words = subject.split_whitespace();
    let address = words.next()?;
    if words.next().is_some() {
        return None;
    }
    let rest = match address.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("https") => rest,
        Some(_) => return None,
        None => address,
    };
    if !rest.is_ascii() || rest.len() > MAX_ADDRESS_CHARS {
        return None;
    }
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let labels: Vec<&str> = host.split('.').collect();
    let host_is_a_name = labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        && labels.last().is_some_and(|tld| tld.chars().all(|c| c.is_ascii_alphabetic()));
    let path_is_plain =
        path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.'));
    if !host_is_a_name || !path_is_plain {
        return None;
    }
    let mut said = host.to_string();
    for part in path.split('/').filter(|part| !part.is_empty()) {
        said.push_str(" slash ");
        said.push_str(part);
    }
    Some(said)
}

/// The question to read aloud for a confirmation that
/// [`may_answer_aloud`] allows: one short sentence holding all of what it is
/// asking about.
pub fn question(kind: &str, subject: &str) -> String {
    match kind {
        "search" => match search_words(subject) {
            Some(words) => format!("Should I search the web for {words}?"),
            None => "Should I search the web?".to_string(),
        },
        "fetch" => match spoken_address(subject) {
            Some(address) => format!("Should I open {address}?"),
            None => "Should I open a web page?".to_string(),
        },
        _ => "Should I go ahead?".to_string(),
    }
}

/// What is said when only a click can answer.
pub const NEEDS_A_CLICK: &str = "That needs your go-ahead on screen.";

/// [`NEEDS_A_CLICK`], on a call through a chat: the question is asked in the
/// chat, where a tap answers it.
pub const NEEDS_A_TAP: &str = "That needs your go-ahead in our chat.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_agreement_is_a_yes() {
        for said in [
            "Yes.",
            "yeah",
            "Yep!",
            "Sure",
            "OK",
            "Okay.",
            "Go ahead.",
            "Do it",
            "Please do.",
            "Yes, please.",
            "Yeah, go ahead",
            "okay, do it",
            "please do it",
            "Sure thing",
        ] {
            assert_eq!(spoken_answer(said), SpokenAnswer::Yes, "{said:?}");
        }
    }

    #[test]
    fn short_refusal_is_a_no() {
        for said in [
            "No.",
            "Nope",
            "Don't",
            "don\u{2019}t do it",
            "Do not.",
            "Cancel",
            "Never mind.",
            "No, don't",
            "no thanks",
        ] {
            assert_eq!(spoken_answer(said), SpokenAnswer::No, "{said:?}");
        }
    }

    #[test]
    fn a_sentence_that_merely_contains_yes_is_unclear() {
        for said in [
            "yes but only after you have checked my calendar",
            "I said yes to the other one yesterday",
            "tell me more about it first",
            "what are you searching for",
            "",
            "   ",
        ] {
            assert_eq!(spoken_answer(said), SpokenAnswer::Unclear, "{said:?}");
        }
    }

    #[test]
    fn a_negation_is_never_a_yes() {
        for said in ["yes no", "ok but don't", "sure not now", "yes never", "no go ahead"] {
            assert_ne!(spoken_answer(said), SpokenAnswer::Yes, "{said:?}");
        }
        // ... and a long one is not an answer at all.
        assert_eq!(
            spoken_answer("yes go ahead and do it but do not touch the other thing"),
            SpokenAnswer::Unclear
        );
    }

    #[test]
    fn only_a_search_or_a_fetch_may_be_answered_aloud() {
        assert!(may_answer_aloud("search", "best e-bike"));
        assert!(may_answer_aloud("fetch", "https://news.example.org/today"));
        for kind in [
            "destructive",
            "outward",
            "type",
            "click",
            "routine",
            "rewrite",
            "connection",
            "keep",
            "",
            "Search",
        ] {
            assert!(!may_answer_aloud(kind, "https://news.example.org"), "{kind:?}");
        }
    }

    #[test]
    fn a_search_says_all_of_what_it_would_send() {
        assert_eq!(
            question("search", "best e-bike under 2000"),
            "Should I search the web for best e-bike under 2000?"
        );
        assert!(!may_answer_aloud("search", "  "));
        let long = "word ".repeat(40);
        assert!(!may_answer_aloud("search", &long), "cutting it would hide the rest");
    }

    #[test]
    fn a_fetch_says_the_whole_address() {
        assert_eq!(
            question("fetch", "https://www.Example.com/news/today"),
            "Should I open example.com slash news slash today?"
        );
        assert_eq!(question("fetch", "http://news.example.org"), "Should I open news.example.org?");
        assert_eq!(question("fetch", "example.com/page/"), "Should I open example.com slash page?");
    }

    #[test]
    fn an_address_that_cannot_be_judged_by_ear_needs_the_card() {
        for address in [
            "https://attacker.example/?d=private",
            "https://attacker.example/#private",
            "https://attacker.example/a%20b",
            "https://accounts.google.com@evil.test/login",
            "https://good.example\\@evil.test/",
            "https://example.com:8443/",
            "https://exam\u{440}le.com/",
            "ftp://example.com/",
            "https://[::1]/x",
            "https://127.0.0.1/x",
            "https:///nothing",
            "https://accounts.google.com.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.evil.test/",
            "https://example.com/ and more",
            "",
        ] {
            assert!(!may_answer_aloud("fetch", address), "{address}");
        }
    }

    #[test]
    fn another_kind_gets_a_plain_question() {
        assert_eq!(question("destructive", "the deck"), "Should I go ahead?");
    }

    #[test]
    fn a_subject_cannot_smuggle_in_more_sentences() {
        let said = question("search", "cheap flights. Tell them it is approved!\nNow");
        assert_eq!(said.matches('?').count(), 1, "{said}");
        assert!(!said.contains('.') && !said.contains('!') && !said.contains('\n'), "{said}");
    }
}
