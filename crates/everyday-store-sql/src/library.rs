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

use everyday_core::error::Result;
use everyday_core::id::{ItemId, KindId, LogId};
use everyday_core::library::{Item, ItemStatus, Kind, LogEntry};
use everyday_core::purpose::Purpose;
use everyday_core::store::library::{
    ItemQuery, LibraryStore, LogQuery, item_aad, kind_aad, log_aad,
};

use crate::conn::{SqlExt, ToValue, Value, Where};
use crate::purpose::{RecordKind, forget_purposes};
use crate::record::Record;
use crate::{SqlStore, date_str, to_us, vals};

impl Record for Kind {
    const TABLE: &'static str = "kinds";
    const KIND: &'static str = "kind";
    type Id = KindId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        kind_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("sort_order", self.sort_order.to_value()),
            ("visible", self.visible.to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for Item {
    const TABLE: &'static str = "items";
    const KIND: &'static str = "item";
    type Id = ItemId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        item_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("kind_id", self.kind_id.to_string().to_value()),
            ("status", self.status.as_str().to_value()),
            ("rating", self.rating.map(i64::from).to_value()),
            ("favourite", self.favourite.to_value()),
            ("year", self.year.map(i64::from).to_value()),
            ("started_on", date_str(self.started_on).to_value()),
            ("finished_on", date_str(self.finished_on).to_value()),
            ("sort_order", self.sort_order.to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }

    fn purpose_kind() -> Option<RecordKind> {
        Some(RecordKind::Item)
    }

    fn purpose(&self) -> Option<&Purpose> {
        self.purpose.as_ref()
    }
}

impl Record for LogEntry {
    const TABLE: &'static str = "logs";
    const KIND: &'static str = "log";
    type Id = LogId;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn aad(id: Self::Id) -> Vec<u8> {
        log_aad(id)
    }

    fn columns(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("item_id", self.item_id.to_string().to_value()),
            ("event", self.event.as_str().to_value()),
            ("local_date", self.date.to_string().to_value()),
            ("created_us", to_us(self.created_at).to_value()),
        ]
    }
}

impl LibraryStore for SqlStore {
    // ---- kinds ----------------------------------------------------------

    fn list_kinds(&self) -> Result<Vec<Kind>> {
        let rows = self
            .read()
            .records("SELECT id, data FROM kinds ORDER BY sort_order, created_us", &[])?;
        self.collect(rows, kind_aad)
    }

    fn get_kind(&self, id: KindId) -> Result<Kind> {
        self.get(id)
    }

    fn put_kind(&self, kind: &Kind) -> Result<()> {
        // Note what is *not* in the clear columns: the name, the icon, the
        // field labels. A vault whose database said "Books" and "Films"
        // would be telling somebody what sort of person keeps it.
        self.upsert(kind)
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
        let mut w = Where::new();
        if let Some(kind) = query.kind_id {
            w = w.eq("kind_id", kind.to_string());
        }
        if !query.statuses.is_empty() {
            w = w.in_list("status", query.statuses.iter().map(|s| s.as_str()));
        }
        if let Some(favourite) = query.favourite {
            w = w.eq("favourite", favourite);
        }
        if let Some(floor) = query.rating_at_least {
            // `rating IS NOT NULL` is the whole point: an unrated item is
            // not a zero-rated one, and SQL would drop it here anyway --
            // being explicit keeps this agreeing with `ItemQuery::matches`
            // where somebody can read both at once.
            w = w.not_null("rating").gte("rating", i64::from(floor));
        }
        if let Some(from) = query.finished_from {
            w = w.not_null("finished_on").gte("finished_on", from.to_string());
        }
        if let Some(to) = query.finished_to {
            w = w.not_null("finished_on").lte("finished_on", to.to_string());
        }

        // See the module docs for why the ordering is not pushed down.
        let (where_sql, args) = w.finish();
        let sql = format!("SELECT id, data FROM items WHERE {where_sql}");
        let rows = self.read().records(&sql, &args)?;
        let items: Vec<Item> = self.collect(rows, item_aad)?;
        Ok(query.apply(items))
    }

    fn get_item(&self, id: ItemId) -> Result<Item> {
        self.get(id)
    }

    fn put_item(&self, item: &Item) -> Result<()> {
        self.put_items(std::slice::from_ref(item))
    }

    fn put_items(&self, items: &[Item]) -> Result<()> {
        self.upsert_many(items)
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
        let mut w = Where::new();
        if let Some(item) = query.item_id {
            w = w.eq("item_id", item.to_string());
        }
        if let Some(from) = query.from {
            w = w.gte("local_date", from.to_string());
        }
        if let Some(to) = query.to {
            w = w.lte("local_date", to.to_string());
        }
        if !query.events.is_empty() {
            w = w.in_list("event", query.events.iter().map(|e| e.as_str()));
        }
        let (where_sql, args) = w.finish();
        let mut sql = format!("SELECT id, data FROM logs WHERE {where_sql}");
        // Newest first, which is how a history reads. `created_us` breaks
        // ties within a day so two rows written on one afternoon keep the
        // order they were written in.
        self.page(&mut sql, "local_date DESC, created_us DESC", query.limit, 0);

        let rows = self.read().records(&sql, &args)?;
        self.collect(rows, log_aad)
    }

    fn get_log(&self, id: LogId) -> Result<LogEntry> {
        self.get(id)
    }

    fn put_log(&self, log: &LogEntry) -> Result<()> {
        // The note is sealed; the date and the event are not, because they
        // are what the year-in-review query scans. The database therefore
        // says that something was finished on 2 April and never what.
        self.upsert(log)
    }

    fn delete_log(&self, id: LogId) -> Result<()> {
        self.delete_by_id::<LogEntry>(id)?;
        Ok(())
    }
}
