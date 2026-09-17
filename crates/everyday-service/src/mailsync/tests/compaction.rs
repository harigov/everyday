use everyday_core::MailQuery;

use crate::mailsync::discovery::LabelMailboxes;
use crate::mailsync::ingest::ThreadIndex;
use crate::mailsync::passes::{self, SyncContext};

use super::fixtures::*;
// ---------------------------------------------------------------------
// Compaction, wired through the account task
// ---------------------------------------------------------------------

/// A `stop` channel that never fires, for a `compact_account` call a test
/// does not mean to interrupt.
fn never_stops() -> tokio::sync::watch::Receiver<bool> {
    tokio::sync::watch::channel(false).1
}

/// End to end, through exactly the sequence `mailsync::task::compact_account`
/// runs: sync ten messages, delete half of them on the server and sync
/// again (marking their pack frames dead without reclaiming anything), then
/// compact. The pack store actually shrinks, every survivor's raw bytes
/// read back identical via its (remapped) ref, and search -- untouched by
/// any of this, since compaction changes no message id -- still finds
/// exactly the survivors.
#[tokio::test]
async fn compacting_through_the_account_task_shrinks_packs_and_keeps_survivors_searchable() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let mut uids = Vec::new();
    let raws: Vec<Vec<u8>> = (0..10)
        .map(|i| {
            raw_message(
                &format!("msg{i}@example.com"),
                None,
                "a@example.com",
                &format!("Subject {i}"),
                "01 Jan 2024 10:00:00 +0000",
                "findableword",
            )
        })
        .collect();
    {
        let mut s = server.lock().unwrap();
        for raw in &raws {
            uids.push(s.append("INBOX", raw.clone(), flags_seen(), None));
        }
    }
    let mut session = FakeMailSession::new(server.clone());
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    // Delete half the messages on the server, then sync again -- marks
    // their pack frames dead; nothing is reclaimed yet.
    {
        let mut s = server.lock().unwrap();
        for uid in &uids[..5] {
            s.remove("INBOX", *uid);
        }
    }
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let stop = never_stops();
    let summary = crate::mailsync::task::compact_account(&vault, &ctx.packs, account_id, &stop)
        .await
        .unwrap()
        .expect("half the account's one pack being dead must make compaction worthwhile");
    assert!(summary.bytes_reclaimed > 0, "the pack files must shrink");
    assert_eq!(summary.packs_rewritten, 1);

    for (i, raw) in raws.iter().enumerate().skip(5) {
        let msg = vault
            .message_by_message_id_header(account_id, &format!("msg{i}@example.com"))
            .unwrap()
            .expect("a survivor must still be a row");
        assert_eq!(
            &svc.packs().unwrap().read(&msg.pack).unwrap(),
            raw,
            "a survivor's raw bytes must read back identical through its remapped ref"
        );
    }
    let hits =
        svc.mail_index().unwrap().search(&MailQuery::parse("findableword"), 10, None).unwrap();
    assert_eq!(hits.hits.len(), 5, "search must still find exactly the survivors");
}

/// A crash between `compact` returning and the caller committing its remap
/// -- simulated by calling `PackStore::compact` directly and simply never
/// calling `remap_packs` or `drop_packs` on the result, the way a killed
/// process would leave things. Nothing is lost: every message is still
/// readable at its original, untouched address. Running the *whole*
/// production sequence again picks the same still-referenced pack back up
/// and finishes compacting it properly; the pack this first, abandoned
/// attempt wrote is itself only reclaimed by a *third* run, once something
/// newer than it exists to prove it is no longer the account's current
/// pack -- see `packstore`'s own module docs on why the account's newest
/// pack is never swept on absence from `referenced` alone.
#[tokio::test]
async fn a_crash_between_compact_and_remap_packs_loses_nothing_and_eventually_reclaims_the_orphan()
{
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let mut uids = Vec::new();
    {
        let mut s = server.lock().unwrap();
        for i in 0..3 {
            uids.push(s.append(
                "INBOX",
                raw_message(
                    &format!("m{i}@example.com"),
                    None,
                    "a@example.com",
                    "Subject",
                    "01 Jan 2024 10:00:00 +0000",
                    "x",
                ),
                flags_seen(),
                None,
            ));
        }
    }
    let mut session = FakeMailSession::new(server.clone());
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();
    {
        let mut s = server.lock().unwrap();
        s.remove("INBOX", uids[0]);
    }
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let survivors: Vec<_> = (1..3)
        .map(|i| {
            let id = vault
                .message_by_message_id_header(account_id, &format!("m{i}@example.com"))
                .unwrap()
                .unwrap()
                .id;
            let raw = raw_message(
                &format!("m{i}@example.com"),
                None,
                "a@example.com",
                "Subject",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            );
            (id, raw)
        })
        .collect();
    let original_refs: Vec<_> =
        survivors.iter().map(|(id, _)| vault.mail_message(*id).unwrap().pack).collect();

    // The crash: `compact` runs and writes its replacement pack durably,
    // but neither `remap_packs` nor `drop_packs` is ever called.
    let snapshot = vault.mail_referenced_snapshot(ctx.packs.as_ref(), account_id).unwrap();
    let crashed = ctx.packs.compact(&account_id.to_string(), &snapshot, &|| true).unwrap();
    assert!(!crashed.remap.is_empty(), "the test needs compaction to have actually run");
    let abandoned_ref = crashed.remap[0].1.clone();

    // Nothing lost: every survivor still reads at its original address --
    // the vault's own rows were never touched.
    for ((id, raw), original) in survivors.iter().zip(&original_refs) {
        assert_eq!(vault.mail_message(*id).unwrap().pack, *original);
        assert_eq!(&ctx.packs.read(original).unwrap(), raw);
    }

    // Run again, the whole production sequence this time: the still-
    // referenced pack compacts cleanly, and every survivor is readable at
    // its (now genuinely committed) new address.
    let stop = never_stops();
    let second = crate::mailsync::task::compact_account(&vault, &ctx.packs, account_id, &stop)
        .await
        .unwrap()
        .expect("still over the dead threshold");
    assert!(second.packs_rewritten >= 1);
    for (id, raw) in &survivors {
        let refreshed = vault.mail_message(*id).unwrap();
        assert_eq!(&ctx.packs.read(&refreshed.pack).unwrap(), raw);
    }
    // The first, abandoned attempt's pack is not reclaimed by this run --
    // it was still the account's newest pack when this run's own snapshot
    // was taken, so the guard that protects a pack still possibly open for
    // appends protected it too.
    assert!(ctx.packs.read(&abandoned_ref).is_ok(), "not yet reclaimed after only one more run");

    // A third run: now that the second run's own replacement pack is
    // newer, the first attempt's leftover is finally just an ordinary
    // orphan. Nothing was left worth compacting again by this point, so
    // this is purely an orphan sweep.
    crate::mailsync::task::compact_account(&vault, &ctx.packs, account_id, &stop).await.unwrap();
    assert!(
        ctx.packs.read(&abandoned_ref).is_err(),
        "the doubly-orphaned pack from the crashed first attempt must be reclaimed by now"
    );
    for (id, raw) in &survivors {
        let refreshed = vault.mail_message(*id).unwrap();
        assert_eq!(&ctx.packs.read(&refreshed.pack).unwrap(), raw);
    }
}

/// A crash between `remap_packs` committing and `drop_packs` running --
/// simulated by calling both by hand and simply skipping the second.
/// Unlike the compact-then-crash case above, the leftover pack here is
/// reclaimed by the very next run: `referenced` already names the new
/// pack, so the leftover is no longer the account's newest and is an
/// ordinary orphan from the start.
#[tokio::test]
async fn a_crash_between_remap_packs_and_drop_packs_reclaims_the_leftover_on_the_next_run() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let mut uids = Vec::new();
    {
        let mut s = server.lock().unwrap();
        for i in 0..3 {
            uids.push(s.append(
                "INBOX",
                raw_message(
                    &format!("m{i}@example.com"),
                    None,
                    "a@example.com",
                    "Subject",
                    "01 Jan 2024 10:00:00 +0000",
                    "y",
                ),
                flags_seen(),
                None,
            ));
        }
    }
    let mut session = FakeMailSession::new(server.clone());
    let statuses = svc.mail_statuses().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: svc.mail_unread_cache(),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();
    {
        let mut s = server.lock().unwrap();
        s.remove("INBOX", uids[0]);
    }
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let survivors: Vec<_> = (1..3)
        .map(|i| {
            let id = vault
                .message_by_message_id_header(account_id, &format!("m{i}@example.com"))
                .unwrap()
                .unwrap()
                .id;
            let raw = raw_message(
                &format!("m{i}@example.com"),
                None,
                "a@example.com",
                "Subject",
                "01 Jan 2024 10:00:00 +0000",
                "y",
            );
            (id, raw)
        })
        .collect();

    let snapshot = vault.mail_referenced_snapshot(ctx.packs.as_ref(), account_id).unwrap();
    let result = ctx.packs.compact(&account_id.to_string(), &snapshot, &|| true).unwrap();
    assert!(!result.remap.is_empty());
    vault.remap_packs(account_id, &result.remap).unwrap();
    // The crash: `drop_packs` never runs.

    for (id, raw) in &survivors {
        let refreshed = vault.mail_message(*id).unwrap();
        assert_eq!(&ctx.packs.read(&refreshed.pack).unwrap(), raw, "the committed remap must read");
    }

    let stop = never_stops();
    crate::mailsync::task::compact_account(&vault, &ctx.packs, account_id, &stop).await.unwrap();

    // The leftover pack (`result.remap`'s old addresses all shared one) is
    // gone: `referenced` already named the new pack at the moment the
    // remap was committed, so it was never the account's newest and needed
    // no second run to be treated as an ordinary orphan.
    assert!(
        ctx.packs.read(&result.remap[0].0).is_err(),
        "the leftover pack must be reclaimed on the very next run"
    );
    for (id, raw) in &survivors {
        let refreshed = vault.mail_message(*id).unwrap();
        assert_eq!(&ctx.packs.read(&refreshed.pack).unwrap(), raw);
    }
}

/// The rate limit: two attempts inside the same hour must compact only
/// once. Tested against [`crate::mailsync::task::compaction_due`] directly, with
/// synthetic [`std::time::Instant`]s rather than a real hour of wall-clock
/// time.
#[test]
fn the_rate_limit_allows_one_attempt_an_hour() {
    use std::time::{Duration, Instant};
    let now = Instant::now();

    // `None` -- a freshly started task, or the first idle moment after an
    // unlock -- is always due.
    assert!(crate::mailsync::task::compaction_due(None, now));

    // An attempt a moment ago is not due again yet.
    let just_now = now;
    assert!(!crate::mailsync::task::compaction_due(Some(just_now), now + Duration::from_secs(1)));
    assert!(!crate::mailsync::task::compaction_due(
        Some(just_now),
        now + Duration::from_secs(60 * 60 - 1)
    ));

    // An attempt an hour (or more) ago is due again.
    assert!(crate::mailsync::task::compaction_due(
        Some(just_now),
        now + Duration::from_secs(60 * 60)
    ));
    assert!(crate::mailsync::task::compaction_due(
        Some(just_now),
        now + Duration::from_secs(2 * 60 * 60)
    ));
}
