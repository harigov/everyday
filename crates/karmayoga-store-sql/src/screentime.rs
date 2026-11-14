//! Screen time: spans, the labels a person gives apps and sites, the
//! categories those are filed under, and the settings. See
//! `karmayoga_core::store::screentime` for the rules an implementation keeps
//! and `schema.rs`'s `v25` for the tables.
//!
//! What stays in the clear is only what an index is built from: a span's
//! stream, start and end; a label's two timestamps; a category's order and
//! its two timestamps. Everything a person would recognise -- the device's
//! name, the collector, the app, the host, the window or tab title, an item
//! key (which *is* an app id or a domain), a category's name or colour -- is
//! in the sealed `data` beside them.
//!
//! The stream is a *keyed* hash of the device and the collector, computed by
//! the vault (see `Vault::record_heartbeats`), never here: this module is
//! handed it as an opaque string and keeps it only so that a stream's last
//! span is one indexed lookup. A database holder learns that some machine was
//! in use from nine to five, and how many streams there are -- never which
//! machine, and never what on.
//!
//! # Every row is bound to its own name
//!
//! A span is sealed under [`screen_span_aad`] with its own id, a label and a
//! category under theirs (through [`Record`]), and the settings under their
//! own singleton label, so a payload copied into another row by somebody who
//! can write the database but does not hold the key will not open there.
//!
//! # Spans are not records
//!
//! [`ScreenSpan`] has no [`RecordDescriptor`](karmayoga_core::record::RecordDescriptor):
//! it is written by the thousand a day and never edited, so it has no change
//! history of its own, no purpose and no place in the record vocabulary. Its
//! table still has the shape every table here has -- an id, clear columns,
//! sealed `data` -- so its queries are written out by hand below rather than
//! through [`Record`], which would need it to be a record kind first.

use jiff::Timestamp;
use karmayoga_core::error::{Error, Result};
use karmayoga_core::id::{ScreenAppId, ScreenCategoryId, ScreenSpanId};
use karmayoga_core::screentime::{
    MAX_SPAN_SECS, ScreenApp, ScreenCategory, ScreenSpan, ScreenTimeSettings,
};
use karmayoga_core::store::screentime::{
    ScreenTimeStore, screen_app_aad, screen_category_aad, screen_settings_aad, screen_span_aad,
};

use crate::conn::{SqlExt, ToValue, Value};
use crate::record::Record;
use crate::{SqlStore, from_us, placeholders, to_us, vals};

impl Record for ScreenApp {
    const TABLE: &'static str = "screen_apps";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // Not the key. A key is `app:org.mozilla.firefox` or
        // `site:reddit.com`: the very thing the person labelled, and what
        // they would least want a database holder to read. Not the name,
        // the category, or whether they asked for it to be ignored either.
        vec![
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for ScreenCategory {
    const TABLE: &'static str = "screen_categories";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // Not the name, the colour, or which starter it began as.
        vec![
            ("sort_order", self.sort_order.to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

/// How many ids go into one `IN (...)`. SQLite refuses a statement with more
/// than 32,766 bound parameters and Postgres more than 65,535, and forgetting
/// a site somebody visited every day for a year names more spans than either.
/// `money.rs`, `news.rs` and `mail/write.rs` keep the same number for the
/// same reason.
const IN_CHUNK: usize = 500;

/// The one statement a span is written with: insert, or replace by id.
///
/// Every column is reassigned on conflict, the stream included: a span the
/// vault extends keeps its id and moves its end, and a span re-imported from
/// an archive is written under the import's stream (see the transfer part).
const PUT_SPAN: &str = "INSERT INTO screen_spans (id, stream, start_us, end_us, data)
     VALUES (?1, ?2, ?3, ?4, ?5)
     ON CONFLICT (id) DO UPDATE SET stream = ?2, start_us = ?3, end_us = ?4, data = ?5";

impl ScreenTimeStore for SqlStore {
    // ---- spans ----------------------------------------------------------

    fn put_spans(&self, stream: &str, spans: &[ScreenSpan]) -> Result<()> {
        if spans.is_empty() {
            return Ok(());
        }
        // The bound `spans` leans on: a longer span could start before the
        // window its range read looks back over, and be missed.
        if let Some(long) = spans.iter().find(|s| s.end < s.start || s.secs() > MAX_SPAN_SECS) {
            return Err(Error::Invalid(format!(
                "a span runs backwards or lasts longer than {MAX_SPAN_SECS} seconds ({})",
                long.id
            )));
        }
        // Sealed before the writer is taken, as `put_txns` does: a collector
        // back from a week offline sends thousands of beats, and nothing else
        // needs to wait behind their encryption.
        let mut sealed = Vec::with_capacity(spans.len());
        for span in spans {
            sealed.push((span, self.seal(&screen_span_aad(span.id), span)?));
        }

        // One transaction. An early `?` drops it, which rolls it back: all
        // of the batch or none of it, so a heartbeat batch that half landed
        // -- the extended span written and the new one after it not -- is
        // never what the next batch is joined onto.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        for (span, data) in sealed {
            tx.execute(
                PUT_SPAN,
                &vals![span.id.to_string(), stream, to_us(span.start), to_us(span.end), data],
            )?;
        }
        tx.commit()
    }

    fn last_span(&self, stream: &str) -> Result<Option<ScreenSpan>> {
        // `(stream, end_us)` is an index, so this is one seek however many
        // spans the vault holds. Ties by id, greatest first, which is the
        // order the trait promises: ids are v7 UUIDs, written as fixed-width
        // lower-case hex, so their text sorts as they do.
        let rows = self.read().records(
            "SELECT id, data FROM screen_spans WHERE stream = ?1
             ORDER BY end_us DESC, id DESC LIMIT 1",
            &vals![stream],
        )?;
        let found: Vec<ScreenSpan> = self.collect(rows, screen_span_aad)?;
        Ok(found.into_iter().next())
    }

    fn spans(&self, from: Timestamp, to: Timestamp) -> Result<Vec<ScreenSpan>> {
        // Overlap, both ends open: a span that ended exactly at `from`, or
        // began exactly at `to`, shares no time with the range and is not
        // answered. Days laid end to end therefore never count a span's
        // boundary twice.
        //
        // No span outlasts `MAX_SPAN_SECS` (`put_spans` refuses one that
        // would), so one that overlaps began no earlier than that before
        // `from`. Saying so gives the `start_us` index a lower bound as well
        // as an upper, and a week's report reads a week and a half-day of
        // rows rather than every span ever recorded before it.
        let earliest = from.checked_sub(jiff::SignedDuration::from_secs(MAX_SPAN_SECS));
        let rows = self.read().records(
            "SELECT id, data FROM screen_spans
             WHERE start_us < ?1 AND start_us >= ?3 AND end_us > ?2
             ORDER BY start_us, id",
            &vals![to_us(to), to_us(from), to_us(earliest.unwrap_or(Timestamp::MIN))],
        )?;
        self.collect(rows, screen_span_aad)
    }

    fn delete_spans(&self, ids: &[ScreenSpanId]) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        // One transaction for every chunk, so "forget this site" either
        // forgets all of it or none of it.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        let mut gone = 0u64;
        for chunk in ids.chunks(IN_CHUNK) {
            let args: Vec<Value> = chunk.iter().map(|id| Value::Text(id.to_string())).collect();
            gone += tx.execute(
                &format!("DELETE FROM screen_spans WHERE id IN ({})", placeholders(1, chunk.len())),
                &args,
            )?;
        }
        tx.commit()?;
        Ok(gone)
    }

    fn delete_spans_before(&self, before: Timestamp) -> Result<u64> {
        // By `end_us`, which is indexed: the retention sweep, and a span
        // still running at the cut-off is kept whole rather than trimmed.
        self.write().execute("DELETE FROM screen_spans WHERE end_us < ?1", &vals![to_us(before)])
    }

    fn strip_titles(&self, from: Timestamp, before: Timestamp) -> Result<u64> {
        // A title is sealed, so which spans have one is only known by
        // opening them; `end_us` narrows that to the ones that aged past the
        // cut-off since the last sweep. Rewritten in one transaction, `data`
        // only, sealed again under the same id.
        let rows = self.read().records(
            "SELECT id, data FROM screen_spans WHERE end_us >= ?1 AND end_us < ?2",
            &vals![to_us(from), to_us(before)],
        )?;
        let spans: Vec<ScreenSpan> = self.collect(rows, screen_span_aad)?;
        let mut changed = Vec::new();
        for mut span in spans.into_iter().filter(|s| !s.title.is_empty()) {
            span.title.clear();
            changed.push((span.id.to_string(), self.seal(&screen_span_aad(span.id), &span)?));
        }
        if changed.is_empty() {
            return Ok(0);
        }
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        for (id, data) in &changed {
            tx.execute(
                "UPDATE screen_spans SET data = ?1 WHERE id = ?2",
                &vals![data.clone(), id.clone()],
            )?;
        }
        tx.commit()?;
        Ok(changed.len() as u64)
    }

    fn earliest_span(&self) -> Result<Option<Timestamp>> {
        let row = self.read().query_opt("SELECT MIN(start_us) FROM screen_spans", &[])?;
        Ok(row.map(|r| r.opt_i64(0)).transpose()?.flatten().map(from_us))
    }

    // ---- labels ---------------------------------------------------------

    fn list_apps(&self) -> Result<Vec<ScreenApp>> {
        // In the order they were made. The list a person edits labels in is
        // drawn by the vault from usage, so this order is only ever a
        // stable one, never a shown one.
        let rows =
            self.read().records("SELECT id, data FROM screen_apps ORDER BY created_us, id", &[])?;
        self.collect(rows, screen_app_aad)
    }

    fn get_app(&self, id: ScreenAppId) -> Result<ScreenApp> {
        self.get(id)
    }

    fn put_app(&self, app: &ScreenApp) -> Result<()> {
        self.upsert(app)
    }

    fn delete_app(&self, id: ScreenAppId) -> Result<()> {
        // Only the label. Spans never pointed at it -- they name an app or a
        // host, and a label is matched to them by key at report time -- so
        // the time stays, under the collector's name again.
        self.delete_by_id::<ScreenApp>(id)?;
        Ok(())
    }

    // ---- categories -----------------------------------------------------

    fn list_categories(&self) -> Result<Vec<ScreenCategory>> {
        let rows = self.read().records(
            "SELECT id, data FROM screen_categories ORDER BY sort_order, created_us, id",
            &[],
        )?;
        self.collect(rows, screen_category_aad)
    }

    fn get_category(&self, id: ScreenCategoryId) -> Result<ScreenCategory> {
        self.get(id)
    }

    fn put_category(&self, category: &ScreenCategory) -> Result<()> {
        self.upsert(category)
    }

    fn delete_category(&self, id: ScreenCategoryId) -> Result<()> {
        // Only the category. A label still filed under it is the vault's to
        // tidy, under its own write lock -- the category id is inside each
        // label's sealed payload, which this could only reach by opening
        // every label.
        self.delete_by_id::<ScreenCategory>(id)?;
        Ok(())
    }

    // ---- settings -------------------------------------------------------

    fn screen_settings(&self) -> Result<ScreenTimeSettings> {
        let sealed = self.read().sealed("SELECT data FROM screen_settings WHERE id = 1", &[])?;
        match sealed {
            Some(sealed) => self.unseal(&screen_settings_aad(), &sealed),
            // Never saved. The defaults -- three idle minutes, titles kept,
            // nothing pruned -- are a real answer, as `meeting_settings`'
            // are.
            None => Ok(ScreenTimeSettings::default()),
        }
    }

    fn put_screen_settings(&self, settings: &ScreenTimeSettings) -> Result<()> {
        let data = self.seal(&screen_settings_aad(), settings)?;
        self.write().execute(
            "INSERT INTO screen_settings (id, data) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET data = ?1",
            &vals![data],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::Dialect;
    use crate::schema::steps;
    use karmayoga_core::screentime::SpanKind;

    /// The columns the schema declares for `table`, in order, `id` and
    /// `data` left out -- the list [`Record::columns`] has to match.
    fn declared(table: &str) -> Vec<String> {
        let head = format!("CREATE TABLE IF NOT EXISTS {table} (");
        let ddl = steps(Dialect::Sqlite)
            .concat()
            .into_iter()
            .find(|sql| sql.contains(&head))
            .unwrap_or_else(|| panic!("no CREATE TABLE for {table}"));
        let open = ddl.find('(').expect("an open paren");
        let close = ddl.rfind(')').expect("a close paren");
        let body = &ddl[open + 1..close];
        body.lines()
            .filter_map(|line| line.split_whitespace().next())
            .map(|word| word.trim_end_matches(',').to_string())
            .filter(|word| word != "id" && word != "data")
            .collect()
    }

    fn names<R: Record>(record: &R) -> Vec<String> {
        record.columns().into_iter().map(|(name, _)| name.to_string()).collect()
    }

    const WORD: &str = "Platypus";

    fn label() -> ScreenApp {
        let key = format!("site:{}.example", WORD.to_lowercase());
        let mut app = ScreenApp::new(key, SpanKind::Site);
        app.name = format!("{WORD} News");
        app.category = Some(ScreenCategoryId::new());
        app.ignored = true;
        app.split = true;
        app
    }

    fn category() -> ScreenCategory {
        let mut category = ScreenCategory::new(format!("{WORD} time"));
        category.color = "#2563eb".into();
        category.builtin = WORD.to_lowercase();
        category.sort_order = 3;
        category
    }

    #[test]
    fn the_clear_columns_are_exactly_the_ones_v25_declares() {
        // `upsert_stmt` writes `columns()` into the table by name, so a
        // column here that the table lacks fails every write, and one the
        // table has that is missing here is left at its default forever.
        assert_eq!(names(&label()), declared(ScreenApp::TABLE));
        assert_eq!(names(&category()), declared(ScreenCategory::TABLE));
    }

    #[test]
    fn the_span_statement_names_every_column_v25_declares() {
        // Spans are written by hand rather than through `Record`, so the
        // same promise is checked against the statement's own text.
        let mut want = vec!["id".to_string()];
        want.extend(declared("screen_spans"));
        want.push("data".into());
        let head = PUT_SPAN.split_once('(').unwrap().1.split_once(')').unwrap().0;
        let got: Vec<String> = head.split(',').map(|c| c.trim().to_string()).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn no_clear_column_holds_anything_a_person_would_recognise() {
        // What a database holder sees of a label or a category is exactly
        // its `columns()`: an order and two times. Not a key, a name, a
        // colour or a starter's word.
        let all = [("label", label().columns()), ("category", category().columns())];
        for (record, columns) in all {
            for (name, value) in columns {
                match &value {
                    Value::Text(text) => {
                        assert!(
                            !text.to_lowercase().contains(&WORD.to_lowercase()),
                            "{record}.{name} holds {text:?}"
                        );
                        assert!(!text.contains("#2563eb"), "{record}.{name} holds the colour");
                    }
                    Value::Int(_) | Value::Null | Value::Bool(_) => {}
                    Value::Bytes(_) | Value::Real(_) => {
                        panic!("{record}.{name} is not a clear column of a kind v25 declares")
                    }
                }
            }
        }
    }
}
