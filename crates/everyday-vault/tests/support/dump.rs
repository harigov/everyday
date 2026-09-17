//! Reads every record a [`Vault`] holds, through the public API only, and
//! renders it as one canonical, sorted JSON document.
//!
//! `frozen_vault.rs` is the only caller: it diffs this against a checked-in
//! golden file, so a change to encryption labels, column names, the purpose
//! side table, a record's serde shape or a migration shows up as a reviewed
//! diff on this file rather than as a vault nobody can open again. See
//! `docs/plans/architecture-refactor.md`, Phase 0.3.
//!
//! # What is left out
//!
//! Three fields are stamped with the real wall clock by the write path
//! itself, not by the caller -- see [`crate::support::fixture`]'s doc
//! comment on why [`Vault::save_tracker`], [`Vault::save_reading`] and
//! [`Vault::save_profile`] cannot be made to keep a fixed `updated_at` the
//! way every other `save_*` can. Comparing those three verbatim would make
//! this test fail on every run at a different wall-clock second, which is
//! not a regression; they are redacted to a fixed placeholder instead, so
//! the rest of each record still proves everything a golden diff is for.

#![allow(dead_code)]

use everyday_core::{
    BlockQuery, ConversationQuery, EventQuery, GoalQuery, ItemQuery, LogQuery, ProposalQuery,
    ReadingQuery, RecordingQuery, RunQuery, TaskQuery, ThreadFilter, Vault,
};
use jiff::Timestamp;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Placeholder standing in for a field the write path stamps with
/// `Timestamp::now()` regardless of what the caller passed in.
const REDACTED: &str = "<stamped at write time, excluded from the fixture>";

/// A generous upper bound on how many rows this fixture could ever hold in
/// one list -- the vault built by `support::fixture::build` has a handful
/// of everything, twice over at most (the frozen generation plus one
/// appended generation), so this is never close to paging.
const PLENTY: u32 = 1000;

/// The far end of the range [`jiff::Timestamp`] can represent -- used as
/// "no upper bound" when listing mail ops, which [`Vault::due_ops`] only
/// otherwise answers up to a given instant.
fn end_of_time() -> Timestamp {
    Timestamp::from_second(253_402_207_200 - 1).unwrap() // the latest jiff::Timestamp allows
}

fn value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap()
}

fn redact_updated_at(mut v: Value) -> Value {
    if let Some(obj) = v.as_object_mut() {
        obj.insert("updatedAt".to_string(), json!(REDACTED));
    }
    v
}

/// A canonical, pretty-printed, sorted dump of every record `vault` holds,
/// read entirely through the public [`Vault`] API.
pub fn dump(vault: &Vault) -> String {
    let mut rows: Vec<(&'static str, String, Value)> = Vec::new();

    for j in vault.journals().unwrap() {
        rows.push(("journal", j.id.to_string(), value(&j)));
    }

    for e in vault.all_entries().unwrap() {
        for att in &e.attachments {
            let bytes = vault.blob(att.blob).unwrap();
            rows.push((
                "blob",
                att.blob.to_hex(),
                json!({ "bytesUtf8": String::from_utf8_lossy(&bytes) }),
            ));
        }
        rows.push(("entry", e.id.to_string(), value(&e)));
    }

    for n in vault.all_notes().unwrap() {
        rows.push(("note", n.id.to_string(), value(&n)));
        if let Ok(Some(t)) = vault.transcript_for_note(n.id) {
            rows.push(("transcript", t.id.to_string(), value(&t)));
        }
    }

    for p in vault.projects().unwrap() {
        rows.push(("project", p.id.to_string(), value(&p)));
    }
    for t in vault.tasks(&TaskQuery::default()).unwrap() {
        rows.push(("task", t.id.to_string(), value(&t)));
    }
    for b in vault.blocks(&BlockQuery::default()).unwrap() {
        rows.push(("block", b.id.to_string(), value(&b)));
    }

    for c in vault.calendars().unwrap() {
        rows.push(("calendar", c.id.to_string(), value(&c)));
    }
    for ev in vault.events(&EventQuery::default()).unwrap() {
        rows.push(("event", ev.id.to_string(), value(&ev)));
    }

    for k in vault.kinds().unwrap() {
        rows.push(("library_kind", k.id.to_string(), value(&k)));
    }
    for it in vault.items(&ItemQuery::default()).unwrap() {
        rows.push(("item", it.id.to_string(), value(&it)));
    }
    for l in vault.logs(&LogQuery::default()).unwrap() {
        rows.push(("log", l.id.to_string(), value(&l)));
    }

    for tr in vault.trackers().unwrap() {
        rows.push(("tracker", tr.id.to_string(), redact_updated_at(value(&tr))));
    }
    for r in vault.readings(&ReadingQuery::default()).unwrap() {
        rows.push(("reading", r.id.to_string(), redact_updated_at(value(&r))));
    }

    for role in vault.roles().unwrap() {
        rows.push(("role", role.id.to_string(), value(&role)));
    }
    for g in vault.goals(&GoalQuery::default()).unwrap() {
        rows.push(("goal", g.id.to_string(), value(&g)));
    }

    for ro in vault.routines().unwrap() {
        rows.push(("routine", ro.id.to_string(), value(&ro)));
    }
    for run in vault.runs(&RunQuery::default()).unwrap() {
        rows.push(("routine_run", run.id.to_string(), value(&run)));
    }

    for pr in vault.proposals(&ProposalQuery::default()).unwrap() {
        rows.push(("proposal", pr.id.to_string(), value(&pr)));
    }

    for conv in vault.conversations(&ConversationQuery::default()).unwrap() {
        rows.push(("conversation", conv.id.to_string(), value(&conv)));
        for m in vault.messages(conv.id).unwrap() {
            rows.push(("message", m.id.to_string(), value(&m)));
        }
    }
    for mem in vault.memories().unwrap() {
        rows.push(("memory", mem.id.to_string(), value(&mem)));
    }

    // A singleton: no id of its own, and the key beside it is write-only
    // everywhere except here -- see `Vault::agent_credentials`'s own docs.
    if let Ok(settings) = vault.agent_settings() {
        let key = vault.agent_credentials().ok().and_then(|(_, k)| k);
        rows.push((
            "agent_settings",
            "singleton".to_string(),
            json!({ "settings": value(&settings), "key": key }),
        ));
    }

    for acc in vault.accounts().unwrap() {
        rows.push(("account", acc.id.to_string(), value(&acc)));
        if let Ok(Some(secret)) = vault.account_secret(acc.id) {
            rows.push(("account_secret", acc.id.to_string(), value(&secret)));
        }

        let mut thread_ids = BTreeSet::new();
        for mb in vault.mailboxes(acc.id).unwrap() {
            rows.push(("mailbox", mb.id.to_string(), value(&mb)));
            let page = vault.list_threads(mb.id, &ThreadFilter::default(), None, PLENTY).unwrap();
            thread_ids.extend(page.threads.into_iter().map(|t| t.id));
        }
        for id in thread_ids {
            let (thread, messages) = vault.thread(id).unwrap();
            rows.push(("mail_thread", thread.id.to_string(), value(&thread)));
            for m in messages {
                rows.push(("mail_message", m.id.to_string(), value(&m)));
                if let Ok(body) = vault.body(m.id) {
                    rows.push(("mail_body", m.id.to_string(), value(&body)));
                }
            }
        }

        for d in vault.drafts(acc.id).unwrap() {
            rows.push(("mail_draft", d.id.to_string(), value(&d)));
        }

        // `due_ops` and `in_flight_ops` between them cover every state an
        // op built by this fixture can be in -- see `support::fixture`,
        // which only ever mints `Pending` ones.
        for op in vault.due_ops(acc.id, end_of_time(), PLENTY).unwrap() {
            rows.push(("mail_op", op.id.to_string(), value(&op)));
        }
        for op in vault.in_flight_ops(acc.id).unwrap() {
            rows.push(("mail_op", op.id.to_string(), value(&op)));
        }
    }

    for rec in vault.recordings(&RecordingQuery::default()).unwrap() {
        rows.push(("recording", rec.id.to_string(), value(&rec)));
    }
    for vp in vault.voiceprints().unwrap() {
        rows.push(("voiceprint", vp.id.to_string(), value(&vp)));
    }

    if let Ok(profile) = vault.profile() {
        rows.push(("profile", "singleton".to_string(), redact_updated_at(value(&profile))));
    }

    rows.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));

    let doc: Vec<Value> = rows
        .into_iter()
        .map(|(kind, id, record)| json!({ "kind": kind, "id": id, "record": record }))
        .collect();
    format!("{}\n", serde_json::to_string_pretty(&doc).unwrap())
}
