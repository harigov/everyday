//! Where SQLite and Postgres actually disagree.
//!
//! Which is: less than you would think. The domain queries in this crate —
//! every filter, every join, every upsert, the whole clear-column index
//! strategy — are one string each, shared. What is left is this file, and it
//! is short on purpose: each entry here is a place the standard is not
//! standard, and adding a third database should mean adding a variant here
//! and a driver, not a third copy of `tasks.rs`.
//!
//! # The five differences
//!
//! | | SQLite | Postgres |
//! |---|---|---|
//! | placeholders | `?1` | `$1` |
//! | unlimited `OFFSET` | `LIMIT -1 OFFSET 20` | `LIMIT ALL OFFSET 20` |
//! | greater of two values | `MAX(a, b)` | `GREATEST(a, b)` |
//! | column types | `BLOB`, `INTEGER`, `REAL` | `BYTEA`, `BIGINT`, `DOUBLE PRECISION` |
//! | schema version | `PRAGMA user_version` | a table (see the driver) |
//!
//! Everything else that might have differed was written out of the queries
//! instead of into this file, which is the better trade every time it is
//! available:
//!
//! * `INSERT OR IGNORE` and `INSERT OR REPLACE` are SQLite-only spellings of
//!   things `ON CONFLICT ... DO NOTHING` and `ON CONFLICT ... DO UPDATE` say
//!   in both, so the queries say them the portable way.
//! * `WHERE visible = 1` became `WHERE visible`, because Postgres will not
//!   compare a boolean to an integer and both understand the shorter form.
//! * `at_us ASC` became `at_us ASC NULLS FIRST`. The two databases have
//!   *opposite* defaults for where NULLs sort — SQLite first, Postgres last —
//!   and untimed readings sorting to the wrong end of a day would be a
//!   silent, plausible-looking wrong answer rather than an error. Saying it
//!   out loud costs nothing and is now checked by the conformance suite.
//! * Sums that were divided in SQL are divided in Rust, because `SUM` over a
//!   `BIGINT` is exact in SQLite and a `numeric` in Postgres, and `/` means
//!   something different to each.

use std::borrow::Cow;

/// A SQL dialect this crate knows how to speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Postgres,
}

impl Dialect {
    /// Rewrite `?N` placeholders into whatever this database wants.
    ///
    /// Queries in this crate are written with SQLite's numbering because
    /// that is what they were written with before there was a second
    /// database, and a mechanical rewrite here was cheaper — and far less
    /// error-prone — than renumbering 1,500 lines of working SQL by hand.
    ///
    /// The rewrite is textual, so it assumes no `?` appears inside a string
    /// literal in this crate's SQL. Nothing here has one, and
    /// `no_query_hides_a_question_mark_in_a_literal` in `schema.rs` keeps it
    /// that way.
    pub fn bind<'a>(&self, sql: &'a str) -> Cow<'a, str> {
        match self {
            Dialect::Sqlite => Cow::Borrowed(sql),
            Dialect::Postgres if !sql.contains('?') => Cow::Borrowed(sql),
            Dialect::Postgres => {
                let mut out = String::with_capacity(sql.len());
                let mut rest = sql;
                while let Some(at) = rest.find('?') {
                    out.push_str(&rest[..at]);
                    let digits = rest[at + 1..].len()
                        - rest[at + 1..].trim_start_matches(|c: char| c.is_ascii_digit()).len();
                    if digits == 0 {
                        // A bare `?` is not a placeholder this crate writes.
                        // Passing it through unchanged means the database
                        // gets to complain about it, which beats guessing.
                        out.push('?');
                        rest = &rest[at + 1..];
                        continue;
                    }
                    out.push('$');
                    out.push_str(&rest[at + 1..at + 1 + digits]);
                    rest = &rest[at + 1 + digits..];
                }
                out.push_str(rest);
                Cow::Owned(out)
            }
        }
    }

    /// `LIMIT`/`OFFSET`, including the case with an offset and no limit.
    ///
    /// Both databases refuse to honour a bare `OFFSET` without a `LIMIT`, and
    /// they spell "no limit" differently: SQLite takes a negative number,
    /// Postgres takes the word `ALL`.
    pub fn limit_offset(&self, limit: Option<u32>, offset: u32) -> String {
        let limit = match (limit, self) {
            (Some(n), _) => n.to_string(),
            (None, Dialect::Sqlite) => "-1".into(),
            (None, Dialect::Postgres) => "ALL".into(),
        };
        format!(" LIMIT {limit} OFFSET {offset}")
    }

    /// The greater of two scalars. Not the aggregate `MAX`, confusingly.
    pub fn greatest(&self) -> &'static str {
        match self {
            Dialect::Sqlite => "MAX",
            Dialect::Postgres => "GREATEST",
        }
    }

    // ---- column types, for the DDL in `schema.rs` -----------------------

    /// Sealed payloads and other opaque bytes.
    pub fn blob(&self) -> &'static str {
        match self {
            Dialect::Sqlite => "BLOB",
            Dialect::Postgres => "BYTEA",
        }
    }

    /// Every integer column, whatever its range.
    ///
    /// One width rather than several. `sort_order` would fit in 32 bits, but
    /// Postgres will not take a 64-bit parameter for a 32-bit column without
    /// a cast, and this crate binds every integer as an `i64` — so a narrower
    /// column would buy a few bytes a row in exchange for a class of runtime
    /// type error. Timestamps need the full width anyway.
    pub fn int(&self) -> &'static str {
        match self {
            Dialect::Sqlite => "INTEGER",
            Dialect::Postgres => "BIGINT",
        }
    }

    /// A tracker reading's value: the one genuinely floating column.
    pub fn real(&self) -> &'static str {
        match self {
            Dialect::Sqlite => "REAL",
            Dialect::Postgres => "DOUBLE PRECISION",
        }
    }

    /// Flags. SQLite has no boolean and stores these as `0`/`1`, which is why
    /// [`Row::bool`](crate::conn::Row::bool) takes either.
    pub fn boolean(&self) -> &'static str {
        match self {
            Dialect::Sqlite => "INTEGER",
            Dialect::Postgres => "BOOLEAN",
        }
    }

    /// The default for a boolean column, which has to match its type.
    pub fn bool_default(&self, value: bool) -> &'static str {
        match (self, value) {
            (Dialect::Sqlite, true) => "1",
            (Dialect::Sqlite, false) => "0",
            (Dialect::Postgres, true) => "TRUE",
            (Dialect::Postgres, false) => "FALSE",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_renumbered_for_postgres_and_left_alone_for_sqlite() {
        let sql = "UPDATE t SET a = ?2, b = ?10 WHERE id = ?1";
        assert_eq!(Dialect::Sqlite.bind(sql), sql);
        assert_eq!(
            Dialect::Postgres.bind(sql),
            "UPDATE t SET a = $2, b = $10 WHERE id = $1",
            "two-digit placeholders must not be split into $1 followed by a 0"
        );
    }

    #[test]
    fn a_query_without_placeholders_is_not_copied() {
        let sql = "SELECT COUNT(*) FROM entries";
        assert!(matches!(Dialect::Postgres.bind(sql), Cow::Borrowed(_)));
    }

    #[test]
    fn an_offset_without_a_limit_is_spelled_for_each_database() {
        assert_eq!(Dialect::Sqlite.limit_offset(None, 20), " LIMIT -1 OFFSET 20");
        assert_eq!(Dialect::Postgres.limit_offset(None, 20), " LIMIT ALL OFFSET 20");
        assert_eq!(Dialect::Postgres.limit_offset(Some(5), 0), " LIMIT 5 OFFSET 0");
    }
}
