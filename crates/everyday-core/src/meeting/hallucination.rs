//! Lines a recogniser makes up when there is no speech to read.
//!
//! A speech recogniser -- Whisper above all -- is a language model listening
//! to audio, and when the audio holds nothing to read it still writes
//! something. Over silence, breathing, a keyboard or a faint room it reads
//! its own prompt back as speech ("Weekly sync. With Ana Ruiz"), writes the
//! stock lines of the video subtitles it learned from ("Thank you.",
//! "Thanks for watching!", an Amara.org credit), writes noise tags
//! ("[Music]", "(applause)", "♪"), puts names from its prompt in front of
//! real speech as if they were a speaker label ("Ana Ruiz: so let me just
//! get"), or gets stuck and writes one phrase over and over. None of it was
//! said, and every line of it would reach the notes model as if it had been.
//!
//! [`clean_line`] is the filter, one line at a time. Every rule is narrow,
//! because a dropped line is lost for good while a stray one only costs the
//! notes model a moment's reading:
//!
//! - a stock line goes only when the audio under it held almost no clearly
//!   loud sound ([`clear_speech_ms`] under [`MIN_CLEAR_MS`]) -- people do say
//!   "thank you", and one said in a loud clip is kept;
//! - a prompt read-back must be two or more of the prompt's words in its
//!   order with nothing of its own -- a single name said for real is kept, and
//!   so is a sentence that uses one;
//! - a speaker label comes off only when it is the prompt's words in order; a
//!   colon after words of the line's own ("Note: ...") stays;
//! - a noise tag must be the whole line, or the line must hold no words at
//!   all;
//! - a loop is cut back to one saying of its phrase rather than dropped, since
//!   the first saying is often real, and twice in a row is left alone ("very
//!   very good" is speech);
//! - Whisper's own confidence counts only when it says both that there was
//!   probably no speech and that it was unsure of the words.
//!
//! The approach, the stock lines and the loudness thresholds follow Yogurt
//! (github.com/FrigadeHQ/yogurt, MIT). Cutting loops back is ours.

use super::SAMPLE_RATE;
use super::words::{is_unspaced, tokens};
use std::collections::HashSet;

/// One line as a recogniser returned it, with the confidence figures a
/// Whisper-style `verbose_json` carries (absent from every other backend).
#[derive(Debug, Clone, Copy)]
pub struct Heard<'a> {
    pub text: &'a str,
    /// Whisper's probability that the window held no speech at all.
    pub no_speech_prob: Option<f32>,
    /// The mean log probability of the line's tokens: how sure Whisper was
    /// of the words it wrote.
    pub avg_logprob: Option<f32>,
}

/// Over this probability of no speech, together with an average log
/// probability under [`UNSURE_LOGPROB`], Whisper is saying it heard nothing
/// and guessed. These are Whisper's own defaults for skipping a window as
/// silent. Either figure alone is too noisy to act on: a short word in a
/// mostly quiet window can score a high no-speech probability, and a name or
/// an accent can pull the log probability down on words that were said.
const NO_SPEECH_PROB: f32 = 0.6;

/// See [`NO_SPEECH_PROB`].
const UNSURE_LOGPROB: f32 = -1.0;

/// The line cleaned, or `None` when it should be dropped. `prompt` is what
/// the recogniser was told (see `super::vocab::transcriber_prompt`); `faint`
/// says the audio it came from held less than [`MIN_CLEAR_MS`] of clear
/// speech (see [`clear_speech_ms`]).
///
/// In order: the spacing is tidied; names from the prompt used as a speaker
/// label come off ([`without_prompt_label`]); an empty line or a noise tag
/// goes ([`is_noise`]), and so does a line Whisper itself says was probably
/// no speech and unsure words; then the prompt read back
/// ([`is_prompt_echo`]; under [`LONG_ECHO_TOKENS`] words only over faint
/// audio, since a short line of prompt words is often said for real) and,
/// over faint audio, a stock line
/// ([`is_silence_guess`]). A loop is cut back to one saying
/// ([`collapse_repeats`]) and those two checks run again on what is left,
/// since "Thank you. Thank you. Thank you." is a stock line once it is said
/// once. Last, a line that is still the same few words over and over goes
/// ([`is_garbled`]). An empty prompt never drops anything.
pub fn clean_line(heard: Heard<'_>, prompt: &str, faint: bool) -> Option<String> {
    let said = heard.text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = without_prompt_label(&said, prompt);
    if text.is_empty() || is_noise(text) {
        return None;
    }
    if let (Some(no_speech), Some(logprob)) = (heard.no_speech_prob, heard.avg_logprob)
        && no_speech > NO_SPEECH_PROB
        && logprob < UNSURE_LOGPROB
    {
        return None;
    }
    // A short read-back goes only over faint audio: "Nick and Jeremy?" said
    // out loud is the prompt's words too. Judged by the length of the line
    // as it was said, so a name looped three times still counts as long
    // once it is cut back to one saying.
    let long = tokens(text).len() >= LONG_ECHO_TOKENS;
    let made_up = |line: &str| {
        (is_prompt_echo(line, prompt) && (faint || long)) || (faint && is_silence_guess(line))
    };
    if made_up(text) {
        return None;
    }
    let collapsed = collapse_repeats(text);
    if (collapsed != text && made_up(collapsed.as_str())) || is_garbled(&collapsed) {
        return None;
    }
    Some(collapsed)
}

/// A stretch of the prompt this many words long, and nothing else, is the
/// prompt read back. One word is not enough: a single name is said for real
/// all the time.
const MIN_ECHO_TOKENS: usize = 2;

/// A read-back at least this many words long may carry one stray word of its
/// own: Whisper sometimes starts one with a "to" or an "and". Shorter, a
/// word of its own makes the line a real sentence that uses a name ("Thanks,
/// Sam Lee.").
const LONG_ECHO_TOKENS: usize = 4;

/// Whisper cuts a read-back off mid-word ("... Sam Lee of Northw."): the
/// line's last word counts as the prompt's when it is the start of it and at
/// least this many characters long, so a stray "a" or "of" cannot stand in
/// for every word that starts with it.
const MIN_CUT_CHARS: usize = 3;

/// Whether `text` is `prompt` read back: two or more of the prompt's words in
/// its order and nothing else -- the last one possibly cut off mid-word, and
/// one stray word allowed in a read-back of four or more. Never true for an
/// empty prompt.
pub fn is_prompt_echo(text: &str, prompt: &str) -> bool {
    let t = tokens(text);
    let p = tokens(prompt);
    if t.len() < MIN_ECHO_TOKENS || t.len() > p.len() + 1 {
        return false;
    }
    let last = t.len() - 1;
    let same = |k: usize, word: &str| {
        word == t[k]
            || (k == last
                && t[k].chars().count() >= MIN_CUT_CHARS
                && word.starts_with(t[k].as_str()))
    };
    // The longest stretch of the line that reads the prompt word for word.
    let mut run = 0;
    for a in 0..t.len() {
        for i in 0..p.len() {
            let mut n = 0;
            while a + n < t.len() && i + n < p.len() && same(a + n, p[i + n].as_str()) {
                n += 1;
            }
            run = run.max(n);
        }
    }
    run == t.len() || (run >= LONG_ECHO_TOKENS && t.len() - run <= 1)
}

/// The longest a speaker label can be, in characters. A colon further into
/// the line than this belongs to a sentence.
const MAX_LABEL_CHARS: usize = 120;

/// `text` without names from `prompt` that Whisper put in front of real
/// speech as a speaker label ("Eric and Sam Lee: so let me just get" → "so
/// let me just get"). The label is everything before the line's first colon,
/// and comes off only when its words are a stretch of the prompt's, in order,
/// and speech follows the colon after a space. A colon after words of the
/// line's own ("Note: the board is late"), a time ("10:30"), or a label with
/// nothing after it is left alone.
pub fn without_prompt_label<'a>(text: &'a str, prompt: &str) -> &'a str {
    let Some((label, after)) = text.split_once(':') else {
        return text;
    };
    if label.is_empty()
        || label.chars().count() > MAX_LABEL_CHARS
        || label.contains('\n')
        || !after.starts_with(char::is_whitespace)
    {
        return text;
    }
    let speech = after.trim_start();
    let (label, p) = (tokens(label), tokens(prompt));
    if speech.is_empty() || label.is_empty() || p.is_empty() {
        return text;
    }
    let in_prompt =
        (0..p.len()).any(|i| label.iter().enumerate().all(|(k, w)| p.get(i + k) == Some(w)));
    if in_prompt { speech } else { text }
}

/// Whisper's stock lines over quiet audio, learned from video subtitles:
/// sign-offs, pleas to subscribe and subtitlers' credits, in the languages it
/// most often writes them in. Compared as whole lines, word for word (see
/// [`super::words::tokens`]), so "Thank you for the update" is never one.
const GUESSES: &[&str] = &[
    "you",
    "thank you",
    "thank you very much",
    "thanks",
    "thanks for watching",
    "thank you for watching",
    "thanks for watching bye",
    "thank you bye",
    "bye",
    "bye bye",
    "please subscribe",
    "like and subscribe",
    "for more information visit www fema org",
    "subtitles by the amara org community",
    "untertitel der amara org community",
    "sous titres réalisés para la communauté d amara org",
    "sous titres réalisés par la communauté d amara org",
    "ご視聴ありがとうございました",
    "продолжение следует",
];

/// Whether `text` is, as a whole line, one of Whisper's stock lines for
/// silence (`GUESSES`). Only grounds to drop a line over faint audio:
/// people do say thank you.
pub fn is_silence_guess(text: &str) -> bool {
    let line = tokens(text).join(" ");
    !line.is_empty() && GUESSES.iter().any(|guess| tokens(guess).join(" ") == line)
}

/// Whether `text` is noise rather than speech: a line with no words in it at
/// all ("...", "♪", "-"), or one made only of bracketed tags -- `[Music]`,
/// `(applause)`, `*sighs*`, `♪ lyrics ♪` -- with nothing but spaces, dots
/// and notes between them. A tag inside real speech ("(laughs) Yeah, that's
/// right") leaves the line alone.
pub fn is_noise(text: &str) -> bool {
    let mut rest = text.trim();
    if tokens(rest).is_empty() {
        return true;
    }
    while let Some(c) = rest.chars().next() {
        let after = &rest[c.len_utf8()..];
        let next = match tag_end(c, after) {
            Some(end) => &after[end..],
            None if matches!(c, '♪' | '.' | '…') => after,
            None => return false,
        };
        rest = next.trim_start();
    }
    true
}

/// Where the tag `open` starts closes in `after` (the text just past it), as
/// the byte offset just past its closing mark; `None` when `open` does not
/// start a tag or the tag never closes. Brackets nest; notes and asterisks
/// close at the next one.
fn tag_end(open: char, after: &str) -> Option<usize> {
    let close = match open {
        '[' => ']',
        '(' => ')',
        '♪' => '♪',
        '*' => '*',
        _ => return None,
    };
    let mut depth = 0usize;
    for (i, c) in after.char_indices() {
        if c == close {
            if depth == 0 {
                return Some(i + c.len_utf8());
            }
            depth -= 1;
        } else if c == open {
            depth += 1;
        }
    }
    None
}

/// The longest phrase, in words, that [`collapse_repeats`] looks for. A
/// recogniser's loops are short -- a word, a sign-off, half a sentence -- and
/// a longer phrase said three times running is rarer as a loop than as a
/// speaker making a point.
const MAX_LOOP_WORDS: usize = 8;

/// How many times running a phrase must come to be a loop. Twice is ordinary
/// speech ("very very good", "no, no"); three times or more is far likelier
/// the recogniser stuck, and cutting a real one back to a single saying loses
/// only the emphasis.
const MIN_LOOP_REPEATS: usize = 3;

/// `text` with every loop -- the same phrase of 1 to `MAX_LOOP_WORDS` (8)
/// words said `MIN_LOOP_REPEATS` (3) or more times back to back -- cut back
/// to one saying, keeping everything before and after it. Phrases are
/// compared by their words (see [`super::words::tokens`]), but the line is
/// rebuilt from its own words, so case and punctuation survive: the first
/// saying is kept, except that it ends with the last saying's punctuation, so
/// it joins what follows the way the loop did ("So I think, I think, I think
/// we should" → "So I think we should"). A line with no loop comes back as it
/// was.
pub fn collapse_repeats(text: &str) -> String {
    collapse_unspaced(&collapse_spaced(text))
}

/// The longest phrase, in characters, [`collapse_unspaced`] looks for a loop
/// of: a long sentence of Japanese or Chinese, punctuation included.
const MAX_LOOP_CHARS: usize = 40;

/// Loops in a script written without spaces (Japanese, Chinese), where
/// there are no words to split on: the same run of two or more characters,
/// holding at least one of that script's, said [`MIN_LOOP_REPEATS`] times
/// or more back to back, cut back to one saying
/// ("ありがとうございました。ありがとうございました。ありがとうございました。" →
/// "ありがとうございました。"). A single repeated character ("ははは") is
/// laughter, and is left alone.
fn collapse_unspaced(text: &str) -> String {
    if !text.chars().any(is_unspaced) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let mut best: Option<(usize, usize)> = None;
        for n in 2..=MAX_LOOP_CHARS {
            if i + n * MIN_LOOP_REPEATS > chars.len() {
                break;
            }
            let phrase = &chars[i..i + n];
            if !phrase.iter().copied().any(is_unspaced) {
                continue;
            }
            let mut repeats = 1;
            while chars.get(i + repeats * n..i + (repeats + 1) * n) == Some(phrase) {
                repeats += 1;
            }
            if repeats >= MIN_LOOP_REPEATS && best.is_none_or(|(bn, br)| n * repeats > bn * br) {
                best = Some((n, repeats));
            }
        }
        match best {
            Some((n, repeats)) => {
                out.extend(&chars[i..i + n]);
                i += n * repeats;
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

/// Loops of whole words, in a script written with spaces -- see
/// [`collapse_repeats`].
fn collapse_spaced(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let keys: Vec<String> = words.iter().map(|w| tokens(w).join(" ")).collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut changed = false;
    let mut i = 0;
    while i < words.len() {
        let Some((n, repeats)) = loop_at(&keys, i) else {
            out.push(words[i].to_string());
            i += 1;
            continue;
        };
        out.extend(words[i..i + n - 1].iter().map(|w| w.to_string()));
        let (stem, _) = split_ending(words[i + n - 1]);
        let (_, ending) = split_ending(words[i + n * repeats - 1]);
        out.push(format!("{stem}{ending}"));
        i += n * repeats;
        changed = true;
    }
    if changed { out.join(" ") } else { text.to_string() }
}

/// The loop that starts at word `i` of `keys`, as (phrase length in words,
/// times said), if one does: of the phrases that repeat often enough, the one
/// covering the most words, the shortest of equals ("sorry sorry sorry sorry"
/// is "sorry" four times, not "sorry sorry" twice). A phrase must hold at
/// least one word, not only punctuation.
fn loop_at(keys: &[String], i: usize) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for n in 1..=MAX_LOOP_WORDS {
        if i + n * MIN_LOOP_REPEATS > keys.len() {
            break;
        }
        let phrase = &keys[i..i + n];
        if phrase.iter().all(|k| k.is_empty()) {
            continue;
        }
        let mut repeats = 1;
        while keys.get(i + repeats * n..i + (repeats + 1) * n) == Some(phrase) {
            repeats += 1;
        }
        if repeats >= MIN_LOOP_REPEATS && best.is_none_or(|(bn, br)| n * repeats > bn * br) {
            best = Some((n, repeats));
        }
    }
    best
}

/// `word` split before the punctuation it ends with: "sorry." → ("sorry",
/// ".").
fn split_ending(word: &str) -> (&str, &str) {
    word.split_at(word.trim_end_matches(|c: char| !c.is_alphanumeric()).len())
}

/// A line this many words long or longer can be judged garbled; shorter ones
/// ("ha ha ha", "yes yes") are too short to tell.
const GARBLED_MIN_TOKENS: usize = 8;

/// A line is garbled when each distinct word in it comes back this many times
/// or more on average -- a quarter as many distinct words as words. Real
/// speech of eight words or more almost never repeats itself that much; a
/// recogniser lost in the wrong language does.
const GARBLED_WORDS_PER_DISTINCT: usize = 4;

/// Whether `text` is empty or the same few words over and over: the
/// recogniser lost its way, usually because the language was wrong.
pub fn is_garbled(text: &str) -> bool {
    if text.trim().is_empty() {
        return true;
    }
    let words = tokens(text);
    let distinct: HashSet<&str> = words.iter().map(String::as_str).collect();
    words.len() >= GARBLED_MIN_TOKENS && distinct.len() * GARBLED_WORDS_PER_DISTINCT <= words.len()
}

/// Under this much clearly loud audio, a request held a faint sound, not
/// somebody talking.
pub const MIN_CLEAR_MS: u64 = 300;

/// Loudness is measured in windows this long: long enough to hold a
/// syllable, short enough that a pause between words shows as a quiet one.
const WINDOW_MS: u64 = 100;

/// Samples in one window, at [`SAMPLE_RATE`].
const WINDOW_SAMPLES: usize = SAMPLE_RATE as usize * WINDOW_MS as usize / 1000;

/// The noise floor is the window this many percent of the way up from the
/// quietest: the level a clip's quietest tenth stays under. Not the very
/// quietest window, which one dropout can pull to nothing.
const FLOOR_PERCENTILE: usize = 10;

/// Clear speech is a window louder than [`CLEAR_FACTOR`] times a level of
/// this many times the noise floor, that level held between
/// [`MIN_THRESHOLD`] and [`MAX_THRESHOLD`]. A ratio rather than a fixed level,
/// so that a quiet headset and a noisy room both work. Yogurt measured it on
/// a solo test note: Whisper wrote "Thank you." over faint sounds whose
/// loudest windows stayed under it (-51 dB and below), while the quietest
/// real sentence had several windows over it (-43 dB).
const LOUD_FACTOR: f32 = 3.0;

/// See [`LOUD_FACTOR`].
const CLEAR_FACTOR: f32 = 3.0;

/// The least the level under [`LOUD_FACTOR`] may be (about -56 dB): over a
/// digitally silent floor, any sound at all would otherwise count as clear.
const MIN_THRESHOLD: f32 = 0.0015;

/// The most the level under [`LOUD_FACTOR`] may be (about -34 dB): where the
/// floor is itself speech, three times it would leave only shouting clear.
const MAX_THRESHOLD: f32 = 0.02;

/// A clip's noise floor: the RMS (on the -1.0..=1.0 scale, an `i16` sample
/// divided by 32768) of the 100 ms window `FLOOR_PERCENTILE` (10) percent of
/// the way up from the quietest -- the level its quietest tenth of windows
/// stays under. 0.0 for a clip too short to hold half a window.
pub fn noise_floor(samples: &[i16]) -> f32 {
    let mut levels = window_levels(samples);
    if levels.is_empty() {
        return 0.0;
    }
    let rank = levels.len() * FLOOR_PERCENTILE / 100;
    *levels.select_nth_unstable_by(rank, f32::total_cmp).1
}

/// How much of `samples` is clearly speech, in milliseconds: 100 ms windows
/// well over `floor` (see [`noise_floor`]): louder than three times a level
/// of three times the floor, that level held between about -56 dB and -34 dB.
///
/// `floor` is best measured over more than the clip itself -- a track's last
/// minute, as Yogurt does -- since in a clip that is speech from end to end
/// the quietest tenth is speech too; `MAX_THRESHOLD` bounds the harm when it
/// is not.
pub fn clear_speech_ms(samples: &[i16], floor: f32) -> u64 {
    let clear = (floor * LOUD_FACTOR).clamp(MIN_THRESHOLD, MAX_THRESHOLD) * CLEAR_FACTOR;
    let windows = window_levels(samples).into_iter().filter(|&level| level > clear).count();
    windows as u64 * WINDOW_MS
}

/// The loudness of each window of `samples`, as RMS on the -1.0..=1.0 scale.
/// A trailing part-window counts when it is at least half full; less is too
/// little sound to judge.
fn window_levels(samples: &[i16]) -> Vec<f32> {
    samples
        .chunks(WINDOW_SAMPLES)
        .filter(|window| window.len() * 2 >= WINDOW_SAMPLES)
        .map(|window| {
            let sum: f64 = window.iter().map(|&s| (f64::from(s) / 32768.0).powi(2)).sum();
            (sum / window.len() as f64).sqrt() as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heard(text: &str) -> Heard<'_> {
        Heard { text, no_speech_prob: None, avg_logprob: None }
    }

    const NORTHWIND: &str = "Northwind sync. With Sam Lee of Northwind and Joe Park of Acme.";
    const STANDUP: &str = "Standup. With Ana, Maya Lopez, Omar and Sam Lee of Northwind.";
    const BOARD: &str = "Product board. With Eric and Christian Mathiesen.";

    #[test]
    fn the_prompt_or_a_stretch_of_it_is_an_echo() {
        assert!(is_prompt_echo("Sam Lee of Northwind.", "With Sam Lee of Northwind."));
        assert!(is_prompt_echo("With Sam Lee of Northwind.", "With Sam Lee of Northwind."));
        assert!(is_prompt_echo("Northwind sync.", NORTHWIND));
        assert!(is_prompt_echo("joe park of acme", NORTHWIND));
    }

    #[test]
    fn a_line_with_words_of_its_own_or_a_single_word_is_not_an_echo() {
        assert!(!is_prompt_echo("I work at Acme with Sam.", NORTHWIND));
        assert!(!is_prompt_echo("Sam Lee of Northwind, nice to meet you.", NORTHWIND));
        assert!(!is_prompt_echo("Zentrik.", NORTHWIND));
        assert!(!is_prompt_echo("Omar.", STANDUP));
    }

    #[test]
    fn nothing_is_an_echo_of_an_empty_prompt() {
        assert!(!is_prompt_echo("Sam Lee of Northwind.", ""));
        assert!(!is_prompt_echo("Sam Lee", "  ...  "));
        assert!(!is_prompt_echo("", NORTHWIND));
    }

    // Seen in a real standup: Whisper cut the read-back short, mid-word.
    #[test]
    fn a_read_back_cut_off_mid_word_is_an_echo() {
        assert!(is_prompt_echo("Omar and Sam Lee of Northw.", STANDUP));
        assert!(is_prompt_echo("Ana, Maya Lopez, Omar and Sam Lee of Northw.", STANDUP));
        assert!(is_prompt_echo("Sam Lee of Nor.", STANDUP));
        // Two letters are too few to stand for a word.
        assert!(!is_prompt_echo("Omar an.", STANDUP));
    }

    #[test]
    fn a_long_read_back_may_carry_one_stray_word() {
        assert!(is_prompt_echo("to Omar and Sam Lee of Northwind.", STANDUP));
        assert!(!is_prompt_echo("so to Omar and Sam Lee of Northwind.", STANDUP));
        // Short, a word of its own makes it a sentence.
        assert!(!is_prompt_echo("Thanks, Sam Lee.", STANDUP));
    }

    #[test]
    fn real_lines_that_use_a_name_or_two_are_kept() {
        assert!(!is_prompt_echo("Omar Reyes for some reason.", STANDUP));
        assert!(!is_prompt_echo("Thanks Dana.", STANDUP));
        assert!(!is_prompt_echo("Ask Maya Lopez about it.", STANDUP));
        assert!(!is_prompt_echo("Dana and Kai.", STANDUP));
    }

    #[test]
    fn prompt_names_read_back_as_a_speaker_label_come_off() {
        assert_eq!(
            without_prompt_label("Eric and Christian Mathiesen: So let me just get", BOARD),
            "So let me just get"
        );
        assert_eq!(
            without_prompt_label(
                "With Eric and Christian Mathiesen: Hurt to summarize the product board.",
                BOARD
            ),
            "Hurt to summarize the product board."
        );
        assert_eq!(without_prompt_label("Christian Mathiesen: okay", BOARD), "okay");
        assert_eq!(without_prompt_label("Eric: yeah, let me check", BOARD), "yeah, let me check");
    }

    #[test]
    fn a_colon_after_words_of_the_lines_own_stays() {
        for line in [
            "Here is the plan: ship Friday",
            "Note: the product board is late",
            "Eric and Christian Mathiesen joined late",
            "Eric and Christian Mathiesen:",
            "We meet at 10:30 with Eric",
        ] {
            assert_eq!(without_prompt_label(line, BOARD), line);
        }
        assert_eq!(without_prompt_label("Eric: hi", ""), "Eric: hi");
    }

    #[test]
    fn stock_lines_are_silence_guesses() {
        for line in [
            "Thank you.",
            "Thanks for watching!",
            "you",
            "Bye.",
            "Thank you for watching.",
            "Thank you very much.",
            "Bye!",
            "Subtitles by the Amara.org community",
            "ご視聴ありがとうございました",
            "Продолжение следует...",
        ] {
            assert!(is_silence_guess(line), "{line}");
        }
    }

    #[test]
    fn real_sentences_that_start_the_same_way_are_not_silence_guesses() {
        for line in [
            "Thank you for the update on the budget.",
            "You said Tuesday.",
            "Bye for now, see you Monday.",
            "",
        ] {
            assert!(!is_silence_guess(line), "{line}");
        }
    }

    #[test]
    fn noise_tags_and_wordless_lines_are_noise() {
        for line in [
            "[Music]",
            "(applause)",
            "♪",
            "♪ ♪ ♪",
            "♪ Never gonna give you up ♪",
            "*sighs*",
            "...",
            "-",
            " [BLANK_AUDIO] ",
            "[Music] [Applause]",
            "(upbeat music).",
            "[Music (soft)]",
        ] {
            assert!(is_noise(line), "{line}");
        }
    }

    #[test]
    fn speech_with_a_tag_in_it_is_not_noise() {
        for line in [
            "Okay.",
            "(laughs) Yeah, that's right.",
            "[Music] We should ship (soon)",
            "Let's ship (finally).",
            "♪ la la",
            "We said [inaudible] maybe",
        ] {
            assert!(!is_noise(line), "{line}");
        }
    }

    #[test]
    fn the_same_few_words_over_and_over_are_garbled() {
        assert!(is_garbled(""));
        assert!(is_garbled("la la la la la la la la la la"));
        assert!(!is_garbled("ha ha ha"));
        assert!(!is_garbled("We will ship on Tuesday and then review."));
        assert!(is_garbled("はいはいはいはいはいはいはいはいはいはいはい"));
        assert!(!is_garbled("明日の会議は十時からです"));
    }

    #[test]
    fn a_loop_is_cut_back_to_one_saying() {
        assert_eq!(collapse_repeats("I'm sorry. I'm sorry. I'm sorry. I'm sorry."), "I'm sorry.");
        assert_eq!(collapse_repeats("and then we and then we and then we"), "and then we");
        // The first saying's case, the last saying's punctuation.
        assert_eq!(collapse_repeats("Sorry. sorry, SORRY!"), "Sorry!");
    }

    #[test]
    fn what_comes_before_and_after_a_loop_is_kept() {
        assert_eq!(
            collapse_repeats("Okay, so we we we we need to ship it."),
            "Okay, so we need to ship it."
        );
        assert_eq!(
            collapse_repeats("So I think, I think, I think, I think we should go."),
            "So I think we should go."
        );
        assert_eq!(
            collapse_repeats("Right. Thank you. Thank you. Thank you. Next item?"),
            "Right. Thank you. Next item?"
        );
    }

    #[test]
    fn twice_is_speech_and_is_left_alone() {
        for line in [
            "very very good",
            "Thank you. Thank you.",
            "No, no. Not that one.",
            "We ship it, we ship it on Friday.",
            "spacing  is   kept when nothing loops",
        ] {
            assert_eq!(collapse_repeats(line), line);
        }
    }

    #[test]
    fn a_loop_of_up_to_eight_words_is_found() {
        let eight = "one two three four five six seven eight";
        assert_eq!(collapse_repeats(&[eight; 3].join(" ")), eight);
        let nine = "one two three four five six seven eight nine";
        let line = [nine; 3].join(" ");
        assert_eq!(collapse_repeats(&line), line);
    }

    #[test]
    fn a_line_whisper_heard_no_speech_in_goes_only_when_it_was_unsure_too() {
        let line = |no_speech, logprob| Heard {
            text: "We ship on Friday.",
            no_speech_prob: no_speech,
            avg_logprob: logprob,
        };
        assert_eq!(clean_line(line(Some(0.9), Some(-1.4)), "", false), None);
        assert!(clean_line(line(Some(0.9), Some(-0.3)), "", false).is_some());
        assert!(clean_line(line(Some(0.2), Some(-1.4)), "", false).is_some());
        assert!(clean_line(line(Some(0.9), None), "", false).is_some());
        assert!(clean_line(line(None, Some(-1.4)), "", false).is_some());
    }

    #[test]
    fn clean_line_tidies_spacing_and_drops_empty_lines_and_noise() {
        assert_eq!(
            clean_line(heard("  so   we\n ship  "), "", false).as_deref(),
            Some("so we ship")
        );
        assert_eq!(clean_line(heard("   "), "", false), None);
        assert_eq!(clean_line(heard("[Music]"), "", false), None);
    }

    #[test]
    fn an_empty_prompt_never_drops_a_line() {
        for line in ["Sam Lee of Northwind.", "Eric: hi", "Eric and Christian Mathiesen: okay"] {
            assert_eq!(clean_line(heard(line), "", false).as_deref(), Some(line));
        }
    }

    #[test]
    fn clean_line_drops_the_prompt_read_back_and_takes_off_a_label() {
        assert_eq!(clean_line(heard("Northwind sync. With Sam Lee"), NORTHWIND, false), None);
        // Two words of the prompt go only over faint audio: said out loud,
        // a short line of names is somebody talking.
        assert_eq!(clean_line(heard("Northwind sync."), NORTHWIND, true), None);
        assert_eq!(
            clean_line(heard("Northwind sync."), NORTHWIND, false).as_deref(),
            Some("Northwind sync.")
        );
        assert_eq!(
            clean_line(heard("Eric and Christian Mathiesen: So let me just get"), BOARD, false)
                .as_deref(),
            Some("So let me just get")
        );
        // Nothing after the colon: not a label, but the prompt read back.
        assert_eq!(clean_line(heard("Eric and Christian Mathiesen:"), BOARD, false), None);
        assert_eq!(
            clean_line(heard("Ask Eric about it."), BOARD, false).as_deref(),
            Some("Ask Eric about it.")
        );
    }

    #[test]
    fn a_stock_line_goes_only_over_faint_audio() {
        assert_eq!(clean_line(heard("Thank you."), "", true), None);
        assert_eq!(clean_line(heard("Thank you."), "", false).as_deref(), Some("Thank you."));
        assert_eq!(
            clean_line(heard("Thank you for the update."), "", true).as_deref(),
            Some("Thank you for the update.")
        );
    }

    #[test]
    fn a_loop_is_cut_back_and_then_judged_again() {
        let looped = "I'm sorry. I'm sorry. I'm sorry.";
        assert_eq!(clean_line(heard(looped), "", false).as_deref(), Some("I'm sorry."));
        let thanks = "Thank you. Thank you. Thank you.";
        assert_eq!(clean_line(heard(thanks), "", false).as_deref(), Some("Thank you."));
        assert_eq!(clean_line(heard(thanks), "", true), None);
        assert_eq!(clean_line(heard("Sam Lee, Sam Lee, Sam Lee"), NORTHWIND, false), None);
    }

    #[test]
    fn a_line_still_garbled_after_its_loops_are_cut_goes() {
        assert_eq!(clean_line(heard("yes no yes no no yes yes no no yes"), "", false), None);
    }

    fn samples(secs: f64) -> usize {
        (secs * f64::from(SAMPLE_RATE)).round() as usize
    }

    /// `secs` of a sine at `amp` on the -1.0..=1.0 scale, `step` radians a
    /// sample.
    fn sine(secs: f64, amp: f64, step: f64) -> Vec<i16> {
        (0..samples(secs))
            .map(|i| (amp * 32767.0 * (i as f64 * step).sin()).round() as i16)
            .collect()
    }

    fn tone(secs: f64, amp: f64) -> Vec<i16> {
        sine(secs, amp, 0.1)
    }

    /// A quiet room: about 0.001 RMS.
    fn hum(secs: f64) -> Vec<i16> {
        sine(secs, 0.0014, 1.3)
    }

    fn quiet(secs: f64) -> Vec<i16> {
        vec![0; samples(secs)]
    }

    #[test]
    fn silence_holds_no_clear_speech() {
        let clip = quiet(2.0);
        let floor = noise_floor(&clip);
        assert_eq!(floor, 0.0);
        assert_eq!(clear_speech_ms(&clip, floor), 0);
        assert_eq!(noise_floor(&[]), 0.0);
        assert_eq!(clear_speech_ms(&[], 0.0), 0);
    }

    #[test]
    fn a_second_of_talking_over_a_quiet_room_is_a_second_of_clear_speech() {
        let clip = [hum(2.0), tone(1.0, 0.2), hum(1.0)].concat();
        let floor = noise_floor(&clip);
        assert!(floor > 0.0005 && floor < 0.002, "{floor}");
        assert_eq!(clear_speech_ms(&clip, floor), 1000);
    }

    #[test]
    fn a_faint_sound_is_not_clear_speech() {
        // A faint sound just over a quiet room.
        let clip = [hum(4.0), tone(0.6, 0.008), hum(2.0)].concat();
        assert_eq!(clear_speech_ms(&clip, noise_floor(&clip)), 0);
        // A clip faint from end to end.
        let clip = tone(3.0, 0.004);
        assert!(clear_speech_ms(&clip, noise_floor(&clip)) < MIN_CLEAR_MS);
    }

    #[test]
    fn quiet_speech_on_a_quiet_headset_is_clear() {
        let clip = [quiet(3.0), tone(2.0, 0.007)].concat();
        assert!(clear_speech_ms(&clip, noise_floor(&clip)) >= 1900);
    }

    #[test]
    fn a_trailing_part_window_counts_when_at_least_half_full() {
        let loud = |n: usize| tone(n as f64 / f64::from(SAMPLE_RATE), 0.2);
        assert_eq!(clear_speech_ms(&loud(WINDOW_SAMPLES + WINDOW_SAMPLES / 2), 0.0), 200);
        assert_eq!(clear_speech_ms(&loud(WINDOW_SAMPLES + WINDOW_SAMPLES / 2 - 100), 0.0), 100);
    }

    #[test]
    fn a_loop_without_spaces_is_cut_back_and_kept() {
        let looped = "ありがとうございました。ありがとうございました。ありがとうございました。";
        assert_eq!(collapse_repeats(looped), "ありがとうございました。");
        assert_eq!(
            clean_line(heard(looped), "", false).as_deref(),
            Some("ありがとうございました。")
        );
        // Laughter, and a phrase said twice, are left alone.
        assert_eq!(collapse_repeats("ははは"), "ははは");
        assert_eq!(collapse_repeats("はいはい、わかりました"), "はいはい、わかりました");
    }
}
