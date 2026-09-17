//! Phase 0.2 of `docs/plans/architecture-refactor.md`: a snapshot of every
//! name that crosses the wire but has no snapshot of its own already --
//! `surface.rs` covers commands, `mcp.rs` covers tools, and this covers the
//! rest of what a client (the interface, the SSE stream, a remote MCP
//! client) has to agree with the service about: what a [`Change`] can say it
//! touched, what it can say happened to it, the shape of the event itself,
//! what a backend can say it supports, and every code a [`CommandError`] can
//! carry.
//!
//! None of this is meant to be immutable forever -- adding a domain adds a
//! [`events::Kind`] variant, and that is expected. It exists so that doing
//! so is a reviewable diff in this file rather than a silent change nobody
//! looked at, which is exactly what `surface.rs`'s doc comment says about
//! `surface.json`.
//!
//! To accept a change: `UPDATE_SURFACE=1 cargo test -p everyday-service
//! --test wire_names`.

use std::path::PathBuf;

use everyday_core::store::Capabilities;
use everyday_service::error::codes;
use everyday_service::events::{Change, Kind, Op};

#[allow(dead_code)]
mod support;

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("wire_names.json")
}

/// Every [`Kind`] variant, spelled out by hand rather than discovered.
///
/// `Kind` derives no `EnumIter` and offers no other way to list its own
/// variants, so this is a hand-written inventory -- which is exactly the
/// kind of list that silently falls out of step with the type it describes.
/// `assert_every_kind_is_named` below is what stops that: it is a second,
/// independent enumeration with no wildcard arm, so a variant added to
/// `Kind` and not to *both* places fails to compile this test rather than
/// quietly shipping an incomplete snapshot.
fn all_kinds() -> Vec<Kind> {
    vec![
        Kind::Journal,
        Kind::Entry,
        Kind::Note,
        Kind::Project,
        Kind::Task,
        Kind::Block,
        Kind::Calendar,
        Kind::Event,
        Kind::Shelf,
        Kind::Item,
        Kind::Log,
        Kind::Tracker,
        Kind::Reading,
        Kind::Role,
        Kind::Goal,
        Kind::Account,
        Kind::Mailbox,
        Kind::Thread,
        Kind::Draft,
        Kind::Conversation,
        Kind::Routine,
        Kind::RoutineRun,
        Kind::Proposal,
        Kind::Memory,
        Kind::Recording,
        Kind::Transcript,
        Kind::Voiceprint,
        Kind::Settings,
        Kind::BackgroundTask,
    ]
}

/// The compile-time half of the guard `all_kinds`'s doc comment describes.
///
/// Every arm names exactly one variant and there is no `_ =>` fallback, so
/// the match itself stops compiling the moment `events::Kind` grows a
/// variant this function does not yet mention -- which is the earliest
/// possible point to notice, well before the snapshot comparison below would
/// have caught the same thing at `cargo test` time.
fn assert_every_kind_is_named(k: Kind) {
    match k {
        Kind::Journal
        | Kind::Entry
        | Kind::Note
        | Kind::Project
        | Kind::Task
        | Kind::Block
        | Kind::Calendar
        | Kind::Event
        | Kind::Shelf
        | Kind::Item
        | Kind::Log
        | Kind::Tracker
        | Kind::Reading
        | Kind::Role
        | Kind::Goal
        | Kind::Account
        | Kind::Mailbox
        | Kind::Thread
        | Kind::Draft
        | Kind::Conversation
        | Kind::Routine
        | Kind::RoutineRun
        | Kind::Proposal
        | Kind::Memory
        | Kind::Recording
        | Kind::Transcript
        | Kind::Voiceprint
        | Kind::Settings
        | Kind::BackgroundTask => {}
    }
}

fn all_ops() -> Vec<Op> {
    vec![Op::Created, Op::Updated, Op::Deleted]
}

/// A `Capabilities` with nothing turned on: no attachments, no durability
/// guarantee, no human-readable files, no size limit recorded (because there
/// is nothing to size-limit), and every domain flag false -- the shape
/// `MemStore` reports today, minus the one domain (`blobs`) it happens to
/// support. `Capabilities` has no `#[derive(Default)]`, so this is spelled
/// by hand rather than called; that is the point of pinning it.
fn capabilities_baseline() -> Capabilities {
    Capabilities {
        blobs: false,
        transactional: false,
        human_readable: false,
        max_blob_bytes: None,
        tasks: false,
        calendars: false,
        library: false,
        trackers: false,
        purpose: false,
        notes: false,
        routines: false,
        proposals: false,
        agent: false,
        secrets: false,
        accounts: false,
        mail: false,
        meetings: false,
    }
}

/// Every flag on, including the ones a real backend has never reported both
/// of at once. `max_blob_bytes: None` here for the same reason `None` means
/// "no limit" everywhere else this field is read: the most permissive value,
/// not the largest finite one.
fn capabilities_all_true() -> Capabilities {
    Capabilities {
        blobs: true,
        transactional: true,
        human_readable: true,
        max_blob_bytes: None,
        tasks: true,
        calendars: true,
        library: true,
        trackers: true,
        purpose: true,
        notes: true,
        routines: true,
        proposals: true,
        agent: true,
        secrets: true,
        accounts: true,
        mail: true,
        meetings: true,
    }
}

fn sample_change_with_id() -> Change {
    Change {
        kind: Kind::Task,
        op: Op::Updated,
        id: Some("task-1".to_string()),
        ids: Vec::new(),
        origin: None,
    }
}

fn sample_change_with_ids() -> Change {
    Change {
        kind: Kind::Task,
        op: Op::Updated,
        id: None,
        ids: vec!["task-1".to_string(), "task-2".to_string()],
        origin: None,
    }
}

fn sample_change_with_origin() -> Change {
    Change {
        kind: Kind::Task,
        op: Op::Updated,
        id: Some("task-1".to_string()),
        ids: Vec::new(),
        origin: Some("window-a".to_string()),
    }
}

/// The whole document, in a stable order: every section is a JSON array or
/// object built from a fixed, hand-written list above, never from iterating
/// something the standard library or a derive macro handed back in an order
/// this file does not control.
fn current() -> String {
    for k in all_kinds() {
        assert_every_kind_is_named(k);
    }

    let doc = serde_json::json!({
        "kinds": all_kinds().iter().map(|k| serde_json::to_value(k).unwrap()).collect::<Vec<_>>(),
        "ops": all_ops().iter().map(|o| serde_json::to_value(o).unwrap()).collect::<Vec<_>>(),
        "changes": {
            "withId": serde_json::to_value(sample_change_with_id()).unwrap(),
            "withIds": serde_json::to_value(sample_change_with_ids()).unwrap(),
            "withOrigin": serde_json::to_value(sample_change_with_origin()).unwrap(),
        },
        "capabilities": {
            "baseline": serde_json::to_value(capabilities_baseline()).unwrap(),
            "allTrue": serde_json::to_value(capabilities_all_true()).unwrap(),
        },
        "errorCodes": codes::ALL,
    });
    format!("{}\n", serde_json::to_string_pretty(&doc).unwrap())
}

#[test]
fn the_wire_names_are_what_was_written_down() {
    support::compare(
        &snapshot_path(),
        &current(),
        "the wire's vocabulary has changed: a `Change`'s `kind` or `op`, the shape \
         of `Capabilities`, or an error `code` a client may have to react to \
         structurally.\n\n\
         Adding a `Kind` variant, an error code, or a `Capabilities` flag is an \
         additive, expected change -- update `all_kinds` (and its exhaustiveness \
         match right beside it) or the relevant list above, then accept the \
         snapshot. Renaming or removing one is a wire break and needs the same \
         care `surface.rs` asks for a removed command.",
        "UPDATE_SURFACE=1 cargo test -p everyday-service --test wire_names",
    );
}
