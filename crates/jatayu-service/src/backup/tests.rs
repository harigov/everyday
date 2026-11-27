//! Backups end to end, against a real encrypted vault on SQLite and a folder
//! destination: what a copy holds, what a second copy shares with the first,
//! what pruning lets go of, and that what comes back opens with the password
//! and holds what went in.

use std::path::Path;
use std::sync::Arc;

use jatayu_core::backup::{BackupKeys, Destination};
use jatayu_core::note::Note;
use jatayu_core::{Vault, VaultConfig};

use super::manifest::{Area, Manifest};
use super::repo::{self, ManifestCache};
use super::target::{Target, TargetErrorKind};
use super::{BackupError, Plan, Progress, RestoreOptions};

const PASSWORD: &str = "correct horse battery";

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

fn vault(dir: &Path, password: &str) -> Arc<Vault> {
    let cfg = VaultConfig {
        password: Some(password.into()),
        kdf: jatayu_core::crypto::KdfParams::insecure_fast(),
        ..Default::default()
    };
    Arc::new(jatayu_vault::create(dir, cfg).unwrap())
}

fn plan(destination: &Path, keep: u32) -> Plan {
    Plan {
        destination: Destination::Folder { path: destination.display().to_string() },
        keep,
        config_dir: None,
        files: false,
        expect_existing: false,
        s3_keys: None,
        cache_dir: None,
        host: "test-host".into(),
    }
}

fn note(vault: &Vault, title: &str) -> Note {
    let note = Note::new(title);
    vault.save_note(&note, None).unwrap();
    note
}

/// Write `n` bytes that no other test file shares, so a chunk is new.
fn noise(n: usize, seed: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let mut x = u32::from(seed).wrapping_mul(2_654_435_761).wrapping_add(1);
    while out.len() < n {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(n);
    out
}

#[test]
fn a_backup_restores_into_a_vault_that_opens_with_the_password() {
    let (home, dest, restored) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let v = vault(home.path(), PASSWORD);
    let kept = note(&v, "Kept in the backup");
    let picture = v.put_blob(&noise(300_000, 1)).unwrap();
    // Something beside the store that a backup also copies.
    std::fs::create_dir_all(home.path().join("spool/recordings/r1")).unwrap();
    std::fs::write(home.path().join("spool/recordings/r1/mic-0.chunk"), b"sealed audio").unwrap();
    // And something beside the vault that it must not.
    std::fs::create_dir_all(home.path().join("models/speechKit")).unwrap();
    std::fs::write(home.path().join("models/speechKit/vad.onnx"), b"not the vault's").unwrap();

    rt().block_on(async {
        let report =
            super::run(v.clone(), plan(dest.path(), 7), Progress::new(true)).await.unwrap();
        assert!(report.files >= 3, "{report:?}");
        assert!(report.uploaded_chunks > 0);
        assert!(report.skipped.is_empty());

        let into = restored.path().join("vault");
        let target = Target::folder(dest.path());
        let restore = super::restore(
            &target,
            RestoreOptions {
                snapshot: None,
                into: into.clone(),
                config_into: None,
                files_into: None,
                password: Some(PASSWORD.into()),
                allow_unencrypted: false,
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(restore.snapshot, report.snapshot);

        assert!(into.join("spool/recordings/r1/mic-0.chunk").is_file());
        assert!(!into.join("models").exists(), "downloaded models are not the vault's");
        let back = jatayu_vault::open(&into).unwrap();
        back.unlock(Some(PASSWORD)).unwrap();
        assert_eq!(back.note(kept.id).unwrap().title, "Kept in the backup");
        assert_eq!(back.blob(picture).unwrap(), noise(300_000, 1));
    });
}

#[test]
fn a_second_backup_sends_only_what_changed_and_reuses_unchanged_files() {
    let (home, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let v = vault(home.path(), PASSWORD);
    note(&v, "First");
    v.put_blob(&noise(3_000_000, 2)).unwrap();

    rt().block_on(async {
        let first =
            super::run(v.clone(), plan(dest.path(), 7), Progress::new(false)).await.unwrap();
        // The same vault again: the attachment is unchanged on disk and is
        // not read, and the database differs in a page or two at most.
        let mut p = plan(dest.path(), 7);
        p.expect_existing = true;
        let second = super::run(v.clone(), p.clone(), Progress::new(false)).await.unwrap();
        assert!(
            second.uploaded_bytes * 10 < first.uploaded_bytes,
            "first sent {}, second {}",
            first.uploaded_bytes,
            second.uploaded_bytes
        );
        assert_ne!(first.snapshot, second.snapshot);

        // A new attachment is the new chunks, and not much else.
        v.put_blob(&noise(2_000_000, 3)).unwrap();
        let third = super::run(v.clone(), p, Progress::new(false)).await.unwrap();
        assert!(third.uploaded_bytes >= 2_000_000);
        assert!(third.uploaded_bytes < first.uploaded_bytes);

        let target = Target::folder(dest.path());
        assert_eq!(repo::snapshots(&target).await.unwrap().len(), 3);
        let report = super::verify(&target, &v.backup_keys().unwrap(), true).await.unwrap();
        assert!(report.sound(), "{report:?}");
    });
}

#[test]
fn pruning_keeps_the_newest_copies_and_drops_the_chunks_only_old_ones_used() {
    let (home, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let v = vault(home.path(), PASSWORD);
    let first_blob = v.put_blob(&noise(1_500_000, 4)).unwrap();

    rt().block_on(async {
        let mut p = plan(dest.path(), 2);
        super::run(v.clone(), p.clone(), Progress::new(false)).await.unwrap();
        p.expect_existing = true;

        // The first copy's attachment goes from the vault: only that copy
        // holds its chunks.
        let mut n = note(&v, "holds nothing");
        n.title = "changed".into();
        v.overwrite_note(&n).unwrap();
        v.with_store(|s| {
            std::fs::remove_file(blob_path(home.path(), s, first_blob)).ok();
            Ok(())
        })
        .unwrap();

        super::run(v.clone(), p.clone(), Progress::new(false)).await.unwrap();
        let third = super::run(v.clone(), p, Progress::new(false)).await.unwrap();
        assert_eq!(third.removed_snapshots, 1);
        assert!(third.removed_chunks > 0, "{third:?}");

        let target = Target::folder(dest.path());
        assert_eq!(repo::snapshots(&target).await.unwrap().len(), 2);
        let report = super::verify(&target, &v.backup_keys().unwrap(), false).await.unwrap();
        assert!(report.sound(), "what stays is whole: {report:?}");
    });
}

/// Where the file store keeps `blob`, found by walking for its id rather
/// than knowing the layout.
fn blob_path(
    home: &Path,
    _store: &dyn jatayu_core::store::JournalStore,
    blob: jatayu_core::id::BlobId,
) -> std::path::PathBuf {
    let id = blob.to_hex();
    jatayu_core::backup::list_files(&home.join("store/media"), "", &|_| false)
        .unwrap()
        .into_iter()
        .find(|f| f.path.contains(&id))
        .map(|f| f.source)
        .expect("the blob is somewhere under media")
}

#[test]
fn another_vault_and_a_wrong_password_open_nothing() {
    let (a_home, b_home, dest) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = vault(a_home.path(), PASSWORD);
    let b = vault(b_home.path(), "a different one");
    rt().block_on(async {
        super::run(a.clone(), plan(dest.path(), 7), Progress::new(false)).await.unwrap();
        let err =
            super::run(b.clone(), plan(dest.path(), 7), Progress::new(false)).await.unwrap_err();
        assert!(
            matches!(err, BackupError::Refused(ref why) if why.contains("another vault")),
            "{err}"
        );

        let target = Target::folder(dest.path());
        let err = super::restore::open_repository(&target, Some("wrong"), false).await.unwrap_err();
        assert!(matches!(err, BackupError::Vault(jatayu_core::Error::BadPassword)), "{err}");
    });
}

#[test]
fn a_destination_that_has_gone_is_not_filled_afresh() {
    let (home, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let v = vault(home.path(), PASSWORD);
    let unplugged = dest.path().join("Backup Disk");
    rt().block_on(async {
        let mut p = plan(&unplugged, 7);
        p.expect_existing = true;
        let err = super::run(v.clone(), p, Progress::new(false)).await.unwrap_err();
        assert!(err.unreachable(), "{err}");
        assert!(!unplugged.exists(), "nothing was made where the disk should be");

        // The first time, it is made.
        super::run(v.clone(), plan(&unplugged, 7), Progress::new(false)).await.unwrap();
        assert!(unplugged.join(repo::MARKER).is_file());
    });
}

/// A repository a build under a former name made -- Karma Yoga's, with a
/// `karmayoga-backup.json` in format `karmayoga-backup` -- is the same
/// repository to this build: checked as holding this vault's backups, backed
/// up into rather than beside, its marker left as it was, and restored from.
#[test]
fn a_repository_made_under_a_former_name_is_the_same_repository() {
    let (home, dest, restored) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let v = vault(home.path(), PASSWORD);
    let kept = note(&v, "Backed up as Karma Yoga");
    rt().block_on(async {
        let first =
            super::run(v.clone(), plan(dest.path(), 7), Progress::new(false)).await.unwrap();
        // The marker as Karma Yoga wrote it, under its own name.
        let marker = dest.path().join(repo::MARKER);
        let mut written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
        written["format"] = "karmayoga-backup".into();
        let former = dest.path().join("karmayoga-backup.json");
        std::fs::write(&former, serde_json::to_vec_pretty(&written).unwrap()).unwrap();
        std::fs::remove_file(&marker).unwrap();

        let destination = Destination::Folder { path: dest.path().display().to_string() };
        let keys = v.backup_keys().unwrap();
        let check = super::check_destination(&destination, None, Some(&keys)).await;
        assert!(check.ok && check.message.contains("One backup"), "{}", check.message);

        // A destination a backup has gone to before: a marker that could not
        // be found would be refused as a disk that is not plugged in.
        note(&v, "Backed up as Jatayu");
        let mut again = plan(dest.path(), 7);
        again.expect_existing = true;
        let second = super::run(v.clone(), again, Progress::new(false)).await.unwrap();
        let target = Target::folder(dest.path());
        assert_eq!(
            repo::snapshots(&target).await.unwrap(),
            [first.snapshot.clone(), second.snapshot.clone()],
            "one repository holding both copies"
        );
        assert!(!marker.exists(), "no second marker beside the first");
        assert!(former.is_file(), "the former marker is left where it was");

        let into = restored.path().join("vault");
        let restore =
            super::restore(&target, restore_options(&into, Some(PASSWORD)), None).await.unwrap();
        assert_eq!(restore.snapshot, second.snapshot);
        let back = jatayu_vault::open(&into).unwrap();
        back.unlock(Some(PASSWORD)).unwrap();
        assert_eq!(back.note(kept.id).unwrap().title, "Backed up as Karma Yoga");
    });
}

/// A former name's marker is held to what this build's is: one that is not
/// a marker at all is refused by its own name, and one in a format no build
/// of this application wrote is not taken for a repository.
#[test]
fn a_former_marker_is_read_as_strictly_as_the_current_one() {
    let (home, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let v = vault(home.path(), PASSWORD);
    let target = Target::folder(dest.path());
    let keys = v.backup_keys().unwrap();
    rt().block_on(async {
        std::fs::write(dest.path().join("karmayoga-backup.json"), b"not json").unwrap();
        let err = repo::marker(&target).await.unwrap_err();
        assert!(err.to_string().contains("karmayoga-backup.json"), "{err}");

        let foreign = serde_json::json!({
            "format": "somebody-elses-backup",
            "version": 1,
            "repository": keys.repository_id(),
            "created": "2026-11-20T00:00:00Z",
        });
        std::fs::write(dest.path().join("karmayoga-backup.json"), foreign.to_string()).unwrap();
        assert!(repo::prepare(&target, &keys, true).await.is_err());
        assert!(!dest.path().join(repo::MARKER).exists());
    });
}

#[test]
fn a_damaged_chunk_is_refused_on_restore() {
    let (home, dest, out) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let v = vault(home.path(), PASSWORD);
    v.put_blob(&noise(200_000, 5)).unwrap();
    rt().block_on(async {
        super::run(v.clone(), plan(dest.path(), 7), Progress::new(false)).await.unwrap();
        let target = Target::folder(dest.path());
        // Flip a byte in every chunk: whichever the restore reads first is
        // refused.
        for name in repo::chunks(&target).await.unwrap() {
            let path = dest.path().join(repo::chunk_object(&name));
            let mut bytes = std::fs::read(&path).unwrap();
            let last = bytes.len() - 1;
            bytes[last] ^= 0x55;
            std::fs::write(&path, bytes).unwrap();
        }
        let err = super::restore(
            &target,
            RestoreOptions {
                snapshot: None,
                into: out.path().join("v"),
                config_into: None,
                files_into: None,
                password: Some(PASSWORD.into()),
                allow_unencrypted: false,
            },
            None,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("damaged"), "{err}");
        assert!(
            !out.path().join("v").join("vault.json").exists(),
            "a broken restore is not a vault"
        );
        let report = super::verify(&target, &v.backup_keys().unwrap(), true).await.unwrap();
        assert!(!report.damaged.is_empty());
    });
}

#[test]
fn settings_files_come_back_only_when_asked_and_never_over_what_is_there() {
    let (home, dest, out, config, new_config) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    std::fs::write(config.path().join("devices.json"), b"{\"paired\":[]}").unwrap();
    std::fs::write(config.path().join("mcp.json"), b"{\"token\":\"t\"}").unwrap();
    std::fs::write(config.path().join("last-vault"), b"/somewhere/else").unwrap();
    std::fs::create_dir_all(config.path().join("mail-index/abc")).unwrap();
    std::fs::write(config.path().join("mail-index/abc/seg"), b"index").unwrap();
    std::fs::write(new_config.path().join("mcp.json"), b"{\"token\":\"mine\"}").unwrap();

    let v = vault(home.path(), PASSWORD);
    rt().block_on(async {
        let mut p = plan(dest.path(), 7);
        p.config_dir = Some(config.path().to_path_buf());
        super::run(v.clone(), p, Progress::new(false)).await.unwrap();

        let target = Target::folder(dest.path());
        let keys = v.backup_keys().unwrap();
        let name = repo::snapshots(&target).await.unwrap().pop().unwrap();
        let manifest =
            repo::load_manifest(&target, &keys, &name, &ManifestCache::new(None)).await.unwrap();
        let config_paths: Vec<&str> = manifest
            .files
            .iter()
            .filter(|f| f.area == Area::Config)
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(config_paths, ["devices.json", "mcp.json"]);

        let report = super::restore(
            &target,
            RestoreOptions {
                snapshot: Some(name),
                into: out.path().join("v"),
                config_into: Some(new_config.path().to_path_buf()),
                files_into: None,
                password: Some(PASSWORD.into()),
                allow_unencrypted: false,
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(report.config_restored, ["devices.json"]);
        assert_eq!(report.config_kept, ["mcp.json"]);
        assert_eq!(
            std::fs::read(new_config.path().join("mcp.json")).unwrap(),
            b"{\"token\":\"mine\"}"
        );
    });
}

#[test]
fn a_cancelled_backup_leaves_no_copy_and_the_next_one_finishes() {
    let (home, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let v = vault(home.path(), PASSWORD);
    v.put_blob(&noise(4_000_000, 6)).unwrap();
    rt().block_on(async {
        let progress = Progress::new(true);
        progress.cancel();
        let err = super::run(v.clone(), plan(dest.path(), 7), progress).await.unwrap_err();
        assert!(matches!(err, BackupError::Cancelled), "{err}");
        let target = Target::folder(dest.path());
        assert!(repo::snapshots(&target).await.unwrap().is_empty());
        assert!(target.get(repo::LOCK).await.unwrap().is_none(), "the lock is let go");

        super::run(v.clone(), plan(dest.path(), 7), Progress::new(false)).await.unwrap();
        assert_eq!(repo::snapshots(&target).await.unwrap().len(), 1);
    });
}

#[test]
fn a_fresh_lock_from_another_computer_holds_a_backup_off() {
    let (home, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let v = vault(home.path(), PASSWORD);
    rt().block_on(async {
        super::run(v.clone(), plan(dest.path(), 7), Progress::new(false)).await.unwrap();
        let target = Target::folder(dest.path());
        let now = jiff::Timestamp::now();
        let held =
            repo::Lock { host: "the-other-laptop".into(), pid: 1, started: now, touched: now };
        target.put(repo::LOCK, serde_json::to_vec(&held).unwrap().into()).await.unwrap();
        let mut p = plan(dest.path(), 7);
        p.expect_existing = true;
        let err = super::run(v.clone(), p.clone(), Progress::new(false)).await.unwrap_err();
        assert!(err.to_string().contains("the-other-laptop"), "{err}");

        // Stale: left by a crash hours ago.
        let old = now.checked_sub(jiff::SignedDuration::from_hours(3)).unwrap();
        let stale = repo::Lock { touched: old, started: old, ..held };
        target.put(repo::LOCK, serde_json::to_vec(&stale).unwrap().into()).await.unwrap();
        super::run(v.clone(), p, Progress::new(false)).await.unwrap();
    });
}

#[test]
fn checking_a_destination_says_what_it_holds() {
    let (home, other_home, dest) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let v = vault(home.path(), PASSWORD);
    let other = vault(other_home.path(), PASSWORD);
    let destination = Destination::Folder { path: dest.path().join("new").display().to_string() };
    rt().block_on(async {
        let keys = v.backup_keys().unwrap();
        let check = super::check_destination(&destination, None, Some(&keys)).await;
        assert!(check.ok, "{}", check.message);
        assert!(check.message.contains("Nothing is kept"), "{}", check.message);

        let mut p = plan(&dest.path().join("new"), 7);
        p.expect_existing = false;
        super::run(v.clone(), p, Progress::new(false)).await.unwrap();
        let check = super::check_destination(&destination, None, Some(&keys)).await;
        assert!(check.ok && check.message.contains("One backup"), "{}", check.message);

        let theirs = other.backup_keys().unwrap();
        let check = super::check_destination(&destination, None, Some(&theirs)).await;
        assert!(!check.ok && check.message.contains("another vault"), "{}", check.message);

        let snapshots = super::list_snapshots(&destination, None).await.unwrap();
        assert_eq!(snapshots.len(), 1);
    });
}

#[test]
fn a_folder_that_is_not_there_is_reported_as_missing() {
    let dir = tempfile::tempdir().unwrap();
    let target = Target::folder(&dir.path().join("nope"));
    rt().block_on(async {
        let err = repo::snapshots(&target).await.unwrap_err();
        assert_eq!(err.kind, TargetErrorKind::Missing);
    });
}

#[test]
fn what_is_left_out_of_the_settings_folder() {
    use super::run::leaves_out_of_config as out;
    assert!(out("last-vault"));
    // Every former name's mark that this computer's webview folder was
    // carried across: Every Day's, which a machine that went through both
    // renames still has, and Karma Yoga's, which the app writes now.
    assert!(out("webview-carried-over"));
    assert!(out("webview-carried-over-from-karmayoga"));
    assert!(!out("Notes/webview-carried-over"), "only in the folder itself");
    assert!(out("mail-index"));
    assert!(out("mail-index/0123/meta.json"));
    assert!(out("chat/abc/matrix-sdk-crypto.sqlite3"));
    assert!(out("vault.lock"));
    assert!(!out("devices.json"));
    assert!(!out("server.key"));
    assert!(!out("mcp.json"));
    assert!(!out("chatty.json"));
}

#[test]
fn keys_from_the_header_match_the_running_vaults() {
    let home = tempfile::tempdir().unwrap();
    let v = vault(home.path(), PASSWORD);
    let header: jatayu_core::vault::VaultHeader =
        serde_json::from_slice(&v.backup_header().unwrap()).unwrap();
    let from_header = BackupKeys::from_header(&header, Some(PASSWORD), false).unwrap();
    let live = v.backup_keys().unwrap();
    assert_eq!(from_header.repository_id(), live.repository_id());
    assert_eq!(from_header.chunk_name(b"x"), live.chunk_name(b"x"));
    assert!(BackupKeys::from_header(&header, Some("nope"), false).is_err());
}

#[test]
fn next_due_follows_the_last_success_and_waits_after_a_failure() {
    use jatayu_core::backup::{BackupSettings, BackupStatus};
    let mut settings = BackupSettings {
        destination: Some(Destination::Folder { path: "/mnt/b".into() }),
        ..BackupSettings::default()
    };
    let status = BackupStatus::default();
    assert_eq!(super::next_due(&settings, &status), None, "off");
    settings.enabled = true;
    assert_eq!(
        super::next_due(&settings, &status),
        Some(jiff::Timestamp::UNIX_EPOCH),
        "never: now"
    );

    let at: jiff::Timestamp = "2026-11-14T03:00:00Z".parse().unwrap();
    let ok =
        BackupStatus { last_success: Some(at), last_attempt: Some(at), ..BackupStatus::default() };
    assert_eq!(super::next_due(&settings, &ok), Some("2026-11-15T03:00:00Z".parse().unwrap()));

    let failed_at: jiff::Timestamp = "2026-11-15T04:00:00Z".parse().unwrap();
    let failing = BackupStatus {
        last_attempt: Some(failed_at),
        last_error: Some("unplugged".into()),
        ..ok.clone()
    };
    assert_eq!(super::next_due(&settings, &failing), Some("2026-11-15T04:30:00Z".parse().unwrap()));
    settings.every_hours = 1;
    let failing_hourly = BackupStatus { last_success: Some(failed_at), ..failing };
    assert_eq!(
        super::next_due(&settings, &failing_hourly),
        Some("2026-11-15T05:00:00Z".parse().unwrap()),
        "an hour after the last success, which is later than half an hour after the failure"
    );
}

#[test]
fn settings_are_tidied_checked_and_the_access_key_never_comes_back_with_them() {
    use jatayu_core::backup::{BackupSettings, S3Credentials, S3Keys};
    let home = tempfile::tempdir().unwrap();
    let v = vault(home.path(), PASSWORD);
    assert!(v.supports_backup());
    assert_eq!(v.backup_settings().unwrap(), BackupSettings::default());

    let saved = v
        .save_backup_settings(&BackupSettings {
            enabled: true,
            destination: Some(Destination::S3 {
                bucket: " my-backups ".into(),
                prefix: "/jatayu/".into(),
                region: Some("".into()),
                endpoint: None,
                profile: Some("work".into()),
                credentials: S3Credentials::Stored,
            }),
            every_hours: 6,
            keep: 14,
            include_config: false,
        })
        .unwrap();
    assert_eq!(saved.destination.as_ref().unwrap().label(), "s3://my-backups/jatayu");
    assert_eq!(v.backup_settings().unwrap(), saved);

    let refused = v.save_backup_settings(&BackupSettings { every_hours: 3, ..saved.clone() });
    assert!(refused.is_err());

    assert!(!v.has_backup_s3_keys().unwrap());
    assert!(
        v.set_backup_s3_keys(Some(&S3Keys {
            access_key_id: "AKID".into(),
            secret_access_key: " ".into()
        }))
        .is_err()
    );
    v.set_backup_s3_keys(Some(&S3Keys {
        access_key_id: " AKID ".into(),
        secret_access_key: "wJalrXUtnFEMI".into(),
    }))
    .unwrap();
    let keys = v.backup_s3_keys().unwrap().unwrap();
    assert_eq!(
        (keys.access_key_id.as_str(), keys.secret_access_key.as_str()),
        ("AKID", "wJalrXUtnFEMI")
    );
    assert!(!format!("{keys:?}").contains("wJalrXUtnFEMI"), "the secret is never printed");
    let settings_json = serde_json::to_string(&v.backup_settings().unwrap()).unwrap();
    assert!(!settings_json.contains("wJalrXUtnFEMI"));
    v.set_backup_s3_keys(None).unwrap();
    assert!(!v.has_backup_s3_keys().unwrap());
}

#[test]
fn locations_parse_as_typed() {
    use super::{LocationOptions, parse_location};
    let opts = LocationOptions { region: Some("eu-west-2".into()), ..Default::default() };
    match parse_location("s3://bucket/a/b/", &opts).unwrap() {
        Destination::S3 { bucket, prefix, region, .. } => {
            assert_eq!((bucket.as_str(), prefix.as_str()), ("bucket", "a/b"));
            assert_eq!(region.as_deref(), Some("eu-west-2"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(parse_location("gs://bkt", &opts).unwrap().label(), "gs://bkt");
    assert!(parse_location("ftp://x/y", &opts).is_err());
    assert!(parse_location("s3://Bad_Bucket", &opts).is_err());
    match parse_location("relative/dir", &opts).unwrap() {
        Destination::Folder { path } => assert!(Path::new(&path).is_absolute()),
        other => panic!("{other:?}"),
    }
}

fn unencrypted(dir: &Path) -> Arc<Vault> {
    Arc::new(
        jatayu_vault::create(dir, VaultConfig { password: None, ..Default::default() }).unwrap(),
    )
}

#[test]
fn two_unencrypted_vaults_do_not_share_a_destination() {
    let (a_home, b_home, dest) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = unencrypted(a_home.path());
    // Created a moment later, so its header's identity differs.
    std::thread::sleep(std::time::Duration::from_millis(5));
    let b = unencrypted(b_home.path());
    rt().block_on(async {
        super::run(a.clone(), plan(dest.path(), 7), Progress::new(false)).await.unwrap();
        let err =
            super::run(b.clone(), plan(dest.path(), 7), Progress::new(false)).await.unwrap_err();
        assert!(err.to_string().contains("another vault"), "{err}");
        let target = Target::folder(dest.path());
        assert_eq!(repo::snapshots(&target).await.unwrap().len(), 1, "a's copy is untouched");
    });
}

#[test]
fn a_stop_is_recorded_and_the_schedule_waits_a_whole_interval() {
    use jatayu_core::backup::{BackupSettings, BackupStatus};
    let home = tempfile::tempdir().unwrap();
    let v = vault(home.path(), PASSWORD);
    let destination = Destination::Folder { path: "/mnt/backup".into() };
    let at = jiff::Timestamp::now();
    let stopped =
        super::task::save_outcome(&v, &destination, at, &Err(BackupError::Cancelled), true)
            .expect("a stop is recorded");
    assert!(stopped.stopped_at.is_some());
    assert!(stopped.last_error.is_none(), "stopping is not failing");
    // Locked, not stopped: nothing changes.
    assert!(
        super::task::save_outcome(&v, &destination, at, &Err(BackupError::Cancelled), false)
            .is_none()
    );

    let settings = BackupSettings {
        enabled: true,
        destination: Some(destination.clone()),
        every_hours: 24,
        ..BackupSettings::default()
    };
    let due = super::next_due(&settings, &stopped).unwrap();
    assert!(due > at.checked_add(jiff::SignedDuration::from_hours(23)).unwrap(), "{due}");
    // A backup that works afterwards puts the schedule back.
    let ok = BackupStatus { last_success: Some(due), ..stopped };
    assert_eq!(
        super::next_due(&settings, &ok),
        Some(due.checked_add(jiff::SignedDuration::from_hours(24)).unwrap())
    );
}

#[test]
fn a_backup_that_works_remembers_its_destination() {
    let home = tempfile::tempdir().unwrap();
    let v = vault(home.path(), PASSWORD);
    let destination = Destination::Folder { path: "/mnt/backup".into() };
    let report = jatayu_core::backup::BackupReport::default();
    let status =
        super::task::save_outcome(&v, &destination, jiff::Timestamp::now(), &Ok(report), false)
            .unwrap();
    assert!(status.has_used(&destination));
    let elsewhere = Destination::Folder { path: "/mnt/other".into() };
    assert!(
        v.backup_status()
            .unwrap()
            .for_destination(&elsewhere)
            .for_destination(&destination)
            .has_used(&destination)
    );
}

#[test]
fn a_staging_folder_is_in_use_while_its_process_lives() {
    let home = tempfile::tempdir().unwrap();
    let parent = home.path().join(jatayu_core::vault::BACKUP_STAGING_DIRNAME);
    std::fs::create_dir_all(parent.join("999999999-deadbeef")).unwrap();
    assert!(!repo::staging_in_use(home.path()), "a process that is gone");
    std::fs::create_dir_all(parent.join("1-00000001")).unwrap();
    assert!(repo::staging_in_use(home.path()), "init is always alive");
    let mine = repo::staging_dir(home.path()).unwrap();
    assert!(!parent.join("999999999-deadbeef").exists(), "the dead one is swept");
    assert!(parent.join("1-00000001").exists(), "a live one is left alone");
    assert!(mine.is_dir());
    assert!(super::running_on(home.path()));
}

#[test]
fn a_staging_folder_that_cannot_be_made_leaves_no_lock_behind() {
    let (home, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let v = vault(home.path(), PASSWORD);
    // A file where the staging folder goes.
    std::fs::write(home.path().join(jatayu_core::vault::BACKUP_STAGING_DIRNAME), b"").unwrap();
    rt().block_on(async {
        assert!(super::run(v.clone(), plan(dest.path(), 7), Progress::new(false)).await.is_err());
        let target = Target::folder(dest.path());
        assert!(target.get(repo::LOCK).await.unwrap().is_none());
    });
}

// ---- the files folder ----------------------------------------------------------

/// A vault with its files folder set up at `folder`.
fn with_files(home: &Path, folder: &Path) -> Arc<Vault> {
    let v = vault(home, PASSWORD);
    v.set_up_files_folder(Some(folder)).unwrap();
    v
}

fn with_files_plan(destination: &Path, keep: u32) -> Plan {
    Plan { files: true, ..plan(destination, keep) }
}

/// The newest copy at `target`, and its manifest.
async fn newest(target: &Target, v: &Vault) -> (String, Manifest) {
    let name = repo::snapshots(target).await.unwrap().pop().unwrap();
    let keys = v.backup_keys().unwrap();
    let manifest =
        repo::load_manifest(target, &keys, &name, &ManifestCache::new(None)).await.unwrap();
    (name, manifest)
}

/// What a copy holds of the files folder, by the path it names it under.
fn files_in(manifest: &Manifest) -> Vec<String> {
    manifest.files.iter().filter(|f| f.in_files_folder()).map(|f| f.path.clone()).collect()
}

#[test]
fn the_files_folder_is_in_the_copy_under_files_and_comes_back_without_what_a_scan_leaves_out() {
    let (home, dest, folder, restored) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    std::fs::write(folder.path().join("a.txt"), b"alpha").unwrap();
    std::fs::create_dir_all(folder.path().join("Photos")).unwrap();
    std::fs::write(folder.path().join("Photos/b.jpg"), noise(1_500_000, 6)).unwrap();
    // What a scan of the folder leaves out, so a copy of it does.
    std::fs::write(folder.path().join(".DS_Store"), b"litter").unwrap();
    std::fs::write(folder.path().join(".hidden"), b"dotted").unwrap();
    let v = with_files(home.path(), folder.path());

    rt().block_on(async {
        let report = super::run(v.clone(), with_files_plan(dest.path(), 7), Progress::new(true))
            .await
            .unwrap();
        assert!(!report.files_folder_unavailable, "{report:?}");
        let target = Target::folder(dest.path());
        let (name, manifest) = newest(&target, &v).await;
        assert_eq!(files_in(&manifest), ["files/Photos/b.jpg", "files/a.txt"]);
        assert!(manifest.files_folder.is_some(), "the copy says where the folder was");
        let sound = super::verify(&target, &v.backup_keys().unwrap(), true).await.unwrap();
        assert!(sound.sound(), "{sound:?}");

        // Back where it was told to go: the files, the folder marked as
        // Jatayu's again, and nothing laid down inside the vault.
        let files_back = restored.path().join("Files");
        let options = |into: &str| RestoreOptions {
            snapshot: Some(name.clone()),
            into: restored.path().join(into),
            config_into: None,
            files_into: Some(files_back.clone()),
            password: Some(PASSWORD.into()),
            allow_unencrypted: false,
        };
        let report = super::restore(&target, options("vault"), None).await.unwrap();
        assert_eq!((report.files_folder_restored, report.files_folder_kept), (2, 0));
        assert_eq!(std::fs::read(files_back.join("a.txt")).unwrap(), b"alpha");
        assert_eq!(std::fs::read(files_back.join("Photos/b.jpg")).unwrap(), noise(1_500_000, 6));
        assert!(!files_back.join(".DS_Store").exists());
        assert!(files_back.join(jatayu_core::files::INTERNAL_DIR).is_dir());
        assert!(!restored.path().join("vault/files").exists());

        // A second restore is never over what is there now.
        std::fs::write(files_back.join("a.txt"), b"mine").unwrap();
        let report = super::restore(&target, options("vault2"), None).await.unwrap();
        assert_eq!((report.files_folder_restored, report.files_folder_kept), (0, 2));
        assert_eq!(std::fs::read(files_back.join("a.txt")).unwrap(), b"mine");
    });
}

#[test]
fn a_files_folder_that_is_not_there_is_carried_forward_and_never_recorded_empty() {
    let (home, dest, folder, restored) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    std::fs::write(folder.path().join("a.txt"), b"alpha").unwrap();
    std::fs::write(folder.path().join("b.bin"), noise(1_200_000, 7)).unwrap();
    let v = with_files(home.path(), folder.path());

    rt().block_on(async {
        // Keeping one copy is the hard case: the second run lets go of the
        // first, and with it the only chunks that hold the files.
        let first = super::run(v.clone(), with_files_plan(dest.path(), 1), Progress::new(false))
            .await
            .unwrap();
        assert!(!first.files_folder_unavailable);
        let target = Target::folder(dest.path());
        let (_, before) = newest(&target, &v).await;

        let away = folder.path().with_extension("away");
        std::fs::rename(folder.path(), &away).unwrap();
        let mut p = with_files_plan(dest.path(), 1);
        p.expect_existing = true;
        let second = super::run(v.clone(), p.clone(), Progress::new(false)).await.unwrap();
        assert!(second.files_folder_unavailable, "{second:?}");
        assert_eq!(second.removed_snapshots, 1);
        let (name, after) = newest(&target, &v).await;
        assert_eq!(files_in(&after), files_in(&before));
        assert_eq!(files_in(&after).len(), 2);
        assert_eq!(after.files_folder, before.files_folder);
        let sound = super::verify(&target, &v.backup_keys().unwrap(), true).await.unwrap();
        assert!(sound.sound(), "what the copy names is still there: {sound:?}");

        // And it opens: the files are in the copy as they were.
        let files_back = restored.path().join("Files");
        super::restore(
            &target,
            RestoreOptions {
                snapshot: Some(name),
                into: restored.path().join("vault"),
                config_into: None,
                files_into: Some(files_back.clone()),
                password: Some(PASSWORD.into()),
                allow_unencrypted: false,
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(files_back.join("b.bin")).unwrap(), noise(1_200_000, 7));

        // The folder back: it is recorded as it is.
        std::fs::rename(&away, folder.path()).unwrap();
        std::fs::remove_file(folder.path().join("a.txt")).unwrap();
        let third = super::run(v.clone(), p, Progress::new(false)).await.unwrap();
        assert!(!third.files_folder_unavailable);
        let (_, now) = newest(&target, &v).await;
        assert_eq!(files_in(&now), ["files/b.bin"]);
    });
}

#[test]
fn a_folder_that_was_never_set_up_costs_the_copy_nothing() {
    let (home, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let v = vault(home.path(), PASSWORD);
    rt().block_on(async {
        let report = super::run(v.clone(), with_files_plan(dest.path(), 7), Progress::new(false))
            .await
            .unwrap();
        assert!(!report.files_folder_unavailable, "{report:?}");
        let (_, manifest) = newest(&Target::folder(dest.path()), &v).await;
        assert!(files_in(&manifest).is_empty());
        assert_eq!(manifest.files_folder, None);
    });
}

#[test]
fn one_file_comes_back_from_the_newest_copy_that_holds_it_and_never_over_a_file() {
    let (home, dest, folder) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let a = folder.path().join("Docs/a.txt");
    std::fs::create_dir_all(a.parent().unwrap()).unwrap();
    std::fs::write(&a, b"version one").unwrap();
    let v = with_files(home.path(), folder.path());

    rt().block_on(async {
        let p = with_files_plan(dest.path(), 7);
        super::run(v.clone(), p.clone(), Progress::new(false)).await.unwrap();
        std::fs::write(&a, b"version two, longer").unwrap();
        let mut p = p;
        p.expect_existing = true;
        super::run(v.clone(), p, Progress::new(false)).await.unwrap();
        let target = Target::folder(dest.path());
        let (newest_name, _) = newest(&target, &v).await;

        std::fs::remove_file(&a).unwrap();
        let keys = v.backup_keys().unwrap();
        let staging = folder.path().join(jatayu_core::files::INTERNAL_DIR).join("tmp");
        let restored =
            super::restore::restore_one(&target, &keys, "Docs/a.txt", &a, &staging, None)
                .await
                .unwrap()
                .expect("a copy holds it");
        assert_eq!(restored.snapshot, newest_name);
        assert_eq!(std::fs::read(&a).unwrap(), b"version two, longer");
        assert!(
            std::fs::read_dir(&staging).map(|mut d| d.next().is_none()).unwrap_or(true),
            "nothing is left in the staging folder"
        );

        // Already there: refused, and untouched.
        let err = super::restore::restore_one(&target, &keys, "Docs/a.txt", &a, &staging, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already there"), "{err}");
        assert_eq!(std::fs::read(&a).unwrap(), b"version two, longer");

        // No copy holds it.
        let elsewhere = folder.path().join("never.txt");
        assert_eq!(
            super::restore::restore_one(&target, &keys, "never.txt", &elsewhere, &staging, None)
                .await
                .unwrap(),
            None
        );
        assert!(!elsewhere.exists());
    });
}

// ---- what a destination somebody else can write to may say ---------------------

fn restore_options(into: &Path, password: Option<&str>) -> RestoreOptions {
    RestoreOptions {
        snapshot: None,
        into: into.to_path_buf(),
        config_into: None,
        files_into: None,
        password: password.map(str::to_string),
        allow_unencrypted: false,
    }
}

/// The header kept at `dest`, changed by `edit` and put back.
fn edit_header(dest: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let path = dest.join(repo::HEADER);
    let mut header: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    edit(&mut header);
    std::fs::write(&path, serde_json::to_vec_pretty(&header).unwrap()).unwrap();
}

#[test]
fn a_header_at_the_destination_that_says_none_does_not_skip_the_password() {
    let (home, dest, out) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let v = vault(home.path(), PASSWORD);
    note(&v, "private");
    rt().block_on(async {
        super::run(v.clone(), plan(dest.path(), 7), Progress::new(false)).await.unwrap();
        let target = Target::folder(dest.path());

        // Whoever can write to the destination says the vault is not
        // encrypted, and that nothing is needed to open it.
        edit_header(dest.path(), |h| {
            let fields = h.as_object_mut().unwrap();
            fields.insert("cipher".into(), "none".into());
            for key in ["kdf", "salt", "wrappedKey", "keyCheck"] {
                fields.remove(key);
            }
        });
        let into = out.path().join("v");
        for password in [None, Some(PASSWORD)] {
            let err =
                super::restore(&target, restore_options(&into, password), None).await.unwrap_err();
            assert!(
                matches!(err, BackupError::Refused(ref why) if why.contains("--unencrypted")),
                "{err}"
            );
        }
        assert!(!into.exists(), "nothing was laid down");

        // Only one word changed, the rest left as it was: the same.
        edit_header(dest.path(), |h| {
            *h = serde_json::from_slice(&v.backup_header().unwrap()).unwrap();
            h["cipher"] = "none".into();
        });
        assert!(
            super::restore(&target, restore_options(&into, Some(PASSWORD)), None).await.is_err()
        );
        let err = super::restore::open_repository(&target, Some(PASSWORD), true).await.unwrap_err();
        assert!(err.to_string().contains("not encrypted"), "{err}");
    });
}

#[test]
fn a_vault_that_really_has_no_password_restores_when_that_is_said() {
    let (home, dest, out) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let v = unencrypted(home.path());
    rt().block_on(async {
        super::run(v.clone(), plan(dest.path(), 7), Progress::new(false)).await.unwrap();
        let target = Target::folder(dest.path());
        let into = out.path().join("v");

        let err = super::restore(&target, restore_options(&into, None), None).await.unwrap_err();
        assert!(err.to_string().contains("--unencrypted"), "{err}");
        assert!(!into.exists());

        let options = RestoreOptions { allow_unencrypted: true, ..restore_options(&into, None) };
        super::restore(&target, options, None).await.unwrap();
        assert!(into.join("vault.json").is_file());
    });
}

#[test]
fn a_header_at_the_destination_cannot_ask_for_a_key_derivation_that_exhausts_memory() {
    let (home, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let v = vault(home.path(), PASSWORD);
    rt().block_on(async {
        super::run(v.clone(), plan(dest.path(), 7), Progress::new(false)).await.unwrap();
        edit_header(dest.path(), |h| h["kdf"]["memoryKib"] = u32::MAX.into());
        let target = Target::folder(dest.path());
        let err =
            super::restore::open_repository(&target, Some(PASSWORD), false).await.unwrap_err();
        assert!(
            matches!(err, BackupError::Vault(jatayu_core::Error::Invalid(_))),
            "refused before Argon2 ran: {err}"
        );
    });
}

#[test]
fn files_named_in_a_copy_go_only_where_this_computer_keeps_its_files_folder_or_where_told() {
    let (home, dest, folder, out) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    std::fs::write(folder.path().join("a.txt"), b"alpha").unwrap();
    let v = with_files(home.path(), folder.path());
    rt().block_on(async {
        super::run(v.clone(), with_files_plan(dest.path(), 7), Progress::new(false)).await.unwrap();
        let target = Target::folder(dest.path());
        let (_, manifest) = newest(&target, &v).await;
        assert!(manifest.files_folder.is_some(), "the copy names the folder it was made from");

        // Nothing named: the copy's say-so is not enough, so the files are
        // left out and the vault still comes back.
        std::fs::remove_file(folder.path().join("a.txt")).unwrap();
        let report =
            super::restore(&target, restore_options(&out.path().join("v"), Some(PASSWORD)), None)
                .await
                .unwrap();
        assert_eq!((report.files_folder_restored, report.files_folder_left_out), (0, 1));
        assert_eq!(report.files_folder, None);
        assert_eq!(report.files_folder_recorded, manifest.files_folder);
        assert!(!folder.path().join("a.txt").exists(), "nothing was written where it said");
        assert!(out.path().join("v/vault.json").is_file());

        // Named: it goes there.
        let options = RestoreOptions {
            files_into: Some(out.path().join("Files")),
            ..restore_options(&out.path().join("v2"), Some(PASSWORD))
        };
        let report = super::restore(&target, options, None).await.unwrap();
        assert_eq!(report.files_folder_restored, 1);
        assert_eq!(std::fs::read(out.path().join("Files/a.txt")).unwrap(), b"alpha");
    });
}

/// Write `name` into `snapshots/` the way somebody with write access to the
/// destination would, with something that is not a manifest.
fn plant(dest: &Path, stamp: &str) -> String {
    let name = jatayu_core::backup::snapshot_name(stamp.parse().unwrap());
    std::fs::write(dest.join(repo::snapshot_object(&name)), b"not a manifest of anybody's")
        .unwrap();
    name
}

#[test]
fn a_planted_name_from_the_far_future_neither_costs_real_copies_nor_wins_a_restore() {
    let (home, dest, out) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let v = vault(home.path(), PASSWORD);
    let kept = note(&v, "survives");
    rt().block_on(async {
        let mut p = plan(dest.path(), 2);
        super::run(v.clone(), p.clone(), Progress::new(false)).await.unwrap();
        p.expect_existing = true;
        let second = super::run(v.clone(), p.clone(), Progress::new(false)).await.unwrap();
        let target = Target::folder(dest.path());
        let planted = plant(dest.path(), "2999-01-01T00:00:00Z");
        let real_before = repo::snapshots(&target).await.unwrap().len() - 1;
        assert_eq!(real_before, 2);

        // Keeping two, this run makes a third and lets go of exactly one
        // real copy: the planted name is not "the newest" to keep.
        let third = super::run(v.clone(), p, Progress::new(false)).await.unwrap();
        assert_eq!(third.removed_snapshots, 1, "{third:?}");
        assert!(third.snapshot < planted, "named by the clock, not after the plant");
        let names = repo::snapshots(&target).await.unwrap();
        assert!(names.contains(&planted), "an object that is not ours is left where it is");
        assert!(names.contains(&second.snapshot) && names.contains(&third.snapshot), "{names:?}");
        assert_eq!(names.len(), 3);

        // A default restore takes the newest that opens, and says what it
        // passed over.
        let into = out.path().join("v");
        let report =
            super::restore(&target, restore_options(&into, Some(PASSWORD)), None).await.unwrap();
        assert_eq!(report.snapshot, third.snapshot);
        assert_eq!(report.passed_over, std::slice::from_ref(&planted));
        let back = jatayu_vault::open(&into).unwrap();
        back.unlock(Some(PASSWORD)).unwrap();
        assert_eq!(back.note(kept.id).unwrap().title, "survives");

        // Asking for the planted one by name is an error, not a fallback.
        let options = RestoreOptions {
            snapshot: Some(planted),
            ..restore_options(&out.path().join("w"), Some(PASSWORD))
        };
        assert!(super::restore(&target, options, None).await.is_err());
    });
}

#[cfg(unix)]
#[test]
fn what_a_restore_writes_for_the_vault_and_the_settings_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let (home, dest, out, config, new_config) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    std::fs::write(config.path().join("mcp.json"), b"{\"token\":\"t\"}").unwrap();
    std::fs::create_dir_all(config.path().join("certs/room")).unwrap();
    std::fs::write(config.path().join("certs/room/key.pem"), b"secret").unwrap();
    let v = vault(home.path(), PASSWORD);
    rt().block_on(async {
        let mut p = plan(dest.path(), 7);
        p.config_dir = Some(config.path().to_path_buf());
        super::run(v.clone(), p, Progress::new(false)).await.unwrap();
        let target = Target::folder(dest.path());
        let into = out.path().join("v");
        let settings = new_config.path().join("settings");
        let options = RestoreOptions {
            config_into: Some(settings.clone()),
            ..restore_options(&into, Some(PASSWORD))
        };
        super::restore(&target, options, None).await.unwrap();

        assert_eq!(mode(&settings.join("mcp.json")), 0o600);
        assert_eq!(mode(&settings), 0o700, "a folder the restore made");
        assert_eq!(mode(&settings.join("certs/room/key.pem")), 0o600);
        assert_eq!(mode(&settings.join("certs")), 0o700);
        assert_eq!(mode(&settings.join("certs/room")), 0o700);
        assert_eq!(mode(&into), 0o700);
        assert_eq!(mode(&into.join("vault.json")), 0o600);
        assert_eq!(mode(&into.join("vault.json.bak")), 0o600);
        let store_files =
            jatayu_core::backup::list_files(&into.join("store"), "", &|_| false).unwrap();
        assert!(!store_files.is_empty());
        for f in store_files {
            assert_eq!(mode(&f.source), 0o600, "{}", f.path);
        }
        assert_eq!(mode(&into.join("store")), 0o700);
    });
}

// ── What a destination may hand back ────────────────────────────────────────
//
// A destination is somebody else's disk or bucket. Everything read from one is
// held in memory before it has been opened with the vault's key, so what is in
// it must not be able to decide how much memory that is.

mod hostile_destinations {
    use std::sync::Mutex;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::routing::get;
    use bytes::Bytes;
    use jatayu_core::backup::{S3Credentials, S3Keys};

    use super::*;
    use crate::backup::creds::{self, SendError};
    use crate::backup::target::{
        MAX_CHUNK_BYTES, MAX_LIST_PAGES, MAX_OBJECT_BYTES, MAX_REPLY_BYTES, object_limit,
    };

    const MIB: usize = 1024 * 1024;

    /// Serve `app` on a free local port, and say where.
    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://127.0.0.1:{port}")
    }

    /// What a test binary has to do before it builds a client; the
    /// application's own startup does it otherwise.
    fn provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    /// `mebibytes` of zeros, gzipped: a few dozen kilobytes that inflate to a
    /// size the tests can check was never reached.
    fn gzip_of_zeros(mebibytes: usize) -> Vec<u8> {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let zeros = vec![0u8; MIB];
        for _ in 0..mebibytes {
            encoder.write_all(&zeros).unwrap();
        }
        encoder.finish().unwrap()
    }

    /// A body of `mebibytes` one-mebibyte pieces, made as it is sent and with
    /// no length declared, so that only counting what arrives can stop it.
    fn endless(mebibytes: usize) -> Body {
        Body::from_stream(futures::stream::iter(
            (0..mebibytes).map(|_| Ok::<_, std::convert::Infallible>(Bytes::from(vec![7u8; MIB]))),
        ))
    }

    fn s3_at(base: &str) -> Target {
        provider();
        Target::open(
            &Destination::S3 {
                bucket: "bucket".into(),
                prefix: String::new(),
                region: None,
                endpoint: Some(base.to_string()),
                profile: None,
                credentials: S3Credentials::Stored,
            },
            Some(S3Keys { access_key_id: "AKID".into(), secret_access_key: "secret".into() }),
        )
        .unwrap()
    }

    fn client() -> reqwest::Client {
        provider();
        reqwest::Client::new()
    }

    #[test]
    fn a_chunk_is_held_to_a_smaller_size_than_a_manifest_and_both_exceed_what_is_written() {
        assert_eq!(object_limit("chunks/ab/abcdef"), MAX_CHUNK_BYTES);
        assert_eq!(object_limit("snapshots/2026-11-14T030000Z.snapshot"), MAX_OBJECT_BYTES);
        assert_eq!(object_limit("jatayu-backup.json"), MAX_OBJECT_BYTES);
        // What a backup writes: a megabyte cut of a file, and the seal's few
        // dozen bytes on top.
        const {
            assert!(crate::backup::run::FILE_CHUNK as u64 + 1024 < MAX_CHUNK_BYTES);
            assert!(MAX_CHUNK_BYTES < MAX_OBJECT_BYTES);
        }
    }

    #[tokio::test]
    async fn an_answer_is_read_up_to_its_limit_and_no_further() {
        let app = Router::new()
            .route("/exact", get(|| async { vec![1u8; MIB] }))
            .route("/over", get(|| async { vec![1u8; MIB + 1] }))
            .route("/endless", get(|| async { endless(300) }));
        let base = serve(app).await;
        let http = client();
        let limit = MIB as u64;

        let exact = creds::send(http.get(format!("{base}/exact")), limit).await.unwrap();
        assert_eq!(exact.body.len(), MIB, "an answer exactly as long as may be is read whole");

        // Declared too long in advance: refused without reading any of it.
        let over = creds::send(http.get(format!("{base}/over")), limit).await.err().unwrap();
        assert!(matches!(over, SendError::TooLarge { limit: l } if l == limit), "{over:?}");
        assert!(!over.transient() && !over.unreachable(), "asking again would not help");

        // No length declared: counted as it arrives, and dropped at the limit
        // rather than after three hundred megabytes.
        let endless = creds::send(http.get(format!("{base}/endless")), limit).await.err().unwrap();
        assert!(matches!(endless, SendError::TooLarge { .. }), "{endless:?}");
        assert!(endless.cause().contains("more than 1 MB"), "{}", endless.cause());
    }

    #[tokio::test]
    async fn a_refusal_is_kept_short_rather_than_refused() {
        let app = Router::new().route(
            "/refused",
            get(|| async { (StatusCode::SERVICE_UNAVAILABLE, vec![b'x'; 2 * MIB]) }),
        );
        let base = serve(app).await;
        let reply =
            creds::send(client().get(format!("{base}/refused")), MAX_REPLY_BYTES).await.unwrap();
        assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE, "what happened is still said");
        assert_eq!(reply.body.len(), 64 * 1024, "and the explanation is cut");
    }

    /// The decoder is the reason a few kilobytes can become a hundred
    /// gigabytes, so the client asks for nothing compressed and, if a server
    /// sends it anyway, does not inflate it -- and the cap holds for a client
    /// that does.
    #[tokio::test]
    async fn a_gzip_bomb_is_neither_asked_for_nor_inflated() {
        let bomb = gzip_of_zeros(40);
        assert!(bomb.len() < MIB, "{} bytes", bomb.len());
        let asked: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        // Answers to any address: the bucket's own, and `/bomb` for the plain client.
        let app = Router::new().route(
            "/{*path}",
            get({
                let (asked, bomb) = (asked.clone(), bomb.clone());
                move |headers: HeaderMap| {
                    let (asked, bomb) = (asked.clone(), bomb.clone());
                    async move {
                        let said = headers
                            .get(header::ACCEPT_ENCODING)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        asked.lock().unwrap().push(said);
                        ([(header::CONTENT_ENCODING, "gzip")], bomb)
                    }
                }
            }),
        );
        let base = serve(app).await;
        let url = format!("{base}/bomb");

        // The client a backup uses, whole: through a destination's `get`.
        let target = s3_at(&base);
        let got = target.get("chunks/ab/abcdef").await.unwrap().unwrap();
        assert_eq!(&got[..], &bomb[..], "the bytes as sent, not what they inflate to");
        assert!(
            asked
                .lock()
                .unwrap()
                .iter()
                .all(|said| said.as_deref().is_none_or(|v| !v.contains("gzip"))),
            "a backup asks for no compression: {:?}",
            asked.lock().unwrap()
        );

        // A client that does inflate is stopped by the count, at the size the
        // object may be and not at forty mebibytes.
        let inflating = reqwest::Client::builder().gzip(true).build().unwrap();
        let stopped = creds::send(inflating.get(&url), MAX_CHUNK_BYTES).await;
        assert!(matches!(stopped, Err(SendError::TooLarge { .. })), "inflated past the limit");
    }

    #[tokio::test]
    async fn a_bucket_cannot_make_a_chunk_larger_than_a_chunk_can_be() {
        let app = Router::new().route(
            "/bucket/{*key}",
            // One byte more than a chunk may be, for whatever is asked for: for
            // a manifest, that is well within what one may be.
            get(|| async { vec![3u8; MAX_CHUNK_BYTES as usize + 1] }),
        );
        let target = s3_at(&serve(app).await);

        let chunk = target.get("chunks/ab/abcdef").await.unwrap_err();
        assert_eq!(chunk.kind, TargetErrorKind::Other, "{chunk}");
        assert!(chunk.message.contains("more than 16 MB"), "{chunk}");
        assert!(chunk.message.contains("bucket"), "{chunk}");

        let manifest = target.get("snapshots/2026-11-14T030000Z.snapshot").await.unwrap().unwrap();
        assert_eq!(manifest.len(), MAX_CHUNK_BYTES as usize + 1);
    }

    #[tokio::test]
    async fn a_listing_that_never_ends_is_given_up_on() {
        let pages = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app = Router::new().route(
            "/bucket",
            get({
                let pages = pages.clone();
                move || {
                    let page = pages.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    // A different token every time, so that "the same page
                    // twice" never catches it.
                    async move {
                        format!(
                            "<ListBucketResult><IsTruncated>true</IsTruncated>\
                             <NextContinuationToken>page-{page}</NextContinuationToken>\
                             <Contents><Key>chunks/ab/{page:064x}</Key></Contents>\
                             </ListBucketResult>"
                        )
                    }
                }
            }),
        );
        let target = s3_at(&serve(app).await);
        let error = target.list("chunks/").await.unwrap_err();
        assert!(error.message.contains(&MAX_LIST_PAGES.to_string()), "{error}");
        assert!(error.message.contains("not followed further"), "{error}");
        assert_eq!(
            pages.load(std::sync::atomic::Ordering::SeqCst),
            MAX_LIST_PAGES,
            "it stops at the cap, and does not ask once more"
        );
    }

    #[tokio::test]
    async fn a_file_in_a_folder_destination_is_not_read_past_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        let target = Target::folder(dir.path());
        std::fs::create_dir_all(dir.path().join("chunks/ab")).unwrap();
        let chunk = dir.path().join("chunks/ab/abcdef");

        // Exactly as large as may be: read.
        std::fs::File::create(&chunk).unwrap().set_len(MAX_CHUNK_BYTES).unwrap();
        assert_eq!(
            target.get("chunks/ab/abcdef").await.unwrap().unwrap().len() as u64,
            MAX_CHUNK_BYTES
        );

        // One byte more: refused, unread, and said.
        std::fs::File::create(&chunk).unwrap().set_len(MAX_CHUNK_BYTES + 1).unwrap();
        let refused = target.get("chunks/ab/abcdef").await.unwrap_err();
        assert_eq!(refused.kind, TargetErrorKind::Other);
        assert!(refused.message.contains("larger than any object a backup stores"), "{refused}");

        // Not there at all is still `None`, not an error.
        assert!(target.get("chunks/ab/nothing").await.unwrap().is_none());
    }
}
