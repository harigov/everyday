use std::sync::Arc;

use everyday_core::account::{Account, AccountStatus, AuthMethod, Provider};
use everyday_core::mail::MailboxRole;
use everyday_core::store::mail::ThreadFilter;
use everyday_core::{MailQuery, VaultConfig};
use everyday_mail::session::{Credential, GmailMeta, MailSession, Result as SessionResult};

use crate::mailsync::discovery::{self, LabelMailboxes};
use crate::mailsync::ingest::ThreadIndex;
use crate::mailsync::passes::{self, SyncContext};

use super::fixtures::*;
// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_first_sync_lands_everything_with_correct_threads_and_a_searchable_index() {
    let env = TestEnv::new();
    let server = plain_server();
    const PER_MAILBOX: usize = 666; // ~2,000 messages across INBOX, Sent, Archive.
    {
        let mut s = server.lock().unwrap();
        for mailbox in ["INBOX", "Sent", "Archive"] {
            for i in 0..PER_MAILBOX {
                let date = format!("{:02} Jan 2024 10:00:00 +0000", (i % 27) + 1);
                let raw = raw_message(
                    &format!("{mailbox}-{i}@example.com"),
                    None,
                    "alice@example.com",
                    &format!("Report {i}"),
                    &date,
                    &format!("Body of message {i}, mentioning marmalade{}.", i % 5),
                );
                s.append(mailbox, raw, flags_seen(), None);
            }
        }
        // A reply chain, split across two mailboxes the way Sent/Archive
        // copies of a real conversation would be.
        let root = raw_message(
            "thread-root@example.com",
            None,
            "bob@example.com",
            "Lunch on Friday",
            "01 Jan 2024 09:00:00 +0000",
            "Are you free Friday?",
        );
        s.append("INBOX", root, flags_seen(), None);
        let reply = raw_message(
            "thread-reply@example.com",
            Some("thread-root@example.com"),
            "me@example.com",
            "Re: Lunch on Friday",
            "01 Jan 2024 09:30:00 +0000",
            "Yes, works for me.",
        );
        s.append("Sent", reply, flags_seen(), None);
    }

    let mut session = FakeMailSession::new(server);
    let started = std::time::Instant::now();
    let mailboxes = env.sync(&mut session).await;
    let elapsed = started.elapsed();
    let total_messages = PER_MAILBOX * 3 + 2;
    eprintln!(
        "first sync of {total_messages} messages across 3 mailboxes took {elapsed:?} \
         ({:.0} messages/s)",
        total_messages as f64 / elapsed.as_secs_f64().max(0.001)
    );
    assert_eq!(mailboxes.len(), 3, "INBOX, Sent, Archive");

    let inbox = mailboxes.iter().find(|m| m.row.role == MailboxRole::Inbox).unwrap();
    let page =
        env.vault.list_threads(inbox.row.id, &ThreadFilter::default(), None, 10_000).unwrap();
    // `PER_MAILBOX` standalone messages plus the shared reply thread.
    assert_eq!(
        page.threads.len(),
        PER_MAILBOX + 1,
        "every message landed as its own or a shared thread"
    );

    let root_message = env
        .vault
        .message_by_message_id_header(env.account_id, "thread-root@example.com")
        .unwrap()
        .expect("root message stored");
    let reply_message = env
        .vault
        .message_by_message_id_header(env.account_id, "thread-reply@example.com")
        .unwrap()
        .expect("reply message stored");
    assert_eq!(
        root_message.thread_id, reply_message.thread_id,
        "a reply and its parent must land in the same thread even though they were fetched from different mailboxes"
    );
    let (thread, messages) = env.vault.thread(root_message.thread_id).unwrap();
    assert_eq!(thread.message_count, 2);
    assert_eq!(messages.len(), 2);

    // The index is searchable.
    let hits = env.index.search(&MailQuery::parse("marmalade2"), 20, None).unwrap();
    assert!(!hits.hits.is_empty(), "a word from the body should be findable");
}

#[tokio::test]
async fn a_process_killed_mid_pass_resumes_without_duplicates() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        for i in 0..40 {
            let raw = raw_message(
                &format!("m{i}@example.com"),
                None,
                "alice@example.com",
                &format!("Subject {i}"),
                "01 Jan 2024 10:00:00 +0000",
                "hello there",
            );
            s.append("INBOX", raw, flags_seen(), None);
        }
    }

    // First "attempt": headers land, but the process is killed before
    // bodies are fetched.
    let mut session = FakeMailSession::new(server.clone());
    let mut labels = LabelMailboxes::new(&env.vault, env.account_id);
    let mut threads = ThreadIndex::new();
    let mut mailboxes =
        discovery::discover(&env.vault, env.account_id, &mut session).await.unwrap();
    for mailbox in &mut mailboxes {
        passes::sync_headers(&env.ctx(), &mut session, mailbox, &mut labels, &mut threads)
            .await
            .unwrap();
    }
    let inbox = mailboxes.iter().find(|m| m.row.role == MailboxRole::Inbox).unwrap();
    let uids_after_headers = env.vault.mail_uid_set(inbox.row.id).unwrap();
    assert_eq!(uids_after_headers.len(), 40);

    // A fresh "process" resumes: a brand new session and index state, same
    // vault. A full sweep must not create duplicate rows and must finish
    // fetching every body.
    let mut session2 = FakeMailSession::new(server);
    let mailboxes2 = env.sync(&mut session2).await;
    let inbox2 = mailboxes2.iter().find(|m| m.row.role == MailboxRole::Inbox).unwrap();

    let page = env.vault.list_threads(inbox2.row.id, &ThreadFilter::default(), None, 1000).unwrap();
    assert_eq!(page.threads.len(), 40, "no duplicates from resuming");

    for uid in env.vault.mail_uid_set(inbox2.row.id).unwrap() {
        let message = env.vault.message_by_uid(inbox2.row.id, uid).unwrap().unwrap();
        assert!(
            !crate::mailsync::ingest::is_pending(&message.pack),
            "uid {uid} should have a real body by now"
        );
    }
}

/// Regression: `bodies_pass` used to upsert the whole `Message` snapshot
/// `pending_messages` took *before* the network fetch, so a local write
/// landing in the window between the headers pass creating a row and the
/// bodies pass finally reaching it -- a person reading the message, or the
/// model setting its category, during a big first sync -- was silently
/// reverted back to whatever the headers pass originally saw. This drives
/// `sync_headers` and `bodies_pass` separately (rather than through
/// `sync_once`, which runs them back to back with nothing in between) so
/// the write can land in exactly that window.
#[tokio::test]
async fn a_flag_change_during_the_bodies_pass_survives_it() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message(
                "body-race@example.com",
                None,
                "a@example.com",
                "Hi",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }
    let mut session = FakeMailSession::new(server);
    let mut labels = LabelMailboxes::new(&env.vault, env.account_id);
    let mut threads = ThreadIndex::new();
    let mut mailboxes =
        discovery::discover(&env.vault, env.account_id, &mut session).await.unwrap();
    for mailbox in &mut mailboxes {
        passes::sync_headers(&env.ctx(), &mut session, mailbox, &mut labels, &mut threads)
            .await
            .unwrap();
    }
    let inbox = mailboxes.iter().find(|m| m.row.role == MailboxRole::Inbox).unwrap();
    let uid = env.vault.mail_uid_set(inbox.row.id).unwrap()[0];
    let pending = env.vault.message_by_uid(inbox.row.id, uid).unwrap().unwrap();
    assert!(crate::mailsync::ingest::is_pending(&pending.pack), "the body must not be fetched yet");

    // The write this test's own regression is about has to land genuinely
    // *during* the pass -- after `pending_messages`'s own snapshot at the
    // top of `bodies_pass`, before the batch's ingest uses it -- not
    // merely before this call starts, which the fix's own re-read would
    // already see correctly either way. `tokio::join!` polls its two
    // futures in order on a current-thread runtime: `bodies_pass` runs
    // synchronously (this fake session never otherwise suspends) until
    // `raw`'s own real `yield_now` -- see that method's own docs -- at
    // which point the write closure below, having nothing of its own to
    // await, runs to completion before `bodies_pass` is ever polled again.
    let write_race = async {
        env.vault
            .update_message_flags(
                inbox.row.id,
                uid,
                everyday_core::mail::MessageFlags { seen: true, ..Default::default() },
            )
            .unwrap();
    };
    let ctx = env.ctx();
    let (result, ()) = tokio::join!(passes::bodies_pass(&ctx, &mut session, inbox), write_race);
    result.unwrap();

    let after = env.vault.message_by_uid(inbox.row.id, uid).unwrap().unwrap();
    assert!(after.flags.seen, "a flag change during the bodies pass must survive it");
    assert!(
        !crate::mailsync::ingest::is_pending(&after.pack),
        "the body must still have been fetched despite the concurrent write"
    );
}

#[tokio::test]
async fn a_flag_change_a_deletion_and_a_new_message_apply_incrementally() {
    let env = TestEnv::new();
    let server = plain_server();
    let uid_to_delete;
    let uid_to_flag;
    {
        let mut s = server.lock().unwrap();
        uid_to_delete = s.append(
            "INBOX",
            raw_message(
                "del@example.com",
                None,
                "a@example.com",
                "Bye",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
        uid_to_flag = s.append(
            "INBOX",
            raw_message(
                "flag@example.com",
                None,
                "a@example.com",
                "Flag me",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    let mailboxes = env.sync(&mut session).await;
    let inbox = mailboxes.iter().find(|m| m.row.role == MailboxRole::Inbox).unwrap();
    assert_eq!(env.vault.mail_uid_set(inbox.row.id).unwrap().len(), 2);

    {
        let mut s = server.lock().unwrap();
        s.remove("INBOX", uid_to_delete);
        s.set_flags(
            "INBOX",
            uid_to_flag,
            everyday_mail::session::Flags::SEEN | everyday_mail::session::Flags::FLAGGED,
        );
        s.append(
            "INBOX",
            raw_message(
                "new@example.com",
                None,
                "a@example.com",
                "New",
                "02 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }
    let mailboxes2 = env.sync(&mut session).await;
    let inbox2 = mailboxes2.iter().find(|m| m.row.role == MailboxRole::Inbox).unwrap();

    let remaining = env.vault.mail_uid_set(inbox2.row.id).unwrap();
    assert_eq!(remaining.len(), 2, "one deleted, one added, net still two");
    assert!(!remaining.contains(&uid_to_delete), "the deleted message should be gone");

    let flagged = env.vault.message_by_uid(inbox2.row.id, uid_to_flag).unwrap().unwrap();
    assert!(flagged.flags.seen, "the flag change should have applied");
    assert!(flagged.flags.flagged);

    let new_message = env
        .vault
        .message_by_message_id_header(env.account_id, "new@example.com")
        .unwrap()
        .expect("the new message should have arrived");
    assert!(
        !crate::mailsync::ingest::is_pending(&new_message.pack),
        "its body should have been fetched too"
    );
}

/// Regression: every uid `changes_since` reported in `flag_changes` used
/// to be written back unconditionally, even when the flags it reported
/// already matched what was stored -- a full rewrite (decrypt, re-seal,
/// upsert, thread recompute) for nothing. The fake session, like a real
/// Gmail server on a label-only change, can bump a message's own `MODSEQ`
/// without changing any of the five IMAP flags this crate tracks, so this
/// also exercises the secondary bug: `changed` used to be driven by
/// `flag_changes` being non-empty at all, which invalidated the unread
/// cache on every such pass regardless of whether anything a person would
/// notice actually happened.
#[tokio::test]
async fn an_unrelated_modseq_bump_with_unchanged_flags_skips_the_write_and_the_cache() {
    let (svc, vault, account_id, _dir) = service_test_env();
    let server = plain_server();
    let uid;
    {
        let mut s = server.lock().unwrap();
        uid = s.append(
            "INBOX",
            raw_message(
                "modseq-only@example.com",
                None,
                "a@example.com",
                "Hi",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    let statuses = svc.mail_statuses().unwrap();
    let cache = svc.mail_unread_cache().unwrap();
    let ctx = SyncContext {
        vault: &vault,
        account_id,
        packs: svc.packs().unwrap(),
        index: svc.mail_index().unwrap(),
        statuses: &statuses,
        attachment_cap_bytes: None,
        index_commit: passes::CommitPacer::new(),
        unread_cache: Some(cache.clone()),
        contacts: svc.mail_contacts(),
        identities: Vec::new(),
    };
    let mut labels = LabelMailboxes::new(&vault, account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let inbox_id = vault
        .mailboxes(account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap()
        .id;
    let flags_before = vault.message_by_uid(inbox_id, uid).unwrap().unwrap().flags;

    // Prime the cache with a sentinel this test controls -- keyed to
    // `inbox_id` itself, so a later read that still sees the same sentinel
    // proves nothing invalidated it in between.
    let sentinel = vec![(inbox_id, 999)];
    cache.get_or_compute(account_id, || Ok(sentinel.clone())).unwrap();

    // Bump the message's own `MODSEQ` without touching any of its actual
    // flags -- exactly what a Gmail label-only change, or a redundant
    // `STORE`, does.
    {
        let mut s = server.lock().unwrap();
        s.set_flags("INBOX", uid, flags_seen());
    }
    passes::sync_once(&ctx, &mut session, &mut labels, &mut threads).await.unwrap();

    let flags_after = vault.message_by_uid(inbox_id, uid).unwrap().unwrap().flags;
    assert_eq!(flags_before, flags_after, "flags did not actually change");

    let cached = cache
        .get_or_compute(account_id, || panic!("must not recompute: nothing actually changed"))
        .unwrap();
    assert_eq!(cached, sentinel);
}

/// Regression for "removed messages are never marked dead in the pack
/// store, or removed from search": a message the server no longer has must
/// stop being searchable, and its pack frame must actually be reclaimable
/// -- not merely gone from the vault's own `mail_messages` row, which
/// `remove_uids` already got right before this fix.
#[tokio::test]
async fn a_deleted_messages_pack_frame_is_marked_dead_and_dropped_from_search() {
    let env = TestEnv::new();
    let server = plain_server();
    let uid;
    {
        let mut s = server.lock().unwrap();
        uid = s.append(
            "INBOX",
            raw_message(
                "reap@example.com",
                None,
                "a@example.com",
                "Reap me",
                "01 Jan 2024 10:00:00 +0000",
                "marmaladewords",
            ),
            flags_seen(),
            None,
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;

    let message = env
        .vault
        .message_by_message_id_header(env.account_id, "reap@example.com")
        .unwrap()
        .expect("stored");
    assert!(
        !crate::mailsync::ingest::is_pending(&message.pack),
        "its body must have been fetched first"
    );
    let pack_before = message.pack.clone();

    let hits_before = env.index.search(&MailQuery::parse("marmaladewords"), 10, None).unwrap();
    assert!(!hits_before.hits.is_empty(), "indexed before removal");

    {
        let mut s = server.lock().unwrap();
        s.remove("INBOX", uid);
    }
    env.sync(&mut session).await;

    let hits_after = env.index.search(&MailQuery::parse("marmaladewords"), 10, None).unwrap();
    assert!(hits_after.hits.is_empty(), "a removed message must no longer be searchable");

    // `compact` reclaims a pack once a third of it is dead -- trivially
    // true for a pack holding only this one, now-dead, message. It no
    // longer deletes anything itself (see `PackStore::compact`'s own
    // two-step contract), so the test plays the caller's part: a real
    // `ReferencedSnapshot`, built the same way `mailsync::task` builds one,
    // is the honest input -- nothing else references this account's packs
    // any more once the message above was removed, so the snapshot's own
    // `referenced` legitimately comes back empty, and `drop_packs` is what
    // actually reclaims the space.
    let snapshot = env.vault.mail_referenced_snapshot(env.packs.as_ref(), env.account_id).unwrap();
    let result = env.packs.compact(&env.account_id.to_string(), &snapshot, &|| true).unwrap();
    assert!(result.remap.is_empty(), "nothing live was left to remap");
    env.vault.remap_packs(env.account_id, &result.remap).unwrap();
    env.packs.drop_packs(&env.account_id.to_string(), &result.obsolete).unwrap();
    assert!(
        env.packs.read(&pack_before).is_err(),
        "the dead frame's pack must actually have been reclaimed"
    );
}

#[tokio::test]
async fn a_uidvalidity_reset_rematches_without_refetching_raw() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message(
                "stable@example.com",
                None,
                "a@example.com",
                "Stable",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            None,
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    let _mailboxes = env.sync(&mut session).await;
    let before = env
        .vault
        .message_by_message_id_header(env.account_id, "stable@example.com")
        .unwrap()
        .expect("stored");
    assert!(!crate::mailsync::ingest::is_pending(&before.pack));
    let pack_before = before.pack.clone();

    {
        let mut s = server.lock().unwrap();
        s.bump_uidvalidity("INBOX");
    }
    let mailboxes2 = env.sync(&mut session).await;
    let inbox2 = mailboxes2.iter().find(|m| m.row.role == MailboxRole::Inbox).unwrap();

    let after = env
        .vault
        .message_by_message_id_header(env.account_id, "stable@example.com")
        .unwrap()
        .expect("still stored after the reset");
    assert_eq!(after.id, before.id, "the same physical message, not a new row");
    assert_eq!(after.pack, pack_before, "raw bytes were not refetched");

    let uids = env.vault.mail_uid_set(inbox2.row.id).unwrap();
    assert_eq!(uids.len(), 1, "membership was rebuilt under the new uid");
}

/// Regression: an interrupted `UIDVALIDITY` reset used to permanently
/// duplicate every message the crash left stranded. `reset_mailbox` durably
/// zeroes the row's own `UIDVALIDITY`, but the *new* one was only ever held
/// in memory until `sync_headers`'s own completion at the very end -- so a
/// crash any time before that (a dropped connection mid-batch, the task
/// aborted) meant the very next attempt saw a mailbox indistinguishable
/// from one that had simply never been synced, `force_db_rematch` came back
/// `false`, and every remaining message was minted fresh under a new id
/// with a fresh body download, while the original rows survived, orphaned,
/// with no mailbox membership at all.
///
/// This drives the resume with a brand new [`ThreadIndex`] -- the one a
/// freshly restarted task actually has -- rather than reusing the one the
/// interrupted attempt would have been building up, since that in-memory
/// map is exactly what a crash loses.
#[tokio::test]
async fn resuming_an_interrupted_uidvalidity_reset_does_not_duplicate_messages() {
    let env = TestEnv::new();
    let server = plain_server();
    let message_ids: Vec<String> = (0..3).map(|i| format!("reset{i}@example.com")).collect();
    {
        let mut s = server.lock().unwrap();
        for id in &message_ids {
            s.append(
                "INBOX",
                raw_message(id, None, "a@example.com", "Hi", "01 Jan 2024 10:00:00 +0000", "x"),
                flags_seen(),
                None,
            );
        }
    }
    let mut session = FakeMailSession::new(server.clone());
    let inbox = env
        .sync(&mut session)
        .await
        .into_iter()
        .find(|m| m.row.role == MailboxRole::Inbox)
        .unwrap();
    let original_ids: std::collections::HashSet<_> = message_ids
        .iter()
        .map(|id| env.vault.message_by_message_id_header(env.account_id, id).unwrap().unwrap().id)
        .collect();
    assert_eq!(original_ids.len(), 3);

    // Bump `UIDVALIDITY` on the server -- every uid this account remembers
    // for `INBOX` is now meaningless.
    {
        let mut s = server.lock().unwrap();
        s.bump_uidvalidity("INBOX");
    }

    // The crash this test stands in for: an earlier attempt got as far as
    // `sync_headers`'s own durable reset write (`reset_mailbox`, plus
    // stamping the new `UIDVALIDITY` with `uidnext` left at its `0`
    // sentinel -- see that function's own docs) but never reached the
    // headers loop at all, let alone its own completion.
    let state = session.select("INBOX").await.unwrap();
    env.vault.reset_mailbox(inbox.row.id).unwrap();
    let mut interrupted_row = inbox.row.clone();
    interrupted_row.uidvalidity = state.uidvalidity;
    interrupted_row.uidnext = 0;
    interrupted_row.highest_modseq = 0;
    env.vault.save_mailbox(&interrupted_row).unwrap();

    // The resume: rediscover the mailbox (picking the row back up exactly
    // as the interrupted attempt left it) and run headers with a fresh
    // `ThreadIndex`.
    let mut labels = LabelMailboxes::new(&env.vault, env.account_id);
    let mut threads = ThreadIndex::new();
    let mut mailboxes2 =
        discovery::discover(&env.vault, env.account_id, &mut session).await.unwrap();
    let inbox2 = mailboxes2.iter_mut().find(|m| m.row.role == MailboxRole::Inbox).unwrap();
    passes::sync_headers(&env.ctx(), &mut session, inbox2, &mut labels, &mut threads)
        .await
        .unwrap();

    let uids_after = env.vault.mail_uid_set(inbox2.row.id).unwrap();
    assert_eq!(uids_after.len(), 3, "still exactly three messages after resuming");
    let resumed_ids: std::collections::HashSet<_> = uids_after
        .iter()
        .map(|&uid| env.vault.message_by_uid(inbox2.row.id, uid).unwrap().unwrap().id)
        .collect();
    assert_eq!(
        resumed_ids, original_ids,
        "each message must have been rematched to its original id, not minted fresh"
    );
}

/// Regression: on a server that never reports `UIDNEXT` at all (see
/// [`FakeMailbox::reports_uidnext`]'s own docs), `mailbox.row.uidnext` used
/// to read `0` forever, because `sync_headers`'s own completion write
/// copied the server's -- permanently absent -- value straight in. That
/// made `resuming_an_interrupted_reset` (meant only for a crash mid-reset)
/// true on *every* pass, not just an interrupted one, so an entirely
/// ordinary second sync -- nothing new on the server, no crash, no
/// `UIDVALIDITY` change -- wiped the mailbox's membership and rematched
/// every message by `Message-ID` all over again instead of doing nothing at
/// all.
///
/// The fix derives the completion marker from what this pass itself
/// learned the mailbox holds, rather than trusting the server's optional
/// report of it -- see the doc comment on the write in `sync_headers` this
/// is a regression test for.
#[tokio::test]
async fn a_server_that_never_reports_uidnext_does_not_reset_every_pass() {
    let env = TestEnv::new();
    let server = plain_server();
    let message_ids: Vec<String> = (0..3).map(|i| format!("nouidnext{i}@example.com")).collect();
    {
        let mut s = server.lock().unwrap();
        s.mailbox("INBOX", None).stop_reporting_uidnext();
        for id in &message_ids {
            s.append(
                "INBOX",
                raw_message(id, None, "a@example.com", "Hi", "01 Jan 2024 10:00:00 +0000", "x"),
                flags_seen(),
                None,
            );
        }
    }
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;

    let inbox_after_first = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap();
    assert_ne!(
        inbox_after_first.uidnext, 0,
        "a completed pass must leave uidnext non-zero even when the server never reports one"
    );

    let original_ids: std::collections::HashSet<_> = message_ids
        .iter()
        .map(|id| env.vault.message_by_message_id_header(env.account_id, id).unwrap().unwrap().id)
        .collect();
    let pack_before = env
        .vault
        .message_by_message_id_header(env.account_id, &message_ids[0])
        .unwrap()
        .unwrap()
        .pack;

    // A second, perfectly ordinary pass: nothing new on the server, no
    // crash, no `UIDVALIDITY` change. On a server that reports `UIDNEXT`
    // this is a no-op past the `SELECT`; before the fix, this is exactly
    // the pass that wiped and re-ingested everything above.
    env.sync(&mut session).await;

    let resumed_ids: std::collections::HashSet<_> = message_ids
        .iter()
        .map(|id| env.vault.message_by_message_id_header(env.account_id, id).unwrap().unwrap().id)
        .collect();
    assert_eq!(
        resumed_ids, original_ids,
        "an ordinary second pass must not rematch or re-mint any message"
    );
    let pack_after = env
        .vault
        .message_by_message_id_header(env.account_id, &message_ids[0])
        .unwrap()
        .unwrap()
        .pack;
    assert_eq!(pack_after, pack_before, "raw bytes must not be refetched on an ordinary pass");

    let inbox_after_second = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap();
    let uids = env.vault.mail_uid_set(inbox_after_second.id).unwrap();
    assert_eq!(uids.len(), 3, "membership must not have been wiped and rebuilt a second time");
}

/// Regression: a message moved between mailboxes server-side (INBOX to
/// Archive, say) used to be deleted and reborn under a fresh id the moment
/// both sides of the move were noticed in the same sync round --
/// `sync_headers`'s own vanished-uid reap ran inline, mailbox by mailbox,
/// so the inbox's copy was already gone by the time Archive's headers pass
/// got a chance to rematch it by `Message-ID`. `sync_once` now defers
/// reaping until every mailbox in the round has ingested its own new
/// headers first -- see that function's own docs.
///
/// Uses `sync_once` directly, twice, with the same `labels`/`threads`
/// kept alive across both calls -- exactly the shape `run_account_with`'s
/// own long-lived locals give a real account task across every wake -- so
/// the in-memory half of rematching (`ThreadIndex::seen_by_message_id`)
/// still has this message's id from the first round when the second round
/// sees it reappear elsewhere.
#[tokio::test]
async fn a_message_moved_between_mailboxes_in_one_round_keeps_its_identity() {
    let env = TestEnv::new();
    let server = plain_server();
    let raw = raw_message(
        "moved@example.com",
        None,
        "a@example.com",
        "Moving day",
        "01 Jan 2024 10:00:00 +0000",
        "x",
    );
    let uid;
    {
        let mut s = server.lock().unwrap();
        uid = s.append("INBOX", raw.clone(), flags_seen(), None);
    }
    let mut session = FakeMailSession::new(server.clone());
    let mut labels = LabelMailboxes::new(&env.vault, env.account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&env.ctx(), &mut session, &mut labels, &mut threads).await.unwrap();

    let original = env
        .vault
        .message_by_message_id_header(env.account_id, "moved@example.com")
        .unwrap()
        .expect("stored after the first round");
    assert!(
        !crate::mailsync::ingest::is_pending(&original.pack),
        "its body must already be fetched"
    );

    {
        let mut s = server.lock().unwrap();
        s.remove("INBOX", uid);
        s.append("Archive", raw, flags_seen(), None);
    }
    // Same `labels`/`threads` as the first round -- see this test's own
    // docs.
    passes::sync_once(&env.ctx(), &mut session, &mut labels, &mut threads).await.unwrap();

    let moved = env
        .vault
        .message_by_message_id_header(env.account_id, "moved@example.com")
        .unwrap()
        .expect("still stored after the move");
    assert_eq!(moved.id, original.id, "a message moved between mailboxes must keep its identity");
    assert_eq!(moved.pack, original.pack, "and must not be re-downloaded");

    let inbox = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap();
    let archive = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Archive)
        .unwrap();
    assert!(env.vault.mail_uid_set(inbox.id).unwrap().is_empty(), "gone from the inbox");
    assert_eq!(env.vault.mail_uid_set(archive.id).unwrap().len(), 1, "and filed under Archive");
}

#[tokio::test]
async fn a_message_referencing_two_existing_threads_merges_them() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message(
                "root-a@example.com",
                None,
                "a@example.com",
                "Topic A",
                "01 Jan 2024 09:00:00 +0000",
                "start of A",
            ),
            flags_seen(),
            None,
        );
        s.append(
            "INBOX",
            raw_message(
                "root-b@example.com",
                None,
                "b@example.com",
                "Topic B",
                "01 Jan 2024 09:05:00 +0000",
                "start of B",
            ),
            flags_seen(),
            None,
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;

    let root_a = env
        .vault
        .message_by_message_id_header(env.account_id, "root-a@example.com")
        .unwrap()
        .expect("root a stored");
    let root_b = env
        .vault
        .message_by_message_id_header(env.account_id, "root-b@example.com")
        .unwrap()
        .expect("root b stored");
    assert_ne!(root_a.thread_id, root_b.thread_id, "two unrelated threads to start");

    // A later message whose `References` names both -- a real client that
    // merged the two conversations under one, or an unusual reply chain
    // that the two prior messages themselves never revealed a link between.
    let merging = b"Message-ID: <merges-both@example.com>\r\n\
References: <root-a@example.com> <root-b@example.com>\r\n\
From: c@example.com\r\n\
To: me@example.com\r\n\
Subject: Re: Topic A\r\n\
Date: 01 Jan 2024 09:10:00 +0000\r\n\
Content-Type: text/plain\r\n\r\nties them together\r\n"
        .to_vec();
    {
        let mut s = server.lock().unwrap();
        s.append("INBOX", merging, flags_seen(), None);
    }
    env.sync(&mut session).await;

    let root_a_after = env
        .vault
        .message_by_message_id_header(env.account_id, "root-a@example.com")
        .unwrap()
        .expect("root a still stored");
    let root_b_after = env
        .vault
        .message_by_message_id_header(env.account_id, "root-b@example.com")
        .unwrap()
        .expect("root b still stored");
    let merger = env
        .vault
        .message_by_message_id_header(env.account_id, "merges-both@example.com")
        .unwrap()
        .expect("the merging message stored");
    assert_eq!(
        root_a_after.thread_id, root_b_after.thread_id,
        "the two roots must now share a thread"
    );
    assert_eq!(merger.thread_id, root_a_after.thread_id);

    let (thread, messages) = env.vault.thread(root_a_after.thread_id).unwrap();
    assert_eq!(thread.message_count, 3, "all three messages under the one kept thread");
    assert_eq!(messages.len(), 3);

    let inbox = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap();
    let page = env.vault.list_threads(inbox.id, &ThreadFilter::default(), None, 10).unwrap();
    assert_eq!(page.threads.len(), 1, "the merged-away thread must no longer appear in the list");
}

/// The regression for `ThreadIndex::seen_by_message_id` caching a whole
/// stale [`everyday_core::mail::Message`] rather than just an id: a real
/// `Message` a `ThreadIndex` (kept alive across a whole account task's
/// life, per `mailsync::task::run_account_with`, never per sync attempt)
/// once cached for a message can go stale two ways before that same
/// `Message-ID` is ever seen again -- its body arrives (turning a `pending`
/// pack and an empty snippet into real ones) and a later message's
/// `References` chain merges its thread into another. Both must survive the
/// message turning up again at a new location, which is exactly what moving
/// mailboxes -- or a new `UID` after a `UIDVALIDITY`-free move a plain
/// `IMAP` `MOVE` produces -- does.
#[tokio::test]
async fn a_merged_and_relocated_message_keeps_its_thread_pack_and_snippet() {
    let env = TestEnv::new();
    let server = plain_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            raw_message(
                "root-a@example.com",
                None,
                "a@example.com",
                "Topic A",
                "01 Jan 2024 09:00:00 +0000",
                "start of A",
            ),
            flags_seen(),
            None,
        );
        s.append(
            "INBOX",
            raw_message(
                "root-b@example.com",
                None,
                "b@example.com",
                "Topic B",
                "01 Jan 2024 09:05:00 +0000",
                "the body of B, which the snippet must still say after this wake",
            ),
            flags_seen(),
            None,
        );
    }

    // One `ThreadIndex`, reused across two wakes -- the shape
    // `run_account_with` actually keeps one in for the account task's whole
    // life, never a fresh one per sync attempt the way `TestEnv::sync`'s
    // convenience wrapper does.
    let mut session = FakeMailSession::new(server.clone());
    let mut labels = LabelMailboxes::new(&env.vault, env.account_id);
    let mut threads = ThreadIndex::new();
    passes::sync_once(&env.ctx(), &mut session, &mut labels, &mut threads).await.unwrap();

    let root_b_before = env
        .vault
        .message_by_message_id_header(env.account_id, "root-b@example.com")
        .unwrap()
        .expect("root b stored");
    assert!(
        !crate::mailsync::ingest::is_pending(&root_b_before.pack),
        "the first wake's bodies pass ran"
    );
    assert!(!root_b_before.snippet.is_empty());
    let pack_before = root_b_before.pack.clone();
    let snippet_before = root_b_before.snippet.clone();

    // The second wake: a message merging root a and root b's threads, *and*
    // root b turning up again at a new location -- the two ways this bug
    // needs at once. INBOX is synced before Archive (inbox-first), so the
    // merge below is already applied to the vault by the time root b's
    // `Message-ID` is seen again while syncing Archive, in the same
    // `threads`.
    {
        let mut s = server.lock().unwrap();
        s.append(
            "INBOX",
            b"Message-ID: <merges-both@example.com>\r\n\
References: <root-a@example.com> <root-b@example.com>\r\n\
From: c@example.com\r\n\
To: me@example.com\r\n\
Subject: Re: Topic A\r\n\
Date: 01 Jan 2024 09:10:00 +0000\r\n\
Content-Type: text/plain\r\n\r\nties them together\r\n"
                .to_vec(),
            flags_seen(),
            None,
        );
        s.append(
            "Archive",
            raw_message(
                "root-b@example.com",
                None,
                "b@example.com",
                "Topic B",
                "01 Jan 2024 09:05:00 +0000",
                "the body of B, which the snippet must still say after this wake",
            ),
            flags_seen(),
            None,
        );
    }
    passes::sync_once(&env.ctx(), &mut session, &mut labels, &mut threads).await.unwrap();

    let root_a_after = env
        .vault
        .message_by_message_id_header(env.account_id, "root-a@example.com")
        .unwrap()
        .expect("root a still stored");
    let root_b_after = env
        .vault
        .message_by_message_id_header(env.account_id, "root-b@example.com")
        .unwrap()
        .expect("root b still stored");

    assert_eq!(
        root_b_after.thread_id, root_a_after.thread_id,
        "root b must keep the merged thread, not revert to a stale cached one"
    );
    assert_eq!(root_b_after.pack, pack_before, "root b's pack ref must not be reset to pending");
    assert_eq!(root_b_after.snippet, snippet_before, "root b's real snippet must survive");

    let locations = env.vault.mail_message_locations(root_b_after.id).unwrap();
    assert_eq!(
        locations.len(),
        2,
        "root b must now be filed in both INBOX and Archive: {locations:?}"
    );
}

#[tokio::test]
async fn gmail_labels_put_one_message_in_both_the_inbox_and_a_user_label() {
    let env = TestEnv::new();
    let server = gmail_server();
    {
        let mut s = server.lock().unwrap();
        s.append(
            "All Mail",
            raw_message(
                "labelled@example.com",
                None,
                "a@example.com",
                "Travel",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            Some(GmailMeta { thrid: 1, msgid: 1, labels: vec!["\\Inbox".into(), "Travel".into()] }),
        );
    }
    let mut session = FakeMailSession::new(server);
    let mailboxes = env.sync(&mut session).await;

    // Only the five folder-backed mailboxes are ever synced as such --
    // `INBOX` itself must not appear.
    assert_eq!(mailboxes.len(), 5, "All Mail, Sent, Drafts, Spam, Trash only");
    assert!(mailboxes.iter().all(|m| m.remote_name != "INBOX"));

    let all_accounts_mailboxes = env.vault.mailboxes(env.account_id).unwrap();
    let inbox_label = all_accounts_mailboxes
        .iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .expect("an inbox label mailbox should have been minted");
    let travel_label = all_accounts_mailboxes
        .iter()
        .find(|m| m.remote_name == "Travel")
        .expect("a user label mailbox should have been minted");

    let inbox_page =
        env.vault.list_threads(inbox_label.id, &ThreadFilter::default(), None, 10).unwrap();
    let travel_page =
        env.vault.list_threads(travel_label.id, &ThreadFilter::default(), None, 10).unwrap();
    assert_eq!(inbox_page.threads.len(), 1, "the message appears in the inbox label's list");
    assert_eq!(travel_page.threads.len(), 1, "and in the user label's list");
    assert_eq!(
        inbox_page.threads[0].id, travel_page.threads[0].id,
        "it is the same thread in both, not two copies"
    );
}

/// Steady state: a message archived in another client loses `\Inbox`.
/// `changes_since` reports the modseq bump in `flag_changes`, which is
/// what `refresh_gmail_labels`'s targeted path reads `X-GM-LABELS` for.
#[tokio::test]
async fn archiving_in_another_client_removes_the_message_from_the_inbox_label() {
    let env = TestEnv::new();
    let server = gmail_server();
    let uid;
    {
        let mut s = server.lock().unwrap();
        uid = s.append(
            "All Mail",
            raw_message(
                "archived-elsewhere@example.com",
                None,
                "a@example.com",
                "Will be archived",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            Some(GmailMeta { thrid: 1, msgid: 1, labels: vec!["\\Inbox".into()] }),
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;
    let inbox_label = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .expect("the inbox label mailbox exists after the first sync");
    assert_eq!(
        env.vault
            .list_threads(inbox_label.id, &ThreadFilter::default(), None, 10)
            .unwrap()
            .threads
            .len(),
        1,
        "the message starts in the inbox label's list"
    );

    // Archived elsewhere: `\Inbox` dropped, `MODSEQ` bumped, exactly what a
    // real Gmail label change looks like from this side.
    {
        let mut s = server.lock().unwrap();
        s.set_gmail_labels("All Mail", uid, Vec::new());
    }
    env.sync(&mut session).await;

    let after = env.vault.list_threads(inbox_label.id, &ThreadFilter::default(), None, 10).unwrap();
    assert!(after.threads.is_empty(), "the archived message must leave the inbox label's list");
}

/// The fallback: a label changes with no `MODSEQ` bump at all -- the case
/// `changes_since` cannot diff by construction. Only the newest-N re-check
/// notices it.
#[tokio::test]
async fn a_label_change_with_no_modseq_bump_is_still_caught_by_the_fallback() {
    let env = TestEnv::new();
    let server = gmail_server();
    let uid;
    {
        let mut s = server.lock().unwrap();
        uid = s.append(
            "All Mail",
            raw_message(
                "silently-relabelled@example.com",
                None,
                "a@example.com",
                "Will be relabelled without a modseq bump",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            Some(GmailMeta { thrid: 2, msgid: 2, labels: vec!["\\Inbox".into()] }),
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;
    let inbox_label = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .unwrap();
    assert_eq!(
        env.vault
            .list_threads(inbox_label.id, &ThreadFilter::default(), None, 10)
            .unwrap()
            .threads
            .len(),
        1
    );

    {
        let mut s = server.lock().unwrap();
        s.set_gmail_labels_without_a_modseq_bump("All Mail", uid, Vec::new());
    }
    env.sync(&mut session).await;

    let after = env.vault.list_threads(inbox_label.id, &ThreadFilter::default(), None, 10).unwrap();
    assert!(
        after.threads.is_empty(),
        "the newest-N fallback must catch a label change changes_since never reported"
    );
}

/// Regression: `refresh_gmail_labels`'s added-label branch used to
/// re-ingest the message with its *pre-update* label set, reverting the
/// very label this pass just added and leaving the next pass to "notice"
/// the same difference again, forever. A message un-archived elsewhere
/// (gains `\Inbox`) must land in the inbox label's list, and the row's own
/// `labels` must actually say so -- not just the mailbox membership -- and
/// stay that way after a second, otherwise-quiet sync.
#[tokio::test]
async fn un_archiving_in_another_client_adds_the_message_to_the_inbox_label_and_it_sticks() {
    let env = TestEnv::new();
    let server = gmail_server();
    let uid;
    {
        let mut s = server.lock().unwrap();
        uid = s.append(
            "All Mail",
            raw_message(
                "unarchived-elsewhere@example.com",
                None,
                "a@example.com",
                "Will be un-archived",
                "01 Jan 2024 10:00:00 +0000",
                "x",
            ),
            flags_seen(),
            Some(GmailMeta { thrid: 3, msgid: 3, labels: Vec::new() }),
        );
    }
    let mut session = FakeMailSession::new(server.clone());
    env.sync(&mut session).await;
    // No `\Inbox` label mailbox exists at all yet -- it is minted the first
    // time some message actually carries the label (see `LabelMailboxes`'s
    // own docs), which has not happened yet for an account whose one
    // message starts outside the inbox.
    assert!(
        env.vault.mailboxes(env.account_id).unwrap().iter().all(|m| m.role != MailboxRole::Inbox),
        "no inbox label mailbox should exist before anything has ever carried \\Inbox"
    );

    // Un-archived elsewhere: `\Inbox` added, `MODSEQ` bumped -- the mirror
    // image of the removal case above.
    {
        let mut s = server.lock().unwrap();
        s.set_gmail_labels("All Mail", uid, vec!["\\Inbox".into()]);
    }
    env.sync(&mut session).await;

    let inbox_label = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::Inbox)
        .expect("the addition must mint the inbox label mailbox");
    let after = env.vault.list_threads(inbox_label.id, &ThreadFilter::default(), None, 10).unwrap();
    assert_eq!(
        after.threads.len(),
        1,
        "the un-archived message must land in the inbox label's list"
    );

    let all_mail = env
        .vault
        .mailboxes(env.account_id)
        .unwrap()
        .into_iter()
        .find(|m| m.role == MailboxRole::All)
        .unwrap();
    let stored = env.vault.message_by_uid(all_mail.id, uid).unwrap().unwrap();
    assert_eq!(
        stored.labels,
        vec!["\\Inbox".to_string()],
        "the row's own label set must reflect the addition, not the pre-update snapshot"
    );

    // A further, otherwise-quiet sync must not revert the addition again --
    // exactly what the bug being regressed did, on every single pass.
    env.sync(&mut session).await;
    let stored_again = env.vault.message_by_uid(all_mail.id, uid).unwrap().unwrap();
    assert_eq!(stored_again.labels, vec!["\\Inbox".to_string()]);
}

#[tokio::test]
async fn an_auth_failure_sets_needs_sign_in_and_the_task_stops() {
    let dir = tempfile::tempdir().unwrap();
    let vault =
        everyday_vault::create(dir.path(), VaultConfig { password: None, ..Default::default() })
            .unwrap();

    let mut account = Account::new(Provider::Custom, "me@example.com");
    account.auth = AuthMethod::Password { username: "me@example.com".into() };
    let account_id = account.id;

    // `Service::set` opens mail's storage itself, since this vault is
    // unlocked from the moment it is created -- see `Service::set`'s own
    // docs. That happens before the account below is saved, so it
    // registers no task for it; this test drives `run_account_with`
    // directly instead, which is what lets it assert on the *outcome* of
    // one attempt rather than the supervisor's restart loop around it.
    let svc = Arc::new(crate::service::Service::new());
    let vault_arc = svc.set(vault);
    vault_arc.save_account(&account).unwrap();
    // No secret at all: `credential::resolve` must refuse before a
    // connection is ever attempted -- `never_connects` panics if it is.

    let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let outcome = crate::mailsync::task::run_account_with(
        svc.clone(),
        vault_arc.clone(),
        account_id,
        stop_rx,
        never_connects,
        |_account, _svc, _vault| FakeSender::default(),
    )
    .await
    .expect("an auth failure must not be an error the supervisor retries");

    assert_eq!(outcome, crate::supervisor::Outcome::Done);
    let account = vault_arc.account(account_id).unwrap();
    assert!(matches!(account.status, AccountStatus::NeedsSignIn { .. }), "{:?}", account.status);
}

async fn never_connects(
    _account: Account,
    _credential: Credential,
) -> SessionResult<FakeMailSession> {
    panic!("must not attempt a connection once the credential itself is refused")
}
