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
    ToolContext { vault, today: TODAY, tz: "UTC", conversation: None, unattended: false }
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
