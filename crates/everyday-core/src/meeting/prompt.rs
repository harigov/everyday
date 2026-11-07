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

/// What every summarising request is told about the material it is reading
/// -- shared by [`summarise`] and [`combine`], which see the same noise.
const ABOUT_THE_TRANSCRIPT: &str = "\
About the transcript:
- It came from automatic speech recognition, not a person. Expect misheard \
  words, wrong homophones, garbled names, numbers and product names, \
  sentences cut in half, and filler. Read through the noise: correct an \
  obvious mishearing from context and from how names are spelled in the \
  people list, and quietly drop what cannot be made sense of.
- Speaker labels can be wrong, and one person can appear under more than one \
  label. A label such as \"Unknown 1\" or \"Others\" is a voice nobody could \
  put a name to: never present it as a person's name, and never give it an \
  action item -- say \"someone\" in prose, and \"Unassigned\" as an owner.
- Where something that matters is unclear -- an amount, a date, who agreed, \
  who owns a task -- say so in a few words, \"(unclear)\", rather than guess. \
  Never build a decision or an action item on a fragment you cannot make \
  sense of.";

/// How the note should read -- shared by [`summarise`] and [`combine`].
const HOW_TO_WRITE: &str = "\
How to write:
- Keep every heading from the template, in the order it appears, and write \
  nothing outside those headings.
- The text under each heading in the template is an instruction to you, not \
  content to keep: replace it with what actually happened. Where it shows a \
  table, write a Markdown table with exactly those columns.
- The note already lists who attended. Do not list attendees again, and do \
  not open sentences with who was present; name a person only where it \
  matters -- who decided something, who owns something, whose view differed.
- Be specific and concrete: the actual numbers, dates, names of systems, \
  customers and documents, and the reasons given. No filler (\"the team \
  discussed\", \"a productive conversation\"), and no repeating the same \
  point under two headings.
- Use people's names as the people list spells them; the person who \
  recorded the call by their name too.
- Never invent a name, a decision, a number or a date that was not said.
- If a heading has nothing to say, write \"Nothing recorded.\" under it.
- Output Markdown only: no code fences, and no commentary before or after \
  the note.";

/// The request for one piece (or the whole call). `part` is `Some((n, of))`
/// when the call was split. `people` is [`people_block`]'s, or empty.
pub fn summarise(
    filled_template: &str,
    people: &str,
    lines: &str,
    part: Option<(usize, usize)>,
) -> Request {
    let system = format!(
        "You are writing meeting notes from a transcript, using the template you are \
         given.\n\n{ABOUT_THE_TRANSCRIPT}\n\n{HOW_TO_WRITE}"
    );
    let mut user = String::new();
    if let Some((n, of)) = part {
        user.push_str(&format!(
            "This is part {n} of {of} of one call's transcript, split only because it was \
             long. Summarise what is in this part alone; another pass will merge every \
             part's answer into one note, so do not worry about what came before or after.\n\n"
        ));
    }
    if !people.is_empty() {
        user.push_str(people);
        user.push_str("\n\n");
    }
    user.push_str("Template:\n\n");
    user.push_str(filled_template);
    user.push_str("\n\nTranscript:\n\n");
    user.push_str(lines);
    Request { system, user }
}

/// The request that merges the per-piece answers into one note.
pub fn combine(filled_template: &str, people: &str, partials: &[String]) -> Request {
    let system = format!(
        "You are merging several partial meeting notes into one final note. Each part \
         was written from a different stretch of the same call's transcript, using the \
         same template.\n\n\
         Combine what each part says under a heading into one account. Do not repeat a \
         decision or an action item that more than one part recorded; merge tables into \
         one table, keeping one person's rows together. If every part left a heading \
         with nothing recorded, write \"Nothing recorded.\" under it.\n\n\
         {ABOUT_THE_TRANSCRIPT}\n\n{HOW_TO_WRITE}"
    );
    let mut user = String::new();
    if !people.is_empty() {
        user.push_str(people);
        user.push_str("\n\n");
    }
    user.push_str("Template:\n\n");
    user.push_str(filled_template);
    for (i, partial) in partials.iter().enumerate() {
        user.push_str(&format!("\n\nPart {} of {}:\n\n", i + 1, partials.len()));
        user.push_str(partial);
    }
    Request { system, user }
}

/// Who was on the call, for the summarising model: the person recording,
/// every voice heard and how sure the naming of it is, and everybody
/// invited who was not heard, with their answer. Empty when there is
/// nobody to say anything about.
pub fn people_block(transcript: &Transcript, invitees: &[super::people::Invitee]) -> String {
    use super::Attribution;

    let mut spoke: Vec<u16> = Vec::new();
    for seg in &transcript.segments {
        if !spoke.contains(&seg.speaker) {
            spoke.push(seg.speaker);
        }
    }
    let mut lines = Vec::new();
    for key in &spoke {
        let Some(s) = transcript.speaker(*key) else { continue };
        let how = match s.how {
            Attribution::Owner => "the person who recorded this call; certain",
            Attribution::Named => "named by hand; certain",
            Attribution::Matched { .. } => "recognised by voice; fairly sure",
            Attribution::Inferred => "worked out from the invite and what was said; may be wrong",
            Attribution::Unknown => "a voice nobody could put a name to",
        };
        let answer = invitees
            .iter()
            .find(|i| i.answers_to(&s.label, s.email.as_deref()))
            .map(|i| i.answer())
            .filter(|a| !a.is_empty())
            .map(|a| format!("{a}; "))
            .unwrap_or_default();
        lines.push(format!("- {} -- {answer}spoke ({how})", s.label));
    }
    for person in invitees {
        let heard = spoke
            .iter()
            .filter_map(|k| transcript.speaker(*k))
            .any(|s| person.answers_to(&s.label, s.email.as_deref()));
        if heard {
            continue;
        }
        let answer = person.answer();
        if answer.is_empty() {
            lines.push(format!("- {} -- invited; not heard", person.name));
        } else {
            lines.push(format!("- {} -- {answer}; not heard", person.name));
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "People (from the calendar invite and the audio; someone not heard may still have \
         been there, listening or under an unnamed label):\n{}",
        lines.join("\n")
    )
}

const TITLE_RULES: &str = "\
You name meeting notes. Given a meeting's calendar title and its notes, \
write a short title of three to eight words for what this particular \
meeting was about -- its main topic or outcome -- so that notes from a \
meeting that repeats every week can be told apart. Not the calendar title \
again, no date, no people's names unless they are the subject, no quotes, \
no trailing full stop. Answer with the title alone.";

/// The request for [`title_of`]'s topic: what this meeting, as opposed to
/// every other one with the same name, was about.
pub fn title_request(event_title: &str, note: &str) -> Request {
    let mut user = format!("Calendar title: {event_title}\n\nNotes:\n\n");
    user.push_str(&crate::model::truncate_on_char_boundary(note, 12_000));
    Request { system: TITLE_RULES.to_string(), user }
}

/// The topic the model answered with, tidied, or `None` when it said
/// nothing usable.
pub fn clean_title(answer: &str) -> Option<String> {
    let line = answer.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line
        .trim_start_matches(['#', '*', '-'])
        .trim()
        .trim_start_matches("Title:")
        .trim()
        .trim_matches(['"', '\'', '*', '`', '\u{201c}', '\u{201d}'])
        .trim_end_matches('.')
        .trim();
    let words = line.split_whitespace().count();
    if line.is_empty() || words > 14 {
        return None;
    }
    Some(crate::model::truncate_on_char_boundary(line, 120))
}

/// The note's title: the calendar's, then what this meeting was about,
/// so a weekly meeting does not write the same title every week. Without a
/// topic the date tells them apart instead. A topic that already says
/// everything the calendar title does stands alone.
pub fn title_of(event_title: &str, topic: Option<&str>, date: &str) -> String {
    let event_title = event_title.trim();
    match topic.map(str::trim).filter(|t| !t.is_empty()) {
        Some(topic) if event_title.is_empty() => topic.to_string(),
        Some(topic) if topic.to_lowercase().contains(&event_title.to_lowercase()) => {
            topic.to_string()
        }
        Some(topic) => format!("{event_title}: {topic}"),
        None if event_title.is_empty() => format!("Call on {date}"),
        None => format!("{event_title}, {date}"),
    }
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

/// Cut a string down to (about) `max` bytes without splitting a name in the
/// middle: back up to the last ", " at or before the limit.
///
/// The limit is first moved back onto a character boundary: attendee names
/// are whatever the calendar says, and "José" straddling byte 400 must not
/// take the whole pipeline down with it.
fn truncate_at_name_boundary(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut max = max;
    while !s.is_char_boundary(max) {
        max -= 1;
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
        let req = summarise("# T\n## Summary\n...", "", "[00:00] A: hi\n", Some((2, 3)));
        assert!(req.system.contains("Keep every heading"));
        assert!(req.system.contains("speech recognition"));
        assert!(req.user.contains("part 2 of 3"));
        assert!(req.user.contains("# T"));
        assert!(req.user.contains("[00:00] A: hi"));

        let whole = summarise("# T", "People:\n- Priya", "lines", None);
        assert!(!whole.user.contains("part"));
        assert!(whole.user.starts_with("People:\n- Priya"));
    }

    #[test]
    fn combine_lists_every_partial_against_the_template() {
        let req = combine("# T\n## Summary", "", &["first answer".into(), "second answer".into()]);
        assert!(req.user.contains("Part 1 of 2"));
        assert!(req.user.contains("first answer"));
        assert!(req.user.contains("Part 2 of 2"));
        assert!(req.user.contains("second answer"));
        assert!(req.system.to_lowercase().contains("repeat"));
    }

    #[test]
    fn a_title_is_the_calendars_and_the_topic_or_the_date() {
        assert_eq!(
            title_of("Weekly sync", Some("Pricing page slips to May"), "6 November"),
            "Weekly sync: Pricing page slips to May"
        );
        assert_eq!(
            title_of("Acme review", Some("Acme review of discount tiers"), "6 November"),
            "Acme review of discount tiers"
        );
        assert_eq!(title_of("Weekly sync", None, "6 November"), "Weekly sync, 6 November");
        assert_eq!(title_of("", None, "6 November"), "Call on 6 November");
        assert_eq!(title_of("", Some("Hiring plan"), "6 November"), "Hiring plan");
    }

    #[test]
    fn a_topic_is_tidied_or_refused() {
        assert_eq!(
            clean_title("\"Pricing page slips to May.\"\n").as_deref(),
            Some("Pricing page slips to May")
        );
        assert_eq!(clean_title("Title: Q4 hiring plan").as_deref(), Some("Q4 hiring plan"));
        assert_eq!(clean_title("**Q4 hiring**").as_deref(), Some("Q4 hiring"));
        assert_eq!(clean_title("   \n"), None);
        assert_eq!(clean_title(&"word ".repeat(30)), None);
    }

    #[test]
    fn the_people_block_says_who_spoke_how_sure_and_who_was_not_heard() {
        use crate::mail::AttendeeResponse;
        use crate::meeting::people::Invitee;
        let mut t = transcript_of(&[("Hari", "hi"), ("Priya Raman", "hello"), ("Unknown 1", "yo")]);
        for s in &mut t.speakers {
            s.how = match s.label.as_str() {
                "Hari" => Attribution::Owner,
                "Priya Raman" => Attribution::Inferred,
                _ => Attribution::Unknown,
            };
        }
        let invitee = |name: &str, response| Invitee {
            name: name.into(),
            email: None,
            response: Some(response),
            organizer: false,
        };
        let block = people_block(
            &t,
            &[
                invitee("Priya Raman", AttendeeResponse::Accepted),
                invitee("Dev Patel", AttendeeResponse::Declined),
            ],
        );
        assert!(
            block.contains("- Hari -- spoke (the person who recorded this call; certain)"),
            "{block}"
        );
        assert!(block.contains("- Priya Raman -- accepted; spoke (worked out"), "{block}");
        assert!(block.contains("- Unknown 1 -- spoke (a voice nobody"), "{block}");
        assert!(block.contains("- Dev Patel -- declined; not heard"), "{block}");
        assert_eq!(people_block(&transcript_of(&[]), &[]), "");
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
    fn transcriber_hint_never_cuts_through_a_character() {
        // Every offset near the limit, so one of them lands inside a
        // two-byte "é" or a three-byte "田".
        for pad in 0..6 {
            let names: Vec<String> =
                (0..60).map(|i| format!("{}José 田中 Müller {i}", "x".repeat(pad))).collect();
            let hint = transcriber_hint("Réunion", &names);
            assert!(hint.len() <= 401, "{}", hint.len());
            assert!(hint.ends_with('.'));
        }
        // And one long name with no comma to back up to at all.
        let hint = transcriber_hint("Réunion", &["é".repeat(400)]);
        assert!(hint.len() <= 401);
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
