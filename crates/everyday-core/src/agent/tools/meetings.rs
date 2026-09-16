//! Meeting notes, read-only: the history of finished calls, and what was
//! actually said in one of them.
//!
//! Everything mutating -- starting a recording, naming a speaker, rewriting
//! a summary -- lives in `everyday_service::domains::meetings` instead, the
//! same split `journals`'s own module doc draws between what a model can
//! decide offline and what needs the pipeline, a transcriber, and a socket.
//! What is here is the half that is pure lookup: find a note that came from
//! a call, then read what was said in it.
//!
//! # Why the interesting parts are free functions
//!
//! [`day_window_timestamps`], [`attendee_matches`], [`page_bounds`] and
//! [`attribution_json`] hold the whole of this file's logic, and every one
//! of them is pure: dates and strings in, a value out, nothing borrowed from
//! a vault. That is deliberate, not incidental -- `everyday-core`'s own test
//! backend (`crate::testing::MemStore`) carries journals and nothing else,
//! the same way every other domain's real work already avoids needing a
//! live vault to test by keeping the vault call itself to one or two lines
//! at the very end of `run_list_meeting_notes` and `run_get_transcript`.

use jiff::Timestamp;
use jiff::civil::Date;
use serde_json::{Value, json};

use super::{Args, Tool, ToolContext, Window, day, number, schema, text};
use crate::error::Result;
use crate::id::NoteId;
use crate::meeting::{Attribution, Speaker, Stage};
use crate::store::meetings::RecordingQuery;

/// How far back [`list_meeting_notes`] looks when `from` is not given. The
/// same span `everyday_service::scheduler`'s own "last time with these
/// people" search uses for a routine's meeting prep, so a person asking the
/// assistant "when did I last talk to Priya" and a routine answering the
/// same question unasked agree on what "recently" means.
const DEFAULT_HISTORY_DAYS: u32 = 90;

/// Rows [`list_meeting_notes`] returns when nobody says how many.
///
/// Narrower than [`super::DEFAULT_LIMIT`]: a meeting history is read to find
/// *one* call, not browsed the way a task list is, and twenty is already
/// more calls than most weeks hold.
const DEFAULT_NOTES_LIMIT: u32 = 20;

/// A generous, fixed cap on how many finished recordings are read out of the
/// vault before filtering -- independent of the model's own `limit`, which
/// is applied afterwards. Keeps the query itself bounded regardless of how
/// wide a date window or how rare an attendee match is asked for, without
/// making the caller's own page size do double duty as a store-level cap.
const RECORDING_SCAN_CAP: u32 = 500;

/// Segments [`get_transcript`] returns in one page when nobody says how many.
///
/// A segment is a few seconds of speech; four hundred is comfortably more
/// than an hour's call at ordinary turn-taking speed, so most transcripts
/// are read in one call and a long one pages cleanly rather than arriving as
/// one very large reply.
const DEFAULT_SEGMENT_LIMIT: u32 = 400;

/// Most segments [`get_transcript`] will return in one page, whatever it is
/// asked for.
const MAX_SEGMENT_LIMIT: u32 = 2000;

pub(super) static TOOLS: &[Tool] = &[
    tool!(
        "list_meeting_notes",
        Read,
        Meetings,
        schema(
            vec![
                ("from", day("Start of the window, inclusive. Defaults to 90 days ago.")),
                ("to", day("End of the window, inclusive. Defaults to today.")),
                (
                    "attendee",
                    text(
                        "Only calls with somebody matching this: a substring of their name \
                         or email address, case-insensitive."
                    )
                ),
                ("limit", number("Most notes to return. Defaults to 20, capped at 200.")),
            ],
            &[]
        ),
        "Meeting notes already taken, newest first. Filter by when the call was or who \
         was in it. Returns each one's note id and title, when the call was, which \
         calendar it came from, and who was invited -- not the note's body or what was \
         said. Call get_note for the body, or get_transcript for the dialogue.",
        run_list_meeting_notes
    ),
    tool!(
        "get_transcript",
        Read,
        Meetings,
        schema(
            vec![
                ("note_id", text("Id of the meeting note, from list_meeting_notes or search.")),
                (
                    "offset",
                    number("Skip this many segments from the start, to continue a previous page.")
                ),
                (
                    "limit",
                    number("Segments to return in this page. Defaults to 400, capped at 2000.")
                ),
            ],
            &["note_id"]
        ),
        "The transcript kept beside a meeting note: who spoke, and a timestamped line for \
         each turn they took. A long call pages in segments -- if `has_more` comes back \
         true, call again with `offset` set to this call's own `offset` plus `returned` \
         to keep reading. A note with no transcript answers with an empty one rather \
         than an error.",
        run_get_transcript
    ),
];

/// Whether any of `attendees` (raw strings, e.g. `"Priya Raman <priya@x.com>"`)
/// contains `needle` -- already trimmed and lower-cased by the caller -- in
/// its name or its address. A substring of the raw string covers both: an
/// attendee's own text already holds whichever of the two exists.
fn attendee_matches(attendees: &[String], needle: &str) -> bool {
    attendees.iter().any(|a| a.to_lowercase().contains(needle))
}

/// The instants [`RecordingQuery::from`]/[`RecordingQuery::to`] want for the
/// calendar dates `from`..=`to`, in `tz`.
///
/// `to` is inclusive as a date but [`RecordingQuery::to`] is an exclusive
/// instant, so the upper bound is the *start of the day after* -- the same
/// shift a whole day's worth of local wall-clock time takes to become a
/// half-open range of instants. `None` on either side only if `tz` is
/// unrecognised or a date does not exist in it (leap-second-style
/// calendars); [`RecordingQuery`] reads an absent bound as "no limit" on
/// that side, which is the safe direction to fail in.
fn day_window_timestamps(from: Date, to: Date, tz: &str) -> (Option<Timestamp>, Option<Timestamp>) {
    let zone = jiff::tz::TimeZone::get(tz).unwrap_or(jiff::tz::TimeZone::UTC);
    let from_ts = from.at(0, 0, 0, 0).to_zoned(zone.clone()).ok().map(|z| z.timestamp());
    let to_ts = to
        .tomorrow()
        .ok()
        .and_then(|d| d.at(0, 0, 0, 0).to_zoned(zone).ok())
        .map(|z| z.timestamp());
    (from_ts, to_ts)
}

/// The half-open range of segment indices `[start, end)` one page covers, out
/// of `total`: at most `limit` segments starting at `offset`. `offset` past
/// `total` is not an error -- it clamps to an empty page -- since a caller
/// re-reading its own `offset + returned` from a shrinking transcript should
/// never have to special-case running off the end.
fn page_bounds(total: usize, offset: usize, limit: usize) -> (usize, usize) {
    let start = offset.min(total);
    let end = start.saturating_add(limit).min(total);
    (start, end)
}

fn attribution_json(how: &Attribution) -> Value {
    match how {
        Attribution::Owner => json!("owner"),
        // The score travels too: a low-confidence match is worth reading
        // differently from a certain one, and the model cannot tell the
        // two apart from the label alone.
        Attribution::Matched { score } => json!({ "type": "matched", "confidence": score }),
        Attribution::Inferred => json!("inferred"),
        Attribution::Named => json!("named"),
        Attribution::Unknown => json!("unknown"),
    }
}

fn speaker_json(speaker: &Speaker) -> Value {
    json!({ "label": speaker.label, "how": attribution_json(&speaker.how) })
}

fn run_list_meeting_notes(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let (from, to) = args.window(ctx, Window::Back(DEFAULT_HISTORY_DAYS))?;
    let (from_ts, to_ts) = day_window_timestamps(from, to, ctx.tz);

    let query = RecordingQuery {
        stages: vec![Stage::Done.as_str().to_string()],
        from: from_ts,
        to: to_ts,
        limit: Some(RECORDING_SCAN_CAP),
        ..Default::default()
    };
    // Newest first already -- `RecordingQuery::apply` sorts before this ever
    // sees it -- so taking the first `limit` matches below is enough; there
    // is no second sort to do here.
    let recordings = ctx.vault.recordings(&query)?;

    let needle = args.opt_str("attendee").map(|s| s.trim().to_lowercase());
    let limit = args.opt_u32("limit").unwrap_or(DEFAULT_NOTES_LIMIT).clamp(1, super::MAX_LIMIT);

    let mut notes = Vec::new();
    for r in recordings {
        let Some(note_id) = r.note_id else { continue };
        if let Some(needle) = &needle {
            let matches = r.event.as_ref().is_some_and(|e| attendee_matches(&e.attendees, needle));
            if !matches {
                continue;
            }
        }
        // A recording can outlive the note it pointed at being deleted by
        // hand before `Vault::delete_note`'s own cascade catches up to this
        // row -- see `Recording::note_id`'s doc. Skipped rather than
        // failing the whole call over one stale pointer.
        let Ok(note) = ctx.vault.note(note_id) else { continue };
        notes.push(json!({
            "note_id": note_id.to_string(),
            // The call's own title, from the recording -- may differ from
            // the note's current one if it was renamed since.
            "title": r.title,
            "note_title": note.display_title(),
            "start": r.started_at.to_string(),
            "end": r.ended_at.map(|t| t.to_string()),
            "calendar": r.event.as_ref().map(|e| e.calendar_name.clone()),
            "attendees": r.event.as_ref().map(|e| e.attendees.clone()).unwrap_or_default(),
        }));
        if notes.len() as u32 >= limit {
            break;
        }
    }

    Ok(json!({
        "count": notes.len(),
        "from": from.to_string(),
        "to": to.to_string(),
        "notes": notes,
    }))
}

fn run_get_transcript(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let note_id: NoteId = args.id("note_id", "note")?;
    // Read first, both to give a proper "no such note" error and so a note
    // with nothing said in it yet still answers with a title.
    let note = ctx.vault.note(note_id)?;

    let Some(transcript) = ctx.vault.transcript_for_note(note_id)? else {
        // Most notes are not meeting notes, and even a meeting note can be
        // mid-pipeline or have lost its transcript to a delete -- neither is
        // an error a model should have to recover from.
        return Ok(json!({
            "note_id": note_id.to_string(),
            "note_title": note.display_title(),
            "has_transcript": false,
            "speakers": [],
            "lines": [],
            "total_segments": 0,
            "offset": 0,
            "returned": 0,
            "has_more": false,
        }));
    };

    let offset = args.opt_u32("offset").unwrap_or(0) as usize;
    let limit =
        args.opt_u32("limit").unwrap_or(DEFAULT_SEGMENT_LIMIT).clamp(1, MAX_SEGMENT_LIMIT) as usize;
    let total = transcript.segments.len();
    let (start, end) = page_bounds(total, offset, limit);
    let page = &transcript.segments[start..end];

    Ok(json!({
        "note_id": note_id.to_string(),
        "note_title": note.display_title(),
        "has_transcript": true,
        "speakers": transcript.speakers.iter().map(speaker_json).collect::<Vec<_>>(),
        "lines": page.iter().map(|seg| transcript.line(seg)).collect::<Vec<_>>(),
        "total_segments": total,
        "offset": start,
        "returned": page.len(),
        "has_more": end < total,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn attendee_matching_is_case_insensitive_on_name_or_address() {
        let attendees =
            vec!["Priya Raman <priya@x.com>".to_string(), "Sam Okafor <sam@x.com>".to_string()];
        assert!(attendee_matches(&attendees, "priya"));
        assert!(attendee_matches(&attendees, "sam@x.com"));
        assert!(!attendee_matches(&attendees, "nobody"));
    }

    #[test]
    fn the_upper_date_bound_is_the_start_of_the_day_after() {
        let (from, to) = day_window_timestamps(date(2026, 9, 1), date(2026, 9, 3), "UTC");
        let from = from.unwrap();
        let to = to.unwrap();
        assert_eq!(from.to_string(), "2026-09-01T00:00:00Z");
        assert_eq!(to.to_string(), "2026-09-04T00:00:00Z", "inclusive of the whole of Sept 3rd");
        assert!(to > from);
    }

    #[test]
    fn page_bounds_clamp_at_the_end_and_say_whether_more_remain() {
        assert_eq!(page_bounds(10, 0, 4), (0, 4));
        assert_eq!(page_bounds(10, 8, 4), (8, 10));
        assert_eq!(page_bounds(10, 20, 4), (10, 10), "an offset past the end is not an error");
        assert_eq!(page_bounds(0, 0, 4), (0, 0));
    }

    #[test]
    fn attribution_reads_plainly_for_every_kind_of_match() {
        assert_eq!(attribution_json(&Attribution::Owner), json!("owner"));
        assert_eq!(attribution_json(&Attribution::Inferred), json!("inferred"));
        assert_eq!(attribution_json(&Attribution::Named), json!("named"));
        assert_eq!(attribution_json(&Attribution::Unknown), json!("unknown"));
        let json = attribution_json(&Attribution::Matched { score: 0.87 });
        assert_eq!(json["type"], "matched");
        assert!((json["confidence"].as_f64().unwrap() - 0.87).abs() < 0.001, "{json}");
    }
}
