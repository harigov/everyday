//! The Feed: sources, what the refresher learned about them, and the stories
//! they brought.
//!
//! Everything goes through [`Vault::with_news`], which fails with
//! `unsupported` on a backend that holds journals only. Like the calendar,
//! this layer is handed things already fetched and already parsed -- the
//! core has no socket -- and owns what is worth testing about them: the
//! validation, the order a sidebar is drawn in, and the keyed hash that makes
//! a story the same story twice.

use super::Vault;
use super::session::Domain;
use crate::error::{Error, Result};
use crate::id::{MailMessageId, NewsSourceId, StoryId};
use crate::news::{
    CleanupDefaults, ExpireReport, ExpiryRule, FetchState, NEWS_KEY_LABEL, NewsSource, SourceCount,
    SourceKind, SourceSummary, Story, StoryBody,
};
use crate::record::RecordKind;
use crate::store::news::{NewsStore, StoryCursor, StoryPage, StoryQuery};
use jiff::Timestamp;
use std::collections::{HashMap, HashSet};

impl Vault {
    /// Does this vault's backend store a feed?
    pub fn supports_news(&self) -> bool {
        self.with_news(|_| Ok(())).is_ok()
    }

    pub(super) fn with_news<T>(&self, f: impl FnOnce(&dyn NewsStore) -> Result<T>) -> Result<T> {
        self.with_domain(Domain::News, |s| s.news().map(f))
    }

    /// The dedupe key for a story whose identity -- its canonical address,
    /// or its guid when it has none -- is `identity`: a BLAKE3 keyed hash
    /// under a subkey of this vault's own, hex, 32 characters.
    ///
    /// Keyed so the clear `dedupe_key` column cannot be checked against a
    /// list of known addresses to learn what somebody reads. Deterministic
    /// for one vault, which is all a dedupe key needs to be.
    pub fn news_key(&self, identity: &str) -> Result<String> {
        let key = self.derive_subkey(NEWS_KEY_LABEL)?;
        let hash = blake3::keyed_hash(key.expose(), identity.as_bytes());
        Ok(hash.to_hex()[..32].to_string())
    }

    // ---- sources --------------------------------------------------------

    /// Every source, in the sidebar's order: by folder (unfiled first), then
    /// by `sort_order`, then by title.
    pub fn news_sources(&self) -> Result<Vec<NewsSource>> {
        self.with_news(|n| {
            let mut sources = n.list_sources()?;
            sort_sources(&mut sources);
            Ok(sources)
        })
    }

    pub fn news_source(&self, id: NewsSourceId) -> Result<NewsSource> {
        self.with_news(|n| n.get_source(id))
    }

    /// Every source with its counts and its refresher's view of its health,
    /// in the sidebar's order. One decrypt of the sources and their fetch
    /// state, one `GROUP BY` for the counts.
    pub fn news_source_summaries(&self) -> Result<Vec<SourceSummary>> {
        self.with_news(|n| {
            let mut sources = n.list_sources()?;
            sort_sources(&mut sources);
            let counts: HashMap<NewsSourceId, SourceCount> =
                n.counts()?.into_iter().map(|c| (c.source_id, c)).collect();
            sources
                .into_iter()
                .map(|source| {
                    let state = n.fetch_state(source.id)?;
                    let count = counts.get(&source.id);
                    Ok(SourceSummary {
                        unread: count.map_or(0, |c| c.unread),
                        total: count.map_or(0, |c| c.total),
                        last_ok: state.as_ref().and_then(|s| s.last_ok),
                        last_error: state.as_ref().and_then(|s| s.last_error.clone()),
                        next_at: state.as_ref().map(|s| s.next_at),
                        source,
                    })
                })
                .collect()
        })
    }

    pub fn save_news_source(&self, source: &NewsSource) -> Result<()> {
        self.writable()?;
        validate_source(source)?;
        self.with_news(|n| n.put_source(source))?;
        self.wrote(RecordKind::NewsSource, source.id);
        Ok(())
    }

    /// Unfollow: the source, every story it brought (saved ones too), their
    /// bodies and its fetch state. Only the source is recorded as touched;
    /// the cascade has no ids in hand here to name.
    pub fn delete_news_source(&self, id: NewsSourceId) -> Result<()> {
        self.writable()?;
        self.with_news(|n| n.delete_source(id))?;
        self.wrote(RecordKind::NewsSource, id);
        Ok(())
    }

    // ---- what the refresher learned -------------------------------------

    pub fn news_fetch_state(&self, id: NewsSourceId) -> Result<Option<FetchState>> {
        self.with_news(|n| n.fetch_state(id))
    }

    /// Machine state, not a record anybody edits, so nothing is recorded as
    /// touched: no list redraws because a validator changed.
    pub fn save_news_fetch_state(&self, state: &FetchState) -> Result<()> {
        self.writable()?;
        self.with_news(|n| n.put_fetch_state(state))
    }

    pub fn due_news_sources(&self, now: Timestamp) -> Result<Vec<NewsSourceId>> {
        self.with_news(|n| n.due_sources(now))
    }

    pub fn next_news_due(&self) -> Result<Option<Timestamp>> {
        self.with_news(|n| n.next_due())
    }

    // ---- stories --------------------------------------------------------

    /// Store what a refresh brought. Idempotent by `(source, key)`; returns
    /// the ids actually inserted.
    pub fn insert_stories(&self, stories: &[(Story, Option<StoryBody>)]) -> Result<Vec<StoryId>> {
        self.writable()?;
        for (story, body) in stories {
            if story.key.is_empty() {
                return Err(Error::Invalid("a story needs a dedupe key".into()));
            }
            if let Some(body) = body
                && body.story_id != story.id
            {
                return Err(Error::Invalid("a story's body names a different story".into()));
            }
        }
        let inserted = self.with_news(|n| n.insert_stories(stories))?;
        for id in &inserted {
            self.wrote(RecordKind::Story, id);
        }
        Ok(inserted)
    }

    pub fn story_by_key(&self, key: &str) -> Result<Option<Story>> {
        self.with_news(|n| n.story_by_key(key))
    }

    pub fn story_keys(&self, source: NewsSourceId) -> Result<Vec<String>> {
        self.with_news(|n| n.keys_for_source(source))
    }

    /// Rewrite the fields a refresh owns after insert. See
    /// [`NewsStore::update_story`].
    pub fn update_story(&self, story: &Story) -> Result<()> {
        self.writable()?;
        self.with_news(|n| n.update_story(story))?;
        self.wrote(RecordKind::Story, story.id);
        Ok(())
    }

    /// Move stories to another source. See [`NewsStore::move_stories`].
    pub fn move_stories(&self, moves: &[(StoryId, NewsSourceId)]) -> Result<u32> {
        self.writable()?;
        let moved = self.with_news(|n| n.move_stories(moves))?;
        for (story, _) in moves {
            self.wrote(RecordKind::Story, *story);
        }
        Ok(moved)
    }

    pub fn stories(&self, query: &StoryQuery) -> Result<StoryPage> {
        self.with_news(|n| n.list_stories(query))
    }

    pub fn story(&self, id: StoryId) -> Result<Story> {
        self.with_news(|n| n.get_story(id))
    }

    pub fn stories_by_id(&self, ids: &[StoryId]) -> Result<Vec<Story>> {
        self.with_news(|n| n.get_stories(ids))
    }

    pub fn stories_for_mail(
        &self,
        messages: &[MailMessageId],
    ) -> Result<Vec<(MailMessageId, StoryId)>> {
        self.with_news(|n| n.stories_for_mail(messages))
    }

    pub fn story_body(&self, id: StoryId) -> Result<Option<StoryBody>> {
        self.with_news(|n| n.get_body(id))
    }

    pub fn save_story_body(&self, body: &StoryBody) -> Result<()> {
        self.writable()?;
        self.with_news(|n| n.put_body(body))?;
        self.wrote(RecordKind::Story, body.story_id);
        Ok(())
    }

    /// Mark these stories read or unread. A mail-backed story's column is
    /// written too, but means nothing: its read state is its message's, and
    /// the caller is expected to mark the message through mail.
    pub fn set_stories_read(&self, ids: &[StoryId], read: bool) -> Result<()> {
        self.writable()?;
        self.with_news(|n| n.set_read(ids, read))?;
        for id in ids {
            self.wrote(RecordKind::Story, id);
        }
        Ok(())
    }

    /// Mark everything unread in these sources (all of them when empty),
    /// published at or before `up_to`, read. Returns the ids that changed.
    pub fn mark_all_stories_read(
        &self,
        sources: &[NewsSourceId],
        up_to: Timestamp,
    ) -> Result<Vec<StoryId>> {
        self.writable()?;
        let changed = self.with_news(|n| n.mark_all_read(sources, up_to))?;
        for id in &changed {
            self.wrote(RecordKind::Story, id);
        }
        Ok(changed)
    }

    pub fn set_stories_saved(&self, ids: &[StoryId], saved: bool) -> Result<()> {
        self.writable()?;
        self.with_news(|n| n.set_saved(ids, saved))?;
        for id in ids {
            self.wrote(RecordKind::Story, id);
        }
        Ok(())
    }

    pub fn news_counts(&self) -> Result<Vec<SourceCount>> {
        self.with_news(|n| n.counts())
    }

    /// Expire what each source's retention says has had its time, and drop
    /// old tombstones. Reads every source to learn its retention -- sealed,
    /// so the store cannot -- and hands the rules down: a source's own, else
    /// the default for its kind in Settings → Feed.
    ///
    /// The report names the email of each issue cleaned up from a source that
    /// moves its mail to Trash ([`NewsSource::trash_mail`]). It has to be
    /// worked out first: what the store keeps of an expired story is a
    /// tombstone, which no longer says which message it was.
    pub fn expire_stories(&self, now: Timestamp) -> Result<ExpireReport> {
        if !self.is_writable() {
            return Ok(ExpireReport::default());
        }
        self.with_news(|n| {
            let defaults = match n.feed_settings() {
                Ok(settings) => settings.unwrap_or_default().cleanup,
                Err(Error::Unsupported(_)) => CleanupDefaults::default(),
                Err(e) => return Err(e),
            };
            let sources = n.list_sources()?;
            let rules: Vec<ExpiryRule> = sources
                .iter()
                .map(|s| ExpiryRule { source_id: s.id, retention: s.retention_with(&defaults) })
                .collect();
            // What is in Up Next is kept until it is played or taken out.
            let keep: Vec<StoryId> = match n.queue() {
                Ok(queue) => queue
                    .iter()
                    .filter_map(|e| match e.what {
                        crate::news::ContentRef::Story(id) => Some(id),
                        _ => None,
                    })
                    .collect(),
                Err(Error::Unsupported(_)) => Vec::new(),
                Err(e) => return Err(e),
            };
            let binning = issues_to_bin(n, &sources, &defaults, &keep, now)?;
            let mut report = n.expire_except(&rules, &keep, now)?;
            report.mail = binned_mail(n, binning);
            Ok(report)
        })
    }
}

/// The issues the store is about to expire whose source moves its mail to
/// Trash, as `(story, message)`. Only a source that reads mail from a sender
/// ([`SourceKind::Mail`]) can say so -- never the Newsletters source, which
/// takes whatever Mail files under Newsletters, misfiled mail included.
///
/// Mirrors the store's own rule (`NewsStore::expire_except`): a story goes
/// when it is older than its source's days -- by the date it was published
/// -- read or unread, unless it is saved, has files, or is in `keep`.
/// Only what `expire_except` really expired is reported, see [`binned_mail`],
/// so a story saved in between is not a message lost.
fn issues_to_bin(
    n: &dyn NewsStore,
    sources: &[NewsSource],
    defaults: &CleanupDefaults,
    keep: &[StoryId],
    now: Timestamp,
) -> Result<Vec<(StoryId, MailMessageId)>> {
    const DAY_US: i64 = 86_400_000_000;
    let spare: HashSet<StoryId> = keep.iter().copied().collect();
    let mut found = Vec::new();
    for source in sources {
        if !source.trash_mail || !matches!(source.kind, SourceKind::Mail { .. }) {
            continue;
        }
        let retention = source.retention_with(defaults);
        let before =
            |days: u32| now.as_microsecond().saturating_sub(i64::from(days).saturating_mul(DAY_US));
        let (read_before, unread_before) =
            (before(retention.read_days), before(retention.unread_days));
        // Nothing newer than the later of the two cutoffs can be due, so the
        // walk, newest first, starts there rather than decrypting every
        // issue a source still keeps. The id is the greatest there is: the
        // cursor means "older than this (date, id)", and a story published
        // at the cutoff itself is tested below like any other.
        let mut at = Timestamp::from_microsecond(read_before.max(unread_before)).ok().map(|at| {
            StoryCursor { published_at: at, id: StoryId::from(uuid::Uuid::from_u128(u128::MAX)) }
        });
        loop {
            let page = n.list_stories(&StoryQuery {
                sources: vec![source.id],
                before: at,
                limit: StoryQuery::MAX_LIMIT,
                ..Default::default()
            })?;
            for story in &page.stories {
                let Some(message) = story.mail_message else { continue };
                let published = story.published_at.as_microsecond();
                let due =
                    if story.read { published < read_before } else { published < unread_before };
                if due && !story.saved && story.files.media.is_none() && !spare.contains(&story.id)
                {
                    found.push((story.id, message));
                }
            }
            match page.next {
                Some(next) => at = Some(next),
                None => break,
            }
        }
    }
    Ok(found)
}

/// Of the issues [`issues_to_bin`] expected to expire, the messages of those
/// the store did. If that cannot be read, none: a message is only ever sent
/// to Trash on the store's word that its story is gone.
fn binned_mail(n: &dyn NewsStore, expected: Vec<(StoryId, MailMessageId)>) -> Vec<MailMessageId> {
    if expected.is_empty() {
        return Vec::new();
    }
    let ids: Vec<StoryId> = expected.iter().map(|(story, _)| *story).collect();
    match n.get_stories(&ids) {
        Ok(stories) => {
            let gone: HashSet<StoryId> =
                stories.iter().filter(|s| s.expired).map(|s| s.id).collect();
            expected
                .into_iter()
                .filter(|(story, _)| gone.contains(story))
                .map(|(_, message)| message)
                .collect()
        }
        Err(e) => {
            tracing::warn!(error = %e, "expired old issues but could not tell which emails to bin");
            Vec::new()
        }
    }
}

/// Unfiled first, then folders alphabetically; within one, by `sort_order`
/// and then title, case-insensitively.
fn sort_sources(sources: &mut [NewsSource]) {
    sources.sort_by(|a, b| {
        let fa = a.folder.trim().to_lowercase();
        let fb = b.folder.trim().to_lowercase();
        (!fa.is_empty())
            .cmp(&!fb.is_empty())
            .then_with(|| fa.cmp(&fb))
            .then_with(|| a.sort_order.cmp(&b.sort_order))
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
}

fn validate_source(source: &NewsSource) -> Result<()> {
    if source.title.trim().is_empty() {
        return Err(Error::Invalid("a source needs a name".into()));
    }
    match &source.kind {
        SourceKind::Web { url } => {
            let url = url.trim();
            let lower = url.to_ascii_lowercase();
            if !(lower.starts_with("https://") || lower.starts_with("http://")) {
                return Err(Error::Invalid(
                    "a feed's address must start with https:// or http://".into(),
                ));
            }
        }
        SourceKind::HackerNews { .. } | SourceKind::Newsletters => {}
        SourceKind::Mail { sender, .. } => {
            if !sender.contains('@') {
                return Err(Error::Invalid(
                    "a newsletter source needs the sender's address".into(),
                ));
            }
        }
    }
    if source.trash_mail && !matches!(source.kind, SourceKind::Mail { .. }) {
        return Err(Error::Invalid(
            "only a newsletter followed from Mail can move its email to Trash".into(),
        ));
    }
    if let Some(r) = source.retention
        && (r.read_days == 0 || r.unread_days == 0)
    {
        return Err(Error::Invalid("keep stories for at least a day".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(title: &str, folder: &str, order: i64) -> NewsSource {
        let mut s =
            NewsSource::new(SourceKind::Web { url: "https://a.example/feed".into() }, title);
        s.folder = folder.into();
        s.sort_order = order;
        s
    }

    #[test]
    fn the_sidebar_order_is_unfiled_then_folders_then_order_then_title() {
        let mut v =
            vec![src("b", "Tech", 0), src("z", "", 0), src("a", "Tech", 0), src("y", "Art", 0)];
        sort_sources(&mut v);
        let titles: Vec<&str> = v.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["z", "y", "a", "b"]);
    }

    #[test]
    fn a_web_source_needs_an_http_address() {
        let mut s = src("x", "", 0);
        s.kind = SourceKind::Web { url: "file:///etc/passwd".into() };
        assert!(validate_source(&s).is_err());
        s.kind = SourceKind::Web { url: "HTTPS://example.com/feed".into() };
        assert!(validate_source(&s).is_ok());
    }

    #[test]
    fn only_a_newsletter_followed_from_mail_can_bin_its_email() {
        let mut s = src("x", "", 0);
        s.trash_mail = true;
        let refused = |kind: SourceKind| {
            let mut s = s.clone();
            s.kind = kind;
            validate_source(&s).expect_err("not a source that reads a sender")
        };
        refused(SourceKind::Web { url: "https://a.example/feed".into() });
        refused(SourceKind::HackerNews { list: crate::news::HnList::Front, min_points: 0 });
        // Mail files misfiled mail under Newsletters, which is why the one
        // source that takes all of it cannot be told to bin it.
        let Error::Invalid(why) = refused(SourceKind::Newsletters) else { panic!("invalid") };
        assert!(why.contains("followed from Mail"), "{why}");

        s.kind = SourceKind::Mail { sender: "news@a.example".into(), name: "The Download".into() };
        assert!(validate_source(&s).is_ok());
        s.trash_mail = false;
        s.kind = SourceKind::Newsletters;
        assert!(validate_source(&s).is_ok(), "it is only the setting that is refused");
    }
}
