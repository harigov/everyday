//! What a voice is handed: a sentence of plain prose, with a little in it that
//! says how it is to be said and is never shown.
//!
//! Two marks, and nothing else:
//!
//! - **`{word|respelling}`**: say `word` the way `respelling` spells it, in
//!   plain letters with hyphens between the syllables and the stressed one in
//!   capitals -- `{Siobhan|shiv-AWN}`. A spoken reply writes these itself (the
//!   core's note to a spoken turn asks for them), and [`Book::mark`] writes
//!   them for the words the person has said how to say ([`Pronunciation`]).
//!   Each voice says the respelling its own way: Kokoro turns it into its
//!   phonemes, syllable by syllable, and the others read it as letters
//!   ([`respelled`]).
//! - **[`LINE_END`]** at the end of a sentence: it ended a line -- an item of a
//!   list, the end of a paragraph -- which [`pause_after`] gives a longer
//!   pause than a full stop. The splitter in [`super::text`] puts it there,
//!   since by the time a sentence is cut the line break is gone.
//!
//! Everything a sentence becomes other than audio -- the reply on screen and
//! as kept, what was heard of it, what the microphone is compared with --
//! goes through [`shown`] first, which leaves the word and drops the rest.
//! [`Shown`] does the same for a reply as it streams, holding back a `{` until
//! it is known whether it opens a mark.
//!
//! # Whose marks are said
//!
//! A mark has the voice say something other than what is shown, so only the
//! assistant's own spoken reply is said with its marks, and the person's own
//! pronunciations. Everything else a voice says -- a question about a call it
//! is about to make, something read aloud, an update, Listen -- is made
//! [`inert`] first and said as written: a stranger's text cannot make a
//! question say one thing while another is sent, or a mail read aloud say
//! what it does not. A reply written after reading a stranger's words could
//! still be led into marking a word with another; a respelling holds only
//! letters, spaces, hyphens and apostrophes, and no more words than the word
//! it is for and one more, of at most [`MAX_PRONOUNCED_WORDS`]
//! ([`Pronunciation::validate`] holds the person's to the same), which keeps
//! that to a word or two, and nothing that approves anything is said with
//! marks.
//!
//! A `{` that does not open a well-formed mark is only a brace, said and shown
//! as the text has it.
//!
//! # Pauses
//!
//! A voice's own pause between sentences is whatever its model leaves at the
//! end of a clip, which differs wildly: Phonon leaves half a second or more
//! after each, Kokoro next to nothing, so lists ran together on one and
//! replies dragged on the other. [`pause_after`] says how long the pause after
//! a sentence should be, from how it ends, and the voice's own silence is
//! trimmed so that this is what is heard. See `super::synth::Delivery`.

use karmayoga_core::speech::{
    MAX_PRONOUNCED_CHARS, MAX_PRONOUNCED_WORDS, MAX_SAY_CHARS, Pronunciation,
};
use std::ops::Range;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Closes a sentence that ended a line. Not white space, so trimming a
/// sentence keeps it.
pub const LINE_END: char = '¶';

/// One stretch of a sentence: prose, or a word and how to say it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece<'a> {
    Plain(&'a str),
    Say { word: &'a str, sound: &'a str },
}

/// Each stretch of `text`, prose or mark, in order, with where it is in
/// bytes; and where a `{` at the end may yet open a mark, if one does. The
/// one reading of marks everything here shares.
fn scan(text: &str) -> (Vec<(Range<usize>, Piece<'_>)>, Option<usize>) {
    let mut out = Vec::new();
    let mut plain_from = 0;
    let mut at = 0;
    let mut unfinished = None;
    while let Some(open) = text[at..].find('{').map(|i| at + i) {
        match mark_at(&text[open..]) {
            Mark::Whole { len, word, sound } => {
                if open > plain_from {
                    out.push((plain_from..open, Piece::Plain(&text[plain_from..open])));
                }
                out.push((open..open + len, Piece::Say { word, sound }));
                at = open + len;
                plain_from = at;
            }
            Mark::Unfinished => {
                unfinished.get_or_insert(open);
                at = open + 1;
            }
            Mark::Not => at = open + 1,
        }
    }
    if plain_from < text.len() {
        out.push((plain_from..text.len(), Piece::Plain(&text[plain_from..])));
    }
    (out, unfinished)
}

/// `text` cut into prose and marks, in order. [`LINE_END`] is prose here:
/// it is the caller's to take off first.
pub fn pieces(text: &str) -> Vec<Piece<'_>> {
    scan(text).0.into_iter().map(|(_, piece)| piece).collect()
}

/// Where in `text` each mark is, in bytes from its `{` through its `}`.
pub fn marks(text: &str) -> Vec<Range<usize>> {
    scan(text)
        .0
        .into_iter()
        .filter(|(_, p)| matches!(p, Piece::Say { .. }))
        .map(|(r, _)| r)
        .collect()
}

/// Where a `{` in `text` begins that more text could still close into a
/// mark, if one does: what a splitter waits for before cutting after it.
pub fn unfinished_mark(text: &str) -> Option<usize> {
    scan(text).1
}

/// `text` as it is shown: each mark its word, and no [`LINE_END`].
pub fn shown(text: &str) -> String {
    render(text, |word, _| word.to_string())
}

/// `text` with every mark's respelling in place of its word, for a voice that
/// reads letters: see [`respelled`]. No [`LINE_END`].
pub fn as_read(text: &str) -> String {
    render(text, |_, sound| respelled(sound))
}

/// `text` with each mark as `mark` makes it, and no [`LINE_END`].
fn render(text: &str, mark: impl Fn(&str, &str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    for piece in pieces(text) {
        match piece {
            Piece::Plain(plain) => out.extend(plain.chars().filter(|&c| c != LINE_END)),
            Piece::Say { word, sound } => out.push_str(&mark(word, sound)),
        }
    }
    out
}

/// `text` with nothing in it that could open a mark: every brace a
/// bracket. For whatever a voice says that the assistant did not write as
/// its spoken reply -- see the module docs -- so that all of it is said, as
/// written.
pub fn inert(text: &str) -> String {
    if !text.contains(['{', '}']) {
        return text.to_string();
    }
    text.replace('{', "(").replace('}', ")")
}

/// A respelling as a voice that reads letters should be given it: lower case,
/// since a run of capitals is read as letters ("AWN" as "A W N"), with the
/// hyphens kept between syllables so each is said as spelled rather than
/// guessed at as one long word. The stress the capitals marked is lost, which
/// is the price of not having it spelled out.
pub fn respelled(sound: &str) -> String {
    sound
        .split_whitespace()
        .map(|word| {
            word.split('-').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("-").to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A respelling's words, each a list of its syllables, lower case, with
/// whether each is the stressed one: written in capitals, or -- when a word
/// of several syllables marks none -- its first.
pub fn syllables(sound: &str) -> Vec<Vec<(String, bool)>> {
    sound
        .split_whitespace()
        .map(|word| {
            let parts: Vec<&str> = word.split('-').filter(|s| !s.is_empty()).collect();
            let capital = |s: &str| {
                s.chars().any(char::is_alphabetic)
                    && s.chars().filter(|c| c.is_alphabetic()).all(char::is_uppercase)
            };
            let marked = parts.len() > 1 && parts.iter().any(|s| capital(s));
            parts
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let stressed = if marked { capital(s) } else { parts.len() > 1 && i == 0 };
                    (s.to_lowercase(), stressed)
                })
                .collect::<Vec<_>>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// What the text after a `{` amounts to.
enum Mark<'a> {
    /// A whole mark, `len` bytes from the `{` through the `}`.
    Whole { len: usize, word: &'a str, sound: &'a str },
    /// Nothing yet says it is not one: more text could close it.
    Unfinished,
    /// Not a mark: the `{` is only a brace.
    Not,
}

/// Whether `rest`, which starts with `{`, opens a mark. A word is up to
/// [`MAX_PRONOUNCED_CHARS`] of anything but braces, bars and line breaks, in
/// at most [`MAX_PRONOUNCED_WORDS`] words, with a letter or digit in it; a
/// respelling up to [`MAX_SAY_CHARS`] of letters, spaces, hyphens and
/// apostrophes, with a letter in it and no more words than the word's and
/// one more.
fn mark_at(rest: &str) -> Mark<'_> {
    debug_assert!(rest.starts_with('{'));
    let body = &rest[1..];
    // Where the bar is, once it has come; and how many characters the part
    // being read has so far.
    let mut bar = None;
    let mut count = 0;
    for (i, c) in body.char_indices() {
        match (bar, c) {
            (None, '|') => {
                let word = &body[..i];
                if !word.chars().any(char::is_alphanumeric)
                    || word.split_whitespace().count() > MAX_PRONOUNCED_WORDS
                {
                    return Mark::Not;
                }
                bar = Some(i);
                count = 0;
            }
            (None, '{' | '}' | '\n' | '\r' | LINE_END) => return Mark::Not,
            (None, _) => {
                count += 1;
                if count > MAX_PRONOUNCED_CHARS {
                    return Mark::Not;
                }
            }
            (Some(b), '}') => {
                let (word, sound) = (&body[..b], &body[b + 1..i]);
                let words = word.split_whitespace().count();
                if !sound.chars().any(char::is_alphabetic)
                    || sound.split_whitespace().count() > words + 1
                {
                    return Mark::Not;
                }
                return Mark::Whole { len: 1 + i + 1, word: word.trim(), sound: sound.trim() };
            }
            (Some(_), c) => {
                count += 1;
                let sounding = c.is_alphabetic() || matches!(c, ' ' | '-' | '\'' | '’');
                if !sounding || count > MAX_SAY_CHARS {
                    return Mark::Not;
                }
            }
        }
    }
    Mark::Unfinished
}

/// A reply as it streams, as it is shown: [`shown`] of what has arrived, a
/// piece at a time. A `{` that may yet open a mark is held back until it is
/// known; everything before it goes out as it comes.
#[derive(Debug, Default)]
pub struct Shown {
    raw: String,
    /// How much of `raw` has been dealt with, in bytes.
    settled: usize,
}

impl Shown {
    pub fn new() -> Self {
        Self::default()
    }

    /// Streamed text in; what of it may be shown now out.
    pub fn push(&mut self, delta: &str) -> String {
        self.raw.push_str(delta);
        self.drain(false)
    }

    /// The reply now reads `text` (a retried step): all of it that may be
    /// shown, to replace what was.
    pub fn replace(&mut self, text: &str) -> String {
        self.raw = text.to_string();
        self.settled = 0;
        self.drain(false)
    }

    /// The reply is over: whatever was held back, which is now only text.
    pub fn finish(&mut self) -> String {
        self.drain(true)
    }

    fn drain(&mut self, last: bool) -> String {
        let mut out = String::new();
        loop {
            let rest = &self.raw[self.settled..];
            let Some(open) = rest.find('{') else {
                out.push_str(rest);
                self.settled = self.raw.len();
                break;
            };
            out.push_str(&rest[..open]);
            self.settled += open;
            match mark_at(&self.raw[self.settled..]) {
                Mark::Whole { len, word, .. } => {
                    out.push_str(word);
                    self.settled += len;
                }
                Mark::Unfinished if !last => break,
                Mark::Unfinished | Mark::Not => {
                    out.push('{');
                    self.settled += 1;
                }
            }
        }
        out
    }
}

/// The person's pronunciations, ready to be found in a sentence.
#[derive(Debug, Clone, Default)]
pub struct Dictionary {
    /// Each word as lower-case characters, with how it is said; longest
    /// first, so "New York" is found before "York".
    entries: Vec<(Vec<char>, String)>,
}

impl Dictionary {
    pub fn new(pronunciations: &[Pronunciation]) -> Self {
        let mut entries: Vec<(Vec<char>, String)> = pronunciations
            .iter()
            .filter(|p| p.validate().is_ok())
            .map(|p| (lower(p.word.trim()), p.say.trim().to_string()))
            .collect();
        entries.sort_by_key(|(word, _)| std::cmp::Reverse(word.len()));
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `text` with every word the dictionary has, found whole and whatever
    /// its case, written as a mark. A word already in a mark is left alone:
    /// how the reply said to say it is the more particular.
    pub fn mark(&self, text: &str) -> String {
        if self.entries.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        for piece in pieces(text) {
            match piece {
                Piece::Plain(plain) => self.mark_plain(plain, &mut out),
                Piece::Say { word, sound } => {
                    out.push('{');
                    out.push_str(word);
                    out.push('|');
                    out.push_str(sound);
                    out.push('}');
                }
            }
        }
        out
    }

    fn mark_plain(&self, plain: &str, out: &mut String) {
        let chars: Vec<(usize, char)> = plain.char_indices().collect();
        let mut i = 0;
        while i < chars.len() {
            let starts_word = i == 0 || !chars[i - 1].1.is_alphanumeric();
            let found = starts_word
                .then(|| {
                    self.entries.iter().find_map(|(word, say)| {
                        let end = i + word.len();
                        let fits = end <= chars.len()
                            && chars[i..end].iter().zip(word).all(|(&(_, c), w)| lower_is(c, *w))
                            && chars.get(end).is_none_or(|(_, c)| !c.is_alphanumeric());
                        fits.then_some((end, say))
                    })
                })
                .flatten();
            match found {
                Some((end, say)) => {
                    let from = chars[i].0;
                    let to = chars.get(end).map_or(plain.len(), |(at, _)| *at);
                    out.push('{');
                    out.push_str(&plain[from..to]);
                    out.push('|');
                    out.push_str(say);
                    out.push('}');
                    i = end;
                }
                None => {
                    out.push(chars[i].1);
                    i += 1;
                }
            }
        }
    }
}

/// The person's pronunciations as every voice built from them reads them:
/// one list, shared, which a pronunciation saved while a voice is in use --
/// the person correcting the assistant mid-conversation -- changes for that
/// voice's next sentence, not the next conversation's. See
/// [`Service::pronunciations`](crate::service::Service::pronunciations).
#[derive(Debug, Clone, Default)]
pub struct Book(Arc<RwLock<Dictionary>>);

impl Book {
    pub fn new(pronunciations: &[Pronunciation]) -> Self {
        Self(Arc::new(RwLock::new(Dictionary::new(pronunciations))))
    }

    /// From now on, these.
    pub fn set(&self, pronunciations: &[Pronunciation]) {
        let dictionary = Dictionary::new(pronunciations);
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = dictionary;
    }

    /// [`Dictionary::mark`] with the list as it is now.
    pub fn mark(&self, text: &str) -> String {
        self.0.read().unwrap_or_else(|e| e.into_inner()).mark(text)
    }
}

/// `word` as lower-case characters, one for one: a character whose lower
/// case is more than one (rare, and never in a name that needs saying
/// differently) is kept as it is.
fn lower(word: &str) -> Vec<char> {
    word.chars().map(lower_char).collect()
}

fn lower_char(c: char) -> char {
    let mut lowered = c.to_lowercase();
    match (lowered.next(), lowered.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

fn lower_is(c: char, lowered: char) -> bool {
    lower_char(c) == lowered
}

/// The pause after a full stop, an exclamation or a question within a
/// paragraph: about what a person leaves between two sentences when talking.
const AFTER_SENTENCE: Duration = Duration::from_millis(300);
/// After a question, a little longer: it hangs.
const AFTER_QUESTION: Duration = Duration::from_millis(380);
/// After an ellipsis: a beat, which is what it was written for.
const AFTER_ELLIPSIS: Duration = Duration::from_millis(480);
/// After a line: an item of a list, the end of a paragraph, a heading. The
/// pause that lets a list be heard as one.
const AFTER_LINE: Duration = Duration::from_millis(550);
/// After a colon or semicolon: something more is coming.
const AFTER_COLON: Duration = Duration::from_millis(280);
/// After a sentence too long to say in one go, cut at a comma or a dash.
const AFTER_CLAUSE: Duration = Duration::from_millis(160);
/// After a sentence cut where there was no mark at all: barely a breath, so
/// the two halves still sound like one sentence.
const AFTER_CUT: Duration = Duration::from_millis(60);

/// How long to leave after saying `text`, at the voice's own pace, from how
/// it ends. Closing quotes and brackets are looked past.
pub fn pause_after(text: &str) -> Duration {
    pause_after_shown(&shown(text), text.trim_end().ends_with(LINE_END))
}

/// [`pause_after`] for a sentence already [`shown`], with whether it ended a
/// line: for a caller that has both to hand.
pub fn pause_after_shown(shown: &str, ended_line: bool) -> Duration {
    if ended_line {
        return AFTER_LINE;
    }
    let end = shown.trim_end().trim_end_matches(['"', '\'', '”', '’', ')', ']', '}', '*', '_']);
    if end.ends_with("...") || end.ends_with('…') {
        return AFTER_ELLIPSIS;
    }
    match end.chars().last() {
        Some('?' | '？') => AFTER_QUESTION,
        Some('.' | '!' | '。' | '！') => AFTER_SENTENCE,
        Some(':' | ';') => AFTER_COLON,
        Some(',' | '—' | '–' | '-') => AFTER_CLAUSE,
        _ => AFTER_CUT,
    }
}

/// `text` with a dash between two numbers -- an em or en dash, or a hyphen
/// standing alone between spaces: "2–3 days", "10 - 15 minutes" -- said as
/// "to", which is what it means, and which no voice here says by itself:
/// Phonon reads the dash as a space and Kokoro as a pause. A hyphen joining
/// two numbers with no space ("2026-11-21", a phone number) is left alone.
pub fn ranges_said(text: &str) -> String {
    rewrite_dashes(text, None)
}

/// `text` with [`ranges_said`], and every other em or en dash, and a hyphen
/// standing alone between spaces, made a comma: for Phonon, which reads a
/// dash as a space and so runs straight on where a speaker pauses -- a comma
/// is how it is asked to pause. A hyphen inside a word ("well-known") stays.
pub fn dashes_as_commas(text: &str) -> String {
    rewrite_dashes(
        text,
        Some(&|out: &mut String| {
            // No comma straight after another mark, or at the very start.
            let before = out.trim_end();
            let marked = before.ends_with([',', '.', '!', '?', ';', ':']) || before.is_empty();
            out.truncate(before.len());
            if !marked {
                out.push(',');
            }
            out.push(' ');
        }),
    )
}

/// The one walk over `text`'s dashes both of the above share: one between
/// two numbers becomes " to "; any other is handed to `other`, when there is
/// one, and is otherwise kept.
fn rewrite_dashes(text: &str, other: Option<&dyn Fn(&mut String)>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let spaced_hyphen = c == '-'
            && i > 0
            && chars[i - 1].is_whitespace()
            && chars.get(i + 1).is_some_and(|n| n.is_whitespace());
        let dash = matches!(c, '—' | '–') || spaced_hyphen;
        // The nearest characters either side that are not spaces.
        let before = chars[..i].iter().rev().find(|c| !c.is_whitespace());
        let after = chars[i + 1..].iter().find(|c| !c.is_whitespace());
        let between_numbers = dash
            && before.is_some_and(char::is_ascii_digit)
            && after.is_some_and(char::is_ascii_digit);
        let rewrite: Option<&dyn Fn(&mut String)> = match (between_numbers, dash) {
            (true, _) => Some(&|out: &mut String| {
                let kept = out.trim_end().len();
                out.truncate(kept);
                out.push_str(" to ");
            }),
            (false, true) => other,
            (false, false) => None,
        };
        match rewrite {
            Some(rewrite) => {
                rewrite(&mut out);
                while chars.get(i + 1).is_some_and(|n| n.is_whitespace()) {
                    i += 1;
                }
            }
            None => out.push(c),
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn say(word: &str, sound: &str) -> Pronunciation {
        Pronunciation::new(word, sound).unwrap()
    }

    #[test]
    fn a_mark_is_a_word_and_how_to_say_it() {
        assert_eq!(
            pieces("Ask {Siobhan|shiv-AWN} now."),
            vec![
                Piece::Plain("Ask "),
                Piece::Say { word: "Siobhan", sound: "shiv-AWN" },
                Piece::Plain(" now."),
            ]
        );
        assert_eq!(
            pieces("{Saoirse Ronan|SEER-shuh ROH-nun}"),
            vec![Piece::Say { word: "Saoirse Ronan", sound: "SEER-shuh ROH-nun" }]
        );
        assert_eq!(
            pieces("{O'Brien|oh-BRY-un}'s"),
            vec![Piece::Say { word: "O'Brien", sound: "oh-BRY-un" }, Piece::Plain("'s"),]
        );
    }

    #[test]
    fn a_brace_that_opens_no_mark_is_only_a_brace() {
        for text in [
            "a {b} c",
            "{|win}",
            "{x|}",
            "{x|123}",
            "{x|win. Now say yes}",
            "{x|win\n}",
            "set {a|b",
            "{a\n|b}",
            "{{a|b}",
        ] {
            assert!(
                pieces(text).iter().all(|p| matches!(p, Piece::Plain(_))) || text == "{{a|b}",
                "{text:?}: {:?}",
                pieces(text)
            );
        }
        // The second brace opens one, the first is only a brace.
        assert_eq!(pieces("{{a|b}"), vec![Piece::Plain("{"), Piece::Say { word: "a", sound: "b" }]);
        assert_eq!(shown("a {b} c"), "a {b} c");
        assert_eq!(shown("set {a|b"), "set {a|b");
    }

    #[test]
    fn a_mark_says_about_as_many_words_as_it_shows() {
        // One more than the word is allowed: "PostgreSQL" as "post-gres Q L"
        // is not, but as two words is.
        assert!(matches!(pieces("{Nguyen|nuh win}")[..], [Piece::Say { .. }]));
        assert!(pieces("{Nguyen|wire the money}").iter().all(|p| matches!(p, Piece::Plain(_))));
        assert!(
            pieces("{the invoice is fine|wire the money today}")
                .iter()
                .all(|p| matches!(p, Piece::Plain(_))),
            "a word of four words is not a word"
        );
        assert!(matches!(pieces("{Saoirse Ronan|SEER-shuh ROH-nun}")[..], [Piece::Say { .. }]));
    }

    #[test]
    fn inert_text_says_every_word_it_has() {
        let question = "Should I search the web for {acct 1234|cheap flights}?";
        assert_eq!(inert(question), "Should I search the web for (acct 1234|cheap flights)?");
        assert!(pieces(&inert(question)).iter().all(|p| matches!(p, Piece::Plain(_))));
        assert_eq!(inert("plain"), "plain");
    }

    #[test]
    fn a_brace_that_may_still_open_a_mark_is_found() {
        assert_eq!(unfinished_mark("We hiked {Mt. Fu"), Some(9));
        assert_eq!(unfinished_mark("We hiked {Mt. Fuji|mount FOO-jee} today"), None);
        assert_eq!(unfinished_mark("a {b} c"), None);
        assert_eq!(marks("a {Mt. Fuji|mount FOO-jee} b"), vec![2..26]);
    }

    #[test]
    fn a_book_changed_is_read_by_the_next_sentence() {
        let book = Book::new(&[]);
        let voice = book.clone();
        assert_eq!(voice.mark("Ask Siobhan."), "Ask Siobhan.");
        book.set(&[say("Siobhan", "shiv-AWN")]);
        assert_eq!(voice.mark("Ask Siobhan."), "Ask {Siobhan|shiv-AWN}.");
    }

    #[test]
    fn what_is_shown_is_the_word_without_the_line_end() {
        assert_eq!(shown("Ask {Siobhan|shiv-AWN} now.¶"), "Ask Siobhan now.");
        assert_eq!(shown("plain"), "plain");
        assert_eq!(as_read("Ask {Siobhan|shiv-AWN} now.¶"), "Ask shiv-awn now.");
    }

    #[test]
    fn a_respelling_is_read_in_lower_case_syllables() {
        assert_eq!(respelled("shiv-AWN"), "shiv-awn");
        assert_eq!(respelled(" SEER-shuh  ROH-nun "), "seer-shuh roh-nun");
        assert_eq!(respelled("win"), "win");
        assert_eq!(respelled("a--b"), "a-b");
    }

    #[test]
    fn the_stressed_syllable_is_the_capital_one_or_the_first() {
        assert_eq!(syllables("shiv-AWN"), vec![vec![("shiv".into(), false), ("awn".into(), true)]]);
        assert_eq!(syllables("win"), vec![vec![("win".into(), false)]]);
        assert_eq!(syllables("keen-wah"), vec![vec![("keen".into(), true), ("wah".into(), false)]]);
        assert_eq!(
            syllables("SEER-shuh ROH-nun"),
            vec![
                vec![("seer".into(), true), ("shuh".into(), false)],
                vec![("roh".into(), true), ("nun".into(), false)],
            ]
        );
        // One capital word alone marks nothing: it is only loud.
        assert_eq!(syllables("WIN"), vec![vec![("win".into(), false)]]);
    }

    #[test]
    fn the_screen_is_shown_the_word_as_the_reply_streams() {
        let mut s = Shown::new();
        let mut out = String::new();
        for delta in ["Ask {Sio", "bhan|shi", "v-AWN} n", "ow, {not a", " mark} ok", "."] {
            out.push_str(&s.push(delta));
        }
        out.push_str(&s.finish());
        assert_eq!(out, "Ask Siobhan now, {not a mark} ok.");

        // Held back only while it could still be one.
        let mut s = Shown::new();
        assert_eq!(s.push("Hi {Sio"), "Hi ");
        assert_eq!(s.push("bhan"), "");
        assert_eq!(s.push("\nthere"), "{Siobhan\nthere");

        // Never closed: shown as written when the reply ends.
        let mut s = Shown::new();
        assert_eq!(s.push("Odd {x|y"), "Odd ");
        assert_eq!(s.finish(), "{x|y");

        // A retried step replaces the lot.
        let mut s = Shown::new();
        s.push("Ask {Siobhan|shiv-AWN} and {Ng");
        assert_eq!(s.replace("Ask {Siobhan|shiv-AWN} and"), "Ask Siobhan and");
        assert_eq!(s.push(" {Nguyen|win}."), " Nguyen.");
    }

    #[test]
    fn the_dictionary_marks_whole_words_whatever_their_case() {
        let d = Dictionary::new(&[
            say("Nguyen", "win"),
            say("New York", "noo YORK"),
            say("York", "yawk"),
        ]);
        assert_eq!(
            d.mark("Mr nguyen flew from New York to York, not Nguyenville."),
            "Mr {nguyen|win} flew from {New York|noo YORK} to {York|yawk}, not Nguyenville."
        );
        // What the reply marked itself is left as it said.
        assert_eq!(d.mark("{Nguyen|nwin} and Nguyen"), "{Nguyen|nwin} and {Nguyen|win}");
        assert_eq!(Dictionary::new(&[]).mark("Nguyen"), "Nguyen");
        assert_eq!(d.mark("Ünïcode ok"), "Ünïcode ok");
    }

    #[test]
    fn the_pause_after_a_sentence_follows_how_it_ends() {
        assert_eq!(pause_after("Done."), AFTER_SENTENCE);
        assert_eq!(pause_after("Done!"), AFTER_SENTENCE);
        assert_eq!(pause_after("Is it?"), AFTER_QUESTION);
        assert_eq!(pause_after("He said “yes.”"), AFTER_SENTENCE);
        assert_eq!(pause_after("Well..."), AFTER_ELLIPSIS);
        assert_eq!(pause_after("Well…"), AFTER_ELLIPSIS);
        assert_eq!(pause_after("Buy milk¶"), AFTER_LINE);
        assert_eq!(pause_after("First, the dentist.¶"), AFTER_LINE);
        assert_eq!(pause_after("Here they are:"), AFTER_COLON);
        assert_eq!(pause_after("and then,"), AFTER_CLAUSE);
        assert_eq!(pause_after("and then the"), AFTER_CUT);
        assert_eq!(pause_after("See {Siobhan|shiv-AWN}"), AFTER_CUT);
        assert!(AFTER_CUT < AFTER_CLAUSE && AFTER_CLAUSE < AFTER_SENTENCE);
        assert!(AFTER_SENTENCE < AFTER_LINE);
    }

    #[test]
    fn a_dash_is_a_comma_for_a_voice_that_skips_dashes() {
        assert_eq!(dashes_as_commas("two — and in the big room"), "two, and in the big room");
        assert_eq!(dashes_as_commas("two—and"), "two, and");
        assert_eq!(dashes_as_commas("so - here"), "so, here");
        assert_eq!(dashes_as_commas("well-known"), "well-known");
        assert_eq!(dashes_as_commas("Wait, — what"), "Wait, what");
        assert_eq!(dashes_as_commas("— hello"), " hello");
        assert_eq!(dashes_as_commas("2–3 days, 9—5"), "2 to 3 days, 9 to 5");
        assert_eq!(
            dashes_as_commas("10 - 15 minutes, 2 – 3 days"),
            "10 to 15 minutes, 2 to 3 days"
        );
        // A range alone, for a voice that keeps its dashes.
        assert_eq!(ranges_said("10 - 15 minutes — maybe less"), "10 to 15 minutes — maybe less");
        assert_eq!(ranges_said("on 2026-11-21, call 555-0100"), "on 2026-11-21, call 555-0100");
    }
}
