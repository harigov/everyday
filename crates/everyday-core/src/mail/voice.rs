//! How a person writes, read from their own sent mail -- so a reply the
//! model drafts for them reads like theirs rather than like a model's.
//!
//! Everything here is pure and synchronous: no I/O, no model call, nothing
//! that needs a vault open. [`own_words`] strips a sent message down to
//! what the person actually typed -- no quoted reply, no "On ... wrote:"
//! attribution, no Outlook header block, no signature -- and [`analyze`]
//! turns a handful of those into a [`VoiceProfile`]: a greeting, a
//! sign-off, how long their sentences run, whether they use emoji, em
//! dashes or contractions. [`VoiceProfile::style_guide`] turns that back
//! into plain instructions a model can follow, and [`humanize`] is the
//! last line of defence on the way out: it cleans the tells in
//! [`AI_TELLS`] out of whatever the model actually wrote, and swaps em
//! dashes for what the person would have written instead when their own
//! mail never uses one. [`text_to_html`] is the one bit of formatting
//! glue both directions need -- plain text to the `<p>`/`<br>` HTML a
//! draft body holds.
//!
//! `everyday_service::mailvoice` is what calls all of this against a real
//! vault; everything below is tested without one.

use serde::{Deserialize, Serialize};

/// What a person's own sent mail says about how they write. Every field is
/// measured locally from the text of messages they sent -- nothing here is a
/// model's opinion.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VoiceProfile {
    /// How many messages this was read from. Zero means "nothing known";
    /// [`VoiceProfile::style_guide`] says nothing for such a profile.
    pub samples: u32,
    /// The opening they use most, with the other person's first name
    /// replaced by `{name}` -- `"Hi {name},"`, `"Hey {name} -"`. `None`
    /// when they usually open with no greeting at all.
    pub greeting: Option<String>,
    /// The closing they use most, as it appears (may span two lines --
    /// `"Thanks,\nHari"`). `None` when they usually just stop.
    pub sign_off: Option<String>,
    /// Median length of one of their messages, in words.
    pub median_words: u32,
    /// Median length of one of their sentences, in words.
    pub median_sentence_words: u32,
    /// Most of their sentences start in lowercase.
    pub lowercase_starts: bool,
    /// They use exclamation marks at all, with any regularity.
    pub uses_exclamations: bool,
    /// They use emoji with any regularity.
    pub uses_emoji: bool,
    /// They use em dashes (`—`) with any regularity.
    pub uses_em_dash: bool,
    /// They write "I'm", "don't", "can't" rather than "I am", "do not".
    pub uses_contractions: bool,
    /// They use bulleted or numbered lists with any regularity.
    pub uses_lists: bool,
}

/// Phrases that mark text as a model's rather than a person's. Every
/// writing prompt forbids them, and [`humanize`] removes the ones that slip
/// through anyway -- but only the sentence they sit in when that sentence
/// is pure filler; see that function's own docs for the line it draws.
pub const AI_TELLS: &[&str] = &[
    "i hope this email finds you well",
    "i hope this message finds you well",
    "i trust this email finds you well",
    "i hope you're doing well",
    "i hope you are doing well",
    "i hope all is well",
    "thank you for reaching out",
    "thanks for reaching out",
    "please don't hesitate to",
    "please do not hesitate to",
    "do not hesitate to contact me",
    "feel free to reach out",
    "i wanted to reach out",
    "i am writing to",
    "i'm writing to",
    "i wanted to follow up",
    "just wanted to circle back",
    "delve",
    "rest assured",
    "i appreciate your patience",
    "at your earliest convenience",
    "great question",
    "certainly!",
    "absolutely!",
    "happy to help!",
    "i'd be happy to",
    "i would be happy to",
    "looking forward to hearing from you",
    "let me know if you have any questions",
    "let me know if there's anything else",
    "should you have any questions",
    "hope this helps",
    "as an ai",
];

/// Read a [`VoiceProfile`] from the person's own words -- each sample
/// already passed through [`own_words`].
///
/// Every field but [`VoiceProfile::samples`] is a measurement over
/// `samples`, never a guess filled in when the evidence is thin: a
/// greeting or sign-off only survives into the profile when the same
/// pattern recurs in at least 40% of what was read, and every boolean is a
/// plain frequency against a fixed bar -- see each field's own doc comment
/// for the number. `samples.is_empty()` answers with
/// [`VoiceProfile::default`] rather than a profile full of false and zero
/// that would otherwise be indistinguishable from "measured, and the
/// answer was no".
pub fn analyze(samples: &[&str]) -> VoiceProfile {
    if samples.is_empty() {
        return VoiceProfile::default();
    }
    let total = samples.len();

    let greeting = most_common(samples.iter().map(|s| greeting_candidate(s)).collect());
    let sign_off = most_common(samples.iter().map(|s| sign_off_candidate(s)).collect());

    let median_words = median(samples.iter().map(|s| word_count(s) as u32).collect());

    let mut sentence_word_counts = Vec::new();
    let mut lowercase_sentences = 0usize;
    let mut determinable_sentences = 0usize;
    for sample in samples {
        for sentence in split_sentences(sample) {
            let words = word_count(sentence) as u32;
            if words == 0 {
                continue;
            }
            sentence_word_counts.push(words);
            if let Some(first_alpha) = sentence.chars().find(|c| c.is_alphabetic()) {
                determinable_sentences += 1;
                if first_alpha.is_lowercase() {
                    lowercase_sentences += 1;
                }
            }
        }
    }
    let median_sentence_words = median(sentence_word_counts);
    let lowercase_starts = determinable_sentences > 0
        && (lowercase_sentences as f64 / determinable_sentences as f64) > 0.6;

    let uses_exclamations = fraction_at_least(samples, 0.2, |s| s.contains('!'));
    let uses_emoji = fraction_at_least(samples, 0.15, |s| s.chars().any(is_emoji));
    let uses_em_dash = fraction_at_least(samples, 0.15, |s| s.contains('—'));
    let uses_lists = fraction_at_least(samples, 0.15, |s| s.lines().any(is_list_line));

    let (contractions, expansions) = count_contractions(samples);
    let uses_contractions = contractions > expansions;

    VoiceProfile {
        samples: total as u32,
        greeting,
        sign_off,
        median_words,
        median_sentence_words,
        lowercase_starts,
        uses_exclamations,
        uses_emoji,
        uses_em_dash,
        uses_contractions,
        uses_lists,
    }
}

/// What fraction of `samples` satisfy `pred` -- the one shape every
/// frequency-gated boolean in [`analyze`] reduces to.
fn fraction_at_least(samples: &[&str], threshold: f64, pred: impl Fn(&str) -> bool) -> bool {
    let hits = samples.iter().filter(|s| pred(s)).count();
    (hits as f64 / samples.len() as f64) >= threshold
}

/// The part of a sent message the person actually wrote: the quoted message
/// below a reply ("On … wrote:", `>` lines, Outlook's "-----Original
/// Message-----" / "From: … Sent: …" header block) and a trailing `-- `
/// signature removed, whitespace trimmed.
///
/// A cut is found first -- the earliest line that starts a quoted reply, a
/// forwarded message, an Outlook header block or a signature -- and
/// everything from there on is dropped outright, since "and everything
/// after" is exactly what each of those markers means. What is left is
/// then filtered a second time, line by line, for the two things that do
/// *not* announce themselves with a marker: a bare `>`-quoted line (the
/// plain-text quoting style that carries no attribution at all) and a
/// mobile trailer ("Sent from my iPhone") that a phone's mail client tacks
/// on with no cut of its own, often *before* the quote it is attached to.
pub fn own_words(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let cut = cut_point(&lines);

    let mut out = String::with_capacity(text.len());
    for line in &lines[..cut] {
        if line.trim_start().starts_with('>') || is_trailer_line(line) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

/// The index of the first line in `lines` that starts a quoted reply, a
/// forwarded message, an Outlook header block or a signature --
/// `lines.len()` when nothing does, meaning the whole thing is the
/// person's own words.
fn cut_point(lines: &[&str]) -> usize {
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_lowercase();
        let is_marker = is_rule_line(trimmed)
            || lower.contains("original message")
            || lower.contains("forwarded message")
            || trimmed == "--";
        if is_marker || is_attribution_line(trimmed, &lower, lines.get(i + 1)) {
            return i;
        }
    }
    lines.len()
}

/// A horizontal rule Outlook's web client draws above a quoted header
/// block -- a run of underscores and nothing else.
fn is_rule_line(trimmed: &str) -> bool {
    trimmed.len() >= 8 && trimmed.chars().all(|c| c == '_')
}

/// "On Tue, 3 Sep 2026 at 10:02, Sam Lee <sam@x.com> wrote:" -- Gmail's and
/// Apple Mail's attribution line, which may wrap onto a second line when
/// the name and address are long, in which case it is that second line
/// which ends in "wrote:".
fn is_attribution_line(trimmed: &str, lower: &str, next: Option<&&str>) -> bool {
    if !trimmed.starts_with("On ") {
        return false;
    }
    if lower.ends_with("wrote:") {
        return true;
    }
    next.is_some_and(|n| n.trim().to_lowercase().ends_with("wrote:"))
}

/// "Sent from my iPhone", "Sent from Outlook for iOS", "Get Outlook for
/// Android" -- a mobile client's own trailer, which carries no cut of its
/// own and can sit above a quote rather than only at the very end.
fn is_trailer_line(line: &str) -> bool {
    let lower = line.trim().to_lowercase();
    lower.starts_with("sent from ") || lower.starts_with("get outlook for")
}

impl VoiceProfile {
    /// The profile as a short list of instructions for a prompt -- "Opens
    /// with 'Hi {name},'", "Keeps it to about 40 words", "Never uses
    /// exclamation marks". Empty when `samples == 0`.
    ///
    /// A boolean field only ever earns a bullet in the direction that
    /// corrects a model's own default: a model left to itself reaches for
    /// an em dash, an exclamation mark, an emoji and a bulleted list far
    /// more readily than most people write, so those four only speak up
    /// when the profile says *not* to; a model left to itself also
    /// defaults to spelling out "I am" rather than "I'm", so
    /// [`VoiceProfile::uses_contractions`] only speaks up when the profile
    /// says to use them. Silence, either way, means "the examples already
    /// show this -- nothing to correct."
    pub fn style_guide(&self) -> String {
        if self.samples == 0 {
            return String::new();
        }
        let mut lines: Vec<String> = Vec::new();

        match &self.greeting {
            Some(greeting) => {
                lines.push(format!("- Open with \"{greeting}\" using the recipient's first name."))
            }
            None => lines.push("- Don't open with a greeting; start with the point.".to_string()),
        }
        match &self.sign_off {
            Some(sign_off) => {
                lines.push(format!("- Close with:\n  {}", sign_off.replace('\n', "\n  ")))
            }
            None => lines.push("- Don't add a sign-off.".to_string()),
        }
        if self.median_words > 0 {
            lines.push(format!(
                "- Keep it to about {} words; their emails are short.",
                self.median_words
            ));
        }
        if self.median_sentence_words > 0 {
            lines.push(format!("- Short sentences, about {} words.", self.median_sentence_words));
        }
        if self.lowercase_starts {
            lines.push("- Write in lowercase, as they do.".to_string());
        }
        if !self.uses_exclamations {
            lines.push("- No exclamation marks.".to_string());
        }
        if !self.uses_emoji {
            lines.push("- No emoji.".to_string());
        }
        if !self.uses_em_dash {
            lines.push("- Never use em dashes (—).".to_string());
        }
        if self.uses_contractions {
            lines.push("- Use contractions (I'm, don't).".to_string());
        }
        if !self.uses_lists {
            lines.push("- No bullet points.".to_string());
        }

        lines.join("\n")
    }
}

/// Clean model-written text so it reads as the person's: drop every
/// [`AI_TELLS`] phrase (and the sentence it opens, when it is a filler
/// sentence), swap em dashes for what the person uses when the profile says
/// they never use one, strip a leading "Subject:" line or a trailing
/// placeholder signature ("[Your name]"), and trim.
///
/// Works a line at a time, and a quoted line (one starting with `>`) is
/// never touched by any of it -- not the tell removal, not the em-dash
/// swap, not the placeholder check -- since that text is somebody else's,
/// quoted back, not the model's own output to clean up.
pub fn humanize(text: &str, profile: &VoiceProfile) -> String {
    let without_subject = strip_subject_line(text);
    let tells = active_tells(profile);

    let mut lines: Vec<String> = Vec::new();
    for line in without_subject.lines() {
        if line.trim_start().starts_with('>') {
            lines.push(line.to_string());
            continue;
        }
        let mut line = strip_filler_sentences(line, &tells);
        if !profile.uses_em_dash {
            line = replace_em_dashes(&line);
        }
        if is_placeholder_signature(&line) {
            continue;
        }
        lines.push(line);
    }

    collapse_blank_lines(&lines.join("\n")).trim().to_string()
}

/// [`AI_TELLS`], minus any phrase the profile's own greeting or sign-off
/// happens to contain -- the one exception [`humanize`]'s contract makes,
/// so a person who actually signs off "rest assured, I'll handle it" never
/// has their own habitual closing mangled by a filter meant for a model's.
fn active_tells(profile: &VoiceProfile) -> Vec<&'static str> {
    let greeting = profile.greeting.as_deref().unwrap_or("").to_lowercase();
    let sign_off = profile.sign_off.as_deref().unwrap_or("").to_lowercase();
    AI_TELLS.iter().copied().filter(|t| !greeting.contains(*t) && !sign_off.contains(*t)).collect()
}

fn strip_subject_line(text: &str) -> String {
    let Some(first) = text.lines().next() else { return text.to_string() };
    if !first.trim_start().to_lowercase().starts_with("subject:") {
        return text.to_string();
    }
    text.lines().skip(1).collect::<Vec<_>>().join("\n")
}

/// Drop every sentence in `line` that is pure filler built around one of
/// `tells` -- the tell phrase removed, and what is left is two words or
/// fewer once stray punctuation is trimmed off it too. A sentence that
/// still has something to say once the tell is lifted out of it is kept
/// whole, tell and all: this is a decision about the *sentence*, never an
/// edit that rewrites part of one.
fn strip_filler_sentences(line: &str, tells: &[&str]) -> String {
    if tells.is_empty() {
        return line.to_string();
    }
    let lower_line = line.to_lowercase();
    if !tells.iter().any(|t| lower_line.contains(*t)) {
        return line.to_string();
    }

    let mut out = String::with_capacity(line.len());
    for (sentence, terminator) in sentence_spans(line) {
        let lower = sentence.to_lowercase();
        let keep_whole = match tells.iter().copied().find(|t| lower.contains(*t)) {
            None => true,
            Some(tell) => {
                let idx = lower.find(tell).unwrap_or(0);
                let start = floor_char_boundary(sentence, idx);
                let end = ceil_char_boundary(sentence, idx + tell.len());
                let mut remainder = String::new();
                remainder.push_str(&sentence[..start]);
                remainder.push_str(&sentence[end..]);
                let remainder = remainder.trim_matches(|c: char| {
                    c.is_whitespace() || matches!(c, ',' | '.' | '!' | '-' | ':')
                });
                word_count(remainder) > 2
            }
        };
        if keep_whole {
            out.push_str(sentence);
            out.push_str(terminator);
        }
    }
    out
}

/// Split `line` into `(sentence, terminator)` pairs that concatenate back
/// to `line` exactly -- so [`strip_filler_sentences`] can drop a whole
/// filler sentence, terminator included, without disturbing a single byte
/// around it. A terminator swallows a following close-quote or bracket
/// too ("...well\"." stays one sentence), and the last pair's terminator is
/// `""` when `line` ends mid-sentence, with no `.`/`!`/`?` of its own.
fn sentence_spans(line: &str) -> Vec<(&str, &str)> {
    let mut spans = Vec::new();
    let mut start = 0usize;
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c != '.' && c != '!' && c != '?' {
            continue;
        }
        let mut end = i + c.len_utf8();
        while let Some(&(j, next)) = chars.peek() {
            if matches!(next, '"' | '\'' | ')' | '.' | '!' | '?') {
                end = j + next.len_utf8();
                chars.next();
            } else {
                break;
            }
        }
        spans.push((&line[start..i], &line[i..end]));
        start = end;
    }
    if start < line.len() {
        spans.push((&line[start..], ""));
    }
    spans
}

/// The nearest char boundary at or before `idx` in `s`. A tell matched on
/// `s.to_lowercase()` never lands off a boundary of `s` itself for the
/// plain-ASCII tells in [`AI_TELLS`] (lower-casing ASCII never changes a
/// string's length), but a model can write anything, so this is here on
/// the same reasoning `crate::mail::records::Body::model_text`'s own
/// `safe_slice` is: a cut computed one way must never panic slicing a
/// string it is handed against.
fn floor_char_boundary(s: &str, idx: usize) -> usize {
    let idx = idx.min(s.len());
    (0..=idx).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0)
}

fn ceil_char_boundary(s: &str, idx: usize) -> usize {
    let idx = idx.min(s.len());
    (idx..=s.len()).find(|&i| s.is_char_boundary(i)).unwrap_or(s.len())
}

/// Em dashes, swapped for what a profile that never writes one would use
/// instead. A dash paired with a second one before the sentence ends reads
/// as a parenthetical aside -- "the house — which was empty — stood
/// there" -- and the closest ordinary punctuation to that job is a pair of
/// commas, so that is what a pair becomes. A dash with no partner is doing
/// a plainer job (a beat, a trailing thought), which an ordinary hyphen
/// reads fine for, so a lone one becomes `" - "`. `" -- "` (a model's other
/// common spelling of the same mark) is normalised to an em dash first, so
/// both spellings end up handled by the one rule.
fn replace_em_dashes(text: &str) -> String {
    let normalised = text.replace(" -- ", " — ").replace("--", " — ");
    let mut out = String::new();
    let mut rest = normalised.as_str();
    while let Some(first) = rest.find('—') {
        let before = &rest[..first];
        let after = &rest[first + '—'.len_utf8()..];
        let boundary = after.find(['.', '!', '?']).unwrap_or(after.len());
        let window = &after[..boundary];
        if let Some(second) = window.find('—') {
            out.push_str(before.trim_end());
            out.push(',');
            out.push_str(after[..second].trim_end());
            out.push(',');
            rest = &after[second + '—'.len_utf8()..];
        } else {
            out.push_str(before.trim_end());
            out.push_str(" - ");
            rest = after.trim_start();
        }
    }
    out.push_str(rest);
    out
}

/// A placeholder a model leaves for the person to sign -- never a real
/// bracketed gap the person meant to fill in themselves, which this does
/// not touch: only these three exact, case-insensitive strings, as a whole
/// line, are a model's own unfilled placeholder rather than an honest
/// "[fill this in]" marker the instruction asked for.
fn is_placeholder_signature(line: &str) -> bool {
    const PLACEHOLDERS: &[&str] = &["[your name]", "[name]", "<your name>"];
    PLACEHOLDERS.contains(&line.trim().to_lowercase().as_str())
}

fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = 0u32;
    for c in text.chars() {
        if c == '\n' {
            run += 1;
            if run <= 2 {
                out.push(c);
            }
        } else {
            run = 0;
            out.push(c);
        }
    }
    out
}

/// Plain text to the HTML a draft body holds: every character escaped, a
/// blank line starting a new `<p>`, a single newline a `<br>`.
pub fn text_to_html(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }

    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(line);
    }
    if !current.is_empty() {
        blocks.push(current);
    }

    blocks
        .into_iter()
        .map(|block| {
            let joined = block.iter().map(|l| escape_html(l)).collect::<Vec<_>>().join("<br>");
            format!("<p>{joined}</p>")
        })
        .collect::<Vec<_>>()
        .join("")
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

// ---- greeting / sign-off candidates ---------------------------------------

/// The first non-empty line, normalised to a greeting pattern, if it is
/// short enough and looks like one at all.
fn greeting_candidate(sample: &str) -> Option<String> {
    let first = sample.lines().map(str::trim).find(|l| !l.is_empty())?;
    if word_count(first) > 6 {
        return None;
    }
    normalize_greeting(first)
}

/// "Hi Sam," -> `Some("Hi {name},")`; "Sam," (no lead-in word at all) ->
/// `Some("{name},")`; anything else -> `None`. The lead-in's own case and
/// the punctuation that followed the name are both kept exactly as
/// written -- only the name itself is replaced.
fn normalize_greeting(line: &str) -> Option<String> {
    // Longer, more specific lead-ins first: "hiya" must be checked before
    // "hi", which is a prefix of it, or "Hiya Sam," would be misread as
    // "Hi" followed by the "name" "ya".
    const GREETING_LEAD_INS: &[&str] =
        &["good morning", "hello", "hiya", "morning", "dear", "hey", "hi"];

    let lower = line.to_lowercase();
    for lead in GREETING_LEAD_INS {
        if !lower.starts_with(*lead) {
            continue;
        }
        let after = &line[lead.len()..];
        if after.chars().next().is_some_and(|c| c.is_alphabetic()) {
            continue; // "Historical ..." is not "Hi" + a name
        }
        let lead_text = &line[..lead.len()];
        let rest = after.trim_start();
        let sep = &after[..after.len() - rest.len()];
        let name_end = rest.find(|c: char| !c.is_alphabetic()).unwrap_or(rest.len());
        if name_end == 0 {
            // A lead-in with nothing after it to call a name ("Hi," alone)
            // -- kept as written, with nothing to template.
            return Some(line.to_string());
        }
        let trailing = &rest[name_end..];
        return Some(format!("{lead_text}{sep}{{name}}{trailing}"));
    }

    // A bare first name on its own, followed by a comma -- "Sam," -- is a
    // greeting with no lead-in word at all.
    let trimmed = line.trim();
    let bare_name = trimmed.strip_suffix(',')?;
    if !bare_name.is_empty()
        && word_count(bare_name) == 1
        && bare_name.chars().all(char::is_alphabetic)
    {
        return Some("{name},".to_string());
    }
    None
}

/// The last one or two non-empty lines, kept exactly as written, if they
/// look like a closing.
fn sign_off_candidate(sample: &str) -> Option<String> {
    let lines: Vec<&str> = sample.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let last = *lines.last()?;

    let two_line = lines.len() >= 2
        && looks_like_closing_word(lines[lines.len() - 2])
        && (looks_like_bare_name(last) || looks_like_dash_name(last));
    if two_line {
        return Some(format!("{}\n{last}", lines[lines.len() - 2]));
    }

    if looks_like_closing_word(last) || looks_like_dash_name(last) || looks_like_bare_name(last) {
        return Some(last.to_string());
    }
    None
}

fn looks_like_closing_word(line: &str) -> bool {
    const CLOSING_WORDS: &[&str] =
        &["thanks", "thank you", "thx", "best", "cheers", "regards", "best regards", "warmly"];
    let stripped = line.trim_end_matches([',', '.', '!']).trim();
    CLOSING_WORDS.contains(&stripped.to_lowercase().as_str())
}

/// "- Hari" or "— Hari" -- a dash, then a short, name-shaped word or two.
fn looks_like_dash_name(line: &str) -> bool {
    let t = line.trim_start();
    let Some(rest) = t.strip_prefix('-').or_else(|| t.strip_prefix('—')) else { return false };
    let rest = rest.trim();
    !rest.is_empty()
        && word_count(rest) <= 3
        && rest.chars().all(|c| c.is_alphabetic() || c.is_whitespace() || c == '.')
}

/// A short, name-shaped line with no dash and no closing word of its own --
/// only promoted to a real sign-off by [`most_common`] actually recurring
/// across several of the person's own messages, which is what tells it
/// apart from a short sentence that happened to end a message once.
fn looks_like_bare_name(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty()
        && word_count(t) <= 3
        && t.chars().all(|c| c.is_alphabetic() || c.is_whitespace() || c == '.')
}

/// The most common `Some` value among `candidates`, if it makes up at
/// least 40% of `candidates.len()` -- the one rule [`VoiceProfile::greeting`]
/// and [`VoiceProfile::sign_off`] both follow. A plain `Vec` rather than a
/// hash map, so that a tie is broken the same way on every run rather than
/// however a hasher happens to order two keys.
fn most_common(candidates: Vec<Option<String>>) -> Option<String> {
    let total = candidates.len();
    if total == 0 {
        return None;
    }
    let mut counted: Vec<(String, u32)> = Vec::new();
    for candidate in candidates.into_iter().flatten() {
        match counted.iter_mut().find(|(value, _)| *value == candidate) {
            Some(entry) => entry.1 += 1,
            None => counted.push((candidate, 1)),
        }
    }
    let (best, count) = counted.into_iter().max_by_key(|(_, count)| *count)?;
    if (count as f64 / total as f64) >= 0.4 { Some(best) } else { None }
}

// ---- small measurements ---------------------------------------------------

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

fn median(mut values: Vec<u32>) -> u32 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        ((values[mid - 1] as u64 + values[mid] as u64) / 2) as u32
    } else {
        values[mid]
    }
}

/// `text`, split on `.`/`!`/`?`, each piece trimmed and empty pieces
/// dropped -- used only to measure sentence length and capitalisation in
/// [`analyze`], never to reconstruct the text (see [`sentence_spans`] for
/// that job).
fn split_sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, c) in text.char_indices() {
        if c == '.' || c == '!' || c == '?' {
            let seg = text[start..i].trim();
            if !seg.is_empty() {
                out.push(seg);
            }
            start = i + c.len_utf8();
        }
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

/// A character in one of the common emoji blocks -- emoticons, misc
/// symbols and pictographs, transport, the newer pictograph extensions,
/// and the dingbats/misc-symbols ranges older emoji (☕, ✅, ⭐) live in.
/// Not exhaustive of every code point Unicode now calls an emoji, but
/// enough to tell "this message has one" from "this message does not"
/// without pulling in a Unicode emoji data table for it.
fn is_emoji(c: char) -> bool {
    matches!(
        c as u32,
        0x1F300..=0x1F5FF
            | 0x1F600..=0x1F64F
            | 0x1F680..=0x1F6FF
            | 0x1F900..=0x1F9FF
            | 0x1FA70..=0x1FAFF
            | 0x2600..=0x26FF
            | 0x2700..=0x27BF
    )
}

/// How many contracted words ("I'm", "don't") versus expanded phrases ("I
/// am", "do not") appear across `samples`, pooled.
fn count_contractions(samples: &[&str]) -> (u32, u32) {
    const EXPANDED_PHRASES: &[&str] = &[
        "i am",
        "do not",
        "can not",
        "cannot",
        "it is",
        "i have",
        "i will",
        "will not",
        "did not",
        "is not",
        "that is",
        "you are",
        "we are",
        "they are",
        "was not",
        "were not",
        "have not",
        "has not",
        "would not",
        "could not",
        "should not",
    ];
    const CONTRACTION_SUFFIXES: &[&str] = &["'t", "'s", "'re", "'ve", "'ll", "'m", "'d"];

    let mut contractions = 0u32;
    let mut expansions = 0u32;
    for sample in samples {
        let lower = sample.to_lowercase();
        for word in lower.split_whitespace() {
            let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'');
            if trimmed.contains('\'')
                && CONTRACTION_SUFFIXES.iter().any(|suffix| trimmed.ends_with(*suffix))
            {
                contractions += 1;
            }
        }
        for phrase in EXPANDED_PHRASES {
            expansions += lower.matches(*phrase).count() as u32;
        }
    }
    (contractions, expansions)
}

fn is_list_line(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("• ") {
        return true;
    }
    let digits_end = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    digits_end > 0 && t[digits_end..].starts_with(". ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- own_words ---------------------------------------------------

    #[test]
    fn own_words_keeps_text_with_no_cuts_at_all() {
        assert_eq!(own_words("Just this."), "Just this.");
    }

    #[test]
    fn own_words_cuts_a_gmail_attribution_and_its_quote() {
        let text = "Thanks, sounds good.\n\nOn Tue, 3 Sep 2026 at 10:02, Sam Lee <sam@x.com> wrote:\n> Are we still on for Tuesday?\n> Let me know.";
        assert_eq!(own_words(text), "Thanks, sounds good.");
    }

    #[test]
    fn own_words_cuts_an_apple_mail_attribution() {
        let text = "Great, see you then.\n\nOn Sep 3, 2026, at 10:02 AM, Sam Lee <sam@x.com> wrote:\n\n> Hi, are we still on for Tuesday?";
        assert_eq!(own_words(text), "Great, see you then.");
    }

    #[test]
    fn own_words_cuts_an_attribution_wrapped_onto_a_second_line() {
        let text =
            "Works for me.\n\nOn Tue, 3 Sep 2026 at 10:02,\nSam Lee <sam@x.com> wrote:\n> quoted";
        assert_eq!(own_words(text), "Works for me.");
    }

    #[test]
    fn own_words_cuts_an_outlook_rule_and_header_block() {
        let text = "Sure, that works for me.\n\n________________________________\nFrom: Sam Lee <sam@x.com>\nSent: Tuesday, September 3, 2026 10:02 AM\nTo: Me <me@x.com>\nSubject: Meeting Tuesday?\n\nAre we still on for Tuesday?";
        assert_eq!(own_words(text), "Sure, that works for me.");
    }

    #[test]
    fn own_words_cuts_a_classic_outlook_original_message_marker() {
        let text = "Sounds good.\n\n-----Original Message-----\nFrom: Sam Lee <sam@x.com>\nSent: Tuesday, September 3, 2026 10:02 AM\nTo: Me <me@x.com>\nSubject: Meeting Tuesday?\n\nAre we still on for Tuesday?";
        assert_eq!(own_words(text), "Sounds good.");
    }

    #[test]
    fn own_words_cuts_a_forwarded_message_marker() {
        let text = "FYI, forwarding this.\n\n---------- Forwarded message ---------\nFrom: Sam Lee <sam@x.com>\nDate: Tue, Sep 3, 2026\nSubject: Meeting\n\nAre we on?";
        assert_eq!(own_words(text), "FYI, forwarding this.");
    }

    #[test]
    fn own_words_cuts_a_signature_delimiter() {
        let text = "Talk soon.\n\n-- \nHari Govardhanam\nCEO, Example Inc.";
        assert_eq!(own_words(text), "Talk soon.");
    }

    #[test]
    fn own_words_drops_a_mobile_trailer_with_no_marker_of_its_own() {
        assert_eq!(own_words("Sounds great!\nSent from my iPhone"), "Sounds great!");
    }

    #[test]
    fn own_words_drops_plain_quote_lines_with_no_attribution_at_all() {
        let text = "Sounds good to me.\n\n> Original question here\n> more context";
        assert_eq!(own_words(text), "Sounds good to me.");
    }

    // ---- analyze: greeting and sign-off --------------------------------

    #[test]
    fn analyze_normalises_a_recurring_hi_name_greeting_and_two_line_sign_off() {
        let s1 = "Hi Sam,\n\nSounds good, let's do Tuesday.\n\nThanks,\nHari";
        let s2 = "Hi Alex,\n\nWorks for me.\n\nThanks,\nHari";
        let s3 = "Hi Jordan,\n\nSee you then.\n\nThanks,\nHari";
        let s4 = "Quick note: let's push to Thursday instead.\n\nThanks,\nHari";
        let profile = analyze(&[s1, s2, s3, s4]);
        assert_eq!(profile.greeting, Some("Hi {name},".to_string()));
        assert_eq!(profile.sign_off, Some("Thanks,\nHari".to_string()));
        assert_eq!(profile.samples, 4);
    }

    #[test]
    fn analyze_normalises_a_bare_name_comma_greeting_and_finds_no_sign_off() {
        let s1 = "Sam,\n\nYes, Tuesday works.";
        let s2 = "Alex,\n\nWorks for me.";
        let s3 = "Random thought - let's push it.";
        let profile = analyze(&[s1, s2, s3]);
        assert_eq!(profile.greeting, Some("{name},".to_string()));
        assert_eq!(profile.sign_off, None);
    }

    #[test]
    fn analyze_keeps_a_dash_name_sign_off_as_written() {
        let s1 = "Hey,\n\nWorks for me.\n\n- Hari";
        let s2 = "Hey,\n\nSounds good.\n\n- Hari";
        let s3 = "Hey,\n\nTuesday then.\n\n- Hari";
        let profile = analyze(&[s1, s2, s3]);
        assert_eq!(profile.sign_off, Some("- Hari".to_string()));
    }

    #[test]
    fn analyze_finds_no_greeting_when_none_recurs() {
        let s1 = "Sounds good, Tuesday works.";
        let s2 = "No problem at all, see you then.";
        let profile = analyze(&[s1, s2]);
        assert_eq!(profile.greeting, None);
    }

    // ---- analyze: the boolean and numeric measurements ------------------

    #[test]
    fn analyze_detects_mostly_lowercase_sentence_starts() {
        let s1 = "hey, sounds good. let's do tuesday then.";
        let s2 = "no worries. i'll send it over.";
        let s3 = "sure thing. appreciate it.";
        assert!(analyze(&[s1, s2, s3]).lowercase_starts);
    }

    #[test]
    fn analyze_does_not_flag_lowercase_starts_for_ordinary_capitalisation() {
        let s1 = "Hey, sounds good. Let's do Tuesday then.";
        let s2 = "No worries. I'll send it over.";
        assert!(!analyze(&[s1, s2]).lowercase_starts);
    }

    #[test]
    fn analyze_flags_exclamations_at_the_twenty_percent_floor() {
        let samples = ["Great!", "ok", "ok", "ok", "ok"];
        assert!(analyze(&samples).uses_exclamations);
    }

    #[test]
    fn analyze_does_not_flag_exclamations_below_the_floor() {
        let samples = ["Great!", "ok", "ok", "ok", "ok", "ok"];
        assert!(!analyze(&samples).uses_exclamations);
    }

    #[test]
    fn analyze_flags_emoji_at_the_fifteen_percent_floor() {
        let mut samples = vec!["sounds good 😀", "sounds good 😀", "sounds good 😀"];
        samples.extend(std::iter::repeat_n("ok", 17));
        assert_eq!(samples.len(), 20);
        assert!(analyze(&samples).uses_emoji);
    }

    #[test]
    fn analyze_flags_em_dash_usage() {
        let samples = ["the plan — if it works — saves a week", "ok", "ok", "ok"];
        assert!(analyze(&samples).uses_em_dash);
    }

    #[test]
    fn analyze_does_not_flag_em_dash_usage_when_absent() {
        let samples = ["the plan, if it works, saves a week", "ok", "ok", "ok"];
        assert!(!analyze(&samples).uses_em_dash);
    }

    #[test]
    fn analyze_flags_contractions_when_they_outnumber_expansions() {
        let samples = ["I'm happy to help.", "Don't worry about it.", "I am going to check."];
        assert!(analyze(&samples).uses_contractions);
    }

    #[test]
    fn analyze_does_not_flag_contractions_when_expansions_win() {
        let samples = ["I am going.", "Do not worry.", "It is fine."];
        assert!(!analyze(&samples).uses_contractions);
    }

    #[test]
    fn analyze_flags_lists_at_the_fifteen_percent_floor() {
        let samples = ["- one\n- two", "ok", "ok", "ok"];
        assert!(analyze(&samples).uses_lists);
    }

    #[test]
    fn analyze_computes_medians() {
        let profile = analyze(&["one two three", "one two three four five"]);
        assert_eq!(profile.median_words, 4);
    }

    #[test]
    fn analyze_of_empty_input_is_the_default_profile() {
        assert_eq!(analyze(&[]), VoiceProfile::default());
    }

    // ---- style_guide ----------------------------------------------------

    #[test]
    fn style_guide_is_empty_for_a_profile_with_no_samples() {
        assert_eq!(VoiceProfile::default().style_guide(), "");
    }

    #[test]
    fn style_guide_renders_every_bullet_in_its_restrictive_direction() {
        let profile = VoiceProfile {
            samples: 10,
            greeting: Some("Hi {name},".to_string()),
            sign_off: Some("Thanks,\nHari".to_string()),
            median_words: 40,
            median_sentence_words: 9,
            lowercase_starts: false,
            uses_exclamations: false,
            uses_emoji: false,
            uses_em_dash: false,
            uses_contractions: true,
            uses_lists: false,
        };
        let expected = "\
- Open with \"Hi {name},\" using the recipient's first name.
- Close with:
  Thanks,
  Hari
- Keep it to about 40 words; their emails are short.
- Short sentences, about 9 words.
- No exclamation marks.
- No emoji.
- Never use em dashes (—).
- Use contractions (I'm, don't).
- No bullet points.";
        assert_eq!(profile.style_guide(), expected);
    }

    #[test]
    fn style_guide_says_nothing_about_a_habit_the_profile_shares_with_a_model() {
        let profile = VoiceProfile {
            samples: 5,
            greeting: None,
            sign_off: None,
            median_words: 0,
            median_sentence_words: 0,
            lowercase_starts: true,
            uses_exclamations: true,
            uses_emoji: true,
            uses_em_dash: true,
            uses_contractions: false,
            uses_lists: true,
        };
        let expected = "\
- Don't open with a greeting; start with the point.
- Don't add a sign-off.
- Write in lowercase, as they do.";
        assert_eq!(profile.style_guide(), expected);
    }

    // ---- humanize ---------------------------------------------------------

    #[test]
    fn humanize_drops_a_pure_filler_sentence_but_keeps_real_content() {
        let profile = VoiceProfile { uses_em_dash: true, ..Default::default() };
        let text = "I hope this email finds you well. Let's meet Tuesday at 3pm to go over the budget numbers.";
        assert_eq!(
            humanize(text, &profile),
            "Let's meet Tuesday at 3pm to go over the budget numbers."
        );
    }

    #[test]
    fn humanize_keeps_a_sentence_whole_when_a_tell_sits_inside_real_content() {
        let profile = VoiceProfile { uses_em_dash: true, ..Default::default() };
        let text = "Thanks for reaching out, I can get you the deck by Friday with the full pricing breakdown.";
        assert_eq!(humanize(text, &profile), text);
    }

    #[test]
    fn humanize_never_touches_a_quoted_line() {
        let profile = VoiceProfile { uses_em_dash: false, ..Default::default() };
        let text = "Sounds good — see you then.\n> I hope this email finds you well — thanks.";
        assert_eq!(
            humanize(text, &profile),
            "Sounds good - see you then.\n> I hope this email finds you well — thanks."
        );
    }

    #[test]
    fn humanize_keeps_em_dashes_when_the_profile_uses_them() {
        let profile = VoiceProfile { uses_em_dash: true, ..Default::default() };
        let text = "The plan — if it works — saves us a week.";
        assert_eq!(humanize(text, &profile), text);
    }

    #[test]
    fn humanize_strips_a_leading_subject_line() {
        let profile = VoiceProfile::default();
        let text = "Subject: Re: Tuesday\n\nSounds good, see you then.";
        assert_eq!(humanize(text, &profile), "Sounds good, see you then.");
    }

    #[test]
    fn humanize_strips_a_placeholder_signature_but_keeps_a_real_bracketed_gap() {
        let profile = VoiceProfile::default();
        assert_eq!(humanize("Thanks,\nHari\n\n[Your Name]", &profile), "Thanks,\nHari");
        assert_eq!(
            humanize("Let's meet at [time] on Tuesday.", &profile),
            "Let's meet at [time] on Tuesday."
        );
    }

    #[test]
    fn humanize_collapses_long_runs_of_blank_lines() {
        let profile = VoiceProfile { uses_em_dash: true, ..Default::default() };
        assert_eq!(humanize("one\n\n\n\n\ntwo", &profile), "one\n\ntwo");
    }

    // ---- text_to_html -------------------------------------------------

    #[test]
    fn text_to_html_escapes_every_special_character() {
        assert_eq!(
            text_to_html("She said \"hi\" & <left>."),
            "<p>She said &quot;hi&quot; &amp; &lt;left&gt;.</p>"
        );
    }

    #[test]
    fn text_to_html_turns_blank_lines_into_paragraphs_and_newlines_into_br() {
        assert_eq!(
            text_to_html("Line one\nLine two\n\nSecond para"),
            "<p>Line one<br>Line two</p><p>Second para</p>"
        );
    }

    #[test]
    fn text_to_html_of_blank_input_is_empty() {
        assert_eq!(text_to_html(""), "");
        assert_eq!(text_to_html("   \n  \n"), "");
    }
}
