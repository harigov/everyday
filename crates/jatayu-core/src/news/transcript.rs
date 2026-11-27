//! What was said in a recording, with when: an episode's transcript, a
//! recording's in Files. See `docs/plans/podcasts.md` (phase 2).
//!
//! # The file is the transcript
//!
//! A transcript is kept as a Markdown file beside the recording, in the files
//! folder, one paragraph to a stretch of speech with its time in front of it:
//!
//! ```text
//! # Episode 41: The people who wind the clocks
//!
//! The Slow Hour · 2026-11-20 · transcript
//!
//! [00:00:00] Welcome back. This week we go up the tower...
//!
//! [00:01:12] The mechanism is older than the church...
//! ```
//!
//! That one file is what a person reads, what the document index searches,
//! what the reader opens, and what "what did she just say" reads back
//! ([`parse_markdown`] then [`around`]) -- so there is no second copy in the
//! vault to fall out of step with it, and a correction somebody types into it
//! is what is read from then on.
//!
//! # Where the words come from
//!
//! The publisher's own (`podcast:transcript`): WebVTT, SRT or the Podcasting
//! 2.0 JSON, read by [`parse_vtt`], [`parse_srt`] and [`parse_json`] -- or
//! HTML or plain text with no times at all ([`parse_plain`]). Otherwise the
//! recording is listened to by the configured speech model, in the service.
//! Either way what arrives is [`TimedText`] segments, which [`paragraphs`]
//! gathers into what is written out.
//!
//! Everything here is pure: no file, no socket, no model.

use serde::{Deserialize, Serialize};

use crate::text::one_line;

/// The property a recording in Files carries naming its transcript's file
/// (a [`crate::files::PropValue::Text`] holding the id).
pub const TRANSCRIPT_PROPERTY: &str = "feed.transcript";
/// The property a transcript carries naming the recording it is of.
pub const TRANSCRIPT_OF_PROPERTY: &str = "feed.transcript_of";
/// How long a paragraph aims to be, in milliseconds of speech.
pub const PARAGRAPH_MS: u64 = 60_000;

/// One stretch of speech.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TimedText {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// Who said it, when the source says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

/// Where a transcript's words came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum TranscriptOrigin {
    /// The publisher's own, named by the feed.
    Publisher,
    /// Heard by the speech model chosen in Settings → Models.
    Recognised,
}

impl TranscriptOrigin {
    pub fn word(self) -> &'static str {
        match self {
            TranscriptOrigin::Publisher => "the publisher's transcript",
            TranscriptOrigin::Recognised => "transcript",
        }
    }
}

/// What the file's heading says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptMeta {
    pub title: String,
    /// The show, or the folder a recording is in.
    pub source: String,
    /// `YYYY-MM-DD`, when known.
    pub date: Option<String>,
    pub origin: TranscriptOrigin,
}

/// A pause at least this long between two stretches of speech is a place a
/// paragraph may end, once it has run for a while. Shorter than the pause
/// somebody leaves for a new thought, longer than the one between two
/// breaths.
pub const PAUSE_MS: u64 = 1_200;

/// `hh:mm:ss`.
pub fn format_clock(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

// ── Paragraphs ───────────────────────────────────────────────────────────

/// Gather segments into paragraphs of about `target_ms`, breaking at a pause
/// or a change of speaker where one falls near the mark. Each paragraph spans
/// its segments' times.
///
/// "Near the mark" is three steps, so that a paragraph is neither cut in the
/// middle of a thought nor allowed to run on for ever. From two thirds of
/// `target_ms` it ends at a pause ([`PAUSE_MS`]); from `target_ms` it ends at
/// any pause or wherever a sentence does; at half as much again it ends at the
/// next segment, whatever. A change of speaker always begins a new paragraph:
/// a paragraph with two voices in it would be put in the mouth of one.
///
/// Segments with nothing in them are left out, and white space inside a
/// segment's words is made one space, except for the blank lines that already
/// separate paragraphs (a transcript that came as plain text), which are kept.
pub fn paragraphs(segments: &[TimedText], target_ms: u64) -> Vec<TimedText> {
    let mut out: Vec<TimedText> = Vec::new();
    for segment in segments {
        let text = tidy(&segment.text);
        if text.is_empty() {
            continue;
        }
        let end_ms = segment.end_ms.max(segment.start_ms);
        match out.last_mut() {
            Some(open) if !breaks_before(open, segment, target_ms) => {
                open.text.push(' ');
                open.text.push_str(&text);
                open.end_ms = open.end_ms.max(end_ms);
            }
            _ => out.push(TimedText {
                start_ms: segment.start_ms,
                end_ms,
                text,
                speaker: segment.speaker.clone(),
            }),
        }
    }
    out
}

/// Whether `next` begins a paragraph of its own rather than continuing
/// `open`. See [`paragraphs`].
fn breaks_before(open: &TimedText, next: &TimedText, target_ms: u64) -> bool {
    if open.speaker != next.speaker {
        return true;
    }
    let age = next.start_ms.saturating_sub(open.start_ms);
    let pause = next.start_ms.saturating_sub(open.end_ms) >= PAUSE_MS;
    if age >= target_ms.saturating_add(target_ms / 2) {
        return true;
    }
    if age >= target_ms {
        return pause || ends_a_sentence(&open.text);
    }
    age >= target_ms.saturating_mul(2) / 3 && pause
}

/// Whether `text` ends where a sentence does, closing quotes and brackets
/// aside.
fn ends_a_sentence(text: &str) -> bool {
    let last = text.trim_end_matches(['"', '\'', ')', ']', '\u{201d}', '\u{2019}']);
    last.ends_with(['.', '?', '!', '\u{2026}'])
}

/// `text` as paragraphs: the words of each one single-spaced, and the
/// paragraphs separated by a blank line, which is what is left of the
/// line breaks a publisher's file had.
fn tidy(text: &str) -> String {
    let mut blocks: Vec<String> = Vec::new();
    let mut words: Vec<&str> = Vec::new();
    for line in text.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if !words.is_empty() {
                blocks.push(words.join(" "));
                words.clear();
            }
        } else {
            words.extend(line.split_whitespace());
        }
    }
    blocks.join("\n\n")
}

// ── The file ─────────────────────────────────────────────────────────────

/// The Markdown file for a transcript: heading, line, then a `[hh:mm:ss]`
/// paragraph each.
///
/// A paragraph that came with paragraphs of its own (a plain-text transcript
/// has no times to cut it by) is written as its time and first paragraph,
/// then the others with none, which [`parse_markdown`] folds back into it. A
/// speaker, when the source named one, is in bold in front of the words:
/// `[00:01:12] **Anna:** ...`.
pub fn to_markdown(meta: &TranscriptMeta, paragraphs: &[TimedText]) -> String {
    let title = one_line(&meta.title);
    let mut out = format!("# {}\n\n", if title.is_empty() { "Transcript" } else { &title });

    let mut line: Vec<String> = Vec::new();
    line.push(one_line(&meta.source));
    line.push(meta.date.as_deref().map(one_line).unwrap_or_default());
    line.retain(|part| !part.is_empty());
    line.push(meta.origin.word().to_string());
    out.push_str(&line.join(" \u{b7} "));
    out.push_str("\n\n");

    for paragraph in paragraphs {
        let text = tidy(&paragraph.text);
        let mut blocks = text.split("\n\n");
        let Some(first) = blocks.next().filter(|b| !b.is_empty()) else { continue };
        out.push_str(&format!("[{}] ", format_clock(paragraph.start_ms)));
        if let Some(speaker) = paragraph.speaker.as_deref() {
            let speaker = one_line(&speaker.replace('*', ""));
            if !speaker.is_empty() {
                out.push_str(&format!("**{speaker}:** "));
            }
        }
        out.push_str(first);
        out.push_str("\n\n");
        for block in blocks {
            out.push_str(block);
            out.push_str("\n\n");
        }
    }
    let mut out = out.trim_end().to_string();
    out.push('\n');
    out
}

/// Read a transcript file back: every paragraph that starts with a time.
/// Each one ends where the next begins (the last at its own start). Lines
/// before the first time -- the heading -- are not speech.
///
/// A paragraph is what stands between blank lines, so a line somebody's
/// editor wrapped is still the one paragraph. One that begins with no time
/// belongs to the paragraph with a time before it, as words typed in by hand
/// do. Times may be `hh:mm:ss` or `mm:ss`, for the same reason.
pub fn parse_markdown(text: &str) -> Vec<TimedText> {
    let mut out: Vec<TimedText> = Vec::new();
    for block in blocks_of(text) {
        let words = block.join(" ");
        if let Some((start_ms, rest)) = time_prefix(&words) {
            let (speaker, text) = speaker_prefix(rest);
            out.push(TimedText { start_ms, end_ms: start_ms, text, speaker });
        } else if let Some(last) = out.last_mut() {
            last.text.push_str("\n\n");
            last.text.push_str(&one_line(&words));
        }
    }
    for i in 1..out.len() {
        out[i - 1].end_ms = out[i].start_ms.max(out[i - 1].start_ms);
    }
    out
}

/// The blocks of `text` that stand between blank lines, each as its lines
/// trimmed.
fn blocks_of(text: &str) -> Vec<Vec<&str>> {
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut open: Vec<&str> = Vec::new();
    for line in text.lines().chain(std::iter::once("")) {
        let line = line.trim();
        if line.is_empty() {
            if !open.is_empty() {
                blocks.push(std::mem::take(&mut open));
            }
        } else {
            open.push(line);
        }
    }
    blocks
}

/// `[hh:mm:ss] rest` as the time in milliseconds and what follows it.
fn time_prefix(line: &str) -> Option<(u64, &str)> {
    let inner = line.strip_prefix('[')?;
    let (clock, rest) = inner.split_once(']')?;
    let parts: Vec<&str> = clock.split(':').collect();
    if !(2..=3).contains(&parts.len())
        || parts
            .iter()
            .any(|p| p.is_empty() || p.len() > 6 || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let seconds = parts.iter().try_fold(0u64, |total, part| {
        part.parse::<u64>().ok().and_then(|n| total.checked_mul(60)?.checked_add(n))
    })?;
    Some((seconds.checked_mul(1000)?, rest.trim_start()))
}

/// `**Anna:** words` as the speaker and the words.
fn speaker_prefix(text: &str) -> (Option<String>, String) {
    if let Some(named) = text.strip_prefix("**")
        && let Some((name, words)) = named.split_once(":**")
        && !name.trim().is_empty()
    {
        return (Some(name.trim().to_string()), words.trim_start().to_string());
    }
    (None, text.to_string())
}

/// The paragraphs from `before_ms` ahead of `at_ms` to `after_ms` after it:
/// what was being said around a moment.
///
/// A paragraph is taken to go on until the next one begins, so a moment that
/// falls in a gap between two -- music, a pause -- finds the paragraph before
/// it; and the last one goes on to the end, since [`parse_markdown`] knows no
/// end for it.
pub fn around(
    paragraphs: &[TimedText],
    at_ms: u64,
    before_ms: u64,
    after_ms: u64,
) -> Vec<TimedText> {
    let from = at_ms.saturating_sub(before_ms);
    let to = at_ms.saturating_add(after_ms);
    paragraphs
        .iter()
        .enumerate()
        .filter(|(i, p)| {
            let held_until =
                paragraphs.get(i + 1).map_or(u64::MAX, |next| next.start_ms.max(p.end_ms));
            p.start_ms <= to && held_until > from
        })
        .map(|(_, p)| p.clone())
        .collect()
}

// ── The publisher's own ──────────────────────────────────────────────────

/// WebVTT cues.
///
/// The header, `NOTE`, `STYLE` and `REGION` blocks, cue identifiers and cue
/// settings are read past; the tags inside a cue are taken out, a `<v Name>`
/// voice tag becoming the cue's speaker; and a cue of several lines is one
/// segment.
pub fn parse_vtt(text: &str) -> Vec<TimedText> {
    parse_cues(text)
}

/// SubRip cues: a number, a `00:00:01,000 --> 00:00:04,000` line, the words.
pub fn parse_srt(text: &str) -> Vec<TimedText> {
    parse_cues(text)
}

/// What both cue formats have in common: a line saying when, then the words
/// until a blank line. Read line by line, so that a file with no blank line
/// between two cues (it happens) still has both.
fn parse_cues(text: &str) -> Vec<TimedText> {
    struct Cue {
        start_ms: u64,
        end_ms: u64,
        lines: Vec<String>,
    }
    fn finish(cue: Option<Cue>, out: &mut Vec<TimedText>) {
        let Some(cue) = cue else { return };
        let mut speaker = None;
        let words: Vec<String> =
            cue.lines.iter().map(|line| strip_markup(line, &mut speaker)).collect();
        let text = tidy(&words.join(" "));
        if !text.is_empty() {
            out.push(TimedText {
                start_ms: cue.start_ms,
                end_ms: cue.end_ms.max(cue.start_ms),
                text,
                speaker,
            });
        }
    }

    let mut out = Vec::new();
    let mut open: Option<Cue> = None;
    for line in text.trim_start_matches('\u{feff}').lines() {
        if let Some((start_ms, end_ms)) = timing(line) {
            // The number SubRip puts in front of each cue, when the blank line
            // before it was left out, is the last thing the cue before took.
            if let Some(cue) = open.as_mut()
                && cue.lines.last().is_some_and(|l| {
                    !l.trim().is_empty() && l.trim().bytes().all(|b| b.is_ascii_digit())
                })
            {
                cue.lines.pop();
            }
            finish(open.take(), &mut out);
            open = Some(Cue { start_ms, end_ms, lines: Vec::new() });
        } else if line.trim().is_empty() {
            finish(open.take(), &mut out);
        } else if let Some(cue) = open.as_mut() {
            cue.lines.push(line.to_string());
        }
    }
    finish(open, &mut out);
    out
}

/// `00:00:01.000 --> 00:00:04.000 align:start`, as the two times.
fn timing(line: &str) -> Option<(u64, u64)> {
    let (from, to) = line.split_once("-->")?;
    Some((timestamp(from.trim())?, timestamp(to.split_whitespace().next()?)?))
}

/// `[hh:]mm:ss.mmm`, with a comma for the point in SubRip, as milliseconds.
fn timestamp(text: &str) -> Option<u64> {
    let parts: Vec<&str> = text.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let (seconds, fraction) = parts.last()?.split_once(['.', ',']).unwrap_or((parts.last()?, ""));
    let mut total = 0u64;
    for part in &parts[..parts.len() - 1] {
        total = total.checked_mul(60)?.checked_add(digits(part)?)?;
    }
    total = total.checked_mul(60)?.checked_add(digits(seconds)?)?;
    let millis = if fraction.is_empty() {
        0
    } else {
        // `.5` is half a second, not five milliseconds.
        let fraction_digits: String = fraction.chars().take(3).collect();
        digits(&fraction_digits)? * 10u64.pow(3 - fraction_digits.len() as u32)
    };
    total.checked_mul(1000)?.checked_add(millis)
}

/// `text` as a number, if it is only digits.
fn digits(text: &str) -> Option<u64> {
    text.bytes().all(|b| b.is_ascii_digit()).then(|| text.parse().ok()).flatten()
}

/// A cue's words without the tags in them. The first `<v Name>` voice tag
/// found is who is speaking.
fn strip_markup(text: &str, speaker: &mut Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>')
            && looks_like_tag(&rest[1..end])
        {
            if speaker.is_none() {
                *speaker = voice_name(&rest[1..end]);
            }
            rest = &rest[end + 1..];
        } else if c == '{'
            && rest[1..].starts_with('\\')
            && let Some(end) = rest.find('}')
        {
            // SubRip's own: `{\an8}` says where on the screen.
            rest = &rest[end + 1..];
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    decode_entities(&out)
}

/// Whether what is between `<` and `>` is a tag (`i`, `/c.yellow`, `v Anna`,
/// `00:00:01.500`) and not somebody's "I <3 you".
fn looks_like_tag(inner: &str) -> bool {
    match inner.chars().next() {
        Some(c) if c.is_ascii_alphabetic() || c == '/' => true,
        Some(c) if c.is_ascii_digit() => inner.contains(':'),
        _ => false,
    }
}

/// `v Anna` and `v.loud Anna` as `Anna`.
fn voice_name(tag: &str) -> Option<String> {
    let rest = tag.strip_prefix('v')?;
    if !rest.starts_with(|c: char| c.is_whitespace() || c == '.') {
        return None;
    }
    let (_, name) = rest.split_once(char::is_whitespace)?;
    let name = one_line(name);
    (!name.is_empty()).then_some(name)
}

/// The Podcasting 2.0 JSON transcript (`{"segments": [{"startTime", "endTime",
/// "body", "speaker"}]}`), times in seconds.
///
/// A bare list of segments is read too, and `start`, `end` and `text` for
/// the names the spec gives otherwise; anything that is not JSON, or has no
/// segments, is no transcript at all.
pub fn parse_json(text: &str) -> Vec<TimedText> {
    use serde_json::Value;
    let Ok(value) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) else {
        return Vec::new();
    };
    let list = match &value {
        Value::Array(list) => list,
        Value::Object(object) => match object.get("segments").and_then(Value::as_array) {
            Some(list) => list,
            None => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    let seconds = |value: Option<&Value>| -> Option<u64> {
        let seconds = match value? {
            Value::Number(n) => n.as_f64()?,
            Value::String(s) => s.trim().parse::<f64>().ok()?,
            _ => return None,
        };
        (seconds.is_finite() && seconds >= 0.0).then(|| (seconds * 1000.0).round() as u64)
    };
    let mut out = Vec::new();
    for item in list {
        let words = item.get("body").or_else(|| item.get("text")).and_then(Value::as_str);
        let Some(words) = words.map(tidy).filter(|w| !w.is_empty()) else { continue };
        let Some(start_ms) = seconds(item.get("startTime").or_else(|| item.get("start"))) else {
            continue;
        };
        let end_ms = seconds(item.get("endTime").or_else(|| item.get("end"))).unwrap_or(start_ms);
        let speaker =
            item.get("speaker").and_then(Value::as_str).map(one_line).filter(|s| !s.is_empty());
        out.push(TimedText { start_ms, end_ms: end_ms.max(start_ms), text: words, speaker });
    }
    out
}

/// HTML or plain text with no times: one segment from zero, its text with
/// the markup taken out and paragraphs kept.
///
/// What is a paragraph in HTML (`p`, `div`, a heading, an item, a table row)
/// is one here; a line break inside one is a space; scripts, styles and
/// comments are dropped with what is in them. Plain text keeps the paragraphs
/// it has -- blank lines -- and its other line breaks become spaces.
pub fn parse_plain(text: &str) -> Vec<TimedText> {
    let words = tidy(&decode_entities(&strip_html(text)));
    if words.is_empty() {
        return Vec::new();
    }
    vec![TimedText { start_ms: 0, end_ms: 0, text: words, speaker: None }]
}

/// The elements whose ending is the end of a paragraph.
const BLOCK_ELEMENTS: &[&str] = &[
    "p",
    "div",
    "li",
    "ul",
    "ol",
    "tr",
    "table",
    "section",
    "article",
    "blockquote",
    "pre",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "footer",
    "main",
    "dd",
    "dt",
    "dl",
    "hr",
];

/// `html` as text: tags gone, a paragraph break where a block element
/// begins or ends, and what scripts and styles hold left out. The entities
/// are still in it.
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(c) = rest.chars().next() {
        if c != '<' {
            out.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        }
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        let Some(end) = rest.find('>').filter(|&end| looks_like_tag(&rest[1..end])) else {
            // "a < b" is arithmetic, not markup.
            out.push(c);
            rest = &rest[1..];
            continue;
        };
        let tag = &rest[1..end];
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        rest = &rest[end + 1..];
        if (name == "script" || name == "style") && !tag.starts_with('/') && !tag.ends_with('/') {
            let close = format!("</{name}");
            let found = rest.to_ascii_lowercase().find(&close);
            rest = found
                .and_then(|at| rest[at..].find('>').map(|gt| &rest[at + gt + 1..]))
                .unwrap_or_default();
        } else if name == "br" {
            out.push('\n');
        } else if BLOCK_ELEMENTS.contains(&name.as_str()) {
            out.push_str("\n\n");
        }
    }
    out
}

/// The entities a transcript carries -- the markup five, `&nbsp;`, the
/// typographer's few -- and any number. Anything else is left as written.
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|&semi| semi <= 10).and_then(|semi| {
            let name = &rest[1..semi];
            let c = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" | "lrm" | "rlm" => ' ',
                "ndash" => '\u{2013}',
                "mdash" => '\u{2014}',
                "lsquo" => '\u{2018}',
                "rsquo" => '\u{2019}',
                "ldquo" => '\u{201c}',
                "rdquo" => '\u{201d}',
                "hellip" => '\u{2026}',
                _ => {
                    let number = name.strip_prefix('#')?;
                    let code = match number.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => number.parse().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((c, semi + 1))
        });
        match decoded {
            Some((c, used)) => {
                out.push(c);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start_ms: u64, end_ms: u64, text: &str) -> TimedText {
        TimedText { start_ms, end_ms, text: text.into(), speaker: None }
    }

    fn said(start_ms: u64, end_ms: u64, who: &str, text: &str) -> TimedText {
        TimedText { start_ms, end_ms, text: text.into(), speaker: Some(who.into()) }
    }

    fn texts(segments: &[TimedText]) -> Vec<&str> {
        segments.iter().map(|s| s.text.as_str()).collect()
    }

    fn meta() -> TranscriptMeta {
        TranscriptMeta {
            title: "Episode 41: The people who wind the clocks".into(),
            source: "The Slow Hour".into(),
            date: Some("2026-11-20".into()),
            origin: TranscriptOrigin::Recognised,
        }
    }

    // ── The clock ────────────────────────────────────────────────────────

    #[test]
    fn a_time_is_hours_minutes_and_seconds() {
        assert_eq!(format_clock(0), "00:00:00");
        assert_eq!(format_clock(999), "00:00:00");
        assert_eq!(format_clock(61_000), "00:01:01");
        assert_eq!(format_clock(3_725_999), "01:02:05");
        assert_eq!(format_clock(100 * 3_600_000), "100:00:00");
    }

    // ── Paragraphs ───────────────────────────────────────────────────────

    #[test]
    fn short_segments_run_together_until_a_pause_near_the_target() {
        // Ten seconds a segment with no gaps, a target of a minute: nothing
        // breaks until a pause turns up after two thirds of it (40 s).
        let mut segments: Vec<TimedText> =
            (0..4).map(|i| seg(i * 10_000, i * 10_000 + 10_000, &format!("one {i}"))).collect();
        // A pause of two seconds before the fifth, which is 40 s in.
        segments.push(seg(42_000, 52_000, "two 0"));
        segments.push(seg(52_000, 62_000, "two 1"));
        let made = paragraphs(&segments, PARAGRAPH_MS);
        assert_eq!(texts(&made), ["one 0 one 1 one 2 one 3", "two 0 two 1"]);
        assert_eq!((made[0].start_ms, made[0].end_ms), (0, 40_000));
        assert_eq!((made[1].start_ms, made[1].end_ms), (42_000, 62_000));
    }

    #[test]
    fn a_pause_before_two_thirds_of_the_target_does_not_break() {
        let segments = [seg(0, 5_000, "a."), seg(8_000, 12_000, "b."), seg(20_000, 24_000, "c.")];
        assert_eq!(texts(&paragraphs(&segments, PARAGRAPH_MS)), ["a. b. c."]);
    }

    #[test]
    fn past_the_target_a_sentence_end_will_do() {
        // No pauses anywhere; the first sentence ends after the mark.
        let segments = [
            seg(0, 30_000, "and then"),
            seg(30_000, 59_000, "we went on,"),
            seg(59_000, 62_000, "and on."),
            seg(62_000, 70_000, "Next topic."),
        ];
        let made = paragraphs(&segments, PARAGRAPH_MS);
        assert_eq!(texts(&made), ["and then we went on, and on.", "Next topic."]);
    }

    #[test]
    fn a_paragraph_that_will_not_pause_is_cut_at_half_as_much_again() {
        let segments: Vec<TimedText> =
            (0..12).map(|i| seg(i * 10_000, (i + 1) * 10_000, &format!("w{i}"))).collect();
        let made = paragraphs(&segments, PARAGRAPH_MS);
        // 90 s is a paragraph and a half: the tenth segment starts the next.
        assert_eq!(made.len(), 2);
        assert_eq!(made[0].text, "w0 w1 w2 w3 w4 w5 w6 w7 w8");
        assert_eq!(made[1].text, "w9 w10 w11");
        assert_eq!(made[1].start_ms, 90_000);
    }

    #[test]
    fn a_change_of_speaker_always_begins_a_paragraph() {
        let segments = [
            said(0, 2_000, "Anna", "Welcome."),
            said(2_000, 4_000, "Anna", "Today, clocks."),
            said(4_000, 5_000, "Ben", "Thanks."),
            said(5_000, 6_000, "Anna", "Quite."),
        ];
        let made = paragraphs(&segments, PARAGRAPH_MS);
        assert_eq!(texts(&made), ["Welcome. Today, clocks.", "Thanks.", "Quite."]);
        assert_eq!(made[0].speaker.as_deref(), Some("Anna"));
        assert_eq!(made[1].speaker.as_deref(), Some("Ben"));
    }

    #[test]
    fn white_space_is_single_and_empty_segments_are_dropped() {
        let segments = [
            seg(0, 1_000, "  two\n lines\t here "),
            seg(1_000, 2_000, "  "),
            seg(2_000, 3_000, "more"),
        ];
        assert_eq!(texts(&paragraphs(&segments, PARAGRAPH_MS)), ["two lines here more"]);
        assert!(paragraphs(&[], PARAGRAPH_MS).is_empty());
        assert!(paragraphs(&[seg(0, 1, "")], PARAGRAPH_MS).is_empty());
    }

    #[test]
    fn a_segment_that_ends_before_it_begins_is_made_a_point() {
        let made = paragraphs(&[seg(5_000, 1_000, "odd")], PARAGRAPH_MS);
        assert_eq!((made[0].start_ms, made[0].end_ms), (5_000, 5_000));
    }

    #[test]
    fn the_paragraphs_a_plain_file_had_are_kept() {
        let made = paragraphs(&[seg(0, 0, "First para\nwrapped.\n\n\nSecond.")], PARAGRAPH_MS);
        assert_eq!(made[0].text, "First para wrapped.\n\nSecond.");
    }

    // ── The file ─────────────────────────────────────────────────────────

    #[test]
    fn a_transcript_is_a_heading_a_line_and_a_timed_paragraph_each() {
        let made = [
            seg(0, 72_000, "Welcome back. This week we go up the tower..."),
            seg(72_000, 90_000, "The mechanism is older than the church..."),
        ];
        assert_eq!(
            to_markdown(&meta(), &made),
            "# Episode 41: The people who wind the clocks\n\n\
             The Slow Hour \u{b7} 2026-11-20 \u{b7} transcript\n\n\
             [00:00:00] Welcome back. This week we go up the tower...\n\n\
             [00:01:12] The mechanism is older than the church...\n"
        );
    }

    #[test]
    fn the_line_under_the_heading_says_only_what_is_known() {
        let bare = TranscriptMeta {
            title: " ".into(),
            source: String::new(),
            date: None,
            origin: TranscriptOrigin::Publisher,
        };
        assert_eq!(
            to_markdown(&bare, &[seg(0, 1, "Hi.")]),
            "# Transcript\n\nthe publisher's transcript\n\n[00:00:00] Hi.\n"
        );
        let dated = TranscriptMeta { source: String::new(), ..meta() };
        assert!(to_markdown(&dated, &[]).contains("\n\n2026-11-20 \u{b7} transcript\n"));
    }

    #[test]
    fn a_title_with_line_breaks_stays_one_heading() {
        let odd = TranscriptMeta { title: "Two\nlines".into(), ..meta() };
        assert!(to_markdown(&odd, &[]).starts_with("# Two lines\n\n"));
    }

    #[test]
    fn a_transcript_with_nothing_said_is_still_a_file() {
        let file = to_markdown(&meta(), &[]);
        assert!(file.ends_with("transcript\n"), "{file:?}");
        assert!(parse_markdown(&file).is_empty());
    }

    #[test]
    fn a_file_read_back_has_every_paragraph_and_each_ends_where_the_next_begins() {
        let made = [
            seg(0, 70_000, "Welcome back."),
            seg(72_000, 140_000, "The mechanism."),
            seg(3_725_000, 3_730_000, "Last words."),
        ];
        let read = parse_markdown(&to_markdown(&meta(), &made));
        assert_eq!(texts(&read), ["Welcome back.", "The mechanism.", "Last words."]);
        let times: Vec<(u64, u64)> = read.iter().map(|p| (p.start_ms, p.end_ms)).collect();
        // The last has no end of its own to be found: it ends where it begins.
        assert_eq!(times, [(0, 72_000), (72_000, 3_725_000), (3_725_000, 3_725_000)]);
    }

    #[test]
    fn a_speaker_comes_back_as_one() {
        let made = [
            said(0, 5_000, "Anna", "Hello."),
            seg(5_000, 9_000, "Ben says hi."),
            said(9_000, 10_000, "Dr. Ben *B*", "Hm."),
        ];
        let file = to_markdown(&meta(), &made);
        assert!(file.contains("[00:00:00] **Anna:** Hello.\n"), "{file}");
        assert!(file.contains("[00:00:05] Ben says hi.\n"), "{file}");
        assert!(file.contains("[00:00:09] **Dr. Ben B:** Hm.\n"), "{file}");
        let read = parse_markdown(&file);
        assert_eq!(read[0].speaker.as_deref(), Some("Anna"));
        assert_eq!(read[0].text, "Hello.");
        assert_eq!(read[1].speaker, None);
        assert_eq!(read[2].speaker.as_deref(), Some("Dr. Ben B"));
    }

    #[test]
    fn what_a_file_says_is_the_same_after_a_round_trip() {
        let made = [
            said(0, 1_000, "Anna", "One."),
            said(61_000, 62_000, "Ben", "Two, with: a colon and **bold** words."),
            seg(3_600_000, 3_601_000, "Three."),
        ];
        let file = to_markdown(&meta(), &made);
        let read = parse_markdown(&file);
        for (written, back) in made.iter().zip(&read) {
            assert_eq!(
                (written.start_ms, &written.text, &written.speaker),
                (back.start_ms, &back.text, &back.speaker)
            );
        }
        // And written again, it is the same file.
        assert_eq!(to_markdown(&meta(), &read), file);
    }

    #[test]
    fn a_paragraph_with_paragraphs_of_its_own_comes_back_whole() {
        let plain = parse_plain("Opening words.\n\nMore words.");
        let file = to_markdown(&meta(), &plain);
        assert!(file.ends_with("[00:00:00] Opening words.\n\nMore words.\n"), "{file}");
        let read = parse_markdown(&file);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].text, "Opening words.\n\nMore words.");
    }

    #[test]
    fn what_comes_before_the_first_time_is_not_speech() {
        let file = "# Title\n\nShow \u{b7} 2026-01-01 \u{b7} transcript\n\nA note somebody added.\n\n[00:00:05] First.\n";
        let read = parse_markdown(file);
        assert_eq!(texts(&read), ["First."]);
        assert_eq!(read[0].start_ms, 5_000);
        assert!(parse_markdown("").is_empty());
        assert!(parse_markdown("# Only a heading\n\nNo times here.").is_empty());
    }

    #[test]
    fn a_wrapped_line_is_one_paragraph_and_words_typed_in_belong_to_the_one_before() {
        let file = "[00:00:00] A line that an editor\nwrapped in two.\n\n\
                    Some words somebody typed in.\n\n[01:30] Short time.\r\n\r\n[1:00:00] An hour.";
        let read = parse_markdown(file);
        assert_eq!(
            texts(&read),
            [
                "A line that an editor wrapped in two.\n\nSome words somebody typed in.",
                "Short time.",
                "An hour."
            ]
        );
        // `mm:ss` and `h:mm:ss` are both times.
        assert_eq!(read[1].start_ms, 90_000);
        assert_eq!(read[2].start_ms, 3_600_000);
    }

    #[test]
    fn something_that_only_looks_like_a_time_is_not_one() {
        for not_a_time in [
            "[12] Footnote.",
            "[ab:cd] No.",
            "[00:00:00:00] No.",
            "[:30] No.",
            "00:00:10 No.",
            "[00:10 No.",
        ] {
            assert!(parse_markdown(not_a_time).is_empty(), "{not_a_time}");
        }
    }

    #[test]
    fn times_going_backwards_do_not_make_a_paragraph_end_before_it_begins() {
        let read = parse_markdown("[00:00:30] A.\n\n[00:00:10] B.");
        assert_eq!((read[0].start_ms, read[0].end_ms), (30_000, 30_000));
    }

    // ── A moment ─────────────────────────────────────────────────────────

    fn file_of_four() -> Vec<TimedText> {
        parse_markdown("[00:00:00] a\n\n[00:01:00] b\n\n[00:02:00] c\n\n[00:03:00] d")
    }

    #[test]
    fn around_a_moment_is_the_paragraph_being_said_and_what_the_window_reaches() {
        let all = file_of_four();
        // 1:30 is in `b`; nothing either side.
        assert_eq!(texts(&around(&all, 90_000, 0, 0)), ["b"]);
        // `b` begins thirty seconds back and `c` thirty ahead: a window as
        // wide as that touches `a`'s end and `c`'s start, and takes `c` only.
        assert_eq!(texts(&around(&all, 90_000, 30_000, 29_999)), ["b"]);
        assert_eq!(texts(&around(&all, 90_000, 30_000, 30_000)), ["b", "c"]);
        assert_eq!(texts(&around(&all, 90_000, 30_001, 0)), ["a", "b"]);
        assert_eq!(texts(&around(&all, 90_000, 90_000, 90_000)), ["a", "b", "c", "d"]);
    }

    #[test]
    fn around_the_start_of_one_does_not_include_the_one_that_ended_there() {
        let all = file_of_four();
        assert_eq!(texts(&around(&all, 60_000, 0, 0)), ["b"]);
        assert_eq!(texts(&around(&all, 60_000, 1, 0)), ["a", "b"]);
    }

    #[test]
    fn around_a_moment_after_the_last_start_is_the_last_paragraph() {
        let all = file_of_four();
        assert_eq!(texts(&around(&all, 9_000_000, 0, 0)), ["d"]);
        assert_eq!(texts(&around(&all, 200_000, 30_000, 5_000)), ["c", "d"]);
    }

    #[test]
    fn around_a_gap_between_paragraphs_is_the_one_before_it() {
        let spaced = [seg(0, 10_000, "talk"), seg(120_000, 130_000, "more talk")];
        assert_eq!(texts(&around(&spaced, 60_000, 0, 0)), ["talk"]);
        assert_eq!(texts(&around(&spaced, 60_000, 0, 70_000)), ["talk", "more talk"]);
    }

    #[test]
    fn around_with_nothing_or_before_the_start() {
        assert!(around(&[], 5, 5, 5).is_empty());
        let all = file_of_four();
        assert_eq!(texts(&around(&all, 0, 10_000_000, 0)), ["a"]);
        // Nothing overflows at the far end of time.
        assert_eq!(texts(&around(&all, u64::MAX, u64::MAX, u64::MAX)).len(), 4);
    }

    // ── WebVTT ───────────────────────────────────────────────────────────

    const VTT: &str = "\u{feff}WEBVTT - the slow hour\n\
        Kind: captions\n\
        \n\
        STYLE\n\
        ::cue { color: yellow }\n\
        \n\
        NOTE This is a comment\n\
        that --> spans lines.\n\
        \n\
        intro\n\
        00:00:00.000 --> 00:00:04.500 align:start position:0%\n\
        <v Anna>Welcome back to <i>The Slow Hour</i>.</v>\n\
        \n\
        2\n\
        00:00:04.500 --> 00:00:09.000\n\
        This week,\n\
        we go up the <c.yellow>tower</c>\n\
        &amp; look at the <00:00:07.000>clock &lt;3\n\
        \n\
        01:02:03.250 --> 01:02:05.000\n\
        <v.loud Dr. Ben Carter>An hour in.\n\
        \n\
        00:09.5 --> 00:10.000\n\
        \n\
        00:11.000 --> 00:12.000\n\
        Short form.\n";

    #[test]
    fn webvtt_cues_are_read_past_headers_notes_ids_settings_and_tags() {
        let cues = parse_vtt(VTT);
        assert_eq!(
            texts(&cues),
            [
                "Welcome back to The Slow Hour.",
                "This week, we go up the tower & look at the clock <3",
                "An hour in.",
                "Short form."
            ]
        );
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (0, 4_500));
        assert_eq!(cues[0].speaker.as_deref(), Some("Anna"));
        assert_eq!((cues[1].start_ms, cues[1].end_ms), (4_500, 9_000));
        assert_eq!(cues[1].speaker, None);
        assert_eq!(cues[2].start_ms, 3_723_250);
        assert_eq!(cues[2].speaker.as_deref(), Some("Dr. Ben Carter"));
        // `mm:ss.mmm` is a time too; a cue with nothing in it is not a cue.
        assert_eq!((cues[3].start_ms, cues[3].end_ms), (11_000, 12_000));
    }

    #[test]
    fn webvtt_with_crlf_line_ends_reads_the_same() {
        let crlf = VTT.replace('\n', "\r\n");
        assert_eq!(parse_vtt(&crlf), parse_vtt(VTT));
    }

    #[test]
    fn a_time_with_a_short_fraction_is_what_it_says() {
        let cues = parse_vtt("WEBVTT\n\n00:00:01.5 --> 00:00:02.25\nHi\n");
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (1_500, 2_250));
    }

    #[test]
    fn webvtt_that_is_not_webvtt_has_no_cues() {
        assert!(parse_vtt("").is_empty());
        assert!(parse_vtt("WEBVTT\n\n").is_empty());
        assert!(parse_vtt("<html><body>Not found</body></html>").is_empty());
        assert!(parse_vtt("00:00:01.000 --> nonsense\ntext").is_empty());
    }

    // ── SubRip ───────────────────────────────────────────────────────────

    #[test]
    fn subrip_cues_have_numbers_and_commas_and_styling() {
        let srt = "1\r\n00:00:01,000 --> 00:00:03,500\r\n<i>Hello</i> there,\r\nand welcome.\r\n\r\n\
                   2\r\n00:00:03,500 --> 00:00:06,000\r\n{\\an8}<font color=\"#fff\">Second</font> cue &amp; more\r\n\r\n\
                   3\r\n01:00:00,000 --> 01:00:01,000\r\nI <3 you\r\n";
        let cues = parse_srt(srt);
        assert_eq!(texts(&cues), ["Hello there, and welcome.", "Second cue & more", "I <3 you"]);
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (1_000, 3_500));
        assert_eq!(cues[2].start_ms, 3_600_000);
        assert!(cues.iter().all(|c| c.speaker.is_none()));
    }

    #[test]
    fn subrip_with_no_blank_line_between_cues_still_has_both() {
        let srt =
            "1\n00:00:01,000 --> 00:00:02,000\nOne.\n2\n00:00:02,000 --> 00:00:03,000\nTwo.\n";
        assert_eq!(texts(&parse_srt(srt)), ["One.", "Two."]);
    }

    #[test]
    fn a_number_said_in_a_cue_is_not_taken_for_the_next_ones_id_when_a_blank_line_follows() {
        let srt =
            "1\n00:00:01,000 --> 00:00:02,000\n2024\n\n2\n00:00:02,000 --> 00:00:03,000\nTwo.\n";
        assert_eq!(texts(&parse_srt(srt)), ["2024", "Two."]);
    }

    // ── Podcasting 2.0 JSON ──────────────────────────────────────────────

    #[test]
    fn json_segments_are_in_seconds_with_speakers() {
        let json = r#"{"version":"1.0.0","segments":[
            {"speaker":"Anna","startTime":0,"endTime":4.5,"body":"Welcome back."},
            {"startTime":4.5,"endTime":9.0,"body":"  Second\n line. "},
            {"startTime":"12.25","body":"No end."},
            {"startTime":20,"endTime":21,"body":""},
            {"endTime":30,"body":"No start."},
            {"speaker":"Ben","startTime":30,"endTime":31,"body":"Last."}
        ]}"#;
        let segments = parse_json(json);
        assert_eq!(texts(&segments), ["Welcome back.", "Second line.", "No end.", "Last."]);
        assert_eq!((segments[0].start_ms, segments[0].end_ms), (0, 4_500));
        assert_eq!(segments[0].speaker.as_deref(), Some("Anna"));
        assert_eq!(segments[1].speaker, None);
        assert_eq!((segments[2].start_ms, segments[2].end_ms), (12_250, 12_250));
        assert_eq!(segments[3].speaker.as_deref(), Some("Ben"));
    }

    #[test]
    fn a_bare_list_and_the_other_names_are_read_too() {
        let segments =
            parse_json(r#"[{"start":1,"end":2,"text":"Hi."},{"start":-1,"text":"No."}]"#);
        assert_eq!(texts(&segments), ["Hi."]);
    }

    #[test]
    fn json_that_is_no_transcript_has_no_segments() {
        for not_a_transcript in
            ["", "nope", "{}", r#"{"segments":"x"}"#, "42", "[1,2]", "\u{feff}{\"segments\":[]}"]
        {
            assert!(parse_json(not_a_transcript).is_empty(), "{not_a_transcript}");
        }
    }

    // ── Plain ────────────────────────────────────────────────────────────

    #[test]
    fn html_is_words_with_paragraphs() {
        let html = "<html><head><title>x</title><style>p { color: red }</style></head><body>\
            <h1>Episode 41</h1><!-- a comment -->\
            <p>Welcome &amp; hello,<br>again.</p>\
            <script type=\"text/javascript\">var a = '<p>no</p>';</script>\
            <div>Second <b>bold</b> one &#8212; yes &#x2014; &bogus; a < b.</div>\
            <ul><li>one</li><li>two</li></ul></body></html>";
        let plain = parse_plain(html);
        assert_eq!(plain.len(), 1);
        assert_eq!((plain[0].start_ms, plain[0].end_ms), (0, 0));
        assert_eq!(
            plain[0].text,
            "x\n\nEpisode 41\n\nWelcome & hello, again.\n\n\
             Second bold one \u{2014} yes \u{2014} &bogus; a < b.\n\none\n\ntwo"
        );
    }

    #[test]
    fn plain_text_keeps_its_paragraphs_and_joins_its_wrapped_lines() {
        let plain = parse_plain("\u{a0}First line\nsecond line.\r\n\r\n\r\nNew paragraph.  \n");
        assert_eq!(plain[0].text, "First line second line.\n\nNew paragraph.");
        assert!(parse_plain("").is_empty());
        assert!(parse_plain("  \n <p> </p> <script>x</script> ").is_empty());
    }

    #[test]
    fn an_unclosed_script_takes_the_rest_and_an_unclosed_tag_is_only_text() {
        assert_eq!(parse_plain("Before<script>var x = 1;")[0].text, "Before");
        assert_eq!(parse_plain("Before <b unfinished")[0].text, "Before <b unfinished");
    }

    // ── Together ─────────────────────────────────────────────────────────

    #[test]
    fn a_publishers_transcript_is_gathered_into_a_file_and_found_again() {
        let cues = parse_vtt(VTT);
        let made = paragraphs(&cues, PARAGRAPH_MS);
        let publisher = TranscriptMeta { origin: TranscriptOrigin::Publisher, ..meta() };
        let file = to_markdown(&publisher, &made);
        assert!(file.contains("the publisher's transcript"), "{file}");
        let read = parse_markdown(&file);
        // Anna spoke, then a cue with no voice, then Dr. Ben Carter, then none.
        assert_eq!(read.len(), 4);
        assert_eq!(read[0].speaker.as_deref(), Some("Anna"));
        assert_eq!(
            texts(&around(&read, 4_600, 0, 0)),
            ["This week, we go up the tower & look at the clock <3"]
        );
    }
}
