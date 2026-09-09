//! The domain model.
//!
//! These types are the contract between the storage backends, the search
//! index and the UI. They are deliberately backend-agnostic: nothing here
//! knows about SQL tables or Markdown frontmatter.

use crate::id::{BlobId, EntryId, JournalId, TrackerId};
use crate::purpose::Purpose;
use crate::richtext::RichDoc;
use crate::tracker::Tracker;
use jiff::{Timestamp, civil::Date};
use serde::{Deserialize, Serialize};

/// A named collection of entries. Day One calls these "journals".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Journal {
    pub id: JournalId,
    pub name: String,
    /// `#rrggbb`. Drives the accent colour of the journal in the UI.
    pub color: String,
    /// A short emoji or glyph shown next to the name.
    pub icon: String,
    pub description: String,
    /// Manual ordering in the sidebar; ties broken by `name`.
    pub sort_order: i32,
    /// What this journal records alongside its entries: habits, doses,
    /// symptoms, counts. See [`crate::tracker`].
    ///
    /// The definitions live here — inside the sealed journal record — rather
    /// than in a table of their own, because "what am I tracking" is a
    /// setting of the journal, is a handful of rows rather than thousands,
    /// and is the one part of this domain whose *names* must never reach a
    /// clear column. The readings themselves are stored separately, and
    /// separately for the opposite reason: there are thousands of them and
    /// they have to be scannable without being decrypted.
    #[serde(default)]
    pub trackers: Vec<Tracker>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Journal {
    pub fn new(name: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: JournalId::new(),
            name: name.into(),
            color: DEFAULT_JOURNAL_COLORS[0].to_string(),
            icon: "\u{1f4d3}".into(), // notebook
            description: String::new(),
            sort_order: 0,
            trackers: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_color(mut self, color: impl Into<String>) -> Self {
        self.color = color.into();
        self
    }

    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = icon.into();
        self
    }

    /// The trackers the day's chips should offer, in the order they were
    /// arranged. Archived ones are excluded: they keep their history and
    /// leave the page.
    pub fn active_trackers(&self) -> impl Iterator<Item = &Tracker> {
        let mut live: Vec<&Tracker> = self.trackers.iter().filter(|t| !t.archived).collect();
        live.sort_by_key(|t| (t.sort_order, t.created_at));
        live.into_iter()
    }

    pub fn tracker(&self, id: TrackerId) -> Option<&Tracker> {
        self.trackers.iter().find(|t| t.id == id)
    }
}

pub const DEFAULT_JOURNAL_COLORS: &[&str] =
    &["#c2410c", "#0f766e", "#4338ca", "#a21caf", "#b45309", "#15803d", "#0369a1", "#be123c"];

/// Where an entry was written. Optional everywhere.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
}

/// Weather at the time of writing, as captured by an importer or plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Weather {
    pub temperature_c: f64,
    pub condition: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    File,
}

impl MediaKind {
    /// Classify by MIME type, falling back to `File`.
    pub fn from_mime(mime: &str) -> Self {
        match mime.split('/').next().unwrap_or("") {
            "image" => MediaKind::Image,
            "video" => MediaKind::Video,
            "audio" => MediaKind::Audio,
            _ => MediaKind::File,
        }
    }
}

/// Metadata for a piece of media embedded in an entry.
///
/// The bytes themselves live in the backend's blob store, addressed by
/// `blob`. Several entries may reference the same blob.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub blob: BlobId,
    pub kind: MediaKind,
    pub mime: String,
    pub filename: String,
    /// Size of the *plaintext* payload in bytes.
    pub byte_len: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Alt text / caption. Indexed for search.
    #[serde(default)]
    pub caption: String,
}

/// A single journal entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: EntryId,
    pub journal_id: JournalId,
    /// Explicit title. May be empty, in which case the UI derives a heading
    /// from the first line of the body.
    #[serde(default)]
    pub title: String,
    /// The rich text body, as a ProseMirror document.
    pub body: RichDoc,
    /// The calendar date the entry is *filed under*, in the author's local
    /// time. This is what the UI groups and sorts by, and it is deliberately
    /// separate from `created_at` so that back-dating an entry works.
    pub local_date: Date,
    /// IANA time zone the entry was written in, e.g. `Europe/Berlin`.
    #[serde(default = "utc_tz")]
    pub tz: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(default)]
    pub tags: Vec<String>,
    /// What this day's writing was *for*, if it was for anything.
    ///
    /// Most entries will never set it, and that is the expected shape: a
    /// journal is not a work log. It exists so that the fortnight you wrote
    /// every evening about learning to sail is evidence the goal was alive,
    /// which is a thing no task and no block records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<Purpose>,
    #[serde(default)]
    pub starred: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weather: Option<Weather>,
    /// Media referenced by the body, in document order.
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

fn utc_tz() -> String {
    "UTC".to_string()
}

impl Entry {
    /// A blank entry filed under today's date in the given time zone.
    pub fn new(journal_id: JournalId, tz: &str) -> Self {
        let now = Timestamp::now();
        let local_date = local_date_in(now, tz);
        Self {
            id: EntryId::new(),
            journal_id,
            title: String::new(),
            body: RichDoc::empty(),
            local_date,
            tz: tz.to_string(),
            created_at: now,
            updated_at: now,
            tags: Vec::new(),
            starred: false,
            pinned: false,
            location: None,
            weather: None,
            attachments: Vec::new(),
            purpose: None,
        }
    }

    /// The heading the UI should show: the explicit title if set, else the
    /// first non-empty line of the body, else a placeholder.
    pub fn display_title(&self) -> String {
        if !self.title.trim().is_empty() {
            return self.title.trim().to_string();
        }
        let text = self.body.plain_text();
        match text.lines().map(str::trim).find(|l| !l.is_empty()) {
            Some(line) => truncate_on_char_boundary(line, 120),
            None => "Untitled entry".to_string(),
        }
    }

    /// Everything a full-text index should see for this entry.
    pub fn searchable_text(&self) -> String {
        let mut out = String::with_capacity(256);
        out.push_str(&self.title);
        out.push('\n');
        out.push_str(&self.body.plain_text());
        for tag in &self.tags {
            out.push('\n');
            out.push_str(tag);
        }
        for att in &self.attachments {
            if !att.caption.is_empty() {
                out.push('\n');
                out.push_str(&att.caption);
            }
        }
        if let Some(loc) = &self.location {
            for part in [&loc.place_name, &loc.locality, &loc.country].into_iter().flatten() {
                out.push('\n');
                out.push_str(part);
            }
        }
        out
    }

    pub fn word_count(&self) -> u32 {
        self.body.plain_text().split_whitespace().count() as u32
    }

    /// Condensed form used by list views, so the UI never has to load a
    /// whole document (and all its media) just to draw a row.
    pub fn summarize(&self) -> EntrySummary {
        let plain = self.body.plain_text();
        let excerpt = plain
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .skip(usize::from(self.title.trim().is_empty()))
            .collect::<Vec<_>>()
            .join(" ");
        EntrySummary {
            id: self.id,
            journal_id: self.journal_id,
            title: self.display_title(),
            excerpt: truncate_on_char_boundary(excerpt.trim(), 240),
            local_date: self.local_date,
            created_at: self.created_at,
            updated_at: self.updated_at,
            tags: self.tags.clone(),
            starred: self.starred,
            pinned: self.pinned,
            word_count: plain.split_whitespace().count() as u32,
            attachment_count: self.attachments.len() as u32,
            cover: self.attachments.iter().find(|a| a.kind == MediaKind::Image).map(|a| a.blob),
            place: self
                .location
                .as_ref()
                .and_then(|l| l.place_name.clone().or_else(|| l.locality.clone())),
            purpose: self.purpose,
        }
    }
}

/// Truncate to at most `max` chars, appending an ellipsis, without ever
/// splitting a UTF-8 code point.
fn truncate_on_char_boundary(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// A row in the entry list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntrySummary {
    pub id: EntryId,
    pub journal_id: JournalId,
    pub title: String,
    pub excerpt: String,
    pub local_date: Date,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub tags: Vec<String>,
    pub starred: bool,
    pub pinned: bool,
    pub word_count: u32,
    pub attachment_count: u32,
    /// First image in the entry, used as a thumbnail in the list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover: Option<BlobId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// Carried so the list can say what an entry is filed under, and offer
    /// to change it, without opening the entry to find out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<Purpose>,
}

/// Resolve the local calendar date of an instant in a named time zone,
/// falling back to UTC if the zone is unknown to the platform tz database.
pub fn local_date_in(ts: Timestamp, tz: &str) -> Date {
    match jiff::tz::TimeZone::get(tz) {
        Ok(zone) => ts.to_zoned(zone).date(),
        Err(_) => ts.to_zoned(jiff::tz::TimeZone::UTC).date(),
    }
}

/// The machine's own time zone, as an IANA name, falling back to UTC.
///
/// Records store this so their local date survives the author moving
/// countries: an entry written in Berlin is still filed under the day it was
/// Berlin, read back in Chennai.
///
/// It lives here, in the core, rather than in each front end. Both the shell
/// and the CLI need it, and between them they had written this one line out
/// six times -- three of those inline in a single file. The zone a record is
/// filed under, and the day "overdue" is measured from, have to be decided by
/// the same code or they disagree at midnight.
pub fn system_tz() -> String {
    jiff::tz::TimeZone::system().iana_name().unwrap_or("UTC").to_string()
}

/// Today, on the machine's own calendar. Kept beside [`system_tz`] so the two
/// can never disagree about which day it is.
pub fn today_local() -> Date {
    local_date_in(Timestamp::now(), &system_tz())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::richtext::RichDoc;

    fn entry_with_body(md: &str) -> Entry {
        let mut e = Entry::new(JournalId::new(), "UTC");
        e.body = RichDoc::from_plain_text(md);
        e
    }

    #[test]
    fn display_title_falls_back_to_first_line() {
        let e = entry_with_body("A quiet morning\nThen the rain started.");
        assert_eq!(e.display_title(), "A quiet morning");
    }

    #[test]
    fn display_title_prefers_explicit_title() {
        let mut e = entry_with_body("A quiet morning");
        e.title = "  Rainy Tuesday  ".into();
        assert_eq!(e.display_title(), "Rainy Tuesday");
    }

    #[test]
    fn display_title_handles_empty_body() {
        let e = entry_with_body("");
        assert_eq!(e.display_title(), "Untitled entry");
    }

    #[test]
    fn excerpt_skips_the_line_used_as_the_title() {
        let e = entry_with_body("A quiet morning\nThen the rain started.");
        assert_eq!(e.summarize().excerpt, "Then the rain started.");
    }

    #[test]
    fn truncation_never_splits_a_multibyte_char() {
        // 200 astral-plane chars: byte-slicing would panic here.
        let s = "\u{1f600}".repeat(200);
        let out = truncate_on_char_boundary(&s, 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn media_kind_classifies_by_mime_prefix() {
        assert_eq!(MediaKind::from_mime("image/avif"), MediaKind::Image);
        assert_eq!(MediaKind::from_mime("video/mp4"), MediaKind::Video);
        assert_eq!(MediaKind::from_mime("application/pdf"), MediaKind::File);
    }

    #[test]
    fn searchable_text_includes_tags_and_captions() {
        let mut e = entry_with_body("hello");
        e.tags = vec!["travel".into()];
        e.attachments.push(Attachment {
            blob: BlobId::of(b"x"),
            kind: MediaKind::Image,
            mime: "image/png".into(),
            filename: "x.png".into(),
            byte_len: 1,
            width: None,
            height: None,
            duration_ms: None,
            caption: "sunset over the harbour".into(),
        });
        let text = e.searchable_text();
        assert!(text.contains("travel"));
        assert!(text.contains("harbour"));
    }
}
