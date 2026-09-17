//! The memory commands, end to end, through [`Service::call`].
//!
//! `everyday_core::vault::agent`'s own tests cover the caps and the
//! eviction order; what belongs here is the one thing the vault does not
//! know about -- the correction `save_memory` applies before it ever reaches
//! the vault, because it depends on *who is asking*, not on storage limits.
//! See `docs/plans/dreaming.md`'s phase 2.

use everyday_core::{Memory, MemoryOrigin};
use everyday_service::{Ctx, Service};
use serde_json::json;
use std::sync::Arc;

#[allow(dead_code)]
mod support;

fn env() -> (Arc<Service>, tempfile::TempDir) {
    support::vault::service(None)
}

async fn save(svc: &Arc<Service>, memory: &Memory) -> Memory {
    let out = svc
        .call(Ctx::local(), "save_memory", json!({ "memory": memory }))
        .await
        .unwrap_or_else(|e| panic!("save_memory: {e}"));
    let _evicted: Vec<Memory> = serde_json::from_value(out).unwrap();
    // The command answers with everything evicted, not the row just
    // written -- fetch it back from the list to see what was actually
    // stored.
    let vault = svc.get().unwrap();
    vault.memories().unwrap().into_iter().find(|m| m.id == memory.id).unwrap()
}

#[tokio::test]
async fn a_new_memory_arriving_as_inferred_is_coerced_to_told() {
    let (svc, _dir) = env();

    // Only a dream infers; the interface has no other way to write one, but
    // a stray or hand-edited payload must not be believed either.
    let memory = Memory { origin: MemoryOrigin::Inferred, ..Memory::new("Reads before bed") };
    let saved = save(&svc, &memory).await;
    assert_eq!(saved.origin, MemoryOrigin::Told, "a new memory cannot arrive already inferred");
}

#[tokio::test]
async fn rewriting_an_inferred_memory_confirms_it() {
    let (svc, _dir) = env();
    let vault = svc.get().unwrap();

    let inferred =
        Memory::inferred("Seems to work late on Thursdays", jiff::civil::date(2026, 9, 1));
    vault.save_memory(&inferred).unwrap();

    let mut edited = inferred.clone();
    edited.text = "Works late most Thursdays".into();
    let saved = save(&svc, &edited).await;
    assert_eq!(saved.origin, MemoryOrigin::Confirmed, "rewriting it is standing behind it");
    assert_eq!(saved.text, "Works late most Thursdays");
}

#[tokio::test]
async fn saving_an_inferred_memory_unchanged_leaves_its_origin_alone() {
    let (svc, _dir) = env();
    let vault = svc.get().unwrap();

    let inferred =
        Memory::inferred("Seems to work late on Thursdays", jiff::civil::date(2026, 9, 1));
    vault.save_memory(&inferred).unwrap();

    // Pinning it, say, without touching the text is not "standing behind"
    // the guess in the sense that promotes it.
    let mut same_text = inferred.clone();
    same_text.pinned = true;
    let saved = save(&svc, &same_text).await;
    assert_eq!(saved.origin, MemoryOrigin::Inferred, "no rewrite, no promotion");
}

#[tokio::test]
async fn set_memory_origin_goes_through_the_command_layer_and_keeps_its_refusal() {
    let (svc, _dir) = env();
    let vault = svc.get().unwrap();
    let inferred = Memory::inferred("Plans on Sundays", jiff::civil::date(2026, 9, 1));
    vault.save_memory(&inferred).unwrap();

    let confirmed: Memory = serde_json::from_value(
        svc.call(
            Ctx::local(),
            "set_memory_origin",
            json!({ "id": inferred.id, "origin": "confirmed" }),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(confirmed.origin, MemoryOrigin::Confirmed);

    let err = svc
        .call(Ctx::local(), "set_memory_origin", json!({ "id": inferred.id, "origin": "inferred" }))
        .await
        .expect_err("only a dream infers");
    assert!(err.message.contains("infers"), "{}", err.message);
}
