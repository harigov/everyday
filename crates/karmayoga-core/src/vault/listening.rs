//! Podcasts' files, Up Next, the Feed's memory of what was read and heard,
//! and the ranks that order Recommended. See [`crate::news::listening`] and
//! `docs/plans/podcasts.md`.
//!
//! Everything goes through [`Vault::with_news`], so a backend with no feed
//! refuses all of it the same way.

use super::Vault;
use crate::error::{Error, Result};
use crate::files::{RefKind, RefOwner};
use crate::id::{ConsumptionId, QueueEntryId, StoryId};
use crate::news::listening::QUEUE_GAP;
use crate::news::{
    ConsumedQuery, Consumption, ContentRef, Criterion, FeedSettings, NewsSource, PodcastPolicy,
    QueueEntry, Rank, Story, StoryFiles,
};
use crate::record::RecordKind;
use jiff::Timestamp;

/// The most criteria a ranking may have. Each one is a request per story.
pub const MAX_CRITERIA: usize = 12;
/// The longest a criterion's words may be, in characters.
pub const MAX_CRITERION_CHARS: usize = 240;
/// The most Up Next may be topped up to.
pub const MAX_AUTOFILL: u32 = 50;

/// Where a new entry goes in Up Next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "id", rename_all = "camelCase")]
pub enum QueuePlace {
    /// After everything.
    #[default]
    End,
    /// Before everything: what plays next.
    Next,
    /// Just before this entry; at the end if it is gone.
    Before(QueueEntryId),
}

impl Vault {
    // ---- the Feed's settings ----------------------------------------------

    /// The Feed's settings, or the defaults if they were never saved.
    pub fn feed_settings(&self) -> Result<FeedSettings> {
        self.with_news(|n| n.feed_settings()).map(Option::unwrap_or_default)
    }

    /// Check, tidy and save the Feed's settings; answers what was saved.
    pub fn save_feed_settings(&self, settings: &FeedSettings) -> Result<FeedSettings> {
        self.writable()?;
        let settings = tidy_settings(settings)?;
        self.with_news(|n| n.put_feed_settings(&settings))?;
        Ok(settings)
    }

    /// What applies to `source`, if it is a podcast: its own settings over
    /// the Feed's defaults.
    pub fn podcast_policy(&self, source: &NewsSource) -> Result<Option<PodcastPolicy>> {
        let Some(show) = &source.podcast else { return Ok(None) };
        let defaults = self.feed_settings()?.podcasts;
        Ok(Some(PodcastPolicy::of(show, &defaults)))
    }

    // ---- a story's files ------------------------------------------------

    /// Change a story's files with `change`, and record which files it now
    /// refers to. Answers the story as it is now. The one way anything links
    /// a story to a file: a refresh never does.
    pub fn update_story_files(
        &self,
        id: StoryId,
        change: impl FnOnce(&mut StoryFiles),
    ) -> Result<Story> {
        self.writable()?;
        let mut change = Some(change);
        let story = self.with_news(|n| {
            n.change_story_files(id, &mut |files| {
                if let Some(change) = change.take() {
                    change(files)
                }
            })
        })?;
        self.track_file_refs(RefOwner::new(RefKind::Story, id), story.files.all());
        self.wrote(RecordKind::Story, id);
        Ok(story)
    }

    /// Every live story with files.
    pub fn stories_with_files(&self) -> Result<Vec<Story>> {
        self.with_news(|n| n.stories_with_files())
    }

    /// Every live story with a downloaded episode, for clean-up.
    pub fn stories_with_media(&self) -> Result<Vec<Story>> {
        self.with_news(|n| n.stories_with_media())
    }

    // ---- Up Next ----------------------------------------------------------

    /// Up Next, in order.
    pub fn queue(&self) -> Result<Vec<QueueEntry>> {
        self.with_news(|n| n.queue())
    }

    /// Put `entry` in Up Next at `place`. Something already queued is moved
    /// there, keeping its entry's id. Answers the entry as stored.
    pub fn add_to_queue(&self, entry: QueueEntry, place: QueuePlace) -> Result<QueueEntry> {
        self.writable()?;
        if entry.title.trim().is_empty() {
            return Err(Error::Invalid("something in Up Next needs a title".into()));
        }
        self.with_news(|n| {
            let queue = n.queue()?;
            let mut entry = entry;
            if let Some(existing) = queue.iter().find(|e| e.what == entry.what) {
                entry.id = existing.id;
                entry.added_at = existing.added_at;
            }
            let others: Vec<&QueueEntry> = queue.iter().filter(|e| e.id != entry.id).collect();
            put_at(n, &others, entry, place)
        })
    }

    /// Move an entry to just before `before`, or to the end.
    pub fn move_in_queue(&self, id: QueueEntryId, before: Option<QueueEntryId>) -> Result<()> {
        self.writable()?;
        self.with_news(|n| {
            let queue = n.queue()?;
            let Some(entry) = queue.iter().find(|e| e.id == id).cloned() else {
                return Err(Error::NotFound { kind: "queue entry", id: id.to_string() });
            };
            let others: Vec<&QueueEntry> = queue.iter().filter(|e| e.id != id).collect();
            let place = before.map_or(QueuePlace::End, QueuePlace::Before);
            put_at(n, &others, entry, place).map(|_| ())
        })
    }

    pub fn remove_from_queue(&self, ids: &[QueueEntryId]) -> Result<()> {
        self.writable()?;
        self.with_news(|n| n.delete_queue_entries(ids))
    }

    /// Take these things out of Up Next, wherever they are in it.
    pub fn remove_from_queue_for(&self, what: &[ContentRef]) -> Result<()> {
        self.writable()?;
        self.with_news(|n| n.delete_queue_for(what))
    }

    // ---- what was read, heard and watched ----------------------------------

    pub fn consumption(&self, what: &ContentRef) -> Result<Option<Consumption>> {
        self.with_news(|n| n.consumption(what))
    }

    pub fn consumptions_for(&self, what: &[ContentRef]) -> Result<Vec<Consumption>> {
        self.with_news(|n| n.consumptions_for(what))
    }

    /// Newest first.
    pub fn consumptions(&self, query: &ConsumedQuery) -> Result<Vec<Consumption>> {
        self.with_news(|n| n.consumptions(query))
    }

    /// Insert or replace. The caller has already merged what it learned into
    /// what was stored (see `karmayoga_service::news::consumed`).
    pub fn save_consumption(&self, consumption: &Consumption) -> Result<()> {
        self.writable()?;
        if !(0.0..=1.0).contains(&consumption.progress) {
            return Err(Error::Invalid("progress is a fraction from 0 to 1".into()));
        }
        self.with_news(|n| n.put_consumption(consumption))
    }

    /// Forget one thing was read or heard. Its kept words stay in the files
    /// folder; only the record goes.
    pub fn forget_consumption(&self, id: ConsumptionId) -> Result<()> {
        self.writable()?;
        self.with_news(|n| n.delete_consumption(id))
    }

    // ---- ranks --------------------------------------------------------------

    pub fn ranks(&self, what: &[ContentRef]) -> Result<Vec<Rank>> {
        self.with_news(|n| n.ranks(what))
    }

    /// Machine state: nothing is recorded as touched.
    pub fn save_ranks(&self, ranks: &[Rank]) -> Result<()> {
        self.writable()?;
        self.with_news(|n| n.put_ranks(ranks))
    }

    pub fn clear_ranks(&self, before: Option<Timestamp>) -> Result<u32> {
        self.writable()?;
        self.with_news(|n| n.clear_ranks(before))
    }
}

/// Store `entry` at `place` among `others` (in order, not including it). When
/// there is no room left between its neighbours -- a slot dragged into a
/// dozen times halves its gap away -- every entry is renumbered a
/// [`QUEUE_GAP`] apart instead, in the order wanted, so a move always lands
/// exactly where it was dropped.
fn put_at(
    n: &dyn crate::store::news::NewsStore,
    others: &[&QueueEntry],
    mut entry: QueueEntry,
    place: QueuePlace,
) -> Result<QueueEntry> {
    let index = index_of(others, place);
    let order = place_at(others, place);
    let after_previous = index == 0 || order > others[index - 1].sort_order;
    let before_next = index == others.len() || order < others[index].sort_order;
    if after_previous && before_next {
        entry.sort_order = order;
        n.put_queue_entry(&entry)?;
        return Ok(entry);
    }
    let mut ordered: Vec<QueueEntry> = others.iter().map(|e| (*e).clone()).collect();
    ordered.insert(index, entry);
    for (i, e) in ordered.iter_mut().enumerate() {
        e.sort_order = (i as i64).saturating_mul(QUEUE_GAP);
        n.put_queue_entry(e)?;
    }
    Ok(ordered.swap_remove(index))
}

/// Where `place` falls among `others`, as an index to insert at.
fn index_of(others: &[&QueueEntry], place: QueuePlace) -> usize {
    match place {
        QueuePlace::End => others.len(),
        QueuePlace::Next => 0,
        QueuePlace::Before(id) => others.iter().position(|e| e.id == id).unwrap_or(others.len()),
    }
}

/// The `sort_order` that puts something at `place` among `others`, which
/// are in order and do not include it.
fn place_at(others: &[&QueueEntry], place: QueuePlace) -> i64 {
    let first = others.first().map(|e| e.sort_order);
    let last = others.last().map(|e| e.sort_order);
    match place {
        QueuePlace::End => last.map_or(0, |l| l.saturating_add(QUEUE_GAP)),
        QueuePlace::Next => first.map_or(0, |f| f.saturating_sub(QUEUE_GAP)),
        QueuePlace::Before(id) => match others.iter().position(|e| e.id == id) {
            None => last.map_or(0, |l| l.saturating_add(QUEUE_GAP)),
            Some(0) => others[0].sort_order.saturating_sub(QUEUE_GAP),
            Some(i) => {
                let (lo, hi) = (others[i - 1].sort_order, others[i].sort_order);
                // Halfway; with no room left between them this answers one
                // of the two, and `put_at` renumbers instead.
                lo + (hi - lo) / 2
            }
        },
    }
}

fn tidy_settings(settings: &FeedSettings) -> Result<FeedSettings> {
    let mut s = settings.clone();
    let mut seen = std::collections::HashSet::new();
    let mut criteria: Vec<Criterion> = Vec::new();
    for c in &s.ranking.criteria {
        let text = c.text.trim();
        if text.is_empty() {
            continue;
        }
        if text.chars().count() > MAX_CRITERION_CHARS {
            return Err(Error::Invalid(format!(
                "a ranking criterion can be at most {MAX_CRITERION_CHARS} characters"
            )));
        }
        let id = c.id.trim();
        if id.is_empty() || !seen.insert(id.to_string()) {
            return Err(Error::Invalid("every ranking criterion needs an id of its own".into()));
        }
        criteria.push(Criterion {
            id: id.to_string(),
            text: text.to_string(),
            weight: c.weight.clamp(-3, 3),
        });
    }
    if criteria.len() > MAX_CRITERIA {
        return Err(Error::Invalid(format!("a ranking can have at most {MAX_CRITERIA} criteria")));
    }
    s.ranking.criteria = criteria;
    s.ranking.window_days = s.ranking.window_days.clamp(1, 60);
    s.autofill = s.autofill.map(|n| n.min(MAX_AUTOFILL)).filter(|n| *n > 0);
    s.skip_back_s = s.skip_back_s.clamp(5, 120);
    s.skip_forward_s = s.skip_forward_s.clamp(5, 120);
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(order: i64) -> QueueEntry {
        QueueEntry::new(ContentRef::Story(StoryId::new()), "x", order)
    }

    #[test]
    fn a_place_falls_between_its_neighbours() {
        let (a, b, c) = (entry(0), entry(1024), entry(2048));
        let others = [&a, &b, &c];
        assert_eq!(place_at(&others, QueuePlace::End), 3072);
        assert_eq!(place_at(&others, QueuePlace::Next), -1024);
        assert_eq!(place_at(&others, QueuePlace::Before(b.id)), 512);
        assert_eq!(place_at(&others, QueuePlace::Before(a.id)), -1024);
        assert_eq!(place_at(&others, QueuePlace::Before(QueueEntryId::new())), 3072);
        assert_eq!(place_at(&[], QueuePlace::Next), 0);
    }

    #[test]
    fn settings_are_tidied_and_bad_criteria_refused() {
        let mut s = FeedSettings::default();
        s.ranking.criteria = vec![
            Criterion { id: "a".into(), text: "  AI  ".into(), weight: 9 },
            Criterion { id: "b".into(), text: "   ".into(), weight: 1 },
        ];
        s.autofill = Some(0);
        let tidy = tidy_settings(&s).unwrap();
        assert_eq!(tidy.ranking.criteria.len(), 1);
        assert_eq!(tidy.ranking.criteria[0].text, "AI");
        assert_eq!(tidy.ranking.criteria[0].weight, 3);
        assert_eq!(tidy.autofill, None);
        s.ranking.criteria.push(Criterion { id: "a".into(), text: "again".into(), weight: 1 });
        assert!(tidy_settings(&s).is_err());
    }
}
