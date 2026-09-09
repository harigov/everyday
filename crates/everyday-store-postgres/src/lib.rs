//! Postgres driver for the SQL storage backend — including Supabase.
//!
//! Everything about *what* is stored is in
//! [`everyday_store_sql`](everyday_store_sql): the schema, the queries, the
//! cascades, the decision about which columns stay in the clear. This crate
//! is the part that is true of Postgres and of nothing else — a connection, a
//! TLS stack, a schema to put the tables in, and a table to remember the
//! migration version.
//!
//! # What a hosted vault is, and what it is not
//!
//! It is not a sync service. There is no merge, no per-device state and no
//! conflict UI beyond the one the store already has
//! ([`put_entry_if`](everyday_core::JournalStore::put_entry_if), which is a
//! single conditional statement here as it is on SQLite). What it is: the
//! same vault, in a database two machines can both reach, with the vault's
//! own write lock still doing what it always did — one writer at a time.
//!
//! It is also not a hole in the encryption. Payloads are sealed by
//! [`SqlStore`] before they reach the wire, so the server holds ciphertext
//! and the clear index columns listed in
//! [`everyday_store_sql`](everyday_store_sql)'s docs, and never a key. The
//! password is never sent anywhere; there is nothing for the server to be
//! trusted with except availability. The honest cost is the one that table
//! spells out: whoever runs the database can see the *shape* of the vault —
//! how many journals, which days were written on, how many things are
//! overdue — in exactly the way whoever holds a SQLite file can.
//!
//! # Attachments
//!
//! In a `blobs` table, not in a directory. A folder of media on one laptop is
//! not part of a vault that two machines open — see
//! [`Media`](everyday_store_sql::Media). Postgres caps a single value at
//! 1 GB, so this driver declares a limit and the interface can refuse a file
//! before it is uploaded rather than after.
//!
//! # Configuration
//!
//! Two settings, both sealed under the vault key (see
//! [`BackendSettings`](everyday_core::BackendSettings)):
//!
//! | Key | Meaning |
//! |---|---|
//! | `url` | `postgresql://user:password@host:5432/database`. Required. |
//! | `schema` | Which schema to put the tables in. Defaults to `everyday`. |
//!
//! [`URL_ENV`] overrides the stored URL, which is how a deployment keeps the
//! credential out of the vault file entirely and in whatever it already uses
//! for secrets.
//!
//! ## Supabase
//!
//! Use the **session** connection string (port 5432), not the transaction
//! pooler (port 6543). This driver prepares its statements, which is exactly
//! what a transaction-mode pooler cannot carry across; [`PostgresStore::open`]
//! says so out loud when it sees that port rather than letting the failure
//! arrive later as a confusing error mid-save.

use everyday_core::error::{Error, Result};
use everyday_core::store::{
    BackendSettings, JournalStore, SettingSpec, StoreContext, StoreFactory,
};
use everyday_store_sql::conn::{Connection, Row, Sql, Transaction, Value};
use everyday_store_sql::dialect::Dialect;
use everyday_store_sql::schema::VersionStore;
use everyday_store_sql::{Driver, Media, SqlStore};
use postgres::types::{IsNull, ToSql, Type};
use rustls_platform_verifier::BuilderVerifierExt;
use std::collections::HashMap;
use std::path::Path;

pub const BACKEND_ID: &str = "postgres";

/// Environment variable that overrides the stored connection URL.
///
/// For the deployment that would rather its database password lived in a
/// secret manager than in a vault file, however well sealed.
pub const URL_ENV: &str = "EVERYDAY_DATABASE_URL";

/// Settings key for the schema the tables live in.
pub const SCHEMA_KEY: &str = "schema";

/// Where the tables go unless told otherwise.
///
/// Not `public`. On a Supabase project `public` is where the *user's* own
/// tables are, and a journal that scattered ten tables called `items`,
/// `logs` and `events` among them would be a poor guest — and would collide
/// with a second vault in the same database. A named schema makes "this
/// database holds two vaults and an application" work by default.
pub const DEFAULT_SCHEMA: &str = "everyday";

/// The largest attachment this backend accepts.
///
/// Postgres refuses a single value over 1 GB. The sealed container adds a
/// header and one AEAD tag per 256 KiB chunk, so the ceiling is set a little
/// under it — a limit that rejects the paste is a better outcome than one
/// that accepts the paste and fails the insert.
pub const MAX_BLOB_BYTES: u64 = 900 * 1024 * 1024;

/// Registers this backend with a [`everyday_core::BackendRegistry`].
pub struct PostgresFactory;

impl StoreFactory for PostgresFactory {
    fn id(&self) -> &'static str {
        BACKEND_ID
    }

    fn name(&self) -> &'static str {
        "On a Postgres server"
    }

    fn describe(&self) -> &'static str {
        "A Postgres database \u{2014} Supabase or your own, reachable from more than one computer"
    }

    fn settings(&self) -> Vec<SettingSpec> {
        vec![
            SettingSpec {
                key: BackendSettings::URL,
                label: "Connection URL",
                placeholder: "postgresql://user:password@host:5432/database",
                required: true,
                // It has a password in it. Masked on entry, and sealed under
                // the vault key once stored.
                secret: true,
            },
            SettingSpec {
                key: SCHEMA_KEY,
                label: "Schema",
                placeholder: DEFAULT_SCHEMA,
                required: false,
                secret: false,
            },
        ]
    }

    fn open(&self, ctx: StoreContext) -> Result<Box<dyn JournalStore>> {
        Ok(Box::new(PostgresStore::open(ctx)?))
    }
}

/// A vault's records in a Postgres database.
pub struct PostgresStore;

impl PostgresStore {
    pub fn open(ctx: StoreContext) -> Result<SqlStore> {
        // The environment wins. A vault carried between machines can then
        // name a database per machine -- a local one for development, the
        // hosted one otherwise -- without rewriting its header.
        let url = match std::env::var(URL_ENV) {
            Ok(url) if !url.trim().is_empty() => url,
            _ => ctx.settings.require(BackendSettings::URL, BACKEND_ID)?.to_string(),
        };
        let schema = ctx.settings.get(SCHEMA_KEY).unwrap_or(DEFAULT_SCHEMA);
        let schema = valid_identifier(schema)?;

        warn_about_transaction_pooling(&url);

        let mut client = postgres::Client::connect(&url, tls()?).map_err(Error::backend)?;

        // The tables go in their own schema, made if it is not there. Both
        // statements are idempotent, and `search_path` is what lets every
        // query in `everyday-store-sql` name a bare table exactly as it does
        // on SQLite.
        client
            .batch_execute(&format!(
                "CREATE SCHEMA IF NOT EXISTS \"{schema}\"; SET search_path TO \"{schema}\""
            ))
            .map_err(Error::backend)?;

        SqlStore::open(
            Box::new(PostgresDriver),
            Box::new(PgConn { client, cache: StatementCache::new() }),
            // In the database, not on this disk: see the module docs.
            Media::Table,
            &ctx,
        )
    }
}

/// Say something useful about the one Supabase footgun.
///
/// Port 6543 is Supabase's transaction-mode pooler. This driver prepares
/// statements, and a transaction-mode pooler hands the next statement to a
/// different backend, so a prepared statement is gone by the time it is
/// executed. The failure that produces is `prepared statement "s0" does not
/// exist` in the middle of an ordinary save, which is not a message anyone
/// can act on. Saying it here, at connect time, is.
fn warn_about_transaction_pooling(url: &str) {
    if url.contains(":6543") {
        tracing::warn!(
            "this looks like Supabase's transaction pooler (port 6543); use the session \
             connection string on port 5432 -- prepared statements do not survive a \
             transaction-mode pooler"
        );
    }
}

/// TLS, with the platform's own trust store.
///
/// An explicit crypto provider rather than the process default: this process
/// may already carry another rustls user (the calendar fetcher does), and
/// two providers in one binary makes "the default" ambiguous enough to panic
/// at connect time. Naming one here cannot be ambiguous.
fn tls() -> Result<tokio_postgres_rustls::MakeRustlsConnect> {
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(Error::backend)?
        .with_platform_verifier()
        .map_err(Error::backend)?
        .with_no_client_auth();
    Ok(tokio_postgres_rustls::MakeRustlsConnect::new(config))
}

/// A schema name that can be interpolated into DDL.
///
/// `search_path` and `CREATE SCHEMA` take an identifier, and an identifier
/// cannot be a bound parameter -- so this is the one place in the two SQL
/// crates where a caller's string reaches a statement as text. It is
/// therefore checked rather than escaped: letters, digits and underscores,
/// starting with a letter or an underscore, within Postgres's 63-byte limit.
/// Anything else is refused by name.
fn valid_identifier(name: &str) -> Result<&str> {
    let ok = !name.is_empty()
        && name.len() <= 63
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(name)
    } else {
        Err(Error::Invalid(format!(
            "{name:?} is not a usable schema name: use letters, digits and underscores, \
             starting with a letter"
        )))
    }
}

/// What Postgres answers that no other database does.
pub struct PostgresDriver;

impl Driver for PostgresDriver {
    fn backend_id(&self) -> &'static str {
        BACKEND_ID
    }

    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }

    fn max_blob_bytes(&self) -> Option<u64> {
        Some(MAX_BLOB_BYTES)
    }

    // `flush` is the default no-op: the server has committed by the time it
    // answered, and there is no local buffer to push. `check_integrity` is
    // the default empty answer, for the same sort of reason -- a torn page in
    // a hosted database is not this process's to find or to fix.

    /// Refused, and pointedly.
    ///
    /// A "backup" written by a client that cannot take a consistent
    /// server-side snapshot would be a file someone trusted and should not
    /// have. Postgres has `pg_dump` and Supabase has scheduled backups, so
    /// this names them rather than imitating them badly. `everyday export`
    /// still works and is the other honest answer: it reads every entry
    /// through the vault and writes readable files.
    fn snapshot(&self, _conn: &mut dyn Sql, _root: &Path, _dir: &Path) -> Result<()> {
        Err(Error::Unsupported(
            "copying a database it does not host (use pg_dump or your provider's backups, \
             or `everyday export` for readable files)",
        ))
    }
}

impl VersionStore for PostgresDriver {
    /// A one-row table, because Postgres has no equivalent of SQLite's free
    /// integer in the file header.
    ///
    /// Created here rather than by a migration step for the obvious reason:
    /// it is what the migration reads to decide which steps to run.
    fn schema_version(&self, conn: &mut dyn Sql) -> Result<i64> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS schema_version (
                 only_row BOOLEAN PRIMARY KEY DEFAULT TRUE,
                 version  BIGINT  NOT NULL,
                 CONSTRAINT schema_version_is_one_row CHECK (only_row)
             )",
            &[],
        )?;
        match conn.query("SELECT version FROM schema_version", &[])?.first() {
            Some(row) => row.i64(0),
            // No row means a database nothing has been created in yet, which
            // is version zero -- the same thing `PRAGMA user_version` says
            // about an empty file.
            None => Ok(0),
        }
    }

    fn set_schema_version(&self, conn: &mut dyn Sql, version: i64) -> Result<()> {
        conn.execute(
            "INSERT INTO schema_version (only_row, version) VALUES (TRUE, ?1)
             ON CONFLICT (only_row) DO UPDATE SET version = ?1",
            &[Value::Int(version)],
        )?;
        Ok(())
    }
}

// ---- the driver proper: postgres behind the two-method trait -------------

/// The statements a session has already prepared, oldest evicted first.
///
/// The cache is what makes a batch write one round trip a row instead of two.
/// Every hot loop in `everyday-store-sql` runs the same SQL repeatedly -- one
/// `INSERT` per task in a save, one per occurrence in a calendar sync -- and
/// against a *network* database the parse round trip is the one that shows.
/// It is also why the transaction pooler is not supported: a prepared
/// statement belongs to the session that prepared it.
///
/// # Why it is bounded
///
/// Because the key is the SQL text and the SQL text is not from a fixed set.
/// `LIMIT n OFFSET m` is written into the query as literals, so paging a long
/// list produces a new statement per page; the filter combinations in
/// `list_tasks` and `list_entries` produce another handful each. Unbounded,
/// that is a slow leak of *server* memory as much as client memory -- every
/// entry is a prepared statement the backend is holding open for a session
/// that may last all day. Dropping a `Statement` sends a `Close`, so eviction
/// really does give it back.
///
/// Insertion order rather than a true LRU, which needs a crate or a linked
/// list to do properly. The distinction does not signify at this size: the
/// working set is a dozen statements -- the upserts and the by-id lookups --
/// and the churn is all pagination, which never repeats a page anyway.
///
/// Generic in what it holds only so the eviction can be tested: a
/// `postgres::Statement` cannot be built without a server to prepare it on,
/// and the bookkeeping is the part with a bug in it to have.
struct StatementCache<T = postgres::Statement> {
    by_sql: HashMap<String, T>,
    order: std::collections::VecDeque<String>,
}

impl<T: Clone> StatementCache<T> {
    /// Bigger than the working set by enough that the statements that matter
    /// are never the ones evicted. Rusqlite's own cache defaults to 16.
    const CAPACITY: usize = 64;

    fn new() -> Self {
        Self { by_sql: HashMap::new(), order: std::collections::VecDeque::new() }
    }

    fn get(&self, sql: &str) -> Option<T> {
        self.by_sql.get(sql).cloned()
    }

    fn insert(&mut self, sql: &str, stmt: &T) {
        if self.by_sql.insert(sql.to_string(), stmt.clone()).is_none() {
            self.order.push_back(sql.to_string());
        }
        while self.order.len() > Self::CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.by_sql.remove(&oldest);
            }
        }
    }
}

/// A session, with the statements it has already prepared.
struct PgConn {
    client: postgres::Client,
    cache: StatementCache,
}

impl PgConn {
    fn statement(&mut self, sql: &str) -> Result<postgres::Statement> {
        if let Some(stmt) = self.cache.get(sql) {
            return Ok(stmt);
        }
        let stmt = self.client.prepare(sql).map_err(Error::backend)?;
        self.cache.insert(sql, &stmt);
        Ok(stmt)
    }
}

impl Sql for PgConn {
    fn execute(&mut self, sql: &str, args: &[Value]) -> Result<u64> {
        let sql = Dialect::Postgres.bind(sql);
        let stmt = self.statement(&sql)?;
        let binds = bind(args);
        self.client.execute(&stmt, &as_params(&binds)).map_err(Error::backend)
    }

    fn query(&mut self, sql: &str, args: &[Value]) -> Result<Vec<Row>> {
        let sql = Dialect::Postgres.bind(sql);
        let stmt = self.statement(&sql)?;
        let binds = bind(args);
        let rows = self.client.query(&stmt, &as_params(&binds)).map_err(Error::backend)?;
        rows.iter().map(row_of).collect()
    }
}

impl Connection for PgConn {
    fn begin(&mut self) -> Result<Box<dyn Transaction + '_>> {
        let tx = self.client.transaction().map_err(Error::backend)?;
        Ok(Box::new(PgTx { tx, cache: StatementCache::new() }))
    }
}

/// A transaction, with its own cache.
///
/// Its own, and not the connection's, because the connection is borrowed
/// mutably for as long as this exists. It is bounded on the same terms and
/// for the same reason, though it rarely fills: a transaction here runs one
/// or two statements in a loop and then commits.
struct PgTx<'a> {
    tx: postgres::Transaction<'a>,
    cache: StatementCache,
}

impl PgTx<'_> {
    fn statement(&mut self, sql: &str) -> Result<postgres::Statement> {
        if let Some(stmt) = self.cache.get(sql) {
            return Ok(stmt);
        }
        let stmt = self.tx.prepare(sql).map_err(Error::backend)?;
        self.cache.insert(sql, &stmt);
        Ok(stmt)
    }
}

impl Sql for PgTx<'_> {
    fn execute(&mut self, sql: &str, args: &[Value]) -> Result<u64> {
        let sql = Dialect::Postgres.bind(sql);
        let stmt = self.statement(&sql)?;
        let binds = bind(args);
        self.tx.execute(&stmt, &as_params(&binds)).map_err(Error::backend)
    }

    fn query(&mut self, sql: &str, args: &[Value]) -> Result<Vec<Row>> {
        let sql = Dialect::Postgres.bind(sql);
        let stmt = self.statement(&sql)?;
        let binds = bind(args);
        let rows = self.tx.query(&stmt, &as_params(&binds)).map_err(Error::backend)?;
        rows.iter().map(row_of).collect()
    }
}

impl Transaction for PgTx<'_> {
    fn commit(self: Box<Self>) -> Result<()> {
        self.tx.commit().map_err(Error::backend)
    }
}

/// Wrap each argument so it can be handed to `postgres`.
///
/// The `Bind` newtype is not decoration: [`Value`] and [`ToSql`] are both
/// foreign to this crate, so the impl has to hang off a local type.
fn bind(args: &[Value]) -> Vec<Bind<'_>> {
    args.iter().map(Bind).collect()
}

/// The same wrappers as `&dyn ToSql`, which is the shape `postgres` takes.
fn as_params<'a>(binds: &'a [Bind<'a>]) -> Vec<&'a (dyn ToSql + Sync)> {
    binds.iter().map(|b| b as &(dyn ToSql + Sync)).collect()
}

/// A row of Postgres values as [`Row`].
fn row_of(row: &postgres::Row) -> Result<Row> {
    let mut out = Vec::with_capacity(row.len());
    for (i, column) in row.columns().iter().enumerate() {
        out.push(value_of(row, i, column.type_())?);
    }
    Ok(Row(out))
}

fn value_of(row: &postgres::Row, i: usize, ty: &Type) -> Result<Value> {
    let value = match *ty {
        Type::BOOL => row.get::<_, Option<bool>>(i).map(Value::Bool),
        Type::INT2 => row.get::<_, Option<i16>>(i).map(|n| Value::Int(i64::from(n))),
        Type::INT4 => row.get::<_, Option<i32>>(i).map(|n| Value::Int(i64::from(n))),
        Type::INT8 => row.get::<_, Option<i64>>(i).map(Value::Int),
        Type::FLOAT4 => row.get::<_, Option<f32>>(i).map(|f| Value::Real(f64::from(f))),
        Type::FLOAT8 => row.get::<_, Option<f64>>(i).map(Value::Real),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN => {
            row.get::<_, Option<String>>(i).map(Value::Text)
        }
        Type::BYTEA => row.get::<_, Option<Vec<u8>>>(i).map(Value::Bytes),
        // Deliberately not a silent fallback. The one place this crate could
        // have produced a `NUMERIC` -- `SUM` over a `BIGINT` -- is cast in
        // the query that does it, so reaching this means a new query has
        // grown a type nothing above knows how to read, and the useful
        // outcome is being told which.
        ref other => {
            return Err(Error::Invalid(format!(
                "column {i} has Postgres type {other}, which this driver cannot read"
            )));
        }
    };
    Ok(value.unwrap_or(Value::Null))
}

/// A [`Value`] on its way into a statement.
///
/// Postgres is strict about parameter types in a way SQLite is not, and this
/// crate binds every integer as an `i64` -- so the conversion is driven by
/// the type the *server* inferred for the placeholder rather than by the
/// shape of the value. That is what lets `substr(data, ?2, ?3)`, whose
/// arguments Postgres types as `int4`, take the same `Value::Int` as an
/// insert into a `BIGINT` column.
#[derive(Debug)]
struct Bind<'a>(&'a Value);

impl ToSql for Bind<'_> {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut postgres::types::private::BytesMut,
    ) -> std::result::Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match (self.0, ty) {
            (Value::Null, _) => Ok(IsNull::Yes),
            (Value::Bool(b), _) => b.to_sql(ty, out),
            (Value::Int(n), &Type::INT2) => i16::try_from(*n)?.to_sql(ty, out),
            (Value::Int(n), &Type::INT4) => i32::try_from(*n)?.to_sql(ty, out),
            (Value::Int(n), &Type::FLOAT4) => (*n as f32).to_sql(ty, out),
            (Value::Int(n), &Type::FLOAT8) => (*n as f64).to_sql(ty, out),
            (Value::Int(n), _) => n.to_sql(ty, out),
            (Value::Real(f), &Type::FLOAT4) => (*f as f32).to_sql(ty, out),
            (Value::Real(f), _) => f.to_sql(ty, out),
            (Value::Text(s), _) => s.to_sql(ty, out),
            (Value::Bytes(b), _) => b.to_sql(ty, out),
        }
    }

    /// Everything, because what a value can become depends on the type the
    /// server inferred and that is not available here. A genuine mismatch is
    /// reported by [`ToSql::to_sql`] above, with both types named.
    fn accepts(_: &Type) -> bool {
        true
    }

    postgres::types::to_sql_checked!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_schema_name_that_could_break_out_of_the_ddl_is_refused() {
        // The one place a caller's string is interpolated rather than bound,
        // so this is the check that has to hold.
        for bad in ["public; DROP TABLE entries", "with space", "\"quoted\"", "1st", "", "a-b"] {
            assert!(valid_identifier(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(valid_identifier(&"x".repeat(64)).is_err(), "over Postgres's 63-byte limit");
    }

    #[test]
    fn ordinary_schema_names_are_allowed() {
        for good in ["everyday", "_private", "vault2", DEFAULT_SCHEMA, &"x".repeat(63)] {
            assert!(valid_identifier(good).is_ok(), "{good:?} should be allowed");
        }
    }

    #[test]
    fn the_statement_cache_stops_growing() {
        // Not a nicety: the cache key is the SQL text, and pagination writes
        // its offset into the text -- so an unbounded cache would hold one
        // prepared statement per page ever scrolled, on the server as well as
        // here, for as long as the vault stayed open.
        type Cache = StatementCache<u32>;
        let cap = Cache::CAPACITY;
        let mut cache = Cache::new();

        for n in 0..cap * 3 {
            cache.insert(&format!("SELECT … OFFSET {n}"), &(n as u32));
        }
        assert_eq!(cache.by_sql.len(), cap);
        assert_eq!(cache.order.len(), cap, "the queue must not outgrow the map either");

        // Oldest first, so what survives is what was asked for most recently.
        assert!(cache.get("SELECT … OFFSET 0").is_none());
        assert_eq!(
            cache.get(&format!("SELECT … OFFSET {}", cap * 3 - 1)),
            Some(cap as u32 * 3 - 1)
        );
    }

    #[test]
    fn re_preparing_the_same_statement_does_not_queue_it_twice() {
        // The hot path: the same `INSERT` once per task in a batch. If each
        // repeat pushed another key, the queue would outgrow the map and
        // start evicting live entries.
        let mut cache = StatementCache::<u32>::new();
        for _ in 0..100 {
            cache.insert("INSERT INTO tasks …", &1);
        }
        assert_eq!(cache.order.len(), 1);
        assert_eq!(cache.get("INSERT INTO tasks …"), Some(1));
    }

    #[test]
    fn the_factory_asks_for_a_url_and_marks_it_secret() {
        let settings = PostgresFactory.settings();
        let url = settings.iter().find(|s| s.key == BackendSettings::URL).expect("a url field");
        assert!(url.required);
        assert!(url.secret, "a connection URL carries a password");
        // The placeholder is shown in an empty field, so it must not be a
        // real credential someone could paste in by accident.
        assert!(!url.placeholder.contains("supabase"));
    }

    #[test]
    fn opening_without_a_url_says_which_setting_is_missing() {
        // Not a connection error three layers down: the vault was configured
        // wrongly, and the message should name the thing to fix.
        let ctx = StoreContext::new(
            std::path::PathBuf::from("/tmp/nowhere"),
            std::sync::Arc::new(everyday_core::crypto::NullCipher),
        );
        // Only meaningful when the environment is not supplying one.
        if std::env::var(URL_ENV).is_ok() {
            return;
        }
        let Err(err) = PostgresStore::open(ctx) else {
            panic!("opening without a connection URL must fail");
        };
        assert_eq!(err.code(), "invalid", "got {err}");
        assert!(err.to_string().contains("url"), "got {err}");
    }
}
