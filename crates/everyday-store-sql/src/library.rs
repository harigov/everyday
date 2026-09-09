//! The library domain: shelves, the things on them, and the log.
//!
//! What stays in the clear here is only what an index is built from — which
//! shelf, what status, your rating, whether you starred it, the year, the
//! finish date, and a log row's date and event. Everything a person would
//! recognise is sealed: titles, creators, summaries, notes, tags, addresses,
//! cover addresses, and the *names of the shelves themselves*.
//!
//! The trade that buys is the same one the other domains make. A shelf query
//! is an index scan over a hundred clear integers instead of a decryption of
//! every item in the vault, and what somebody holding the database learns is
//! that eleven things were finished in March and never what any of them were.
//!
//! # Where the sorting happens
//!
//! Filters that sit on clear columns are pushed into SQL; sorting and
//! pagination are done in Rust by
//! [`ItemQuery::apply`](everyday_core::store::library::ItemQuery::apply).
//! That is not laziness. Half the orderings a shelf offers — by title, and
//! any of them once a text filter is in play — need the payload, which means
//! decrypting the rows anyway; and having *one* implementation of "unrated
//! sorts last, favourites float" rather than one here and one in the core is
//! what keeps two backends from disagreeing about what a shelf looks like.
//! The set being sorted is one shelf, which is hundreds of rows.

use everyday_core::error::{Error, Result};
use everyday_core::id::{ItemId, KindId, LogId};
use everyday_core::library::{Item, ItemStatus, Kind, LogEntry};
use everyday_core::store::library::{
    ItemQuery, LibraryStore, LogQuery, item_aad, kind_aad, log_aad,
};

use crate::conn::{SqlExt, Value};
use crate::purpose::{RecordKind, forget_purposes, set_purpose};
use crate::{SqlStore, date_str, to_us, vals};

impl LibraryStore for SqlStore {
    // ---- kinds ----------------------------------------------------------

    fn list_kinds(&self) -> Result<Vec<Kind>> {
        let rows = self
            .read()
            .records("SELECT id, data FROM kinds ORDER BY sort_order, created_us", &[])?;
        self.collect(rows, kind_aad)
    }

    fn get_kind(&self, id: KindId) -> Result<Kind> {
        let sealed = self
            .read()
            .sealed("SELECT data FROM kinds WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("kind", id))?;
        self.unseal(&kind_aad(id), &sealed)
    }

    fn put_kind(&self, kind: &Kind) -> Result<()> {
        // Note what is *not* in the clear columns: the name, the icon, the
        // field labels. A vault whose database said "Books" and "Films"
        // would be telling somebody what sort of person keeps it.
        let data = self.seal(&kind_aad(kind.id), kind)?;
        self.write().execute(
            "INSERT INTO kinds (id, sort_order, visible, created_us, updated_us, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO UPDATE SET
                sort_order = ?2, visible = ?3, created_us = ?4, updated_us = ?5, data = ?6",
            &vals![
                kind.id.to_string(),
                kind.sort_order,
                kind.visible,
                to_us(kind.created_at),
                to_us(kind.updated_at),
                data,
            ],
        )?;
        Ok(())
    }

    fn delete_kind(&self, id: KindId) -> Result<()> {
        // The cascade would happen by foreign key -- but only if the database
        // is enforcing them, which on SQLite is a connection pragma a future
        // refactor could quietly turn off. Three indexed statements do not
        // depend on a setting staying put, and the log has to go through the
        // items to be reached at all.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        // Collected before the delete, because afterwards there is nothing
        // left to ask which items were on this shelf.
        let doomed: Vec<String> = tx
            .query("SELECT id FROM items WHERE kind_id = ?1", &vals![id.to_string()])?
            .into_iter()
            .map(|r| r.text(0))
            .collect::<Result<_>>()?;
        tx.execute(
            "DELETE FROM logs WHERE item_id IN (SELECT id FROM items WHERE kind_id = ?1)",
            &vals![id.to_string()],
        )?;
        tx.execute("DELETE FROM items WHERE kind_id = ?1", &vals![id.to_string()])?;
        tx.execute("DELETE FROM kinds WHERE id = ?1", &vals![id.to_string()])?;
        forget_purposes(tx.as_mut(), RecordKind::Item, &doomed)?;
        tx.commit()
    }

    // ---- items ----------------------------------------------------------

    fn list_items(&self, query: &ItemQuery) -> Result<Vec<Item>> {
        let mut sql = String::from("SELECT id, data FROM items WHERE 1=1");
        let mut args: Vec<Value> = Vec::new();

        if let Some(kind) = query.kind_id {
            args.push(Value::Text(kind.to_string()));
            sql.push_str(&format!(" AND kind_id = ?{}", args.len()));
        }
        if !query.statuses.is_empty() {
            let holes: Vec<String> = query
                .statuses
                .iter()
                .map(|s| {
                    args.push(Value::Text(s.as_str().to_string()));
                    format!("?{}", args.len())
                })
                .collect();
            sql.push_str(&format!(" AND status IN ({})", holes.join(",")));
        }
        if let Some(favourite) = query.favourite {
            args.push(Value::Bool(favourite));
            sql.push_str(&format!(" AND favourite = ?{}", args.len()));
        }
        if let Some(floor) = query.rating_at_least {
            // `rating IS NOT NULL` is the whole point: an unrated item is
            // not a zero-rated one, and SQL would drop it here anyway --
            // being explicit keeps this agreeing with `ItemQuery::matches`
            // where somebody can read both at once.
            args.push(Value::Int(i64::from(floor)));
            sql.push_str(&format!(" AND rating IS NOT NULL AND rating >= ?{}", args.len()));
        }
        if let Some(from) = query.finished_from {
            args.push(Value::Text(from.to_string()));
            sql.push_str(&format!(
                " AND finished_on IS NOT NULL AND finished_on >= ?{}",
                args.len()
            ));
        }
        if let Some(to) = query.finished_to {
            args.push(Value::Text(to.to_string()));
            sql.push_str(&format!(
                " AND finished_on IS NOT NULL AND finished_on <= ?{}",
                args.len()
            ));
        }

        // See the module docs for why the ordering is not pushed down.
        let rows = self.read().records(&sql, &args)?;
        let items: Vec<Item> = self.collect(rows, item_aad)?;
        Ok(query.apply(items))
    }

    fn get_item(&self, id: ItemId) -> Result<Item> {
        let sealed = self
            .read()
            .sealed("SELECT data FROM items WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("item", id))?;
        self.unseal(&item_aad(id), &sealed)
    }

    fn put_item(&self, item: &Item) -> Result<()> {
        self.put_items(std::slice::from_ref(item))
    }

    fn put_items(&self, items: &[Item]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        // Sealed before the lock is taken: encryption is the expensive part
        // and there is no reason to hold the connection through it.
        let sealed: Vec<(&Item, Vec<u8>)> =
            items.iter().map(|i| Ok((i, self.seal(&item_aad(i.id), i)?))).collect::<Result<_>>()?;

        let mut conn = self.write();
        let mut tx = conn.begin()?;
        for (item, data) in &sealed {
            tx.execute(
                "INSERT INTO items
                    (id, kind_id, status, rating, favourite, year, started_on,
                     finished_on, sort_order, created_us, updated_us, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT (id) DO UPDATE SET
                    kind_id = ?2, status = ?3, rating = ?4, favourite = ?5, year = ?6,
                    started_on = ?7, finished_on = ?8, sort_order = ?9, created_us = ?10,
                    updated_us = ?11, data = ?12",
                &vals![
                    item.id.to_string(),
                    item.kind_id.to_string(),
                    item.status.as_str(),
                    item.rating.map(i64::from),
                    item.favourite,
                    item.year.map(i64::from),
                    date_str(item.started_on),
                    date_str(item.finished_on),
                    item.sort_order,
                    to_us(item.created_at),
                    to_us(item.updated_at),
                    data,
                ],
            )?;
            set_purpose(
                tx.as_mut(),
                RecordKind::Item,
                &item.id.to_string(),
                item.purpose.as_ref(),
            )?;
        }
        tx.commit()
    }

    fn delete_item(&self, id: ItemId) -> Result<()> {
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute("DELETE FROM logs WHERE item_id = ?1", &vals![id.to_string()])?;
        tx.execute("DELETE FROM items WHERE id = ?1", &vals![id.to_string()])?;
        forget_purposes(tx.as_mut(), RecordKind::Item, &[id.to_string()])?;
        tx.commit()
    }

    fn count_items(&self, kind: KindId) -> Result<(u64, u64)> {
        // Both numbers in one statement, off the clear `status` column, so
        // the sidebar's counts decrypt nothing at all.
        let open: Vec<String> = ItemStatus::ALL
            .iter()
            .filter(|s| s.is_open())
            .map(|s| format!("'{}'", s.as_str()))
            .collect();
        let row = self
            .read()
            .query_opt(
                &format!(
                    "SELECT COUNT(*), COALESCE(SUM(CASE WHEN status IN ({}) THEN 1 ELSE 0 END), 0)
                     FROM items WHERE kind_id = ?1",
                    open.join(",")
                ),
                &vals![kind.to_string()],
            )?
            .unwrap_or_default();
        Ok((row.u64(0)?, row.u64(1)?))
    }

    // ---- the log --------------------------------------------------------

    fn list_logs(&self, query: &LogQuery) -> Result<Vec<LogEntry>> {
        let mut sql = String::from("SELECT id, data FROM logs WHERE 1=1");
        let mut args: Vec<Value> = Vec::new();

        if let Some(item) = query.item_id {
            args.push(Value::Text(item.to_string()));
            sql.push_str(&format!(" AND item_id = ?{}", args.len()));
        }
        if let Some(from) = query.from {
            args.push(Value::Text(from.to_string()));
            sql.push_str(&format!(" AND local_date >= ?{}", args.len()));
        }
        if let Some(to) = query.to {
            args.push(Value::Text(to.to_string()));
            sql.push_str(&format!(" AND local_date <= ?{}", args.len()));
        }
        if !query.events.is_empty() {
            let holes: Vec<String> = query
                .events
                .iter()
                .map(|e| {
                    args.push(Value::Text(e.as_str().to_string()));
                    format!("?{}", args.len())
                })
                .collect();
            sql.push_str(&format!(" AND event IN ({})", holes.join(",")));
        }
        // Newest first, which is how a history reads. `created_us` breaks
        // ties within a day so two rows written on one afternoon keep the
        // order they were written in.
        sql.push_str(" ORDER BY local_date DESC, created_us DESC");
        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        let rows = self.read().records(&sql, &args)?;
        self.collect(rows, log_aad)
    }

    fn get_log(&self, id: LogId) -> Result<LogEntry> {
        let sealed = self
            .read()
            .sealed("SELECT data FROM logs WHERE id = ?1", &vals![id.to_string()])?
            .ok_or_else(|| Error::not_found("log", id))?;
        self.unseal(&log_aad(id), &sealed)
    }

    fn put_log(&self, log: &LogEntry) -> Result<()> {
        // The note is sealed; the date and the event are not, because they
        // are what the year-in-review query scans. The database therefore
        // says that something was finished on 2 April and never what.
        let data = self.seal(&log_aad(log.id), log)?;
        self.write().execute(
            "INSERT INTO logs (id, item_id, event, local_date, created_us, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO UPDATE SET
                item_id = ?2, event = ?3, local_date = ?4, created_us = ?5, data = ?6",
            &vals![
                log.id.to_string(),
                log.item_id.to_string(),
                log.event.as_str(),
                log.date.to_string(),
                to_us(log.created_at),
                data,
            ],
        )?;
        Ok(())
    }

    fn delete_log(&self, id: LogId) -> Result<()> {
        self.write().execute("DELETE FROM logs WHERE id = ?1", &vals![id.to_string()])?;
        Ok(())
    }
}
