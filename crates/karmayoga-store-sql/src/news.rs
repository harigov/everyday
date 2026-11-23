//! The Feed: sources, what the refresher learned about each, the stories they
//! brought, and those stories' bodies.
//!
//! What stays in the clear is only what an index is built from: which source
//! a story belongs to, its dedupe key, when it was published, whether it is
//! read, saved or expired, and which mail message a newsletter story is. A
//! source keeps its sort order and dates in the clear, and its fetch state
//! the time it is next due. Everything a person would recognise is sealed:
//! titles, addresses, authors, summaries, folder names, mute words, the
//! feed's own address, what the refresher last heard, and every body.
//!
//! The dedupe key is a *keyed* hash (see `Vault::news_key`), so a database
//! holder cannot check it against a list of known addresses to learn what
//! somebody reads. What they learn is that twenty-three sources are followed
//! and forty stories were read on Tuesday.
//!
//! # Where the filtering happens
//!
//! All of it in SQL, unlike the library, whose orderings need the payload.
//! Here every filter (source, unread, saved, since) and the order itself
//! (`published_us`, then `id`) are clear columns, so a page is an index scan
//! that decrypts exactly the stories it returns -- and never the extra row
//! fetched to learn whether there is a next page.
//!
//! # Why the columns win
//!
//! `is_read`, `saved` and `expired` are authoritative, and every read of a
//! story overwrites the sealed copy's three fields from them. That is what
//! lets marking a thousand stories read be one `UPDATE` that opens nothing:
//! the alternative, rewriting each sealed record, would decrypt and reseal a
//! thousand rows to flip a bit, and would leave two copies that could
//! disagree. The sealed copy is written once, at insert, and
//! [`update_story`](NewsStore::update_story) rewrites it without ever
//! touching the columns, so a refresh cannot undo a tap.
//!
//! # A newsletter's read state
//!
//! A story that points at a mail message has no read state of its own: it is
//! the `SEEN` bit of that message's `flags`, read through a `LEFT JOIN`, so
//! marking the message read in Mail marks the story read here with nothing
//! to keep in step. If the message is gone from Mail the join finds nothing,
//! and the story is neither listed nor counted. The join is written once,
//! as [`READ`], and every query that cares about read state uses it.
//!
//! # Expiry
//!
//! A tombstone is built from the clear columns alone -- id, source, key,
//! date -- so turning ten thousand old stories into tombstones decrypts none
//! of them. Its `created_us` is the story's `fetched_at`: when it first
//! arrived, which is what [`story_by_key`](NewsStore::story_by_key) means by
//! "the first one's row".
//!
//! # A story's files
//!
//! `news_story_files` is a clear side table kept in step with the sealed
//! [`Story::files`]: a story with files is never expired, and the references
//! table is rebuilt from the stories that have them, so both ask a question
//! an index answers without decrypting every story to find out. The files are
//! written only by [`set_story_files`](NewsStore::set_story_files), in one
//! transaction that reads the stored copy, replaces the files and reseals it
//! -- and [`update_story`](NewsStore::update_story) copies the stored files
//! over whatever it was handed, which is what stops a refresh that read a
//! story before a download finished from unlinking the episode.
//!
//! # Up Next, what was consumed, and ranks
//!
//! Three small tables on one pattern: a clear `target` -- [`ContentRef::key`],
//! an id and nothing else -- that makes "one per thing" a `UNIQUE` index
//! rather than a rule to remember, the one or two columns an `ORDER BY` or a
//! filter needs (`sort_order` and `added_us`; `last_us` and `finished`;
//! `ranked_us`), and the record sealed. A queue entry and a consumption are
//! sealed under their own ids and a rank under its target, so a row cannot be
//! passed off as another. Putting one for a thing that already has a row
//! under another id deletes that row first, in the same transaction.
//!
//! Deleting a source takes its stories' queue entries and ranks with it, and
//! never a consumption: what was read is remembered after the source is gone.

use jiff::Timestamp;
use karmayoga_core::error::{Error, Result};
use karmayoga_core::id::{
    BlobId, ConsumptionId, MailMessageId, NewsSourceId, QueueEntryId, StoryId,
};
use karmayoga_core::news::{
    ConsumedQuery, Consumption, ContentRef, ExpireReport, ExpiryRule, FeedSettings, FetchState,
    NewsSource, QueueEntry, Rank, Retention, SourceCount, Story, StoryBody, StoryFiles,
    TOMBSTONE_DAYS,
};
use karmayoga_core::record::RecordKind;
use karmayoga_core::store::news::{
    NewsStore, StoryCursor, StoryPage, StoryQuery, consumption_aad, feed_settings_aad,
    fetch_state_aad, queue_entry_aad, rank_aad, source_aad, story_aad, story_body_aad,
};
use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::str::FromStr;

use crate::conn::{Row, Sql, SqlExt, ToValue, Value, Where};
use crate::keyset::{Dir, KeyCursor};
use crate::record::Record;
use crate::{SqlStore, from_us, placeholders, to_us, vals};

impl Record for NewsSource {
    const TABLE: &'static str = "news_sources";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // The title, the address, the folder and the icon are not here. A
        // database that said "Hacker News" and "Lobsters" would be telling
        // somebody what sort of person keeps it; this says how many sources
        // there are and when each was added.
        vec![
            ("sort_order", self.sort_order.to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

/// How many ids go into one `IN (...)`. SQLite refuses a statement with more
/// than 32,766 bound parameters and Postgres more than 65,535; marking a
/// whole backlog read can name more than either. `mail/write.rs` keeps the
/// same number for the same reason.
const IN_CHUNK: usize = 500;

const DAY_US: i64 = 86_400_000_000;

/// A story and the message it points at, if it points at one. The alias `s`
/// is the story and `m` the message, in every query below.
const STORY_FROM: &str = "news_stories s LEFT JOIN mail_messages m ON m.id = s.mail_message_id";

/// Whether a story counts as read, as `1` or `0`: its own column, or for a
/// newsletter the `SEEN` bit (`1 << 0`, see `MessageFlags::bits`) of its
/// message's flags. An integer rather than a comparison so that SQLite,
/// which answers a comparison with an integer, and Postgres, which answers
/// with a boolean, hand back something the same accessor reads.
///
/// A message that is gone has no row to join, so `m.flags` is `NULL` and the
/// story reads as unread -- which does not matter, because [`SHOWN`] keeps
/// it out of every list and count.
const READ: &str = "CASE WHEN s.mail_message_id IS NULL \
                    THEN CASE WHEN s.is_read THEN 1 ELSE 0 END \
                    WHEN (COALESCE(m.flags, 0) & 1) = 1 THEN 1 ELSE 0 END";

/// Not a newsletter story whose message has been deleted from Mail.
const SHOWN: &str = "(s.mail_message_id IS NULL OR m.id IS NOT NULL)";

/// `SELECT` a story the way [`SqlStore::story_of`] reads it: `id`, `data`,
/// `saved`, `expired`, the effective read state, then `published_us`.
fn story_select() -> String {
    format!("SELECT s.id, s.data, s.saved, s.expired, {READ}, s.published_us FROM {STORY_FROM}")
}

fn id_of<T: FromStr>(text: &str) -> Result<T>
where
    T::Err: Display,
{
    text.parse().map_err(|e: T::Err| Error::Invalid(e.to_string()))
}

impl SqlStore {
    /// A story from a row of [`story_select`]'s shape, with the three fields
    /// the columns own filled in from the columns.
    fn story_of(&self, row: &Row) -> Result<Story> {
        let id: StoryId = id_of(&row.text(0)?)?;
        let mut story: Story = self.unseal(&story_aad(id), &row.bytes(1)?)?;
        story.saved = row.bool(2)?;
        story.expired = row.bool(3)?;
        story.read = row.bool(4)?;
        Ok(story)
    }

    /// `UPDATE news_stories SET <column> = <on>` over these ids, in one
    /// transaction so a long list is all or nothing. Ids that are not there
    /// are ignored, as the trait promises.
    fn set_flag(&self, column: &str, ids: &[StoryId], on: bool) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        flag_each(tx.as_mut(), column, &ids, on)?;
        tx.commit()
    }

    /// Turn one source's stories older than `keep` into tombstones and
    /// return how many. Skips saved stories, stories already expired, stories
    /// with files, and any story in `spare` (what is in Up Next).
    ///
    /// Selected and rewritten on the writer inside one transaction, so a
    /// story saved a moment ago cannot be selected as unsaved and then
    /// expired anyway -- nor one a download has just linked a file to.
    fn tombstone_source(
        &self,
        source: NewsSourceId,
        keep: Retention,
        spare: &HashSet<String>,
        now_us: i64,
    ) -> Result<u32> {
        let read_before = now_us.saturating_sub(i64::from(keep.read_days).saturating_mul(DAY_US));
        let unread_before =
            now_us.saturating_sub(i64::from(keep.unread_days).saturating_mul(DAY_US));

        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let rows = tx.query(
            &format!(
                "SELECT s.id, s.dedupe_key, s.published_us, s.created_us FROM {STORY_FROM}
                 WHERE s.source_id = ?1 AND s.expired = ?2 AND s.saved = ?3
                   AND s.id NOT IN (SELECT story_id FROM news_story_files WHERE media = ?6)
                   AND ((({READ}) = 1 AND s.published_us < ?4)
                     OR (({READ}) = 0 AND s.published_us < ?5))"
            ),
            &vals![source.to_string(), false, false, read_before, unread_before, true],
        )?;
        let mut expired = 0u32;
        for row in &rows {
            let id: StoryId = id_of(&row.text(0)?)?;
            if spare.contains(&id.to_string()) {
                continue;
            }
            let mut tombstone = Story::new(source, row.text(1)?, "", from_us(row.i64(2)?));
            tombstone.id = id;
            tombstone.fetched_at = from_us(row.i64(3)?);
            tombstone.expired = true;
            let data = self.seal(&story_aad(id), &tombstone)?;
            tx.execute(
                "UPDATE news_stories SET data = ?2, expired = ?3 WHERE id = ?1",
                &vals![id.to_string(), data, true],
            )?;
            tx.execute("DELETE FROM news_bodies WHERE story_id = ?1", &vals![id.to_string()])?;
            expired += 1;
        }
        tx.commit()?;
        Ok(expired)
    }

    /// `DELETE FROM <table> WHERE <column> IN (...)` over these values, a
    /// chunk at a time in one transaction. Values that match nothing are
    /// ignored, as the trait promises. `table` and `column` are this file's
    /// own words, never a caller's.
    fn delete_each(&self, table: &str, column: &str, values: &[String]) -> Result<()> {
        if values.is_empty() {
            return Ok(());
        }
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        for chunk in values.chunks(IN_CHUNK) {
            let args: Vec<Value> = chunk.iter().cloned().map(Value::Text).collect();
            tx.execute(
                &format!(
                    "DELETE FROM {table} WHERE {column} IN ({})",
                    placeholders(1, chunk.len())
                ),
                &args,
            )?;
        }
        tx.commit()
    }
}

/// `UPDATE news_stories SET <column> = <on> WHERE id IN (...)`, a chunk at a
/// time, inside the caller's transaction.
fn flag_each(tx: &mut dyn Sql, column: &str, ids: &[String], on: bool) -> Result<()> {
    for chunk in ids.chunks(IN_CHUNK) {
        let mut args = vec![Value::Bool(on)];
        args.extend(chunk.iter().cloned().map(Value::Text));
        tx.execute(
            &format!(
                "UPDATE news_stories SET {column} = ?1 WHERE id IN ({})",
                placeholders(2, chunk.len())
            ),
            &args,
        )?;
    }
    Ok(())
}

impl NewsStore for SqlStore {
    // ---- sources --------------------------------------------------------

    fn list_sources(&self) -> Result<Vec<NewsSource>> {
        let rows = self
            .read()
            .records("SELECT id, data FROM news_sources ORDER BY sort_order, created_us", &[])?;
        self.collect(rows, source_aad)
    }

    fn get_source(&self, id: NewsSourceId) -> Result<NewsSource> {
        self.get(id)
    }

    fn put_source(&self, source: &NewsSource) -> Result<()> {
        self.upsert(source)
    }

    fn delete_source(&self, id: NewsSourceId) -> Result<()> {
        // The same reasoning as `delete_kind`: indexed statements do not
        // depend on a foreign-key pragma staying switched on, and the bodies,
        // queue entries and ranks have to go through the stories to be
        // reached at all -- none of them carries a source of its own. What
        // was consumed is left: it carries its own title and address, and
        // outlives the source it came from.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute(
            "DELETE FROM news_bodies
             WHERE story_id IN (SELECT id FROM news_stories WHERE source_id = ?1)",
            &vals![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM news_story_files
             WHERE story_id IN (SELECT id FROM news_stories WHERE source_id = ?1)",
            &vals![id.to_string()],
        )?;
        for table in ["feed_queue", "feed_ranks"] {
            tx.execute(
                &format!(
                    "DELETE FROM {table} WHERE target IN
                        (SELECT 'story:' || id FROM news_stories WHERE source_id = ?1)"
                ),
                &vals![id.to_string()],
            )?;
        }
        tx.execute("DELETE FROM news_stories WHERE source_id = ?1", &vals![id.to_string()])?;
        tx.execute("DELETE FROM news_fetch WHERE source_id = ?1", &vals![id.to_string()])?;
        tx.execute("DELETE FROM news_sources WHERE id = ?1", &vals![id.to_string()])?;
        tx.commit()
    }

    // ---- what the refresher learned -------------------------------------

    fn fetch_state(&self, source: NewsSourceId) -> Result<Option<FetchState>> {
        let sealed = self.read().sealed(
            "SELECT data FROM news_fetch WHERE source_id = ?1",
            &vals![source.to_string()],
        )?;
        sealed.map(|sealed| self.unseal(&fetch_state_aad(source), &sealed)).transpose()
    }

    fn put_fetch_state(&self, state: &FetchState) -> Result<()> {
        let data = self.seal(&fetch_state_aad(state.source_id), state)?;
        self.write().execute(
            "INSERT INTO news_fetch (source_id, next_us, data) VALUES (?1, ?2, ?3)
             ON CONFLICT (source_id) DO UPDATE SET next_us = ?2, data = ?3",
            &vals![state.source_id.to_string(), to_us(state.next_at), data],
        )?;
        Ok(())
    }

    fn due_sources(&self, now: Timestamp) -> Result<Vec<NewsSourceId>> {
        // A source nobody has fetched has no row to order by, so it is given
        // a group of its own ahead of the rest: new, and wanted at once.
        let rows = self.read().query(
            "SELECT s.id FROM news_sources s LEFT JOIN news_fetch f ON f.source_id = s.id
             WHERE f.source_id IS NULL OR f.next_us <= ?1
             ORDER BY CASE WHEN f.source_id IS NULL THEN 0 ELSE 1 END,
                      f.next_us, s.created_us, s.id",
            &vals![to_us(now)],
        )?;
        rows.iter().map(|r| id_of(&r.text(0)?)).collect()
    }

    fn next_due(&self) -> Result<Option<Timestamp>> {
        // Joined to the sources so that fetch state left behind for a source
        // deleted mid-refresh cannot hold the refresher awake for a time
        // nothing will ever be due at.
        let row = self.read().query_opt(
            "SELECT MIN(f.next_us) FROM news_fetch f JOIN news_sources s ON s.id = f.source_id",
            &[],
        )?;
        Ok(row.map(|r| r.opt_i64(0)).transpose()?.flatten().map(from_us))
    }

    // ---- stories --------------------------------------------------------

    fn insert_stories(&self, stories: &[(Story, Option<StoryBody>)]) -> Result<Vec<StoryId>> {
        if stories.is_empty() {
            return Ok(Vec::new());
        }
        // Sealed before the writer is taken: encryption is the expensive part
        // of a refresh that brings two hundred stories, and nothing else
        // needs to wait behind it. The sealed copy says whether a body
        // exists because the body is about to be stored, not because the
        // caller remembered to say so.
        let mut sealed = Vec::with_capacity(stories.len());
        for (story, body) in stories {
            if let Some(body) = body
                && body.story_id != story.id
            {
                return Err(Error::Invalid("a story's body names a different story".into()));
            }
            let mut stored = story.clone();
            stored.has_body = body.is_some();
            let data = self.seal(&story_aad(story.id), &stored)?;
            let body =
                body.as_ref().map(|b| self.seal(&story_body_aad(story.id), b)).transpose()?;
            sealed.push((story, data, body));
        }

        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let mut inserted = Vec::new();
        for (story, data, body) in sealed {
            // `DO NOTHING` turns "already stored under this (source, key)" --
            // live or a tombstone -- into zero rows rather than an error, and
            // the count is how we know whether this one landed.
            let landed = tx.execute(
                "INSERT INTO news_stories
                    (id, source_id, dedupe_key, published_us, is_read, saved, expired,
                     mail_message_id, created_us, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT (source_id, dedupe_key) DO NOTHING",
                &vals![
                    story.id.to_string(),
                    story.source_id.to_string(),
                    story.key,
                    to_us(story.published_at),
                    story.read,
                    story.saved,
                    story.expired,
                    story.mail_message.map(|m| m.to_string()),
                    to_us(story.fetched_at),
                    data,
                ],
            )?;
            if landed == 0 {
                continue;
            }
            if !story.files.is_empty() {
                tx.execute(
                    "INSERT INTO news_story_files (story_id, media) VALUES (?1, ?2)
                     ON CONFLICT (story_id) DO UPDATE SET media = ?2",
                    &vals![story.id.to_string(), story.files.media.is_some()],
                )?;
            }
            if let Some(body) = body {
                tx.execute(
                    "INSERT INTO news_bodies (story_id, data) VALUES (?1, ?2)
                     ON CONFLICT (story_id) DO UPDATE SET data = ?2",
                    &vals![story.id.to_string(), body],
                )?;
            }
            inserted.push(story.id);
        }
        tx.commit()?;
        Ok(inserted)
    }

    fn story_by_key(&self, key: &str) -> Result<Option<Story>> {
        let row = self.read().query_opt(
            &format!(
                "{} WHERE s.dedupe_key = ?1 AND s.expired = ?2 AND {SHOWN}
                 ORDER BY s.created_us, s.id LIMIT 1",
                story_select()
            ),
            &vals![key, false],
        )?;
        row.map(|row| self.story_of(&row)).transpose()
    }

    fn keys_for_source(&self, source: NewsSourceId) -> Result<Vec<String>> {
        self.read()
            .query(
                "SELECT dedupe_key FROM news_stories WHERE source_id = ?1",
                &vals![source.to_string()],
            )?
            .iter()
            .map(|r| r.text(0))
            .collect()
    }

    fn update_story(&self, story: &Story) -> Result<()> {
        // Only `data`. The three columns are not in this statement at all,
        // which is how "never touches read, saved or expired" is kept rather
        // than merely intended. A tombstone is left alone too: rewriting it
        // would put a title back into a row whose whole point is to hold
        // none.
        //
        // Nor are `has_body` and `files` taken from the caller. A refresh
        // updating a story's points holds a copy read before an extraction
        // may have stored a body or a download linked an episode, and
        // resealing that copy as it stands would say neither is there. Both
        // are answered by what is stored, read inside the same transaction as
        // the write: the body by its row, the files by the stored copy's own.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let has_body = !tx
            .query("SELECT 1 FROM news_bodies WHERE story_id = ?1", &vals![story.id.to_string()])?
            .is_empty();
        let mut story = story.clone();
        story.has_body = has_body;
        // The stored copy is read for its files only when the index says it
        // has any: a Hacker News refresh rewrites hundreds of stories, almost
        // none of which link a file, and each would otherwise pay a sealed
        // read and a decrypt under the write lock for nothing.
        let linked = !tx
            .query(
                "SELECT 1 FROM news_story_files WHERE story_id = ?1",
                &vals![story.id.to_string()],
            )?
            .is_empty();
        story.files = if linked {
            let stored = tx
                .sealed(
                    "SELECT data FROM news_stories WHERE id = ?1 AND expired = ?2",
                    &vals![story.id.to_string(), false],
                )?
                .ok_or_else(|| Error::not_found(RecordKind::Story.not_found_noun(), story.id))?;
            let stored: Story = self.unseal(&story_aad(story.id), &stored)?;
            stored.files
        } else {
            StoryFiles::default()
        };
        let data = self.seal(&story_aad(story.id), &story)?;
        let changed = tx.execute(
            "UPDATE news_stories SET data = ?2 WHERE id = ?1 AND expired = ?3",
            &vals![story.id.to_string(), data, false],
        )?;
        if changed == 0 {
            return Err(Error::not_found(RecordKind::Story.not_found_noun(), story.id));
        }
        tx.commit()
    }

    fn move_stories(&self, moves: &[(StoryId, NewsSourceId)]) -> Result<u32> {
        if moves.is_empty() {
            return Ok(0);
        }
        // Each story is read on the writer and resealed in the same
        // transaction, for `put_body`'s reason: a copy read on a pooled
        // reader could be a write behind, and sealing it again would revert
        // whatever that write was. The clear `source_id` and the sealed one
        // change together, which is the whole point -- every reader trusts
        // the sealed copy for the rest of the story and the column for
        // which list it is in.
        //
        // What stays where it is: a tombstone, whose key is what keeps its
        // article from coming back to the source that expired it; a story
        // already at its destination; and one whose key the destination
        // already has, which the unique index would refuse -- the same
        // article reached it another way.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let mut moved = 0u32;
        for (id, to) in moves {
            let Some(row) = tx.query_opt(
                "SELECT source_id, dedupe_key, expired, data FROM news_stories WHERE id = ?1",
                &vals![id.to_string()],
            )?
            else {
                continue;
            };
            if row.bool(2)? || row.text(0)? == to.to_string() {
                continue;
            }
            let taken = !tx
                .query(
                    "SELECT 1 FROM news_stories WHERE source_id = ?1 AND dedupe_key = ?2",
                    &vals![to.to_string(), row.text(1)?],
                )?
                .is_empty();
            if taken {
                continue;
            }
            let mut story: Story = self.unseal(&story_aad(*id), &row.bytes(3)?)?;
            story.source_id = *to;
            tx.execute(
                "UPDATE news_stories SET source_id = ?2, data = ?3 WHERE id = ?1",
                &vals![id.to_string(), to.to_string(), self.seal(&story_aad(*id), &story)?],
            )?;
            moved += 1;
        }
        tx.commit()?;
        Ok(moved)
    }

    fn list_stories(&self, query: &StoryQuery) -> Result<StoryPage> {
        let mut w = Where::new().eq("s.expired", false);
        if !query.sources.is_empty() {
            w = w.in_list("s.source_id", query.sources.iter().map(|s| s.to_string()));
        }
        if query.saved_only {
            w = w.eq("s.saved", true);
        }
        if let Some(since) = query.since {
            w = w.gte("s.published_us", to_us(since));
        }
        let (where_sql, mut args) = w.finish();
        let mut sql = format!("{} WHERE {where_sql} AND {SHOWN}", story_select());
        if query.unread_only {
            sql.push_str(&format!(" AND ({READ}) = 0"));
        }

        // One more than the page, to learn whether there is another without
        // asking for it. The extra row is never decrypted.
        let limit = query.limit() as usize;
        let cursor = query
            .before
            .map(|c| {
                let at = Value::Int(to_us(c.published_at));
                KeyCursor::new(vec![at, Value::Text(c.id.to_string())])
            })
            .transpose()?;
        self.page_after(
            &mut sql,
            &mut args,
            &[("s.published_us", Dir::Desc), ("s.id", Dir::Desc)],
            cursor.as_ref(),
            query.limit() + 1,
        )?;

        let rows = self.read().query(&sql, &args)?;
        let more = rows.len() > limit;
        let mut stories = Vec::with_capacity(rows.len().min(limit));
        let mut last = None;
        for row in rows.iter().take(limit) {
            let story = self.story_of(row)?;
            // The cursor is built from the column, not from the story: the
            // column is whole microseconds and the sealed time may not be.
            last = Some(StoryCursor { published_at: from_us(row.i64(5)?), id: story.id });
            stories.push(story);
        }
        Ok(StoryPage { stories, next: if more { last } else { None } })
    }

    fn get_story(&self, id: StoryId) -> Result<Story> {
        let row = self
            .read()
            .query_opt(&format!("{} WHERE s.id = ?1", story_select()), &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found(RecordKind::Story.not_found_noun(), id))?;
        self.story_of(&row)
    }

    fn get_stories(&self, ids: &[StoryId]) -> Result<Vec<Story>> {
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(IN_CHUNK) {
            let args: Vec<Value> = chunk.iter().map(|id| Value::Text(id.to_string())).collect();
            let rows = self.read().query(
                &format!("{} WHERE s.id IN ({})", story_select(), placeholders(1, chunk.len())),
                &args,
            )?;
            for row in &rows {
                out.push(self.story_of(row)?);
            }
        }
        Ok(out)
    }

    fn stories_for_mail(
        &self,
        messages: &[MailMessageId],
    ) -> Result<Vec<(MailMessageId, StoryId)>> {
        let mut out = Vec::new();
        for chunk in messages.chunks(IN_CHUNK) {
            let mut args = vec![Value::Bool(false)];
            args.extend(chunk.iter().map(|m| Value::Text(m.to_string())));
            let rows = self.read().query(
                &format!(
                    "SELECT mail_message_id, id FROM news_stories
                     WHERE expired = ?1 AND mail_message_id IN ({})",
                    placeholders(2, chunk.len())
                ),
                &args,
            )?;
            for row in &rows {
                out.push((id_of(&row.text(0)?)?, id_of(&row.text(1)?)?));
            }
        }
        Ok(out)
    }

    // ---- bodies ---------------------------------------------------------

    fn get_body(&self, story: StoryId) -> Result<Option<StoryBody>> {
        let sealed = self.read().sealed(
            "SELECT data FROM news_bodies WHERE story_id = ?1",
            &vals![story.to_string()],
        )?;
        let body: Option<StoryBody> =
            sealed.map(|sealed| self.unseal(&story_body_aad(story), &sealed)).transpose()?;
        // Stored before the rename, it names the old scheme; every reader
        // gets the current one. See `karmayoga_core::legacy::current_scheme`.
        Ok(body.map(|mut body| {
            karmayoga_core::legacy::move_to_current_scheme(&mut body.html);
            body
        }))
    }

    fn put_body(&self, body: &StoryBody) -> Result<()> {
        let id = body.story_id;
        let sealed_body = self.seal(&story_body_aad(id), body)?;

        // The story's sealed `has_body` is rewritten in the same transaction
        // as the body row, and read on the writer: a pooled reader could be
        // a write behind, and resealing what it saw would revert it.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let sealed = tx
            .sealed(
                "SELECT data FROM news_stories WHERE id = ?1 AND expired = ?2",
                &vals![id.to_string(), false],
            )?
            .ok_or_else(|| Error::not_found(RecordKind::Story.not_found_noun(), id))?;
        let mut story: Story = self.unseal(&story_aad(id), &sealed)?;
        story.has_body = true;
        tx.execute(
            "UPDATE news_stories SET data = ?2 WHERE id = ?1",
            &vals![id.to_string(), self.seal(&story_aad(id), &story)?],
        )?;
        tx.execute(
            "INSERT INTO news_bodies (story_id, data) VALUES (?1, ?2)
             ON CONFLICT (story_id) DO UPDATE SET data = ?2",
            &vals![id.to_string(), sealed_body],
        )?;
        tx.commit()
    }

    // ---- state ----------------------------------------------------------

    fn set_read(&self, ids: &[StoryId], read: bool) -> Result<()> {
        self.set_flag("is_read", ids, read)
    }

    fn mark_all_read(&self, sources: &[NewsSourceId], up_to: Timestamp) -> Result<Vec<StoryId>> {
        // A newsletter story is left out by the `mail_message_id IS NULL`:
        // its read state is its message's, and writing the column would only
        // be a lie nothing reads.
        let (where_sql, args) = Where::new()
            .eq("expired", false)
            .eq("is_read", false)
            .is_null("mail_message_id")
            .lte("published_us", to_us(up_to))
            .in_list("source_id", sources.iter().map(|s| s.to_string()))
            .finish();

        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let ids: Vec<String> = tx
            .query(&format!("SELECT id FROM news_stories WHERE {where_sql}"), &args)?
            .iter()
            .map(|r| r.text(0))
            .collect::<Result<_>>()?;
        flag_each(tx.as_mut(), "is_read", &ids, true)?;
        tx.commit()?;
        ids.iter().map(|id| id_of(id)).collect()
    }

    fn set_saved(&self, ids: &[StoryId], saved: bool) -> Result<()> {
        self.set_flag("saved", ids, saved)
    }

    fn counts(&self) -> Result<Vec<SourceCount>> {
        let rows = self.read().query(
            &format!(
                "SELECT s.source_id, COUNT(*),
                        COALESCE(SUM(CASE WHEN ({READ}) = 0 THEN 1 ELSE 0 END), 0)
                 FROM {STORY_FROM} WHERE s.expired = ?1 AND {SHOWN} GROUP BY s.source_id"
            ),
            &vals![false],
        )?;
        let clamp = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
        rows.iter()
            .map(|r| {
                Ok(SourceCount {
                    source_id: id_of(&r.text(0)?)?,
                    total: clamp(r.u64(1)?),
                    unread: clamp(r.u64(2)?),
                })
            })
            .collect()
    }

    fn expire(&self, rules: &[ExpiryRule], now: Timestamp) -> Result<ExpireReport> {
        self.expire_except(rules, &[], now)
    }

    fn expire_except(
        &self,
        rules: &[ExpiryRule],
        keep: &[StoryId],
        now: Timestamp,
    ) -> Result<ExpireReport> {
        let rules: HashMap<NewsSourceId, Retention> =
            rules.iter().map(|r| (r.source_id, r.retention)).collect();
        let spare: HashSet<String> = keep.iter().map(|id| id.to_string()).collect();
        let now_us = to_us(now);

        // Every source that has a live story, whether or not it came with a
        // rule: one with none gets the default, so the sources to visit are
        // found here and not in `rules`.
        let sources: Vec<NewsSourceId> = self
            .read()
            .query("SELECT DISTINCT source_id FROM news_stories WHERE expired = ?1", &vals![false])?
            .iter()
            .map(|r| id_of(&r.text(0)?))
            .collect::<Result<_>>()?;

        let mut report = ExpireReport::default();
        for source in sources {
            let keep = rules.get(&source).copied().unwrap_or_default();
            report.expired += self.tombstone_source(source, keep, &spare, now_us)?;
        }

        // Then the tombstones that have outlived their purpose. By the date
        // the story was published, which is all a tombstone still knows.
        let cutoff = now_us.saturating_sub(i64::from(TOMBSTONE_DAYS) * DAY_US);
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute(
            "DELETE FROM news_bodies WHERE story_id IN
                (SELECT id FROM news_stories WHERE expired = ?1 AND published_us < ?2)",
            &vals![true, cutoff],
        )?;
        let removed = tx.execute(
            "DELETE FROM news_stories WHERE expired = ?1 AND published_us < ?2",
            &vals![true, cutoff],
        )?;
        tx.commit()?;
        report.removed = u32::try_from(removed).unwrap_or(u32::MAX);
        Ok(report)
    }

    fn blob_refs(&self) -> Result<Vec<BlobId>> {
        let mut refs: Vec<BlobId> =
            self.list_sources()?.into_iter().filter_map(|s| s.icon).collect();
        let rows = self.read().records("SELECT story_id, data FROM news_bodies", &[])?;
        let bodies: Vec<StoryBody> = self.collect(rows, story_body_aad)?;
        refs.extend(bodies.iter().flat_map(|b| b.images.iter().filter_map(|i| i.cached_blob)));
        Ok(refs)
    }

    // ---- a story's files ------------------------------------------------

    fn set_story_files(&self, id: StoryId, files: &StoryFiles) -> Result<()> {
        self.change_story_files(id, &mut |f| *f = files.clone()).map(|_| ())
    }

    fn change_story_files(
        &self,
        id: StoryId,
        change: &mut dyn FnMut(&mut StoryFiles),
    ) -> Result<Story> {
        // Read on the writer, changed and resealed in the same transaction:
        // a copy read on a pooled reader could be a write behind, and
        // resealing it would revert whatever that write was -- a body just
        // stored, a score just refreshed, another file just linked. A
        // tombstone has no files to link.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let sealed = tx
            .sealed(
                "SELECT data FROM news_stories WHERE id = ?1 AND expired = ?2",
                &vals![id.to_string(), false],
            )?
            .ok_or_else(|| Error::not_found(RecordKind::Story.not_found_noun(), id))?;
        let mut story: Story = self.unseal(&story_aad(id), &sealed)?;
        change(&mut story.files);
        tx.execute(
            "UPDATE news_stories SET data = ?2 WHERE id = ?1",
            &vals![id.to_string(), self.seal(&story_aad(id), &story)?],
        )?;
        if story.files.is_empty() {
            tx.execute("DELETE FROM news_story_files WHERE story_id = ?1", &vals![id.to_string()])?;
        } else {
            tx.execute(
                "INSERT INTO news_story_files (story_id, media) VALUES (?1, ?2)
                 ON CONFLICT (story_id) DO UPDATE SET media = ?2",
                &vals![id.to_string(), story.files.media.is_some()],
            )?;
        }
        tx.commit()?;
        Ok(story)
    }

    fn stories_with_files(&self) -> Result<Vec<Story>> {
        // Not narrowed by `SHOWN`: this feeds the references table, and a
        // newsletter story whose message has left Mail still has its files,
        // which must go on being referenced even while it is not listed.
        let rows = self.read().query(
            &format!(
                "{} WHERE s.expired = ?1 AND s.id IN (SELECT story_id FROM news_story_files)
                 ORDER BY s.published_us DESC, s.id DESC",
                story_select()
            ),
            &vals![false],
        )?;
        rows.iter().map(|row| self.story_of(row)).collect()
    }

    fn stories_with_media(&self) -> Result<Vec<Story>> {
        let rows = self.read().query(
            &format!(
                "{} WHERE s.expired = ?1
                   AND s.id IN (SELECT story_id FROM news_story_files WHERE media = ?2)
                 ORDER BY s.published_us DESC, s.id DESC",
                story_select()
            ),
            &vals![false, true],
        )?;
        rows.iter().map(|row| self.story_of(row)).collect()
    }

    // ---- the Feed's settings ----------------------------------------------

    fn feed_settings(&self) -> Result<Option<FeedSettings>> {
        let sealed = self.read().sealed("SELECT data FROM feed_settings WHERE id = 1", &[])?;
        sealed.map(|sealed| self.unseal(&feed_settings_aad(), &sealed)).transpose()
    }

    fn put_feed_settings(&self, settings: &FeedSettings) -> Result<()> {
        let data = self.seal(&feed_settings_aad(), settings)?;
        self.write().execute(
            "INSERT INTO feed_settings (id, data) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET data = ?1",
            &vals![data],
        )?;
        Ok(())
    }

    // ---- Up Next ----------------------------------------------------------

    fn queue(&self) -> Result<Vec<QueueEntry>> {
        let rows = self
            .read()
            .records("SELECT id, data FROM feed_queue ORDER BY sort_order, added_us, id", &[])?;
        self.collect(rows, queue_entry_aad)
    }

    fn put_queue_entry(&self, entry: &QueueEntry) -> Result<()> {
        let data = self.seal(&queue_entry_aad(entry.id), entry)?;
        let target = entry.what.key();

        let mut conn = self.write();
        let mut tx = conn.begin()?;
        // Whatever else is queued for this thing goes first -- `target` is
        // `UNIQUE`, so leaving it would fail the insert, not replace it.
        tx.execute(
            "DELETE FROM feed_queue WHERE target = ?1 AND id <> ?2",
            &vals![target, entry.id.to_string()],
        )?;
        tx.execute(
            "INSERT INTO feed_queue (id, target, sort_order, added_us, data)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET target = ?2, sort_order = ?3, added_us = ?4, data = ?5",
            &vals![entry.id.to_string(), target, entry.sort_order, to_us(entry.added_at), data],
        )?;
        tx.commit()
    }

    fn delete_queue_entries(&self, ids: &[QueueEntryId]) -> Result<()> {
        let ids: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        self.delete_each("feed_queue", "id", &ids)
    }

    fn delete_queue_for(&self, what: &[ContentRef]) -> Result<()> {
        let keys: Vec<String> = what.iter().map(ContentRef::key).collect();
        self.delete_each("feed_queue", "target", &keys)
    }

    // ---- what was read, heard and watched ----------------------------------

    fn consumption(&self, what: &ContentRef) -> Result<Option<Consumption>> {
        Ok(self.consumptions_for(std::slice::from_ref(what))?.into_iter().next())
    }

    fn consumptions_for(&self, what: &[ContentRef]) -> Result<Vec<Consumption>> {
        let mut out = Vec::with_capacity(what.len());
        for chunk in what.chunks(IN_CHUNK) {
            let args: Vec<Value> = chunk.iter().map(|w| Value::Text(w.key())).collect();
            let rows = self.read().records(
                &format!(
                    "SELECT id, data FROM feed_consumed WHERE target IN ({})",
                    placeholders(1, chunk.len())
                ),
                &args,
            )?;
            out.extend(self.collect::<Consumption, ConsumptionId>(rows, consumption_aad)?);
        }
        Ok(out)
    }

    fn consumptions(&self, query: &ConsumedQuery) -> Result<Vec<Consumption>> {
        let mut w = Where::new();
        if let Some(since) = query.since {
            w = w.gte("last_us", to_us(since));
        }
        if let Some(until) = query.until {
            w = w.lt("last_us", to_us(until));
        }
        if query.finished_only {
            w = w.eq("finished", true);
        }
        let (where_sql, args) = w.finish();
        let mut sql = format!("SELECT id, data FROM feed_consumed WHERE {where_sql}");
        self.page(&mut sql, "last_us DESC, id DESC", Some(query.limit()), 0);
        let rows = self.read().records(&sql, &args)?;
        self.collect(rows, consumption_aad)
    }

    fn put_consumption(&self, consumption: &Consumption) -> Result<()> {
        let data = self.seal(&consumption_aad(consumption.id), consumption)?;
        let target = consumption.what.key();

        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute(
            "DELETE FROM feed_consumed WHERE target = ?1 AND id <> ?2",
            &vals![target, consumption.id.to_string()],
        )?;
        tx.execute(
            "INSERT INTO feed_consumed (id, target, last_us, finished, data)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE
                SET target = ?2, last_us = ?3, finished = ?4, data = ?5",
            &vals![
                consumption.id.to_string(),
                target,
                to_us(consumption.last_at),
                consumption.finished,
                data
            ],
        )?;
        tx.commit()
    }

    fn delete_consumption(&self, id: ConsumptionId) -> Result<()> {
        self.write().execute("DELETE FROM feed_consumed WHERE id = ?1", &vals![id.to_string()])?;
        Ok(())
    }

    // ---- ranks --------------------------------------------------------------

    fn ranks(&self, what: &[ContentRef]) -> Result<Vec<Rank>> {
        let mut out = Vec::with_capacity(what.len());
        for chunk in what.chunks(IN_CHUNK) {
            let args: Vec<Value> = chunk.iter().map(|w| Value::Text(w.key())).collect();
            let rows = self.read().records(
                &format!(
                    "SELECT target, data FROM feed_ranks WHERE target IN ({})",
                    placeholders(1, chunk.len())
                ),
                &args,
            )?;
            for (target, sealed) in rows {
                let what = ContentRef::parse_key(&target)
                    .ok_or_else(|| Error::Invalid(format!("a rank is held for {target:?}")))?;
                out.push(self.unseal(&rank_aad(&what), &sealed)?);
            }
        }
        Ok(out)
    }

    fn put_ranks(&self, ranks: &[Rank]) -> Result<()> {
        if ranks.is_empty() {
            return Ok(());
        }
        // Sealed before the writer is taken, as `insert_stories` does: a
        // ranking pass can hand over a few hundred at once.
        let mut sealed = Vec::with_capacity(ranks.len());
        for rank in ranks {
            sealed.push((rank, self.seal(&rank_aad(&rank.what), rank)?));
        }
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        for (rank, data) in sealed {
            tx.execute(
                "INSERT INTO feed_ranks (target, ranked_us, data) VALUES (?1, ?2, ?3)
                 ON CONFLICT (target) DO UPDATE SET ranked_us = ?2, data = ?3",
                &vals![rank.what.key(), to_us(rank.at), data],
            )?;
        }
        tx.commit()
    }

    fn clear_ranks(&self, before: Option<Timestamp>) -> Result<u32> {
        let cleared = match before {
            Some(before) => self
                .write()
                .execute("DELETE FROM feed_ranks WHERE ranked_us < ?1", &vals![to_us(before)])?,
            None => self.write().execute("DELETE FROM feed_ranks", &[])?,
        };
        Ok(u32::try_from(cleared).unwrap_or(u32::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use karmayoga_core::mail::MessageFlags;

    #[test]
    fn the_bit_the_read_expression_tests_is_the_one_mail_writes() {
        // `READ` spells the `SEEN` bit as a literal `1`, the way the mail
        // queries do. If `MessageFlags::bits` ever moves it, this fails and
        // says where to look.
        let seen = MessageFlags { seen: true, ..Default::default() };
        assert_eq!(seen.bits(), 1, "update the `& 1` in READ");
        assert!(READ.contains("& 1)"));
    }
}
