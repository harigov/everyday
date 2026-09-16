//! Note templates.
//!
//! A template is Markdown. Placeholders (`{{title}}`, `{{date}}`, `{{start}}`,
//! `{{end}}`, `{{duration}}`, `{{organizer}}`, `{{attendees}}`,
//! `{{present}}`, `{{calendar}}`) are filled by code before the model sees
//! anything. After that, a heading is kept verbatim and the text under it is
//! an instruction the model replaces with content.

use super::{EventRef, NoteTemplate};
use jiff::Timestamp;

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

/// The template a new install starts with.
pub fn starter() -> NoteTemplate {
    todo!("core-logic agent")
}

/// Replace every known placeholder. Unknown `{{…}}` are left alone.
pub fn fill(body: &str, facts: &Facts) -> String {
    let _ = (body, facts);
    todo!("core-logic agent")
}

/// The headings of a (filled) template, in order, as written (`## Decisions`).
pub fn headings(body: &str) -> Vec<String> {
    let _ = body;
    todo!("core-logic agent")
}

/// Check the model kept the template's headings in order. Missing ones are
/// appended with "(not written)" so the gap shows; the answer is otherwise
/// kept as it came.
pub fn enforce_headings(filled_template: &str, answer: &str) -> String {
    let _ = (filled_template, answer);
    todo!("core-logic agent")
}

/// The block code writes at the top of every meeting note, whatever the
/// template says: when, calendar, organizer, attendees, who spoke, join link.
/// Markdown.
pub fn details_block(facts: &Facts) -> String {
    let _ = facts;
    todo!("core-logic agent")
}

/// Unknown placeholders and other problems, for the settings editor.
pub fn lint(body: &str) -> Vec<String> {
    let _ = body;
    todo!("core-logic agent")
}
