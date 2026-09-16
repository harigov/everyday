//! What the summarising model is asked, and how a long call is split.

use super::Transcript;

/// Rough token estimate: four characters each, rounded up.
pub fn estimate_tokens(text: &str) -> u32 {
    (text.len() as u32).div_ceil(4)
}

/// The default budget for one summarising request's transcript, in tokens:
/// smaller for a loopback (local) endpoint.
pub fn default_budget(loopback: bool) -> u32 {
    if loopback { 6_000 } else { 24_000 }
}

/// A system prompt and a user message.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub system: String,
    pub user: String,
}

/// Split the transcript's lines (see [`Transcript::as_lines`]) into pieces
/// that each fit `budget`, only ever on line boundaries. One piece if it fits.
pub fn pieces(transcript: &Transcript, budget: u32) -> Vec<String> {
    let text = transcript.as_lines();
    if text.is_empty() {
        return vec![String::new()];
    }
    if estimate_tokens(&text) <= budget {
        return vec![text];
    }

    let mut out = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        let mut with_newline = String::with_capacity(line.len() + 1);
        with_newline.push_str(line);
        with_newline.push('\n');

        if estimate_tokens(&with_newline) > budget {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            out.extend(split_long_line(&with_newline, budget));
            continue;
        }

        if !current.is_empty()
            && estimate_tokens(&current) + estimate_tokens(&with_newline) > budget
        {
            out.push(std::mem::take(&mut current));
        }
        current.push_str(&with_newline);
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Split one over-long line on whitespace into pieces that each fit
/// `budget`. A single word longer than the whole budget still becomes its
/// own piece -- there is nowhere else to cut it.
fn split_long_line(line: &str, budget: u32) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for word in line.split_inclusive(' ') {
        if !current.is_empty() && estimate_tokens(&current) + estimate_tokens(word) > budget {
            out.push(std::mem::take(&mut current));
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(line.to_string());
    }
    out
}

const SUMMARISE_RULES: &str = "\
You are writing meeting notes from a transcript, using the template you are \
given.

Rules:
- Keep every heading from the template, in the order it appears.
- Write nothing outside those headings.
- The text under each heading in the template is an instruction to you, not \
  content to keep: replace it with what actually happened.
- Attribute what was said or decided using the names as they appear in the \
  transcript.
- Never invent a name, a decision, or a date that was not said.
- If a heading has nothing to say, write \"Nothing recorded.\" under it.
- Output Markdown only: no code fences, and no commentary before or after \
  the note.";

/// The request for one piece (or the whole call). `part` is `Some((n, of))`
/// when the call was split.
pub fn summarise(filled_template: &str, lines: &str, part: Option<(usize, usize)>) -> Request {
    let mut user = String::new();
    if let Some((n, of)) = part {
        user.push_str(&format!(
            "This is part {n} of {of} of one call's transcript, split only because it was \
             long. Summarise what is in this part alone; another pass will merge every \
             part's answer into one note, so do not worry about what came before or after.\n\n"
        ));
    }
    user.push_str("Template:\n\n");
    user.push_str(filled_template);
    user.push_str("\n\nTranscript:\n\n");
    user.push_str(lines);
    Request { system: SUMMARISE_RULES.to_string(), user }
}

const COMBINE_RULES: &str = "\
You are merging several partial meeting notes into one final note. Each part \
was written from a different stretch of the same call's transcript, using \
the same template.

Rules:
- Keep every heading from the template, in the order it appears.
- Write nothing outside those headings.
- Combine what each part says under a heading into one account. Do not \
  repeat a decision or an action item that more than one part recorded.
- Never invent a name, a decision, or a date that is not in one of the \
  parts.
- If every part left a heading with nothing recorded, write \"Nothing \
  recorded.\" under it.
- Output Markdown only: no code fences, and no commentary before or after \
  the note.";

/// The request that merges the per-piece answers into one note.
pub fn combine(filled_template: &str, partials: &[String]) -> Request {
    let mut user = String::new();
    user.push_str("Template:\n\n");
    user.push_str(filled_template);
    for (i, partial) in partials.iter().enumerate() {
        user.push_str(&format!("\n\nPart {} of {}:\n\n", i + 1, partials.len()));
        user.push_str(partial);
    }
    Request { system: COMBINE_RULES.to_string(), user }
}

/// Hints for a transcriber: event title and attendee names, so names are
/// spelled right. Short; recognisers cap their prompts.
pub fn transcriber_hint(title: &str, names: &[String]) -> String {
    let cleaned: Vec<String> = names
        .iter()
        .filter_map(|raw| {
            let (name, email) = super::identify::parse_attendee(raw);
            name.or_else(|| email.map(|e| local_part(&e)))
        })
        .collect();

    let mut hint = format!("Meeting: {title}.");
    if !cleaned.is_empty() {
        hint.push_str(" Participants: ");
        hint.push_str(&cleaned.join(", "));
        hint.push('.');
    }
    truncate_at_name_boundary(&hint, 400)
}

fn local_part(email: &str) -> String {
    email.split('@').next().unwrap_or(email).to_string()
}

/// Cut a string down to (about) `max` characters without splitting a name in
/// the middle: back up to the last ", " at or before the limit.
fn truncate_at_name_boundary(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let cut = s[..max].rfind(", ").unwrap_or(max);
    let mut out = s[..cut].to_string();
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::NoteId;
    use crate::meeting::{Attribution, Segment, Speaker};

    fn transcript_of(lines: &[(&str, &str)]) -> Transcript {
        let speakers: Vec<Speaker> = lines
            .iter()
            .map(|(name, _)| *name)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .enumerate()
            .map(|(i, name)| Speaker {
                key: i as u16,
                label: name.to_string(),
                email: None,
                voiceprint_id: None,
                how: Attribution::Unknown,
                centroid: Vec::new(),
                embedding_model: String::new(),
            })
            .collect();
        let key_of = |name: &str| speakers.iter().find(|s| s.label == name).unwrap().key;
        let mut at = 0u64;
        let segments = lines
            .iter()
            .map(|(name, text)| {
                let seg = Segment {
                    start_ms: at,
                    end_ms: at + 1000,
                    speaker: key_of(name),
                    text: text.to_string(),
                };
                at += 1000;
                seg
            })
            .collect();
        Transcript {
            id: crate::id::TranscriptId::new(),
            note_id: NoteId::new(),
            recording_id: None,
            language: None,
            backend: "test".into(),
            speakers,
            segments,
            created_at: jiff::Timestamp::now(),
            updated_at: jiff::Timestamp::now(),
        }
    }

    #[test]
    fn a_short_transcript_is_one_piece() {
        let t = transcript_of(&[("A", "hello"), ("B", "hi there")]);
        let p = pieces(&t, 10_000);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0], t.as_lines());
    }

    #[test]
    fn an_empty_transcript_is_one_empty_piece() {
        let t = transcript_of(&[]);
        assert_eq!(pieces(&t, 100), vec![String::new()]);
    }

    #[test]
    fn a_long_transcript_splits_on_line_boundaries() {
        let lines: Vec<(&str, &str)> = (0..50)
            .map(|_| ("A", "this line is fairly long and repeats a bit to add up"))
            .collect();
        let t = transcript_of(&lines);
        let budget = 200;
        let p = pieces(&t, budget);
        assert!(p.len() > 1, "expected more than one piece");
        for piece in &p {
            assert!(estimate_tokens(piece) <= budget, "piece over budget: {piece:?}");
        }
        // Every original line survives exactly once, in order, across the pieces.
        let rejoined: String = p.concat();
        assert_eq!(rejoined, t.as_lines());
    }

    #[test]
    fn a_single_line_longer_than_the_budget_becomes_its_own_piece_split_on_whitespace() {
        let huge = "word ".repeat(200);
        let t = transcript_of(&[("A", &huge)]);
        let budget = 20;
        let p = pieces(&t, budget);
        assert!(p.len() > 1);
        for piece in &p {
            assert!(estimate_tokens(piece) <= budget || !piece.contains(' '), "{piece:?}");
        }
    }

    #[test]
    fn summarise_carries_the_template_and_transcript_and_says_which_part() {
        let req = summarise("# T\n## Summary\n...", "[00:00] A: hi\n", Some((2, 3)));
        assert!(req.system.contains("Keep every heading"));
        assert!(req.user.contains("part 2 of 3"));
        assert!(req.user.contains("# T"));
        assert!(req.user.contains("[00:00] A: hi"));

        let whole = summarise("# T", "lines", None);
        assert!(!whole.user.contains("part"));
    }

    #[test]
    fn combine_lists_every_partial_against_the_template() {
        let req = combine("# T\n## Summary", &["first answer".into(), "second answer".into()]);
        assert!(req.user.contains("Part 1 of 2"));
        assert!(req.user.contains("first answer"));
        assert!(req.user.contains("Part 2 of 2"));
        assert!(req.user.contains("second answer"));
        assert!(req.system.to_lowercase().contains("repeat"));
    }

    #[test]
    fn transcriber_hint_names_participants_by_their_display_name() {
        let hint = transcriber_hint(
            "Design sync",
            &["Priya Raman <priya@x.com>".into(), "bob@example.com".into(), "Just A Name".into()],
        );
        assert_eq!(hint, "Meeting: Design sync. Participants: Priya Raman, bob, Just A Name.");
    }

    #[test]
    fn transcriber_hint_with_no_names_is_just_the_title() {
        assert_eq!(transcriber_hint("Design sync", &[]), "Meeting: Design sync.");
    }

    #[test]
    fn transcriber_hint_truncates_at_a_name_boundary() {
        let names: Vec<String> =
            (0..100).map(|i| format!("Person With A Rather Long Full Name Number {i}")).collect();
        let untruncated_len: usize = "Meeting: A very long meeting title indeed. Participants: "
            .len()
            + names.iter().map(|n| n.len() + 2).sum::<usize>();
        assert!(
            untruncated_len > 400,
            "test needs a hint that actually overflows: {untruncated_len}"
        );
        let hint = transcriber_hint("A very long meeting title indeed", &names);
        assert!(hint.len() <= 401, "{}: {}", hint.len(), hint);
        assert!(hint.ends_with('.'));
        // What is kept must be a whole, comma-separated list of names, not a
        // name sliced in half partway through.
        let names_part = hint.trim_end_matches('.').split("Participants: ").nth(1).unwrap();
        for name in names_part.split(", ") {
            assert!(
                name.starts_with("Person With A Rather Long Full Name Number"),
                "cut mid-name: {name:?} in {hint:?}"
            );
        }
    }
}
