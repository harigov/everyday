//! Comparing a committed snapshot against what the code says today.
//!
//! Two snapshots are kept in this crate -- `surface.json`, the command table
//! every client is generated from, and `mcp.json`, the tool catalogue an MCP
//! client and a model read -- and they want the same three things: write the
//! file when the environment says to, compare it when it does not, and fail
//! with a message that says how to accept the change rather than merely that
//! something differs.
//!
//! It lives in a module rather than being written twice because the second
//! copy is where the drift starts: a better diff, a different environment
//! variable, or a decision about what a missing file means would otherwise
//! have to be made twice and would silently stop being the same rule.
//!
//! Rust compiles every file in `tests/` as its own binary, so this is shared
//! by `mod support;` in each of them rather than by being importable. That is
//! also why it carries `#![allow(dead_code)]` in effect: a binary that uses
//! only part of it would otherwise warn about the rest.

use std::path::Path;

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
