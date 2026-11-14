//! What the summarising model is asked, and how a long call is split.
//!
//! The shape of the note follows what made Granola's notes read well, as
//! the open-source Yogurt (github.com/FrigadeHQ/yogurt, MIT) spells it out:
//! sections named after what was talked about rather than fixed buckets,
//! short bullets that start with the point, decisions kept with their
//! topic, small talk left out, and next steps last, one person at a time.

use super::{Attribution, Transcript};

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

/// Split the transcript's turns (see [`Transcript::as_turns`]) into pieces
/// that each fit `budget`, only ever between turns. One piece if it fits.
///
/// Even pieces read better than a full one and a scrap, so each is first
/// aimed at an equal share of the whole. Turns cannot be cut to fill a share
/// exactly, though, and when aiming at the share would take more pieces --
/// and more requests -- than filling each up to the budget, the budget wins.
pub fn pieces(transcript: &Transcript, budget: u32) -> Vec<String> {
    let text = transcript.as_turns();
    if text.is_empty() {
        return vec![String::new()];
    }
    let total = estimate_tokens(&text);
    if total <= budget {
        return vec![text];
    }
    let budget = budget.max(1);
    let parts = total.div_ceil(budget);
    let even = pack(&text, total.div_ceil(parts).clamp(1, budget), budget);
    if even.len() as u32 <= parts { even } else { pack(&text, budget, budget) }
}

/// The turns of `text` packed into pieces of at most `target` tokens; a turn
/// longer than `budget` is cut on whitespace by [`split_long_line`].
fn pack(text: &str, target: u32, budget: u32) -> Vec<String> {
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
            && estimate_tokens(&current) + estimate_tokens(&with_newline) > target
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

/// Split one over-long turn on whitespace into pieces that each fit
/// `budget`, each piece after the first starting with who said it again.
/// A single word longer than the whole budget still becomes its own piece
/// -- there is nowhere else to cut it.
fn split_long_line(line: &str, budget: u32) -> Vec<String> {
    let who = line.split_once(": ").filter(|(w, _)| w.chars().count() <= 60).map(|(w, _)| w);
    let mut out = Vec::new();
    let mut current = String::new();
    for word in line.split_inclusive(' ') {
        if !current.is_empty() && estimate_tokens(&current) + estimate_tokens(word) > budget {
            out.push(std::mem::take(&mut current));
            if let Some(who) = who
                && estimate_tokens(who) * 4 < budget
            {
                current.push_str(who);
                current.push_str(": ");
            }
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

/// What every request is told about the material it is reading.
const ABOUT_THE_TRANSCRIPT: &str = "\
About the transcript:
- Each line is one turn: who spoke, then what they said.
- It came from automatic speech recognition, not a person. Expect misheard \
  words, wrong homophones, garbled names, numbers and product names, \
  sentences cut in half, and filler. Read through the noise: correct an \
  obvious mishearing from context and from how names are spelled in the \
  people list and the spelling line, and quietly drop what cannot be made \
  sense of.
- Speaker labels can be wrong, and one person can appear under more than one \
  label. A label such as \"Unknown 1\" or \"Others\" is a voice nobody could \
  put a name to: never present it as a person's name, and never give it an \
  action item -- say \"someone\" in prose, and \"Unassigned\" as an owner. \
  Who someone is can often be told from what is said: people address each \
  other by name, and say what they are working on.
- Where something that matters is unclear -- an amount, a date, who agreed, \
  who owns a task -- say so in a few words, \"(unclear)\", rather than guess. \
  Never build a decision or an action item on a fragment you cannot make \
  sense of.";

/// How the note should read.
const HOW_TO_WRITE: &str = "\
How to write, so the notes read at a glance:
- Keep every heading from the template, in the order it appears, and write \
  nothing outside those headings. The text under each heading in the \
  template is an instruction to you, not content to keep: replace it with \
  what actually happened. Where it shows a table, write a Markdown table \
  with exactly those columns.
- A heading written {{topics}} is not kept as it is: it stands for any \
  number of sections at that heading's level, one per topic, each under a \
  heading you write naming what it is about in a few specific words \
  (\"Pricing for the Acme renewal\", \"Desktop launch date\"), never \
  \"Discussion\", \"Updates\" or \"Topic 1\". Most important first. Bring \
  together what was said about one topic at different times. When people \
  took turns giving their own updates, a section per person (\"Maya's \
  update\") works well.
- Notes are about the substance: customers, deals and prices, numbers, \
  plans, problems, launches, what each person is working on, what was \
  decided. Leave out small talk, greetings, weekends and trips, jokes, \
  errands and other personal chat, and talk about the call itself (audio, \
  screens, who can hear whom). Something personal goes in only if the \
  meeting was about it.
- Bullets (\"- \") with real substance: what was said, found, proposed or \
  decided, and why. A bullet is one full point of up to about twenty words \
  that starts with the point, never with \"The team discussed\". More detail \
  goes in indented bullets under it.
- Keep the concrete details: names of people, customers, companies and \
  systems, numbers, prices, dates, ticket numbers and tools.
- A decision or an open question goes in the topic it belongs to, said \
  plainly (\"Decided: no paid placements for now\", \"Open: who owns the \
  migration\").
- Cover the whole meeting, the end as well. A short meeting gets fewer \
  sections, not thinner ones.
- **Bold** rarely: a deadline, a big number or a key term, a few times in all.
- The note already lists who attended. Do not list attendees again, and do \
  not open sentences with who was present; name a person where it matters \
  -- who decided something, who owns something, whose view differed.
- Use people's names as the people list spells them; the person who \
  recorded the call by their name too.
- Never invent a name, a decision, a number or a date that was not said.
- If a heading has nothing to say, write \"Nothing recorded.\" under it.
- Output Markdown only: no title of your own, no introduction, no code \
  fences, and nothing after the last section.";

/// What every request about one meeting carries besides its material.
#[derive(Debug, Clone, Copy, Default)]
pub struct Context<'a> {
    /// The template, placeholders filled -- see `template::fill`.
    pub template: &'a str,
    /// [`people_block`]'s, or empty.
    pub people: &'a str,
    /// `vocab::spelling_line`'s, or empty.
    pub spelling: &'a str,
    /// [`room_note`]'s, or empty.
    pub room: &'a str,
}

impl Context<'_> {
    /// The people, spelling and room blocks, each followed by a blank line,
    /// leaving out whichever is empty.
    fn preamble(&self) -> String {
        let mut out = String::new();
        for block in [self.people, self.spelling, self.room] {
            if !block.trim().is_empty() {
                out.push_str(block.trim());
                out.push_str("\n\n");
            }
        }
        out
    }
}

/// The request for the whole call, read in one go.
pub fn summarise(cx: &Context<'_>, lines: &str) -> Request {
    let system = format!(
        "You are writing meeting notes from a transcript, using the template you are \
         given.\n\n{ABOUT_THE_TRANSCRIPT}\n\n{HOW_TO_WRITE}"
    );
    let mut user = cx.preamble();
    user.push_str("Template:\n\n");
    user.push_str(cx.template);
    user.push_str("\n\nTranscript:\n\n");
    user.push_str(lines);
    Request { system, user }
}

/// The request for notes on one part of a call too long to read in one go.
/// Free-form and thorough rather than in the template's shape: the shape
/// is applied once, by [`from_parts`], to every part's notes together, so
/// nothing is lost to a section one part had nothing for.
pub fn part_notes(cx: &Context<'_>, lines: &str, n: usize, of: usize) -> Request {
    let system = format!(
        "You take compact notes on one part of a long meeting's transcript. Another \
         pass will write the final notes from every part's, in order.\n\n\
         Write notes on this part only: every topic discussed with its concrete \
         details (numbers, dates, names, customers, tools, examples), what was decided, \
         open questions, and who agreed to do what, by name. Short \"- \" bullets \
         grouped under a \"### \" line naming the topic. Leave out small talk. Nothing \
         else: no introduction, no closing line.\n\n{ABOUT_THE_TRANSCRIPT}"
    );
    let mut user = cx.preamble();
    user.push_str(&format!("This is part {n} of {of} of the transcript.\n\nTranscript:\n\n"));
    user.push_str(lines);
    Request { system, user }
}

/// The request that writes the note from every part's notes, in order.
pub fn from_parts(cx: &Context<'_>, parts: &[String]) -> Request {
    let system = format!(
        "You are writing meeting notes, using the template you are given, from notes \
         taken on each part of a long meeting, in order. Bring together what different \
         parts said about one topic, and never repeat a point or a next step two parts \
         both noted.\n\n{ABOUT_THE_TRANSCRIPT}\n\n{HOW_TO_WRITE}"
    );
    let mut user = cx.preamble();
    user.push_str("Template:\n\n");
    user.push_str(cx.template);
    push_parts(&mut user, parts);
    Request { system, user }
}

/// What a note was written from, for [`missing_sections`].
#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    Transcript(&'a str),
    Parts(&'a [String]),
}

/// The request for only the template's sections a first answer left out --
/// a model that stopped before the end, as one sometimes does. `written`
/// is that first answer.
pub fn missing_sections(
    cx: &Context<'_>,
    source: Source<'_>,
    written: &str,
    missing: &[String],
) -> Request {
    let system = format!(
        "You are finishing meeting notes someone else started, using the template you \
         are given.\n\n{ABOUT_THE_TRANSCRIPT}\n\n{HOW_TO_WRITE}"
    );
    let mut user = cx.preamble();
    user.push_str("Template:\n\n");
    user.push_str(cx.template);
    match source {
        Source::Transcript(lines) => {
            user.push_str("\n\nTranscript:\n\n");
            user.push_str(lines);
        }
        Source::Parts(parts) => push_parts(&mut user, parts),
    }
    user.push_str("\n\nNotes written so far:\n\n");
    user.push_str(written.trim());
    user.push_str(&format!(
        "\n\nThese sections of the template are missing from them: {}. Write only \
         those sections, each under its heading exactly as the template has it, \
         following the same rules. Nothing else.",
        missing.join(", ")
    ));
    Request { system, user }
}

fn push_parts(user: &mut String, parts: &[String]) {
    user.push_str("\n\nNotes on each part, in order:");
    for (i, part) in parts.iter().enumerate() {
        user.push_str(&format!("\n\nPart {} of {}:\n\n", i + 1, parts.len()));
        user.push_str(part.trim());
    }
}

/// From an answer to [`missing_sections`], the sections asked for and
/// nothing else -- not a section the first answer already had, written out
/// again -- or `None` when it wrote none of them.
pub fn take_sections(answer: &str, asked: &[String]) -> Option<String> {
    use super::template::{heading_key, is_heading_line};
    let answer = tidy_answer(answer);
    let wanted: Vec<String> = asked.iter().map(|h| heading_key(h)).collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut keeping = false;
    for line in answer.lines() {
        if is_heading_line(line) {
            keeping = wanted.contains(&heading_key(line));
        }
        if keeping {
            kept.push(line);
        }
    }
    let kept = kept.join("\n").trim().to_string();
    (!kept.is_empty()).then_some(kept)
}

/// An answer without the wrapping models add: a code fence around the
/// whole of it, and one line of chatter before the first heading ("Here are
/// the notes:"). Anything else before the first heading -- a summary a
/// template asked for, a topic written as a bold line -- is the note, and
/// stays, as does everything after the last heading ("Nothing recorded."
/// under a final heading is part of the note).
pub fn tidy_answer(answer: &str) -> String {
    let lines: Vec<&str> = answer
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("```") || t.starts_with("~~~"))
        })
        .collect();
    let first_text = lines.iter().position(|l| !l.trim().is_empty());
    let start = match first_text {
        Some(i)
            if is_chatter(lines[i])
                && lines[i + 1..].iter().any(|l| l.trim_start().starts_with('#')) =>
        {
            i + 1
        }
        _ => 0,
    };
    lines[start..].join("\n").trim().to_string()
}

/// A model's line of preamble: short, prose, and introducing what follows
/// -- it ends with a colon, or opens the way such lines do.
fn is_chatter(line: &str) -> bool {
    let t = line.trim();
    if t.starts_with(['#', '-', '*', '|', '>']) || t.chars().count() > 120 {
        return false;
    }
    let lower = t.to_lowercase();
    t.ends_with(':')
        || ["here are", "here is", "here's", "sure", "certainly", "below are", "below is"]
            .iter()
            .any(|p| lower.starts_with(p))
}

/// Who was on the call, for the summarising model: the person recording,
/// every voice heard and how sure the naming of it is, and everybody
/// invited who was not heard, with their answer. Empty when there is
/// nobody to say anything about.
pub fn people_block(transcript: &Transcript, invitees: &[super::people::Invitee]) -> String {
    let mut spoke: Vec<u16> = Vec::new();
    for seg in &transcript.segments {
        if !spoke.contains(&seg.speaker) {
            spoke.push(seg.speaker);
        }
    }
    let one_room_mic = transcript.room;
    let mut lines = Vec::new();
    for key in &spoke {
        let Some(s) = transcript.speaker(*key) else { continue };
        let how = match s.how {
            Attribution::Owner if one_room_mic => {
                "the microphone; if several people were in the room, any of them, whoever said it"
            }
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

/// What the summarising model needs to know about a meeting held in a room
/// on one microphone whose voices could not be told apart -- see
/// [`Transcript::room`]. Empty otherwise.
pub fn room_note(transcript: &Transcript) -> String {
    match transcript.owner() {
        Some(owner) if transcript.room => format!(
            "Nothing came through the computer's sound, so this was most likely a meeting \
             in a room heard on one microphone, and every line is labelled {} whoever said \
             it. If the lines read like several people talking, credit something to {} only \
             when the line itself makes clear who said it; otherwise leave out who said it, \
             or say \"someone\".",
            owner.label, owner.label
        ),
        _ => String::new(),
    }
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
            room: false,
            created_at: jiff::Timestamp::now(),
            updated_at: jiff::Timestamp::now(),
        }
    }

    #[test]
    fn a_short_transcript_is_one_piece() {
        let t = transcript_of(&[("A", "hello"), ("B", "hi there")]);
        let p = pieces(&t, 10_000);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0], t.as_turns());
    }

    #[test]
    fn an_empty_transcript_is_one_empty_piece() {
        let t = transcript_of(&[]);
        assert_eq!(pieces(&t, 100), vec![String::new()]);
    }

    #[test]
    fn a_long_transcript_splits_on_line_boundaries() {
        let lines: Vec<(&str, &str)> = (0..50)
            .map(|i| {
                (if i % 2 == 0 { "A" } else { "B" }, "this line is fairly long and repeats a bit")
            })
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
        assert_eq!(rejoined, t.as_turns());
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

    fn context<'a>(template: &'a str, people: &'a str) -> Context<'a> {
        Context { template, people, spelling: "", room: "" }
    }

    #[test]
    fn turns_join_one_speakers_fragments_and_drop_the_timestamps() {
        let t = transcript_of(&[("A", "so the"), ("A", "plan is"), ("B", "fine"), ("A", "good")]);
        // `transcript_of` numbers speakers alphabetically: A then B.
        assert_eq!(t.as_turns(), "A: so the plan is\nB: fine\nA: good\n");
    }

    #[test]
    fn summarise_carries_the_template_the_rules_and_the_transcript() {
        let req =
            summarise(&context("## {{topics}}\n...\n## Next steps", "People:\n- Priya"), "A: hi\n");
        assert!(req.system.contains("Keep every heading"));
        assert!(req.system.contains("speech recognition"));
        assert!(req.system.contains("{{topics}}"), "the marker is explained");
        assert!(req.system.contains("small talk"));
        assert!(req.user.starts_with("People:\n- Priya"));
        assert!(req.user.contains("## Next steps"));
        assert!(req.user.contains("A: hi"));
        assert!(!req.user.contains("Spelling"), "no spelling line was given");
    }

    #[test]
    fn spelling_and_the_room_go_with_every_request_when_given() {
        let cx = Context {
            template: "## Notes",
            people: "",
            spelling: "- Spelling: Kubernetes.",
            room: "Recorded in a room.",
        };
        for req in [
            summarise(&cx, "A: hi"),
            part_notes(&cx, "A: hi", 1, 2),
            from_parts(&cx, &["### X\n- y".into()]),
            missing_sections(
                &cx,
                Source::Transcript("A: hi"),
                "## Notes\n- a",
                &["## Next".into()],
            ),
        ] {
            assert!(req.user.contains("- Spelling: Kubernetes."), "{}", req.user);
            assert!(req.user.contains("Recorded in a room."), "{}", req.user);
        }
    }

    #[test]
    fn a_part_is_noted_freely_and_the_note_written_from_every_part() {
        let part = part_notes(&context("## {{topics}}", ""), "A: hi", 2, 3);
        assert!(part.user.contains("part 2 of 3"), "{}", part.user);
        assert!(!part.user.contains("{{topics}}"), "parts never see the template");
        assert!(part.system.contains("### "));

        let whole = from_parts(&context("## {{topics}}", ""), &["first".into(), "second".into()]);
        assert!(whole.user.contains("Part 1 of 2:\n\nfirst"), "{}", whole.user);
        assert!(whole.user.contains("Part 2 of 2:\n\nsecond"), "{}", whole.user);
        assert!(whole.user.contains("## {{topics}}"));
        assert!(whole.system.contains("Keep every heading"));
    }

    #[test]
    fn a_missing_section_is_asked_for_alone() {
        let req = missing_sections(
            &context("## {{topics}}\n## Next steps\n...", ""),
            Source::Parts(&["### Pricing\n- up".into()]),
            "## Pricing\n- Up 5%",
            &["## Next steps".into()],
        );
        assert!(req.user.contains("## Pricing\n- Up 5%"), "{}", req.user);
        assert!(req.user.contains("Write only"), "{}", req.user);
        assert!(req.user.contains("## Next steps"));
        assert!(req.user.contains("Part 1 of 1"));
    }

    #[test]
    fn only_the_sections_asked_for_are_taken_from_the_answer() {
        let asked = vec!["## Next steps".to_string()];
        assert_eq!(
            take_sections("Sure! Here they are:\n\n## Next steps\n- **Bo:** ship it\n", &asked),
            Some("## Next steps\n- **Bo:** ship it".to_string())
        );
        assert_eq!(take_sections("Nobody is doing anything.", &asked), None);
        assert_eq!(
            take_sections("```markdown\n## next steps\n- **Bo:** ship\n```", &asked),
            Some("## next steps\n- **Bo:** ship".to_string())
        );
        // A section it was not asked for, written again, is left out.
        assert_eq!(
            take_sections("## Decisions\n- x\n\n## Next steps\n- y\n\n## Pricing\n- z", &asked),
            Some("## Next steps\n- y".to_string())
        );
    }

    #[test]
    fn an_answer_loses_its_fences_and_its_preamble() {
        assert_eq!(
            tidy_answer("Here are the notes:\n\n```markdown\n## A\n- b\n```\n"),
            "## A\n- b"
        );
        // What a template asked for before its first heading is the note.
        let summary = "We agreed to ship on Friday. Pricing stays.\n\n## Action items\n- x";
        assert_eq!(tidy_answer(summary), summary);
        let bold = "**Pricing**\nWe agreed to raise prices 5%.\n\n## Next steps\n- x";
        assert_eq!(tidy_answer(bold), bold);
        let numbered = "1. Pricing up 5%\n\n## Next steps\n- x";
        assert_eq!(tidy_answer(numbered), numbered);
        assert_eq!(
            tidy_answer("## A\n- b\n\nNothing recorded.\n"),
            "## A\n- b\n\nNothing recorded."
        );
        assert_eq!(tidy_answer("Just prose, no headings."), "Just prose, no headings.");
    }

    #[test]
    fn a_room_heard_on_one_microphone_is_explained() {
        let mut one = transcript_of(&[("Hari", "we should ship")]);
        one.speakers[0].how = Attribution::Owner;
        assert_eq!(room_note(&one), "");
        one.room = true;
        let note = room_note(&one);
        assert!(note.contains("every line is labelled Hari whoever said it"), "{note}");
        assert!(note.contains("If the lines read like several people"), "{note}");
        let block = people_block(&one, &[]);
        assert!(block.contains("whoever said it"), "{block}");

        // A hybrid meeting: someone on the call besides the room's microphone.
        let mut hybrid = transcript_of(&[("Hari", "we should"), ("Priya", "agreed")]);
        hybrid.room = true;
        hybrid.speakers[0].how = Attribution::Owner;
        hybrid.speakers[1].how = Attribution::Inferred;
        assert!(room_note(&hybrid).contains("labelled Hari"));
        assert!(people_block(&hybrid, &[]).contains("- Priya -- spoke (worked out"));
    }

    #[test]
    fn pieces_are_even_but_never_more_than_filling_each_would_take() {
        let words = |n: usize| "word ".repeat(n);
        // 4900, 200 and 4900 tokens' worth (four characters a token).
        let (a, b, c) = (words(3_920), words(160), words(3_920));
        let t = transcript_of(&[("A", &a), ("B", &b), ("A", &c)]);
        let p = pieces(&t, 6_000);
        assert_eq!(p.len(), 2, "{:?}", p.iter().map(|x| estimate_tokens(x)).collect::<Vec<_>>());
        for piece in &p {
            assert!(estimate_tokens(piece) <= 6_000);
        }
    }
}
