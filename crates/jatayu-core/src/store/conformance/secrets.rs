//! The per-record secret store's half of the conformance suite.

use crate::store::secrets::SecretStore;

/// Run the suite. Called by [`run_all`] when the backend has a
/// [`SecretStore`]; public so a backend under construction can run it alone.
///
/// The store must hold no secrets on entry; it is left that way on success.
pub fn run_secret_suite(store: &dyn SecretStore) {
    eprintln!("--- secret conformance suite ---");

    an_owner_with_no_secret_has_none(store);
    put_get_delete_round_trips(store);
    storing_twice_replaces_rather_than_accumulates(store);
    two_owners_of_the_same_kind_do_not_collide(store);
    a_secret_cannot_be_opened_under_a_different_owner(store);
    deleting_a_missing_secret_is_not_an_error(store);
    bytes_survive_a_round_trip_whatever_they_hold(store);

    eprintln!("--- secret suite passed ---");
}

fn an_owner_with_no_secret_has_none(store: &dyn SecretStore) {
    assert!(store.get_secret("account", "never-stored").unwrap().is_none());
}

fn put_get_delete_round_trips(store: &dyn SecretStore) {
    store.put_secret("account", "acc-1", b"refresh-token-xyz").unwrap();
    assert_eq!(
        store.get_secret("account", "acc-1").unwrap().as_deref(),
        Some(b"refresh-token-xyz".as_slice())
    );

    store.delete_secret("account", "acc-1").unwrap();
    assert!(store.get_secret("account", "acc-1").unwrap().is_none());
}

fn storing_twice_replaces_rather_than_accumulates(store: &dyn SecretStore) {
    store.put_secret("account", "acc-2", b"first").unwrap();
    store.put_secret("account", "acc-2", b"second").unwrap();
    assert_eq!(
        store.get_secret("account", "acc-2").unwrap().as_deref(),
        Some(b"second".as_slice())
    );
    store.delete_secret("account", "acc-2").unwrap();
}

fn two_owners_of_the_same_kind_do_not_collide(store: &dyn SecretStore) {
    store.put_secret("account", "acc-a", b"secret-a").unwrap();
    store.put_secret("account", "acc-b", b"secret-b").unwrap();

    assert_eq!(
        store.get_secret("account", "acc-a").unwrap().as_deref(),
        Some(b"secret-a".as_slice())
    );
    assert_eq!(
        store.get_secret("account", "acc-b").unwrap().as_deref(),
        Some(b"secret-b".as_slice())
    );

    store.delete_secret("account", "acc-a").unwrap();
    // Deleting one owner's secret must not touch another's, even one sharing
    // every character but the id.
    assert_eq!(
        store.get_secret("account", "acc-b").unwrap().as_deref(),
        Some(b"secret-b".as_slice())
    );
    store.delete_secret("account", "acc-b").unwrap();
}

/// The whole point of binding both halves of the owner's name into the
/// associated data: a row that decrypted under any owner but the one it was
/// sealed for would let an attacker with database access -- but not the
/// vault's key -- move a credential from one record to another and have it
/// open there. This cannot be tested through the trait alone, since
/// `put_secret`/`get_secret` never let a caller ask "open this row as if it
/// belonged to someone else". What the trait *can* promise, and what this
/// checks, is the externally observable half of that guarantee: two owners
/// that differ only in kind, or only in id, read back exactly what was
/// written for each and never what was written for the other.
fn a_secret_cannot_be_opened_under_a_different_owner(store: &dyn SecretStore) {
    store.put_secret("account", "shared-id", b"account secret").unwrap();
    store.put_secret("mailbox", "shared-id", b"mailbox secret").unwrap();

    assert_eq!(
        store.get_secret("account", "shared-id").unwrap().as_deref(),
        Some(b"account secret".as_slice()),
        "two owners sharing an id but not a kind must not share a secret"
    );
    assert_eq!(
        store.get_secret("mailbox", "shared-id").unwrap().as_deref(),
        Some(b"mailbox secret".as_slice())
    );

    store.delete_secret("account", "shared-id").unwrap();
    store.delete_secret("mailbox", "shared-id").unwrap();
}

fn deleting_a_missing_secret_is_not_an_error(store: &dyn SecretStore) {
    store.delete_secret("account", "was-never-there").unwrap();
}

fn bytes_survive_a_round_trip_whatever_they_hold(store: &dyn SecretStore) {
    // Not a string: a secret is opaque bytes, and this backend must not
    // assume UTF-8, JSON, or any encoding at all.
    let bytes: Vec<u8> = (0..=255u8).collect();
    store.put_secret("account", "binary", &bytes).unwrap();
    assert_eq!(store.get_secret("account", "binary").unwrap(), Some(bytes));
    store.delete_secret("account", "binary").unwrap();
}
