//! A vault made before the rename from Every Day, which named no schema and
//! so kept its tables in the default of the time, `everyday`: read where it
//! is, by any process, and never renamed -- another machine sharing the
//! database may still be reading it there. A database with nothing under the
//! old name gets the new default. See `default_schema` in the crate.
//!
//! Skipped unless `KARMAYOGA_TEST_DATABASE_URL` names a scratch database, like
//! the conformance suite. The only test that uses the default schemas, so it
//! never runs beside anything else that does.

use karmayoga_core::Journal;
use karmayoga_core::crypto::{Cipher, NullCipher};
use karmayoga_core::store::{BackendSettings, JournalStore, StoreContext};
use karmayoga_store_postgres::{DEFAULT_SCHEMA, PostgresStore, SCHEMA_KEY};
use std::sync::Arc;

fn schemas(admin: &mut postgres::Client) -> Vec<String> {
    admin
        .query(
            "SELECT nspname FROM pg_namespace WHERE nspname IN ('everyday', 'karmayoga') ORDER BY 1",
            &[],
        )
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
}

#[test]
fn a_vault_from_before_the_rename_keeps_its_tables_where_they_are() {
    let Some(url) =
        std::env::var("KARMAYOGA_TEST_DATABASE_URL").ok().filter(|u| !u.trim().is_empty())
    else {
        eprintln!("skipping: set KARMAYOGA_TEST_DATABASE_URL to a scratch database");
        return;
    };
    let mut admin = postgres::Client::connect(&url, postgres::NoTls).expect("connect");
    let drop_both =
        "DROP SCHEMA IF EXISTS everyday CASCADE; DROP SCHEMA IF EXISTS karmayoga CASCADE";
    admin.batch_execute(drop_both).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let cipher: Arc<dyn Cipher> = Arc::new(NullCipher);
    let ctx = |settings: BackendSettings| {
        StoreContext::new(dir.path(), cipher.clone()).with_settings(settings)
    };

    // What an Every Day build did with no schema named.
    let mut old = BackendSettings::with_url(&url);
    old.set(SCHEMA_KEY, "everyday");
    let journal = Journal::new("Morning pages");
    PostgresStore::open(ctx(old)).unwrap().put_journal(&journal).unwrap();
    assert_eq!(schemas(&mut admin), ["everyday"]);

    // Read where it is, whether or not this process holds the vault.
    let unnamed = BackendSettings::with_url(&url);
    for exclusive in [false, true] {
        let store = PostgresStore::open(ctx(unnamed.clone()).exclusive(exclusive)).unwrap();
        assert_eq!(store.list_journals().unwrap()[0].id, journal.id);
        drop(store);
        assert_eq!(schemas(&mut admin), ["everyday"], "nothing was renamed or added");
    }

    // A database with nothing under the old name gets the new default.
    admin.batch_execute(drop_both).unwrap();
    let store = PostgresStore::open(ctx(unnamed)).unwrap();
    assert!(store.list_journals().unwrap().is_empty());
    drop(store);
    assert_eq!(schemas(&mut admin), [DEFAULT_SCHEMA]);

    admin.batch_execute(drop_both).unwrap();
}
