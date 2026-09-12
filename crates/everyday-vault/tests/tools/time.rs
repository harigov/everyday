//! Time blocks, through the tools a model actually has.

use super::support::{call, call_err, vault};

#[test]
fn a_time_block_is_placed_at_the_wall_clock_time_it_was_given() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let task_id = call(&vault, "create_task", serde_json::json!({ "title": "Deep work" }))["id"]
        .as_str()
        .unwrap()
        .to_string();

    call(
        &vault,
        "create_time_block",
        serde_json::json!({
            "date": "2026-09-09",
            "start_time": "09:00",
            "end_time": "10:30",
            "task_id": task_id,
        }),
    );

    let blocks = call(
        &vault,
        "list_time_blocks",
        serde_json::json!({ "from": "2026-09-09", "to": "2026-09-09" }),
    );
    assert_eq!(blocks["count"], 1);
    assert_eq!(blocks["blocks"][0]["minutes"], 90);
    assert_eq!(blocks["blocks"][0]["kind"], "planned");
    assert_eq!(blocks["blocks"][0]["for"]["task_id"], task_id);
}

#[test]
fn a_block_must_say_what_the_time_is_for_and_may_only_say_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let base =
        serde_json::json!({ "date": "2026-09-09", "start_time": "09:00", "end_time": "10:00" });

    let err = call_err(&vault, "create_time_block", base.clone());
    assert!(err.contains("give one of"), "got {err}");

    let task_id = call(&vault, "create_task", serde_json::json!({ "title": "x" }))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut both = base.clone();
    both["task_id"] = serde_json::json!(task_id);
    both["label"] = serde_json::json!("Lunch");
    let err = call_err(&vault, "create_time_block", both);
    assert!(err.contains("only one of"), "got {err}");

    // Backwards is refused rather than silently stored as zero minutes.
    let mut backwards = base;
    backwards["label"] = serde_json::json!("Lunch");
    backwards["end_time"] = serde_json::json!("08:00");
    let err = call_err(&vault, "create_time_block", backwards);
    assert!(err.contains("after"), "got {err}");
}
