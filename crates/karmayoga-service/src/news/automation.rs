//! What a source does by itself with the stories it brings: summarise them,
//! make articles into audio -- [`NewsSource::automation`]. See
//! `docs/plans/feed-usability.md`.
//!
//! A refresh, or a turn of the Newsletters source, hands what it stored to
//! [`arrived`], which only queues: a summary ([`super::summary`]) and a render
//! ([`super::audio`]) are each a list the Feed's tasks work through, and what
//! happens to them there is shown, failed or not, on the Feed's own list of
//! work.
//!
//! # What is cheap
//!
//! A summary is a request to a model, and audio is minutes of a voice, so
//! neither is made for what is old. A story published more than [`FRESH`] ago
//! is left alone, and a source's very first look, which brings its whole back
//! catalogue at once, does only the newest [`FIRST_LOOK`]. A person can ask
//! for any of the rest with a button.
//!
//! # What a podcast is spared
//!
//! Nothing here is for a show. Whether it summarises ([`NewsSource::summarizes`])
//! is the article written after a transcript ([`super::transcribe`]), and an
//! episode has no article to make into audio: it is audio already.
//!
//! # What is not a failure
//!
//! A vault with no model to summarise with, or a newsletter whose account
//! Mail has not allowed to send to one, is a choice and not a failure, and
//! says so once, on the button, and not against every story that arrives:
//! [`super::summary::request`] refuses, and what it refuses is dropped here.

use super::{audio, summary};
use crate::error::CommandResult;
use crate::service::{Service, blocking};
use jiff::Timestamp;
use karmayoga_core::id::{NewsSourceId, StoryId};
use karmayoga_core::news::NewsSource;
use std::sync::Arc;

/// How old a story may be, in seconds, and still be worked on by itself:
/// forty-eight hours.
const FRESH: i64 = 48 * 60 * 60;

/// How many stories a source's first look works on.
const FIRST_LOOK: usize = 3;

/// `stories` have just arrived from `source`: queue what its automation asks
/// for. Quick: queues work and returns.
///
/// The lookups -- how old each is, whether this is the source's first look --
/// are made on the blocking pool by a task of their own, so a refresh that has
/// just stored a hundred stories does not wait on them.
pub fn arrived(svc: &Arc<Service>, source: &NewsSource, stories: &[StoryId]) {
    let (summarise, audio) = asks_for(source);
    if stories.is_empty() || !(summarise || audio) {
        return;
    }
    // Outside a runtime there is nowhere to do it, and nothing to be done
    // about that: a test calling the refresher directly.
    let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
    let (svc, id, stories) = (svc.clone(), source.id, stories.to_vec());
    runtime.spawn(async move {
        if let Err(e) = apply(&svc, id, stories, summarise, audio).await {
            tracing::debug!(error = %e, "feed: could not apply a source's automation");
        }
    });
}

/// What `source` asks for, as whether to summarise what it brings and whether
/// to make it into audio. Nothing, for a show: see the module docs.
fn asks_for(source: &NewsSource) -> (bool, bool) {
    if source.is_podcast() {
        return (false, false);
    }
    (source.summarizes(), source.automation.audio)
}

/// One new story, as far as automation cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Arrival {
    story: StoryId,
    published_at: Timestamp,
    /// It has a summary in Files already.
    summarised: bool,
}

async fn apply(
    svc: &Arc<Service>,
    source: NewsSourceId,
    inserted: Vec<StoryId>,
    summarise: bool,
    audio: bool,
) -> CommandResult<()> {
    let Some(vault) = svc.get().filter(|v| v.is_unlocked() && v.is_writable()) else {
        return Ok(());
    };
    let now = svc.now();
    let wanted = blocking(move || {
        // A source whose every story is one of these has never had another:
        // this is its first look, and it brought the whole of its back
        // catalogue.
        let first = vault.story_keys(source)?.len() <= inserted.len();
        let arrivals: Vec<Arrival> = vault
            .stories_by_id(&inserted)?
            .into_iter()
            .map(|s| Arrival {
                story: s.id,
                published_at: s.published_at,
                summarised: s
                    .files
                    .summary
                    .is_some_and(|f| vault.file_is_present(f).unwrap_or(false)),
            })
            .collect();
        Ok(chosen(&arrivals, first, now))
    })
    .await?;

    for arrival in wanted {
        if summarise
            && !arrival.summarised
            && let Err(e) = summary::request(svc, arrival.story).await
        {
            tracing::debug!(error = %e.message, "feed: a summary was not asked for");
        }
        if audio && let Err(e) = audio::request(svc, arrival.story).await {
            tracing::debug!(error = %e.message, "feed: audio was not asked for");
        }
    }
    Ok(())
}

/// Which of `arrivals` automation is for: those published within [`FRESH`] of
/// `now`, and on a source's `first` look only the newest [`FIRST_LOOK`] of
/// them. Newest first, which is the order they are asked for in.
fn chosen(arrivals: &[Arrival], first: bool, now: Timestamp) -> Vec<Arrival> {
    let mut fresh: Vec<Arrival> = arrivals
        .iter()
        .filter(|a| now.as_second() - a.published_at.as_second() <= FRESH)
        .copied()
        .collect();
    fresh.sort_by(|a, b| b.published_at.cmp(&a.published_at).then(b.story.cmp(&a.story)));
    if first {
        fresh.truncate(FIRST_LOOK);
    }
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Change, EventSink};
    use crate::news::{WorkKind, WorkState, transcribe};
    use karmayoga_core::Vault;
    use karmayoga_core::VaultConfig;
    use karmayoga_core::news::{Automation, PodcastSettings, SourceKind, Story, StoryBody};
    use std::time::Duration;

    const NOW: &str = "2026-03-09T12:00:00Z";

    fn at(hours_ago: i64) -> Timestamp {
        let now: Timestamp = NOW.parse().unwrap();
        Timestamp::from_second(now.as_second() - hours_ago * 3_600).unwrap()
    }

    fn arrival(hours_ago: i64) -> Arrival {
        Arrival { story: StoryId::new(), published_at: at(hours_ago), summarised: false }
    }

    // ── Which of what arrived ────────────────────────────────────────────

    #[test]
    fn what_was_published_more_than_two_days_ago_is_left_alone() {
        let now: Timestamp = NOW.parse().unwrap();
        let (new, edge, old) = (arrival(1), arrival(48), arrival(49));
        let picked = chosen(&[old, new, edge], false, now);
        assert_eq!(picked, [new, edge], "newest first, and an hour past the edge is out");
        assert!(chosen(&[old], false, now).is_empty());
        // A story dated in the future -- a clock that is wrong -- is new.
        let future = arrival(-5);
        assert_eq!(chosen(&[future], false, now), [future]);
    }

    #[test]
    fn a_sources_first_look_is_only_its_newest_three() {
        let now: Timestamp = NOW.parse().unwrap();
        let all: Vec<Arrival> = (1..=8).map(arrival).collect();
        let first = chosen(&all, true, now);
        assert_eq!(first, all[..3], "the newest three of the ones that came");
        assert_eq!(chosen(&all, false, now).len(), 8, "a later turn is not limited");
        // The limit is on the fresh ones: three of an old catalogue are no use.
        let mixed = [arrival(60), arrival(1), arrival(70), arrival(2)];
        assert_eq!(chosen(&mixed, true, now), [mixed[1], mixed[3]]);
    }

    // ── In a vault ───────────────────────────────────────────────────────

    #[derive(Default)]
    struct Quiet;

    impl EventSink for Quiet {
        fn changed(&self, _: Change) {}
    }

    struct Fixture {
        svc: Arc<Service>,
        vault: Arc<Vault>,
        _folder: tempfile::TempDir,
        _home: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let (home, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let config = VaultConfig { password: None, ..Default::default() };
        let vault = karmayoga_vault::create(home.path(), config).unwrap();
        let svc = Arc::new(Service::new());
        svc.set_events(Arc::new(Quiet));
        let vault = svc.set(vault);
        vault.set_up_files_folder(Some(folder.path())).unwrap();
        Fixture { svc, vault, _folder: folder, _home: home }
    }

    /// A vault with a model a summary can be asked for: one on loopback needs
    /// no key, and nothing here is ever asked of it.
    fn fixture_with_a_model() -> Fixture {
        let f = fixture();
        let mut models = f.vault.model_settings().unwrap();
        models.medium.connection.base_url = Some("http://127.0.0.1:1/v1".into());
        models.medium.model.model = "scripted".into();
        f.vault.save_model_settings(&models).unwrap();
        f
    }

    fn source(vault: &Vault, title: &str, automation: Automation) -> NewsSource {
        let kind = SourceKind::Web { url: format!("https://example.com/{title}.xml") };
        let mut source = NewsSource::new(kind, title);
        source.automation = automation;
        vault.save_news_source(&source).unwrap();
        source
    }

    /// A story from `source` published `hours_ago` hours before now, with a
    /// body.
    fn story(f: &Fixture, source: &NewsSource, title: &str, hours_ago: i64) -> StoryId {
        let mut story = Story::new(source.id, format!("key {title}"), title, Timestamp::now());
        story.published_at =
            Timestamp::from_second(Timestamp::now().as_second() - hours_ago * 3_600).unwrap();
        let body = StoryBody {
            story_id: story.id,
            html: format!("<p>The words of {title}.</p>"),
            text: format!("The words of {title}."),
            origin: karmayoga_core::news::BodyOrigin::Feed,
            images: Vec::new(),
        };
        f.vault.insert_stories(&[(story.clone(), Some(body))]).unwrap();
        story.id
    }

    /// What `apply` queued, as `(summaries, audio)` of the stories named.
    fn queued(f: &Fixture) -> (Vec<StoryId>, Vec<StoryId>) {
        let kind = |items: Vec<crate::news::WorkItem>, kind| -> Vec<StoryId> {
            items.iter().filter(|i| i.kind == kind).filter_map(|i| i.story_id).collect()
        };
        (
            kind(transcribe::status(&f.svc), WorkKind::Summary),
            kind(audio::status(&f.svc), WorkKind::Audio),
        )
    }

    /// What `arrived` does, run to its end where it stands and not in a task
    /// of its own.
    async fn arrive(f: &Fixture, source: &NewsSource, stories: &[StoryId]) {
        let (summarise, audio) = asks_for(source);
        apply(&f.svc, source.id, stories.to_vec(), summarise, audio).await.unwrap();
    }

    #[tokio::test]
    async fn a_source_that_summarises_has_a_summary_asked_for_each_new_story() {
        let f = fixture_with_a_model();
        let automation = Automation { summarize: Some(true), audio: false };
        let show = source(&f.vault, "Blog", automation);
        // A later turn: a story was there before these.
        story(&f, &show, "Before", 300);
        let (a, b) = (story(&f, &show, "A", 1), story(&f, &show, "B", 2));
        arrive(&f, &show, &[a, b]).await;
        let (summaries, audio) = queued(&f);
        assert_eq!(summaries.len(), 2);
        assert!(summaries.contains(&a) && summaries.contains(&b), "{summaries:?}");
        assert!(audio.is_empty(), "audio was not asked for");
        let items = transcribe::status(&f.svc);
        assert!(items.iter().all(|i| i.state == WorkState::Waiting && i.kind == WorkKind::Summary));
    }

    #[tokio::test]
    async fn a_source_that_asks_for_audio_has_each_new_article_rendered_and_nothing_else() {
        let f = fixture_with_a_model();
        let show = source(&f.vault, "Blog", Automation { summarize: None, audio: true });
        let a = story(&f, &show, "A", 1);
        story(&f, &show, "Before", 300);
        arrive(&f, &show, &[a]).await;
        assert_eq!(queued(&f), (Vec::new(), vec![a]));

        // And both, when both are asked for.
        let both = source(&f.vault, "Both", Automation { summarize: Some(true), audio: true });
        let b = story(&f, &both, "B", 1);
        story(&f, &both, "Before", 300);
        arrive(&f, &both, &[b]).await;
        assert_eq!(queued(&f), (vec![b], vec![a, b]));
    }

    #[tokio::test]
    async fn a_source_that_asks_for_nothing_has_nothing_done() {
        let f = fixture_with_a_model();
        let plain = source(&f.vault, "Plain", Automation::default());
        let a = story(&f, &plain, "A", 1);
        arrive(&f, &plain, &[a]).await;
        assert_eq!(queued(&f), (Vec::new(), Vec::new()));
        // Nor does one that was told in so many words not to.
        let no = source(&f.vault, "No", Automation { summarize: Some(false), audio: false });
        let b = story(&f, &no, "B", 1);
        arrive(&f, &no, &[b]).await;
        assert_eq!(queued(&f), (Vec::new(), Vec::new()));
    }

    #[tokio::test]
    async fn a_podcast_is_never_summarised_here_or_made_into_audio() {
        let f = fixture_with_a_model();
        let mut show = source(&f.vault, "Show", Automation { summarize: Some(true), audio: true });
        show.podcast = Some(PodcastSettings::default());
        f.vault.save_news_source(&show).unwrap();
        let ep = story(&f, &show, "Ep", 1);
        story(&f, &show, "Before", 300);
        arrive(&f, &show, &[ep]).await;
        assert_eq!(queued(&f), (Vec::new(), Vec::new()));
    }

    #[tokio::test]
    async fn only_what_is_new_is_worked_on_and_a_first_look_is_the_newest_three() {
        let f = fixture_with_a_model();
        let show = source(&f.vault, "Blog", Automation { summarize: Some(true), audio: true });
        // A source that already had a story: a turn like any other.
        story(&f, &show, "Before", 300);
        let (fresh, old) = (story(&f, &show, "Fresh", 3), story(&f, &show, "Old", 72));
        arrive(&f, &show, &[fresh, old]).await;
        assert_eq!(queued(&f), (vec![fresh], vec![fresh]), "the old one is left alone");

        // A source seen for the first time: its whole back catalogue arrives.
        let new_show =
            source(&f.vault, "Archive", Automation { summarize: Some(true), audio: false });
        let all: Vec<StoryId> =
            (1..=6).map(|n| story(&f, &new_show, &format!("Story {n}"), n)).collect();
        arrive(&f, &new_show, &all).await;
        let (summaries, _) = queued(&f);
        let from_archive: Vec<&StoryId> = summaries.iter().filter(|s| all.contains(s)).collect();
        assert_eq!(from_archive.len(), 3);
        assert!(all[..3].iter().all(|s| summaries.contains(s)), "the newest three: {summaries:?}");
    }

    #[tokio::test]
    async fn one_that_has_its_summary_already_is_not_asked_for_again() {
        let f = fixture_with_a_model();
        let show = source(&f.vault, "Blog", Automation { summarize: Some(true), audio: false });
        story(&f, &show, "Before", 300);
        let a = story(&f, &show, "A", 1);
        let options =
            karmayoga_core::files::ImportOptions::new(karmayoga_core::files::FileOrigin::Feed);
        let file = f.vault.import_bytes(b"x", "a.md", &options).unwrap();
        f.vault.update_story_files(a, |files| files.summary = Some(file.id)).unwrap();
        arrive(&f, &show, &[a]).await;
        assert_eq!(queued(&f), (Vec::new(), Vec::new()));
    }

    #[tokio::test]
    async fn a_vault_with_no_model_is_not_a_failure_for_every_story() {
        let f = fixture();
        let show = source(&f.vault, "Blog", Automation { summarize: Some(true), audio: false });
        story(&f, &show, "Before", 300);
        let a = story(&f, &show, "A", 1);
        arrive(&f, &show, &[a]).await;
        assert!(transcribe::status(&f.svc).is_empty(), "nothing queued, nothing failed");
    }

    #[tokio::test]
    async fn arrived_does_it_in_the_background_and_a_show_is_left_out_of_it() {
        let f = fixture_with_a_model();
        let blog = source(&f.vault, "Blog", Automation { summarize: Some(true), audio: false });
        story(&f, &blog, "Before", 300);
        let a = story(&f, &blog, "A", 1);
        arrived(&f.svc, &blog, &[a]);
        let seen = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let (summaries, _) = queued(&f);
                if !summaries.is_empty() {
                    break summaries;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the summary was asked for");
        assert_eq!(seen, [a]);

        // A podcast that was told to do everything is not asked for anything.
        let mut show = source(&f.vault, "Show", Automation { summarize: Some(true), audio: true });
        show.podcast = Some(PodcastSettings::default());
        assert_eq!(asks_for(&show), (false, false));
        assert_eq!(asks_for(&blog), (true, false));
    }
}
