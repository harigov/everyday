//! Templates: a shape to start a piece of writing from.
//!
//! One record serves three places -- a journal entry, a note and a meeting
//! note -- because a person who has written "how I want my Monday page to
//! look" and "how I want a one-to-one written up" has written the same kind
//! of thing twice, and should find both in one list. [`Template::uses`] says
//! where each one is offered.
//!
//! # What the body means
//!
//! Markdown, read in two passes.
//!
//! 1. **Placeholders** are filled by code. The ones every template has are
//!    about the day: `{{date}}`, `{{weekday}}`, `{{day}}`, `{{month}}`,
//!    `{{year}}`, `{{week}}` (the ISO week) and `{{quarter}}`. Any of them
//!    takes an offset -- `{{date+7d}}`, `{{weekday-1d}}`, `{{month+1m}}`,
//!    with `d`, `w`, `m` or `y` -- and `{{date}}` alone takes a format after a
//!    colon, `{{date:%A %-d %B}}`, in `strftime`'s words. A journal entry also
//!    has `{{journal}}`; a meeting note has the meeting's own, listed in
//!    [`MEETING_PLACEHOLDERS`] and filled by [`crate::meeting::template`].
//!    One the place does not know is left as typed.
//! 2. **Headings are kept, and the text under a heading is guidance for
//!    whoever writes that section.** In a meeting note that is the model,
//!    which replaces it. In a journal entry or a note it is the person, so
//!    the text goes in as it is -- "What am I grateful for?" -- and they
//!    write under it. A good journal prompt and a good instruction to a model
//!    turn out to be the same sentence, which is what lets one record serve
//!    both.
//!
//! # Choosing one for a day
//!
//! A journal does not name one template; it names a default and a list of
//! [`TemplateRule`]s -- Mondays, every other Friday, the first of the month,
//! the last Sunday of it -- tried in order, the first that matches winning.
//! See [`JournalTemplates`]. The rules live on the journal rather than in the
//! template, so a template stays plain text somebody can edit without
//! reading conditions in it, and the same "Weekly review" can be Sunday's in
//! one journal and Friday's in another.

use crate::error::{Error, Result};
use crate::id::TemplateId;
use crate::richtext::RichDoc;
use crate::timestamped::Timestamped;
use jiff::civil::Date;
use jiff::{Span, Timestamp};
use serde::{Deserialize, Serialize};

/// Where a template is offered.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TemplateUse {
    /// Starting a journal entry: a journal's default and its rules.
    Journal,
    /// "New note from template", and inserting one into a note.
    Notes,
    /// Writing up a recorded meeting. The model fills it in.
    Meetings,
}

impl TemplateUse {
    pub const ALL: [TemplateUse; 3] =
        [TemplateUse::Journal, TemplateUse::Notes, TemplateUse::Meetings];

    /// How a sentence names it: "is only filled in {}".
    fn place(self) -> &'static str {
        match self {
            TemplateUse::Journal => "a journal entry",
            TemplateUse::Notes => "a note",
            TemplateUse::Meetings => "meeting notes",
        }
    }
}

/// A template. See the module docs for what the body means.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub id: TemplateId,
    /// What to call it -- "Daily page", "Weekly review", "One-to-one".
    pub name: String,
    /// Where it is offered. Empty is allowed: a template kept for later and
    /// offered nowhere. A journal rule that names a template still applies
    /// whatever this says -- the rule was the choice; this decides only what
    /// the pickers list.
    #[serde(default)]
    pub uses: Vec<TemplateUse>,
    /// A title for the entry or note it starts, placeholders allowed --
    /// "Week {{week}} review". Empty leaves it untitled, so the first line
    /// stands in, as for any entry. Meeting notes ignore it: they are titled
    /// for what was discussed.
    #[serde(default)]
    pub title: String,
    /// Markdown. See the module docs.
    pub body: String,
    /// Tags the entry or note starts with.
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Longest a template's name may be. A label, like a skill's or a routine's.
pub const MAX_NAME_BYTES: usize = 200;

/// Longest a template's body may be. A page, generously: a template is read
/// in full every time a day starts, and is sent to a model with every
/// meeting it shapes.
pub const MAX_BODY_BYTES: usize = 20_000;

impl Template {
    pub fn new(name: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: TemplateId::new(),
            name: name.into(),
            uses: Vec::new(),
            title: String::new(),
            body: String::new(),
            tags: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn is_for(&self, place: TemplateUse) -> bool {
        self.uses.contains(&place)
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::Invalid("a template needs a name".into()));
        }
        if self.name.len() > MAX_NAME_BYTES {
            return Err(Error::Invalid(format!(
                "a template's name must be under {MAX_NAME_BYTES} bytes"
            )));
        }
        if self.title.len() > crate::note::MAX_TITLE_BYTES {
            return Err(Error::Invalid(format!(
                "a template's title must be under {} bytes",
                crate::note::MAX_TITLE_BYTES
            )));
        }
        if self.body.len() > MAX_BODY_BYTES {
            return Err(Error::Invalid(format!(
                "a template's text must be under {MAX_BODY_BYTES} bytes"
            )));
        }
        if self.body.trim().is_empty() && self.title.trim().is_empty() {
            return Err(Error::Invalid(
                "a template needs some text, or there is nothing for it to start".into(),
            ));
        }
        // A meeting note ignores the title, so a title is no stand-in there:
        // the model would be asked to write a call up in no shape at all.
        if self.body.trim().is_empty() && self.is_for(TemplateUse::Meetings) {
            return Err(Error::Invalid(
                "a template for meeting notes needs text: its headings are what the model keeps"
                    .into(),
            ));
        }
        Ok(())
    }

    /// The template filled in for `ctx`, ready to become an entry or a note.
    pub fn render(&self, ctx: &Context) -> Rendered {
        Rendered {
            title: fill(&self.title, ctx).trim().to_string(),
            body: RichDoc::from_markdown(&clamp_headings(&fill(&self.body, ctx))),
            tags: self.tags.clone(),
        }
    }
}

impl Timestamped for Template {
    fn touch(&mut self) {
        self.updated_at = Timestamp::now();
    }
}

/// A template filled in: what a new entry or note starts with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rendered {
    pub title: String,
    pub body: RichDoc,
    pub tags: Vec<String>,
}

// ---- placeholders ----------------------------------------------------------

/// The day's placeholders, which every template has.
pub const DATE_PLACEHOLDERS: &[&str] =
    &["date", "weekday", "day", "month", "year", "week", "quarter"];

/// The journal's own.
pub const JOURNAL_PLACEHOLDERS: &[&str] = &["journal"];

/// A meeting note's own, filled from the event and the recording by
/// [`crate::meeting::template::fill`]. Named here so [`lint`] can tell a
/// template meant for the journal that one of them would be left as typed.
pub const MEETING_PLACEHOLDERS: &[&str] =
    &["title", "start", "end", "duration", "organizer", "attendees", "present", "calendar"];

/// Markers only a meeting note understands, which code leaves in place for
/// the model: `{{topics}}` as a heading stands for one section per topic the
/// meeting covered -- see [`crate::meeting::template::TOPICS`].
pub const MEETING_MARKERS: &[&str] = &["topics"];

/// What a placeholder's text says, taken apart.
#[derive(Debug, Clone)]
pub struct Placeholder<'a> {
    /// Lower-cased: `{{Date}}` is `{{date}}`.
    pub name: String,
    /// `+7d`, `-1w`: moves the day before the value is taken.
    pub offset: Option<Span>,
    /// After a colon: `{{date:%A}}`. Only `date` takes one.
    pub format: Option<&'a str>,
}

/// Read `{{…}}`'s inside -- `date+7d:%A` -- or `None` if it is not shaped
/// like a placeholder at all.
pub fn parse_placeholder(raw: &str) -> Option<Placeholder<'_>> {
    let raw = raw.trim();
    let (head, format) = match raw.split_once(':') {
        Some((head, format)) => (head.trim(), Some(format)),
        None => (raw, None),
    };
    let name_len = head.bytes().take_while(u8::is_ascii_alphabetic).count();
    if name_len == 0 {
        return None;
    }
    let name = head[..name_len].to_ascii_lowercase();
    let rest = head[name_len..].trim();
    let offset = if rest.is_empty() { None } else { Some(parse_offset(rest)?) };
    Some(Placeholder { name, offset, format })
}

/// `+7d`, `-1w`, `+2m`, `-1y`, spaces allowed after the sign.
fn parse_offset(s: &str) -> Option<Span> {
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    let rest = rest.trim();
    let unit = rest.chars().last()?;
    // Digits only: a second sign or anything else is not an offset. Parsed
    // unsigned and bounded, so a typo cannot overflow on the way to a span
    // or ask jiff for one it refuses -- a century either way is more than
    // any template means.
    let digits = rest[..rest.len() - unit.len_utf8()].trim();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    if n > 36_500 {
        return None;
    }
    let n = sign * i64::from(n);
    let span = match unit.to_ascii_lowercase() {
        'd' => Span::new().try_days(n).ok()?,
        'w' => Span::new().try_weeks(n).ok()?,
        'm' => Span::new().try_months(n).ok()?,
        'y' => Span::new().try_years(n).ok()?,
        _ => return None,
    };
    Some(span)
}

/// Fill every `{{…}}` `lookup` has a value for and leave the rest as typed.
///
/// `lookup` is handed what is between the braces, untrimmed. An opening
/// `{{` with no closing one after it ends the filling: the rest is copied.
pub fn fill_with(text: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (span, raw) in placeholder_spans(text) {
        out.push_str(&text[copied..span.start]);
        match lookup(raw) {
            Some(value) => out.push_str(&value),
            None => out.push_str(&text[span.clone()]),
        }
        copied = span.end;
    }
    out.push_str(&text[copied..]);
    out
}

/// Every `{{…}}` in `text`, in order: where it sits, braces included, and
/// what is between the braces, untrimmed. The one scanner both filling and
/// [`lint`] use, so they cannot disagree about what counts as a
/// placeholder. An opening `{{` with no `}}` after it ends the scan.
fn placeholder_spans(text: &str) -> impl Iterator<Item = (std::ops::Range<usize>, &str)> {
    let mut from = 0;
    std::iter::from_fn(move || {
        let start = from + text[from..].find("{{")?;
        let end = start + 2 + text[start + 2..].find("}}")?;
        from = end + 2;
        Some((start..end + 2, &text[start + 2..end]))
    })
}

/// The value of one of the day's placeholders for `date`, or `None` if
/// `raw` is not one of them, its offset runs off the calendar, or its format
/// is not one `strftime` understands.
pub fn date_value(raw: &str, date: Date) -> Option<String> {
    let p = parse_placeholder(raw)?;
    if !DATE_PLACEHOLDERS.contains(&p.name.as_str()) {
        return None;
    }
    let date = match p.offset {
        Some(span) => date.checked_add(span).ok()?,
        None => date,
    };
    let formatted = |f: &str| jiff::fmt::strtime::format(f, date).ok();
    match (p.name.as_str(), p.format) {
        ("date", Some(f)) => formatted(f),
        (_, Some(_)) => None,
        ("date", None) => formatted("%A %-d %B %Y"),
        ("weekday", None) => formatted("%A"),
        ("day", None) => Some(date.day().to_string()),
        ("month", None) => formatted("%B"),
        ("year", None) => Some(date.year().to_string()),
        ("week", None) => Some(date.iso_week_date().week().to_string()),
        ("quarter", None) => Some(format!("Q{}", (date.month() - 1) / 3 + 1)),
        _ => None,
    }
}

/// What a journal entry or a note is filled from.
#[derive(Debug, Clone)]
pub struct Context {
    /// The day it is filed under: the entry's date, or today for a note.
    pub date: Date,
    /// The journal's name, for `{{journal}}`. `None` for a note.
    pub journal: Option<String>,
}

/// Fill the placeholders a journal entry or a note knows.
pub fn fill(text: &str, ctx: &Context) -> String {
    fill_with(text, |raw| {
        let p = parse_placeholder(raw)?;
        if p.name == "journal" && p.offset.is_none() && p.format.is_none() {
            return ctx.journal.clone();
        }
        date_value(raw, ctx.date)
    })
}

/// The editor draws three levels of heading. A deeper one -- `####` to
/// `######` -- would come back from it as a top-level heading, so it is
/// written as a third-level one instead. Fenced code is left alone.
fn clamp_headings(markdown: &str) -> String {
    let mut in_fence = false;
    let mut out = String::with_capacity(markdown.len());
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        let after = &trimmed[hashes.min(trimmed.len())..];
        if !in_fence
            && (4..=6).contains(&hashes)
            && (after.starts_with(' ') || after.starts_with('\t'))
        {
            out.push_str(&line[..line.len() - trimmed.len()]);
            out.push_str("###");
            out.push_str(after);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// The headings of a template, in order, as written (`## Decisions`).
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

/// What is wrong with a template, in words, for the editor to show.
///
/// Problems that make a template do something other than what it says --
/// a placeholder nothing fills, a meeting's attendees in a template the
/// journal uses -- not matters of taste. An empty list is a clean bill.
pub fn lint(template: &Template) -> Vec<String> {
    let mut issues = Vec::new();
    let both = format!("{}\n{}", template.title, template.body);

    if both.matches("{{").count() != both.matches("}}").count() {
        issues.push("Unbalanced braces: {{ and }} do not match.".to_string());
    }

    let mut seen = std::collections::BTreeSet::new();
    for (_, raw) in placeholder_spans(&both) {
        if seen.insert(raw.trim()) {
            issues.extend(lint_placeholder(raw, &template.uses));
        }
    }

    if template.is_for(TemplateUse::Meetings) && headings(&template.body).is_empty() {
        issues.push(
            "A template for meeting notes needs headings: they are what the model keeps, and \
             the text under each tells it what to write there."
                .to_string(),
        );
    }
    if template.uses.is_empty() {
        issues.push(
            "This template is not offered anywhere yet. Tick where it should be used.".to_string(),
        );
    }
    issues
}

fn lint_placeholder(raw: &str, uses: &[TemplateUse]) -> Option<String> {
    let shown = format!("{{{{{raw}}}}}");
    let Some(p) = parse_placeholder(raw) else {
        return Some(format!("Unknown placeholder {shown}"));
    };
    let name = p.name.as_str();
    let is_date = DATE_PLACEHOLDERS.contains(&name);
    let only_in = if MEETING_PLACEHOLDERS.contains(&name) || MEETING_MARKERS.contains(&name) {
        Some(TemplateUse::Meetings)
    } else if JOURNAL_PLACEHOLDERS.contains(&name) {
        Some(TemplateUse::Journal)
    } else if is_date {
        None
    } else {
        return Some(format!("Unknown placeholder {shown}"));
    };
    if (p.offset.is_some() || p.format.is_some()) && !is_date {
        return Some(format!("{shown}: only the day's placeholders take an offset or a format"));
    }
    if p.format.is_some() && name != "date" {
        return Some(format!("{shown}: only {{{{date}}}} takes a format"));
    }
    if is_date && date_value(raw, Date::constant(2026, 1, 15)).is_none() {
        return Some(format!("{shown}: that format is not one this understands"));
    }
    if let Some(place) = only_in {
        let elsewhere: Vec<&str> =
            uses.iter().filter(|u| **u != place).map(|u| u.place()).collect();
        if !elsewhere.is_empty() {
            return Some(format!(
                "{shown} is only filled in {}, so {} would show it as typed.",
                place.place(),
                elsewhere.join(" or ")
            ));
        }
    }
    None
}

// ---- choosing one for a day -----------------------------------------------

/// When a [`TemplateRule`] applies. Weekdays are ISO-numbered: 1 is Monday,
/// 7 is Sunday.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum When {
    /// On these days of the week.
    Weekdays { days: Vec<u8> },
    /// Every `every` weeks, counted from the week `from` falls in, on these
    /// days of it -- every day of it when `days` is empty. Counted from a
    /// day rather than by the ISO week's parity, which goes wrong every year
    /// that has a week 53.
    EveryWeeks { every: u8, from: Date, days: Vec<u8> },
    /// On this day of the month, or the last day of it for -1.
    MonthDay { day: i8 },
    /// On the `nth` `day` of the month -- 1 to 5, or -1 for the last.
    NthWeekday { nth: i8, day: u8 },
    /// On this date every year.
    YearDay { month: u8, day: u8 },
}

impl When {
    pub fn matches(&self, date: Date) -> bool {
        let weekday = date.weekday().to_monday_one_offset() as u8;
        match self {
            When::Weekdays { days } => days.contains(&weekday),
            When::EveryWeeks { every, from, days } => {
                let every = i64::from((*every).max(1));
                let weeks = monday_of(*from).until(monday_of(date)).map(|s| s.get_days() / 7);
                let on_week = weeks.is_ok_and(|w| (w as i64).rem_euclid(every) == 0);
                on_week && (days.is_empty() || days.contains(&weekday))
            }
            When::MonthDay { day: -1 } => date == date.last_of_month(),
            When::MonthDay { day } => i16::from(date.day()) == i16::from(*day),
            When::NthWeekday { nth, day } => {
                *day == weekday
                    && match nth {
                        -1 => date.day() + 7 > date.days_in_month(),
                        n => i16::from((date.day() - 1) / 7 + 1) == i16::from(*n),
                    }
            }
            When::YearDay { month, day } => {
                i16::from(date.month()) == i16::from(*month)
                    && i16::from(date.day()) == i16::from(*day)
            }
        }
    }

    pub fn validate(&self) -> Result<()> {
        let weekdays = |days: &[u8]| {
            if days.iter().all(|d| (1..=7).contains(d)) {
                Ok(())
            } else {
                Err(Error::Invalid("a weekday is a number from 1 (Monday) to 7 (Sunday)".into()))
            }
        };
        match self {
            When::Weekdays { days } if days.is_empty() => {
                Err(Error::Invalid("pick at least one day of the week".into()))
            }
            When::Weekdays { days } => weekdays(days),
            When::EveryWeeks { every, days, .. } => {
                if !(1..=52).contains(every) {
                    return Err(Error::Invalid("every 1 to 52 weeks".into()));
                }
                weekdays(days)
            }
            When::MonthDay { day } if *day == -1 || (1..=31).contains(day) => Ok(()),
            When::MonthDay { .. } => {
                Err(Error::Invalid("a day of the month is 1 to 31, or the last".into()))
            }
            When::NthWeekday { nth, day } => {
                if !(*nth == -1 || (1..=5).contains(nth)) {
                    return Err(Error::Invalid("first to fifth, or the last".into()));
                }
                weekdays(&[*day])
            }
            When::YearDay { month, day } => {
                if !(1..=12).contains(month) {
                    return Err(Error::Invalid("a month is 1 to 12".into()));
                }
                if Date::new(2024, *month as i8, *day as i8).is_err() {
                    return Err(Error::Invalid("that month has no such day".into()));
                }
                Ok(())
            }
        }
    }
}

fn monday_of(date: Date) -> Date {
    let back = i64::from(date.weekday().to_monday_zero_offset());
    date.checked_sub(Span::new().days(back)).unwrap_or(date)
}

/// One of a journal's rules: on these days, this template.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateRule {
    pub when: When,
    pub template: TemplateId,
}

/// Which template a journal starts each day's entry from.
///
/// The rules are tried in order and the first that matches the entry's date
/// wins; when none does, `default`; when there is none, a blank page. A rule
/// or a default naming a template that has since been deleted is passed
/// over rather than pruned, the way a journal's tracker ids are: deleting a
/// template should not have to rewrite every journal that used it.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalTemplates {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<TemplateId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<TemplateRule>,
}

/// More rules than anybody needs. Bounded because every new entry walks them.
pub const MAX_RULES: usize = 50;

impl JournalTemplates {
    pub fn is_empty(&self) -> bool {
        self.default.is_none() && self.rules.is_empty()
    }

    /// The template for an entry dated `date`, out of `all`.
    pub fn pick<'a>(&self, date: Date, all: &'a [Template]) -> Option<&'a Template> {
        let find = |id: TemplateId| all.iter().find(|t| t.id == id);
        self.rules
            .iter()
            .filter(|r| r.when.matches(date))
            .find_map(|r| find(r.template))
            .or_else(|| self.default.and_then(find))
    }

    pub fn validate(&self) -> Result<()> {
        if self.rules.len() > MAX_RULES {
            return Err(Error::Invalid(format!("a journal can have up to {MAX_RULES} rules")));
        }
        self.rules.iter().try_for_each(|r| r.when.validate())
    }
}

/// The template a meeting note is written from: the one asked for, else the
/// meetings' default, else the first offered for meetings, else the built-in
/// starter -- so a recording always has a shape, whatever has been deleted
/// since it started.
pub fn for_meeting(
    all: &[Template],
    asked: Option<TemplateId>,
    default: Option<TemplateId>,
) -> Template {
    let find = |id: TemplateId| all.iter().find(|t| t.id == id).cloned();
    asked
        .and_then(find)
        .or_else(|| default.and_then(find))
        .or_else(|| all.iter().find(|t| t.is_for(TemplateUse::Meetings)).cloned())
        .unwrap_or_else(crate::meeting::template::starter)
}

/// Start a new entry from `template`, filled in for the entry's date and its
/// journal: the title, the text and the tags. Whatever the entry held is
/// replaced, so this is for an entry nobody has typed into yet.
pub fn start_entry(
    entry: &mut crate::model::Entry,
    template: &Template,
    journal: &crate::model::Journal,
) {
    let r =
        template.render(&Context { date: entry.local_date, journal: Some(journal.name.clone()) });
    entry.title = r.title;
    entry.body = r.body;
    entry.tags = r.tags;
}

/// Add `more` to `tags`, leaving out any already there in another case --
/// "Boats" and "boats" are one tag to a person. What starting from, or
/// inserting, a template does to the tags already on an entry or a note.
pub fn merge_tags(tags: &mut Vec<String>, more: impl IntoIterator<Item = String>) {
    for tag in more {
        if !tags.iter().any(|have| have.eq_ignore_ascii_case(&tag)) {
            tags.push(tag);
        }
    }
}

/// What a template newly made for `uses` starts with, so nobody faces an
/// empty box: the meeting starter's text when it is for meetings, else a
/// short day page.
pub fn starting_body(uses: &[TemplateUse]) -> String {
    if uses.contains(&TemplateUse::Meetings) {
        crate::meeting::template::starter().body
    } else {
        STARTING_BODY.to_string()
    }
}

const STARTING_BODY: &str = "\
## Today
What today is for, in a sentence.

## On my mind

## Grateful for
";

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i16, m: i8, day: i8) -> Date {
        Date::new(y, m, day).unwrap()
    }

    fn ctx(date: Date) -> Context {
        Context { date, journal: Some("Daily".into()) }
    }

    #[test]
    fn the_days_placeholders_are_filled() {
        // Friday 13 November 2026, ISO week 46.
        let out = fill(
            "{{date}} | {{weekday}} | {{day}} | {{month}} | {{year}} | {{week}} | {{quarter}} | {{journal}}",
            &ctx(d(2026, 11, 13)),
        );
        assert_eq!(
            out,
            "Friday 13 November 2026 | Friday | 13 | November | 2026 | 46 | Q4 | Daily"
        );
    }

    #[test]
    fn offsets_and_formats_move_and_shape_the_day() {
        let c = ctx(d(2026, 11, 13));
        assert_eq!(fill("{{date+7d}}", &c), "Friday 20 November 2026");
        assert_eq!(fill("{{weekday-1d}}", &c), "Thursday");
        assert_eq!(fill("{{ week + 1w }}", &c), "47");
        assert_eq!(fill("{{month+2m}}", &c), "January");
        assert_eq!(fill("{{year-1y}}", &c), "2025");
        assert_eq!(fill("{{date:%a %-d %b}}", &c), "Fri 13 Nov");
        assert_eq!(fill("{{date+1d:%A}}", &c), "Saturday");
    }

    #[test]
    fn an_offset_that_is_not_plain_digits_is_not_an_offset() {
        let c = ctx(d(2026, 11, 13));
        assert_eq!(fill("{{date--9223372036854775808d}}", &c), "{{date--9223372036854775808d}}");
        assert_eq!(fill("{{date+-3d}}", &c), "{{date+-3d}}");
        assert_eq!(fill("{{date+99999d}}", &c), "{{date+99999d}}");
        assert!(lint_placeholder("date--9223372036854775808d", &[TemplateUse::Journal]).is_some());
    }

    #[test]
    fn headings_deeper_than_the_editor_draws_become_third_level() {
        let mut t = Template::new("Deep");
        t.body = "## Kept\n#### Gratitude\n```\n#### in code\n```\n######no space\n".into();
        let md = t.render(&ctx(d(2026, 11, 13))).body.to_markdown();
        assert!(md.contains("### Gratitude"), "{md}");
        assert!(!md.contains("#### Gratitude"), "{md}");
        assert!(md.contains("## Kept"), "{md}");
        assert!(md.contains("#### in code"), "fenced code is left alone: {md}");
    }

    #[test]
    fn tags_merge_without_repeating_one_in_another_case() {
        let mut tags = vec!["Boats".to_string()];
        merge_tags(&mut tags, ["boats".to_string(), "summer".to_string()]);
        assert_eq!(tags, vec!["Boats".to_string(), "summer".to_string()]);
    }

    #[test]
    fn what_cannot_be_filled_is_left_as_typed() {
        let c = Context { date: d(2026, 11, 13), journal: None };
        assert_eq!(
            fill("{{journal}} {{attendees}} {{nope}}", &c),
            "{{journal}} {{attendees}} {{nope}}"
        );
        assert_eq!(fill("{{weekday:%a}}", &c), "{{weekday:%a}}", "only date takes a format");
        assert_eq!(fill("{{date+3x}}", &c), "{{date+3x}}");
        assert_eq!(fill("open {{date", &c), "open {{date");
    }

    #[test]
    fn rendering_fills_title_and_body_and_carries_tags() {
        let mut t = Template::new("Weekly");
        t.title = "Week {{week}} review".into();
        t.body = "## {{weekday}}\nWhat went well?\n".into();
        t.tags = vec!["review".into()];
        let r = t.render(&ctx(d(2026, 11, 13)));
        assert_eq!(r.title, "Week 46 review");
        assert!(r.body.to_markdown().contains("Friday"));
        assert!(r.body.to_markdown().contains("What went well?"));
        assert_eq!(r.tags, vec!["review".to_string()]);
    }

    #[test]
    fn weekday_rules_match_their_days() {
        let mon = When::Weekdays { days: vec![1] };
        assert!(mon.matches(d(2026, 11, 9)));
        assert!(!mon.matches(d(2026, 11, 10)));
        let weekend = When::Weekdays { days: vec![6, 7] };
        assert!(weekend.matches(d(2026, 11, 14)));
        assert!(weekend.matches(d(2026, 11, 15)));
    }

    #[test]
    fn every_other_week_counts_from_its_first_week_across_a_year_end() {
        // 2026 has an ISO week 53, which a parity rule would get wrong.
        let r = When::EveryWeeks { every: 2, from: d(2026, 12, 21), days: vec![5] };
        assert!(r.matches(d(2026, 12, 25)), "the first week");
        assert!(!r.matches(d(2027, 1, 1)), "the week after");
        assert!(r.matches(d(2027, 1, 8)), "two weeks on");
        assert!(r.matches(d(2026, 12, 11)), "and two weeks before");
        assert!(!r.matches(d(2026, 12, 24)), "only on the days it names");
        let whole = When::EveryWeeks { every: 2, from: d(2026, 12, 21), days: vec![] };
        assert!(whole.matches(d(2026, 12, 22)));
    }

    #[test]
    fn month_rules_find_a_day_the_last_day_and_the_nth_weekday() {
        assert!(When::MonthDay { day: 1 }.matches(d(2026, 11, 1)));
        assert!(When::MonthDay { day: -1 }.matches(d(2026, 2, 28)));
        assert!(!When::MonthDay { day: -1 }.matches(d(2028, 2, 28)), "a leap year");
        assert!(When::MonthDay { day: -1 }.matches(d(2028, 2, 29)));
        // The first Monday of November 2026 is the 2nd; the last Sunday the 29th.
        assert!(When::NthWeekday { nth: 1, day: 1 }.matches(d(2026, 11, 2)));
        assert!(!When::NthWeekday { nth: 1, day: 1 }.matches(d(2026, 11, 9)));
        assert!(When::NthWeekday { nth: -1, day: 7 }.matches(d(2026, 11, 29)));
        assert!(!When::NthWeekday { nth: -1, day: 7 }.matches(d(2026, 11, 22)));
        assert!(When::YearDay { month: 12, day: 31 }.matches(d(2026, 12, 31)));
    }

    #[test]
    fn the_first_matching_rule_wins_then_the_default() {
        let daily = Template::new("Daily");
        let monday = Template::new("Monday");
        let gone = TemplateId::new();
        let all = vec![daily.clone(), monday.clone()];
        let jt = JournalTemplates {
            default: Some(daily.id),
            rules: vec![
                TemplateRule { when: When::Weekdays { days: vec![1] }, template: gone },
                TemplateRule { when: When::Weekdays { days: vec![1, 2] }, template: monday.id },
            ],
        };
        assert_eq!(
            jt.pick(d(2026, 11, 9), &all).unwrap().name,
            "Monday",
            "a deleted one is passed over"
        );
        assert_eq!(jt.pick(d(2026, 11, 11), &all).unwrap().name, "Daily");
        assert!(JournalTemplates::default().pick(d(2026, 11, 11), &all).is_none());
    }

    #[test]
    fn rules_that_name_impossible_days_are_refused() {
        assert!(When::Weekdays { days: vec![] }.validate().is_err());
        assert!(When::Weekdays { days: vec![8] }.validate().is_err());
        assert!(When::MonthDay { day: 0 }.validate().is_err());
        assert!(When::NthWeekday { nth: 6, day: 1 }.validate().is_err());
        assert!(When::YearDay { month: 2, day: 30 }.validate().is_err());
        assert!(When::YearDay { month: 2, day: 29 }.validate().is_ok());
        assert!(
            When::EveryWeeks { every: 0, from: d(2026, 1, 1), days: vec![] }.validate().is_err()
        );
    }

    #[test]
    fn lint_says_where_a_placeholder_would_be_left_as_typed() {
        let mut t = Template::new("Mixed");
        t.uses = vec![TemplateUse::Journal, TemplateUse::Notes];
        t.body =
            "## {{attendees}}\n{{journal}} {{date:%Q}} {{weekday:%a}} {{oops}} {{date+2w}}".into();
        let issues = lint(&t);
        assert!(issues.iter().any(|i| i.contains("{{attendees}} is only filled in meeting notes")));
        assert!(
            issues
                .iter()
                .any(|i| i.contains("{{journal}} is only filled in a journal entry, so a note"))
        );
        assert!(issues.iter().any(|i| i.contains("{{date:%Q}}")));
        assert!(issues.iter().any(|i| i.contains("only {{date}} takes a format")));
        assert!(issues.iter().any(|i| i.contains("Unknown placeholder {{oops}}")));
        assert!(!issues.iter().any(|i| i.contains("date+2w")));
    }

    #[test]
    fn the_topics_marker_belongs_to_meeting_notes() {
        let mut t = Template::new("By topic");
        t.uses = vec![TemplateUse::Meetings];
        t.body = "## {{topics}}\n## Next steps\n".into();
        assert!(lint(&t).is_empty(), "{:?}", lint(&t));
        t.uses.push(TemplateUse::Journal);
        assert!(lint(&t).iter().any(|i| i.contains("{{topics}} is only filled in meeting notes")));
        assert_eq!(fill("## {{topics}}", &ctx(d(2026, 11, 13))), "## {{topics}}");
    }

    #[test]
    fn lint_wants_headings_only_for_meetings_and_somewhere_to_be_offered() {
        let mut t = Template::new("Plain");
        t.body = "Just a line.".into();
        assert!(lint(&t).iter().any(|i| i.contains("not offered anywhere")));
        t.uses = vec![TemplateUse::Journal];
        assert!(lint(&t).is_empty());
        t.uses.push(TemplateUse::Meetings);
        assert!(lint(&t).iter().any(|i| i.contains("needs headings")));
    }

    #[test]
    fn a_meeting_falls_back_through_default_and_first_to_the_starter() {
        let mut first = Template::new("First");
        first.uses = vec![TemplateUse::Meetings];
        let mut chosen = Template::new("Chosen");
        chosen.uses = vec![TemplateUse::Journal];
        let all = vec![first.clone(), chosen.clone()];
        assert_eq!(for_meeting(&all, Some(chosen.id), None).name, "Chosen");
        assert_eq!(for_meeting(&all, Some(TemplateId::new()), Some(chosen.id)).name, "Chosen");
        assert_eq!(for_meeting(&all, None, None).name, "First");
        assert_eq!(for_meeting(&[], None, None).id, crate::meeting::template::starter().id);
    }

    #[test]
    fn a_template_needs_a_name_and_something_to_start() {
        let mut t = Template::new(" ");
        t.body = "x".into();
        assert!(t.validate().is_err());
        t.name = "Fine".into();
        assert!(t.validate().is_ok());
        t.body.clear();
        assert!(t.validate().is_err());
        t.title = "Just a title".into();
        assert!(t.validate().is_ok());
        t.uses = vec![TemplateUse::Journal, TemplateUse::Meetings];
        assert!(t.validate().is_err(), "meeting notes ignore the title, so it needs text");
    }
}
