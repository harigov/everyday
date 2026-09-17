//! Phase 0's regression net for `docs/plans/architecture-refactor.md`'s
//! 0.5, "the assistant's safety properties, as explicit tests" -- the
//! checklist Phase 8's shared `update_*` helpers have to keep true.
//!
//! Property (from the plan) → test(s) that pin it. Most of these already
//! existed before this file; this file's own contribution is the fourth and
//! sixth rows, and it is where the inventory lives because that is where
//! the biggest gap was.
//!
//! - **A `Destructive` tool asks when `confirm_destructive` is on, and runs
//!   unasked when it is off** -- `everyday_service::agent`'s
//!   `must_confirm` and its test
//!   `a_destructive_call_is_gated_on_the_setting_and_outward_never_is`;
//!   an unattended run refuses regardless, in
//!   `everyday-service/tests/routines.rs`'s
//!   `a_scheduled_run_may_not_delete_anything` and the `park_unattended_*`
//!   pair.
//! - **An `Outward` tool always asks, whatever the setting, and an
//!   unattended run refuses it** -- the `must_confirm` test above (its
//!   `Outward` half), `everyday-service/tests/mail_agent_tools.rs`'s
//!   `send_draft_through_run_tool_needs_confirm_destructive_even_for_the_
//!   vaults_owner` and `respond_to_invite_through_run_tool_needs_confirm_
//!   destructive_too`, and `everyday-service/tests/routines.rs`'s new
//!   `a_scheduled_run_may_not_send_anything`.
//! - **Tools in a `Sensitivity::Secret` domain are hidden from MCP
//!   (`list_tools`) and refused by `run_tool`** --
//!   `everyday_core::agent::tools`'s own
//!   `a_secret_domain_is_never_offered_to_a_model` and
//!   `every_domain_has_answered_the_question` pin the core half (no
//!   domain is `Secret` today, and none may become one silently); the new
//!   `everyday-service/tests/mcp.rs::
//!   a_secret_domain_tool_is_hidden_from_list_tools_and_refused_by_run_tool`
//!   drives the two real commands, and stays an empty loop until a secret
//!   domain exists -- `everyday-service/tests/call.rs`'s
//!   `a_narrow_token_reaches_the_tools_of_its_own_domain_and_no_others` is
//!   the live proof that both commands read the one filter this hiding
//!   would use, exercised today by scope rather than by sensitivity.
//! - **`update_note` and `update_entry` refuse to replace a body holding
//!   blob refs, and leave the record unchanged** -- `journals.rs`'s
//!   `an_entry_holding_photographs_will_not_have_its_text_replaced`
//!   (already existed) and the new `notes.rs`'s
//!   `a_note_holding_photographs_will_not_have_its_text_replaced`.
//! - **Every `update_*` tool is a partial patch: naming one field leaves
//!   every other field exactly as it was** -- the whole of this file:
//!   `every_update_tool_is_covered_by_a_partial_patch_test` enumerates
//!   `tools::catalog()` so a new `update_*` tool fails loudly until it
//!   gets a test below, and one test per tool follows it
//!   (`update_task_touches_only_the_named_field` and its seven siblings).
//! - **Proposal builders (`build_update_*`) carry `expected_updated_at`
//!   equal to the loaded record's `updated_at`, and accepting a proposal
//!   whose record changed since returns a conflict** --
//!   `everyday-vault/tests/drafting.rs`'s
//!   `update_task_proposes_a_replace_carrying_the_loaded_updated_at` and
//!   the two new siblings for `update_note` and `update_routine` (the
//!   other two `update_*` tools with a `build`); the conflict half is
//!   `everyday-vault/tests/proposals.rs`'s
//!   `a_stale_replace_is_declined_and_the_newer_write_survives` and
//!   `a_fresh_replace_is_saved` (already existed).

use super::support::{call, vault};
use everyday_core::agent::tools;
use everyday_core::{Goal, Note, Purpose, Role};

/// The coverage check the plan asks for: every `update_*` tool in the real
/// catalogue must be named below, so a new one added later fails this test
/// instead of quietly shipping with no partial-patch guarantee checked.
#[test]
fn every_update_tool_is_covered_by_a_partial_patch_test() {
    const COVERED: &[&str] = &[
        "update_task",
        "update_project",
        "update_note",
        "update_entry",
        "update_item",
        "update_goal",
        "update_routine",
        "update_draft",
    ];
    for tool in tools::catalog() {
        if tool.name.starts_with("update_") {
            assert!(
                COVERED.contains(&tool.name),
                "{} is a new update tool with no partial-patch test in \
                 crates/everyday-vault/tests/tools/partial_patch.rs -- add one, then add its \
                 name to COVERED here",
                tool.name
            );
        }
    }
    // And the other direction: nothing named here has quietly left the
    // catalogue, which would leave a passing test that covers nothing.
    for name in COVERED {
        assert!(tools::find(name).is_some(), "{name} is listed here but is no longer a tool");
    }
}

#[test]
fn update_task_touches_only_the_named_field() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let role = Role::new("Work");
    vault.save_role(&role).unwrap();
    let goal = Goal::new(role.id, "Ship the deck");
    vault.save_goal(&goal).unwrap();

    let project_a = call(&vault, "create_project", serde_json::json!({ "name": "The deck" }))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let project_b =
        call(&vault, "create_project", serde_json::json!({ "name": "The fence" }))["id"]
            .as_str()
            .unwrap()
            .to_string();

    let task_id = call(
        &vault,
        "create_task",
        serde_json::json!({
            "title": "Book the dentist",
            "notes": "Ring first",
            "priority": "high",
            "project_id": project_a,
            "due_date": "2026-09-10",
            "due_time": "14:30",
            "start_date": "2026-09-01",
            "estimate_minutes": 45,
            "tags": ["errand", "health"],
            "goal_id": goal.id.to_string(),
        }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let tid: everyday_core::TaskId = task_id.parse().unwrap();
    let mut expected = vault.task(tid).unwrap();

    let mut check =
        |field: &str, args: serde_json::Value, apply: &mut dyn FnMut(&mut everyday_core::Task)| {
            call(&vault, "update_task", args);
            apply(&mut expected);
            let after = vault.task(tid).unwrap();
            expected.updated_at = after.updated_at;
            assert_eq!(after, expected, "naming only `{field}` must leave every other field alone");
        };

    check(
        "title",
        serde_json::json!({"task_id": task_id, "title": "Book the dentist (renamed)"}),
        &mut |t| {
            t.title = "Book the dentist (renamed)".into();
        },
    );
    check(
        "notes",
        serde_json::json!({"task_id": task_id, "notes": "Bring the insurance card"}),
        &mut |t| {
            t.notes = "Bring the insurance card".into();
        },
    );
    check("priority", serde_json::json!({"task_id": task_id, "priority": "low"}), &mut |t| {
        t.priority = everyday_core::Priority::Low;
    });
    check(
        "estimate_minutes",
        serde_json::json!({"task_id": task_id, "estimate_minutes": 90}),
        &mut |t| {
            t.estimate_minutes = Some(90);
        },
    );
    check(
        "start_date",
        serde_json::json!({"task_id": task_id, "start_date": "2026-09-02"}),
        &mut |t| {
            t.start_date = Some("2026-09-02".parse().unwrap());
        },
    );
    check(
        "due_date",
        serde_json::json!({"task_id": task_id, "due_date": "2026-09-12"}),
        &mut |t| {
            t.due_date = Some("2026-09-12".parse().unwrap());
        },
    );
    check("due_time", serde_json::json!({"task_id": task_id, "due_time": "09:00"}), &mut |t| {
        t.due_time = Some("09:00".parse().unwrap());
    });
    check("tags", serde_json::json!({"task_id": task_id, "tags": ["urgent"]}), &mut |t| {
        t.tags = vec!["urgent".into()];
    });
    check(
        "project_id",
        serde_json::json!({"task_id": task_id, "project_id": project_b}),
        &mut |t| {
            t.project_id = Some(project_b.parse().unwrap());
        },
    );
    check(
        "role_id",
        serde_json::json!({"task_id": task_id, "role_id": role.id.to_string()}),
        &mut |t| {
            t.purpose = Some(Purpose::Role { id: role.id });
        },
    );
}

#[test]
fn update_project_touches_only_the_named_field() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let project_id = call(
        &vault,
        "create_project",
        serde_json::json!({
            "name": "The deck",
            "notes": "Rebuild it before winter",
            "priority": "medium",
            "due_date": "2026-11-01",
            "start_date": "2026-09-01",
            "tags": ["home"],
        }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let pid: everyday_core::ProjectId = project_id.parse().unwrap();
    let mut expected = vault.project(pid).unwrap();

    let mut check = |field: &str,
                     args: serde_json::Value,
                     apply: &mut dyn FnMut(&mut everyday_core::Project)| {
        call(&vault, "update_project", args);
        apply(&mut expected);
        let after = vault.project(pid).unwrap();
        expected.updated_at = after.updated_at;
        assert_eq!(after, expected, "naming only `{field}` must leave every other field alone");
    };

    check(
        "name",
        serde_json::json!({"project_id": project_id, "name": "The back deck"}),
        &mut |p| {
            p.name = "The back deck".into();
        },
    );
    check(
        "notes",
        serde_json::json!({"project_id": project_id, "notes": "Rebuild and stain it"}),
        &mut |p| {
            p.notes = "Rebuild and stain it".into();
        },
    );
    check(
        "priority",
        serde_json::json!({"project_id": project_id, "priority": "urgent"}),
        &mut |p| {
            p.priority = everyday_core::Priority::Urgent;
        },
    );
    check(
        "due_date",
        serde_json::json!({"project_id": project_id, "due_date": "2026-11-15"}),
        &mut |p| {
            p.due_date = Some("2026-11-15".parse().unwrap());
        },
    );
    check(
        "start_date",
        serde_json::json!({"project_id": project_id, "start_date": "2026-09-05"}),
        &mut |p| {
            p.start_date = Some("2026-09-05".parse().unwrap());
        },
    );
    check("tags", serde_json::json!({"project_id": project_id, "tags": ["outdoors"]}), &mut |p| {
        p.tags = vec!["outdoors".into()];
    });
}

#[test]
fn update_note_touches_only_the_named_field() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let note_id = call(
        &vault,
        "create_note",
        serde_json::json!({ "title": "Shopping", "body": "oat milk", "tags": ["food"] }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let nid: everyday_core::NoteId = note_id.parse().unwrap();
    let mut expected = vault.note(nid).unwrap();

    let mut check = |field: &str, args: serde_json::Value, apply: &mut dyn FnMut(&mut Note)| {
        call(&vault, "update_note", args);
        apply(&mut expected);
        let after = vault.note(nid).unwrap();
        expected.updated_at = after.updated_at;
        assert_eq!(after, expected, "naming only `{field}` must leave every other field alone");
    };

    check("title", serde_json::json!({"note_id": note_id, "title": "Groceries"}), &mut |n| {
        n.title = "Groceries".into();
    });
    check("tags", serde_json::json!({"note_id": note_id, "tags": ["errand"]}), &mut |n| {
        n.tags = vec!["errand".into()];
    });
    check("pinned", serde_json::json!({"note_id": note_id, "pinned": true}), &mut |n| {
        n.pinned = true;
    });
    check("body", serde_json::json!({"note_id": note_id, "body": "oat milk\ncoffee"}), &mut |n| {
        n.body = everyday_core::RichDoc::from_markdown("oat milk\ncoffee");
    });
}

#[test]
fn update_entry_touches_only_the_named_field() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let journal = everyday_core::Journal::new("Daily");
    vault.save_journal(&journal).unwrap();

    let entry_id = call(
        &vault,
        "create_entry",
        serde_json::json!({
            "journal_id": journal.id.to_string(),
            "title": "Deck, day one",
            "body": "Cut the joists.",
            "date": "2026-09-07",
            "tags": ["deck"],
        }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let eid: everyday_core::EntryId = entry_id.parse().unwrap();
    let mut expected = vault.entry(eid).unwrap();

    let mut check =
        |field: &str, args: serde_json::Value, apply: &mut dyn FnMut(&mut everyday_core::Entry)| {
            call(&vault, "update_entry", args);
            apply(&mut expected);
            let after = vault.entry(eid).unwrap();
            expected.updated_at = after.updated_at;
            assert_eq!(after, expected, "naming only `{field}` must leave every other field alone");
        };

    check("title", serde_json::json!({"entry_id": entry_id, "title": "Deck, day two"}), &mut |e| {
        e.title = "Deck, day two".into();
    });
    check(
        "tags",
        serde_json::json!({"entry_id": entry_id, "tags": ["deck", "weekend"]}),
        &mut |e| {
            e.tags = vec!["deck".into(), "weekend".into()];
        },
    );
    check("starred", serde_json::json!({"entry_id": entry_id, "starred": true}), &mut |e| {
        e.starred = true;
    });
    check("date", serde_json::json!({"entry_id": entry_id, "date": "2026-09-08"}), &mut |e| {
        e.local_date = "2026-09-08".parse().unwrap();
    });
    check(
        "body",
        serde_json::json!({"entry_id": entry_id, "body": "Ran out of screws."}),
        &mut |e| {
            e.body = everyday_core::RichDoc::from_markdown("Ran out of screws.");
        },
    );
}

#[test]
fn update_item_touches_only_the_named_field() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.seed_library().unwrap();
    let shelf =
        call(&vault, "list_shelves", serde_json::json!({}))[0]["id"].as_str().unwrap().to_string();

    let item_id = call(
        &vault,
        "create_item",
        serde_json::json!({
            "shelf_id": shelf,
            "title": "Piranesi",
            "creator": "Susanna Clarke",
            // `done` at creation stamps both `started_on` and `finished_on`
            // (see `Item::set_status`), which is what gives this fixture
            // every patchable field set before a single `update_item` runs.
            "status": "done",
            "rating": 9,
            "notes": "Loved it",
            "tags": ["fiction"],
        }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let iid: everyday_core::ItemId = item_id.parse().unwrap();
    let mut expected = vault.item(iid).unwrap();
    assert!(expected.started_on.is_some() && expected.finished_on.is_some(), "the fixture's point");

    let mut check =
        |field: &str, args: serde_json::Value, apply: &mut dyn FnMut(&mut everyday_core::Item)| {
            call(&vault, "update_item", args);
            apply(&mut expected);
            let after = vault.item(iid).unwrap();
            expected.updated_at = after.updated_at;
            assert_eq!(after, expected, "naming only `{field}` must leave every other field alone");
        };

    check(
        "title",
        serde_json::json!({"item_id": item_id, "title": "Piranesi (reread)"}),
        &mut |i| {
            i.title = "Piranesi (reread)".into();
        },
    );
    check("creator", serde_json::json!({"item_id": item_id, "creator": "S. Clarke"}), &mut |i| {
        i.creator = "S. Clarke".into();
    });
    check("rating", serde_json::json!({"item_id": item_id, "rating": 7}), &mut |i| {
        i.rating = Some(70); // `rating_out_of_ten` stores out of 10 * 10
    });
    check("favourite", serde_json::json!({"item_id": item_id, "favourite": true}), &mut |i| {
        i.favourite = true;
    });
    check(
        "notes",
        serde_json::json!({"item_id": item_id, "notes": "Reread it in the autumn"}),
        &mut |i| {
            i.notes = "Reread it in the autumn".into();
        },
    );
    check(
        "started_on",
        serde_json::json!({"item_id": item_id, "started_on": "2026-08-01"}),
        &mut |i| {
            i.started_on = Some("2026-08-01".parse().unwrap());
        },
    );
    check(
        "finished_on",
        serde_json::json!({"item_id": item_id, "finished_on": "2026-08-20"}),
        &mut |i| {
            i.finished_on = Some("2026-08-20".parse().unwrap());
        },
    );
    check(
        "tags",
        serde_json::json!({"item_id": item_id, "tags": ["fiction", "reread"]}),
        &mut |i| {
            i.tags = vec!["fiction".into(), "reread".into()];
        },
    );
}

#[test]
fn update_goal_touches_only_the_named_field() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let role_a = Role::new("Work");
    vault.save_role(&role_a).unwrap();
    let role_b = Role::new("Parent");
    vault.save_role(&role_b).unwrap();

    let mut goal = Goal::new(role_a.id, "Ship the deck");
    goal.notes = "Before winter".to_string();
    goal.horizon = Some("2026-11-01".parse().unwrap());
    vault.save_goal(&goal).unwrap();
    let mut expected = goal.clone();

    let mut check = |field: &str, args: serde_json::Value, apply: &mut dyn FnMut(&mut Goal)| {
        call(&vault, "update_goal", args);
        apply(&mut expected);
        let after = vault.goal(goal.id).unwrap();
        expected.updated_at = after.updated_at;
        assert_eq!(after, expected, "naming only `{field}` must leave every other field alone");
    };

    check(
        "title",
        serde_json::json!({"goal_id": goal.id.to_string(), "title": "Ship the back deck"}),
        &mut |g| {
            g.title = "Ship the back deck".into();
        },
    );
    check(
        "notes",
        serde_json::json!({"goal_id": goal.id.to_string(), "notes": "Before the first frost"}),
        &mut |g| {
            g.notes = "Before the first frost".into();
        },
    );
    check(
        "horizon",
        serde_json::json!({"goal_id": goal.id.to_string(), "horizon": "2026-11-15"}),
        &mut |g| {
            g.horizon = Some("2026-11-15".parse().unwrap());
        },
    );
    check(
        "role_id",
        serde_json::json!({"goal_id": goal.id.to_string(), "role_id": role_b.id.to_string()}),
        &mut |g| {
            g.role_id = role_b.id;
        },
    );
}

#[test]
fn update_routine_touches_only_the_named_field() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let routine_id = call(
        &vault,
        "create_routine",
        serde_json::json!({
            "name": "Morning brief",
            "instructions": "Say good morning",
            "at": "07:00",
            "days": ["mon", "wed", "fri"],
            "grace_minutes": 15,
        }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let rid: everyday_core::RoutineId = routine_id.parse().unwrap();
    let mut expected = vault.routine(rid).unwrap();

    let mut check = |field: &str,
                     args: serde_json::Value,
                     apply: &mut dyn FnMut(&mut everyday_core::Routine)| {
        call(&vault, "update_routine", args);
        apply(&mut expected);
        let after = vault.routine(rid).unwrap();
        expected.updated_at = after.updated_at;
        assert_eq!(after, expected, "naming only `{field}` must leave every other field alone");
    };

    check(
        "name",
        serde_json::json!({"routine_id": routine_id, "name": "Weekday brief"}),
        &mut |r| {
            r.name = "Weekday brief".into();
        },
    );
    check(
        "instructions",
        serde_json::json!({"routine_id": routine_id, "instructions": "Say good morning and list today's tasks"}),
        &mut |r| {
            r.instructions = "Say good morning and list today's tasks".into();
        },
    );
    check(
        "grace_minutes",
        serde_json::json!({"routine_id": routine_id, "grace_minutes": 30}),
        &mut |r| {
            r.grace_minutes = 30;
        },
    );
    // `at` alone must leave `days` alone, the exact regression the tool's
    // own comment (`apply_update_routine_args`) describes.
    check("at", serde_json::json!({"routine_id": routine_id, "at": "08:00"}), &mut |r| {
        if let everyday_core::Trigger::Schedule { at, .. } = &mut r.trigger {
            *at = "08:00".parse().unwrap();
        }
    });
    // And `days` alone must leave `at` alone.
    check(
        "days",
        serde_json::json!({"routine_id": routine_id, "days": ["tue", "thu"]}),
        &mut |r| {
            if let everyday_core::Trigger::Schedule { days, .. } = &mut r.trigger {
                *days = vec![everyday_core::Weekday::Tue, everyday_core::Weekday::Thu];
            }
        },
    );
    check("enabled", serde_json::json!({"routine_id": routine_id, "enabled": false}), &mut |r| {
        r.enabled = false;
    });
}

#[test]
fn update_draft_touches_only_the_named_field() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut account = everyday_core::account::Account::new(
        everyday_core::account::Provider::Custom,
        "me@example.com",
    );
    account.services.mail = true;
    vault.save_account(&account).unwrap();

    let draft_id = call(
        &vault,
        "draft_message",
        serde_json::json!({
            "account_id": account.id.to_string(),
            "to": ["friend@example.com"],
            "cc": ["cc@example.com"],
            "bcc": ["bcc@example.com"],
            "subject": "Dinner Friday?",
            "body_html": "<p>Are you free?</p>",
        }),
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let did: everyday_core::id::DraftId = draft_id.parse().unwrap();
    let mut expected = vault.draft(did).unwrap();

    let mut check = |field: &str,
                     args: serde_json::Value,
                     apply: &mut dyn FnMut(&mut everyday_core::mail::Draft)| {
        call(&vault, "update_draft", args);
        apply(&mut expected);
        let after = vault.draft(did).unwrap();
        expected.updated_at = after.updated_at;
        assert_eq!(after, expected, "naming only `{field}` must leave every other field alone");
    };

    check("to", serde_json::json!({"draft_id": draft_id, "to": ["other@example.com"]}), &mut |d| {
        d.to = vec![everyday_core::mail::Address::bare("other@example.com")];
    });
    check("cc", serde_json::json!({"draft_id": draft_id, "cc": ["cc2@example.com"]}), &mut |d| {
        d.cc = vec![everyday_core::mail::Address::bare("cc2@example.com")];
    });
    check(
        "bcc",
        serde_json::json!({"draft_id": draft_id, "bcc": ["bcc2@example.com"]}),
        &mut |d| {
            d.bcc = vec![everyday_core::mail::Address::bare("bcc2@example.com")];
        },
    );
    check(
        "subject",
        serde_json::json!({"draft_id": draft_id, "subject": "Dinner Saturday?"}),
        &mut |d| {
            d.subject = "Dinner Saturday?".into();
        },
    );
    check(
        "body_html",
        serde_json::json!({"draft_id": draft_id, "body_html": "<p>Or Saturday?</p>"}),
        &mut |d| {
            d.body_html = "<p>Or Saturday?</p>".into();
        },
    );
}
