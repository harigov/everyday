//! What was read, heard and watched: progress reports folded into one
//! record per thing. See `docs/plans/podcasts.md` and
//! `karmayoga_core::news::listening`.
//!
//! # A report is cheap
//!
//! The player reports every fifteen seconds, on pause, on seek and at the
//! end, so the common path is a few reads and one write: the stored record,
//! the story it is about (whose files change as an episode is downloaded and
//! cleared away), and the record again with what was just heard merged into
//! it. Nothing else is touched until a thing becomes finished.
//!
//! # What finishing does
//!
//! It takes the thing out of Up Next, marks a story read, and tops Up Next up
//! from Recommended. Those are writes the `record_progress` command does not
//! declare -- it declares the consumption -- so each is announced here, and
//! the vault writes are made inside a scope of their own: otherwise the
//! command's debug-build check of what it touched against what it declared
//! would take the story's read mark for a mistake.
//!
//! # What the Feed remembers
//!
//! Two more things follow from a report, and neither may slow it or fail it.
//! An article that is finished is kept as Markdown in the files folder
//! ([`super::keep`]) on a task of its own, which announces what it writes
//! itself. And the *first* time an episode is played, it is put on the list to
//! be transcribed when its show's policy is to do that once somebody plays it
//! ([`super::transcribe`]): the first report is the one that makes the record,
//! so "first" costs no read the report did not already make.
//!
//! Only episodes, and never a file. A recording in Files is the person's own --
//! music, a family film, a memo -- and transcribing one means sending it to
//! whatever recogniser is set up, which may be somebody else's cloud service;
//! that is for the Transcribe button, not for pressing play. The app's own
//! audio (the daily podcast, an article read aloud) was written from words it
//! already has. So the policy that applies is a show's, and a file is
//! transcribed only when somebody asks.

use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Kind, Op};
use crate::news::transcribe::{self, Job};
use crate::news::{keep, newsletters, queue, rank};
use crate::service::{Service, blocking};
use jiff::Timestamp;
use karmayoga_core::id::{FileId, QueueEntryId};
use karmayoga_core::news::listening::{FINISHED_AT, covered, merge_spans};
use karmayoga_core::news::{
    ConsumeMode, Consumption, ContentRef, Enclosure, PodcastPolicy, Span, Story, TranscribePolicy,
};
use karmayoga_core::vault::upnext_entry::ContentFacts;
use karmayoga_core::{Error, Vault};
use serde::Deserialize;
use std::sync::Arc;

/// Two stretches heard this close together are one: a pause, a seek back a
/// few seconds, the gap between two reports.
const SLACK_MS: u64 = 2_000;

/// What a player, the reader or Listen reports. Everything but `what` and
/// `mode` is optional: a reader knows how far down it is, a player where it
/// is and what it just played.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressReport {
    pub what: ContentRef,
    pub mode: ConsumeMode,
    #[serde(default)]
    pub position_ms: Option<u64>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// The stretch played since the last report.
    #[serde(default)]
    pub heard: Option<Span>,
    /// How far through, 0 to 1, for what has no clock (an article read).
    #[serde(default)]
    pub progress: Option<f32>,
    /// Said to be finished (or not), from a button or the end of playback.
    #[serde(default)]
    pub finished: Option<bool>,
}

/// What a thing says about itself, as far as its record keeps it: copied in
/// so the record outlives the thing. The part of [`ContentFacts`] a
/// [`Consumption`] holds.
#[derive(Debug, Clone, Default, PartialEq)]
struct Described {
    title: String,
    source: String,
    url: String,
    media_file: Option<FileId>,
    text_file: Option<FileId>,
}

impl From<&ContentFacts> for Described {
    fn from(facts: &ContentFacts) -> Self {
        Self {
            title: facts.title.clone(),
            source: facts.source.clone(),
            url: facts.url.clone(),
            media_file: facts.media_file,
            text_file: facts.text_file,
        }
    }
}

/// A record after a report, and whether the report finished it.
#[derive(Debug, Clone, PartialEq)]
struct Folded {
    consumption: Consumption,
    /// Became finished with this report, or was told again that it ended:
    /// what takes it out of Up Next.
    finishing: bool,
}

/// `old` with `report` folded in; the first report makes a record from what
/// `target` says. `None` when there is neither -- nothing to keep a record of.
///
/// Pure, so the arithmetic is tested without a vault.
///
/// * What was heard is merged with what was; progress only ever grows, from
///   the report's own figure or from how much of the recording has been
///   heard.
/// * Finished is said, or reached ([`FINISHED_AT`]). Said *not* finished,
///   with nothing else to report, starts the thing over: what was heard would
///   otherwise finish it again at the next report.
/// * Said finished, progress is whole.
fn fold(
    old: Option<Consumption>,
    target: Option<Described>,
    report: &ProgressReport,
    now: Timestamp,
) -> Option<Folded> {
    let mut c = match (old, &target) {
        (Some(c), _) => c,
        (None, Some(t)) => {
            let mut c = Consumption::new(report.what, t.title.clone(), report.mode);
            c.first_at = now;
            c
        }
        (None, None) => return None,
    };
    if let Some(t) = &target {
        // Filled in when they were not known, and kept current when they were:
        // an episode's file comes and goes, a transcript arrives later.
        if c.title.trim().is_empty() {
            c.title = t.title.clone();
        }
        if c.source.is_empty() {
            c.source = t.source.clone();
        }
        if c.url.is_empty() {
            c.url = t.url.clone();
        }
        c.media_file = t.media_file;
        c.text_file = t.text_file;
    }

    let was_finished = c.finished;
    c.mode = report.mode;
    c.last_at = now;

    let start_over = report.finished == Some(false)
        && report.position_ms.is_none()
        && report.heard.is_none()
        && report.progress.is_none();
    if start_over {
        c.heard.clear();
        c.progress = 0.0;
        c.position_ms = None;
    }
    if let Some(duration) = report.duration_ms.filter(|d| *d > 0) {
        c.duration_ms = Some(duration);
    }
    if let Some(position) = report.position_ms {
        c.position_ms = Some(position);
    }
    if let Some(span) = report.heard {
        c.heard = merge_spans(&c.heard, span, SLACK_MS);
    }

    let said = report.progress.filter(|p| p.is_finite()).map(|p| p.clamp(0.0, 1.0));
    let heard = c.duration_ms.map(|d| covered(&c.heard, d));
    c.progress = c.progress.max(said.or(heard).unwrap_or(0.0)).clamp(0.0, 1.0);

    c.finished = match report.finished {
        Some(said) => said,
        None => was_finished || c.progress >= FINISHED_AT,
    };
    if report.finished == Some(true) {
        c.progress = 1.0;
    }
    match (c.finished, c.finished_at) {
        (true, None) => c.finished_at = Some(now),
        (false, Some(_)) => c.finished_at = None,
        _ => {}
    }

    let finishing = c.finished && (!was_finished || report.finished == Some(true));
    Some(Folded { consumption: c, finishing })
}

/// What the thing `what` says about itself, and the story if it is one --
/// the vault's own reading of it, the one Up Next's entries are made from.
/// `None` when it is gone: an expired story, a file in the trash, a deleted
/// item. A file that is not something to hear, watch or read is refused in
/// words.
fn describe(vault: &Vault, what: ContentRef) -> CommandResult<Option<(Described, Option<Story>)>> {
    match vault.describe_content(what) {
        Ok(facts) => Ok(Some((Described::from(&facts), facts.story))),
        Err(Error::NotFound { .. }) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// What [`store`] hands back: the record as saved, and what finishing needs.
struct Saved {
    consumption: Consumption,
    finishing: bool,
    /// The story, if the thing is one: whether it is read, and whether it is
    /// a newsletter.
    story: Option<Story>,
    /// What this report asks to have transcribed: set only by the first
    /// report of an episode that is played. See [`to_transcribe`].
    transcribe: Option<Job>,
}

/// What the first report of an episode that is played puts on the list to be
/// transcribed, if anything: the episode, when its show's policy says to
/// transcribe once it is played and it has no transcript yet.
///
/// A show's own policy is its [`PodcastPolicy`]; a feed that turned out to
/// carry episodes without having been followed as a show has none, and the
/// Feed's defaults stand in. A story that is not an episode is read, not
/// played, and never asks. Nothing but a story does: see the module docs for
/// why a file in Files is left to the Transcribe button.
fn to_transcribe(vault: &Vault, story: &Story) -> CommandResult<Option<Job>> {
    let episode = story.media.as_ref().is_some_and(Enclosure::is_playable);
    if !episode || story.files.transcript.is_some() {
        return Ok(None);
    }
    let defaults = vault.feed_settings()?.podcasts;
    let policy = vault
        .news_source(story.source_id)
        .ok()
        .and_then(|source| source.podcast)
        .map_or(defaults.transcribe, |show| PodcastPolicy::of(&show, &defaults).transcribe);
    Ok((policy == TranscribePolicy::WhenPlayed).then_some(Job::Story(story.id)))
}

/// The reads and the one write of a report. Runs on a blocking thread.
fn store(vault: &Vault, report: &ProgressReport, now: Timestamp) -> CommandResult<Saved> {
    let old = vault.consumption(&report.what)?;
    let first = old.is_none();
    // A story is read every time, for its files; anything else only to make
    // the record the first time.
    let (described, story) = match (&old, report.what) {
        (Some(_), ContentRef::File(_) | ContentRef::Item(_)) => (None, None),
        _ => match describe(vault, report.what)? {
            Some((described, story)) => (Some(described), story),
            None => (None, None),
        },
    };
    // Asked of the first report only, so a report that is one of hundreds
    // reads nothing more than it did before.
    let transcribe = match &story {
        Some(story) if first && matches!(report.mode, ConsumeMode::Listen | ConsumeMode::Watch) => {
            to_transcribe(vault, story)?
        }
        _ => None,
    };
    let Some(folded) = fold(old, described, report, now) else {
        return Err(CommandError::new(
            codes::NOT_FOUND,
            "that is not in the Feed, Files or Library any more, so there is nothing to keep a record of",
        ));
    };
    vault.save_consumption(&folded.consumption)?;
    Ok(Saved { consumption: folded.consumption, finishing: folded.finishing, story, transcribe })
}

/// Fold `progress` into what is stored about the thing, creating it on the
/// first report; when it becomes finished, take it out of Up Next.
pub async fn record(svc: &Arc<Service>, progress: ProgressReport) -> CommandResult<Consumption> {
    let vault = svc.require()?;
    let now = svc.now();
    let saved = {
        let vault = vault.clone();
        blocking(move || store(&vault, &progress, now)).await?
    };
    // Recommended shows how far through each story is, and leaves out what is
    // finished: it must read the Feed afresh, not what it read a moment ago.
    rank::forget_window(svc);
    if let Some(job) = saved.transcribe {
        transcribe::request(svc, job);
    }
    if saved.finishing {
        remember(svc, &saved);
        settle(svc, &vault, &saved).await;
    }
    Ok(saved.consumption)
}

/// A finished article is kept as Markdown: the Feed's memory of it. On a task
/// of its own, so the report does not wait for a file to be written, and so
/// that what it writes -- which the command did not declare -- is announced by
/// [`keep::keep_article`] and not taken for the command's own. A failure is
/// said in the log and never reaches the report, which was kept.
fn remember(svc: &Arc<Service>, saved: &Saved) {
    let Some(story) = saved.story.as_ref().filter(|s| keep::is_article(s)) else { return };
    let (svc, id) = (svc.clone(), story.id);
    tokio::spawn(async move {
        if let Err(e) = keep::keep_article(&svc, id).await {
            tracing::info!(error = %e, "could not keep an article that was finished");
        }
    });
}

/// A thing was finished: out of Up Next, a story read, Up Next topped up.
/// None of it fails the report, which was kept: each step says what it could
/// not do in the log and the next goes on.
async fn settle(svc: &Arc<Service>, vault: &Arc<Vault>, saved: &Saved) {
    let what = saved.consumption.what;
    let to_mark = saved.story.as_ref().filter(|s| !s.read).map(|s| s.id);

    // A newsletter is read through its message, which only mail can mark; it
    // announces the thread it marked itself.
    let newsletter = saved.story.as_ref().filter(|s| !s.read && s.mail_message.is_some());

    let ((done, mail), touched) = crate::touched::scope(async {
        let done = {
            let vault = vault.clone();
            blocking(move || {
                let queued: Vec<QueueEntryId> =
                    vault.queue()?.into_iter().filter(|e| e.what == what).map(|e| e.id).collect();
                if !queued.is_empty() {
                    vault.remove_from_queue_for(&[what])?;
                }
                if let Some(story) = to_mark {
                    vault.set_stories_read(&[story], true)?;
                }
                Ok(queued)
            })
            .await
        };
        let mail = match newsletter {
            Some(story) => {
                newsletters::mark_read(svc, vault, std::slice::from_ref(story), true).await
            }
            None => Ok(()),
        };
        (done, mail)
    })
    .await;
    match done {
        Ok(left) => {
            if !left.is_empty() {
                queue::changed(svc, Op::Deleted, &left);
            }
            crate::events::emit_touched(&*svc.events(), None, &touched, Kind::Story, Op::Updated);
        }
        Err(e) => tracing::info!(error = %e, "could not take a finished thing out of Up Next"),
    }
    if let Err(e) = mail {
        tracing::info!(error = %e, "could not mark a finished newsletter read in mail");
    }

    // Before the top-up asks Recommended what to add: the finished thing is
    // read now and out of Up Next, and the answer must know.
    rank::forget_window(svc);
    if let Err(e) = queue::autofill(svc).await {
        tracing::info!(error = %e, "could not top Up Next up after something was finished");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use karmayoga_core::id::StoryId;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(1_790_000_000 + second).unwrap()
    }

    fn episode() -> ContentRef {
        ContentRef::Story(StoryId::new())
    }

    fn report(what: ContentRef) -> ProgressReport {
        ProgressReport {
            what,
            mode: ConsumeMode::Listen,
            position_ms: None,
            duration_ms: None,
            heard: None,
            progress: None,
            finished: None,
        }
    }

    fn described() -> Described {
        Described {
            title: "Episode 12".into(),
            source: "The Show".into(),
            url: "https://example.com/12".into(),
            media_file: Some(FileId::new()),
            text_file: None,
        }
    }

    fn span(start_ms: u64, end_ms: u64) -> Span {
        Span { start_ms, end_ms }
    }

    #[test]
    fn the_first_report_makes_the_record_from_what_the_thing_says() {
        let what = episode();
        let target = described();
        let r = ProgressReport {
            position_ms: Some(15_000),
            duration_ms: Some(600_000),
            heard: Some(span(0, 15_000)),
            ..report(what)
        };
        let folded = fold(None, Some(target.clone()), &r, at(10)).unwrap();
        let c = folded.consumption;
        assert_eq!((c.title.as_str(), c.source.as_str()), ("Episode 12", "The Show"));
        assert_eq!(c.url, "https://example.com/12");
        assert_eq!(c.media_file, target.media_file);
        assert_eq!(c.mode, ConsumeMode::Listen);
        assert_eq!((c.first_at, c.last_at), (at(10), at(10)));
        assert_eq!(c.position_ms, Some(15_000));
        assert_eq!(c.heard, vec![span(0, 15_000)]);
        assert!((c.progress - 0.025).abs() < 1e-6);
        assert!(!c.finished && !folded.finishing);
        assert!(fold(None, None, &r, at(10)).is_none(), "nothing to keep a record of");
    }

    #[test]
    fn what_was_heard_is_merged_and_progress_only_grows() {
        let what = episode();
        let first = ProgressReport {
            duration_ms: Some(100_000),
            heard: Some(span(0, 10_000)),
            position_ms: Some(10_000),
            ..report(what)
        };
        let c = fold(None, Some(described()), &first, at(0)).unwrap().consumption;

        // The next stretch starts a moment after the last ended: one stretch.
        let second = ProgressReport {
            heard: Some(span(11_000, 30_000)),
            position_ms: Some(30_000),
            ..report(what)
        };
        let c = fold(Some(c), None, &second, at(15)).unwrap().consumption;
        assert_eq!(c.heard, vec![span(0, 30_000)]);
        assert!((c.progress - 0.3).abs() < 1e-6);
        assert_eq!(c.first_at, at(0));
        assert_eq!(c.last_at, at(15));

        // Seeking back and playing a stretch already heard adds nothing.
        let again = ProgressReport {
            heard: Some(span(5_000, 8_000)),
            position_ms: Some(8_000),
            ..report(what)
        };
        let c = fold(Some(c), None, &again, at(30)).unwrap().consumption;
        assert_eq!(c.heard, vec![span(0, 30_000)]);
        assert!((c.progress - 0.3).abs() < 1e-6, "progress does not fall when somebody goes back");
        assert_eq!(c.position_ms, Some(8_000), "but the place to resume from does");

        // A stretch far on is its own.
        let far = ProgressReport { heard: Some(span(60_000, 70_000)), ..report(what) };
        let c = fold(Some(c), None, &far, at(45)).unwrap().consumption;
        assert_eq!(c.heard, vec![span(0, 30_000), span(60_000, 70_000)]);
        assert!((c.progress - 0.4).abs() < 1e-6);
    }

    #[test]
    fn an_article_has_no_clock_and_reports_how_far_down_it_is() {
        let what = ContentRef::Story(StoryId::new());
        let read =
            |p| ProgressReport { mode: ConsumeMode::Read, progress: Some(p), ..report(what) };
        let c = fold(None, Some(described()), &read(0.4), at(0)).unwrap().consumption;
        assert!((c.progress - 0.4).abs() < 1e-6);
        let c = fold(Some(c), None, &read(0.2), at(5)).unwrap().consumption;
        assert!((c.progress - 0.4).abs() < 1e-6, "scrolling back up is not un-reading");
        assert_eq!(c.mode, ConsumeMode::Read);
        // Nonsense is held to the range, and a number that is not one is ignored.
        let c = fold(Some(c), None, &read(7.0), at(6)).unwrap().consumption;
        assert_eq!(c.progress, 1.0);
        let c = fold(None, Some(described()), &read(f32::NAN), at(7)).unwrap().consumption;
        assert_eq!(c.progress, 0.0);
    }

    #[test]
    fn a_thing_is_finished_when_it_is_said_to_be_or_nearly_all_of_it_is_taken_in() {
        let what = episode();
        let nearly = ProgressReport {
            duration_ms: Some(100_000),
            heard: Some(span(0, 91_000)),
            position_ms: Some(91_000),
            ..report(what)
        };
        let folded = fold(None, Some(described()), &nearly, at(100)).unwrap();
        assert!(folded.consumption.finished && folded.finishing);
        assert_eq!(folded.consumption.finished_at, Some(at(100)));

        // Reported again while it plays out: still finished, not finishing,
        // and the moment it finished is kept.
        let more = ProgressReport { heard: Some(span(91_000, 95_000)), ..report(what) };
        let again = fold(Some(folded.consumption), None, &more, at(130)).unwrap();
        assert!(again.consumption.finished && !again.finishing);
        assert_eq!(again.consumption.finished_at, Some(at(100)));

        // The end of playback says so outright, which takes it out of Up Next
        // even when it was already finished (it was queued to hear again).
        let end = ProgressReport { finished: Some(true), ..report(what) };
        let ended = fold(Some(again.consumption), None, &end, at(160)).unwrap();
        assert!(ended.finishing);
        assert_eq!(ended.consumption.progress, 1.0);

        // A button says it with nothing else to go on.
        let button = ProgressReport { finished: Some(true), ..report(episode()) };
        let c = fold(None, Some(described()), &button, at(0)).unwrap();
        assert!(c.consumption.finished && c.finishing);
        assert_eq!(c.consumption.progress, 1.0);
    }

    #[test]
    fn saying_it_is_not_finished_starts_it_over_and_it_stays_unfinished() {
        let what = episode();
        let all = ProgressReport {
            duration_ms: Some(100_000),
            heard: Some(span(0, 100_000)),
            position_ms: Some(100_000),
            ..report(what)
        };
        let done = fold(None, Some(described()), &all, at(0)).unwrap().consumption;
        assert!(done.finished);

        let unplayed = ProgressReport { finished: Some(false), ..report(what) };
        let c = fold(Some(done), None, &unplayed, at(10)).unwrap();
        assert!(!c.consumption.finished && !c.finishing);
        assert_eq!(c.consumption.finished_at, None);
        assert_eq!(c.consumption.progress, 0.0);
        assert!(c.consumption.heard.is_empty());
        assert_eq!(c.consumption.position_ms, None);

        // The next ordinary report does not finish it again.
        let play = ProgressReport {
            heard: Some(span(0, 5_000)),
            position_ms: Some(5_000),
            ..report(what)
        };
        let c = fold(Some(c.consumption), None, &play, at(20)).unwrap();
        assert!(!c.consumption.finished);
        assert!((c.consumption.progress - 0.05).abs() < 1e-6);

        // Unfinished alongside real playback keeps what was heard, but is
        // not finished by it in the same breath.
        let seek = ProgressReport {
            finished: Some(false),
            heard: Some(span(5_000, 99_000)),
            ..report(what)
        };
        let c = fold(Some(c.consumption), None, &seek, at(30)).unwrap();
        assert!(!c.consumption.finished);
    }

    #[test]
    fn a_record_keeps_its_own_words_and_follows_the_files_of_its_story() {
        let what = episode();
        let c = fold(None, Some(described()), &report(what), at(0)).unwrap().consumption;
        let transcript = FileId::new();
        let moved_on = Described {
            title: "A different title".into(),
            source: "Elsewhere".into(),
            url: "https://example.com/other".into(),
            media_file: None,
            text_file: Some(transcript),
        };
        let c = fold(Some(c), Some(moved_on), &report(what), at(60)).unwrap().consumption;
        assert_eq!(c.title, "Episode 12", "what somebody saw it called stays");
        assert_eq!(c.source, "The Show");
        assert_eq!(c.media_file, None, "the episode's file was cleared away");
        assert_eq!(c.text_file, Some(transcript), "and a transcript turned up");
    }

    // ---- against a real vault --------------------------------------------------

    use crate::ctx::Ctx;
    use crate::events::{Change, EventSink};
    use karmayoga_core::QueuePlace;
    use karmayoga_core::files::{FileOrigin, ImportOptions};
    use karmayoga_core::news::{
        AddedBy, Enclosure, FeedSettings, NewsSource, PodcastSettings, QueueEntry, SourceKind,
    };
    use std::sync::Mutex;

    #[derive(Default)]
    struct Collector {
        changes: Mutex<Vec<Change>>,
    }

    impl EventSink for Collector {
        fn changed(&self, change: Change) {
            self.changes.lock().unwrap().push(change);
        }
    }

    impl Collector {
        fn take(&self) -> Vec<(Kind, Op)> {
            std::mem::take(&mut *self.changes.lock().unwrap())
                .into_iter()
                .map(|c| (c.kind, c.op))
                .collect()
        }
    }

    fn world() -> (Arc<Service>, Arc<Vault>, Arc<Collector>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let vault = karmayoga_vault::create(
            dir.path(),
            karmayoga_core::VaultConfig {
                password: Some("correct horse battery staple".into()),
                kdf: karmayoga_core::crypto::KdfParams::insecure_fast(),
                ..Default::default()
            },
        )
        .unwrap();
        let svc = Arc::new(Service::new());
        let vault = svc.set(vault);
        let events = Arc::new(Collector::default());
        svc.set_events(events.clone());
        (svc, vault, events, dir)
    }

    fn stored_story(vault: &Vault, source: &NewsSource, title: &str, media: bool) -> Story {
        let key = vault.news_key(&format!("{}/{title}", source.id)).unwrap();
        let mut story = Story::new(source.id, key, title, Timestamp::now());
        story.url = format!("https://example.com/{title}");
        if media {
            story.media = Some(Enclosure {
                url: "https://x/a.mp3".into(),
                mime: "audio/mpeg".into(),
                length: None,
                duration_ms: Some(100_000),
                transcript_url: String::new(),
                transcript_mime: String::new(),
                episode: None,
                season: None,
            });
        }
        vault.insert_stories(&[(story.clone(), None)]).unwrap();
        story
    }

    #[tokio::test]
    async fn reports_make_one_record_that_follows_the_thing_and_outlives_it() {
        let (svc, vault, _events, _dir) = world();
        let show = NewsSource::new(SourceKind::Web { url: "https://x/feed".into() }, "The Show");
        vault.save_news_source(&show).unwrap();
        let ep = stored_story(&vault, &show, "Ep", true);
        let what = ContentRef::Story(ep.id);
        let play = |from: u64, to: u64| ProgressReport {
            position_ms: Some(to),
            duration_ms: Some(100_000),
            heard: Some(span(from, to)),
            ..report(what)
        };

        let c = record(&svc, play(0, 15_000)).await.unwrap();
        assert_eq!((c.title.as_str(), c.source.as_str()), ("Ep", "The Show"));
        assert_eq!(c.url, "https://example.com/Ep");
        let c2 = record(&svc, play(15_000, 30_000)).await.unwrap();
        assert_eq!(c2.id, c.id, "one record per thing");
        assert_eq!(c2.heard, vec![span(0, 30_000)]);
        assert_eq!(vault.consumptions_for(&[what]).unwrap().len(), 1);

        let stored = vault.consumption(&what).unwrap().unwrap();
        assert_eq!(stored, c2);

        // Gone from the Feed, the record is still there to report on.
        vault.delete_news_source(show.id).unwrap();
        let c3 = record(&svc, play(30_000, 45_000)).await.unwrap();
        assert_eq!(c3.heard, vec![span(0, 45_000)]);
        assert_eq!(c3.title, "Ep");

        // But a thing nobody ever reported on, and that is gone, has nothing
        // to make a record from.
        let nothing = ProgressReport { what: episode(), ..report(what) };
        let err = record(&svc, nothing).await.unwrap_err();
        assert_eq!(err.code, codes::NOT_FOUND);
    }

    #[tokio::test]
    async fn finishing_takes_a_thing_out_of_up_next_marks_it_read_and_tops_up() {
        let (svc, vault, events, _dir) = world();
        let blog = NewsSource::new(SourceKind::Web { url: "https://x/feed".into() }, "Blog");
        vault.save_news_source(&blog).unwrap();
        let read_me = stored_story(&vault, &blog, "Read me", false);
        let other = stored_story(&vault, &blog, "Other", false);
        let spare = stored_story(&vault, &blog, "Spare", false);
        let settings = FeedSettings { autofill: Some(2), ..Default::default() };
        vault.save_feed_settings(&settings).unwrap();
        for story in [&read_me, &other] {
            let mut entry = QueueEntry::new(ContentRef::Story(story.id), story.title.clone(), 0);
            entry.mode = ConsumeMode::Read;
            vault.add_to_queue(entry, QueuePlace::End).unwrap();
        }
        events.take();

        // Half way down: nothing leaves.
        let what = ContentRef::Story(read_me.id);
        let half = ProgressReport { mode: ConsumeMode::Read, progress: Some(0.5), ..report(what) };
        record(&svc, half).await.unwrap();
        assert_eq!(vault.queue().unwrap().len(), 2);
        assert!(!vault.story(read_me.id).unwrap().read);
        assert!(events.take().is_empty(), "a plain report announces nothing of its own");

        // To the end, through the command, as the reader does.
        let done = serde_json::json!({ "progress": {
            "what": { "type": "story", "id": read_me.id }, "mode": "read", "progress": 1.0
        }});
        let value = svc.call(Ctx::local(), "record_progress", done).await.unwrap();
        assert_eq!(value["finished"], true);
        assert!(vault.story(read_me.id).unwrap().read, "it was read");
        let queued: Vec<ContentRef> = vault.queue().unwrap().into_iter().map(|e| e.what).collect();
        assert!(!queued.contains(&what), "out of Up Next");
        assert!(queued.contains(&ContentRef::Story(other.id)));
        assert!(
            queued.contains(&ContentRef::Story(spare.id)),
            "and Up Next was topped up to two again"
        );
        let added =
            vault.queue().unwrap().into_iter().find(|e| e.what == ContentRef::Story(spare.id));
        assert_eq!(added.unwrap().added_by, AddedBy::Recommended);

        let seen = events.take();
        assert!(seen.contains(&(Kind::Queue, Op::Deleted)), "{seen:?}");
        assert!(seen.contains(&(Kind::Story, Op::Updated)), "{seen:?}");
        assert!(seen.contains(&(Kind::Queue, Op::Created)), "{seen:?}");
        assert!(seen.contains(&(Kind::Consumption, Op::Updated)), "the command's own: {seen:?}");

        // Told again that it ended, it is not announced as leaving a second
        // time -- it is not there to leave -- and nothing breaks.
        let again =
            ProgressReport { finished: Some(true), mode: ConsumeMode::Read, ..report(what) };
        record(&svc, again).await.unwrap();
    }

    fn show(vault: &Vault, title: &str, podcast: Option<PodcastSettings>) -> NewsSource {
        let kind = SourceKind::Web { url: format!("https://example.com/{title}.xml") };
        let mut source = NewsSource::new(kind, title);
        source.podcast = podcast;
        vault.save_news_source(&source).unwrap();
        source
    }

    fn policy(vault: &Vault, transcribe: TranscribePolicy) {
        let mut settings = vault.feed_settings().unwrap();
        settings.podcasts.transcribe = transcribe;
        vault.save_feed_settings(&settings).unwrap();
    }

    #[test]
    fn an_episode_is_transcribed_when_played_only_if_the_shows_policy_says_so() {
        let (_svc, vault, _events, _dir) = world();
        let ask = |story: &Story| {
            let story = vault.story(story.id).unwrap();
            to_transcribe(&vault, &story).unwrap()
        };

        // A show on the Feed's defaults, which transcribe once played.
        let plain = show(&vault, "Plain", Some(PodcastSettings::default()));
        let ep = stored_story(&vault, &plain, "Ep", true);
        assert_eq!(ask(&ep), Some(Job::Story(ep.id)));

        // Its own word over the Feed's, either way.
        let off = show(
            &vault,
            "Off",
            Some(PodcastSettings {
                transcribe: Some(TranscribePolicy::Off),
                ..PodcastSettings::default()
            }),
        );
        assert_eq!(ask(&stored_story(&vault, &off, "Ep", true)), None);
        let eager = show(
            &vault,
            "Eager",
            Some(PodcastSettings {
                transcribe: Some(TranscribePolicy::WhenDownloaded),
                ..PodcastSettings::default()
            }),
        );
        assert_eq!(
            ask(&stored_story(&vault, &eager, "Ep", true)),
            None,
            "that one was asked for when it was downloaded, not when played"
        );
        policy(&vault, TranscribePolicy::Off);
        assert_eq!(ask(&ep), None, "the Feed's default is off");
        let asking = show(
            &vault,
            "Asking",
            Some(PodcastSettings {
                transcribe: Some(TranscribePolicy::WhenPlayed),
                ..PodcastSettings::default()
            }),
        );
        let wanted = stored_story(&vault, &asking, "Ep", true);
        assert_eq!(ask(&wanted), Some(Job::Story(wanted.id)));

        // One with a transcript already, and an article, which is nothing to play.
        policy(&vault, TranscribePolicy::WhenPlayed);
        vault.update_story_files(ep.id, |f| f.transcript = Some(FileId::new())).unwrap();
        assert_eq!(ask(&ep), None);
        assert_eq!(ask(&stored_story(&vault, &plain, "Post", false)), None);

        // An episode in a feed nobody followed as a show: the Feed's defaults.
        let blog = show(&vault, "Blog", None);
        let loose = stored_story(&vault, &blog, "Loose", true);
        assert_eq!(ask(&loose), Some(Job::Story(loose.id)));
        policy(&vault, TranscribePolicy::Off);
        assert_eq!(ask(&loose), None);
    }

    #[test]
    fn a_recording_in_files_is_never_transcribed_by_playing_it() {
        let (_svc, vault, _events, dir) = world();
        vault.set_up_files_folder(Some(&dir.path().join("files"))).unwrap();
        // The Feed's default is to transcribe what is played, which is for
        // shows. Music, a family film, a memo, and what the app rendered
        // itself are all in Files too.
        policy(&vault, TranscribePolicy::WhenPlayed);
        let into = |dir: &str, origin| ImportOptions {
            dir: Some(dir.into()),
            ..ImportOptions::new(origin)
        };
        let files = [
            vault.import_bytes(b"x", "song.m4a", &into("Music", FileOrigin::Files)).unwrap(),
            vault.import_bytes(b"x", "party.mp4", &into("Family", FileOrigin::Files)).unwrap(),
            vault.import_bytes(b"x", "Daily.ogg", &into("Feed/Daily", FileOrigin::Feed)).unwrap(),
            vault.import_bytes(b"x", "Article.ogg", &into("Feed/Audio", FileOrigin::Feed)).unwrap(),
        ];
        for file in files {
            for mode in [ConsumeMode::Listen, ConsumeMode::Watch] {
                let first = ProgressReport {
                    mode,
                    position_ms: Some(1_000),
                    ..report(ContentRef::File(file.id))
                };
                let saved = store(&vault, &first, at(0)).unwrap();
                assert_eq!(saved.transcribe, None, "{} asked for a transcript", file.name);
                vault.forget_consumption(saved.consumption.id).unwrap();
            }
        }
    }

    #[test]
    fn only_the_first_report_of_something_heard_asks_for_a_transcript() {
        let (_svc, vault, _events, _dir) = world();
        let source = show(&vault, "Plain", Some(PodcastSettings::default()));
        let ep = stored_story(&vault, &source, "Ep", true);
        let what = ContentRef::Story(ep.id);
        let play = |to: u64| ProgressReport {
            position_ms: Some(to),
            duration_ms: Some(100_000),
            heard: Some(span(0, to)),
            ..report(what)
        };

        assert_eq!(
            store(&vault, &play(15_000), at(0)).unwrap().transcribe,
            Some(Job::Story(ep.id))
        );
        assert_eq!(store(&vault, &play(30_000), at(15)).unwrap().transcribe, None);

        // An episode first opened to read is not being played.
        let other = stored_story(&vault, &source, "Other", true);
        let reading =
            ProgressReport { mode: ConsumeMode::Read, ..report(ContentRef::Story(other.id)) };
        assert_eq!(store(&vault, &reading, at(0)).unwrap().transcribe, None);
        assert_eq!(store(&vault, &play_of(other.id), at(15)).unwrap().transcribe, None);
    }

    fn play_of(story: StoryId) -> ProgressReport {
        ProgressReport { position_ms: Some(5_000), ..report(ContentRef::Story(story)) }
    }

    /// Wait, for as long as a slow machine needs, for `done` to say yes.
    async fn until<T>(mut done: impl FnMut() -> Option<T>) -> T {
        for _ in 0..500 {
            if let Some(found) = done() {
                return found;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("it never happened");
    }

    #[tokio::test]
    async fn finishing_an_article_keeps_it_for_the_memory_without_slowing_the_report() {
        let (svc, vault, events, dir) = world();
        vault.set_up_files_folder(Some(&dir.path().join("files"))).unwrap();
        let blog = show(&vault, "Blog", None);
        let post = stored_story(&vault, &blog, "Slow gardens", false);
        let what = ContentRef::Story(post.id);

        // Half way down: nothing is kept.
        let half = ProgressReport { mode: ConsumeMode::Read, progress: Some(0.5), ..report(what) };
        record(&svc, half).await.unwrap();
        assert!(vault.story(post.id).unwrap().files.text.is_none());

        let done = ProgressReport { mode: ConsumeMode::Read, finished: Some(true), ..report(what) };
        let c = record(&svc, done).await.unwrap();
        assert!(c.finished, "the report did not wait for the file");
        let file = until(|| vault.story(post.id).unwrap().files.text).await;
        let kept = vault.file(file).unwrap();
        assert_eq!(kept.path(), Some(&format!("Feed/Articles/Blog/{}", kept.name)[..]));
        until(|| vault.consumption(&what).unwrap().filter(|c| c.text_file == Some(file))).await;
        // Announced once everything is linked: a moment after the links land.
        let mut seen = Vec::new();
        until(|| {
            seen.extend(events.take());
            seen.contains(&(Kind::File, Op::Created)).then_some(())
        })
        .await;

        // Told again that it ended, it is not written a second time.
        let again =
            ProgressReport { mode: ConsumeMode::Read, finished: Some(true), ..report(what) };
        record(&svc, again).await.unwrap();
        keep::keep_article(&svc, post.id).await.unwrap();
        assert_eq!(vault.story(post.id).unwrap().files.text, Some(file));
        let folder = dir.path().join("files/Feed/Articles/Blog");
        assert_eq!(std::fs::read_dir(folder).unwrap().count(), 1);
        crate::files::settled().await;
    }

    /// A newsletter issue in somebody's inbox, built directly: all a story
    /// needs to be a newsletter's is a message to point at.
    fn a_newsletter_issue(
        vault: &Vault,
    ) -> (karmayoga_core::id::MailMessageId, karmayoga_core::id::ThreadId) {
        use karmayoga_core::account::{Account, Provider};
        use karmayoga_core::id::{MailMessageId, PackId, ThreadId};
        use karmayoga_core::mail::{
            Address, CategorySource, Mailbox, MailboxRole, Message, MessageFlags,
        };
        use karmayoga_core::packstore::PackRef;
        use karmayoga_core::store::mail::IngestMessage;

        let mut account = Account::new(Provider::Custom, "me@example.com");
        account.services.mail = true;
        vault.save_account(&account).unwrap();
        let mailbox = Mailbox::new(account.id, "INBOX", MailboxRole::Inbox);
        vault.save_mailbox(&mailbox).unwrap();
        let (id, thread) = (MailMessageId::new(), ThreadId::new());
        let message = Message {
            id,
            account_id: account.id,
            thread_id: thread,
            message_id_header: format!("<{id}@news.example>"),
            date: Timestamp::from_second(Timestamp::now().as_second() - 3600).unwrap(),
            from: Address::new("The Weekly", "news@example.com"),
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: "Issue 12".into(),
            snippet: "This week".into(),
            flags: MessageFlags::default(),
            labels: Vec::new(),
            has_attachments: false,
            size: 128,
            category: None,
            category_source: CategorySource::Rules,
            pack: PackRef {
                account: account.id.to_string(),
                pack: PackId::new(),
                offset: 0,
                len: 0,
            },
            gmail: None,
            invite: None,
        };
        vault
            .ingest_mail(account.id, vec![IngestMessage { message, mailbox: mailbox.id, uid: 1 }])
            .unwrap();
        (id, thread)
    }

    #[tokio::test]
    async fn finishing_a_newsletter_marks_its_message_read_without_tripping_the_command() {
        // Through the command, in a debug build: what the command declares
        // (the consumption) must be all it touched, so the mail writes that
        // follow from finishing are made in a scope of their own and announce
        // themselves.
        let (svc, vault, events, _dir) = world();
        let (message, thread) = a_newsletter_issue(&vault);
        let source =
            NewsSource::new(SourceKind::Mail { sender: "news@example.com".into() }, "The Weekly");
        vault.save_news_source(&source).unwrap();
        let key = vault.news_key("issue-12").unwrap();
        let mut issue = Story::new(source.id, key, "Issue 12", Timestamp::now());
        issue.mail_message = Some(message);
        vault.insert_stories(&[(issue.clone(), None)]).unwrap();
        assert!(!vault.mail_message(message).unwrap().flags.seen);
        events.take();

        let done = serde_json::json!({ "progress": {
            "what": { "type": "story", "id": issue.id }, "mode": "read", "finished": true
        }});
        let value = svc.call(Ctx::local(), "record_progress", done).await.unwrap();
        assert_eq!(value["finished"], true);

        assert!(vault.mail_message(message).unwrap().flags.seen, "its message was marked read");
        assert_eq!(
            vault.ops_for_thread(thread, 10).unwrap().len(),
            1,
            "and the server will be told"
        );
        let seen = events.take();
        assert!(seen.contains(&(Kind::Thread, Op::Updated)), "mail's windows reload: {seen:?}");
        assert!(seen.contains(&(Kind::Consumption, Op::Updated)), "{seen:?}");
    }
}
