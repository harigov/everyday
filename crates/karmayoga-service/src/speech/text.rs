//! Making writing fit to be heard: an assistant's reply, an article, an email.
//!
//! Pure text in, text out: nothing here reads a clock, a file or a socket.
//! [`SentenceSplitter`] cuts text, streamed or whole, into sentences, so the
//! first can be spoken while the rest is still being written or read, and
//! [`speakable`] turns each into something a voice can say. [`sentences`] is
//! the whole of a text at once, for writing that is already complete.
//!
//! # Cutting sentences while they stream
//!
//! The splitter keeps all the text and how much of it has been handed out,
//! and looks at what is left each time. It never guesses at the end of
//! a sentence it cannot see yet: a full stop at the very end of what has
//! arrived might be the start of "3.5" or "e.g.", so it waits for whatever
//! comes next. Nothing is cut that the next piece of text could un-cut.
//!
//! What counts as the end of a sentence is a `.`, `!` or `?` followed by white
//! space, and any newline. A full stop is not one after an abbreviation
//! ("Dr.", "e.g.", "U.S."), an initial ("J. K. Rowling"), or a list number at
//! the start of a line. Fenced code is not read aloud at all.
//!
//! A fragment too short to be worth a voice's breath ("OK.") is held and
//! joined to the sentence after it. One too long to say in a breath is cut
//! at a comma, or failing that a space.
//!
//! A sentence that ended a line -- an item of a list, the end of a paragraph
//! -- is handed out with [`LINE_END`] after it, so that the voice pauses
//! longer there; and a `{word|respelling}` mark is kept as it was written.
//! Both are [`script`]'s: anything that shows or keeps a sentence rather than
//! saying it takes them off with [`script::shown`].

use super::script::{self, LINE_END};
use std::collections::VecDeque;

/// A sentence shorter than this, once made speakable, is joined to the next
/// one rather than spoken alone, unless the reply ends there.
const MIN_FRAGMENT: usize = 12;

/// A sentence longer than this is cut at a comma or a space. About what a
/// voice says comfortably in one go.
const MAX_SENTENCE: usize = 240;

/// Cuts streamed prose into sentences as they complete.
#[derive(Debug, Default)]
pub struct SentenceSplitter {
    /// Everything the text says so far.
    reply: String,
    /// How many characters of `reply` have been dealt with: spoken, or skipped
    /// because there was nothing to say in them (code, blank space). Counted
    /// in characters rather than bytes so a replacement that differs after
    /// this point cannot land it in the middle of a character.
    handed: usize,
    /// Each sentence handed out, and where in `reply` it ended.
    said: Vec<(usize, String)>,
    /// Sentences already said that a restarted step is expected to say again,
    /// in order. Matching ones are not handed out twice.
    repeats: VecDeque<String>,
}

impl SentenceSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add streamed text; answers the sentences it completed, ready to speak.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.reply.push_str(delta);
        self.drain(false)
    }

    /// The reply was replaced by `reply` (a retried step). Answers whatever
    /// complete sentences of it have not been handed out yet.
    ///
    /// A retried step starts again from where it began, usually in words of its
    /// own. So the place to carry on from is the last sentence that ended
    /// within what the old and new text still share. What was said after that
    /// is remembered: if the step says it again it is not repeated, and if the
    /// step says something else, that is what is heard.
    pub fn replace(&mut self, reply: &str) -> Vec<String> {
        let shared = self.reply.chars().zip(reply.chars()).take_while(|(a, b)| a == b).count();
        self.reply = reply.to_string();
        if self.handed > shared {
            let keep = self.said.iter().take_while(|(end, _)| *end <= shared).count();
            let again = self.said.split_off(keep);
            self.handed = self.said.last().map_or(0, |(end, _)| *end);
            let mut repeats: VecDeque<String> = again.into_iter().map(|(_, s)| s).collect();
            repeats.append(&mut self.repeats);
            self.repeats = repeats;
        }
        self.drain(false)
    }

    /// The reply is over: whatever is left, if it is worth saying.
    pub fn finish(&mut self) -> Option<String> {
        let rest = self.drain(true);
        if rest.is_empty() { None } else { Some(rest.join(" ")) }
    }

    fn drain(&mut self, last: bool) -> Vec<String> {
        let start = byte_of(&self.reply, self.handed);
        let mut pos = start;
        let mut out = Vec::new();
        loop {
            match step(&self.reply[pos..], last) {
                Step::Wait => break,
                Step::Skip(n) if n > 0 => pos += n,
                Step::Emit { end, text } if end > 0 => {
                    pos += end;
                    if !text.is_empty() {
                        // Whether a line ended after it can hang on how
                        // the text arrived, and is only the pause after it.
                        let same = |said: &String| {
                            said.trim_end_matches(LINE_END) == text.trim_end_matches(LINE_END)
                        };
                        if self.repeats.front().is_some_and(same) {
                            // Said before the step was retried.
                            self.repeats.pop_front();
                        } else {
                            self.repeats.clear();
                            let at = self.handed + self.reply[start..pos].chars().count();
                            self.said.push((at, text.clone()));
                            out.push(text);
                        }
                    }
                }
                // Nothing taken: would go round for ever.
                Step::Skip(_) | Step::Emit { .. } => break,
            }
        }
        self.handed += self.reply[start..pos].chars().count();
        out
    }
}

/// Every sentence of a complete `text`, made speakable, in order. What the
/// streaming splitter would hand out for it pushed all at once and finished.
pub fn sentences(text: &str) -> Vec<String> {
    let mut splitter = SentenceSplitter::new();
    let mut out = splitter.push(text);
    out.extend(splitter.finish());
    out
}

/// The byte offset of the `n`th character of `text`, or its end.
fn byte_of(text: &str, n: usize) -> usize {
    text.char_indices().nth(n).map_or(text.len(), |(i, _)| i)
}

/// What the front of the unhandled text amounts to.
enum Step {
    /// Not enough has arrived to say.
    Wait,
    /// This many bytes hold nothing to say.
    Skip(usize),
    /// This many bytes make `text`, which is empty when there was nothing
    /// speakable in them.
    Emit { end: usize, text: String },
}

fn step(rest: &str, last: bool) -> Step {
    let lead = rest.len() - rest.trim_start().len();
    let body = &rest[lead..];
    if body.is_empty() {
        return if lead > 0 { Step::Skip(lead) } else { Step::Wait };
    }
    if starts_fence(body) {
        return match fence_end(body, last) {
            Some(end) => Step::Skip(lead + end),
            None if last => Step::Skip(rest.len()),
            None => Step::Wait,
        };
    }
    let mut end = match boundary(body, last) {
        Some(first) => match merged_end(body, first, last) {
            Some(end) => end,
            None => return Step::Wait,
        },
        None if body.chars().count() > MAX_SENTENCE => body.len(),
        None => return Step::Wait,
    };
    if body[..end].chars().count() > MAX_SENTENCE {
        end = split_point(&body[..end]);
    }
    let mut text = speakable(&body[..end]);
    // A line ended here -- an item of a list, a paragraph -- which is a
    // longer pause than a full stop; the mark says so to the voice, since the
    // line break itself is left behind. See `script::pause_after`.
    if !text.is_empty() && body[end..].trim_start_matches([' ', '\t']).starts_with(['\n', '\r']) {
        text.push(LINE_END);
    }
    Step::Emit { end: lead + end, text }
}

fn starts_fence(body: &str) -> bool {
    body.starts_with("```") || body.starts_with("~~~")
}

/// Where a fenced block opening `body` ends, through the rest of its closing
/// line; `None` while it has not closed yet.
fn fence_end(body: &str, last: bool) -> Option<usize> {
    let marker = &body[..3];
    let close = body[3..].find(marker)? + 3 + 3;
    match body[close..].find('\n') {
        Some(n) => Some(close + n),
        None if last => Some(body.len()),
        None => None,
    }
}

/// Extend the sentence ending at `end` over the ones after it for as long as
/// it is too short to say on its own. `None` when the text after it has not
/// arrived yet.
fn merged_end(body: &str, first: usize, last: bool) -> Option<usize> {
    let mut end = first;
    while is_short(&body[..end]) {
        let after = &body[end..];
        let gap = after.len() - after.trim_start().len();
        let next = &after[gap..];
        if next.is_empty() {
            if last {
                break;
            }
            return None;
        }
        if starts_fence(next) {
            break;
        }
        match boundary(next, last) {
            Some(more) => end += gap + more,
            None if next.chars().count() > MAX_SENTENCE => {
                end = body.len();
                break;
            }
            None => return None,
        }
    }
    Some(end)
}

fn is_short(piece: &str) -> bool {
    speakable(piece).chars().count() < MIN_FRAGMENT
}

/// Where to cut a piece longer than [`MAX_SENTENCE`]: after the last comma in
/// the latter part of its first stretch, failing that at the last space, and
/// failing that after the stretch.
///
/// Never inside a `{word|respelling}` mark, which cut in two would be neither
/// a mark nor what was meant.
fn split_point(piece: &str) -> usize {
    let limit = piece.char_indices().nth(MAX_SENTENCE).map_or(piece.len(), |(i, _)| i);
    let marks = script::marks(piece);
    let free = |i: usize| !marks.iter().any(|m| m.start < i && i < m.end);
    let head = &piece[..limit];
    if let Some((i, _)) = head.rmatch_indices(", ").find(|&(i, _)| free(i))
        && i >= limit / 3
    {
        return i + 1;
    }
    if let Some((i, _)) =
        head.char_indices().rev().find(|&(i, c)| c.is_whitespace() && i > 0 && free(i))
    {
        return i;
    }
    // In the middle of a word, the only cut left: before a mark it would fall
    // in, or after one that starts the piece.
    match marks.iter().find(|m| m.start < limit && limit < m.end) {
        Some(m) if m.start > 0 => m.start,
        Some(m) => m.end,
        None => limit,
    }
}

/// What a full stop after a word amounts to.
enum Dot {
    End,
    NotEnd,
    /// Depends on the next word.
    Ask,
}

/// Words that a full stop follows without ending the sentence.
const ABBREVIATIONS: &[&str] = &[
    "dr", "mr", "mrs", "ms", "mx", "prof", "sr", "jr", "st", "vs", "e.g", "i.e", "cf", "fig",
    "approx", "inc", "ltd", "corp", "dept", "est",
];

/// Whether the full stop at byte `at` of `text`, which a space follows, ends a
/// sentence.
fn dot_ends(text: &str, at: usize) -> Dot {
    let head = &text[..at];
    let start = head
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '.')
        .last()
        .map_or(at, |(i, _)| i);
    let word = &head[start..];
    if word.is_empty() {
        return Dot::End;
    }
    let lower = word.to_lowercase();
    if ABBREVIATIONS.contains(&lower.as_str()) {
        return Dot::NotEnd;
    }
    if lower == "etc" {
        return Dot::Ask;
    }
    // U.S., a.m., Ph.D.: every part between the dots is one letter.
    let parts: Vec<&str> = word.split('.').collect();
    if parts.len() >= 2
        && parts.iter().all(|p| p.chars().count() == 1 && p.chars().all(char::is_alphabetic))
    {
        return Dot::NotEnd;
    }
    // An initial: "J. K. Rowling". Not "I.", which is a word.
    let mut letters = word.chars();
    if let (Some(c), None) = (letters.next(), letters.next())
        && c.is_uppercase()
        && c != 'I'
    {
        return Dot::NotEnd;
    }
    // "1." opening a list item.
    if word.chars().all(|c| c.is_ascii_digit()) && head[..start].trim().is_empty() {
        return Dot::NotEnd;
    }
    Dot::End
}

/// Where the first sentence of `text` ends, in bytes, leaving out the space
/// after it. `None` when what has arrived does not settle it, unless `last`:
/// then whatever there is, is a sentence.
///
/// Never inside a `{word|respelling}` mark -- "Mt." in `{Mt. Fuji|...}` ends
/// nothing -- and never after a `{` that more text could still close into
/// one, which is waited for.
fn boundary(text: &str, last: bool) -> Option<usize> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let n = chars.len();
    let marks = script::marks(text);
    let pending = script::unfinished_mark(text);
    // What to do about a cut at `pos`: `None` passes over it, `Some(None)`
    // waits for more text, `Some(Some(pos))` cuts there.
    let decide = |pos: usize| -> Option<Option<usize>> {
        if marks.iter().any(|m| m.start < pos && pos < m.end) {
            return None;
        }
        if !last && pending.is_some_and(|open| open < pos) {
            return Some(None);
        }
        Some(Some(pos))
    };
    let mut i = 0;
    while i < n {
        let (at, c) = chars[i];
        match c {
            '\n' => {
                if !text[..at].trim().is_empty()
                    && let Some(cut) = decide(at)
                {
                    return cut;
                }
                i += 1;
            }
            '。' | '！' | '？' => {
                if let Some(cut) = decide(at + c.len_utf8()) {
                    return cut;
                }
                i += 1;
            }
            '.' | '!' | '?' | '…' => {
                let mut j = i;
                while j < n && matches!(chars[j].1, '.' | '!' | '?' | '…') {
                    j += 1;
                }
                let mut k = j;
                while k < n
                    && matches!(
                        chars[k].1,
                        '"' | '\'' | '”' | '’' | ')' | ']' | '}' | '*' | '_' | '`'
                    )
                {
                    k += 1;
                }
                if k == n {
                    // Whether it is an end depends on what comes next.
                    return if last { Some(text.len()) } else { None };
                }
                if chars[k].1.is_whitespace() {
                    let ends = if j - i == 1 && c == '.' { dot_ends(text, at) } else { Dot::End };
                    let cut = match ends {
                        Dot::End => decide(chars[k].0),
                        Dot::NotEnd => None,
                        Dot::Ask => match text[chars[k].0..].trim_start().chars().next() {
                            Some(next) if next.is_uppercase() => decide(chars[k].0),
                            Some(_) => None,
                            None if last => decide(chars[k].0),
                            None => Some(None),
                        },
                    };
                    if let Some(cut) = cut {
                        return cut;
                    }
                }
                i = k.max(i + 1);
            }
            _ => i += 1,
        }
    }
    if last && !text.trim().is_empty() { Some(text.len()) } else { None }
}

/// `text` as it should be read aloud: no markdown, links spoken by name.
///
/// Headings, bullets, numbering, quote marks and table pipes come off; emphasis
/// and code ticks come off the words they surround; `[text](url)` is `text`; a
/// bare address is its host ("https://www.example.com/a" is "example.com");
/// pictures and emoji are dropped. Ordinary punctuation stays. Lines that
/// were separate (list items, headings, table rows) are said as separate
/// sentences, since there is no other way for a voice to pause between them.
pub fn speakable(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut in_fence = false;
    for raw in text.lines() {
        let line = raw.trim();
        if starts_fence(line) {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(plain) = plain_line(line) {
            lines.push(plain);
        }
    }
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            if !ends_a_sentence(&out) {
                out.push('.');
            }
            out.push(' ');
        }
        out.push_str(line);
    }
    out
}

/// Whether `text` ends in a mark that already gives a voice its pause, closing
/// quotes and brackets aside.
fn ends_a_sentence(text: &str) -> bool {
    let trimmed = text.trim_end_matches(['"', '\'', '”', '’', ')', ']', '}']);
    matches!(
        trimmed.chars().last(),
        Some('.' | '!' | '?' | ':' | ';' | ',' | '…' | '。' | '！' | '？')
    )
}

/// One line without its markdown, or `None` when nothing is left to say.
fn plain_line(line: &str) -> Option<String> {
    if line.is_empty() {
        return None;
    }
    // A rule, or the line under a table's header.
    let marks = |c: char| matches!(c, '-' | '*' | '_' | '=' | '|' | ':' | '+') || c.is_whitespace();
    if line.chars().all(marks) && line.chars().filter(|c| !c.is_whitespace()).count() >= 3 {
        return None;
    }
    let mut s = line;
    while let Some(rest) = s.strip_prefix('>') {
        s = rest.trim_start();
    }
    if s.starts_with('#') {
        let hashes = s.chars().take_while(|&c| c == '#').count();
        if hashes <= 6 && s[hashes..].starts_with(' ') {
            s = s[hashes..].trim_start();
        }
    }
    for bullet in ["- ", "* ", "+ ", "• "] {
        if let Some(rest) = s.strip_prefix(bullet) {
            s = rest.trim_start();
            break;
        }
    }
    let digits = s.chars().take_while(char::is_ascii_digit).count();
    if digits > 0
        && let Some(rest) = s[digits..].strip_prefix(['.', ')'])
        && rest.starts_with(' ')
    {
        s = rest.trim_start();
    }
    for checkbox in ["[ ] ", "[x] ", "[X] "] {
        if let Some(rest) = s.strip_prefix(checkbox) {
            s = rest.trim_start();
            break;
        }
    }
    let cells;
    if s.starts_with('|') || s.ends_with('|') {
        cells =
            s.split('|').map(str::trim).filter(|c| !c.is_empty()).collect::<Vec<_>>().join(", ");
        s = &cells;
    }
    let spoken = inline(s);
    if spoken.is_empty() { None } else { Some(spoken) }
}

/// Markdown within a line, and emoji.
fn inline(text: &str) -> String {
    let no_links = strip_links(text);
    let no_urls = host_names(&no_links);
    let chars: Vec<char> = no_urls.chars().collect();
    let mut out = String::with_capacity(no_urls.len());
    for (i, &c) in chars.iter().enumerate() {
        let before = i.checked_sub(1).map(|p| chars[p]);
        let after = chars.get(i + 1).copied();
        match c {
            '`' | '*' => {}
            '~' if before == Some('~') || after == Some('~') => {}
            '\\' if after.is_some_and(|a| a.is_ascii_punctuation()) => {}
            '_' => {
                // Inside a word it is a space ("read_web_page"); round one it
                // is emphasis.
                if before.is_some_and(char::is_alphanumeric)
                    && after.is_some_and(char::is_alphanumeric)
                {
                    out.push(' ');
                }
            }
            c if is_emoji(c) => {}
            c if c.is_whitespace() => out.push(' '),
            c => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Emoji, pictographs and the marks that make or join them.
fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF
        | 0x2600..=0x27BF
        | 0x2B00..=0x2BFF
        | 0x23E9..=0x23FA
        | 0xFE00..=0xFE0F
        | 0x200D
        | 0x20E3
    )
}

/// `[text](url)` as `text`, and `![alt](url)` as nothing.
fn strip_links(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let is_image = chars[i] == '!' && chars.get(i + 1) == Some(&'[');
        let open = if is_image { i + 1 } else { i };
        if chars[open] == '['
            && let Some((label, end)) = link_at(&chars, open)
        {
            if !is_image {
                out.push_str(&strip_links(&label));
            }
            i = end;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// The label and end of the link `[label](target)` opening at `open`.
fn link_at(chars: &[char], open: usize) -> Option<(String, usize)> {
    let mut depth = 0usize;
    let mut close = None;
    for (i, &c) in chars.iter().enumerate().skip(open) {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let mut depth = 0usize;
    for (i, &c) in chars.iter().enumerate().skip(close + 1) {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((chars[open + 1..close].iter().collect(), i + 1));
                }
            }
            _ => {}
        }
    }
    None
}

/// Each bare address in `text` as its host name, without `www.`.
fn host_names(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let starts_word = i == 0 || !chars[i - 1].is_alphanumeric();
        if starts_word && let Some(len) = address_len(&chars[i..]) {
            let mut end = i + len;
            let address: String = chars[i..end].iter().collect();
            // An address in angle brackets: take the brackets with it.
            if out.ends_with('<') && chars.get(end) == Some(&'>') {
                out.pop();
                end += 1;
            }
            out.push_str(&host_of(&address));
            i = end;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// How many characters of `chars` are an address, if it starts with one,
/// leaving off the punctuation that follows it in a sentence.
fn address_len(chars: &[char]) -> Option<usize> {
    let word: String = chars.iter().take(8).collect::<String>().to_lowercase();
    let scheme = word.starts_with("http://") || word.starts_with("https://");
    let www = word.starts_with("www.") && chars.get(4).is_some_and(|c| c.is_alphanumeric());
    if !scheme && !www {
        return None;
    }
    let mut depth = 0usize;
    let mut len = 0;
    for &c in chars {
        match c {
            c if c.is_whitespace() => break,
            '<' | '>' | '"' | '\'' | '*' | '[' | ']' | '{' | '}' => break,
            '(' => depth += 1,
            ')' if depth == 0 => break,
            ')' => depth -= 1,
            _ => {}
        }
        len += 1;
    }
    while len > 0 && matches!(chars[len - 1], '.' | ',' | ';' | ':' | '!' | '?' | '_') {
        len -= 1;
    }
    Some(len)
}

/// The host of an address, lower case, without `www.`, user or port.
fn host_of(address: &str) -> String {
    let lower = address.to_lowercase();
    let rest =
        lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://")).unwrap_or(&lower);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(parts: &[&str]) -> (Vec<String>, Option<String>) {
        let mut splitter = SentenceSplitter::new();
        let mut out = Vec::new();
        for part in parts {
            out.extend(splitter.push(part));
        }
        (out, splitter.finish())
    }

    fn all(parts: &[&str]) -> Vec<String> {
        let (mut out, rest) = split(parts);
        out.extend(rest);
        out
    }

    // ---- speakable --------------------------------------------------------

    #[test]
    fn markdown_marks_come_off() {
        assert_eq!(speakable("## Heading here"), "Heading here");
        assert_eq!(speakable("**Bold** and *italic* and `code`"), "Bold and italic and code");
        assert_eq!(speakable("__strong__ and _soft_ and ~~gone~~"), "strong and soft and gone");
        assert_eq!(speakable("> quoted text"), "quoted text");
        assert_eq!(speakable("> > nested"), "nested");
    }

    #[test]
    fn list_items_are_said_as_separate_sentences() {
        assert_eq!(speakable("- item one\n- item two"), "item one. item two");
        assert_eq!(speakable("1. First\n2) Second\n* Third"), "First. Second. Third");
        assert_eq!(speakable("- [x] done thing\n- [ ] open thing"), "done thing. open thing");
    }

    #[test]
    fn a_line_that_already_ends_in_a_stop_gets_no_second_one() {
        assert_eq!(speakable("One.\nTwo?\nThree!"), "One. Two? Three!");
        assert_eq!(speakable("He said \"go.\"\nThen left"), "He said \"go.\" Then left");
    }

    #[test]
    fn a_table_is_read_row_by_row() {
        let table = "| Name | Age |\n|------|-----|\n| Ann | 31 |";
        assert_eq!(speakable(table), "Name, Age. Ann, 31");
    }

    #[test]
    fn rules_are_not_said() {
        assert_eq!(speakable("above\n---\nbelow"), "above. below");
        assert_eq!(speakable("***"), "");
    }

    #[test]
    fn a_link_is_its_text_and_an_image_is_nothing() {
        assert_eq!(speakable("[the docs](https://example.com/docs) say so"), "the docs say so");
        assert_eq!(speakable("![a chart](https://x.com/c.png) Here it is"), "Here it is");
        assert_eq!(
            speakable("see [wiki](https://en.wikipedia.org/wiki/Foo_(bar)) now"),
            "see wiki now"
        );
        assert_eq!(speakable("[text] and (parens)"), "[text] and (parens)");
    }

    #[test]
    fn a_link_whose_text_is_an_address_is_its_host() {
        assert_eq!(speakable("[https://www.example.com/a](https://example.com/a)"), "example.com");
    }

    #[test]
    fn a_bare_address_is_its_host_without_www() {
        assert_eq!(speakable("See https://www.example.com/a/b."), "See example.com.");
        assert_eq!(speakable("See http://Example.org:8080/x?y=1#z now"), "See example.org now");
        assert_eq!(speakable("Try www.example.com/page, then stop"), "Try example.com, then stop");
        assert_eq!(speakable("(see https://example.com/a_b)"), "(see example.com)");
        assert_eq!(speakable("<https://example.com/a>"), "example.com");
        assert_eq!(
            speakable("https://en.wikipedia.org/wiki/Foo_(bar) works"),
            "en.wikipedia.org works"
        );
    }

    #[test]
    fn emoji_are_dropped() {
        assert_eq!(speakable("Great \u{1F389} job \u{2705}"), "Great job");
        assert_eq!(speakable("\u{1F44D}\u{1F3FD} ok"), "ok");
        assert_eq!(speakable("\u{1F389}"), "");
    }

    #[test]
    fn whitespace_is_collapsed() {
        assert_eq!(speakable("  a   b \t c  "), "a b c");
        assert_eq!(speakable("a\u{a0}b"), "a b");
        assert_eq!(speakable(""), "");
        assert_eq!(speakable("   \n  "), "");
    }

    #[test]
    fn ordinary_punctuation_stays() {
        let text = "Costs $3.5 (about 50%), e.g. five; maybe: \"yes\" - no? Yes!";
        assert_eq!(speakable(text), text);
    }

    #[test]
    fn underscores_inside_words_are_spaces() {
        assert_eq!(speakable("the read_web_page tool"), "the read web page tool");
        assert_eq!(speakable("__init__"), "init");
    }

    #[test]
    fn fenced_code_is_not_said() {
        assert_eq!(speakable("before\n```rust\nlet x = 1;\n```\nafter"), "before. after");
    }

    #[test]
    fn a_hash_that_is_not_a_heading_stays() {
        assert_eq!(speakable("#hashtag and C#"), "#hashtag and C#");
    }

    // ---- the splitter -----------------------------------------------------

    #[test]
    fn sentences_come_out_as_they_complete() {
        let mut splitter = SentenceSplitter::new();
        assert_eq!(
            splitter.push("The weather is fine today. How are"),
            ["The weather is fine today."]
        );
        assert_eq!(splitter.push(" you doing? Good."), ["How are you doing?"]);
        assert_eq!(splitter.finish().as_deref(), Some("Good."));
    }

    #[test]
    fn nothing_is_decided_at_the_very_end_of_what_has_arrived() {
        let mut splitter = SentenceSplitter::new();
        assert!(splitter.push("The price went up to 3.").is_empty());
        assert_eq!(
            splitter.push("5 dollars today. Then"),
            ["The price went up to 3.5 dollars today."]
        );
    }

    #[test]
    fn one_character_at_a_time_gives_the_same_sentences() {
        let text = "The first sentence is here. The second one is also long. Third and last one!";
        let parts: Vec<String> = text.chars().map(String::from).collect();
        let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
        assert_eq!(
            all(&parts),
            ["The first sentence is here.", "The second one is also long.", "Third and last one!"]
        );
    }

    #[test]
    fn decimals_are_not_cut() {
        let found = all(&["It is 3.5 degrees outside today. Next up is the weekend forecast."]);
        assert_eq!(found, ["It is 3.5 degrees outside today.", "Next up is the weekend forecast."]);
    }

    #[test]
    fn abbreviations_are_not_cut() {
        let found = all(&["I asked Dr. Smith about it, e.g. the cost. Then I left for the day."]);
        assert_eq!(
            found,
            ["I asked Dr. Smith about it, e.g. the cost.", "Then I left for the day."]
        );
        let found = all(&["Mr. and Mrs. Jones met Ms. Lee vs. the others. All of them stayed."]);
        assert_eq!(
            found,
            ["Mr. and Mrs. Jones met Ms. Lee vs. the others.", "All of them stayed."]
        );
        let found = all(&["He works for the U.S. government at night. That is all of it."]);
        assert_eq!(found, ["He works for the U.S. government at night.", "That is all of it."]);
    }

    #[test]
    fn etc_ends_a_sentence_only_before_a_capital() {
        let found =
            all(&["We bought apples, pears, etc. and then left the shop. Next we drove home."]);
        assert_eq!(
            found,
            ["We bought apples, pears, etc. and then left the shop.", "Next we drove home."]
        );
        let found = all(&["We bought apples, pears, etc. Then we drove all the way home."]);
        assert_eq!(found, ["We bought apples, pears, etc.", "Then we drove all the way home."]);
    }

    #[test]
    fn initials_are_not_cut() {
        let found = all(&["J. K. Rowling wrote the books. The next one is longer."]);
        assert_eq!(found, ["J. K. Rowling wrote the books.", "The next one is longer."]);
        // "I." is a word, not an initial.
        let found = all(&["Neither of them knew but I. Then everybody else went home."]);
        assert_eq!(found, ["Neither of them knew but I.", "Then everybody else went home."]);
    }

    #[test]
    fn addresses_are_not_cut() {
        let found = all(&[
            "Visit https://example.com/a.b/c?x=1. Then go to www.example.org/page for more.",
        ]);
        assert_eq!(found, ["Visit example.com.", "Then go to example.org for more."]);
    }

    #[test]
    fn a_list_number_is_not_a_sentence() {
        let found = all(&["1. Eggs and some milk\n2. Bread and some butter\n"]);
        assert_eq!(found, ["Eggs and some milk¶", "Bread and some butter¶"]);
    }

    #[test]
    fn a_newline_ends_a_sentence() {
        let (out, rest) =
            split(&["First line here is long enough\nSecond line is also long enough\nThi"]);
        // Each ended a line, which the voice pauses longer after.
        assert_eq!(out, ["First line here is long enough¶", "Second line is also long enough¶"]);
        assert_eq!(rest.as_deref(), Some("Thi"));
    }

    #[test]
    fn closing_quotes_and_brackets_stay_with_the_sentence() {
        let found = all(&["She said \"it works.\" Then she left (quickly!) and never came back."]);
        assert_eq!(
            found,
            ["She said \"it works.\"", "Then she left (quickly!)", "and never came back."]
        );
    }

    #[test]
    fn a_short_fragment_joins_the_next_sentence() {
        let mut splitter = SentenceSplitter::new();
        assert_eq!(
            splitter.push("OK. Let me check that for you. Done"),
            ["OK. Let me check that for you."]
        );
        assert_eq!(splitter.finish().as_deref(), Some("Done"));
    }

    #[test]
    fn a_short_fragment_waits_for_what_follows_it() {
        let mut splitter = SentenceSplitter::new();
        assert!(splitter.push("OK. ").is_empty());
        assert!(splitter.push("Let me check").is_empty());
        assert_eq!(splitter.push(" that for you. "), ["OK. Let me check that for you."]);
    }

    #[test]
    fn a_short_reply_is_said_whole_when_it_ends() {
        assert_eq!(all(&["OK."]), ["OK."]);
        assert_eq!(all(&["Yes. Done."]), ["Yes. Done."]);
    }

    #[test]
    fn a_long_unfinished_sentence_is_cut_at_a_comma() {
        let text = "alpha, ".repeat(60);
        let mut splitter = SentenceSplitter::new();
        let out = splitter.push(&text);
        assert_eq!(out.len(), 1);
        assert!(out[0].chars().count() <= MAX_SENTENCE, "{}", out[0].chars().count());
        assert!(out[0].ends_with("alpha,"), "{}", out[0]);
        let rest = splitter.finish().unwrap();
        assert!(rest.starts_with("alpha"));
    }

    #[test]
    fn a_long_finished_sentence_is_cut_at_a_space() {
        let text = format!("{}end.", "word ".repeat(60));
        let found = all(&[text.as_str()]);
        assert!(found.len() >= 2);
        assert!(found.iter().all(|s| s.chars().count() <= MAX_SENTENCE));
        assert_eq!(found.join(" "), text);
    }

    #[test]
    fn something_with_no_spaces_at_all_is_cut_at_the_limit() {
        let text = "x".repeat(300);
        let found = all(&[text.as_str()]);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].chars().count(), MAX_SENTENCE);
        assert_eq!(found.concat(), text);
    }

    #[test]
    fn fenced_code_is_skipped_entirely() {
        let found = all(&[
            "Here is the code you asked for:\n```rust\nfn main() {}\n```\nThat should do it. Anything else?",
        ]);
        assert_eq!(
            found,
            ["Here is the code you asked for:¶", "That should do it.", "Anything else?"]
        );
    }

    #[test]
    fn a_fence_split_across_pieces_is_skipped_too() {
        let mut splitter = SentenceSplitter::new();
        assert_eq!(splitter.push("Look at this:\n```\nlet x"), ["Look at this:¶"]);
        assert!(splitter.push(" = 1; // not. said. aloud.\n").is_empty());
        assert!(splitter.push("```\nDone then").is_empty());
        assert_eq!(splitter.finish().as_deref(), Some("Done then"));
    }

    #[test]
    fn an_unclosed_fence_says_nothing_even_at_the_end() {
        let mut splitter = SentenceSplitter::new();
        assert_eq!(splitter.push("Try this out now:\n```\nrm -rf"), ["Try this out now:¶"]);
        assert_eq!(splitter.finish(), None);
    }

    #[test]
    fn markdown_is_stripped_from_what_comes_out() {
        let found = all(&[
            "**Heads up:** see [the docs](https://example.com/x) for more detail. \u{1F389} Enjoy the rest.",
        ]);
        assert_eq!(found, ["Heads up: see the docs for more detail.", "Enjoy the rest."]);
    }

    #[test]
    fn nothing_worth_saying_is_none() {
        assert_eq!(SentenceSplitter::new().finish(), None);
        let mut splitter = SentenceSplitter::new();
        splitter.push("   \n ");
        assert_eq!(splitter.finish(), None);
        let mut splitter = SentenceSplitter::new();
        splitter.push("\u{1F389}\u{1F389}");
        assert_eq!(splitter.finish(), None);
    }

    #[test]
    fn nothing_comes_out_twice() {
        let mut splitter = SentenceSplitter::new();
        assert_eq!(
            splitter.push("The first sentence is long enough. "),
            ["The first sentence is long enough."]
        );
        assert!(splitter.push("").is_empty());
        assert_eq!(splitter.finish(), None);
        assert_eq!(splitter.finish(), None);
    }

    #[test]
    fn a_replacement_carries_on_from_what_was_handed_out() {
        let mut splitter = SentenceSplitter::new();
        let first = splitter.push("The first sentence is here. The second sentence is here. Third");
        assert_eq!(first, ["The first sentence is here.", "The second sentence is here."]);
        let more = splitter.replace(
            "The first sentence is here. The second sentence is here. The third sentence is new. And more.",
        );
        assert_eq!(more, ["The third sentence is new."]);
        assert_eq!(splitter.finish().as_deref(), Some("And more."));
    }

    #[test]
    fn a_replacement_that_is_shorter_hands_out_nothing_until_it_grows_past() {
        let mut splitter = SentenceSplitter::new();
        splitter.push("The first sentence is here. The second sentence is here. ");
        assert!(splitter.replace("The first sentence is here.").is_empty());
        assert!(splitter.push(" The second sentence is here.").is_empty());
        let more = splitter.push(" The third one comes after that. Tail");
        assert_eq!(more, ["The third one comes after that."]);
        assert_eq!(splitter.finish().as_deref(), Some("Tail"));
    }

    #[test]
    fn replacing_before_anything_was_handed_out_starts_afresh() {
        let mut splitter = SentenceSplitter::new();
        assert!(splitter.push("Half a sentence").is_empty());
        assert_eq!(
            splitter.replace("A whole new sentence here. And"),
            ["A whole new sentence here."]
        );
    }

    #[test]
    fn a_replacement_with_other_characters_never_cuts_one_in_two() {
        let mut splitter = SentenceSplitter::new();
        splitter.push("Caf\u{e9} au lait is nice. Another one is here. ");
        // Shorter in characters, longer in bytes: must not panic.
        assert!(splitter.replace("\u{4f60}\u{597d}").is_empty());
        let more = splitter.replace(
            "Caf\u{e9} au lait is nice. Another one is here. And a third sentence follows. ",
        );
        assert_eq!(more, ["And a third sentence follows."]);
    }

    #[test]
    fn a_whole_text_is_cut_the_way_a_streamed_one_is() {
        let text = "The first sentence is here. The second is a **bold** one! \
                    See [the site](https://www.example.com/a) for more detail?\n\n\
                    ```\ncode();\n```\n\nAnd then the end of it";
        let found = sentences(text);
        assert_eq!(found, all(&[text]));
        assert_eq!(
            found,
            [
                "The first sentence is here.",
                "The second is a bold one!",
                "See the site for more detail?¶",
                "And then the end of it",
            ]
        );
    }

    #[test]
    fn a_mark_is_kept_whole_and_never_cut_in_two() {
        let found = sentences("Say hi to {Siobhan|shiv-AWN} for me.");
        assert_eq!(found, ["Say hi to {Siobhan|shiv-AWN} for me."]);
        // Long enough to be cut, with the only spaces late on inside a mark.
        let long = format!("{} {{Saoirse Ronan|SEER-shuh ROH-nun}} too.", "word".repeat(55));
        for piece in sentences(&long) {
            assert_eq!(script::marks(&piece).len(), piece.matches('{').count(), "{piece}");
        }
    }

    #[test]
    fn a_full_stop_inside_a_mark_ends_nothing() {
        assert_eq!(
            sentences("We hiked {Mt. Fuji|mount FOO-jee} today. It was cold."),
            ["We hiked {Mt. Fuji|mount FOO-jee} today.", "It was cold."]
        );
        // Streamed: a mark still arriving is waited for, not cut.
        let mut splitter = SentenceSplitter::new();
        assert!(splitter.push("We hiked {Mt. Fu").is_empty());
        assert_eq!(
            splitter.push("ji|mount FOO-jee} today. And"),
            ["We hiked {Mt. Fuji|mount FOO-jee} today."]
        );
        // A brace that never closes is only a brace, and does not hold the
        // reply up for good.
        let mut splitter = SentenceSplitter::new();
        assert!(splitter.push("Odd {x. Yes").is_empty());
        assert_eq!(splitter.finish().as_deref(), Some("Odd {x. Yes"));
    }

    #[test]
    fn a_retried_step_that_now_ends_a_line_is_not_said_again() {
        // The retry differs before the sentence (a doubled space), so it is
        // made again -- and this time a line ends after it.
        let mut splitter = SentenceSplitter::new();
        assert_eq!(
            splitter.push("Well, the dentist is at ten. Then"),
            ["Well, the dentist is at ten."]
        );
        assert_eq!(
            splitter.replace("Well,  the dentist is at ten.\nThen lunch with Sam at noon. And"),
            ["Then lunch with Sam at noon."]
        );
    }

    #[test]
    fn a_line_end_is_marked_however_the_line_ended() {
        assert_eq!(
            sentences("You have three things today.\nFirst, the dentist at ten.  \nThen lunch."),
            ["You have three things today.¶", "First, the dentist at ten.¶", "Then lunch."]
        );
        // Not between two sentences of one line.
        assert_eq!(
            sentences("One thing here. And another one."),
            ["One thing here.", "And another one."]
        );
    }

    #[test]
    fn nothing_to_say_is_no_sentences() {
        assert!(sentences("").is_empty());
        assert!(sentences("  \n\n ").is_empty());
        assert!(sentences("```\nonly code\n```").is_empty());
    }

    #[test]
    fn a_text_with_no_full_stop_is_still_said() {
        assert_eq!(sentences("A line with no end to it"), ["A line with no end to it"]);
    }

    #[test]
    fn a_retried_step_in_other_words_is_said_from_its_start() {
        let mut splitter = SentenceSplitter::new();
        assert_eq!(
            splitter.push("The first sentence is here. Second"),
            ["The first sentence is here."]
        );
        assert!(splitter.replace("").is_empty());
        let said = splitter.push("Here is the whole answer now. More follows after it. ");
        assert_eq!(said, ["Here is the whole answer now.", "More follows after it."]);
    }

    #[test]
    fn a_retried_step_in_the_same_words_is_not_said_twice() {
        let mut splitter = SentenceSplitter::new();
        assert_eq!(
            splitter.push("The first sentence is here. Second"),
            ["The first sentence is here."]
        );
        assert!(splitter.replace("").is_empty());
        let said = splitter.push("The first sentence is here. Second one comes next. ");
        assert_eq!(said, ["Second one comes next."]);
    }
}
