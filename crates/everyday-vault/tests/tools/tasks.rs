//! Projects and tasks, through the tools a model actually has.

use super::support::{call, call_err, vault};

#[test]
fn a_task_can_be_captured_found_and_finished_without_ever_being_deleted() {
    // The whole point of the tool surface, in one test: the assistant
    // completes work by moving its status, not by removing the record.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let project = call(&vault, "create_project", serde_json::json!({ "name": "The deck" }));
    let project_id = project["id"].as_str().unwrap().to_string();

    let created = call(
        &vault,
        "create_task",
        serde_json::json!({
            "title": "Order the timber",
            "project_id": project_id,
            "due_date": "2026-09-14",
            "priority": "high",
            "tags": ["shopping"],
        }),
    );
    assert_eq!(created["action"], "created");
    let task_id = created["id"].as_str().unwrap().to_string();

    // Found by the filters a model would actually reach for.
    let found = call(
        &vault,
        "list_tasks",
        serde_json::json!({ "project_id": project_id, "open_only": true }),
    );
    assert_eq!(found["count"], 1);
    assert_eq!(found["tasks"][0]["title"], "Order the timber");
    assert_eq!(found["tasks"][0]["due_date"], "2026-09-14");
    assert_eq!(found["tasks"][0]["priority"], "high");

    // Finished, not deleted.
    call(&vault, "update_task", serde_json::json!({ "task_id": task_id, "status": "done" }));
    let after = call(&vault, "get_task", serde_json::json!({ "task_id": task_id }));
    assert_eq!(after["status"], "done");
    assert_eq!(call(&vault, "list_tasks", serde_json::json!({ "open_only": true }))["count"], 0);

    // ...and the project count in the overview follows.
    let overview = call(&vault, "overview", serde_json::json!({}));
    assert_eq!(overview["tasks"]["open"], 0);
    assert_eq!(overview["tasks"]["done"], 1);
    assert_eq!(overview["today"], "2026-09-08");
}

#[test]
fn an_update_leaves_every_field_it_was_not_given() {
    // The failure this guards against is the quiet one: a model that
    // sends only the field it means to change must not blank the rest.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let id = call(
        &vault,
        "create_task",
        serde_json::json!({
            "title": "Ring the vet",
            "notes": "About the booster",
            "due_date": "2026-09-10",
            "tags": ["pets", "calls"],
            "estimate_minutes": 15,
        }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();

    call(&vault, "update_task", serde_json::json!({ "task_id": id, "priority": "urgent" }));

    let t = call(&vault, "get_task", serde_json::json!({ "task_id": id }));
    assert_eq!(t["priority"], "urgent");
    assert_eq!(t["title"], "Ring the vet", "the title must survive an unrelated edit");
    assert_eq!(t["notes"], "About the booster");
    assert_eq!(t["due_date"], "2026-09-10");
    assert_eq!(t["tags"], serde_json::json!(["pets", "calls"]));
    assert_eq!(t["estimate_minutes"], 15);
}

#[test]
fn clearing_a_field_needs_its_own_flag_because_a_string_cannot_say_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let project_id = call(&vault, "create_project", serde_json::json!({ "name": "Deck" }))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let id = call(
        &vault,
        "create_task",
        serde_json::json!({
            "title": "Order timber",
            "project_id": project_id,
            "due_date": "2026-09-14",
        }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();

    call(
        &vault,
        "update_task",
        serde_json::json!({ "task_id": id, "clear_due_date": true, "clear_project": true }),
    );
    let t = call(&vault, "get_task", serde_json::json!({ "task_id": id }));
    assert!(t.get("due_date").is_none(), "the deadline should be gone: {t}");
    assert!(t.get("project_id").is_none(), "it should be back in the inbox: {t}");
}

#[test]
fn finishing_and_reopening_keep_the_completion_date_honest() {
    // The tools go through `set_status` rather than assigning the field,
    // which is what owns `completed_at`. Assigned directly, a finished
    // task has no completion date -- so "what did I get done this week"
    // never sees it -- and a reopened one keeps the date it was closed.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let id = call(&vault, "create_task", serde_json::json!({ "title": "Order timber" }))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let task_id: everyday_core::TaskId = id.parse().unwrap();
    assert!(vault.task(task_id).unwrap().completed_at.is_none());

    call(&vault, "update_task", serde_json::json!({ "task_id": id, "status": "done" }));
    assert!(vault.task(task_id).unwrap().completed_at.is_some(), "finishing must record when");

    call(&vault, "update_task", serde_json::json!({ "task_id": id, "status": "todo" }));
    assert!(
        vault.task(task_id).unwrap().completed_at.is_none(),
        "a reopened task must not still claim it was finished"
    );

    // Projects are the same shape and the same helper.
    let pid = call(&vault, "create_project", serde_json::json!({ "name": "Deck" }))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let project_id: everyday_core::ProjectId = pid.parse().unwrap();
    call(&vault, "update_project", serde_json::json!({ "project_id": pid, "status": "done" }));
    assert!(vault.project(project_id).unwrap().completed_at.is_some());
    call(&vault, "update_project", serde_json::json!({ "project_id": pid, "status": "active" }));
    assert!(vault.project(project_id).unwrap().completed_at.is_none());
}

#[test]
fn a_task_cannot_be_filed_into_a_project_that_does_not_exist() {
    // There is no foreign key on `project_id`, so an id the model half
    // remembered would otherwise be stored without complaint, producing
    // a task on no board and in no inbox while the tool said "created".
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let ghost = everyday_core::ProjectId::new().to_string();
    let err = call_err(
        &vault,
        "create_task",
        serde_json::json!({ "title": "Order timber", "project_id": ghost }),
    );
    assert!(err.contains("no project with id"), "got {err}");
    assert!(err.contains("list_projects"), "should say how to recover: {err}");

    assert_eq!(
        call(&vault, "list_tasks", serde_json::json!({}))["count"],
        0,
        "nothing should have been created"
    );

    let err = call_err(
        &vault,
        "create_task",
        serde_json::json!({
            "title": "A subtask",
            "parent_id": everyday_core::TaskId::new().to_string(),
        }),
    );
    assert!(err.contains("no task with id"), "got {err}");
}

// ---- reminders, and the assistant's own tasks ----------------------------

#[test]
fn a_reminder_is_set_in_their_time_and_says_when_it_went_out() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let day = super::support::ctx(&vault).now().date().tomorrow().unwrap();
    let created = call(
        &vault,
        "create_task",
        serde_json::json!({
            "title": "Call the dentist",
            "remind_date": day.to_string(),
            "remind_time": "17:00",
        }),
    );
    assert_eq!(created["reminds_them_at"], format!("{day} 17:00"));
    let id: everyday_core::TaskId = created["id"].as_str().unwrap().parse().unwrap();
    let got = call(&vault, "get_task", serde_json::json!({ "task_id": id.to_string() }));
    assert_eq!(got["reminder_sent"], false);

    // Closed before its time: none waiting, and none sent either.
    call(&vault, "update_task", serde_json::json!({ "task_id": id.to_string(), "status": "done" }));
    let got = call(&vault, "get_task", serde_json::json!({ "task_id": id.to_string() }));
    assert_eq!(got["reminder_sent"], false, "a reminder that never went out is not sent");
}

#[test]
fn handing_a_task_over_starts_it_afresh_even_after_its_reminder_went_out() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut task = everyday_core::Task::new("Find a shuttle");
    task.remind_at = Some(jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1));
    task.reminded_at = Some(jiff::Timestamp::now());
    vault.save_task(&task).unwrap();
    assert!(task.next_reminder().is_none());

    call(
        &vault,
        "update_task",
        serde_json::json!({ "task_id": task.id.to_string(), "assignee": "assistant" }),
    );
    let handed = vault.task(task.id).unwrap();
    assert!(handed.next_reminder().is_some(), "the assistant's start is due");
}

#[test]
fn only_a_conversation_may_give_the_assistant_work_for_later() {
    use everyday_core::agent::tools::{self, ToolContext};
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let theirs = serde_json::json!({ "title": "Check the flight", "assignee": "assistant" });

    // A job's follow-up: begun by the application, with strangers' pages
    // in hand.
    let follow_up = ToolContext { follow_up: true, ..super::support::ctx(&vault) };
    let refused = tools::dispatch(&follow_up, "create_task", &theirs).unwrap_err();
    assert!(refused.to_string().contains("work for later"), "{refused}");

    // A dream's proposal: accepting it would start the run.
    let drafting = tools::Drafting {
        source: Some(everyday_core::proposal::ProposalSource::Run {
            run_id: everyday_core::RoutineRunId::new(),
        }),
        direct: Vec::new(),
        max_proposals: None,
    };
    let dream = ToolContext { unattended: true, ..super::support::ctx_drafting(&vault, drafting) };
    let refused = tools::dispatch(&dream, "create_task", &theirs).unwrap_err();
    assert!(refused.to_string().contains("work for later"), "{refused}");

    // A reminder of theirs is a message to them, and stands.
    let tomorrow = super::support::ctx(&vault).now().date().tomorrow().unwrap();
    let reminder = serde_json::json!({
        "title": "Call the dentist", "remind_date": tomorrow.to_string(), "remind_time": "09:00",
    });
    tools::dispatch(&follow_up, "create_task", &reminder).unwrap();
}
