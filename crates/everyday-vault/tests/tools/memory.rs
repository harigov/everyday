//! Memory, through the tools a model actually has.

use super::support::{call, call_err, vault};

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
