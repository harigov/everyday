//! Backups end to end, against a real encrypted vault on SQLite and a folder
//! destination: what a copy holds, what a second copy shares with the first,
//! what pruning lets go of, and that what comes back opens with the password
//! and holds what went in.

use std::path::Path;
use std::sync::Arc;

use karmayoga_core::backup::{BackupKeys, Destination};
use karmayoga_core::note::Note;
use karmayoga_core::{Vault, VaultConfig};

use super::manifest::Area;
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
        kdf: karmayoga_core::crypto::KdfParams::insecure_fast(),
        ..Default::default()
    };
    Arc::new(karmayoga_vault::create(dir, cfg).unwrap())
}

fn plan(destination: &Path, keep: u32) -> Plan {
    Plan {
        destination: Destination::Folder { path: destination.display().to_string() },
        keep,
        config_dir: None,
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
                password: Some(PASSWORD.into()),
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(restore.snapshot, report.snapshot);

        assert!(into.join("spool/recordings/r1/mic-0.chunk").is_file());
        assert!(!into.join("models").exists(), "downloaded models are not the vault's");
        let back = karmayoga_vault::open(&into).unwrap();
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
    _store: &dyn karmayoga_core::store::JournalStore,
    blob: karmayoga_core::id::BlobId,
) -> std::path::PathBuf {
    let id = blob.to_hex();
    karmayoga_core::backup::list_files(&home.join("store/media"), "", &|_| false)
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
        let err = super::restore::open_repository(&target, Some("wrong")).await.unwrap_err();
        assert!(matches!(err, BackupError::Vault(karmayoga_core::Error::BadPassword)), "{err}");
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
                password: Some(PASSWORD.into()),
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
                password: Some(PASSWORD.into()),
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
    assert!(out("webview-carried-over"));
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
    let header: karmayoga_core::vault::VaultHeader =
        serde_json::from_slice(&v.backup_header().unwrap()).unwrap();
    let from_header = BackupKeys::from_header(&header, Some(PASSWORD)).unwrap();
    let live = v.backup_keys().unwrap();
    assert_eq!(from_header.repository_id(), live.repository_id());
    assert_eq!(from_header.chunk_name(b"x"), live.chunk_name(b"x"));
    assert!(BackupKeys::from_header(&header, Some("nope")).is_err());
}

#[test]
fn next_due_follows_the_last_success_and_waits_after_a_failure() {
    use karmayoga_core::backup::{BackupSettings, BackupStatus};
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
    use karmayoga_core::backup::{BackupSettings, S3Credentials, S3Keys};
    let home = tempfile::tempdir().unwrap();
    let v = vault(home.path(), PASSWORD);
    assert!(v.supports_backup());
    assert_eq!(v.backup_settings().unwrap(), BackupSettings::default());

    let saved = v
        .save_backup_settings(&BackupSettings {
            enabled: true,
            destination: Some(Destination::S3 {
                bucket: " my-backups ".into(),
                prefix: "/karmayoga/".into(),
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
    assert_eq!(saved.destination.as_ref().unwrap().label(), "s3://my-backups/karmayoga");
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
        karmayoga_vault::create(dir, VaultConfig { password: None, ..Default::default() }).unwrap(),
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
    use karmayoga_core::backup::{BackupSettings, BackupStatus};
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
    let report = karmayoga_core::backup::BackupReport::default();
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
    let parent = home.path().join(karmayoga_core::vault::BACKUP_STAGING_DIRNAME);
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
    std::fs::write(home.path().join(karmayoga_core::vault::BACKUP_STAGING_DIRNAME), b"").unwrap();
    rt().block_on(async {
        assert!(super::run(v.clone(), plan(dest.path(), 7), Progress::new(false)).await.is_err());
        let target = Target::folder(dest.path());
        assert!(target.get(repo::LOCK).await.unwrap().is_none());
    });
}
