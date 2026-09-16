//! Note templates.
//!
//! A template is Markdown. Placeholders (`{{title}}`, `{{date}}`, `{{start}}`,
//! `{{end}}`, `{{duration}}`, `{{organizer}}`, `{{attendees}}`,
//! `{{present}}`, `{{calendar}}`) are filled by code before the model sees
//! anything. After that, a heading is kept verbatim and the text under it is
//! an instruction the model replaces with content.

use super::identify::parse_attendee;
use super::{EventRef, NoteTemplate};
use crate::id::TemplateId;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::collections::HashSet;

/// What placeholders are filled from.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub title: String,
    pub event: Option<EventRef>,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    /// Labels of everybody who actually spoke, in order of first speech.
    pub present: Vec<String>,
    /// IANA zone to write times in.
    pub tz: String,
}

/// Every placeholder [`fill`] knows how to replace.
const KNOWN_PLACEHOLDERS: &[&str] =
    &["title", "date", "start", "end", "duration", "organizer", "attendees", "present", "calendar"];

/// A fixed id for the built-in starter template. It has to be a real id --
/// [`super::MeetingSettings::default`] calls [`starter`] fresh on every
/// install -- but not a *newly minted* one, or two installs, or a vault
/// reopened twice, would each get a template the other does not recognise as
/// the same one.
const STARTER_TEMPLATE_UUID: uuid::Uuid = uuid::uuid!("01980000-0000-7000-8000-6d656574696e");

/// The template a new install starts with.
pub fn starter() -> NoteTemplate {
    NoteTemplate {
        id: TemplateId(STARTER_TEMPLATE_UUID),
        name: "Meeting notes".to_string(),
        body: STARTER_BODY.to_string(),
    }
}

const STARTER_BODY: &str = "\
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
";

/// Replace every known placeholder. Unknown `{{…}}` are left alone.
pub fn fill(body: &str, facts: &Facts) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find("}}") else {
            // No closing brace anywhere after this: nothing more to fill.
            out.push_str(&rest[start..]);
            return out;
        };
        let key_raw = &after_open[..end];
        let key = key_raw.trim().to_ascii_lowercase();
        match placeholder_value(&key, facts) {
            Some(value) => out.push_str(&value),
            None => {
                out.push_str("{{");
                out.push_str(key_raw);
                out.push_str("}}");
            }
        }
        rest = &after_open[end + 2..];
    }
    out.push_str(rest);
    out
}

fn placeholder_value(key: &str, facts: &Facts) -> Option<String> {
    if !KNOWN_PLACEHOLDERS.contains(&key) {
        return None;
    }
    let tz = resolved_tz(&facts.tz);
    let (start, end) = event_times(facts);
    Some(match key {
        "title" => facts.title.clone(),
        "date" => start.map(|s| format_date(s, &tz)).unwrap_or_default(),
        "start" => start.map(|s| format_time(s, &tz)).unwrap_or_default(),
        "end" => end.map(|e| format_time(e, &tz)).unwrap_or_default(),
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
        _ => unreachable!("checked against KNOWN_PLACEHOLDERS above"),
    })
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

/// The headings of a (filled) template, in order, as written (`## Decisions`).
///
/// ATX headings only (`#` to `######`), and not ones inside a fenced code
/// block -- a template that shows its own Markdown as an example must not
/// have that example mistaken for a real heading.
pub fn headings(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) {
            let after = &trimmed[hashes..];
            if after.is_empty() || after.starts_with(' ') || after.starts_with('\t') {
                out.push(trimmed.trim_end().to_string());
            }
        }
    }
    out
}

fn normalise_heading(heading: &str) -> String {
    heading.trim_start_matches('#').trim().to_ascii_lowercase()
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
    let template_headings = headings(filled_template);
    if template_headings.is_empty() {
        return answer.to_string();
    }
    let present: HashSet<String> = headings(answer).iter().map(|h| normalise_heading(h)).collect();

    let mut missing_title: Option<&str> = None;
    let mut missing_rest: Vec<&str> = Vec::new();
    for (i, heading) in template_headings.iter().enumerate() {
        if present.contains(&normalise_heading(heading)) {
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
        if !event.attendees.is_empty() {
            lines.push(format!("- **Invited:** {}", attendees_display(&event.attendees)));
        }
    }
    if !facts.present.is_empty() {
        lines.push(format!("- **Spoke:** {}", facts.present.join(", ")));
    }
    if let Some(event) = &facts.event {
        if !event.join_url.is_empty() {
            lines.push(format!("- **Join link:** {}", event.join_url));
        }
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

/// Unknown placeholders and other problems, for the settings editor.
pub fn lint(body: &str) -> Vec<String> {
    let mut issues = Vec::new();

    if body.matches("{{").count() != body.matches("}}").count() {
        issues.push("Unbalanced braces: {{ and }} do not match.".to_string());
    }

    let mut rest = body;
    while let Some(start) = rest.find("{{") {
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find("}}") else { break };
        let key_raw = &after_open[..end];
        let key = key_raw.trim().to_ascii_lowercase();
        if !KNOWN_PLACEHOLDERS.contains(&key.as_str()) {
            issues.push(format!("Unknown placeholder {{{{{key_raw}}}}}"));
        }
        rest = &after_open[end + 2..];
    }

    if headings(body).is_empty() {
        issues.push("This template has no headings.".to_string());
    }

    issues
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
                join_url: "https://meet.google.com/abc-defg-hij".into(),
                calendar_name: "Work".into(),
            }),
            started_at: None,
            ended_at: None,
            present: vec!["Priya Raman".into(), "Bob".into()],
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
        let filled = fill(&starter().body, &facts());
        assert!(!filled.contains("{{"), "{filled}");
        assert!(filled.contains("# Design sync"));
        assert!(filled.contains("16 September 2026"));
        assert!(filled.contains("15:00")); // 14:00Z is 15:00 in Europe/London (BST)
        assert!(filled.contains("15:45"));
        assert!(filled.contains("Priya Raman, Bob"));

        // The starter template has no {{duration}}, but the placeholder itself works.
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
            tz: String::new(),
        };
        let block = details_block(&f);
        assert!(!block.contains("\n\n"), "no blank bullet lines: {block:?}");
        assert!(block.contains("**When:**"));
    }

    #[test]
    fn lint_finds_unknown_placeholders_unbalanced_braces_and_missing_headings() {
        let issues = lint("no headings, {{oops}}, and {{ still open");
        assert!(issues.iter().any(|i| i.contains("oops")), "{issues:?}");
        assert!(issues.iter().any(|i| i.contains("Unbalanced")), "{issues:?}");
        assert!(issues.iter().any(|i| i.contains("no headings")), "{issues:?}");
    }

    #[test]
    fn lint_is_quiet_about_a_clean_template() {
        assert!(lint(&starter().body).is_empty());
    }
}
