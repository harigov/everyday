//! The record types' shape, written down.
//!
//! `schema/record-schema.json` is a JSON Schema for every record kind this
//! crate defines, generated from the structs themselves by
//! `everyday_core::schema::bundle` (feature `schema`, off by default — see
//! `everyday-core/Cargo.toml`). It is committed, this test compares the
//! generator's output against it, and
//! `ui/scripts/check-types-drift.mjs` reads the committed file to compare
//! against `ui/src/lib/types.ts` — so the UI check needs no Rust build.
//!
//! To accept a change on the Rust side:
//!
//!   UPDATE_SCHEMA=1 cargo test -p everyday-core --features schema --test schema
//!
//! then re-run the drift check and read its diff against the allowlist:
//!
//!   node ui/scripts/check-types-drift.mjs
//!
//! Compiled only under `--features schema`: without it there is no
//! `everyday_core::schema` to call, and a default `cargo test` must not
//! break for lacking an optional feature it never asked for.
#![cfg(feature = "schema")]

use std::path::PathBuf;

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schema").join("record-schema.json")
}

fn current() -> String {
    format!("{}\n", serde_json::to_string_pretty(&everyday_core::schema::bundle()).unwrap())
}

#[test]
fn the_record_schema_is_what_was_written_down() {
    let path = snapshot_path();
    let current = current();

    if std::env::var("UPDATE_SCHEMA").is_ok() {
        std::fs::write(&path, &current)
            .unwrap_or_else(|e| panic!("could not write {}: {e}", path.display()));
        return;
    }

    // A missing file reads as an empty one, so a checkout that has never
    // generated one reports the whole schema as the change, rather than
    // failing on an unhelpful "no such file".
    let recorded = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        recorded, current,
        "record-schema.json is stale.\n\n\
         This is what ui/scripts/check-types-drift.mjs compares types.ts against, so a \
         change here is worth reading: it is either a real change to a record's shape, or \
         schemars' output changing for an unrelated reason.\n\n\
         Then: UPDATE_SCHEMA=1 cargo test -p everyday-core --features schema --test schema"
    );
}
