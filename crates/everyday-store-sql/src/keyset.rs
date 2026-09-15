//! Keyset paging, beside the offset form in [`SqlStore::page`].
//!
//! Offset paging asks a database to walk past the first `offset` rows and
//! throw them away, every time. That is fine at page nine of a task board
//! and ruinous at page four thousand of a hundred-thousand-message mailbox
//! — the database still has to read and discard everything before the
//! window, so the tenth "next page" click costs ten times what the first
//! one did, on both SQLite and Postgres.
//!
//! Keyset paging asks a different question: "the rows that come after this
//! one, in this order" — a `WHERE` clause a covering index answers directly,
//! with no scan of what was already shown. The price is that you can no
//! longer jump to page four thousand by arithmetic; you can only walk
//! forward (or backward) from somewhere you have already been. Every list
//! this crate has today jumps by offset, because the lists are small and a
//! page number is a nicer thing for a UI to hold than an opaque token. A
//! mailbox is neither small nor page-numbered — Superhuman does not show
//! "page 40 of 2,500" — so it pages by key from the start.
//!
//! # The comparison
//!
//! For a single ascending column the predicate is the familiar `col > ?`.
//! With more than one column, and mixed directions — `thread_mailboxes`
//! orders by `last_date_us DESC, thread_id ASC` — a plain row-value
//! comparison such as `(a, b) > (x, y)` does not say what is wanted: both
//! databases compare a row value lexicographically in *one* direction, and
//! there is no spelling of `>` that means "greater on the first column,
//! lesser on the second." So this expands the comparison into the OR-of-ANDs
//! a mixed-direction order actually needs:
//!
//! ```text
//! (a > ?)
//! OR (a = ? AND b < ?)
//! OR (a = ? AND b = ? AND id > ?)
//! ```
//!
//! one clause per column, each one fixing every earlier column equal and
//! comparing the next, with the operator flipped for a `DESC` column. This
//! is not merely the safe choice: it is the *only* one that is correct for
//! a mixed-direction order, so it is what this always emits — on both
//! dialects, identically, because nothing about it is dialect-specific. The
//! `LIMIT` that follows is the one place the two databases' spellings can
//! differ (see [`Dialect::limit_offset`]), and even that collapses to the
//! same text here because a keyset page always asks for a limit and never
//! an offset.
//!
//! # What a sort column must be
//!
//! Every column in `order` must be `NOT NULL`. `col = ?` is how each clause
//! above re-fixes an earlier column, and SQL's `NULL = NULL` is `NULL`, not
//! true — a cursor sitting on a `NULL` would silently match nothing on the
//! next page. Every column this crate would page a mailbox by (`date_us`,
//! `id`, `mailbox_id`) is one it already keeps `NOT NULL` for the same
//! reason the offset paths do: see the clear/sealed table at the top of
//! `lib.rs`. A caller with a genuinely optional sort key should coalesce it
//! to a sentinel before calling this, the way `trackers.rs` already does for
//! `at_us` — a decision for that caller to make, not one this module can
//! make for it.
//!
//! The last column in `order` should be one that is unique on its own — the
//! id, almost always — so that ties on every earlier column still produce a
//! total order. Without it, two rows that tie on every column *but* the
//! last would come back in whatever order the database happens to produce
//! them, and a page boundary landing between those two rows could show one
//! of them twice or skip it, depending on which way the tie broke.

use crate::SqlStore;
use crate::conn::Value;
use crate::dialect::Dialect;
use base64::Engine;
use everyday_core::error::{Error, Result};
use serde::{Deserialize, Serialize};

/// Which way one column of a keyset order runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Asc,
    Desc,
}

/// The rows [`build_page_after`] compares a cursor against carry only a few
/// kinds of value — a date in microseconds, an id, occasionally a flag —
/// and never the sealed payload. A cursor is serialised to travel outside
/// this crate (a UI holds one between "load more" clicks), so it is its own
/// small, `serde`-friendly mirror of [`Value`] rather than that enum
/// directly: `Value::Bytes` has no business in a sort key, and a type that
/// cannot represent it needs no runtime check to refuse it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum CursorValue {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
}

impl TryFrom<Value> for CursorValue {
    type Error = Error;

    fn try_from(v: Value) -> Result<Self> {
        match v {
            Value::Null => Ok(CursorValue::Null),
            // SQLite has no boolean column, so a flag a caller sorted by
            // arrives here as whichever spelling its own dialect gave back —
            // see `Row::bool`'s comment on the same disagreement.
            Value::Bool(b) => Ok(CursorValue::Int(i64::from(b))),
            Value::Int(n) => Ok(CursorValue::Int(n)),
            Value::Real(f) => Ok(CursorValue::Real(f)),
            Value::Text(s) => Ok(CursorValue::Text(s)),
            Value::Bytes(_) => {
                Err(Error::Invalid("a keyset cursor cannot hold a raw byte column".into()))
            }
        }
    }
}

impl From<CursorValue> for Value {
    fn from(v: CursorValue) -> Self {
        match v {
            CursorValue::Null => Value::Null,
            CursorValue::Int(n) => Value::Int(n),
            CursorValue::Real(f) => Value::Real(f),
            CursorValue::Text(s) => Value::Text(s),
        }
    }
}

/// An opaque position in a keyset-ordered list.
///
/// "Opaque" to the extent Rust's privacy can make it: the one way in is
/// [`KeyCursor::new`], from the values of the row a page ended on, in the
/// same column order [`SqlStore::page_after`] was called with; the one way
/// out that is not [`page_after`](SqlStore::page_after) itself is
/// [`KeyCursor::encode`], a string with nothing legible inside it. A caller
/// holds that string — in a `live.svelte.ts` store, in a saved scroll
/// position — and hands it back as `Option<&str>` via
/// [`KeyCursor::decode`] to ask for the next page. Neither the columns nor
/// their order are promised to stay the same across a schema change, so a
/// cursor that fails to decode, or decodes to the wrong number of columns,
/// is treated as "start again from the top" rather than an error a person
/// ever sees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyCursor {
    values: Vec<CursorValue>,
}

impl KeyCursor {
    /// A cursor made of these values, in the order a call to
    /// [`SqlStore::page_after`] used for `order`.
    ///
    /// Fails only if one of the values is [`Value::Bytes`] — a sort column
    /// is never sealed data, so a caller passing one has the wrong column.
    pub fn new(values: Vec<Value>) -> Result<Self> {
        Ok(Self { values: values.into_iter().map(CursorValue::try_from).collect::<Result<_>>()? })
    }

    fn len(&self) -> usize {
        self.values.len()
    }

    /// Base64 of this cursor's JSON. Stable across a restart — it holds no
    /// pointer into memory, a connection or a transaction, only the values
    /// themselves — so it is safe to persist and to send across a process
    /// boundary.
    pub fn encode(&self) -> String {
        // A cursor's own values are always representable in JSON: dates,
        // ids and flags, never a float that is NaN or infinite.
        let json = serde_json::to_vec(self).expect("a cursor is always valid JSON");
        base64::engine::general_purpose::STANDARD.encode(json)
    }

    /// The cursor `encode` produced, or an error naming the string as
    /// unusable rather than panicking on it. A caller that receives this
    /// from outside the process — a saved scroll position from a build with
    /// a different cursor shape — should treat any error the same way:
    /// start the list again from the top.
    pub fn decode(s: &str) -> Result<Self> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(|e| Error::Invalid(format!("not a keyset cursor: {e}")))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| Error::Invalid(format!("not a keyset cursor: {e}")))
    }
}

/// The keyset predicate, `ORDER BY` and `LIMIT` for one page — the part that
/// can be built, and so tested, without a database. `start` is how many
/// placeholders the query already has before this is appended, so the ones
/// this writes continue that numbering rather than restart it; the caller
/// (only [`SqlStore::page_after`], normally) is what makes `start` agree
/// with the query it is building.
fn build_page_after(
    dialect: Dialect,
    start: usize,
    order: &[(&str, Dir)],
    cursor: Option<&KeyCursor>,
    limit: u32,
) -> Result<(String, Vec<Value>)> {
    let mut sql = String::new();
    let mut args = Vec::new();

    if let Some(cursor) = cursor {
        if cursor.len() != order.len() {
            return Err(Error::Invalid(format!(
                "a keyset cursor of {} column(s) cannot page an order of {}",
                cursor.len(),
                order.len()
            )));
        }
        sql.push_str(" AND (");
        for i in 0..order.len() {
            if i > 0 {
                sql.push_str(" OR ");
            }
            sql.push('(');
            for (j, (col, _)) in order[..i].iter().enumerate() {
                args.push(Value::from(cursor.values[j].clone()));
                sql.push_str(&format!("{col} = ?{} AND ", start + args.len()));
            }
            let (col, dir) = order[i];
            let op = match dir {
                Dir::Asc => '>',
                Dir::Desc => '<',
            };
            args.push(Value::from(cursor.values[i].clone()));
            sql.push_str(&format!("{col} {op} ?{}", start + args.len()));
            sql.push(')');
        }
        sql.push(')');
    }

    let order_by = order
        .iter()
        .map(|(col, dir)| format!("{col} {}", if *dir == Dir::Asc { "ASC" } else { "DESC" }))
        .collect::<Vec<_>>()
        .join(", ");
    sql.push_str(" ORDER BY ");
    sql.push_str(&order_by);
    sql.push_str(&dialect.limit_offset(Some(limit), 0));

    Ok((sql, args))
}

impl SqlStore {
    /// The keyset form of [`page`](SqlStore::page): a `WHERE` clause naming
    /// everything past `cursor`, then the `ORDER BY` and `LIMIT` that agree
    /// with it, appended to a query whose earlier filters (built with
    /// [`Where`](crate::conn::Where), typically) already own `sql` and
    /// `args`. `cursor` of `None` asks for the first page.
    ///
    /// `order` must end in a column that is unique by itself — see the
    /// module documentation — and every column in it must be `NOT NULL`.
    /// Nothing here enforces either; both are checked by the query itself,
    /// which will happily produce a page with a gap or a repeat if they are
    /// not true.
    ///
    /// `pub` rather than `pub(crate)`, unlike [`page`](SqlStore::page):
    /// nothing in this crate calls it yet — the first caller is a mail
    /// listing, in a later phase — so the only callers today are a driver's
    /// own tests, reaching in the same way [`with_read`](SqlStore::with_read)
    /// already lets them run SQL this crate does not itself own.
    pub fn page_after(
        &self,
        sql: &mut String,
        args: &mut Vec<Value>,
        order: &[(&str, Dir)],
        cursor: Option<&KeyCursor>,
        limit: u32,
    ) -> Result<()> {
        let (fragment, new_args) =
            build_page_after(self.dialect, args.len(), order, cursor, limit)?;
        sql.push_str(&fragment);
        args.extend(new_args);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_page_has_no_predicate_at_all() {
        let (sql, args) = build_page_after(
            Dialect::Sqlite,
            0,
            &[("date_us", Dir::Desc), ("id", Dir::Asc)],
            None,
            20,
        )
        .unwrap();
        assert_eq!(sql, " ORDER BY date_us DESC, id ASC LIMIT 20 OFFSET 0");
        assert!(args.is_empty());
    }

    #[test]
    fn a_single_column_order_is_the_familiar_comparison() {
        let cursor = KeyCursor::new(vec![Value::Int(500)]).unwrap();
        let (sql, args) =
            build_page_after(Dialect::Sqlite, 0, &[("id", Dir::Asc)], Some(&cursor), 10).unwrap();
        assert_eq!(sql, " AND ((id > ?1)) ORDER BY id ASC LIMIT 10 OFFSET 0");
        assert_eq!(args, vec![Value::Int(500)]);
    }

    #[test]
    fn a_mixed_direction_order_expands_to_or_of_and() {
        // `last_date_us DESC, thread_id ASC` -- the shape a thread list
        // actually wants: most recent first, ties broken by id.
        let cursor = KeyCursor::new(vec![Value::Int(999), Value::Text("t1".into())]).unwrap();
        let (sql, args) = build_page_after(
            Dialect::Sqlite,
            0,
            &[("last_date_us", Dir::Desc), ("thread_id", Dir::Asc)],
            Some(&cursor),
            25,
        )
        .unwrap();
        assert_eq!(
            sql,
            " AND ((last_date_us < ?1) OR (last_date_us = ?2 AND thread_id > ?3)) \
             ORDER BY last_date_us DESC, thread_id ASC LIMIT 25 OFFSET 0"
        );
        assert_eq!(args, vec![Value::Int(999), Value::Int(999), Value::Text("t1".into())]);
    }

    #[test]
    fn a_three_column_order_re_fixes_every_earlier_column_at_each_step() {
        // The case a two-column order cannot exercise: the third clause has
        // to hold *two* earlier columns equal, and each must be the cursor's
        // own value for that column, not whichever value happened to be
        // pushed last.
        let cursor = KeyCursor::new(vec![
            Value::Text("m1".into()),
            Value::Int(500),
            Value::Text("t1".into()),
        ])
        .unwrap();
        let (sql, args) = build_page_after(
            Dialect::Sqlite,
            0,
            &[("mailbox_id", Dir::Asc), ("last_date_us", Dir::Desc), ("thread_id", Dir::Asc)],
            Some(&cursor),
            10,
        )
        .unwrap();
        assert_eq!(
            sql,
            " AND ((mailbox_id > ?1) \
             OR (mailbox_id = ?2 AND last_date_us < ?3) \
             OR (mailbox_id = ?4 AND last_date_us = ?5 AND thread_id > ?6)) \
             ORDER BY mailbox_id ASC, last_date_us DESC, thread_id ASC LIMIT 10 OFFSET 0"
        );
        assert_eq!(
            args,
            vec![
                Value::Text("m1".into()),
                Value::Text("m1".into()),
                Value::Int(500),
                Value::Text("m1".into()),
                Value::Int(500),
                Value::Text("t1".into()),
            ]
        );
    }

    #[test]
    fn placeholder_numbering_continues_from_where_the_query_already_reached() {
        // A caller that already bound `journal_id = ?1` must see the keyset
        // predicate start at `?2`, or the two halves of the query would
        // fight over the same placeholder.
        let cursor = KeyCursor::new(vec![Value::Int(7)]).unwrap();
        let (sql, args) =
            build_page_after(Dialect::Sqlite, 1, &[("local_date_us", Dir::Asc)], Some(&cursor), 5)
                .unwrap();
        assert_eq!(sql, " AND ((local_date_us > ?2)) ORDER BY local_date_us ASC LIMIT 5 OFFSET 0");
        assert_eq!(args, vec![Value::Int(7)]);
    }

    #[test]
    fn the_two_dialects_produce_the_same_text() {
        // `LIMIT`/`OFFSET` is the one place the dialects can disagree, and
        // they do not disagree here: a keyset page always asks for a limit
        // and never an offset, which is the one case `Dialect::limit_offset`
        // spells identically.
        let cursor = KeyCursor::new(vec![Value::Int(1), Value::Text("a".into())]).unwrap();
        let order = [("a", Dir::Asc), ("id", Dir::Asc)];
        let (sqlite, _) = build_page_after(Dialect::Sqlite, 0, &order, Some(&cursor), 20).unwrap();
        let (postgres, _) =
            build_page_after(Dialect::Postgres, 0, &order, Some(&cursor), 20).unwrap();
        assert_eq!(sqlite, postgres);
    }

    #[test]
    fn a_cursor_with_the_wrong_number_of_columns_is_refused() {
        let cursor = KeyCursor::new(vec![Value::Int(1)]).unwrap();
        let err = build_page_after(
            Dialect::Sqlite,
            0,
            &[("a", Dir::Asc), ("b", Dir::Asc)],
            Some(&cursor),
            10,
        )
        .unwrap_err();
        assert_eq!(err.code(), "invalid");
    }

    #[test]
    fn a_cursor_refuses_raw_bytes() {
        let err = KeyCursor::new(vec![Value::Bytes(vec![1, 2, 3])]).unwrap_err();
        assert_eq!(err.code(), "invalid");
    }

    #[test]
    fn a_cursor_round_trips_through_its_encoded_string() {
        let cursor =
            KeyCursor::new(vec![Value::Int(42), Value::Text("thread".into()), Value::Null])
                .unwrap();
        let decoded = KeyCursor::decode(&cursor.encode()).unwrap();
        assert_eq!(cursor, decoded);
    }

    #[test]
    fn garbage_is_a_decode_error_rather_than_a_panic() {
        assert!(KeyCursor::decode("not base64 at all!!").is_err());
        assert!(KeyCursor::decode("dGhpcyBpcyBub3QganNvbg==").is_err());
    }
}
