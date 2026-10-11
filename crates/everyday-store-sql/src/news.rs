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

use everyday_core::error::{Error, Result};
use everyday_core::id::{BlobId, MailMessageId, NewsSourceId, StoryId};
use everyday_core::news::{
    ExpireReport, ExpiryRule, FetchState, NewsSource, Retention, SourceCount, Story, StoryBody,
    TOMBSTONE_DAYS,
};
use everyday_core::record::RecordKind;
use everyday_core::store::news::{
    NewsStore, StoryCursor, StoryPage, StoryQuery, fetch_state_aad, source_aad, story_aad,
    story_body_aad,
};
use jiff::Timestamp;
use std::collections::HashMap;
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
    /// return how many. Skips saved stories, and stories already expired.
    ///
    /// Selected and rewritten on the writer inside one transaction, so a
    /// story saved a moment ago cannot be selected as unsaved and then
    /// expired anyway.
    fn tombstone_source(&self, source: NewsSourceId, keep: Retention, now_us: i64) -> Result<u32> {
        let read_before = now_us.saturating_sub(i64::from(keep.read_days).saturating_mul(DAY_US));
        let unread_before =
            now_us.saturating_sub(i64::from(keep.unread_days).saturating_mul(DAY_US));

        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let rows = tx.query(
            &format!(
                "SELECT s.id, s.dedupe_key, s.published_us, s.created_us FROM {STORY_FROM}
                 WHERE s.source_id = ?1 AND s.expired = ?2 AND s.saved = ?3
                   AND ((({READ}) = 1 AND s.published_us < ?4)
                     OR (({READ}) = 0 AND s.published_us < ?5))"
            ),
            &vals![source.to_string(), false, false, read_before, unread_before],
        )?;
        for row in &rows {
            let id: StoryId = id_of(&row.text(0)?)?;
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
        }
        tx.commit()?;
        Ok(u32::try_from(rows.len()).unwrap_or(u32::MAX))
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
        // The same reasoning as `delete_kind`: four indexed statements do not
        // depend on a foreign-key pragma staying switched on, and the bodies
        // have to go through the stories to be reached at all -- a body row
        // carries no source of its own.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute(
            "DELETE FROM news_bodies
             WHERE story_id IN (SELECT id FROM news_stories WHERE source_id = ?1)",
            &vals![id.to_string()],
        )?;
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
        // Nor is `has_body` taken from the caller. A refresh updating a
        // story's points holds a copy read before an extraction may have
        // stored a body, and resealing that copy as it stands would say the
        // body is not there. It is answered by the body row itself, inside
        // the same transaction as the write.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let has_body = !tx
            .query("SELECT 1 FROM news_bodies WHERE story_id = ?1", &vals![story.id.to_string()])?
            .is_empty();
        let mut story = story.clone();
        story.has_body = has_body;
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
        sealed.map(|sealed| self.unseal(&story_body_aad(story), &sealed)).transpose()
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
        let rules: HashMap<NewsSourceId, Retention> =
            rules.iter().map(|r| (r.source_id, r.retention)).collect();
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
            report.expired += self.tombstone_source(source, keep, now_us)?;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use everyday_core::mail::MessageFlags;

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
