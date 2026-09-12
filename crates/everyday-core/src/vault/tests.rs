use super::header::from_hex;
use super::*;
use crate::model::{Entry, Journal};
use crate::search::SearchScope;
use crate::store::{EntryQuery, SortOrder, conformance};
use crate::testing::registry;

fn cfg(password: Option<&str>) -> VaultConfig {
    VaultConfig {
        name: "Test".into(),
        backend: "memory".into(),
        settings: BackendSettings::default(),
        password: password.map(str::to_string),
        kdf: KdfParams::insecure_fast(),
        auto_lock_seconds: 0,
        forget_key_seconds: 0,
    }
}

#[test]
fn the_in_memory_backend_passes_the_conformance_suite() {
    // Proves the suite itself is satisfiable, and exercises it under the
    // real encrypting cipher.
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    v.with_store(|s| {
        conformance::run_all(s);
        Ok(())
    })
    .unwrap();
}

#[test]
fn the_assistant_is_offered_only_the_tools_the_backend_can_serve() {
    // The backend decides the catalogue, so a model is never told about
    // a tool whose storage does not exist and cannot then claim to have
    // used one.
    //
    // This lives here, against the in-memory store, because it is the
    // only backend left that holds journals and nothing else. Every one
    // the application ships carries all six domains -- which is exactly
    // why the rule needs a test that does not depend on one of them
    // being poorer than the others, or it goes unexercised until the
    // first backend that is.
    use crate::agent::tools::{self, ToolContext};

    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    assert!(!v.supports_tasks(), "the premise of this test");

    let offered: Vec<&str> = tools::available(&v).iter().map(|t| t.name).collect();
    assert!(offered.contains(&"create_entry"), "journals work on every backend: {offered:?}");
    assert!(!offered.contains(&"create_task"), "this backend stores no tasks: {offered:?}");
    assert!(!offered.contains(&"list_shelves"), "nor a library: {offered:?}");

    // And asking anyway is refused where the model can act on it, rather
    // than failing somewhere deeper with a message about storage.
    let ctx = ToolContext {
        vault: &v,
        today: jiff::civil::Date::constant(2026, 9, 8),
        tz: "UTC",
        conversation: None,
        unattended: false,
    };
    let err = tools::dispatch(&ctx, "create_task", &serde_json::json!({ "title": "x" }))
        .expect_err("a tool the backend cannot serve must be refused");
    assert!(err.to_string().contains("does not store"), "got {err}");
}

#[test]
fn a_new_vault_is_unlocked_and_reports_its_settings() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    let s = v.status();
    assert!(s.unlocked);
    assert!(s.encrypted);
    assert_eq!(s.backend, "memory");
    assert_eq!(s.name, "Test");
    assert!(dir.path().join(HEADER_FILENAME).is_file());
}

#[test]
fn entry_tags_are_counted_most_used_first() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    let j = Journal::new("J");
    v.save_journal(&j).unwrap();

    for tags in [vec!["travel", "spain"], vec!["travel", "food"], vec!["travel"], vec!["food"]] {
        let mut e = Entry::new(j.id, "UTC");
        e.title = "x".into();
        e.tags = tags.into_iter().map(str::to_string).collect();
        v.save_entry(&e, None).unwrap();
    }

    // Most used first; "food" and "spain" tie at the bottom on count and
    // are broken alphabetically, which is what makes the order stable
    // enough to drive an autocomplete.
    assert_eq!(
        v.entry_tags().unwrap(),
        vec![("travel".to_string(), 3), ("food".to_string(), 2), ("spain".to_string(), 1),]
    );
}

#[test]
fn entry_tags_needs_an_unlocked_vault() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    v.lock();
    assert!(matches!(v.entry_tags().unwrap_err(), Error::Locked));
}

#[test]
fn creating_over_an_existing_vault_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    let err = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap_err();
    assert_eq!(err.code(), "already_initialised");
}

#[test]
fn opening_a_missing_vault_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let err = Vault::open(&dir.path().join("nowhere"), registry()).unwrap_err();
    assert_eq!(err.code(), "no_vault");
}

#[test]
fn an_encrypted_vault_reopens_locked_and_needs_the_password() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    {
        let v = Vault::create(dir.path(), cfg(Some("correct horse")), reg.clone()).unwrap();
        let j = Journal::new("Daily");
        v.save_journal(&j).unwrap();
        let mut e = Entry::new(j.id, "UTC");
        e.body = crate::RichDoc::from_plain_text("a secret");
        v.save_entry(&e, None).unwrap();
    }

    let v = Vault::open(dir.path(), reg).unwrap();
    assert!(!v.is_unlocked(), "an encrypted vault must reopen locked");
    assert_eq!(v.journals().unwrap_err().code(), "locked");
    assert_eq!(v.entries(&EntryQuery::default()).unwrap_err().code(), "locked");

    assert_eq!(v.unlock(Some("wrong")).unwrap_err().code(), "bad_password");
    assert_eq!(v.unlock(None).unwrap_err().code(), "bad_password");
    assert!(!v.is_unlocked(), "a failed unlock must not half-open the vault");

    v.unlock(Some("correct horse")).unwrap();
    assert!(v.is_unlocked());
    assert_eq!(v.journals().unwrap().len(), 1);
    assert_eq!(v.search("secret", SearchScope::Everything, 10).unwrap().len(), 1);
}

#[test]
fn locking_denies_access_until_unlocked_again() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    v.save_journal(&Journal::new("Daily")).unwrap();

    v.lock();
    assert!(!v.is_unlocked());
    assert_eq!(v.journals().unwrap_err().code(), "locked");
    assert_eq!(v.stats().unwrap_err().code(), "locked");
    assert_eq!(v.search("x", SearchScope::Everything, 5).unwrap_err().code(), "locked");
    assert_eq!(v.put_blob(b"x").unwrap_err().code(), "locked");

    // Status is still answerable while locked — the UI needs it.
    let s = v.status();
    assert!(!s.unlocked);
    assert!(s.stats.is_none(), "stats must not be readable while locked");

    v.unlock(Some("pw")).unwrap();
    assert_eq!(v.journals().unwrap().len(), 1);
}

#[test]
fn an_unencrypted_vault_opens_without_a_password() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    {
        let v = Vault::create(dir.path(), cfg(None), reg.clone()).unwrap();
        assert!(!v.status().encrypted);
        v.save_journal(&Journal::new("Open")).unwrap();
    }
    let v = Vault::open(dir.path(), reg).unwrap();
    assert!(v.is_unlocked());
    assert_eq!(v.journals().unwrap().len(), 1);
}

#[test]
fn creating_an_encrypted_vault_rejects_an_empty_password() {
    let dir = tempfile::tempdir().unwrap();
    let err = Vault::create(dir.path(), cfg(Some("")), registry()).unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert!(!Vault::exists(dir.path()), "a rejected create must leave no vault behind");
}

#[test]
fn changing_the_password_keeps_the_data_readable() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let v = Vault::create(dir.path(), cfg(Some("old pw")), reg.clone()).unwrap();
    let j = Journal::new("Daily");
    v.save_journal(&j).unwrap();

    assert_eq!(v.change_password(Some("nope"), Some("new pw")).unwrap_err().code(), "bad_password");
    v.change_password(Some("old pw"), Some("new pw")).unwrap();

    v.lock();
    assert_eq!(v.unlock(Some("old pw")).unwrap_err().code(), "bad_password");
    v.unlock(Some("new pw")).unwrap();
    assert_eq!(v.journals().unwrap()[0].name, "Daily");

    // And it survives a full reopen from disk.
    drop(v);
    let v = Vault::open(dir.path(), reg).unwrap();
    v.unlock(Some("new pw")).unwrap();
    assert_eq!(v.journals().unwrap()[0].name, "Daily");
}

#[test]
fn removing_a_password_is_refused_rather_than_silently_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    let err = v.change_password(Some("pw"), None).unwrap_err();
    assert_eq!(err.code(), "unsupported");
    // The old password must still work after the refusal.
    v.lock();
    v.unlock(Some("pw")).unwrap();
}

#[test]
fn on_disk_bytes_do_not_contain_the_plaintext() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    let j = Journal::new("Daily");
    v.save_journal(&j).unwrap();

    // The header is plaintext by design, but must not leak the key or
    // any journal content.
    let header = std::fs::read_to_string(dir.path().join(HEADER_FILENAME)).unwrap();
    assert!(!header.contains("Daily"));
    assert!(header.contains("xchacha20poly1305"));
}

#[test]
fn the_search_index_tracks_edits_and_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    let j = Journal::new("Daily");
    v.save_journal(&j).unwrap();

    let mut e = Entry::new(j.id, "UTC");
    e.body = crate::RichDoc::from_plain_text("kingfisher on the wire");
    v.save_entry(&e, None).unwrap();
    assert_eq!(v.search("kingfisher", SearchScope::Everything, 10).unwrap().len(), 1);

    // An edit, so it carries the version it is replacing. `None` here
    // would be the caller claiming the entry is new, and is a conflict.
    let loaded = e.updated_at;
    e.body = crate::RichDoc::from_plain_text("heron on the wire");
    e.updated_at = Timestamp::now();
    v.save_entry(&e, Some(loaded)).unwrap();
    assert!(
        v.search("kingfisher", SearchScope::Everything, 10).unwrap().is_empty(),
        "edit must reindex"
    );
    assert_eq!(v.search("heron", SearchScope::Everything, 10).unwrap().len(), 1);

    v.delete_entry(e.id).unwrap();
    assert!(
        v.search("heron", SearchScope::Everything, 10).unwrap().is_empty(),
        "delete must deindex"
    );
}

#[test]
fn deleting_a_journal_removes_its_entries_from_the_index() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    let j = Journal::new("Daily");
    v.save_journal(&j).unwrap();
    let mut e = Entry::new(j.id, "UTC");
    e.body = crate::RichDoc::from_plain_text("kingfisher");
    v.save_entry(&e, None).unwrap();

    v.delete_journal(j.id).unwrap();
    assert!(v.search("kingfisher", SearchScope::Everything, 10).unwrap().is_empty());
    assert!(v.entries(&EntryQuery::default()).unwrap().is_empty());
}

#[test]
fn saving_an_entry_rejects_a_malformed_body() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    let j = Journal::new("Daily");
    v.save_journal(&j).unwrap();

    let mut e = Entry::new(j.id, "UTC");
    e.body = crate::RichDoc(serde_json::json!({"type": "paragraph"}));
    assert_eq!(v.save_entry(&e, None).unwrap_err().code(), "invalid");
    assert!(v.entries(&EntryQuery::default()).unwrap().is_empty());
}

#[test]
fn journals_come_back_in_sidebar_order() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    for (order, name) in [(2, "Third"), (0, "First"), (1, "Second")] {
        let mut j = Journal::new(name);
        j.sort_order = order;
        v.save_journal(&j).unwrap();
    }
    let names: Vec<String> = v.journals().unwrap().into_iter().map(|j| j.name).collect();
    assert_eq!(names, ["First", "Second", "Third"]);
}

#[test]
fn a_vault_from_before_the_split_keeps_the_timeout_it_was_left_with() {
    // `auto_lock_seconds` used to drop the key. It now hides a screen, and
    // an upgrade that silently turned fifteen minutes into "never" would
    // have made somebody's vault less careful than they left it.
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    {
        let v = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
        let mut header = v.header();
        header.auto_lock_seconds = 900;
        header.forget_key_seconds = 0;
        header.migrated_lock = None;
        write_header(dir.path(), &header).unwrap();
    }

    let v = Vault::open(dir.path(), reg.clone()).unwrap();
    assert_eq!(v.header().forget_key_seconds, 900, "carried across on the first open");
    assert_eq!(v.header().auto_lock_seconds, 900, "and the screen keeps its own");

    // And "never", chosen afterwards, is not overruled the next time.
    v.unlock(Some("pw")).unwrap();
    v.set_forget_key(0).unwrap();
    drop(v);
    let v = Vault::open(dir.path(), reg).unwrap();
    assert_eq!(v.header().forget_key_seconds, 0, "a choice is a choice, including zero");
}

#[test]
fn forgetting_the_key_is_off_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    assert_eq!(v.header().forget_key_seconds, 0, "a new vault keeps its key");
    assert_eq!(v.seconds_until_forget_key(), None);
    assert!(!v.forget_key_if_idle());
    assert!(v.is_unlocked());
}

#[test]
fn the_key_goes_once_the_idle_timeout_elapses() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    v.set_forget_key(1).unwrap();
    v.touch();
    assert!(!v.forget_key_if_idle(), "should not lock while still fresh");

    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert!(v.forget_key_if_idle(), "should lock after the timeout");
    assert!(!v.is_unlocked());
    // Once locked there is no key left to forget.
    assert_eq!(v.seconds_until_forget_key(), None);
}

#[test]
fn a_person_defers_the_forgetting_and_a_read_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    v.set_forget_key(2).unwrap();
    v.touch();
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(700));
        v.touch(); // what the service does after a command from a person
        assert!(!v.forget_key_if_idle(), "activity should keep the vault open");
    }

    // Reading is not activity. The assistant's scheduler reads this vault
    // every minute; if that counted, the timeout would never fire.
    std::thread::sleep(std::time::Duration::from_millis(1200));
    v.journals().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1200));
    v.journals().unwrap();
    assert!(v.forget_key_if_idle(), "a read must not defer the timeout");
}

#[test]
fn the_right_password_verifies_and_the_wrong_one_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    v.verify_password(Some("pw")).expect("the right password");
    assert_eq!(
        v.verify_password(Some("nope")).unwrap_err().code(),
        "bad_password",
        "the wrong one is refused"
    );
    assert!(v.is_unlocked(), "verifying must not close a vault that was open");

    // And it works from the other side: a locked vault can be asked
    // whether a password is right without being opened by the asking.
    v.lock();
    v.verify_password(Some("pw")).expect("the right password");
    assert!(!v.is_unlocked(), "verifying must not open a vault that was locked");
}

#[test]
fn the_auto_lock_setting_survives_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    {
        let v = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
        v.set_auto_lock(300).unwrap();
    }
    let v = Vault::open(dir.path(), reg).unwrap();
    assert_eq!(v.header().auto_lock_seconds, 300);
}

#[test]
fn a_newer_header_format_is_refused_rather_than_misread() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();

    let path = dir.path().join(HEADER_FILENAME);
    let mut header: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    header["format"] = serde_json::json!(FORMAT_VERSION + 1);
    std::fs::write(&path, serde_json::to_vec(&header).unwrap()).unwrap();

    let err = Vault::open(dir.path(), reg).unwrap_err();
    assert_eq!(err.code(), "unsupported_version");
}

#[test]
fn a_corrupt_wrapped_key_is_reported_as_a_bad_password_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();

    let path = dir.path().join(HEADER_FILENAME);
    let mut header: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut wrapped = header["wrappedKey"].as_str().unwrap().to_string();
    wrapped.replace_range(0..2, "ff");
    header["wrappedKey"] = serde_json::json!(wrapped);
    std::fs::write(&path, serde_json::to_vec(&header).unwrap()).unwrap();

    let v = Vault::open(dir.path(), reg).unwrap();
    assert_eq!(v.unlock(Some("pw")).unwrap_err().code(), "bad_password");
}

#[test]
fn a_lost_header_is_recovered_from_the_backup_copy() {
    // The worst survivable accident: the file holding the wrapped data
    // key is gone. Every entry in the store is still ciphertext under a
    // key that exists nowhere else, so the spare is the difference
    // between a vault that opens and one that never will again.
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let v = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
    let j = Journal::new("J");
    v.save_journal(&j).unwrap();
    // Creating the vault is enough: the spare is seeded on the first
    // header write, not the second.
    assert!(dir.path().join(HEADER_BACKUP_FILENAME).is_file());

    std::fs::remove_file(dir.path().join(HEADER_FILENAME)).unwrap();

    let v = Vault::open(dir.path(), reg).unwrap();
    v.unlock(Some("pw")).unwrap();
    assert_eq!(v.journals().unwrap().len(), 1, "the vault still reads");
    // Recovery also restores the live header, so this is a one-off.
    assert!(dir.path().join(HEADER_FILENAME).is_file());
}

#[test]
fn a_corrupt_header_falls_back_rather_than_reporting_no_vault() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let v = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
    v.set_auto_lock(60).unwrap();

    std::fs::write(dir.path().join(HEADER_FILENAME), b"{ this is not json").unwrap();

    let v = Vault::open(dir.path(), reg).unwrap();
    v.unlock(Some("pw")).unwrap();
    assert!(v.is_unlocked());
}

#[test]
fn a_vault_with_only_a_backup_header_is_not_overwritten_by_create() {
    // `create` refuses an existing vault, and "existing" has to include
    // one whose live header was lost -- otherwise the recovery path above
    // races a fresh vault written on top of the entries it was meant to
    // save.
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let v = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
    v.set_auto_lock(60).unwrap();
    std::fs::remove_file(dir.path().join(HEADER_FILENAME)).unwrap();

    assert_eq!(
        Vault::create(dir.path(), cfg(Some("pw")), reg).unwrap_err().code(),
        "already_initialised"
    );
}

#[test]
fn the_backup_header_is_only_ever_a_readable_one() {
    // A corrupt live header must not be promoted over the last good
    // spare, or one bad write destroys both copies.
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let v = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
    v.set_auto_lock(60).unwrap();
    let good = std::fs::read(dir.path().join(HEADER_BACKUP_FILENAME)).unwrap();

    std::fs::write(dir.path().join(HEADER_FILENAME), b"corrupt").unwrap();
    // Any header write now would otherwise copy the corruption across.
    write_header(dir.path(), &read_header(dir.path()).unwrap()).unwrap();

    assert_eq!(
        std::fs::read(dir.path().join(HEADER_BACKUP_FILENAME)).unwrap(),
        good,
        "the spare must still be the last header that parsed"
    );
}

#[test]
fn a_second_process_opens_read_only_rather_than_racing_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let first = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
    assert!(first.is_writable(), "the creator holds the write lock");

    // The same vault opened again -- another process, in production.
    let second = Vault::open(dir.path(), reg).unwrap();
    second.unlock(Some("pw")).unwrap();
    assert!(!second.is_writable(), "a second opener must not also be a writer");
    assert!(!second.status().writable);

    // It still reads. (What it reads is this backend's business: the
    // in-memory one snapshots its maps per open, so the *cross-instance*
    // visibility of a write is pinned in the SQLite crate, against a
    // store two handles genuinely share.)
    let j = Journal::new("Daily");
    first.save_journal(&j).unwrap();
    second.journals().expect("a read-only vault must still read");

    // And refuses every write, naming what is holding it.
    let err = second.save_journal(&Journal::new("Nope")).unwrap_err();
    assert_eq!(err.code(), "vault_in_use");
    assert!(err.to_string().contains("read-only"), "the message must explain itself");

    let e = Entry::new(j.id, "UTC");
    assert_eq!(second.save_entry(&e, None).unwrap_err().code(), "vault_in_use");
    assert_eq!(second.delete_entry(e.id).unwrap_err().code(), "vault_in_use");
    assert_eq!(second.put_blob(b"x").unwrap_err().code(), "vault_in_use");
    assert_eq!(second.set_auto_lock(30).unwrap_err().code(), "vault_in_use");
    assert_eq!(
        second.collect_garbage(std::time::Duration::ZERO).unwrap_err().code(),
        "vault_in_use"
    );
}

#[test]
fn one_process_reopening_a_path_it_already_holds_gets_a_read_only_vault() {
    // Pinning the behaviour the desktop shell has to work around: the
    // lock is on an open file description, not on a process, so a second
    // `open` of a path this process already holds conflicts with itself.
    // `AppState::close` exists because of this -- the vault in hand has
    // to be released before another is opened, even the same one.
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let held = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
    assert!(held.is_writable());

    let again = Vault::open(dir.path(), reg.clone()).unwrap();
    assert!(!again.is_writable(), "reopening without closing must not get the lock");

    // Release the first, and a fresh open is writable again.
    drop(held);
    drop(again);
    assert!(Vault::open(dir.path(), reg).unwrap().is_writable());
}

#[test]
fn the_write_lock_is_released_when_the_vault_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    {
        let first = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
        assert!(first.is_writable());
    }
    // Closing the app must hand the vault back, or a crash would leave it
    // read-only until the machine was rebooted.
    let next = Vault::open(dir.path(), reg).unwrap();
    assert!(next.is_writable(), "the lock must be reclaimable after a close");
}

#[test]
fn a_read_only_vault_can_still_be_backed_up() {
    // The whole point of degrading to read-only instead of refusing: the
    // things that cannot lose data still work. Backing up is the one that
    // matters most, since it is what someone reaches for when they are
    // worried.
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let _first = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();

    let second = Vault::open(dir.path(), reg).unwrap();
    second.unlock(Some("pw")).unwrap();
    assert!(!second.is_writable());

    // The in-memory backend has no snapshot, so this asserts on *which*
    // error: it must be the backend's limitation, not the write lock.
    let dest = tempfile::tempdir().unwrap();
    assert_eq!(second.backup(&dest.path().join("copy")).unwrap_err().code(), "unsupported");
}

#[test]
fn saving_an_entry_someone_else_changed_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    let j = Journal::new("Daily");
    v.save_journal(&j).unwrap();

    let mut e = Entry::new(j.id, "UTC");
    e.body = crate::RichDoc::from_plain_text("what I wrote");
    v.save_entry(&e, None).unwrap();
    let loaded = e.updated_at;

    // Something else writes it -- the CLI, another machine, a stale tab.
    let mut theirs = e.clone();
    theirs.body = crate::RichDoc::from_plain_text("what they wrote");
    theirs.updated_at = Timestamp::now();
    v.save_entry(&theirs, Some(loaded)).unwrap();

    // Our editor still thinks it holds the current version.
    e.body = crate::RichDoc::from_plain_text("my later paragraph");
    e.updated_at = Timestamp::now();
    assert_eq!(v.save_entry(&e, Some(loaded)).unwrap_err().code(), "conflict");
    assert_eq!(
        v.entry(e.id).unwrap().body.plain_text(),
        "what they wrote",
        "the refused save must not have touched anything"
    );

    // "Keep mine" is the deliberate override, and it also has to put the
    // search index straight.
    v.overwrite_entry(&e).unwrap();
    assert_eq!(v.entry(e.id).unwrap().body.plain_text(), "my later paragraph");
    assert_eq!(v.search("paragraph", SearchScope::Everything, 10).unwrap().len(), 1);
    assert!(
        v.search("they", SearchScope::Everything, 10).unwrap().is_empty(),
        "the index must follow the write"
    );
}

// The backup *round trip* is exercised against SQLite, in that crate:
// the in-memory backend here has no on-disk form to snapshot, so it
// reports `snapshot` as unsupported. What is checkable at this layer is
// the guard in front of it.
#[test]
fn backing_up_over_an_existing_vault_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let v = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
    let other = tempfile::tempdir().unwrap();
    Vault::create(other.path(), cfg(Some("pw")), reg).unwrap();

    assert_eq!(v.backup(other.path()).unwrap_err().code(), "already_initialised");
}

#[test]
fn a_malformed_header_hex_field_is_an_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();

    let path = dir.path().join(HEADER_FILENAME);
    let mut header: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    header["salt"] = serde_json::json!("zzzz");
    std::fs::write(&path, serde_json::to_vec(&header).unwrap()).unwrap();

    let v = Vault::open(dir.path(), reg).unwrap();
    assert_eq!(v.unlock(Some("pw")).unwrap_err().code(), "invalid");
}

#[test]
fn creating_with_an_unknown_backend_fails_before_touching_disk() {
    let dir = tempfile::tempdir().unwrap();
    let bad = VaultConfig { backend: "postgres".into(), ..cfg(Some("pw")) };
    assert_eq!(Vault::create(dir.path(), bad, registry()).unwrap_err().code(), "unknown_backend");
    assert!(!Vault::exists(dir.path()));
}

#[test]
fn entry_queries_are_forwarded_to_the_backend() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    let a = Journal::new("A");
    let b = Journal::new("B");
    v.save_journal(&a).unwrap();
    v.save_journal(&b).unwrap();

    for (j, day) in [(a.id, 1), (a.id, 2), (b.id, 3)] {
        let mut e = Entry::new(j, "UTC");
        e.local_date = jiff::civil::date(2025, 1, day);
        v.save_entry(&e, None).unwrap();
    }
    assert_eq!(v.entries(&EntryQuery::default()).unwrap().len(), 3);
    assert_eq!(v.entries(&EntryQuery::in_journal(a.id)).unwrap().len(), 2);
    let asc = EntryQuery { sort: SortOrder::DateAsc, limit: Some(1), ..Default::default() };
    assert_eq!(v.entries(&asc).unwrap()[0].local_date, jiff::civil::date(2025, 1, 1));
}

#[test]
fn a_backend_without_calendars_says_so_rather_than_failing_obscurely() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    assert!(!v.supports_calendars());
    assert!(!v.status().capabilities.unwrap().calendars);

    assert_eq!(v.calendars().unwrap_err().code(), "unsupported");
    assert_eq!(
        v.events(&crate::store::calendars::EventQuery::default()).unwrap_err().code(),
        "unsupported",
    );
}

#[test]
fn a_calendar_with_an_address_we_would_never_fetch_is_refused_on_the_way_in() {
    // Refused where it is typed, not an hour later by a background sync
    // nobody is watching.
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    let bad = crate::calendar::Calendar::subscribed("Sneaky", "file:///etc/passwd");
    assert_eq!(v.save_calendar(&bad).unwrap_err().code(), "invalid");

    let nameless = crate::calendar::Calendar::subscribed("  ", "https://example.com/x.ics");
    assert_eq!(v.save_calendar(&nameless).unwrap_err().code(), "invalid");
}

#[test]
fn a_reply_that_is_not_a_calendar_never_replaces_one_that_is() {
    // The failure mode of an automatic sync that people actually notice:
    // a captive portal, an expired link, a 200 with an error page in it.
    // The guard has to fire before any store call, which is what this
    // asserts -- the in-memory backend here has no calendar store, so a
    // check made any later would report `unsupported` instead.
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    let err = v
        .sync_calendar_from_ics(
            crate::CalendarId::new(),
            "<!doctype html><title>Sign in to the wifi</title>",
            (jiff::civil::date(2026, 1, 1), jiff::civil::date(2026, 12, 31)),
            "UTC",
        )
        .unwrap_err();
    assert_eq!(err.code(), "invalid", "an HTML page is not a calendar; got {err}");
}

#[test]
fn a_backend_without_tasks_says_so_rather_than_failing_obscurely() {
    // The Markdown vault's situation: journals, and nothing else. The
    // interface reads `supports_tasks` to hide the todo app; a call that
    // slips through anyway must name the reason, not panic or return an
    // empty list that reads as "you have no tasks".
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    assert!(!v.supports_tasks());
    assert!(!v.status().capabilities.unwrap().tasks);

    let err = v.tasks(&crate::store::tasks::TaskQuery::default()).unwrap_err();
    assert_eq!(err.code(), "unsupported", "got {err}");
    assert_eq!(v.projects().unwrap_err().code(), "unsupported");
    assert_eq!(v.task_stats(jiff::civil::date(2026, 3, 10)).unwrap_err().code(), "unsupported");
}

#[test]
fn a_locked_vault_refuses_task_reads_before_it_refuses_the_backend() {
    // "Locked" has to win over "unsupported": which backend is in use is
    // not something a locked vault should be answering questions about.
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    v.lock();
    assert_eq!(v.tasks(&crate::store::tasks::TaskQuery::default()).unwrap_err().code(), "locked");
    assert!(!v.supports_tasks(), "a locked vault supports nothing");
}

#[test]
fn hex_helpers_round_trip_and_reject_junk() {
    let bytes = [0u8, 1, 15, 16, 255];
    assert_eq!(from_hex(&to_hex(&bytes)).unwrap(), bytes);
    assert!(from_hex("abc").is_err(), "odd length");
    assert!(from_hex("zz").is_err(), "non-hex digits");
}
#[test]
fn a_key_kept_in_a_keychain_opens_the_vault_it_came_from() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let key = {
        let v = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
        let mut j = Journal::new("Kept");
        j.description = "written before the restart".into();
        v.save_journal(&j).unwrap();
        v.export_data_key(Some("pw")).unwrap()
    };

    // What a restart looks like: a fresh handle, no password anywhere.
    let v = Vault::open(dir.path(), reg).unwrap();
    assert!(!v.is_unlocked(), "a reopened vault starts shut");
    v.unlock_with_key(key.as_str()).expect("the key from the keychain opens it");
    assert_eq!(v.journals().unwrap()[0].description, "written before the restart");
}

#[test]
fn a_key_that_is_not_the_key_is_refused_even_on_an_empty_vault() {
    // The way this could lose everything. A vault with nothing in it
    // decrypts nothing on open, so before there was a check value any
    // thirty-two bytes would "unlock" it -- and every record written
    // afterwards would be sealed under a key the header does not hold.
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let v = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
    v.lock();
    assert!(v.journals().is_err(), "the vault is shut, and it has no records in it either way");

    let stale = "cd".repeat(32);
    assert_eq!(
        v.unlock_with_key(&stale).unwrap_err().code(),
        "bad_password",
        "a key that is not this vault's must be refused before anything opens"
    );
    assert!(!v.is_unlocked(), "and it must leave the vault shut");

    // The real key still works, and the vault still reads afterwards.
    let key = {
        let real = Vault::open(dir.path(), reg.clone()).unwrap();
        real.unlock(Some("pw")).unwrap();
        real.export_data_key(Some("pw")).unwrap()
    };
    v.unlock_with_key(key.as_str()).expect("its own key opens it");
    v.save_journal(&Journal::new("Written after")).unwrap();
    v.lock();
    v.unlock(Some("pw")).unwrap();
    assert_eq!(
        v.journals().unwrap()[0].name,
        "Written after",
        "the password must still read what the key wrote"
    );
}

#[test]
fn a_vault_written_before_key_checks_gains_one_when_it_is_opened() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry();
    let key = {
        let v = Vault::create(dir.path(), cfg(Some("pw")), reg.clone()).unwrap();
        v.export_data_key(Some("pw")).unwrap()
    };

    // Rewind to the world before the check value existed.
    {
        let v = Vault::open(dir.path(), reg.clone()).unwrap();
        let mut header = v.header();
        header.key_check = None;
        write_header(dir.path(), &header).unwrap();
    }

    // Until it has one, a key alone is refused rather than trusted.
    let v = Vault::open(dir.path(), reg.clone()).unwrap();
    assert!(v.header().key_check.is_none());
    let err = v.unlock_with_key(key.as_str()).unwrap_err();
    assert!(err.to_string().contains("nothing to check"), "got {err}");

    // Opening it with the password writes one, and then it works.
    v.unlock(Some("pw")).unwrap();
    assert!(v.header().key_check.is_some(), "it upgrades itself, quietly");
    v.lock();
    v.unlock_with_key(key.as_str()).expect("now the key is checkable");
}

#[test]
fn a_key_that_is_not_the_key_does_not_open_anything() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(Some("pw")), registry()).unwrap();
    // Something sealed, so there is a tag to fail against.
    v.save_journal(&Journal::new("Sealed")).unwrap();
    v.lock();

    assert!(v.unlock_with_key("not hex").is_err(), "gibberish is refused");
    assert!(v.unlock_with_key(&"aa".repeat(16)).is_err(), "so is a key of the wrong length");

    // The right length and the wrong bytes opens the store -- there is
    // nothing in the header to check a key against, deliberately -- and
    // fails on the first record it reads. That is the same tag that
    // catches a wrong password, and it is the only check there is.
    let wrong = "bb".repeat(32);
    let opened = v.unlock_with_key(&wrong);
    assert!(
        opened.is_err() || v.journals().is_err(),
        "a key that is not this vault's cannot read this vault"
    );
}

#[test]
fn an_unencrypted_vault_has_no_key_to_keep() {
    let dir = tempfile::tempdir().unwrap();
    let v = Vault::create(dir.path(), cfg(None), registry()).unwrap();
    let err = v.export_data_key(None).unwrap_err();
    assert!(err.to_string().contains("already opens"), "got {err}");
}
