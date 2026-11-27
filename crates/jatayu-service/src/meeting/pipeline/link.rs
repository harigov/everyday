//! Filing a recording begun without a calendar meeting under the meeting it
//! was of. See `jatayu_core::meeting::linking` for the judging; this loads
//! the candidates, asks the model when the times do not settle it, and
//! writes the answer onto the recording -- before speakers are told apart,
//! since who was invited is what names them.

use std::sync::Arc;

use jatayu_core::calendar::Event;
use jatayu_core::meeting::linking::{self, LinkAnswer, Span};
use jatayu_core::meeting::people::{self, Confidence, Owner};
use jatayu_core::meeting::prompt::estimate_tokens;
use jatayu_core::meeting::{EventRef, Recording, SAMPLE_RATE, Stage, Track, TrackSegment};
use jatayu_core::store::calendars::EventQuery;
use jatayu_core::store::meetings::RecordingQuery;
use jatayu_core::{RecordingId, Vault};
use jiff::{Timestamp, tz::TimeZone};

use crate::error::CommandResult;
use crate::meeting::{spool, watch};
use crate::service::blocking;

use super::adapters::Summariser;
use super::stages::owner_of;

/// The opening of the call is kept whole: how people greet each other and
/// what they say the call is for settles more than anything later.
const OPENING_MS: u64 = 4 * 60 * 1000;

/// Tokens of the transcript the model is shown, as for the speakers' own
/// question: a local model has to fit it.
const EXCERPT_BUDGET: u32 = 6_000;

/// What the pipeline needs to judge, read in one go off the async thread.
struct Loaded {
    recording: Recording,
    /// Meetings of the person's own near the recording, each with the name
    /// of the calendar it is on.
    events: Vec<(Event, String)>,
    owner: Owner,
}

/// Look for the calendar meeting recording `id` was of, if it was begun
/// without one and nobody has looked yet, and file it there: the
/// recording's `event` (and its title, when the title was only the
/// program's name), and `continues` when an earlier recording of the same
/// meeting ended within `REJOIN_WINDOW_MS` of this one's start. Marks the
/// recording `link_checked` either way. With no `summariser` only the
/// obvious cases are filed. Never fails the pipeline for want of an answer:
/// a model that errs or hedges leaves the recording as it was.
pub(crate) async fn link_to_calendar(
    vault: &Arc<Vault>,
    summariser: Option<&dyn Summariser>,
    id: RecordingId,
) -> CommandResult<()> {
    let loaded = {
        let vault = vault.clone();
        blocking(move || load(&vault, id)).await?
    };
    let Some(Loaded { recording, events, owner }) = loaded else {
        return Ok(());
    };

    let span = span_of(&recording);
    let plain: Vec<Event> = events.iter().map(|(e, _)| e.clone()).collect();
    let found = linking::candidates(span, &plain);

    // Which of the candidates already have a recording of their own: a call
    // begun right after one is likelier a conversation of its own than the
    // meeting carried on.
    let recorded = {
        let vault = vault.clone();
        let events: Vec<Event> = found.iter().map(|c| c.event.clone()).collect();
        let (started_at, this) = (recording.started_at, id);
        blocking(move || Ok(recorded_before(&vault, &events, started_at, this))).await?
    };

    let chosen = if let Some(i) = linking::obvious(&found) {
        Some((i, true))
    } else if let Some(summariser) = summariser {
        let names = first_names(&found, &owner);
        let excerpt = excerpt_of(&recording.partial, &owner.name, &names, EXCERPT_BUDGET);
        let app = app_of(&recording.title);
        match linking::link_request(span, app, &owner.name, &found, &recorded, &excerpt) {
            Some(request) => match summariser.ask(&request.system, &request.user).await {
                Ok(answer) => settle(linking::parse_link(&answer, found.len()), &recorded),
                Err(err) => {
                    tracing::warn!(recording = %id, error = %err.message, "could not ask which meeting a recording was of");
                    None
                }
            },
            None => None,
        }
    } else {
        None
    };
    // Sure: the times settled it or the model said "high". Only then may a
    // recording be filed under a meeting that has one already, or carry it on.
    let sure = chosen.is_some_and(|(_, sure)| sure);
    let chosen = chosen.map(|(i, _)| i);

    let picked = chosen.and_then(|i| {
        let event = found.get(i)?.event;
        let calendar = events.iter().find(|(e, _)| e.id == event.id).map(|(_, c)| c.clone())?;
        Some(EventRef::from_event(event, &calendar))
    });
    tracing::info!(
        recording = %id,
        candidates = found.len(),
        linked = picked.is_some(),
        "looked for the calendar meeting a recording was of"
    );

    let vault = vault.clone();
    blocking(move || {
        spool::mutate_recording(&vault, id, |r| {
            r.link_checked = true;
            // Somebody else may have filed it while the model was thinking.
            let Some(event) = picked else { return Ok(()) };
            if r.event.is_some() {
                return Ok(());
            }
            if is_generic(&r.title) {
                r.title = event.title.clone();
            }
            if sure && r.continues.is_none() {
                r.continues = spool::continued_by(&vault, Some(&event), r.started_at, r.id)?;
            }
            r.event = Some(event);
            Ok(())
        })?;
        Ok(())
    })
    .await
}

/// The recording and the meetings near it, or `None` when there is nothing
/// to do: it has a meeting, was looked at already, or is not at a stage
/// where its words are in and the note not yet made.
fn load(vault: &Vault, id: RecordingId) -> CommandResult<Option<Loaded>> {
    let recording = vault.recording(id)?;
    let pending =
        matches!(recording.stage, Stage::Transcribing | Stage::Identifying | Stage::Summarising);
    if recording.event.is_some() || recording.link_checked || !pending {
        return Ok(None);
    }
    let span = span_of(&recording);
    let zone = TimeZone::system();
    let day = |t: Timestamp, by: i64| {
        t.to_zoned(zone.clone()).date().checked_add(jiff::Span::new().days(by)).ok()
    };
    let events = vault.events(&EventQuery {
        from: day(span.start, -1),
        to: day(span.end, 1),
        ..Default::default()
    })?;
    let settings = vault.meeting_settings()?;
    let owners = watch::owner_addresses(vault)?;
    let calendars = vault.calendars()?;
    let events = events
        .into_iter()
        .filter_map(|e| {
            // A meeting the owner turned down stays on the calendar; a call
            // that merely overlaps it is not that meeting.
            if linking::declined_by_owner(&e, &owners) {
                return None;
            }
            let name = watch::a_meeting_of_ours(&settings, &owners, &calendars, &e)?.name.clone();
            Some((e, name))
        })
        .collect();
    Ok(Some(Loaded { recording, events, owner: owner_of(vault) }))
}

/// What the model's answer allows, with whether it is sure: the meeting's
/// index, and `true` for "high". A meeting that already has a recording is
/// only taken on a sure answer; any other may take a "medium".
fn settle(answer: LinkAnswer, recorded: &[Option<i64>]) -> Option<(usize, bool)> {
    let LinkAnswer::Meeting { index, confidence } = answer else {
        return None;
    };
    let sure = confidence == Confidence::High;
    let already = recorded.get(index).copied().flatten().is_some();
    (sure || !already).then_some((index, sure))
}

/// For each of `events`, the minutes between the end of the latest other
/// recording of it and `started_at`, or `None` when it has none. Looked up
/// like the offers do (`watch::recordings_of`): by calendar and a window
/// around the event's start, confirmed by `uid`. A lookup that fails counts
/// as none: this only makes the link more careful.
fn recorded_before(
    vault: &Vault,
    events: &[Event],
    started_at: Timestamp,
    this: RecordingId,
) -> Vec<Option<i64>> {
    const MARGIN_SECS: i64 = 6 * 60 * 60;
    events
        .iter()
        .map(|event| {
            let around = |by: i64| {
                Timestamp::from_second(event.start.as_second() + by).unwrap_or(event.start)
            };
            let found = vault
                .recordings(&RecordingQuery {
                    calendar_id: Some(event.calendar_id),
                    from: Some(around(-MARGIN_SECS)),
                    to: Some(around(MARGIN_SECS)),
                    ..Default::default()
                })
                .ok()?;
            found
                .into_iter()
                .filter(|r| r.id != this)
                .filter(|r| r.event.as_ref().is_some_and(|e| e.uid == event.uid))
                .map(|r| {
                    let ended = r.ended_at.unwrap_or(r.started_at);
                    (started_at.as_millisecond() - ended.as_millisecond()) / 60_000
                })
                .min_by_key(|gap| gap.abs())
        })
        .collect()
}

/// When the recording ran. A recording that never got an end (the app died)
/// is taken to have run as far as its audio does.
fn span_of(recording: &Recording) -> Span {
    let start = recording.started_at;
    let end = recording.ended_at.unwrap_or_else(|| {
        let ms = recording
            .chunks
            .iter()
            .map(|c| c.start_ms + c.samples * 1000 / u64::from(SAMPLE_RATE))
            .max()
            .unwrap_or(0);
        start.checked_add(jiff::SignedDuration::from_millis(ms as i64)).unwrap_or(start)
    });
    Span { start, end: end.max(start) }
}

/// The program from a title the shell made ("Call on Slack" → "Slack"), or
/// nothing when the title is something else.
fn app_of(title: &str) -> &str {
    title.strip_prefix("Call on ").map(str::trim).unwrap_or("")
}

/// A title that is only the program's name or the app's fallback, which a
/// meeting's own title is better than.
fn is_generic(title: &str) -> bool {
    let title = title.trim();
    // Only the shell's own forms: a title somebody typed that merely starts
    // with "Call" ("Callback plan") is theirs.
    title.is_empty()
        || title == "Call"
        || title == "Recording"
        || title.strip_prefix("Call on ").is_some_and(|app| !app.trim().is_empty())
}

/// First names of everybody the candidates invited, to find where the call
/// names them.
fn first_names(found: &[linking::LinkCandidate<'_>], owner: &Owner) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for candidate in found {
        let event = EventRef::from_event(candidate.event, "");
        for invitee in people::invitees(&event, owner) {
            if let Some(first) = invitee.name.split_whitespace().next()
                && first.chars().count() >= 3
            {
                let first = first.to_lowercase();
                if !names.contains(&first) {
                    names.push(first);
                }
            }
        }
    }
    names
}

/// What was said, as `[mm:ss] Label: text` lines: the first
/// [`OPENING_MS`] and every line that says an invitee's first name, each
/// with the line before and after it, in order, cut at `budget` tokens. The
/// microphone is the owner (`owner_name`, else "You"), the system track
/// "Others": voices are not told apart yet.
fn excerpt_of(
    partial: &[TrackSegment],
    owner_name: &str,
    first_names: &[String],
    budget: u32,
) -> String {
    let mut lines: Vec<&TrackSegment> =
        partial.iter().filter(|s| !s.text.trim().is_empty()).collect();
    lines.sort_by_key(|s| (s.start_ms, s.end_ms));
    let mine = if owner_name.trim().is_empty() { "You" } else { owner_name.trim() };
    let says_a_name = |text: &str| {
        let text = text.to_lowercase();
        text.split(|c: char| !c.is_alphanumeric()).any(|w| first_names.iter().any(|n| n == w))
    };

    let mut keep = vec![false; lines.len()];
    for (i, line) in lines.iter().enumerate() {
        if line.start_ms < OPENING_MS {
            keep[i] = true;
        }
        if says_a_name(&line.text) {
            keep[i.saturating_sub(1)..=(i + 1).min(lines.len() - 1)].fill(true);
        }
    }

    let mut out = String::new();
    let mut gap = false;
    for (i, line) in lines.iter().enumerate() {
        if !keep[i] {
            gap = !out.is_empty();
            continue;
        }
        let label = match line.track {
            Track::Mic => mine,
            Track::System => "Others",
        };
        let secs = line.start_ms / 1000;
        let mut next = String::new();
        if gap {
            next.push_str("[...]\n");
            gap = false;
        }
        next.push_str(&format!(
            "[{:02}:{:02}] {label}: {}\n",
            secs / 60,
            secs % 60,
            line.text.trim()
        ));
        if estimate_tokens(&out) + estimate_tokens(&next) > budget {
            break;
        }
        out.push_str(&next);
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{CommandError, codes};
    use jatayu_core::TemplateId;

    fn test_vault() -> (tempfile::TempDir, Arc<Vault>) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = jatayu_core::VaultConfig {
            password: Some("correct horse battery".into()),
            kdf: jatayu_core::crypto::KdfParams::insecure_fast(),
            ..Default::default()
        };
        let vault = jatayu_vault::create(dir.path(), cfg).unwrap();
        (dir, Arc::new(vault))
    }

    fn said(track: Track, start_ms: u64, text: &str) -> TrackSegment {
        TrackSegment {
            track,
            start_ms,
            end_ms: start_ms + 2000,
            text: text.into(),
            speaker_hint: None,
            hint_scope: 0,
        }
    }

    fn recording(vault: &Vault, stage: Stage) -> RecordingId {
        let mut r = Recording::new("Call on Slack", None, TemplateId::new());
        r.stage = stage;
        r.ended_at = Some(r.started_at);
        vault.save_recording(&r).unwrap();
        r.id
    }

    struct Panics;
    impl Summariser for Panics {
        fn ask<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
        ) -> crate::speech::transcribe::BoxFuture<'a, CommandResult<String>> {
            Box::pin(async { Err(CommandError::new(codes::AGENT, "unusable")) })
        }
    }

    #[test]
    fn the_program_is_read_from_the_shells_title() {
        assert_eq!(app_of("Call on Google Chrome input"), "Google Chrome input");
        assert_eq!(app_of("Design sync"), "");
        assert!(is_generic("Call on Slack") && is_generic("Recording") && is_generic("Call"));
        assert!(!is_generic("Design sync"));
        // Typed titles that merely begin like the shell's.
        assert!(!is_generic("Callback plan with Bob"));
        assert!(!is_generic("Recording studio sync"));
        assert!(!is_generic("Call on"));
    }

    #[test]
    fn a_recorded_meeting_is_only_taken_on_a_sure_answer() {
        let medium = LinkAnswer::Meeting { index: 0, confidence: Confidence::Medium };
        let high = LinkAnswer::Meeting { index: 0, confidence: Confidence::High };
        assert_eq!(settle(medium, &[Some(5)]), None);
        assert_eq!(settle(high, &[Some(5)]), Some((0, true)));
        // Not recorded yet: a medium answer still links, without being sure.
        assert_eq!(settle(medium, &[None]), Some((0, false)));
        assert_eq!(settle(medium, &[]), Some((0, false)));
        assert_eq!(settle(LinkAnswer::None, &[None]), None);
    }

    #[test]
    fn the_excerpt_keeps_the_opening_and_where_a_name_is_said() {
        let mut partial = vec![said(Track::Mic, 1_000, "Morning all.")];
        for i in 0..40u64 {
            partial.push(said(Track::System, 300_000 + i * 10_000, &format!("filler {i}")));
        }
        partial.push(said(Track::System, 800_000, "Thanks, Priya, that helps."));
        partial.sort_by_key(|s| std::cmp::Reverse(s.start_ms));
        let text = excerpt_of(&partial, "Hari", &["priya".to_string()], 6_000);
        assert!(text.starts_with("[00:01] Hari: Morning all."));
        assert!(text.contains("Others: Thanks, Priya"));
        assert!(text.contains("[...]"));
        assert!(!text.contains("filler 5"));
        // The neighbour before the name is kept with it.
        assert!(text.contains("filler 39"));
    }

    #[test]
    fn the_excerpt_is_cut_at_its_budget_and_labels_the_owner_you_without_a_name() {
        let partial: Vec<_> =
            (0..200u64).map(|i| said(Track::Mic, i * 1000, &"word ".repeat(20))).collect();
        let text = excerpt_of(&partial, "", &[], 100);
        assert!(estimate_tokens(&text) <= 100);
        assert!(text.contains("You: word"));
    }

    #[test]
    fn a_recording_without_an_end_runs_as_far_as_its_audio() {
        let mut r = Recording::new("Call on Slack", None, TemplateId::new());
        r.ended_at = None;
        r.chunks.push(jatayu_core::meeting::ChunkMeta {
            track: Track::Mic,
            seq: 0,
            start_ms: 60_000,
            samples: u64::from(SAMPLE_RATE) * 30,
            transcribed: true,
            head_ms: 0,
        });
        let span = span_of(&r);
        assert_eq!(span.end.as_millisecond() - span.start.as_millisecond(), 90_000);
    }

    #[tokio::test]
    async fn a_recording_with_no_meeting_near_it_is_marked_checked_and_left_alone() {
        let (_dir, vault) = test_vault();
        let id = recording(&vault, Stage::Identifying);
        link_to_calendar(&vault, Some(&Panics), id).await.unwrap();
        let r = vault.recording(id).unwrap();
        assert!(r.link_checked);
        assert!(r.event.is_none());
        assert_eq!(r.title, "Call on Slack");
    }

    #[tokio::test]
    async fn a_recording_past_the_words_stage_is_not_looked_at() {
        let (_dir, vault) = test_vault();
        let id = recording(&vault, Stage::Done);
        link_to_calendar(&vault, None, id).await.unwrap();
        assert!(!vault.recording(id).unwrap().link_checked);
    }
}
