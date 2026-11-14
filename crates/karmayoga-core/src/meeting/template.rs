//! What a template means in a meeting note.
//!
//! Templates themselves are [`crate::template::Template`]s, shared with the
//! journal and notes. What is particular to a meeting is here: its own
//! placeholders (`{{title}}`, `{{start}}`, `{{end}}`, `{{duration}}`,
//! `{{organizer}}`, `{{attendees}}`, `{{present}}`, `{{calendar}}`) on top
//! of the day's, all filled by code before the model sees anything; the
//! check that the model kept every heading; the details block written above
//! whatever the template says; and the built-in starter.
//!
//! One heading is not kept verbatim: `{{topics}}` (at any level) stands for
//! any number of sections at that level, one per topic the meeting covered,
//! each under a heading the model writes. Notes organised by what was talked
//! about read far better than the same material sorted into fixed buckets
//! ("Key points", "Decisions"), which tear a decision away from the topic it
//! settled. See [`TOPICS`].

use super::EventRef;
use super::identify::parse_attendee;
use crate::id::TemplateId;
use crate::template::{MEETING_PLACEHOLDERS, Template, TemplateUse, parse_placeholder};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::collections::HashSet;

pub use crate::template::headings;

/// What placeholders are filled from.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub title: String,
    pub event: Option<EventRef>,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    /// Labels of everybody who actually spoke, in order of first speech.
    pub present: Vec<String>,
    /// The addresses of those of them who have one -- a voiceprint's
    /// "Bob" is still the invitee "Bob Lee" by address.
    pub present_emails: Vec<String>,
    /// Everybody invited apart from the owner -- see
    /// [`super::people::invitees`]. Read by [`details_block`] to say who was
    /// invited but not heard.
    pub invitees: Vec<super::people::Invitee>,
    /// IANA zone to write times in.
    pub tz: String,
}

/// The heading text that stands for one section per topic -- see the module
/// doc. Left in place by [`fill`]; never required verbatim by
/// [`enforce_headings`]; taken out of an answer that copied it.
pub const TOPICS: &str = "{{topics}}";

/// Is `heading` (`## {{topics}}`, as [`headings`] returns it) the topics
/// marker?
pub fn is_topics_heading(heading: &str) -> bool {
    let text = heading.trim_start_matches('#').trim();
    text.strip_prefix("{{")
        .and_then(|t| t.strip_suffix("}}"))
        .is_some_and(|t| t.trim().eq_ignore_ascii_case("topics"))
}

/// A fixed id for the built-in starter template. It has to be a real id --
/// [`super::MeetingSettings::default`] calls [`starter`] fresh on every
/// install -- but not a *newly minted* one, or two installs, or a vault
/// reopened twice, would each get a template the other does not recognise as
/// the same one.
const STARTER_TEMPLATE_UUID: uuid::Uuid = uuid::uuid!("01980000-0000-7000-8000-6d656574696e");

/// The template a new install starts with, and what a meeting falls back to
/// when every template offered for meetings has been deleted.
pub fn starter() -> Template {
    let now = Timestamp::now();
    Template {
        id: TemplateId(STARTER_TEMPLATE_UUID),
        name: "Meeting notes".to_string(),
        uses: vec![TemplateUse::Meetings],
        title: String::new(),
        body: STARTER_BODY.to_string(),
        tags: Vec::new(),
        created_at: now,
        updated_at: now,
    }
}

const STARTER_BODY: &str = "\
## {{topics}}
One section per topic that was discussed, most important first, each headed \
by what it was about in a few specific words. When people took turns giving \
their own updates, a section per person. Under each, short bullets that start \
with the point -- what was said, found, proposed or decided, and why -- \
keeping names, numbers, dates and tools, with more detail in indented \
bullets, and decisions and open questions said plainly in the topic they \
belong to.

## Next steps
One bullet per thing someone said they would do after the meeting, all of \
one person's together, as \"- **Name:** what they will do, by when\" -- the \
when only if it was said, and \"**Unassigned:**\" when nobody took it on.
";

/// What [`STARTER_BODY`] used to be. September's repeated the title and
/// every name the details block already gives, and listed action items as
/// checkboxes nobody could scan; November's sorted everything into fixed
/// buckets (Summary, Key points, Decisions...), which read worse than notes
/// organised by topic. A vault whose starter template still reads exactly
/// one of these was never edited, so [`upgrade_starter`] may swap it for
/// the current one.
const OLD_STARTER_BODIES: &[&str] = &[
    "\
# {{title}}
{{date}}, {{start}}\u{2013}{{end}} \u{b7} {{present}}

## Summary
Three sentences: what the call was for and where it landed.

## Decisions
Bullets. Only what was actually agreed, with who agreed it.

## Action items
- [ ] Owner \u{2014} what \u{2014} by when, only if a date was said.

## Open questions
Bullets. What was raised and left unsettled.
",
    "\
## Summary
Two to four sentences: what this meeting was for, what was settled and what \
happens next. Concrete, and without listing who attended.

## Key points
Bullets, one line each: the substance of the discussion -- facts, numbers, \
options weighed and the reasons given.

## Decisions
Bullets. Only what was actually agreed, and who agreed it where that matters.

## Action items
A table, one row per action, with all of one person's rows together. Owner is \
\"Unassigned\" when nobody took it on; Due only if a date was said, else blank.

| Owner | Action | Due |
|---|---|---|

## Open questions
Bullets. What was raised and left unsettled, and who follows up if anyone said.
",
];

/// Bring the starter template up to date wherever it was never edited:
/// same id, and a body that is word for word an earlier starter's. One
/// somebody changed is theirs, and is left alone.
pub fn upgrade_starter(templates: &mut [Template]) {
    for t in templates.iter_mut() {
        if t.id == TemplateId(STARTER_TEMPLATE_UUID)
            && OLD_STARTER_BODIES.contains(&t.body.as_str())
        {
            t.body = STARTER_BODY.to_string();
        }
    }
}

/// Replace every placeholder a meeting knows -- its own and the day's, the
/// day being the meeting's -- and leave the rest as typed.
pub fn fill(body: &str, facts: &Facts) -> String {
    let tz = resolved_tz(&facts.tz);
    let day = event_times(facts).0.map(|start| start.to_zoned(tz.clone()).date());
    crate::template::fill_with(body, |raw| {
        let p = parse_placeholder(raw)?;
        if MEETING_PLACEHOLDERS.contains(&p.name.as_str()) {
            if p.offset.is_some() || p.format.is_some() {
                return None;
            }
            return Some(placeholder_value(&p.name, facts, &tz));
        }
        if crate::template::DATE_PLACEHOLDERS.contains(&p.name.as_str()) {
            // No day known at all: blank, as `{{start}}` is, rather than the
            // placeholder left lying in a note.
            return Some(match day {
                Some(day) => crate::template::date_value(raw, day)?,
                None => String::new(),
            });
        }
        None
    })
}

fn placeholder_value(key: &str, facts: &Facts, tz: &TimeZone) -> String {
    let (start, end) = event_times(facts);
    match key {
        "title" => facts.title.clone(),
        "start" => start.map(|s| format_time(s, tz)).unwrap_or_default(),
        "end" => end.map(|e| format_time(e, tz)).unwrap_or_default(),
        "duration" => match (start, end) {
            (Some(s), Some(e)) => format_duration(e.as_second() - s.as_second()),
            _ => String::new(),
        },
        "organizer" => facts.event.as_ref().map(|e| display_name(&e.organizer)).unwrap_or_default(),
        "attendees" => {
            facts.event.as_ref().map(|e| attendees_display(&e.attendees)).unwrap_or_default()
        }
        "present" => facts.present.join(", "),
        "calendar" => facts.event.as_ref().map(|e| e.calendar_name.clone()).unwrap_or_default(),
        _ => String::new(),
    }
}

/// The event's own start/end if there is one, else the recording's.
fn event_times(facts: &Facts) -> (Option<Timestamp>, Option<Timestamp>) {
    match &facts.event {
        Some(event) => (Some(event.start), Some(event.end)),
        None => (facts.started_at, facts.ended_at),
    }
}

/// `facts.tz`, or UTC when it is empty or not a zone jiff recognises.
fn resolved_tz(tz: &str) -> TimeZone {
    if tz.is_empty() {
        return TimeZone::UTC;
    }
    TimeZone::get(tz).unwrap_or(TimeZone::UTC)
}

/// "Wednesday 16 September 2026".
fn format_date(at: Timestamp, tz: &TimeZone) -> String {
    at.to_zoned(tz.clone()).strftime("%A %-d %B %Y").to_string()
}

/// "14:00".
fn format_time(at: Timestamp, tz: &TimeZone) -> String {
    at.to_zoned(tz.clone()).strftime("%H:%M").to_string()
}

/// "45 min" under an hour, "1 h" or "1 h 5 min" over one.
fn format_duration(seconds: i64) -> String {
    let minutes = seconds.max(0) / 60;
    let hours = minutes / 60;
    let mins = minutes % 60;
    if hours == 0 {
        format!("{mins} min")
    } else if mins == 0 {
        format!("{hours} h")
    } else {
        format!("{hours} h {mins} min")
    }
}

/// An attendee or organiser string, shown by name where there is one, else
/// by address, else as it was given.
fn display_name(raw: &str) -> String {
    let (name, email) = parse_attendee(raw);
    name.or(email).unwrap_or_else(|| raw.to_string())
}

fn attendees_display(attendees: &[String]) -> String {
    attendees.iter().map(|a| display_name(a)).collect::<Vec<_>>().join(", ")
}

/// What a heading says, for telling whether two are the same: without its
/// `#`s, bold marks, a trailing colon or full stop, case and runs of
/// spaces -- "## Next Steps:" is the template's "## Next steps".
pub fn heading_key(heading: &str) -> String {
    let text = heading.trim().trim_start_matches('#').trim();
    let text = text.trim_matches(|c| c == '*' || c == '_').trim();
    let text = text.trim_end_matches([':', '.']).trim();
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// A line written as a heading, by `#`s or -- as models sometimes do -- as
/// a line that is bold and nothing else ("**Next steps**").
pub fn is_heading_line(line: &str) -> bool {
    let t = line.trim();
    let hashes = t.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) {
        let after = &t[hashes..];
        return after.is_empty() || after.starts_with([' ', '\t']);
    }
    let bold = |mark: &str| {
        t.len() > 2 * mark.len()
            && t.starts_with(mark)
            && t.ends_with(mark)
            && !t[mark.len()..t.len() - mark.len()].contains(mark)
    };
    bold("**") || bold("__")
}

/// The keys of every heading in an answer, bold-line headings included,
/// outside code fences.
fn answer_heading_keys(answer: &str) -> HashSet<String> {
    let mut in_fence = false;
    let mut keys = HashSet::new();
    for line in answer.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence && is_heading_line(t) {
            keys.insert(heading_key(t));
        }
    }
    keys
}

/// Check the model kept the template's headings in order. Missing ones are
/// appended with "(not written)" so the gap shows; the answer is otherwise
/// kept as it came.
///
/// "Missing" means genuinely absent -- a heading the answer has, only in the
/// wrong place, is left where the model put it rather than moved, because
/// moving text around is more likely to mangle it than to help. The
/// template's own top-level `# title` heading is the one exception: if the
/// answer dropped it, it is put back at the very top rather than the bottom,
/// since a note that starts mid-body reads as broken in a way a missing
/// `## Open questions` at the end does not.
pub fn enforce_headings(filled_template: &str, answer: &str) -> String {
    let answer = without_topics_marker(answer);
    let template_headings = headings(filled_template);
    if template_headings.is_empty() {
        return answer;
    }
    let present = answer_heading_keys(&answer);

    let mut missing_title: Option<&str> = None;
    let mut missing_rest: Vec<&str> = Vec::new();
    for (i, heading) in template_headings.iter().enumerate() {
        if is_topics_heading(heading) || present.contains(&heading_key(heading)) {
            continue;
        }
        if i == 0 && heading.starts_with("# ") && !heading.starts_with("## ") {
            missing_title = Some(heading);
        } else {
            missing_rest.push(heading);
        }
    }

    let mut out = String::new();
    if let Some(title) = missing_title {
        out.push_str(title);
        out.push_str("\n\n");
    }
    out.push_str(answer.trim_end());
    for heading in missing_rest {
        out.push_str("\n\n");
        out.push_str(heading);
        out.push_str("\n(not written)");
    }
    out
}

/// The template's headings `answer` lacks, the ones worth asking the model
/// for again: never the topics marker, and never a leading `# title`, which
/// [`enforce_headings`] puts back itself.
pub fn missing_headings(filled_template: &str, answer: &str) -> Vec<String> {
    let present = answer_heading_keys(answer);
    headings(filled_template)
        .into_iter()
        .enumerate()
        .filter(|(i, h)| !(*i == 0 && h.starts_with("# ") && !h.starts_with("## ")))
        .filter(|(_, h)| !is_topics_heading(h) && !present.contains(&heading_key(h)))
        .map(|(_, h)| h)
        .collect()
}

/// `answer` without any heading line that is the topics marker itself --
/// a model that copied `## {{topics}}` instead of writing topic headings.
/// What was under it stays.
fn without_topics_marker(answer: &str) -> String {
    let mut in_fence = false;
    let mut out: Vec<&str> = Vec::new();
    for line in answer.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        if !in_fence && trimmed.starts_with('#') && is_topics_heading(trimmed.trim_end()) {
            continue;
        }
        out.push(line);
    }
    let mut joined = out.join("\n");
    if answer.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// The block code writes at the top of every meeting note, whatever the
/// template says: when, calendar, organizer, attendees, who spoke, join link.
/// Markdown.
pub fn details_block(facts: &Facts) -> String {
    let tz = resolved_tz(&facts.tz);
    let mut lines = Vec::new();

    if let Some(when) = when_line(facts, &tz) {
        lines.push(format!("- **When:** {when}"));
    }
    if let Some(event) = &facts.event {
        if !event.calendar_name.is_empty() {
            lines.push(format!("- **Calendar:** {}", event.calendar_name));
        }
        if !event.organizer.is_empty() {
            lines.push(format!("- **Organizer:** {}", display_name(&event.organizer)));
        }
    }
    if !facts.present.is_empty() {
        lines.push(format!("- **Spoke:** {}", facts.present.join(", ")));
    }
    // Each name once: whoever spoke is on the line above, so this one is
    // only the rest of the invite, with their answer where it says
    // something the reader cannot see -- that they declined, or might come.
    if !facts.invitees.is_empty() {
        let rest: Vec<String> = facts
            .invitees
            .iter()
            .filter(|i| {
                !facts.present.iter().any(|p| i.answers_to(p, None))
                    && !facts.present_emails.iter().any(|e| i.answers_to("", Some(e)))
            })
            .map(|i| match i.answer() {
                a @ ("declined" | "maybe" | "no answer") => format!("{} ({a})", i.name),
                _ => i.name.clone(),
            })
            .collect();
        if !rest.is_empty() {
            let heading = if facts.present.is_empty() { "Invited" } else { "Also invited" };
            lines.push(format!("- **{heading}:** {}", rest.join(", ")));
        }
    } else if let Some(event) = &facts.event
        && !event.attendees.is_empty()
    {
        lines.push(format!("- **Invited:** {}", attendees_display(&event.attendees)));
    }
    if let Some(event) = &facts.event
        && !event.join_url.is_empty()
    {
        lines.push(format!("- **Join link:** {}", event.join_url));
    }
    lines.join("\n")
}

fn when_line(facts: &Facts, tz: &TimeZone) -> Option<String> {
    let (start, end) = event_times(facts);
    let start = start?;
    let date = format_date(start, tz);
    let start_time = format_time(start, tz);
    let tz_name = tz.iana_name().unwrap_or("UTC");
    match end {
        Some(end) => {
            let end_time = format_time(end, tz);
            Some(format!("{date}, {start_time}\u{2013}{end_time} ({tz_name})"))
        }
        None => Some(format!("{date}, {start_time} ({tz_name})")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            title: "Design sync".into(),
            event: Some(EventRef {
                calendar_id: crate::id::CalendarId::new(),
                uid: "u1".into(),
                title: "Design sync".into(),
                start: "2026-09-16T14:00:00Z".parse().unwrap(),
                end: "2026-09-16T14:45:00Z".parse().unwrap(),
                tz: "Europe/London".into(),
                organizer: "Priya Raman <priya@example.com>".into(),
                attendees: vec![
                    "Priya Raman <priya@example.com>".into(),
                    "bob@example.com".into(),
                    "Just A Name".into(),
                ],
                guests: Vec::new(),
                join_url: "https://meet.google.com/abc-defg-hij".into(),
                calendar_name: "Work".into(),
                series: None,
            }),
            started_at: None,
            ended_at: None,
            present: vec!["Priya Raman".into(), "Bob".into()],
            present_emails: Vec::new(),
            invitees: Vec::new(),
            tz: "Europe/London".into(),
        }
    }

    #[test]
    fn starter_has_a_fixed_id_across_calls() {
        assert_eq!(starter().id, starter().id);
        assert_eq!(starter().name, "Meeting notes");
        assert!(!headings(&starter().body).is_empty());
    }

    #[test]
    fn every_known_placeholder_is_filled() {
        let filled = fill(OLD_STARTER_BODIES[0], &facts());
        assert!(!filled.contains("{{"), "{filled}");
        assert!(filled.contains("# Design sync"));
        assert!(filled.contains("16 September 2026"));
        assert!(filled.contains("15:00")); // 14:00Z is 15:00 in Europe/London (BST)
        assert!(filled.contains("15:45"));
        assert!(filled.contains("Priya Raman, Bob"));

        // That template has no {{duration}}, but the placeholder itself works.
        assert_eq!(fill("{{duration}}", &facts()), "45 min");
    }

    #[test]
    fn whitespace_inside_braces_is_tolerated() {
        let out = fill("{{ title }} and {{title}}", &facts());
        assert_eq!(out, "Design sync and Design sync");
    }

    #[test]
    fn an_unknown_placeholder_is_left_alone() {
        let out = fill("{{title}} / {{mystery}}", &facts());
        assert_eq!(out, "Design sync / {{mystery}}");
    }

    #[test]
    fn attendees_prefer_names_and_fall_back_to_addresses() {
        let out = fill("{{attendees}}", &facts());
        assert_eq!(out, "Priya Raman, bob@example.com, Just A Name");
    }

    #[test]
    fn duration_reads_hours_and_minutes() {
        assert_eq!(format_duration(45 * 60), "45 min");
        assert_eq!(format_duration(65 * 60), "1 h 5 min");
        assert_eq!(format_duration(120 * 60), "2 h");
    }

    #[test]
    fn ad_hoc_calls_fall_back_to_started_and_ended() {
        let f = Facts {
            title: "Quick chat".into(),
            event: None,
            started_at: Some("2026-09-16T09:00:00Z".parse().unwrap()),
            ended_at: Some("2026-09-16T09:20:00Z".parse().unwrap()),
            present: vec!["Someone".into()],
            present_emails: Vec::new(),
            invitees: Vec::new(),
            tz: String::new(),
        };
        let out = fill("{{date}} {{start}}-{{end}} {{duration}}", &f);
        assert!(out.contains("09:00"), "{out}");
        assert!(out.contains("09:20"), "{out}");
        assert!(out.contains("20 min"), "{out}");

        let block = details_block(&f);
        assert!(block.contains("**When:**"));
        assert!(block.contains("**Spoke:** Someone"));
        assert!(!block.contains("**Calendar:**"), "no event means no calendar line: {block}");
    }

    #[test]
    fn headings_are_found_in_order_and_fenced_examples_are_ignored() {
        let body = "# Title\n\n## Summary\ntext\n\n```\n## Not a heading\n```\n\n## Decisions\n";
        assert_eq!(
            headings(body),
            vec!["# Title".to_string(), "## Summary".to_string(), "## Decisions".to_string()]
        );
    }

    #[test]
    fn enforce_headings_appends_only_what_is_missing_and_leaves_order_alone() {
        let template =
            "# {{title}}\n\n## Summary\n...\n\n## Decisions\n...\n\n## Action items\n...\n";
        let filled_template = fill(template, &facts());

        // The model answered out of order and dropped "Decisions" entirely.
        let answer = "# Design sync\n\n## Action items\n- [ ] Bob — ship it — Friday\n\n## Summary\nWent well.\n";
        let out = enforce_headings(&filled_template, answer);
        assert!(out.starts_with("# Design sync"), "{out}");
        assert!(out.contains("## Action items"));
        assert!(out.contains("## Summary"));
        // Present but out of order: not treated as missing, not duplicated.
        assert_eq!(out.matches("## Summary").count(), 1);
        // Missing: appended with the "not written" marker.
        assert!(out.trim_end().ends_with("## Decisions\n(not written)"), "{out}");
    }

    #[test]
    fn enforce_headings_prepends_a_missing_title_rather_than_appending_it() {
        let template = "# {{title}}\n\n## Summary\n...\n";
        let filled_template = fill(template, &facts());
        let answer = "## Summary\nIt went fine.\n";
        let out = enforce_headings(&filled_template, answer);
        assert!(out.starts_with("# Design sync\n\n## Summary"), "{out}");
    }

    #[test]
    fn details_block_omits_empty_lines() {
        let f = Facts {
            title: "Bare".into(),
            event: None,
            started_at: Some("2026-09-16T09:00:00Z".parse().unwrap()),
            ended_at: None,
            present: Vec::new(),
            present_emails: Vec::new(),
            invitees: Vec::new(),
            tz: String::new(),
        };
        let block = details_block(&f);
        assert!(!block.contains("\n\n"), "no blank bullet lines: {block:?}");
        assert!(block.contains("**When:**"));
    }

    #[test]
    fn details_name_each_person_once_with_the_answers_worth_saying() {
        use crate::mail::AttendeeResponse;
        use crate::meeting::people::Invitee;
        let invitee = |name: &str, response| Invitee {
            name: name.into(),
            email: None,
            response,
            organizer: false,
        };
        let mut f = facts();
        f.present_emails = vec!["bob@example.com".into()];
        f.invitees = vec![
            invitee("Priya Raman", Some(AttendeeResponse::Accepted)),
            Invitee {
                email: Some("bob@example.com".into()),
                ..invitee("Bob Lee", Some(AttendeeResponse::Accepted))
            },
            invitee("Dev Patel", Some(AttendeeResponse::Declined)),
            invitee("Eve Ng", Some(AttendeeResponse::Accepted)),
        ];
        let block = details_block(&f);
        assert!(block.contains("- **Spoke:** Priya Raman, Bob\n"), "{block}");
        assert!(block.contains("- **Also invited:** Dev Patel (declined), Eve Ng"), "{block}");
        assert!(!block.contains("**Invited:**"), "{block}");
    }

    #[test]
    fn an_unedited_starter_is_upgraded_and_an_edited_one_is_not() {
        let old = Template { body: OLD_STARTER_BODIES[0].to_string(), ..starter() };
        let edited = Template { body: "## Mine\n".into(), ..starter() };
        let other = Template {
            id: TemplateId::new(),
            body: OLD_STARTER_BODIES[0].to_string(),
            name: "Copy".into(),
            ..starter()
        };
        let mut templates = vec![old, edited, other];
        upgrade_starter(&mut templates);
        assert_eq!(templates[0].body, STARTER_BODY);
        assert_eq!(templates[1].body, "## Mine\n");
        assert_eq!(templates[2].body, OLD_STARTER_BODIES[0], "only the starter's own id");
    }

    #[test]
    fn the_starter_is_organised_by_topic_and_ends_with_next_steps() {
        let body = starter().body;
        let hs = headings(&body);
        assert!(is_topics_heading(&hs[0]), "{hs:?}");
        assert_eq!(hs.last().map(String::as_str), Some("## Next steps"));
        assert!(body.contains("**Name:**"), "{body}");
        assert!(!body.contains("{{present}}"), "names are in the details block already");
    }

    #[test]
    fn every_earlier_starter_is_upgraded() {
        for old in OLD_STARTER_BODIES {
            let mut templates = vec![Template { body: old.to_string(), ..starter() }];
            upgrade_starter(&mut templates);
            assert_eq!(templates[0].body, STARTER_BODY);
        }
        assert_eq!(OLD_STARTER_BODIES.len(), 2);
        assert!(OLD_STARTER_BODIES[1].contains("| Owner | Action | Due |"));
    }

    #[test]
    fn the_topics_marker_is_filled_by_the_model_not_by_code() {
        let filled = fill(&starter().body, &facts());
        assert!(filled.contains("## {{topics}}"), "{filled}");
        assert!(is_topics_heading("## {{ Topics }}"));
        assert!(!is_topics_heading("## Topics"));

        // Topic headings of the model's own satisfy it; Next steps is still owed.
        let answer = "## Pricing for Acme\n- Up 5%\n\n## Desktop launch\n- May\n";
        assert_eq!(missing_headings(&filled, answer), vec!["## Next steps".to_string()]);
        let out = enforce_headings(&filled, answer);
        assert!(!out.contains("{{topics}}"), "{out}");
        assert!(out.trim_end().ends_with("## Next steps\n(not written)"), "{out}");
    }

    #[test]
    fn a_copied_topics_marker_is_taken_out_and_its_text_kept() {
        let filled = fill(&starter().body, &facts());
        let answer = "## {{topics}}\n- Pricing up 5%\n\n## Next steps\n- **Bo:** send it\n";
        let out = enforce_headings(&filled, answer);
        assert!(!out.contains("{{topics}}"), "{out}");
        assert!(out.contains("- Pricing up 5%"), "{out}");
    }

    #[test]
    fn a_missing_title_is_not_asked_for_again() {
        let filled = fill("# {{title}}\n\n## Summary\n...\n", &facts());
        assert!(missing_headings(&filled, "## Summary\nFine.\n").is_empty());
        assert_eq!(missing_headings(&filled, "Fine.\n"), vec!["## Summary".to_string()]);
    }

    #[test]
    fn lint_is_quiet_about_a_clean_template() {
        assert!(crate::template::lint(&starter()).is_empty());
    }

    #[test]
    fn the_days_placeholders_are_the_meetings_day() {
        // 14:00Z on the 16th is the 16th in London; a meeting at 23:30Z on
        // the 16th is already the 17th in Tokyo.
        assert_eq!(fill("{{weekday}} {{week}} {{date+1d:%-d}}", &facts()), "Wednesday 38 17");
        let mut late = facts();
        late.event.as_mut().unwrap().start = "2026-09-16T23:30:00Z".parse().unwrap();
        late.event.as_mut().unwrap().end = "2026-09-17T00:00:00Z".parse().unwrap();
        late.tz = "Asia/Tokyo".into();
        assert_eq!(fill("{{date}}", &late), "Thursday 17 September 2026");
        let mut none = facts();
        none.event = None;
        assert_eq!(fill("[{{date}}] {{title:%A}}", &none), "[] {{title:%A}}");
    }

    #[test]
    fn a_heading_written_a_little_differently_is_not_missing() {
        let filled = fill(&starter().body, &facts());
        for written in ["## Next Steps:", "**Next steps**", "### next   steps", "__Next steps:__"] {
            let answer = format!("## Pricing\n- Up 5%\n\n{written}\n- **Bo:** send it\n");
            assert!(missing_headings(&filled, &answer).is_empty(), "{written}");
            assert!(!enforce_headings(&filled, &answer).contains("(not written)"), "{written}");
        }
        assert!(!is_heading_line("**Bo:** send it"));
        assert!(!is_heading_line("- **Next steps**"));
        assert_eq!(heading_key("## Next Steps:"), "next steps");
    }
}
