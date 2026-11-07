//! Who could have been speaking, and putting names to the voices the audio
//! alone could not.
//!
//! The calendar invite is the whole list of people a voice on the call can
//! belong to: [`invitees`] reads it, leaving out the owner (theirs is the
//! microphone, never a voice on the other end) and anything that is not a
//! person. Answers are kept, but only ever as a lean -- somebody who
//! declined can still turn up, and somebody who accepted can miss it.
//!
//! [`identify`](super::identify) clusters voices by how they sound, and can
//! name a cluster only from a voiceprint or by elimination. What it cannot
//! use is what people *said*: "thanks, Priya", "Dev, can you take that?",
//! somebody introducing themselves. [`resolve_request`] asks the model to
//! read for exactly that, and [`apply`] takes back only answers that name
//! somebody on the invite -- a name the model made up, or the owner's, is
//! dropped -- then folds labels that turned out to be one person together.
//! [`tidy`] has the last word: whatever is still unnamed is renumbered, and
//! never left as more voices than the invite has people.

use super::identify::parse_attendee;
use super::prompt::{Request, estimate_tokens};
use super::{Attribution, EventRef, Segment, Speaker};
use crate::mail::AttendeeResponse;
use std::collections::{BTreeMap, HashSet};

/// Somebody on the invite who is not the owner.
#[derive(Debug, Clone, PartialEq)]
pub struct Invitee {
    /// What to call them: the calendar's name, or one made from the address
    /// when the calendar gave none (`priya.raman@x.com` → "Priya Raman").
    pub name: String,
    pub email: Option<String>,
    /// `None` where the calendar did not say.
    pub response: Option<AttendeeResponse>,
    pub organizer: bool,
}

impl Invitee {
    /// As [`super::identify::Candidate::attendee`] wants it.
    pub fn attendee(&self) -> String {
        match &self.email {
            Some(email) => format!("{} <{email}>", self.name),
            None => self.name.clone(),
        }
    }

    pub fn declined(&self) -> bool {
        self.response == Some(AttendeeResponse::Declined)
    }

    /// "accepted", "declined", … for a prompt or the details block. Empty
    /// where the calendar did not say.
    pub fn answer(&self) -> &'static str {
        match self.response {
            Some(AttendeeResponse::Accepted) => "accepted",
            Some(AttendeeResponse::Tentative) => "maybe",
            Some(AttendeeResponse::Declined) => "declined",
            Some(AttendeeResponse::NeedsAction) => "no answer",
            None => "",
        }
    }

    /// Is `label` (a speaker's label) this person? By whole name, or by a
    /// voiceprint's shorter one ("Priya" for "Priya Raman").
    pub fn answers_to(&self, label: &str, email: Option<&str>) -> bool {
        if let (Some(mine), Some(theirs)) = (&self.email, email)
            && mine.eq_ignore_ascii_case(theirs)
        {
            return true;
        }
        let label = label.trim();
        !label.is_empty()
            && (self.name.eq_ignore_ascii_case(label)
                || self
                    .name
                    .split_whitespace()
                    .next()
                    .is_some_and(|f| f.eq_ignore_ascii_case(label)))
    }
}

/// Who the owner is, for leaving them off the invite.
#[derive(Debug, Clone, Default)]
pub struct Owner {
    /// Lower-cased: every address of every account in the vault.
    pub addresses: HashSet<String>,
    /// The profile's name; empty when none was typed.
    pub name: String,
}

impl Owner {
    fn is(&self, name: Option<&str>, email: Option<&str>) -> bool {
        if let Some(email) = email
            && self.addresses.contains(&email.to_ascii_lowercase())
        {
            return true;
        }
        let mine = self.name.trim();
        !mine.is_empty() && name.is_some_and(|n| n.trim().eq_ignore_ascii_case(mine))
    }
}

/// Everybody on `event`'s invite apart from the owner, each once, in the
/// calendar's order. Read from [`EventRef::guests`] where the calendar gave
/// them, else from the plain [`EventRef::attendees`] list (no answers).
pub fn invitees(event: &EventRef, owner: &Owner) -> Vec<Invitee> {
    let (organizer_name, organizer_email) = parse_attendee(&event.organizer);
    let is_organizer = |name: &str, email: Option<&str>| {
        email.is_some_and(|e| organizer_email.as_deref() == Some(e.to_ascii_lowercase().as_str()))
            || organizer_name
                .as_deref()
                .is_some_and(|o| !name.is_empty() && o.eq_ignore_ascii_case(name))
    };

    let mut out: Vec<Invitee> = Vec::new();
    let mut push = |name: Option<String>, email: Option<String>, response| {
        let email = email.map(|e| e.trim().to_ascii_lowercase()).filter(|e| !e.is_empty());
        let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
        if owner.is(name.as_deref(), email.as_deref()) {
            return;
        }
        let Some(name) = name.or_else(|| email.as_deref().map(name_from_email)) else { return };
        if out.iter().any(|i| {
            (email.is_some() && i.email == email)
                || (email.is_none() && i.name.eq_ignore_ascii_case(&name))
        }) {
            return;
        }
        let organizer = is_organizer(&name, email.as_deref());
        out.push(Invitee { name, email, response, organizer });
    };

    if event.guests.is_empty() {
        for raw in &event.attendees {
            let (name, email) = parse_attendee(raw);
            push(name, email, None);
        }
    } else {
        for guest in &event.guests {
            push(Some(guest.name.clone()), Some(guest.email.clone()), guest.response);
        }
    }
    out
}

/// A readable name from an address with no name beside it:
/// `priya.raman@x.com` → "Priya Raman", `dev@x.com` → "Dev".
pub fn name_from_email(email: &str) -> String {
    let local = email.split('@').next().unwrap_or(email);
    let local = local.split('+').next().unwrap_or(local);
    let words: Vec<String> = local
        .split(['.', '_', '-'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() { email.to_string() } else { words.join(" ") }
}

/// How sure the model was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

/// One answer: a label, and who the model thinks it is -- or which other
/// label it is the same voice as.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolution {
    pub label: String,
    pub name: Option<String>,
    pub same_as: Option<String>,
    pub confidence: Confidence,
}

/// The `[mm:ss] Label: text` line [`super::Transcript::line`] writes.
fn line(speakers: &[Speaker], seg: &Segment) -> String {
    let name =
        speakers.iter().find(|s| s.key == seg.speaker).map_or("Unknown", |s| s.label.as_str());
    let secs = seg.start_ms / 1000;
    format!("[{:02}:{:02}] {name}: {}", secs / 60, secs % 60, seg.text.trim())
}

/// The voices still to name: every speaker [`identify`](super::identify)
/// left [`Attribution::Unknown`].
/// "Others" -- several voices that could not be told apart -- is not one,
/// since no one name could be right for it.
fn unnamed(speakers: &[Speaker]) -> Vec<&Speaker> {
    speakers.iter().filter(|s| matches!(s.how, Attribution::Unknown) && !is_others(s)).collect()
}

/// The label for voices that could not be told apart.
pub const OTHERS: &str = "Others";

fn is_others(s: &Speaker) -> bool {
    s.label == OTHERS
}

const RESOLVE_RULES: &str = "\
You work out who is speaking in a meeting transcript.

The transcript came from automatic speech recognition. Words can be misheard \
(names especially), and the speaker labels came from software that tells \
voices apart by sound: it can split one person across several labels -- often \
at a gap of a few seconds -- and can occasionally put two people under one.

You are given the people invited, and which labels are still unnamed. For \
each unnamed label, decide who it is from what is said: people addressing \
each other by name (\"thanks, Priya\" usually means Priya spoke just before, \
\"Priya, what do you think?\" that she speaks next), introductions, people \
saying what they own or are working on, and the flow of the conversation.

Rules:
- Only ever answer with a name from the invited list, spelled as it is \
  there. Never the person recording; their lines are already labelled.
- If two labels are clearly the same voice but you cannot tell who, say so \
  with same_as instead of a name.
- An invitation answer is a hint, not a fact: someone who declined can still \
  join, and someone who accepted can miss it.
- Several labels may be the same person. Two different people are never one \
  label in your answer.
- high: the transcript says so. medium: strongly suggested. low: a guess. \
  When you cannot tell, say low with no name -- an honest unknown is better \
  than a wrong name.

Answer with JSON only, no commentary:
{\"speakers\": [{\"label\": \"Unknown 1\", \"name\": \"Priya Raman\" or null, \
\"same_as\": \"Unknown 3\" or null, \"confidence\": \"high\" | \"medium\" | \"low\"}]}";

/// The request that asks the model to put names to the voices still
/// unnamed, or `None` when there is nothing to ask: no unnamed voice, or
/// nobody on the invite to name them after.
///
/// The whole transcript goes when it fits `budget`; otherwise an excerpt
/// does -- each unnamed voice's first lines with the line before each, and
/// every line that says an invitee's first name with the line after it,
/// which is where most of the evidence is.
pub fn resolve_request(
    invitees: &[Invitee],
    owner_label: &str,
    speakers: &[Speaker],
    segments: &[Segment],
    budget: u32,
) -> Option<Request> {
    let unnamed = unnamed(speakers);
    if unnamed.is_empty() || invitees.is_empty() || segments.is_empty() {
        return None;
    }

    let mut user = String::from("Invited:\n");
    for person in invitees {
        user.push_str(&format!("- {}", person.name));
        let mut notes = Vec::new();
        if person.organizer {
            notes.push("organiser");
        }
        if !person.answer().is_empty() {
            notes.push(person.answer());
        }
        if !notes.is_empty() {
            user.push_str(&format!(" ({})", notes.join(", ")));
        }
        user.push('\n');
    }
    user.push_str(&format!(
        "\nThe person recording is labelled \"{owner_label}\".\n\nAlready named: "
    ));
    let named: Vec<&str> = speakers
        .iter()
        .filter(|s| !matches!(s.how, Attribution::Unknown | Attribution::Owner))
        .map(|s| s.label.as_str())
        .collect();
    user.push_str(if named.is_empty() { "nobody" } else { "" });
    user.push_str(&named.join(", "));
    user.push_str("\n\nStill unnamed: ");
    user.push_str(&unnamed.iter().map(|s| s.label.as_str()).collect::<Vec<_>>().join(", "));
    user.push_str("\n\nTranscript:\n\n");
    user.push_str(&excerpt(invitees, speakers, segments, budget));

    Some(Request { system: RESOLVE_RULES.to_string(), user })
}

/// See [`resolve_request`].
fn excerpt(
    invitees: &[Invitee],
    speakers: &[Speaker],
    segments: &[Segment],
    budget: u32,
) -> String {
    let all: Vec<String> = segments.iter().map(|s| line(speakers, s)).collect();
    let whole = all.join("\n");
    if estimate_tokens(&whole) <= budget {
        return whole;
    }

    let unnamed: HashSet<u16> = unnamed(speakers).iter().map(|s| s.key).collect();
    let first_names: Vec<String> = invitees
        .iter()
        .filter_map(|i| i.name.split_whitespace().next())
        .filter(|n| n.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect();
    let says_a_name = |text: &str| {
        let text = text.to_lowercase();
        text.split(|c: char| !c.is_alphanumeric()).any(|w| first_names.iter().any(|n| n == w))
    };

    let mut keep = vec![false; segments.len()];
    let mut per_speaker: BTreeMap<u16, usize> = BTreeMap::new();
    for (i, seg) in segments.iter().enumerate() {
        if unnamed.contains(&seg.speaker) {
            let seen = per_speaker.entry(seg.speaker).or_default();
            if *seen < 6 {
                *seen += 1;
                keep[i] = true;
                if i > 0 {
                    keep[i - 1] = true;
                }
            }
        }
        if says_a_name(&seg.text) {
            keep[i] = true;
            if i + 1 < segments.len() {
                keep[i + 1] = true;
            }
        }
    }

    let mut out = String::new();
    let mut gap = false;
    for (i, text) in all.iter().enumerate() {
        if !keep[i] {
            gap = true;
            continue;
        }
        if gap && !out.is_empty() {
            out.push_str("…\n");
        }
        gap = false;
        if estimate_tokens(&out) + estimate_tokens(text) > budget {
            break;
        }
        out.push_str(text);
        out.push('\n');
    }
    out
}

/// Read the model's answer. Anything that does not parse is no answer at
/// all, rather than an error: the voices stay as the audio left them.
pub fn parse_resolutions(answer: &str) -> Vec<Resolution> {
    let Some(start) = answer.find('{') else { return Vec::new() };
    let Some(end) = answer.rfind('}') else { return Vec::new() };
    if end < start {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&answer[start..=end]) else {
        return Vec::new();
    };
    let text = |v: &serde_json::Value, key: &str| {
        v.get(key)
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("null"))
            .map(str::to_string)
    };
    value
        .get("speakers")
        .and_then(|s| s.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|v| {
                    let label = text(v, "label")?;
                    let confidence = match text(v, "confidence").as_deref() {
                        Some("high") => Confidence::High,
                        Some("medium") => Confidence::Medium,
                        _ => Confidence::Low,
                    };
                    Some(Resolution {
                        label,
                        name: text(v, "name"),
                        same_as: text(v, "same_as"),
                        confidence,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Take the model's answers back: a name only when it is somebody on the
/// invite and the model was at least fairly sure, `same_as` only between
/// two voices that were both unnamed. Speakers that turn out to be one
/// person are folded into one, keeping the first one's key.
///
/// Only [`Attribution::Unknown`] voices are ever renamed -- a voiceprint
/// match, the owner and a name somebody typed are not the model's to
/// overrule -- but an unnamed voice found to be somebody already named is
/// folded into them.
pub fn apply(
    speakers: &mut Vec<Speaker>,
    segments: &mut [Segment],
    invitees: &[Invitee],
    resolutions: &[Resolution],
) {
    let key_of = |speakers: &[Speaker], label: &str| {
        speakers.iter().find(|s| s.label.eq_ignore_ascii_case(label.trim())).map(|s| s.key)
    };
    let is_unknown = |speakers: &[Speaker], key: u16| {
        speakers.iter().any(|s| s.key == key && matches!(s.how, Attribution::Unknown))
    };

    // Names first.
    for r in resolutions {
        if r.confidence == Confidence::Low {
            continue;
        }
        let (Some(name), Some(key)) = (&r.name, key_of(speakers, &r.label)) else { continue };
        if !is_unknown(speakers, key) {
            continue;
        }
        let Some(person) = invitees.iter().find(|i| i.answers_to(name, None)) else { continue };
        // Somebody already speaking under that name: fold into them.
        if let Some(existing) = speakers
            .iter()
            .find(|s| s.key != key && person.answers_to(&s.label, s.email.as_deref()))
            .map(|s| s.key)
        {
            merge(speakers, segments, key, existing);
            continue;
        }
        if let Some(s) = speakers.iter_mut().find(|s| s.key == key) {
            s.label = person.name.clone();
            s.email = person.email.clone();
            s.how = Attribution::Inferred;
        }
    }

    // Then voices that are each other, where neither got a name.
    for r in resolutions {
        if r.confidence == Confidence::Low {
            continue;
        }
        let Some(other) = &r.same_as else { continue };
        let (Some(from), Some(into)) = (key_of(speakers, &r.label), key_of(speakers, other)) else {
            continue;
        };
        if from != into && is_unknown(speakers, from) {
            merge(speakers, segments, from, into);
        }
    }
}

/// Fold speaker `from` into `into`: its lines become `into`'s, and it goes.
fn merge(speakers: &mut Vec<Speaker>, segments: &mut [Segment], from: u16, into: u16) {
    for seg in segments.iter_mut() {
        if seg.speaker == from {
            seg.speaker = into;
        }
    }
    let gone = speakers.iter().position(|s| s.key == from).map(|i| speakers.remove(i));
    if let (Some(gone), Some(kept)) = (gone, speakers.iter_mut().find(|s| s.key == into))
        && kept.centroid.is_empty()
    {
        kept.centroid = gone.centroid;
        kept.embedding_model = gone.embedding_model;
    }
}

/// The last word on unnamed voices. More of them than the invite has
/// people left to be is the audio splitting voices it could not tell apart
/// (or could not compare at all), so in that case they are folded into one,
/// "Others" -- one honest "somebody" rather than a crowd of strangers. Then
/// what remains unnamed is numbered "Unknown 1", "Unknown 2"… by first
/// speech, and a speaker nobody's lines point to any more is dropped.
///
/// `invited` is how many people besides the owner were invited; `None`
/// when there was no invite to count.
pub fn tidy(speakers: &mut Vec<Speaker>, segments: &mut [Segment], invited: Option<usize>) {
    let mut first_line: BTreeMap<u16, usize> = BTreeMap::new();
    for (i, seg) in segments.iter().enumerate() {
        first_line.entry(seg.speaker).or_insert(i);
    }
    speakers.retain(|s| first_line.contains_key(&s.key));

    let mut unknown: Vec<u16> =
        speakers.iter().filter(|s| matches!(s.how, Attribution::Unknown)).map(|s| s.key).collect();
    unknown.sort_by_key(|k| first_line.get(k).copied().unwrap_or(usize::MAX));

    if let Some(invited) = invited {
        let named_remote = speakers
            .iter()
            .filter(|s| !matches!(s.how, Attribution::Unknown | Attribution::Owner))
            .count();
        let room = invited.saturating_sub(named_remote).max(1);
        if unknown.len() > room {
            let into = speakers
                .iter()
                .find(|s| unknown.contains(&s.key) && is_others(s))
                .map_or(unknown[0], |s| s.key);
            for &from in unknown.iter().filter(|&&k| k != into) {
                merge(speakers, segments, from, into);
            }
            if let Some(s) = speakers.iter_mut().find(|s| s.key == into) {
                s.label = OTHERS.to_string();
            }
            return;
        }
    }
    let mut n = 0;
    for key in &unknown {
        if let Some(s) = speakers.iter_mut().find(|s| s.key == *key)
            && !is_others(s)
        {
            n += 1;
            s.label = format!("Unknown {n}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::Attendee;
    use crate::id::CalendarId;

    fn event(guests: Vec<Attendee>, attendees: &[&str]) -> EventRef {
        EventRef {
            calendar_id: CalendarId::new(),
            uid: "u".into(),
            title: "Design sync".into(),
            start: "2026-11-05T15:00:00Z".parse().unwrap(),
            end: "2026-11-05T15:30:00Z".parse().unwrap(),
            tz: "UTC".into(),
            organizer: "Priya Raman <priya@x.com>".into(),
            attendees: attendees.iter().map(|s| s.to_string()).collect(),
            guests,
            join_url: String::new(),
            calendar_name: "Work".into(),
            series: None,
        }
    }

    fn guest(email: &str, name: &str, response: AttendeeResponse) -> Attendee {
        Attendee { email: email.into(), name: name.into(), response: Some(response) }
    }

    fn owner() -> Owner {
        Owner { addresses: ["me@x.com".to_string()].into_iter().collect(), name: "Hari G".into() }
    }

    fn speaker(key: u16, label: &str, how: Attribution) -> Speaker {
        Speaker {
            key,
            label: label.into(),
            email: None,
            voiceprint_id: None,
            how,
            centroid: Vec::new(),
            embedding_model: String::new(),
        }
    }

    fn seg(at: u64, speaker: u16, text: &str) -> Segment {
        Segment { start_ms: at * 1000, end_ms: at * 1000 + 900, speaker, text: text.into() }
    }

    #[test]
    fn invitees_leave_out_the_owner_by_address_or_name_and_keep_answers() {
        let e = event(
            vec![
                guest("priya@x.com", "Priya Raman", AttendeeResponse::Accepted),
                guest("ME@x.com", "", AttendeeResponse::Accepted),
                guest("hari.g@other.com", "Hari G", AttendeeResponse::Accepted),
                guest("dev.patel@x.com", "", AttendeeResponse::Declined),
            ],
            &[],
        );
        let people = invitees(&e, &owner());
        let names: Vec<&str> = people.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Priya Raman", "Dev Patel"]);
        assert!(people[0].organizer);
        assert!(people[1].declined());
        assert_eq!(people[1].email.as_deref(), Some("dev.patel@x.com"));
    }

    #[test]
    fn invitees_fall_back_to_the_plain_list_without_answers() {
        let e = event(Vec::new(), &["Priya Raman", "bob@x.com", "me@x.com", "Priya Raman"]);
        let people = invitees(&e, &owner());
        let names: Vec<&str> = people.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Priya Raman", "Bob"]);
        assert!(people.iter().all(|p| p.response.is_none()));
    }

    #[test]
    fn names_are_made_from_addresses() {
        assert_eq!(name_from_email("priya.raman@x.com"), "Priya Raman");
        assert_eq!(name_from_email("dev_patel+cal@x.com"), "Dev Patel");
        assert_eq!(name_from_email("bob@x.com"), "Bob");
    }

    #[test]
    fn no_request_without_an_unnamed_voice_or_an_invite() {
        let people = invitees(&event(Vec::new(), &["Priya Raman"]), &owner());
        let named = vec![speaker(0, "Hari G", Attribution::Owner)];
        let segs = vec![seg(0, 0, "hello")];
        assert!(resolve_request(&people, "Hari G", &named, &segs, 1000).is_none());
        let unknown = vec![speaker(1, "Unknown 1", Attribution::Unknown)];
        assert!(resolve_request(&[], "Hari G", &unknown, &[seg(0, 1, "hi")], 1000).is_none());
        let req = resolve_request(&people, "Hari G", &unknown, &[seg(0, 1, "hi")], 1000).unwrap();
        assert!(req.user.contains("- Priya Raman (organiser)"), "{}", req.user);
        assert!(req.user.contains("Still unnamed: Unknown 1"));
        assert!(req.user.contains("[00:00] Unknown 1: hi"));
    }

    #[test]
    fn a_long_transcript_is_cut_to_the_lines_that_carry_evidence() {
        let people = invitees(&event(Vec::new(), &["Priya Raman"]), &owner());
        let speakers = vec![
            speaker(0, "Hari G", Attribution::Owner),
            speaker(1, "Unknown 1", Attribution::Unknown),
        ];
        let mut segs = Vec::new();
        for i in 0..400 {
            segs.push(seg(i, 0, "lots of filler talk that carries no names at all here"));
        }
        segs.push(seg(400, 0, "Thanks Priya, over to you"));
        segs.push(seg(401, 1, "Sure, so the launch moves to May"));
        let req = resolve_request(&people, "Hari G", &speakers, &segs, 300).unwrap();
        assert!(req.user.contains("Thanks Priya"), "{}", req.user);
        assert!(req.user.contains("launch moves to May"));
        assert!(estimate_tokens(&req.user) < 600, "{}", estimate_tokens(&req.user));
    }

    #[test]
    fn answers_parse_through_fences_and_junk() {
        let answer = "```json\n{\"speakers\": [\
            {\"label\": \"Unknown 1\", \"name\": \"Priya Raman\", \"same_as\": null, \"confidence\": \"high\"},\
            {\"label\": \"Unknown 2\", \"name\": null, \"same_as\": \"Unknown 1\", \"confidence\": \"medium\"},\
            {\"label\": \"Unknown 3\", \"confidence\": \"whatever\"}]}\n```";
        let r = parse_resolutions(answer);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].name.as_deref(), Some("Priya Raman"));
        assert_eq!(r[1].same_as.as_deref(), Some("Unknown 1"));
        assert_eq!(r[2].confidence, Confidence::Low);
        assert!(parse_resolutions("no idea").is_empty());
    }

    #[test]
    fn apply_names_merges_and_refuses_what_it_should() {
        let people = invitees(
            &event(
                vec![
                    guest("priya@x.com", "Priya Raman", AttendeeResponse::Accepted),
                    guest("bob@x.com", "Bob Lee", AttendeeResponse::Accepted),
                ],
                &[],
            ),
            &owner(),
        );
        let mut speakers = vec![
            speaker(0, "Hari G", Attribution::Owner),
            speaker(1, "Unknown 1", Attribution::Unknown),
            speaker(2, "Unknown 2", Attribution::Unknown),
            speaker(3, "Unknown 3", Attribution::Unknown),
            speaker(4, "Unknown 4", Attribution::Unknown),
            speaker(5, "Unknown 5", Attribution::Unknown),
        ];
        let mut segs: Vec<Segment> = (0..6).map(|k| seg(k as u64, k, "words")).collect();
        let r = |label: &str, name: Option<&str>, same_as: Option<&str>, confidence| Resolution {
            label: label.into(),
            name: name.map(String::from),
            same_as: same_as.map(String::from),
            confidence,
        };
        apply(
            &mut speakers,
            &mut segs,
            &people,
            &[
                r("Unknown 1", Some("priya raman"), None, Confidence::High),
                // The same person again: folded into Unknown 1, now Priya.
                r("Unknown 2", Some("Priya Raman"), None, Confidence::Medium),
                // Not on the invite, and the owner: both refused.
                r("Unknown 3", Some("Zed"), None, Confidence::High),
                r("Unknown 4", Some("Hari G"), None, Confidence::High),
                // Same voice as Unknown 3, which stays unnamed.
                r("Unknown 5", None, Some("Unknown 3"), Confidence::Medium),
                // A low-confidence guess is not taken.
                r("Unknown 3", Some("Bob Lee"), None, Confidence::Low),
            ],
        );
        let labels: Vec<&str> = speakers.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, vec!["Hari G", "Priya Raman", "Unknown 3", "Unknown 4"]);
        assert_eq!(speakers[1].how, Attribution::Inferred);
        assert_eq!(speakers[1].email.as_deref(), Some("priya@x.com"));
        let keys: Vec<u16> = segs.iter().map(|s| s.speaker).collect();
        assert_eq!(keys, vec![0, 1, 1, 3, 4, 3]);
    }

    #[test]
    fn others_stays_others_and_is_never_offered_a_name() {
        let mut speakers = vec![
            speaker(0, "Hari G", Attribution::Owner),
            speaker(1, "Others", Attribution::Unknown),
        ];
        let mut segs = vec![seg(0, 0, "a"), seg(1, 1, "b")];
        tidy(&mut speakers, &mut segs, None);
        assert_eq!(speakers[1].label, "Others");
        let people = invitees(&event(Vec::new(), &["Priya Raman", "Bob"]), &owner());
        assert!(resolve_request(&people, "Hari G", &speakers, &segs, 1000).is_none());
    }

    #[test]
    fn tidy_renumbers_and_folds_more_voices_than_the_invite_has_room_for() {
        let mut speakers = vec![
            speaker(0, "Hari G", Attribution::Owner),
            speaker(1, "Priya Raman", Attribution::Inferred),
            speaker(2, "Unknown 4", Attribution::Unknown),
            speaker(3, "Unknown 2", Attribution::Unknown),
            speaker(9, "Unknown 9", Attribution::Unknown),
        ];
        let mut segs = vec![seg(0, 0, "a"), seg(1, 3, "b"), seg(2, 1, "c"), seg(3, 2, "d")];
        // Two others invited besides Priya: both unknown voices fit.
        tidy(&mut speakers, &mut segs, Some(3));
        let labels: Vec<&str> = speakers.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, vec!["Hari G", "Priya Raman", "Unknown 2", "Unknown 1"]);

        // Only one other invited: the two unknown voices become "Others".
        tidy(&mut speakers, &mut segs, Some(2));
        let labels: Vec<&str> = speakers.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, vec!["Hari G", "Priya Raman", "Others"]);
        assert!(segs.iter().all(|s| s.speaker != 2));
    }
}
