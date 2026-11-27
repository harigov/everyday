//! A vault made by a build under an earlier name, which named no schema and
//! so kept its tables in the default of the time -- `everyday`, then
//! `karmayoga`: read where it is, by any process, and never renamed --
//! another machine sharing the database may still be reading it there. With
//! both, the newer is the one in use. A database with nothing under an
//! earlier name gets the new default. See `default_schema` in the crate.
//!
//! Skipped unless `JATAYU_TEST_DATABASE_URL` names a scratch database, like
//! the conformance suite. The only test that uses the default schemas, so it
//! never runs beside anything else that does.

use jatayu_core::Journal;
use jatayu_core::crypto::{Cipher, NullCipher};
use jatayu_core::store::{BackendSettings, JournalStore, StoreContext};
use jatayu_store_postgres::{DEFAULT_SCHEMA, PostgresStore, SCHEMA_KEY};
use std::sync::Arc;

fn schemas(admin: &mut postgres::Client) -> Vec<String> {
    admin
        .query(
            "SELECT nspname::text FROM pg_namespace \
             WHERE nspname IN ('everyday', 'karmayoga', 'jatayu') ORDER BY 1",
            &[],
        )
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect()
}

#[test]
fn a_vault_from_before_the_rename_keeps_its_tables_where_they_are() {
    let Some(url) = std::env::var("JATAYU_TEST_DATABASE_URL").ok().filter(|u| !u.trim().is_empty())
    else {
        eprintln!("skipping: set JATAYU_TEST_DATABASE_URL to a scratch database");
        return;
    };
    let mut admin = postgres::Client::connect(&url, postgres::NoTls).expect("connect");
    let drop_all = "DROP SCHEMA IF EXISTS everyday CASCADE; \
                    DROP SCHEMA IF EXISTS karmayoga CASCADE; \
                    DROP SCHEMA IF EXISTS jatayu CASCADE";
    admin.batch_execute(drop_all).unwrap();

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

    // What a Karma Yoga build did, on a database that had Every Day's
    // tables too: its own schema is the one in use, and the older one is
    // left as it is.
    let mut karma_yoga = BackendSettings::with_url(&url);
    karma_yoga.set(SCHEMA_KEY, "karmayoga");
    let newer = Journal::new("Evening pages");
    PostgresStore::open(ctx(karma_yoga)).unwrap().put_journal(&newer).unwrap();
    assert_eq!(schemas(&mut admin), ["everyday", "karmayoga"]);
    for exclusive in [false, true] {
        let store = PostgresStore::open(ctx(unnamed.clone()).exclusive(exclusive)).unwrap();
        let journals = store.list_journals().unwrap();
        assert_eq!(journals.len(), 1);
        assert_eq!(journals[0].id, newer.id);
        drop(store);
        assert_eq!(schemas(&mut admin), ["everyday", "karmayoga"], "nothing was renamed or added");
    }

    // A database with nothing under an earlier name gets the new default.
    admin.batch_execute(drop_all).unwrap();
    let store = PostgresStore::open(ctx(unnamed)).unwrap();
    assert!(store.list_journals().unwrap().is_empty());
    drop(store);
    assert_eq!(schemas(&mut admin), [DEFAULT_SCHEMA]);

    admin.batch_execute(drop_all).unwrap();
}
