//! Background work, through the tools a model actually has.
//!
//! Nothing here runs a job: that is the worker's, in `everyday-service`, and
//! it has its own tests. What these tools do is write and read records, so
//! what is checked is exactly that -- what is queued, for which conversation,
//! what a model is told, and what it is refused.

use super::support::{ctx, ctx_drafting, vault};
use everyday_core::agent::tools::{self, Caller, Drafting, Effect, ToolContext};
use everyday_core::id::{ConversationId, JobId, NoteId};
use everyday_core::{Job, JobKind, JobOutput, JobQuery, JobStatus, Vault};
use jiff::Timestamp;
use serde_json::{Value, json};

/// A context for the chat assistant, in `conversation`.
fn asking(vault: &Vault, conversation: ConversationId) -> ToolContext<'_> {
    ToolContext {
        caller: Some(Caller::Assistant { conversation }),
        conversation: Some(conversation),
        ..ctx(vault)
    }
}

fn run(ctx: &ToolContext<'_>, tool: &str, args: Value) -> Value {
    tools::dispatch(ctx, tool, &args).unwrap_or_else(|e| panic!("{tool} failed: {e}"))
}

fn refused(ctx: &ToolContext<'_>, tool: &str, args: Value) -> String {
    tools::dispatch(ctx, tool, &args)
        .map(|v| panic!("{tool} unexpectedly succeeded: {v}"))
        .unwrap_err()
        .to_string()
}

fn every_job(vault: &Vault) -> Vec<Job> {
    vault.jobs(&JobQuery::default()).unwrap()
}

fn id_of(out: &Value) -> JobId {
    out["id"].as_str().expect("a job id").parse().expect("a job id that parses")
}

fn at(second: i64) -> Timestamp {
    Timestamp::from_second(1_790_000_000 + second).unwrap()
}

/// A job asked for `second` seconds after an arbitrary start, saved.
fn queued_at(vault: &Vault, question: &str, second: i64) -> Job {
    let mut job = vault
        .queue_job(JobKind::Research { question: question.into(), brief: String::new() }, None)
        .unwrap();
    job.created_at = at(second);
    vault.save_job(&job).unwrap();
    job
}

// ---- start_research ------------------------------------------------------

#[test]
fn starting_research_queues_a_job_that_comes_back_to_the_conversation_that_asked() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let conversation = ConversationId::new();
    let ctx = asking(&vault, conversation);

    let out = run(
        &ctx,
        "start_research",
        json!({
            "question": "Which e-bike is best for steep hills?",
            "brief": "Commutes 12 km each way; has ruled out hub motors; wants a short list.",
        }),
    );

    assert_eq!(out["ok"], true);
    assert_eq!(out["action"], "started");
    assert_eq!(out["kind"], "research");
    assert_eq!(out["status"], "queued");
    assert_eq!(out["question"], "Which e-bike is best for steep hills?");
    assert_eq!(out["name"], "Which e-bike is best for steep hills?");
    let note = out["note"].as_str().unwrap();
    assert!(note.contains("same reply"), "it must tell the model to say so now: {note}");
    assert!(note.contains("come back to this conversation"), "{note}");
    assert!(note.contains("do not wait"), "{note}");

    let jobs = every_job(&vault);
    assert_eq!(jobs.len(), 1);
    let job = &jobs[0];
    assert_eq!(job.id, id_of(&out), "the id reported is the record written");
    assert_eq!(job.status, JobStatus::Queued);
    assert_eq!(job.asked_in, Some(conversation));
    assert_eq!(
        job.kind,
        JobKind::Research {
            question: "Which e-bike is best for steep hills?".into(),
            brief: "Commutes 12 km each way; has ruled out hub motors; wants a short list.".into(),
        }
    );
    assert!(!job.seen && !job.answered && job.attempts == 0);
}

#[test]
fn the_question_is_tidied_and_a_brief_may_be_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = asking(&vault, ConversationId::new());

    run(&ctx, "start_research", json!({ "question": "  Which stove?\n", "brief": null }));
    run(&ctx, "start_research", json!({ "question": "Which tent?", "brief": "   " }));

    let mut got: Vec<(String, String)> = every_job(&vault)
        .into_iter()
        .map(|j| match j.kind {
            JobKind::Research { question, brief } => (question, brief),
            JobKind::Task { .. } => unreachable!("only research was started"),
        })
        .collect();
    got.sort();
    let expected = vec![
        ("Which stove?".to_string(), String::new()),
        ("Which tent?".to_string(), String::new()),
    ];
    assert_eq!(got, expected);
}

#[test]
fn research_started_from_a_script_or_an_mcp_client_has_nowhere_to_come_back_to() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let args = json!({ "question": "Which e-bike?", "brief": "For hills." });

    // The vault's owner acting directly, and an external agent: neither is
    // sitting in a thread that could be answered.
    let client = Caller::Mcp { client: "device-1".into() };
    let mcp = ToolContext { caller: Some(client), ..ctx(&vault) };
    for ctx in [ctx(&vault), mcp] {
        let out = run(&ctx, "start_research", args.clone());
        let note = out["note"].as_str().unwrap();
        assert!(note.contains("Assistant app"), "it should say where to look instead: {note}");
        assert!(!note.contains("come back to this conversation"), "{note}");
        assert_eq!(vault.job(id_of(&out)).unwrap().asked_in, None);
    }
}

#[test]
fn starting_the_same_research_twice_in_one_conversation_is_one_job() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let here = ConversationId::new();
    let ctx = asking(&vault, here);

    let first = run(&ctx, "start_research", json!({ "question": "Which e-bike?", "brief": "a" }));
    // Two tool calls in one step, or a retry: the same question, spelled a
    // little differently.
    let again =
        run(&ctx, "start_research", json!({ "question": "  which E-BIKE?  ", "brief": "b" }));
    assert_eq!(again["action"], "already_started");
    assert_eq!(again["id"], first["id"], "it points at the job already going");
    assert!(again["note"].as_str().unwrap().contains("Do not start another"), "{again}");
    assert_eq!(every_job(&vault).len(), 1);

    // Another question is another job, and so is the same one in a
    // different conversation: each is owed its own answer.
    run(&ctx, "start_research", json!({ "question": "Which tent?", "brief": "" }));
    let elsewhere = asking(&vault, ConversationId::new());
    run(&elsewhere, "start_research", json!({ "question": "Which e-bike?", "brief": "" }));
    assert_eq!(every_job(&vault).len(), 3);

    // And once the first has finished, asking again is a new piece of work.
    let mut done = vault.job(id_of(&first)).unwrap();
    done.begin();
    done.finish("Three bikes.");
    vault.save_job(&done).unwrap();
    let rerun = run(&ctx, "start_research", json!({ "question": "Which e-bike?", "brief": "" }));
    assert_eq!(rerun["action"], "started");
    assert_ne!(rerun["id"], first["id"]);
    assert_eq!(every_job(&vault).len(), 4);
}

#[test]
fn an_unattended_run_is_told_to_do_the_work_itself_and_nothing_is_queued() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let args = json!({ "question": "Which e-bike?", "brief": "For hills." });

    for ctx in [
        ToolContext { unattended: true, ..asking(&vault, ConversationId::new()) },
        ToolContext { unattended: true, ..ctx(&vault) },
    ] {
        let why = refused(&ctx, "start_research", args.clone());
        assert!(why.contains("may not start"), "got {why}");
        assert!(why.contains("yourself"), "it should say what to do instead: {why}");
    }
    assert!(every_job(&vault).is_empty(), "a refused call queues nothing");
}

#[test]
fn a_dream_can_neither_start_nor_cancel_a_job() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let queued = queued_at(&vault, "Which e-bike?", 0);
    let dreaming = ctx_drafting(&vault, Drafting::default());

    let why = refused(&dreaming, "start_research", json!({ "question": "Q?", "brief": "" }));
    assert!(why.contains("not something you can propose"), "got {why}");
    refused(&dreaming, "cancel_job", json!({ "id": queued.id.to_string() }));

    assert_eq!(every_job(&vault).len(), 1, "nothing was queued");
    assert_eq!(vault.job(queued.id).unwrap().status, JobStatus::Queued, "and nothing cancelled");
    assert_eq!(vault.pending_proposals().unwrap(), 0, "and no proposal made in their place");
}

#[test]
fn what_cannot_be_researched_reaches_the_model_as_an_error_it_can_act_on() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = asking(&vault, ConversationId::new());

    let err = refused(&ctx, "start_research", json!({ "brief": "For hills." }));
    assert!(err.contains("start_research") && err.contains("`question` is required"), "{err}");
    let err = refused(&ctx, "start_research", json!({ "question": "   ", "brief": "x" }));
    assert!(err.contains("`question` is required"), "a blank question is no question: {err}");

    let too_long = "q".repeat(everyday_core::job::MAX_QUESTION_BYTES + 1);
    let err = refused(&ctx, "start_research", json!({ "question": too_long, "brief": "" }));
    assert!(err.contains("2000"), "it should state the limit: {err}");
    assert!(err.contains("brief"), "and say where the context goes: {err}");

    let brief = "b".repeat(everyday_core::job::MAX_BRIEF_BYTES + 1);
    let err = refused(&ctx, "start_research", json!({ "question": "Q?", "brief": brief }));
    assert!(err.contains("brief"), "got {err}");

    assert!(every_job(&vault).is_empty(), "none of them left a job behind");
}

#[test]
fn the_jobs_tools_are_offered_by_a_backend_that_holds_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let offered: Vec<&str> = tools::available(&vault).iter().map(|t| t.name).collect();
    for name in ["start_research", "job_status", "cancel_job"] {
        assert!(offered.contains(&name), "{name} should be offered: {offered:?}");
    }
}

#[test]
fn nothing_about_background_work_is_ever_confirmed() {
    // Starting is the assistant's judgement and cancelling is undone by
    // asking again; the way out is `cancel_job`, not a question first. This
    // is the list the gate reads, so it is checked by what it would answer.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let job = queued_at(&vault, "Which e-bike?", 0);
    let ctx = ctx(&vault);

    let start = json!({ "question": "Q?", "brief": "" });
    assert_eq!(tools::effect_for(&ctx, "start_research", &start), Some(Effect::Write));
    assert_eq!(tools::effect_for(&ctx, "job_status", &json!({})), Some(Effect::Read));
    let cancel = json!({ "id": job.id.to_string() });
    assert_eq!(tools::effect_for(&ctx, "cancel_job", &cancel), Some(Effect::Write));
}

// ---- job_status ----------------------------------------------------------

#[test]
fn job_status_with_an_id_says_where_that_job_stands() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = ctx(&vault);
    let note = NoteId::new();

    let mut job = vault
        .queue_job(
            JobKind::Research {
                question: "Which e-bike is best for steep hills?".into(),
                brief: "Has ruled out hub motors.".into(),
            },
            None,
        )
        .unwrap();
    job.begin();
    job.progress = "Read 14 sources; writing up".into();
    job.steps = 31;
    job.add_note(note);
    vault.save_job(&job).unwrap();

    let out = run(&ctx, "job_status", json!({ "id": job.id.to_string() }));
    assert_eq!(out["id"], job.id.to_string());
    assert_eq!(out["kind"], "research");
    assert_eq!(out["title"], "Which e-bike is best for steep hills?");
    assert_eq!(out["status"], "running");
    assert_eq!(out["progress"], "Read 14 sources; writing up");
    assert_eq!(out["steps"], 31);
    assert_eq!(out["note_id"], note.to_string(), "the note is where the findings are");
    assert_eq!(out["answered"], false);
    assert_eq!(out["brief"], "Has ruled out hub motors.");
    assert!(out["created"].is_string() && out["started"].is_string());
    for absent in ["reason", "finished", "untrusted_text"] {
        assert!(out.get(absent).is_none(), "{absent} has nothing to say yet: {out}");
    }

    // A job that ended badly says why, and what it said last is kept apart
    // from the facts, as every stranger's words are: it was written from web
    // pages.
    job.fail("the endpoint refused");
    job.summary = "I got as far as the torque figures.".into();
    vault.save_job(&job).unwrap();
    let out = run(&ctx, "job_status", json!({ "id": job.id.to_string() }));
    assert_eq!(out["status"], "failed");
    assert_eq!(out["reason"], "the endpoint refused");
    assert!(out["finished"].is_string());
    assert_eq!(out["untrusted_text"]["summary"], "I got as far as the torque figures.");
    assert!(out.get("summary").is_none(), "the summary is not a plain field: {out}");
}

#[test]
fn job_status_for_a_job_that_is_not_there_says_where_to_find_one() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = ctx(&vault);

    let err = refused(&ctx, "job_status", json!({ "id": JobId::new().to_string() }));
    assert!(err.contains("no job with id"), "got {err}");
    assert!(err.contains("job_status"), "it should say how to find a real one: {err}");

    let err = refused(&ctx, "job_status", json!({ "id": "the research one" }));
    assert!(err.contains("must be a job id"), "got {err}");
}

#[test]
fn job_status_without_an_id_lists_the_recent_jobs_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = ctx(&vault);

    let empty = run(&ctx, "job_status", json!({}));
    assert_eq!(empty["count"], 0);
    assert_eq!(empty["jobs"], json!([]));

    let oldest = queued_at(&vault, "Oldest", 10);
    let middle = queued_at(&vault, "Middle", 20);
    let mut newest = queued_at(&vault, "Newest", 30);
    newest.begin();
    newest.progress = "Reading example.com".into();
    vault.save_job(&newest).unwrap();

    for args in [json!({}), json!({ "id": null })] {
        let out = run(&ctx, "job_status", args);
        assert_eq!(out["count"], 3);
        let titles: Vec<&str> =
            out["jobs"].as_array().unwrap().iter().map(|j| j["title"].as_str().unwrap()).collect();
        assert_eq!(titles, ["Newest", "Middle", "Oldest"]);
        assert_eq!(out["jobs"][0]["id"], newest.id.to_string());
        assert_eq!(out["jobs"][0]["status"], "running");
        assert_eq!(out["jobs"][0]["progress"], "Reading example.com");
        assert_eq!(out["jobs"][2]["status"], "queued");
        assert_eq!(out["jobs"][1]["id"], middle.id.to_string());
        assert_eq!(out["jobs"][2]["id"], oldest.id.to_string());
        // A list is not the place for what a job said or was told.
        let first = &out["jobs"][0];
        assert!(first.get("brief").is_none() && first.get("untrusted_text").is_none());
    }

    let two = run(&ctx, "job_status", json!({ "limit": 2 }));
    assert_eq!(two["count"], 2);
    assert_eq!(two["jobs"][1]["title"], "Middle", "a limit keeps the newest");
    let one = run(&ctx, "job_status", json!({ "limit": 0 }));
    assert_eq!(one["count"], 1, "zero rows is never what was meant");
    let all = run(&ctx, "job_status", json!({ "limit": 100_000 }));
    assert_eq!(all["count"], 3, "a huge ask is capped, not refused");
}

#[test]
fn job_status_lists_ten_by_default_and_never_more_than_fifty() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = ctx(&vault);
    for i in 0..52 {
        queued_at(&vault, &format!("Question {i}"), i);
    }

    assert_eq!(run(&ctx, "job_status", json!({}))["count"], 10);
    assert_eq!(run(&ctx, "job_status", json!({ "limit": 25 }))["count"], 25);
    assert_eq!(run(&ctx, "job_status", json!({ "limit": 5_000 }))["count"], 50);
    assert_eq!(run(&ctx, "job_status", json!({}))["jobs"][0]["title"], "Question 51");
}

// ---- cancel_job ----------------------------------------------------------

#[test]
fn cancelling_a_queued_job_cancels_it_on_the_spot_and_says_it_will_not_come_back() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let conversation = ConversationId::new();
    let ctx = asking(&vault, conversation);
    let started = run(&ctx, "start_research", json!({ "question": "Which stove?", "brief": "" }));
    let id = id_of(&started);

    let out = run(&ctx, "cancel_job", json!({ "id": id.to_string(), "reason": "  not needed  " }));
    assert_eq!(out["ok"], true);
    assert_eq!(out["action"], "cancelled");
    assert_eq!(out["id"], id.to_string());
    assert_eq!(out["status"], "cancelled");
    assert_eq!(out["name"], "Which stove?");
    assert_eq!(out["question"], "Which stove?");
    let note = out["note"].as_str().unwrap();
    assert!(note.contains("before it started"), "{note}");
    assert!(note.contains("will not come back"), "{note}");

    let job = vault.job(id).unwrap();
    assert_eq!(job.status, JobStatus::Cancelled);
    assert_eq!(job.reason, "not needed", "the reason is trimmed");
    assert!(job.finished_at.is_some());
    assert_eq!(job.asked_in, Some(conversation), "and still remembers where it was asked");
    assert!(!job.owes_answer(), "a cancelled job does not come back");
    assert!(vault.jobs(&JobQuery::unfinished()).unwrap().is_empty(), "the worker has no work");
}

#[test]
fn cancelling_without_a_reason_records_that_the_assistant_did_it() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = ctx(&vault);
    let job = queued_at(&vault, "Which stove?", 0);

    run(&ctx, "cancel_job", json!({ "id": job.id.to_string() }));
    assert_eq!(vault.job(job.id).unwrap().reason, "cancelled by the assistant at their request");
}

#[test]
fn cancelling_a_running_job_only_moves_the_record_and_keeps_what_it_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = ctx(&vault);
    let note = NoteId::new();
    let mut job = queued_at(&vault, "Which stove?", 0);
    job.begin();
    job.progress = "Reading example.com".into();
    job.add_note(note);
    vault.save_job(&job).unwrap();

    let out = run(&ctx, "cancel_job", json!({ "id": job.id.to_string() }));
    assert_eq!(out["status"], "cancelled");
    assert_eq!(out["note_id"], note.to_string(), "what it had written is still there to read");
    let said = out["note"].as_str().unwrap();
    assert!(said.contains("stop within moments") && said.contains("is kept"), "{said}");

    // The worker is what stops the turn, by watching this record; all the
    // tool owes it is the record.
    let after = vault.job(job.id).unwrap();
    assert_eq!(after.status, JobStatus::Cancelled);
    assert_eq!(after.outputs, [JobOutput::Note { note_id: note }]);
    assert_eq!(after.progress, "Reading example.com");
    assert!(after.started_at.is_some());
}

#[test]
fn a_finished_job_cannot_be_cancelled_and_is_left_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = ctx(&vault);

    let mut done = queued_at(&vault, "Done", 0);
    done.begin();
    done.finish("Three bikes.");
    let mut failed = queued_at(&vault, "Failed", 1);
    failed.begin();
    failed.fail("the endpoint refused");
    let mut cancelled = queued_at(&vault, "Cancelled", 2);
    cancelled.cancel("first").unwrap();
    for job in [&done, &failed, &cancelled] {
        vault.save_job(job).unwrap();
    }

    for (job, state) in [(&done, "done"), (&failed, "failed"), (&cancelled, "cancelled")] {
        let args = json!({ "id": job.id.to_string(), "reason": "late" });
        let err = refused(&ctx, "cancel_job", args);
        assert!(err.contains("already"), "got {err}");
        assert!(err.contains(state), "it should say how it ended: {err}");
        assert_eq!(&vault.job(job.id).unwrap(), job, "a refused cancel rewrites nothing");
    }
}

#[test]
fn cancelling_a_job_that_is_not_there_says_where_to_find_one() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = ctx(&vault);

    let err = refused(&ctx, "cancel_job", json!({ "id": JobId::new().to_string() }));
    assert!(err.contains("no job with id") && err.contains("job_status"), "got {err}");
    let err = refused(&ctx, "cancel_job", json!({}));
    assert!(err.contains("`id` is required"), "got {err}");
    let err = refused(&ctx, "cancel_job", json!({ "id": "that one" }));
    assert!(err.contains("must be a job id"), "got {err}");
}

#[test]
fn a_call_to_cancel_names_the_job_by_its_question_rather_than_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = ctx(&vault);
    let job = queued_at(&vault, "Which stove?", 0);

    assert_eq!(
        tools::describe(&ctx, "cancel_job", &json!({ "id": job.id.to_string() })).as_deref(),
        Some("Cancelled research: Which stove?")
    );
    assert_eq!(
        tools::describe(&ctx, "cancel_job", &json!({ "id": JobId::new().to_string() })),
        None,
        "a job that is gone is not guessed at"
    );
    assert_eq!(tools::describe(&ctx, "job_status", &json!({})), None);
}

// ---- the vault around them -------------------------------------------------

#[test]
fn a_read_only_vault_can_be_asked_after_but_not_told_to_start_or_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let first = vault(dir.path());
    let job = queued_at(&first, "Which stove?", 0);

    let second = everyday_vault::open(dir.path()).unwrap();
    second.unlock(None).unwrap();
    assert!(!second.is_writable(), "the second open should be read-only");
    let ctx = ctx(&second);

    let out = run(&ctx, "job_status", json!({}));
    assert_eq!(out["count"], 1);
    for (tool, args) in [
        ("start_research", json!({ "question": "Q?", "brief": "" })),
        ("cancel_job", json!({ "id": job.id.to_string() })),
    ] {
        let err = refused(&ctx, tool, args);
        assert!(err.contains("read-only"), "{tool}: got {err}");
    }
}
