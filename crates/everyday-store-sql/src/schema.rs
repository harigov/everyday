//! The database schema, and the migrations that reach it.
//!
//! Kept apart from the query code so that "what shape is the database" is one
//! file you can read start to finish, rather than three constants buried
//! between two hundred lines of `SELECT`. Each domain was added by a numbered
//! step that leaves the ones before it alone -- see [`migrate`].
//!
//! # Two databases, one schema
//!
//! The DDL is generated rather than written out, because the alternative is
//! two copies of it that drift. What varies is only the column types, and
//! they come from [`Dialect`]: `BLOB`/`BYTEA`, `INTEGER`/`BIGINT`,
//! `REAL`/`DOUBLE PRECISION`, `INTEGER`/`BOOLEAN`. Table names, column names,
//! nullability, defaults, foreign keys and every index are identical, which
//! is what lets one set of queries run against both.
//!
//! The steps are also numbered identically, so a Postgres database created
//! today is at version 5 exactly as a SQLite one written a year ago is. That
//! matters for the obvious reason -- one `SCHEMA_VERSION` constant, one place
//! to bump -- and for a less obvious one: it keeps a vault's records portable
//! between the two, because there is only ever one shape they can be in.

use crate::conn::Sql;
use crate::dialect::Dialect;
use everyday_core::error::{Error, Result};

/// Schema the code in this crate expects. Bumped by adding a step below.
pub const SCHEMA_VERSION: i64 = 7;

/// How a driver remembers which step a database has reached.
///
/// SQLite has `PRAGMA user_version`, a free 32-bit integer in the file
/// header. Postgres has nothing of the kind, so its driver keeps a
/// one-row table. Neither belongs in this file, which is why this is a
/// parameter and not a query.
pub trait VersionStore {
    fn schema_version(&self, conn: &mut dyn Sql) -> Result<i64>;
    fn set_schema_version(&self, conn: &mut dyn Sql, version: i64) -> Result<()>;
}

/// Bring the database up to [`SCHEMA_VERSION`].
///
/// Stepped rather than all-or-nothing: a vault written by an earlier build
/// has entries in it, so version 2 must *add* the task tables beside them
/// rather than recreate the database, version 3 the calendar tables beside
/// both, version 4 the library tables beside all three, version 5 the
/// readings beside all four and version 6 the assistant beside all five.
/// Each step is idempotent and runs in its own transaction, and the recorded
/// version is only advanced once they all land.
pub fn migrate(
    conn: &mut dyn crate::conn::Connection,
    dialect: Dialect,
    versions: &dyn VersionStore,
) -> Result<()> {
    let version = versions.schema_version(conn)?;
    // A database from a *newer* build is refused rather than opened.
    //
    // Nothing here can know what a later version did to the schema, and the
    // failure is silent in the worst way: the steps below are all skipped,
    // every query still parses against whatever columns happen to remain,
    // and writes land in a shape the newer build did not expect. `Vault::open`
    // already makes exactly this check against the header's format version;
    // the store had been the one layer that would open anything.
    if version > SCHEMA_VERSION {
        return Err(Error::UnsupportedVaultVersion {
            found: version as u32,
            supported: SCHEMA_VERSION as u32,
        });
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }

    for (index, statements) in steps(dialect).into_iter().enumerate() {
        let step_version = index as i64 + 1;
        if version >= step_version {
            continue;
        }
        let mut tx = conn.begin()?;
        for sql in &statements {
            tx.execute(sql, &[])?;
        }
        tx.commit()?;
    }
    versions.set_schema_version(conn, SCHEMA_VERSION)
}

/// Every migration step, in order. Index 0 is version 1.
pub fn steps(d: Dialect) -> Vec<Vec<String>> {
    vec![v1(d), v2(d), v3(d), v4(d), v5(d), v6(d), v7(d)]
}

/// The `blobs` table, for a backend that keeps attachments in the database.
///
/// Deliberately outside the numbered steps above, because it is not part of
/// the record schema: it belongs to the media layer, exactly as the `media/`
/// directory belongs to [`FileBlobStore`](everyday_core::FileBlobStore) and
/// is created by it on open rather than by a migration. A SQLite vault never
/// has this table; a Postgres one creates it the first time it is opened.
pub fn blobs_table(d: Dialect) -> Vec<String> {
    let (blob, int) = (d.blob(), d.int());
    vec![
        format!(
            "CREATE TABLE IF NOT EXISTS blobs (
                 id          TEXT    PRIMARY KEY NOT NULL,
                 byte_len    {int} NOT NULL,
                 created_us  {int} NOT NULL,
                 data        {blob} NOT NULL
             )"
        ),
        // Garbage collection asks "what is old enough to delete", which is a
        // scan of this column and nothing else.
        "CREATE INDEX IF NOT EXISTS blobs_by_age ON blobs (created_us)".into(),
    ]
}

/// Version 1: the journal domain.
fn v1(d: Dialect) -> Vec<String> {
    let (blob, int, boolean) = (d.blob(), d.int(), d.boolean());
    let f = d.bool_default(false);
    vec![
        format!(
            "CREATE TABLE IF NOT EXISTS journals (
                 id          TEXT    PRIMARY KEY NOT NULL,
                 sort_order  {int} NOT NULL DEFAULT 0,
                 updated_us  {int} NOT NULL,
                 data        {blob} NOT NULL
             )"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS entries (
                 id          TEXT    PRIMARY KEY NOT NULL,
                 journal_id  TEXT    NOT NULL,
                 local_date  TEXT    NOT NULL,
                 created_us  {int} NOT NULL,
                 updated_us  {int} NOT NULL,
                 starred     {boolean} NOT NULL DEFAULT {f},
                 pinned      {boolean} NOT NULL DEFAULT {f},
                 data        {blob} NOT NULL,
                 summary     {blob} NOT NULL
             )"
        ),
        // The list view is always "this journal, newest first", so the
        // covering index is (journal_id, local_date, created_us).
        "CREATE INDEX IF NOT EXISTS entries_by_journal_date
             ON entries (journal_id, local_date DESC, created_us DESC)"
            .into(),
        "CREATE INDEX IF NOT EXISTS entries_by_date
             ON entries (local_date DESC, created_us DESC)"
            .into(),
        "CREATE INDEX IF NOT EXISTS entries_by_updated ON entries (updated_us DESC)".into(),
    ]
}

/// Version 2: the task domain -- projects, tasks and time blocks.
///
/// No foreign keys between the three, deliberately. `project_id` and
/// `parent_id` are plain columns because the cascades this domain wants are
/// not the ones `ON DELETE CASCADE` gives: deleting a task must also take
/// the *time blocks* pointing at it, which is a rule about a table the
/// database cannot see the link to (the subject is inside the sealed
/// payload; the columns beside it are a denormalised copy for the indexes).
/// Keeping the whole cascade in one place in Rust is what stops half of it
/// drifting.
fn v2(d: Dialect) -> Vec<String> {
    let (blob, int) = (d.blob(), d.int());
    vec![
        format!(
            "CREATE TABLE IF NOT EXISTS projects (
                 id           TEXT    PRIMARY KEY NOT NULL,
                 status       TEXT    NOT NULL,
                 priority     {int} NOT NULL DEFAULT 0,
                 due_date     TEXT,
                 sort_order   {int} NOT NULL DEFAULT 0,
                 created_us   {int} NOT NULL,
                 updated_us   {int} NOT NULL,
                 completed_us {int},
                 data         {blob} NOT NULL
             )"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS tasks (
                 id           TEXT    PRIMARY KEY NOT NULL,
                 project_id   TEXT,
                 parent_id    TEXT,
                 status       TEXT    NOT NULL,
                 priority     {int} NOT NULL DEFAULT 0,
                 start_date   TEXT,
                 due_date     TEXT,
                 sort_order   {int} NOT NULL DEFAULT 0,
                 created_us   {int} NOT NULL,
                 updated_us   {int} NOT NULL,
                 completed_us {int},
                 data         {blob} NOT NULL
             )"
        ),
        // A board draws one project's top-level cards, split by status and
        // in manual order: that is this index, exactly.
        "CREATE INDEX IF NOT EXISTS tasks_by_project ON tasks (project_id, status, sort_order)"
            .into(),
        // Expanding a task to show its subtasks.
        "CREATE INDEX IF NOT EXISTS tasks_by_parent ON tasks (parent_id, sort_order)".into(),
        // "What is due this week", across every project.
        "CREATE INDEX IF NOT EXISTS tasks_by_due ON tasks (due_date, status)".into(),
        format!(
            "CREATE TABLE IF NOT EXISTS time_blocks (
                 id         TEXT    PRIMARY KEY NOT NULL,
                 task_id    TEXT,
                 project_id TEXT,
                 local_date TEXT    NOT NULL,
                 start_us   {int} NOT NULL,
                 end_us     {int} NOT NULL,
                 kind       TEXT    NOT NULL,
                 data       {blob} NOT NULL
             )"
        ),
        // The calendar query: one week, in time order.
        "CREATE INDEX IF NOT EXISTS blocks_by_date ON time_blocks (local_date, start_us)".into(),
        // "How long did this actually take?"
        "CREATE INDEX IF NOT EXISTS blocks_by_task ON time_blocks (task_id, kind)".into(),
    ]
}

/// Version 3: the calendar domain -- subscriptions and their events.
///
/// `events` *does* carry a foreign key, unlike the task tables, and for the
/// reason those do not: here the cascade the database offers is exactly the
/// cascade wanted. Unsubscribing takes the feed's events and nothing else,
/// because nothing else in the vault points at an event -- they are a cache
/// of what a server said, not a record anyone linked to.
///
/// Two date columns rather than one. A week query has to find a fortnight in
/// Lisbon while standing in the middle of it, so the window test is an
/// overlap -- `end_date >= from AND local_date <= to` -- and both sides of it
/// need an index to sit on.
fn v3(d: Dialect) -> Vec<String> {
    let (blob, int, boolean) = (d.blob(), d.int(), d.boolean());
    let (f, t) = (d.bool_default(false), d.bool_default(true));
    vec![
        format!(
            "CREATE TABLE IF NOT EXISTS calendars (
                 id          TEXT    PRIMARY KEY NOT NULL,
                 visible     {boolean} NOT NULL DEFAULT {t},
                 created_us  {int} NOT NULL,
                 updated_us  {int} NOT NULL,
                 synced_us   {int},
                 data        {blob} NOT NULL
             )"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS events (
                 id          TEXT    PRIMARY KEY NOT NULL,
                 calendar_id TEXT    NOT NULL
                                     REFERENCES calendars (id) ON DELETE CASCADE,
                 local_date  TEXT    NOT NULL,
                 end_date    TEXT    NOT NULL,
                 start_us    {int} NOT NULL,
                 end_us      {int} NOT NULL,
                 all_day     {boolean} NOT NULL DEFAULT {f},
                 data        {blob} NOT NULL
             )"
        ),
        // The calendar query: everything touching a window, in time order.
        "CREATE INDEX IF NOT EXISTS events_by_window ON events (end_date, local_date, start_us)"
            .into(),
        // A sync replaces one feed's rows, and the sidebar counts them.
        "CREATE INDEX IF NOT EXISTS events_by_calendar ON events (calendar_id)".into(),
    ]
}

/// Version 4: the library domain -- shelves, the things on them, and the log.
///
/// Foreign keys, as the calendar tables have and the task tables do not, and
/// for the calendar's reason: here the cascade the database offers is exactly
/// the cascade wanted, and it reaches nothing outside these three tables.
/// Deleting a shelf takes its items, deleting an item takes its log, and
/// nothing else in the vault points at either. What is deliberately *not*
/// cascaded is a cover: covers are content-addressed blobs, shared between
/// records, and are reclaimed by the ordinary garbage collector on its grace
/// period rather than deleted by whoever dropped the last reference.
///
/// Note what stays in the clear, which is only what an index is built from:
/// `kind_id`, `status`, `rating`, `favourite`, `finished_on`, `year`, and a
/// log row's `date` and `event`. Titles, creators, notes, tags, summaries,
/// addresses, cover ids and the *names of the shelves themselves* are all
/// sealed -- so the database says that somebody rated eleven things highly
/// in March and never what any of them were.
fn v4(d: Dialect) -> Vec<String> {
    let (blob, int, boolean) = (d.blob(), d.int(), d.boolean());
    let (f, t) = (d.bool_default(false), d.bool_default(true));
    vec![
        format!(
            "CREATE TABLE IF NOT EXISTS kinds (
                 id          TEXT    PRIMARY KEY NOT NULL,
                 sort_order  {int} NOT NULL DEFAULT 0,
                 visible     {boolean} NOT NULL DEFAULT {t},
                 created_us  {int} NOT NULL,
                 updated_us  {int} NOT NULL,
                 data        {blob} NOT NULL
             )"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS items (
                 id           TEXT    PRIMARY KEY NOT NULL,
                 kind_id      TEXT    NOT NULL
                                      REFERENCES kinds (id) ON DELETE CASCADE,
                 status       TEXT    NOT NULL,
                 rating       {int},
                 favourite    {boolean} NOT NULL DEFAULT {f},
                 year         {int},
                 started_on   TEXT,
                 finished_on  TEXT,
                 sort_order   {int} NOT NULL DEFAULT 0,
                 created_us   {int} NOT NULL,
                 updated_us   {int} NOT NULL,
                 data         {blob} NOT NULL
             )"
        ),
        // A shelf draws one kind, split by status, in manual order: that is
        // this index, exactly.
        "CREATE INDEX IF NOT EXISTS items_by_kind ON items (kind_id, status, sort_order)".into(),
        // "What did I get through this year", across every shelf.
        "CREATE INDEX IF NOT EXISTS items_by_finished ON items (finished_on)".into(),
        "CREATE INDEX IF NOT EXISTS items_by_updated ON items (updated_us DESC)".into(),
        format!(
            "CREATE TABLE IF NOT EXISTS logs (
                 id          TEXT    PRIMARY KEY NOT NULL,
                 item_id     TEXT    NOT NULL
                                     REFERENCES items (id) ON DELETE CASCADE,
                 event       TEXT    NOT NULL,
                 local_date  TEXT    NOT NULL,
                 created_us  {int} NOT NULL,
                 data        {blob} NOT NULL
             )"
        ),
        // One item's history, newest first.
        "CREATE INDEX IF NOT EXISTS logs_by_item ON logs (item_id, local_date DESC)".into(),
        // The year in review: every completion in a window.
        "CREATE INDEX IF NOT EXISTS logs_by_date ON logs (local_date, event)".into(),
    ]
}

/// Version 5: the tracking domain -- the readings a journal's trackers made.
///
/// One table, and no table for the trackers themselves: a `Tracker` is a
/// setting of a journal and is saved inside that journal's sealed payload,
/// which is what keeps "sertraline" out of the database and out of this
/// schema. What lands here is the stream: which tracker, which day, at what
/// time, how much.
///
/// `value` is a clear real column, and that is the whole point of the design.
/// A year of readings is thousands of rows whose entire purpose is to be
/// summed, averaged and counted; sealing the number would make every chart a
/// full decrypt of the vault. What the column leaks is that tracker `7f3a...`
/// was `500` at 08:12 -- never that `7f3a...` is a drug, because the name it
/// maps to is sealed one table over.
///
/// `at_us` is nullable, and deliberately so. A reading always knows its day
/// and only sometimes its minute: ticking "flossed" while writing up
/// yesterday says something true about yesterday and nothing about 23:04.
/// A defaulted timestamp there would put a pin on the calendar at an hour
/// nothing happened and skew the first question anyone asks of this data --
/// *when* do the migraines start -- so the unknown is stored as an unknown
/// and `WHERE at_us IS NOT NULL` is how an hour-of-day query says what it
/// means.
///
/// Four indexes, one per question actually asked:
///
/// | Index | Answers |
/// |---|---|
/// | `readings_by_tracker` | "this tracker, over this year" -- every chart |
/// | `readings_by_day` | "everything on these seven days" -- the calendar |
/// | `readings_by_journal` | "this journal, today" -- the chips under an entry |
/// | `readings_by_entry` | detaching readings from an entry being deleted |
fn v5(d: Dialect) -> Vec<String> {
    let (blob, int, real) = (d.blob(), d.int(), d.real());
    vec![
        format!(
            "CREATE TABLE IF NOT EXISTS readings (
                 id          TEXT    PRIMARY KEY NOT NULL,
                 journal_id  TEXT    NOT NULL,
                 tracker_id  TEXT    NOT NULL,
                 entry_id    TEXT,
                 local_date  TEXT    NOT NULL,
                 at_us       {int},
                 value       {real} NOT NULL,
                 created_us  {int} NOT NULL,
                 updated_us  {int} NOT NULL,
                 data        {blob} NOT NULL
             )"
        ),
        "CREATE INDEX IF NOT EXISTS readings_by_tracker
             ON readings (tracker_id, local_date, at_us)"
            .into(),
        "CREATE INDEX IF NOT EXISTS readings_by_day ON readings (local_date, at_us)".into(),
        "CREATE INDEX IF NOT EXISTS readings_by_journal ON readings (journal_id, local_date)"
            .into(),
        "CREATE INDEX IF NOT EXISTS readings_by_entry ON readings (entry_id)".into(),
    ]
}

/// Version 6: the assistant -- its configuration, its threads, and what it
/// was asked to remember.
///
/// The clear/sealed split every other version of this schema makes does not
/// appear here, and its absence is the design. Versions 4 and 5 keep
/// `status`, `rating`, `local_date` and `value` readable because an index on
/// them is what makes a shelf or a chart one query instead of a full
/// decrypt. Nothing in this domain is ever asked that sort of question: the
/// panel wants one thread in order and the history list wants the newest
/// threads, and `conversation_id` plus a timestamp answer both. So there is
/// no reason to leave a single word of a conversation outside the envelope,
/// and none is left. The database records that fourteen messages were
/// exchanged on the 3rd and nothing at all about them -- which matters more
/// here than anywhere else in the schema, because on the Postgres backend
/// "the database" is a machine somebody else runs.
///
/// Two singleton tables, each pinned to one row by a `CHECK`. `agent_secret`
/// is separate from `agent_settings` rather than a column in it so that
/// reading the configuration -- which happens on every panel open -- cannot
/// pick up the API key on the way past. They are sealed under different
/// associated data for the same reason, so neither can be substituted for
/// the other by anyone who can write to the database.
///
/// `messages` cascades from `conversations`, as `events` does from
/// `calendars` and for the same reason: deleting a thread takes its turns
/// and nothing else, because nothing else in the vault points at a message.
/// What deliberately does *not* cascade is `memories`. A memory holds a
/// `source_id` naming the conversation that taught it, and that pointer is
/// allowed to dangle -- clearing your chat history must not silently make
/// the assistant forget that you plan on Sundays. Which is also why
/// `source_id` is not a column here at all: it lives inside the sealed
/// payload, where a foreign key cannot reach it and try to enforce the
/// cascade this domain does not want.
fn v6(d: Dialect) -> Vec<String> {
    let (blob, int) = (d.blob(), d.int());
    vec![
        format!(
            "CREATE TABLE IF NOT EXISTS agent_settings (
                 id          {int} PRIMARY KEY CHECK (id = 1),
                 data        {blob} NOT NULL
             )"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS agent_secret (
                 id          {int} PRIMARY KEY CHECK (id = 1),
                 data        {blob} NOT NULL
             )"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS conversations (
                 id          TEXT    PRIMARY KEY NOT NULL,
                 created_us  {int} NOT NULL,
                 updated_us  {int} NOT NULL,
                 data        {blob} NOT NULL
             )"
        ),
        // The history pane: most recently used first.
        "CREATE INDEX IF NOT EXISTS conversations_by_updated
             ON conversations (updated_us DESC)"
            .into(),
        format!(
            "CREATE TABLE IF NOT EXISTS messages (
                 id              TEXT    PRIMARY KEY NOT NULL,
                 conversation_id TEXT    NOT NULL
                                         REFERENCES conversations (id) ON DELETE CASCADE,
                 created_us      {int} NOT NULL,
                 data            {blob} NOT NULL
             )"
        ),
        // One thread, in order. `id` breaks ties because a tool result and
        // the turn that asked for it can land in the same microsecond, and
        // replaying them the wrong way round makes a model re-run the call.
        // Ids are UUIDv7, so ordering by one orders by time anyway.
        "CREATE INDEX IF NOT EXISTS messages_by_conversation
             ON messages (conversation_id, created_us, id)"
            .into(),
        format!(
            "CREATE TABLE IF NOT EXISTS memories (
                 id          TEXT    PRIMARY KEY NOT NULL,
                 created_us  {int} NOT NULL,
                 data        {blob} NOT NULL
             )"
        ),
        "CREATE INDEX IF NOT EXISTS memories_by_created ON memories (created_us)".into(),
    ]
}

/// Version 7: purpose -- the roles you play, the goals under them, and the
/// pointer every other record now carries.
///
/// The pointer is a **side table** rather than a pair of columns on each of
/// the five tables that can carry one, and that is worth explaining because
/// two columns on `tasks` would obviously be faster to read.
///
/// Every step in this file is additive and idempotent: `CREATE TABLE IF NOT
/// EXISTS` and nothing else. That is not a stylistic preference, it is what
/// makes [`migrate`] safe to re-run, and it has to be re-runnable because the
/// recorded version is only advanced once *every* step has landed -- so a
/// process that dies between a step's commit and that final write replays the
/// step on the next open. `ALTER TABLE ... ADD COLUMN` cannot be written
/// idempotently in SQL both engines accept: Postgres has `IF NOT EXISTS` and
/// SQLite does not, and a step that differs between the two dialects is
/// exactly what `the_two_dialects_agree_about_everything_but_types` exists to
/// prevent. So the pointer goes in a table of its own, where creating it is
/// `IF NOT EXISTS` like everything else here.
///
/// The cost is three index lookups in
/// [`time_by_purpose`](everyday_core::store::purpose::PurposeStore::time_by_purpose)
/// instead of three column reads. The table is keyed by `(record_kind,
/// record_id)` and holds a row only for records that actually carry a
/// purpose, which in a real vault is a handful of projects and almost
/// nothing else -- the whole point of inheritance is that you set it once
/// high up. A join against a few hundred rows on a covering primary key is
/// not the thing that will make that report slow.
///
/// A calendar's role lives here too, as a row whose `purpose_kind` is
/// `role` -- a feed serves a role rather than one outcome, and giving it the
/// same home as everything else means one table to sweep and one shape to
/// reason about instead of a sixth column somewhere.
///
/// The sealed payload remains the source of truth for a record's purpose;
/// this table is an *index* over what those payloads say, in the same sense
/// that `entries.local_date` is an index over what the entry says. Reading
/// one task never touches it. Only the reports do.
///
/// `trackers` is the other half of a change that started in version 5. The
/// readings have been a table since then; their definitions were a field
/// inside the sealed journal record, which is what made a tracker belong to
/// one journal. Here they become records of their own, so a habit can be the
/// measure of a goal and can be shown on whichever journals you like.
///
/// The clear/sealed split is the usual one. What the new tables leave
/// readable is what an index is built from -- which role a goal is under,
/// its status, its horizon, the ordering, the archived flag. Every word is
/// sealed. The file can say that goal `7f3a` is active under role `91c0`
/// and took three hours on Tuesday; it cannot say that `91c0` is "parent".
fn v7(d: Dialect) -> Vec<String> {
    let (blob, int, boolean, f) = (d.blob(), d.int(), d.boolean(), d.bool_default(false));
    vec![
        format!(
            "CREATE TABLE IF NOT EXISTS roles (
                 id          TEXT    PRIMARY KEY NOT NULL,
                 archived    {boolean} NOT NULL DEFAULT {f},
                 sort_order  {int} NOT NULL DEFAULT 0,
                 created_us  {int} NOT NULL,
                 updated_us  {int} NOT NULL,
                 data        {blob} NOT NULL
             )"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS goals (
                 id           TEXT    PRIMARY KEY NOT NULL,
                 role_id      TEXT    NOT NULL,
                 status       TEXT    NOT NULL,
                 horizon      TEXT,
                 sort_order   {int} NOT NULL DEFAULT 0,
                 created_us   {int} NOT NULL,
                 updated_us   {int} NOT NULL,
                 completed_us {int},
                 data         {blob} NOT NULL
             )"
        ),
        // The Overview's sidebar: one role's goals, open ones first. There
        // is deliberately no foreign key to `roles` -- a reference invites
        // the cascade this domain refuses. See `PurposeStore::delete_role`.
        "CREATE INDEX IF NOT EXISTS goals_by_role ON goals (role_id, status)".into(),
        // The pointer. One row per record that carries one; no row at all is
        // the common case and means unattributed.
        "CREATE TABLE IF NOT EXISTS purposes (
                 record_kind  TEXT NOT NULL,
                 record_id    TEXT NOT NULL,
                 purpose_kind TEXT NOT NULL,
                 purpose_id   TEXT NOT NULL,
                 PRIMARY KEY (record_kind, record_id)
             )"
        .into(),
        // The reverse direction: everything filed against one goal, which is
        // what `goal_activity` walks.
        "CREATE INDEX IF NOT EXISTS purposes_by_target
             ON purposes (purpose_kind, purpose_id, record_kind)"
            .into(),
        // Tracker definitions, out of the journal that used to hold them.
        format!(
            "CREATE TABLE IF NOT EXISTS trackers (
                 id           TEXT    PRIMARY KEY NOT NULL,
                 archived     {boolean} NOT NULL DEFAULT {f},
                 sort_order   {int} NOT NULL DEFAULT 0,
                 created_us   {int} NOT NULL,
                 updated_us   {int} NOT NULL,
                 data         {blob} NOT NULL
             )"
        ),
        "CREATE INDEX IF NOT EXISTS trackers_by_order ON trackers (sort_order, created_us)".into(),
        // `readings`, rebuilt so a reading need not name a journal.
        //
        // Version 5 declared `journal_id` `NOT NULL` because a tracker was a
        // field inside one journal's sealed record, so a reading always had
        // one. Trackers are their own records from this version on, and a
        // reading logged from the Overview -- from no journal page at all --
        // has no journal to name. SQLite cannot drop a `NOT NULL`, so the
        // column is widened the only way it can be.
        //
        // This is the one place in the file that does not merely add, and it
        // is still idempotent, which is what the rest of the file's rule
        // actually asks for. Replayed against a database where it has
        // already run: the create is a no-op on a table that was renamed
        // away and so is made afresh, the insert copies out of the current
        // `readings`, the drop takes it, and the rename puts the copy back
        // -- the same state again. The interrupted-halfway case cannot
        // arise, because the whole step is one transaction and DDL is
        // transactional in both engines.
        //
        // Postgres would accept `ALTER COLUMN ... DROP NOT NULL` and does
        // not get it: one shape of the schema in both engines is worth more
        // than one statement saved, and the drift guard below is what keeps
        // that true.
        format!(
            "CREATE TABLE IF NOT EXISTS readings_v7 (
                 id          TEXT    PRIMARY KEY NOT NULL,
                 journal_id  TEXT,
                 tracker_id  TEXT    NOT NULL,
                 entry_id    TEXT,
                 local_date  TEXT    NOT NULL,
                 at_us       {int},
                 value       {real} NOT NULL,
                 created_us  {int} NOT NULL,
                 updated_us  {int} NOT NULL,
                 data        {blob} NOT NULL
             )",
            real = d.real()
        ),
        // Every column named on both sides, so a later reordering of one
        // cannot silently shift the data into the wrong columns.
        "INSERT INTO readings_v7
             (id, journal_id, tracker_id, entry_id, local_date, at_us, value,
              created_us, updated_us, data)
         SELECT id, journal_id, tracker_id, entry_id, local_date, at_us, value,
                created_us, updated_us, data
         FROM readings"
            .into(),
        "DROP TABLE IF EXISTS readings".into(),
        "ALTER TABLE readings_v7 RENAME TO readings".into(),
        // Rebuilt after the copy rather than before it: a bulk insert into
        // an unindexed table is one append per row instead of four B-tree
        // updates.
        "CREATE INDEX IF NOT EXISTS readings_by_tracker
             ON readings (tracker_id, local_date, at_us)"
            .into(),
        "CREATE INDEX IF NOT EXISTS readings_by_day ON readings (local_date, at_us)".into(),
        "CREATE INDEX IF NOT EXISTS readings_by_journal ON readings (journal_id, local_date)"
            .into(),
        "CREATE INDEX IF NOT EXISTS readings_by_entry ON readings (entry_id)".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_version_has_a_step() {
        assert_eq!(steps(Dialect::Sqlite).len() as i64, SCHEMA_VERSION);
        assert_eq!(steps(Dialect::Postgres).len() as i64, SCHEMA_VERSION);
    }

    #[test]
    fn the_two_dialects_agree_about_everything_but_types() {
        // The check that keeps the schemas from drifting: strip the type
        // words out of both and what is left must be identical, table for
        // table, column for column, index for index.
        let normalise = |sqls: Vec<Vec<String>>| {
            sqls.concat()
                .join("\n")
                .replace("BYTEA", "@blob")
                .replace("BLOB", "@blob")
                .replace("DOUBLE PRECISION", "@real")
                .replace("REAL", "@real")
                .replace("BOOLEAN", "@int")
                .replace("BIGINT", "@int")
                .replace("INTEGER", "@int")
                .replace("TRUE", "@true")
                .replace("FALSE", "@false")
                .replace("DEFAULT 1", "DEFAULT @true")
                .replace("DEFAULT 0", "DEFAULT @false")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(normalise(steps(Dialect::Sqlite)), normalise(steps(Dialect::Postgres)));
    }

    #[test]
    fn no_query_hides_a_question_mark_in_a_literal() {
        // `Dialect::bind` rewrites `?N` textually, which is only safe while
        // no SQL in this crate has a `?` anywhere but a placeholder. The DDL
        // is the part most likely to grow a CHECK constraint with prose in
        // it, so it is checked here; the domain queries are checked by
        // running the conformance suite against both dialects.
        for sql in steps(Dialect::Postgres).concat().iter().chain(&blobs_table(Dialect::Postgres)) {
            assert!(!sql.contains('?'), "DDL must not contain a question mark: {sql}");
        }
    }

    #[test]
    fn the_blobs_table_is_not_part_of_the_record_schema() {
        // It belongs to the media layer and is created on open, so a vault
        // that keeps its attachments in files never grows one. If it ever
        // moves into a numbered step, this fails and says to think again.
        let ddl = steps(Dialect::Postgres).concat().join(" ");
        assert!(!ddl.contains("blobs"));
    }
}
