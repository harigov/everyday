//! The shared conformance suite, against a real Postgres server.
//!
//! Every other test in this crate is about strings — a schema name, a
//! placeholder rewrite, a settings spec — because those can be checked
//! without a database. This one is the check that matters: the same battery
//! `everyday-store-sqlite` passes, run against Postgres, so "one set of
//! queries for both databases" is a tested claim rather than a hope.
//!
//! It needs a server, so it is opt-in:
//!
//! ```sh
//! export EVERYDAY_TEST_DATABASE_URL=postgresql://localhost:5432/everyday_test
//! cargo test -p everyday-store-postgres
//! ```
//!
//! Without that variable the test prints why it did nothing and passes.
//! A silently skipped suite is worse than none, so it says so.
//!
//! **The database is emptied.** Each test drops and recreates its own schema,
//! so point this at a scratch database and never at one with anything in it.

use everyday_core::crypto::{AeadCipher, Cipher, NullCipher, SecretKey};
use everyday_core::store::conformance;
use everyday_core::store::trackers::{ReadingQuery, TrackerStore};
use everyday_core::store::{BackendSettings, JournalStore, StoreContext};
use everyday_core::tracker::Reading;
use everyday_core::{Journal, TrackerId};
use everyday_store_postgres::{PostgresStore, SCHEMA_KEY};
use std::sync::Arc;

/// A store on its own schema, or `None` if no server was configured.
///
/// One schema per test rather than one database: the tests can then run in
/// parallel against a single scratch server, and each starts genuinely empty,
/// which the conformance suite requires.
fn store(schema: &str, encrypted: bool) -> Option<everyday_store_sql::SqlStore> {
    let url = std::env::var("EVERYDAY_TEST_DATABASE_URL").ok().filter(|u| !u.trim().is_empty());
    let Some(url) = url else {
        eprintln!(
            "skipping: set EVERYDAY_TEST_DATABASE_URL to a scratch database to run the \
             Postgres conformance suite"
        );
        return None;
    };

    // A previous run's rows would fail `starts_empty` on the first assertion,
    // and a previous run's *schema* could be a version behind. Dropping both
    // makes each run a first run.
    let mut admin = postgres::Client::connect(&url, postgres::NoTls).expect("connect");
    admin
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .expect("drop the test schema");

    let cipher: Arc<dyn Cipher> = if encrypted {
        Arc::new(AeadCipher::new(&SecretKey::from_bytes([9u8; 32])))
    } else {
        Arc::new(NullCipher)
    };
    let dir = std::env::temp_dir().join(format!("everyday-pg-{schema}"));
    let mut settings = BackendSettings::with_url(&url);
    settings.set(SCHEMA_KEY, schema);
    let ctx = StoreContext::new(dir, cipher).with_settings(settings);
    Some(PostgresStore::open(ctx).expect("open the Postgres store"))
}

#[test]
fn passes_the_shared_conformance_suite_when_encrypted() {
    let Some(store) = store("conformance_sealed", true) else { return };
    conformance::run_all(&store);
}

#[test]
fn passes_the_shared_conformance_suite_unencrypted() {
    let Some(store) = store("conformance_clear", false) else { return };
    conformance::run_all(&store);
}

#[test]
fn attachments_live_in_the_database_and_seek_without_downloading() {
    // The one thing this backend does differently from SQLite: media in a
    // column rather than in a directory. The container format is the same, so
    // a range read is still a range read -- over `substr()` instead of over a
    // `seek`.
    let Some(store) = store("conformance_blobs", true) else { return };

    let payload: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    let id = store.put_blob(&payload).unwrap();

    assert_eq!(store.blob_len(id).unwrap(), payload.len() as u64);
    assert_eq!(store.get_blob(id).unwrap(), payload);
    assert!(store.has_blob(id).unwrap());

    // Windows that straddle chunk boundaries are the ones that go wrong.
    let chunk = everyday_core::blobstore::CHUNK_SIZE as u64;
    for (offset, len) in [
        (0u64, 10u64),
        (chunk - 5, 10),
        (chunk, 100),
        (chunk * 2 + 7, 200_000),
        (payload.len() as u64 - 1, 1),
        (payload.len() as u64 - 1, 500), // clamped past the end
    ] {
        let got = store.get_blob_range(id, offset, len).unwrap();
        let end = ((offset + len) as usize).min(payload.len());
        assert_eq!(got, &payload[offset as usize..end], "range ({offset}, {len}) mismatched");
    }

    // Storing the same bytes twice is the same row, not two.
    assert_eq!(store.put_blob(&payload).unwrap(), id);
    assert_eq!(store.list_blobs().unwrap(), vec![id]);

    store.delete_blob(id).unwrap();
    assert!(!store.has_blob(id).unwrap());
}

#[test]
fn an_untimed_reading_sorts_before_a_timed_one_on_the_same_day() {
    // Postgres sorts NULLs *last* on an ascending column and SQLite sorts
    // them first, so this ordering is the one place the two databases would
    // silently disagree. `NULLS FIRST` is spelled out in the query; this is
    // what proves it.
    let Some(store) = store("conformance_nulls", true) else { return };

    let journal = Journal::new("Health");
    store.put_journal(&journal).unwrap();
    let tracker = TrackerId::new();
    let day = jiff::civil::date(2026, 3, 14);

    let mut timed = Reading::on(tracker, day, 2.0).in_journal(journal.id);
    timed.at = Some("2026-03-14T09:00:00Z".parse().unwrap());
    let untimed = Reading::on(tracker, day, 1.0).in_journal(journal.id);

    store.put_reading(&timed).unwrap();
    store.put_reading(&untimed).unwrap();

    let readings = store.list_readings(&ReadingQuery::default()).unwrap();
    assert_eq!(
        readings.iter().map(|r| r.value).collect::<Vec<_>>(),
        [1.0, 2.0],
        "a reading that never knew its minute belongs at the top of its day"
    );

    // And the aggregate over the same window agrees about the set.
    let days = store.tracker_days(&ReadingQuery::default()).unwrap();
    assert_eq!(days.len(), 1);
    assert_eq!(days[0].count, 2);
    assert_eq!(days[0].sum, 3.0);
    assert_eq!(days[0].first_at, timed.at, "the earliest *known* instant, ignoring the unknown");

    // Deleting the journal detaches its readings rather than taking them,
    // which is the cascade the store owns rather than the database. They
    // belong to the tracker; the journal is only where they were ticked.
    store.delete_journal(journal.id).unwrap();
    let left = store.list_readings(&ReadingQuery::default()).unwrap();
    assert_eq!(left.len(), 2, "readings outlive the journal they were logged in");
    assert!(left.iter().all(|r| r.journal_id.is_none()), "and the link is cleared on both");
}
