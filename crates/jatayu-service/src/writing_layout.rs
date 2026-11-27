//! Keeping a rewrite in the shape of the text it replaces.
//!
//! Writing help pastes the model's answer straight over somebody's selection
//! (see `domains::writing`), so an answer that is better worded but joins two
//! lines, drops a blank line or turns a list back into sentences has damaged
//! the very thing it was asked to help with. The prompt says so at length, and
//! models still do it -- the smaller ones most of all. This is the half that
//! does not depend on the model listening:
//!
//! * [`describe`] puts *this* selection's layout into words for the prompt --
//!   how many lines, how many blank, which markers -- so the model is told the
//!   shape it must hand back rather than shapes in general.
//! * [`problem`] says how an answer broke that layout, for one more try.
//! * [`restore`] puts the original's indentation, list markers, blank lines and
//!   trailing spaces back around the answer's words whenever the answer has the
//!   same number of lines with words on them. That is what a rewrite that kept
//!   the lines but not their decoration looks like -- the commonest way a
//!   model goes wrong here -- and it is put right without asking again.
//!
//! Line endings are the caller's: everything here takes and gives `\n`.
//!
//! # What is held, per action
//!
//! Improving, correcting and simplifying keep every line of words, in order;
//! making it concise may leave lines out but never add or split one.
//!
//! A rewrite the person described in their own words
//! ([`WritingAction::Custom`]) is told to keep the layout unless what they
//! asked is about the layout -- and is held to nothing more than that. Whether
//! "number these steps", "make each line a checkbox" or "quote it" is about
//! the layout is a reading of language, and a word list gets it wrong both
//! ways; and every check here that acts on an answer ([`restore`] above all)
//! would undo exactly what was asked when it was. So the model decides, from
//! the prompt ([`describe`]), and [`held`] says that nothing here checks,
//! asks again or puts anything back afterwards. A message written from speech
//! ([`WritingAction::Compose`]) has no original to keep the shape of: its
//! `original` is what was said.

use jatayu_core::writing::WritingAction;

/// Characters that start a bulleted list item when a space follows them.
const BULLETS: &[char] = &[
    '-', '*', '+', '•', '◦', '▪', '▫', '‣', '⁃', '·', '●', '○', '■', '□', '–', '—', '➢', '➤', '►',
    '✓', '✔',
];

/// One line, split into what belongs to the layout and what to the words.
struct Line<'a> {
    /// Indentation, then any list, quote, heading or checkbox markers and
    /// the spaces after them.
    lead: &'a str,
    /// The words, with no whitespace at either end.
    words: &'a str,
    /// The whitespace that ended the line: Markdown's two-space break.
    tail: &'a str,
}

impl<'a> Line<'a> {
    fn of(s: &'a str) -> Self {
        let body = s.trim_end();
        let tail = &s[body.len()..];
        let indent = body.len() - body.trim_start().len();
        let lead = indent + markers_len(&body[indent..]);
        Line { lead: &body[..lead], words: &body[lead..], tail }
    }

    /// The markers in the lead, without the indentation or spacing.
    fn marker(&self) -> &'a str {
        self.lead.trim()
    }

    fn indented(&self) -> bool {
        self.lead.starts_with([' ', '\t'])
    }
}

/// How long the run of markers opening `s` is, spaces after them included:
/// `"- [ ] "`, `"> > "`, `"1. "`. Zero when `s` opens with words.
fn markers_len(s: &str) -> usize {
    let mut used = 0;
    while let Some(n) = one_marker(&s[used..]) {
        used += n;
    }
    used
}

/// One list, quote, heading or checkbox marker at the start of `s`, with the
/// spaces after it. A marker other than a quote's `>` must be followed by a
/// space or end the line, so "-5°", "1.5 kg" and "#hashtag" are words. A
/// letter counts only before `)`: "A. Smith" is a name.
fn one_marker(s: &str) -> Option<usize> {
    let first = s.chars().next()?;
    let bytes = s.as_bytes();
    let end = if BULLETS.contains(&first) || first == '>' {
        first.len_utf8()
    } else if first == '#' {
        let hashes = bytes.iter().take_while(|&&b| b == b'#').count();
        if hashes > 6 {
            return None;
        }
        hashes
    } else if first == '[' {
        if bytes.len() >= 3 && matches!(bytes[1], b' ' | b'x' | b'X') && bytes[2] == b']' {
            3
        } else {
            return None;
        }
    } else if first == '(' {
        let close = s[1..].find(')')?;
        if !is_label(&s[1..1 + close]) {
            return None;
        }
        close + 2
    } else {
        let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
        if (1..=3).contains(&digits) && matches!(bytes.get(digits), Some(b'.' | b')')) {
            digits + 1
        } else if first.is_ascii_alphabetic() && bytes.get(1) == Some(&b')') {
            2
        } else {
            return None;
        }
    };
    let after = &s[end..];
    let spaces = after.len() - after.trim_start_matches([' ', '\t']).len();
    if spaces == 0 && !after.is_empty() && first != '>' {
        return None;
    }
    Some(end + spaces)
}

/// A list label inside parentheses: `(1)`, `(12)`, `(a)`.
fn is_label(label: &str) -> bool {
    let digits = label.bytes().all(|b| b.is_ascii_digit());
    (digits && (1..=3).contains(&label.len()))
        || (label.len() == 1 && label.bytes().all(|b| b.is_ascii_alphabetic()))
}

fn blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// What [`describe`] and [`problem`] count.
struct Shape {
    /// Lines with words on them.
    filled: usize,
    blank: usize,
    /// Lines that open with a marker.
    marked: usize,
    indented: usize,
    /// The distinct markers, as the first line of each kind spells them.
    kinds: Vec<String>,
}

impl Shape {
    fn of(text: &str) -> Self {
        let mut shape = Shape { filled: 0, blank: 0, marked: 0, indented: 0, kinds: Vec::new() };
        let mut seen = Vec::new();
        for line in text.split('\n') {
            if blank(line) {
                shape.blank += 1;
                continue;
            }
            shape.filled += 1;
            let line = Line::of(line);
            if line.indented() {
                shape.indented += 1;
            }
            let marker = line.marker();
            if marker.is_empty() {
                continue;
            }
            shape.marked += 1;
            // "1." and "2." are one kind of marker, and "a)" and "b)" another.
            let kind: String = marker
                .chars()
                .map(|c| match c {
                    '0'..='9' => '1',
                    c if c.is_ascii_alphabetic() && c != 'x' && c != 'X' => 'a',
                    c => c,
                })
                .collect();
            if !seen.contains(&kind) {
                seen.push(kind);
                shape.kinds.push(marker.to_string());
            }
        }
        shape
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn quoted(kinds: &[String]) -> String {
    kinds.iter().take(4).map(|k| format!("\"{k}\"")).collect::<Vec<_>>().join(", ")
}

/// How a selection's layout reads in words: "5 lines with words on them, 2
/// blank lines, ...". `None` for a single line with nothing around it.
fn shape_in_words(shape: &Shape) -> Option<Vec<String>> {
    if shape.filled <= 1 && shape.blank == 0 {
        return None;
    }
    let mut parts = vec![plural(shape.filled, "line with words on it", "lines with words on them")];
    if shape.blank > 0 {
        parts.push(plural(shape.blank, "blank line", "blank lines"));
    }
    if shape.marked > 0 {
        parts.push(format!(
            "{} that {} with a marker ({})",
            plural(shape.marked, "line", "lines"),
            if shape.marked == 1 { "starts" } else { "start" },
            quoted(&shape.kinds),
        ));
    }
    if shape.indented > 0 {
        parts.push(format!("{} indented", plural(shape.indented, "line", "lines")));
    }
    Some(parts)
}

/// The selection's layout in words, and what the answer must do with it, for
/// the prompt. `text` is the selection with the whitespace around it trimmed
/// off, as the model is shown it.
pub fn describe(text: &str, action: WritingAction) -> String {
    let keep = match action {
        // Dictation has no layout to keep -- see `domains::writing::dictate` --
        // and is never described; it is held to the strictest rule if it is.
        WritingAction::Improve
        | WritingAction::Grammar
        | WritingAction::Dictate
        | WritingAction::Simplify => {
            "Your answer must have exactly the same lines in the same order: the same lines \
             with words, the same blank lines between them, and every marker and indent where \
             it is. Do not join two lines or split one, even where a sentence runs across a \
             line break."
        }
        // Nothing checks this one afterwards -- see `held` -- so it is said in
        // full, including what is not layout, which a check would have to guess.
        WritingAction::Custom => {
            let now = match shape_in_words(&Shape::of(text)) {
                Some(parts) => parts.join(", "),
                None => "a single line".to_string(),
            };
            return format!(
                "Its layout now: {now}. Keep it exactly as it is -- every line break, blank \
                 line, marker and indent -- unless what they ask is about the layout itself: \
                 numbering, bullets, checkboxes, quoting, a list, paragraphs, lines, headings. \
                 Then change the layout as far as they ask, and nowhere else. A title, subject \
                 line or heading they ask you to reword is words, not layout: reword it and \
                 leave it where it is."
            );
        }
        WritingAction::Concise => {
            "You may leave out a whole line or list item. Do not join lines, split one, add a \
             blank line, or take the marker off any item you keep."
        }
        // What a message is written from is speech, which has no layout. Never
        // asked about: `deskwrite::compose` has a prompt of its own.
        WritingAction::Compose => {
            return "It has no layout of its own to keep: lay the message out as its kind of \
                    message is."
                .to_string();
        }
    };
    let Some(parts) = shape_in_words(&Shape::of(text)) else {
        return "Its layout: a single line. Give back a single line, with no line break added."
            .to_string();
    };
    format!("Its layout: {}. {keep}", parts.join(", "))
}

/// Is an answer to `action` checked against the original's layout, asked for
/// again when it broke it, and given the original's markers, indents and blank
/// lines afterwards? Every action's is, but two.
///
/// [`WritingAction::Custom`] is not: it may be asked to change the layout
/// ("number these steps", "make it a list"), and any of the three would undo
/// the very change asked for, silently. Its prompt says to keep the layout
/// unless that is what is asked ([`describe`]), and that is all there is.
/// [`WritingAction::Compose`] has no original layout.
pub fn held(action: WritingAction) -> bool {
    !matches!(action, WritingAction::Custom | WritingAction::Compose)
}

/// How `answer` broke the layout of `original`, said to the model for one
/// more try -- or `None` when it kept it, as far as can be told, or broke it
/// only in ways [`restore`] puts right. Improving or correcting keeps every
/// line; making it concise may drop lines, but may not add any, nor blank
/// lines, nor take a list's markers away.
pub fn problem(original: &str, answer: &str, action: WritingAction) -> Option<String> {
    let was = Shape::of(original);
    let now = Shape::of(answer);
    // The same lines with words: whatever markers, indents or blank lines the
    // answer lost or gained, `restore` gives back the original's, with no
    // second wait on the model.
    if now.filled == was.filled {
        return None;
    }
    match action {
        WritingAction::Improve
        | WritingAction::Grammar
        | WritingAction::Dictate
        | WritingAction::Simplify => (now.filled != was.filled).then(|| {
            format!(
                "the text has {} and your answer has {}. Every line must stay a line of its own",
                plural(was.filled, "line with words on it", "lines with words on them"),
                now.filled,
            )
        }),
        // Nothing to compare a message written from speech with, and what
        // somebody asked to have done to a selection in their own words may be
        // a change of its layout: see `held`.
        WritingAction::Compose | WritingAction::Custom => None,
        WritingAction::Concise => {
            if now.filled > was.filled {
                Some(format!(
                    "the text has {} and your answer has more, {}. Leave lines out; do not split \
                     them",
                    plural(was.filled, "line with words on it", "lines with words on them"),
                    now.filled,
                ))
            } else if was.marked > 0 && now.marked == 0 {
                Some(format!(
                    "the text's list markers ({}) are gone from your answer. Every item you keep \
                     stays on its own line with its marker",
                    quoted(&was.kinds),
                ))
            } else if now.blank > was.blank {
                Some(format!(
                    "the text has {} and your answer has {}. Add no blank lines",
                    plural(was.blank, "blank line", "blank lines"),
                    now.blank,
                ))
            } else {
                None
            }
        }
    }
}

/// What to add to the prompt when asking again: the first answer, and what
/// was wrong with it.
pub fn retry_note(first: &str, problem: &str) -> String {
    format!(
        "Your first answer was:\n{first}\n\nIt changed the layout: {problem}. Rewrite the text \
         again, from the text itself, keeping its layout exactly as described above."
    )
}

/// `answer`'s words in `original`'s layout: each line of words in the answer
/// takes the place of the matching line of words in the original, with that
/// line's indentation, markers and trailing spaces; blank lines are the
/// original's, where the original had them and nowhere else. `None` when the
/// two have different numbers of lines with words, which no rule here can
/// line up.
pub fn restore(original: &str, answer: &str) -> Option<String> {
    let filled = original.split('\n').filter(|l| !blank(l)).count();
    let mut words = answer.split('\n').filter(|l| !blank(l));
    if words.clone().count() != filled {
        return None;
    }
    let lines: Vec<String> = original
        .split('\n')
        .map(|was| {
            if blank(was) {
                return was.to_string();
            }
            let was = Line::of(was);
            let now = Line::of(words.next().unwrap_or_default());
            format!("{}{}{}", was.lead, now.words, was.tail)
        })
        .collect();
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_told_from_words() {
        for (line, lead) in [
            ("- milk", "- "),
            ("  * eggs", "  * "),
            ("• bread", "• "),
            ("1. one", "1. "),
            ("12) twelve", "12) "),
            ("a) first", "a) "),
            ("(2) second", "(2) "),
            ("- [ ] todo", "- [ ] "),
            ("- [x] done", "- [x] "),
            ("> quoted", "> "),
            (">> twice", ">> "),
            ("## Heading", "## "),
            ("-", "-"),
        ] {
            assert_eq!(Line::of(line).lead, lead, "{line:?}");
        }
        for words in
            ["-5° outside", "1.5 kg", "#hashtag", "A. Smith wrote", "10.30 works", "(see below)"]
        {
            assert_eq!(Line::of(words).lead, "", "{words:?}");
        }
    }

    #[test]
    fn lost_bullets_and_blank_lines_are_put_back() {
        let original = "Hi team,\n\nThings to do:\n- buy milk\n- call Sam\n\nThanks";
        let answer = "Hi team,\nThings to do:\nBuy milk.\nCall Sam.\nThanks!";
        assert_eq!(
            restore(original, answer).unwrap(),
            "Hi team,\n\nThings to do:\n- Buy milk.\n- Call Sam.\n\nThanks!"
        );
    }

    #[test]
    fn added_markers_blank_lines_and_indents_come_out() {
        let original = "Line one\nLine two";
        let answer = "- Line one.\n\n    Line two.";
        assert_eq!(restore(original, answer).unwrap(), "Line one.\nLine two.");
    }

    #[test]
    fn a_changed_marker_and_nesting_are_the_originals() {
        let original = "1. first\n  - nested  \n2. second";
        let answer = "1) First\n* Nested\n2) Second";
        assert_eq!(restore(original, answer).unwrap(), "1. First\n  - Nested  \n2. Second");
    }

    #[test]
    fn a_different_number_of_lines_is_not_guessed_at() {
        assert_eq!(restore("one\ntwo", "one and two"), None);
        assert_eq!(restore("one", "one\ntwo"), None);
    }

    #[test]
    fn improving_or_correcting_must_keep_every_line() {
        let original = "First line\nSecond line";
        assert!(problem(original, "First line, second line", WritingAction::Grammar).is_some());
        assert!(problem(original, "First line.\n\nSecond line.", WritingAction::Improve).is_none());
        assert!(problem("One line", "One\nline", WritingAction::Improve).is_some());
    }

    #[test]
    fn concise_may_drop_lines_but_not_split_them_or_lose_the_list() {
        let original = "Intro sentence here.\n- one\n- two\n- three";
        let action = WritingAction::Concise;
        assert!(problem(original, "Intro.\n- one\n- three", action).is_none());
        assert!(problem(original, "Intro: one, two and three.", action).is_some());
        assert!(problem(original, "Intro.\n- one\n- two\n- three\n- four", action).is_some());
        assert!(problem(original, "Intro.\n\n- one\n- two", action).is_some());
        assert!(
            problem(original, "Intro.\none\ntwo\nthree", action).is_none(),
            "the same lines with the markers gone: restore puts them back"
        );
    }

    #[test]
    fn the_description_names_this_texts_layout() {
        let text = "Hi,\n\n- a\n- b\n  1. c\n\nBye";
        let said = describe(text, WritingAction::Improve);
        assert!(said.contains("5 lines with words on them"), "{said}");
        assert!(said.contains("2 blank lines"), "{said}");
        assert!(said.contains("3 lines that start with a marker (\"-\", \"1.\")"), "{said}");
        assert!(said.contains("1 line indented"), "{said}");
        assert!(describe("just words", WritingAction::Grammar).contains("single line"));
        assert!(describe(text, WritingAction::Concise).contains("leave out a whole line"));
    }

    #[test]
    fn simplifying_keeps_every_line_like_an_improvement() {
        let text = "Hi,\n\n- a\n- b";
        assert_eq!(
            describe(text, WritingAction::Simplify),
            describe(text, WritingAction::Improve),
            "told the same"
        );
        assert!(problem("One\nTwo", "One and two", WritingAction::Simplify).is_some());
        assert!(problem("One\nTwo", "Uno\nDos", WritingAction::Simplify).is_none());
        assert!(held(WritingAction::Simplify));
    }

    #[test]
    fn a_message_written_from_speech_has_no_layout_to_hold() {
        let said = describe("write that i am ill\nand will call", WritingAction::Compose);
        assert!(said.contains("no layout of its own"), "{said}");
        assert!(!said.contains("same lines"), "{said}");
        assert_eq!(problem("one\ntwo", "one", WritingAction::Compose), None);
        assert!(!held(WritingAction::Compose));
    }

    #[test]
    fn every_action_that_rewrites_a_selection_is_held_but_a_custom_one() {
        for action in [
            WritingAction::Improve,
            WritingAction::Grammar,
            WritingAction::Concise,
            WritingAction::Simplify,
            WritingAction::Dictate,
        ] {
            assert!(held(action), "{action:?}");
        }
        assert!(!held(WritingAction::Custom));
    }

    /// What was asked may be to change the layout, in words no list would have:
    /// whatever a custom rewrite answers with is the answer, line for line.
    #[test]
    fn a_custom_answer_is_never_checked_or_corrected() {
        let original = "Open the box\nRemove the foam\nPlug it in";
        let numbered = "1. Open the box\n2. Remove the foam\n3. Plug it in";
        // Nothing to ask again about, however many lines the answer has...
        assert_eq!(problem(original, numbered, WritingAction::Custom), None);
        assert_eq!(
            problem(original, "Open it, remove the foam, plug it in", WritingAction::Custom),
            None
        );
        assert_eq!(problem(original, "- a\n\n- b\n- c\n- d", WritingAction::Custom), None);
        // ... and `restore` is what would have undone it, which is why the
        // caller does not call it for one.
        assert_eq!(
            restore(original, numbered).unwrap(),
            original,
            "restore would strip the numbers a custom rewrite was asked to add"
        );
        assert!(!held(WritingAction::Custom));
    }

    #[test]
    fn a_custom_prompt_says_what_to_keep_and_what_is_not_layout() {
        let said = describe("Fix the title\nand the rest.", WritingAction::Custom);
        assert!(said.contains("2 lines with words on them"), "{said}");
        assert!(said.contains("Keep it exactly as it is"), "{said}");
        assert!(said.contains("unless what they ask is about the layout itself"), "{said}");
        for asked_for in ["numbering", "checkboxes", "quoting", "bullets", "paragraphs"] {
            assert!(said.contains(asked_for), "{asked_for}: {said}");
        }
        assert!(said.contains("A title, subject line or heading they ask you to reword"), "{said}");
        // A single line may be asked to become several, so it is not told to
        // stay one.
        let single = describe("Just words.", WritingAction::Custom);
        assert!(single.contains("a single line"), "{single}");
        assert!(!single.contains("Give back a single line"), "{single}");
        assert!(!single.contains("exactly the same lines"), "{single}");
    }
}
