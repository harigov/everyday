//! Test fixtures shared across this crate's integration tests.
//!
//! Three unrelated things live here for the same reason: each is something
//! more than one test file built by hand before this existed, and a second
//! copy is where the drift starts. [`compare`] writes or checks a committed
//! snapshot -- `surface.json`, the command table every client is generated
//! from, and `mcp.json`, the tool catalogue an MCP client and a model read --
//! [`vault::service`] builds a vault to run commands against, which
//! `tests/call.rs` and `tests/transfer.rs` both need and neither is about,
//! and [`clock::FakeClock`] is a `Service`-facing clock a test moves by
//! hand, for whichever test wants to prove a time-dependent behaviour
//! without a sleep.
//!
//! Rust compiles every file in `tests/` as its own binary, so this is shared
//! by `mod support;` in each of them rather than by being importable. That is
//! also why each of those declarations carries `#[allow(dead_code)]`: no one
//! binary uses every part of this module, and the rest would otherwise warn.

use std::path::Path;

pub mod clock;
pub mod vault;

/// The environment variable that turns a comparison into an update.
///
/// One variable for both snapshots on purpose: `make fix` regenerates
/// everything in one pass, and a second spelling would mean a contributor
/// accepting one change and being surprised by the other.
pub const ACCEPT: &str = "UPDATE_SURFACE";

/// Compare `current` against the file at `path`, or write it if asked to.
///
/// `why` is the part that cannot be shared: what this particular snapshot
/// means, and therefore what a reader should think about before accepting a
/// diff to it. It is printed above the command that accepts the change.
pub fn compare(path: &Path, current: &str, why: &str, accept_with: &str) {
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
