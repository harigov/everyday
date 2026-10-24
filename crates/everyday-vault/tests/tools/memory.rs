//! Memory, through the tools a model actually has.

use super::support::{call, call_err, ctx, vault};

#[test]
fn remembering_and_forgetting_go_through_the_settings_that_govern_them() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    // Off by default in a fresh vault? No -- remembering is on, but the
    // switch has to actually be honoured.
    vault
        .save_agent_settings(&everyday_core::AgentSettings {
            remember: false,
            ..Default::default()
        })
        .unwrap();
    let err = call_err(&vault, "remember", serde_json::json!({ "fact": "Plans on Sundays" }));
    assert!(err.contains("switched off"), "got {err}");

    vault.save_agent_settings(&everyday_core::AgentSettings::default()).unwrap();
    let saved = call(&vault, "remember", serde_json::json!({ "fact": "Plans on Sundays" }));
    let id = saved["id"].as_str().unwrap().to_string();
    assert_eq!(saved["forgotten_to_make_room"], serde_json::json!([]));

    let listed = call(&vault, "list_memories", serde_json::json!({}));
    assert_eq!(listed[0]["fact"], "Plans on Sundays");

    call(&vault, "forget", serde_json::json!({ "memory_id": id }));
    assert!(vault.memories().unwrap().is_empty());
}

/// A memory the person told is a standing instruction in every prompt after
/// it; a scheduled run has no person telling it anything, only its
/// instructions and whatever it read, so what it remembers is inferred.
#[test]
fn what_a_scheduled_run_remembers_is_kept_as_inferred() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    for (unattended, origin) in [
        (false, everyday_core::agent::MemoryOrigin::Told),
        (true, everyday_core::agent::MemoryOrigin::Inferred),
    ] {
        let ctx = ctx(&vault).with_unattended(unattended);
        let args = serde_json::json!({ "fact": format!("Prefers tea ({unattended})") });
        let out = everyday_core::agent::tools::dispatch(&ctx, "remember", &args).unwrap();
        let id = out["id"].as_str().unwrap();
        let saved = vault.memories().unwrap().into_iter().find(|m| m.id.to_string() == id).unwrap();
        assert_eq!(saved.origin, origin, "unattended: {unattended}");
    }
}
