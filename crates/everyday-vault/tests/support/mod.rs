//! The harness every end-to-end test in this directory is built on: a
//! throwaway vault over the real SQLite backend, and the three functions
//! that run a tool call through it the way a model actually would.
//!
//! These tests exist to catch "the assistant marked the wrong task done" one
//! layer up from the offline unit tests in `everyday_core::agent::tools`: a
//! real vault, a real SQLite file, a real cipher, and `tools::dispatch`
//! itself, rather than a fake standing in for any of them.
//!
//! Rust compiles every file in `tests/` as its own binary, so this is shared
//! by `mod support;` in each of them rather than by being importable. That is
//! also why it carries `#![allow(dead_code)]` in effect: a binary that uses
//! only part of it would otherwise warn about the rest.

#![allow(dead_code)]

pub mod dump;
pub mod fixture;

use everyday_core::agent::tools::{self, ToolContext};
use everyday_core::{Tracker, Vault, VaultConfig};
use std::path::Path;

/// A real vault over the SQLite backend, unencrypted for speed: these tests
/// are about the rules the vault applies, not about the envelope.
pub fn vault(dir: &Path) -> Vault {
    everyday_vault::create(dir, VaultConfig { password: None, ..Default::default() }).unwrap()
}

/// A saved tracker, and a journal that shows its chip.
pub fn a_tracked(vault: &Vault, tracker: Tracker) -> Tracker {
    let mut journal = everyday_core::Journal::new("Health");
    journal.show(tracker.id);
    vault.save_journal(&journal).unwrap();
    vault.save_tracker(&tracker).unwrap();
    vault.tracker(tracker.id).unwrap()
}

/// The day every tool-catalogue test in this directory pretends it is.
pub const TODAY: jiff::civil::Date = jiff::civil::Date::constant(2026, 9, 8);

pub fn ctx(vault: &Vault) -> ToolContext<'_> {
    ToolContext {
        vault,
        today: TODAY,
        tz: "UTC",
        conversation: None,
        unattended: false,
        caller: None,
        mail_search: None,
        assistant_provider: None,
        mail_rate_limit: None,
        after_mail_write: None,
        invite_responder: None,
        drafting: None,
    }
}

/// As [`ctx`], but with dreaming's own leash on: every `Write` or
/// `Destructive` tool call `dispatch`s into a [`tools::Drafting`] proposal
/// instead of saving. What `crates/everyday-vault/tests/drafting.rs` runs
/// the catalogue through.
pub fn ctx_drafting(vault: &Vault, drafting: tools::Drafting) -> ToolContext<'_> {
    ToolContext { drafting: Some(drafting), ..ctx(vault) }
}

pub fn call(vault: &Vault, tool: &str, args: serde_json::Value) -> serde_json::Value {
    tools::dispatch(&ctx(vault), tool, &args).unwrap_or_else(|e| panic!("{tool} failed: {e}"))
}

pub fn call_err(vault: &Vault, tool: &str, args: serde_json::Value) -> String {
    tools::dispatch(&ctx(vault), tool, &args)
        .map(|v| panic!("{tool} unexpectedly succeeded: {v}"))
        .unwrap_err()
        .to_string()
}

/// The environment variable that turns a golden-file comparison into an
/// update. Copied from `everyday-service/tests/support/mod.rs`'s own
/// `compare`, which this crate cannot reach across the crate boundary --
/// same name, so `make fix` regenerates both crates' snapshots in one pass.
pub const ACCEPT: &str = "UPDATE_SURFACE";

/// Compare `current` against the file at `path`, or write it if asked to.
///
/// `why` is what a reader should think about before accepting a diff to
/// this particular snapshot.
pub fn compare(path: &std::path::Path, current: &str, why: &str, accept_with: &str) {
    if std::env::var(ACCEPT).is_ok() {
        std::fs::write(path, current)
            .unwrap_or_else(|e| panic!("could not write {}: {e}", path.display()));
        return;
    }

    // A missing file reads as an empty one, so the first run in a checkout
    // that has never had one reports the whole snapshot as the change --
    // which is the truth, and is more useful than an error about a path.
    let recorded = std::fs::read_to_string(path).unwrap_or_default();
    if recorded != current {
        panic!("{why}\n\nThen: {accept_with}\n");
    }
}
