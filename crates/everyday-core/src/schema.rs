//! JSON Schema for the record types, generated with `schemars` — feature
//! `schema`, off by default (see `everyday-core/Cargo.toml`). Not part of the
//! shipped binary; this exists only so `crates/everyday-core/tests/schema.rs`
//! can write `crates/everyday-core/schema/record-schema.json`, which
//! `ui/scripts/check-types-drift.mjs` reads to catch drift between these
//! structs and the hand-written mirror in `ui/src/lib/types.ts`.
//!
//! [`bundle`] covers the record kinds `crates/everyday-vault/tests/fixtures`
//! (Phase 0.3) enumerates — the things that have their own id and are
//! written to a table or a sealed payload of their own — not every value
//! type embedded inside one. That is a deliberate, narrower scope than "every
//! `Serialize` struct in this crate": a `Location` or a `Cadence` has no
//! identity or `RecordKind` of its own, so it is not a "record" in the sense
//! this phase is checking, even though it happens to derive
//! [`schemars::JsonSchema`] too (it has to, for its owning record's schema to
//! compile).
//!
//! Each entry is keyed by the name `types.ts` uses, which is not always this
//! crate's name for the same struct — `agent::Message` is `AgentMessage` on
//! the wire because `mail::Message` already claimed `MailMessage`, and
//! `meeting::Voiceprint` is mirrored as `VoiceprintInfo`. The checker looks
//! up the schema by the TypeScript name, so a rename on either side that
//! forgets the other shows up as "no such type in types.ts" or "no such type
//! in the schema" rather than silently comparing the wrong pair.
//!
//! Only the record's own top-level fields are compared — not the shape of
//! whatever a field's type points at. `types.ts` interfaces are flat and
//! per-type already, and a nested type that is itself a record (`Tracker`
//! inside `Journal::trackers`) is checked on its own terms as its own entry.

use schemars::{JsonSchema, schema_for};
use serde_json::Value;

/// One schema, plus which TS name it should be compared against.
fn schema_of<T: JsonSchema>(ts_name: &'static str) -> (String, Value) {
    (ts_name.to_string(), serde_json::to_value(schema_for!(T)).expect("schema serialises"))
}

/// Every record kind Phase 0.3 lists, mapped to the schema of the Rust
/// struct that is sealed or columned as that kind — keyed by the name
/// `ui/src/lib/types.ts` gives it.
///
/// `AccountSecret` has no entry: a secret is never sent to the interface, so
/// there is nothing in `types.ts` for it to drift from. Its absence here
/// would otherwise show up as "no such type in types.ts", which is not a bug
/// — see the allowlist entry instead if that ever changes.
pub fn bundle() -> Value {
    let types: Vec<(String, Value)> = vec![
        schema_of::<crate::model::Journal>("Journal"),
        schema_of::<crate::model::Entry>("Entry"),
        schema_of::<crate::note::Note>("Note"),
        schema_of::<crate::task::Project>("Project"),
        schema_of::<crate::task::Task>("Task"),
        schema_of::<crate::task::TimeBlock>("TimeBlock"),
        schema_of::<crate::calendar::Calendar>("Calendar"),
        schema_of::<crate::calendar::Event>("CalendarEvent"),
        schema_of::<crate::library::Kind>("Kind"),
        schema_of::<crate::library::Item>("Item"),
        schema_of::<crate::library::LogEntry>("LogEntry"),
        schema_of::<crate::tracker::Tracker>("Tracker"),
        schema_of::<crate::tracker::Reading>("Reading"),
        schema_of::<crate::purpose::Role>("Role"),
        schema_of::<crate::purpose::Goal>("Goal"),
        schema_of::<crate::routine::Routine>("Routine"),
        schema_of::<crate::routine::RoutineRun>("RoutineRun"),
        schema_of::<crate::proposal::Proposal>("Proposal"),
        schema_of::<crate::agent::Conversation>("Conversation"),
        schema_of::<crate::agent::Message>("AgentMessage"),
        schema_of::<crate::agent::Memory>("Memory"),
        schema_of::<crate::account::Account>("Account"),
        schema_of::<crate::mail::Mailbox>("Mailbox"),
        schema_of::<crate::mail::Message>("MailMessage"),
        schema_of::<crate::mail::Thread>("Thread"),
        schema_of::<crate::mail::Draft>("Draft"),
        schema_of::<crate::mail::Op>("Op"),
        schema_of::<crate::meeting::Recording>("Recording"),
        schema_of::<crate::meeting::Transcript>("Transcript"),
        schema_of::<crate::meeting::Voiceprint>("VoiceprintInfo"),
        schema_of::<crate::profile::Profile>("Profile"),
    ];
    Value::Object(types.into_iter().collect())
}
