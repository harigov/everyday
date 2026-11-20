//! Background work, through the tools a model actually has.
//!
//! Nothing here runs a job: that is the worker's, in `karmayoga-service`, and
//! it has its own tests. What these tools do is write and read records, so
//! what is checked is exactly that -- what is queued, for which conversation,
//! what a model is told, and what it is refused.

use super::support::{ctx, ctx_drafting, vault};
use jiff::{SignedDuration, Timestamp};
use karmayoga_core::agent::tools::{self, Caller, Domain, Drafting, Effect, ToolContext};
use karmayoga_core::connection::{
    AuthMethod, Connection, ConnectionState, RemoteTool, ToolHints, ToolPolicy, Transport,
};
use karmayoga_core::id::{ConversationId, JobId, NoteId};
use karmayoga_core::{
    Conversation, Job, JobKind, JobOutput, JobQuery, JobStatus, Observed, Probe, Vault,
};
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
            _ => unreachable!("only research was started"),
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
fn research_started_from_a_script_has_nowhere_to_come_back_to() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let args = json!({ "question": "Which e-bike?", "brief": "For hills." });

    // The vault's owner acting directly is not sitting in a thread that
    // could be answered.
    let out = run(&ctx(&vault), "start_research", args);
    let note = out["note"].as_str().unwrap();
    assert!(note.contains("Assistant app"), "it should say where to look instead: {note}");
    assert!(!note.contains("come back to this conversation"), "{note}");
    assert_eq!(vault.job(id_of(&out)).unwrap().asked_in, None);
}

#[test]
fn an_mcp_client_cannot_start_research_at_all() {
    // A job runs as the person with every tool the assistant has, and its
    // answer is a note any token that reads notes can open -- so it is the
    // routine bypass again. See `tools::NOT_OVER_MCP`.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let client = Caller::Mcp { client: "device-1".into() };
    let mcp = ToolContext { caller: Some(client), ..ctx(&vault) };

    let said = refused(&mcp, "start_research", json!({ "question": "Which e-bike?", "brief": "" }));
    assert!(said.contains("MCP"), "it should say why: {said}");
    assert!(every_job(&vault).is_empty(), "nothing was queued");
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

    // Nor watch, run a project, steer a job or report on one.
    for (tool, args) in [
        ("start_watch", json!({ "what": "CI", "notify_when": "", "how": "Check CI" })),
        ("start_project", json!({ "goal": "Plan a trip", "brief": "" })),
        ("update_job", json!({ "id": queued.id.to_string(), "look_now": true })),
        ("report_progress", json!({ "text": "Done." })),
    ] {
        let why = refused(&dreaming, tool, args);
        assert!(why.contains("not something you can propose"), "{tool}: got {why}");
    }

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

    let too_long = "q".repeat(karmayoga_core::job::MAX_QUESTION_BYTES + 1);
    let err = refused(&ctx, "start_research", json!({ "question": too_long, "brief": "" }));
    assert!(err.contains("2000"), "it should state the limit: {err}");
    assert!(err.contains("brief"), "and say where the context goes: {err}");

    let brief = "b".repeat(karmayoga_core::job::MAX_BRIEF_BYTES + 1);
    let err = refused(&ctx, "start_research", json!({ "question": "Q?", "brief": brief }));
    assert!(err.contains("brief"), "got {err}");

    assert!(every_job(&vault).is_empty(), "none of them left a job behind");
}

#[test]
fn the_jobs_tools_are_offered_by_a_backend_that_holds_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let offered: Vec<&str> = tools::available(&vault).iter().map(|t| t.name).collect();
    for name in [
        "start_research",
        "start_watch",
        "start_project",
        "update_job",
        "report_progress",
        "job_status",
        "cancel_job",
    ] {
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

    // Nor is starting a watch or a project, steering a job, or reporting.
    for tool in ["start_watch", "start_project", "update_job", "report_progress"] {
        assert_eq!(tools::effect_for(&ctx, tool, &json!({})), Some(Effect::Write), "{tool}");
    }
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

    let second = karmayoga_vault::open(dir.path()).unwrap();
    second.unlock(None).unwrap();
    assert!(!second.is_writable(), "the second open should be read-only");
    let ctx = ctx(&second);

    let out = run(&ctx, "job_status", json!({}));
    assert_eq!(out["count"], 1);
    for (tool, args) in [
        ("start_research", json!({ "question": "Q?", "brief": "" })),
        ("start_project", json!({ "goal": "Plan a trip", "brief": "" })),
        ("start_watch", json!({ "what": "CI", "notify_when": "", "how": "Check CI" })),
        ("update_job", json!({ "id": job.id.to_string(), "look_now": true })),
        ("report_progress", json!({ "text": "Done." })),
        ("cancel_job", json!({ "id": job.id.to_string() })),
    ] {
        let err = refused(&ctx, tool, args);
        assert!(err.contains("read-only"), "{tool}: got {err}");
    }
}

// ---- watches and projects ---------------------------------------------------

/// What a model sends to watch something by looking itself.
fn lease_watch() -> Value {
    json!({
        "what": "The lease",
        "notify_when": "when Sam replies",
        "how": "Check my inbox for a reply from Sam about the lease",
    })
}

/// A tool a server lists. `reads` says what it claims about itself, which is
/// all the default policy has to go on.
fn remote(name: &str, reads: bool) -> RemoteTool {
    RemoteTool {
        name: name.into(),
        title: None,
        description: String::new(),
        input_schema: json!({ "type": "object" }),
        hints: ToolHints { read_only: Some(reads), ..Default::default() },
    }
}

/// A vault with one connection, "Work GitHub", allowed for background use,
/// which has listed a tool that reads (so is allowed by default) and one that
/// merges (so asks by default). `change` edits it before it is saved.
fn github(vault: &Vault, change: impl FnOnce(&mut Connection)) -> Connection {
    let mut connection = Connection::new(
        "Work GitHub",
        Transport::Http { url: "https://mcp.example.com/mcp".into(), headers: vec![] },
        AuthMethod::None,
    );
    connection.unattended = true;
    change(&mut connection);
    let saved = vault.save_connection(connection).unwrap();
    let state = ConnectionState {
        tools: vec![remote("get_pull_request", true), remote("merge_pull_request", false)],
        tools_at: Some(Timestamp::now()),
        ..Default::default()
    };
    vault.save_connection_state(saved.id, state).unwrap();
    saved
}

fn ci_watch() -> Value {
    json!({
        "what": "CI on pull request 412",
        "notify_when": "when it finishes or fails",
        "connection": "Work GitHub",
        "tool": "get_pull_request",
        "arguments": { "number": 412 },
    })
}

/// A project that is running, and the transcript it reports from.
fn running_project(vault: &Vault) -> (Job, Conversation) {
    let kind = JobKind::Project {
        goal: "Plan a week in Lisbon".into(),
        brief: "Late March, two adults.".into(),
    };
    let mut job = vault.queue_job(kind, Some(ConversationId::new())).unwrap();
    let transcript = Conversation::for_job(job.id, "Plan a week in Lisbon");
    vault.save_conversation(&transcript).unwrap();
    job.begin();
    job.conversation_id = Some(transcript.id);
    vault.save_job(&job).unwrap();
    (job, transcript)
}

/// A project's own turn: nobody is watching, and the caller is the assistant
/// in the project's transcript.
fn in_the_project<'a>(vault: &'a Vault, transcript: &Conversation) -> ToolContext<'a> {
    ToolContext { unattended: true, ..asking(vault, transcript.id) }
}

// ---- start_watch ------------------------------------------------------------

#[test]
fn a_watch_that_looks_for_itself_is_written_asleep_and_due_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let conversation = ConversationId::new();
    let ctx = asking(&vault, conversation);

    let mut args = lease_watch();
    args["every_minutes"] = json!(30);
    args["hours"] = json!(12);
    args["act"] = json!("  summarise what Sam says  ");
    args["call_me"] = json!(true);
    let out = run(&ctx, "start_watch", args);

    assert_eq!(out["ok"], true);
    assert_eq!(out["action"], "started");
    assert_eq!(out["kind"], "watch");
    assert_eq!(out["status"], "waiting");
    assert_eq!(out["what"], "The lease");
    assert_eq!(out["name"], "The lease");
    assert_eq!(
        out["looks"],
        "asks the assistant: Check my inbox for a reply from Sam about the lease"
    );
    let rules = out["rules"].as_str().unwrap();
    for wanted in [
        "tells them: when Sam replies",
        "also does this when it has something to tell: summarise what Sam says",
        "looks every 30 minutes",
        "stops at",
        "rings them",
    ] {
        assert!(rules.contains(wanted), "should say {wanted:?}: {rules}");
    }
    assert!(out["until"].is_string() && out["next_look"].is_string());
    let note = out["note"].as_str().unwrap();
    for wanted in ["same reply", "what you will watch", "until when", "do not wait"] {
        assert!(note.contains(wanted), "should tell the model {wanted:?}: {note}");
    }
    assert!(note.contains("ring them"), "it was asked to ring: {note}");
    assert!(note.contains("update_job") && note.contains("cancel_job"), "{note}");

    let job = vault.job(id_of(&out)).unwrap();
    assert_eq!(job.status, JobStatus::Waiting);
    assert_eq!(job.asked_in, Some(conversation));
    assert_eq!(
        job.kind,
        JobKind::Watch {
            what: "The lease".into(),
            probe: Probe::Assistant {
                how: "Check my inbox for a reply from Sam about the lease".into()
            },
        }
    );
    assert_eq!(job.rules.notify_when, "when Sam replies");
    assert_eq!(job.rules.act, "summarise what Sam says", "tidied");
    assert_eq!(job.rules.every_minutes, 30);
    assert!(job.rules.call_me);
    assert_eq!(job.rules.until, Some(job.created_at + SignedDuration::from_hours(12)));
    assert_eq!(job.wake_at, Some(job.created_at), "its first look is due at once");
    assert!(job.is_due(Timestamp::now()));
    assert_eq!(job.observed, Some(Observed::default()));
    assert!(job.updates.is_empty() && !job.seen && !job.answered);

    // A watch asleep is the watch loop's, not the worker's: the sweep that
    // takes `Queued` and `Running` never sees it.
    assert!(vault.jobs(&JobQuery::unfinished()).unwrap().is_empty());
    assert_eq!(vault.jobs(&JobQuery::waiting()).unwrap().len(), 1);
    assert_eq!(vault.jobs(&JobQuery::live()).unwrap().len(), 1);
}

#[test]
fn a_watch_lasts_eight_hours_and_looks_every_five_minutes_unless_told_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    github(&vault, |_| {});
    let ctx = asking(&vault, ConversationId::new());

    // Any change worth knowing is an empty string, and is a choice rather
    // than something left out.
    let mut args = ci_watch();
    args["notify_when"] = json!("");
    let out = run(&ctx, "start_watch", args);
    let job = vault.job(id_of(&out)).unwrap();
    assert_eq!(job.rules.notify_when, "");
    assert_eq!(job.rules.every_minutes, 0, "unset, so the default");
    assert_eq!(job.rules.until, Some(job.created_at + SignedDuration::from_hours(8)));
    assert_eq!(job.every(), std::time::Duration::from_secs(5 * 60));
    let rules = out["rules"].as_str().unwrap();
    assert!(rules.contains("tells them about any change worth knowing"), "{rules}");
    assert!(rules.contains("looks every 5 minutes"), "{rules}");

    // A look that is a model turn is no more often than every quarter hour.
    let looking = run(&ctx, "start_watch", lease_watch());
    assert!(looking["rules"].as_str().unwrap().contains("looks every 15 minutes"));
    assert_eq!(vault.job(id_of(&looking)).unwrap().every().as_secs(), 15 * 60);
}

#[test]
fn a_watch_on_a_connection_tool_is_written_with_the_tool_and_its_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    github(&vault, |_| {});
    let ctx = asking(&vault, ConversationId::new());

    let out = run(&ctx, "start_watch", ci_watch());
    assert_eq!(out["looks"], "calls get_pull_request on Work GitHub");
    let job = vault.job(id_of(&out)).unwrap();
    assert_eq!(
        job.kind.probe(),
        Some(&Probe::Connection {
            connection: "Work GitHub".into(),
            tool: "get_pull_request".into(),
            arguments: json!({ "number": 412 }),
        })
    );

    // The name as the person calls it, however it was typed; written as the
    // panel shows it. A tool that takes none needs no arguments; and arguments
    // sent as the text of an object are understood.
    let mut args = ci_watch();
    args["what"] = json!("Another");
    args["connection"] = json!("  work GITHUB ");
    args.as_object_mut().unwrap().remove("arguments");
    let bare = run(&ctx, "start_watch", args);
    assert_eq!(
        vault.job(id_of(&bare)).unwrap().kind.probe(),
        Some(&Probe::Connection {
            connection: "Work GitHub".into(),
            tool: "get_pull_request".into(),
            arguments: json!({}),
        })
    );

    let mut args = ci_watch();
    args["what"] = json!("A third");
    args["arguments"] = json!("{\"number\": 7}");
    let text = run(&ctx, "start_watch", args);
    let probe = vault.job(id_of(&text)).unwrap().kind.probe().cloned().unwrap();
    assert_eq!(
        probe,
        Probe::Connection {
            connection: "Work GitHub".into(),
            tool: "get_pull_request".into(),
            arguments: json!({ "number": 7 }),
        }
    );
}

#[test]
fn a_watch_on_a_tool_that_could_not_run_unasked_is_refused_with_what_to_do_instead() {
    // Each case is a vault of its own, set up so exactly one thing is wrong.
    type Change = Box<dyn Fn(&mut Connection)>;
    let off: Change = Box::new(|c: &mut Connection| c.enabled = false);
    let background: Change = Box::new(|c: &mut Connection| c.unattended = false);
    let ask: Change = Box::new(|_: &mut Connection| {});
    let tool_off: Change = Box::new(|c: &mut Connection| {
        c.policies.insert("get_pull_request".into(), ToolPolicy::Off);
    });
    let unlisted: Change = Box::new(|_: &mut Connection| {});
    let merge = json!({
        "what": "Merge",
        "notify_when": "",
        "connection": "Work GitHub",
        "tool": "merge_pull_request",
    });
    let invented = json!({
        "what": "X", "notify_when": "", "connection": "Work GitHub", "tool": "delete_repo",
    });

    let settings = "Settings \u{2192} Connections";
    let cases: Vec<(&str, Change, Value, Vec<&str>)> = vec![
        ("the connection is off", off, ci_watch(), vec!["switched off", settings, "`how`"]),
        (
            "no background use",
            background,
            ci_watch(),
            vec!["not allowed for background use", settings, "`how`"],
        ),
        (
            "the tool asks first",
            ask,
            merge,
            vec!["set to ask first", "nobody to ask", "Allow", settings, "`how`"],
        ),
        ("the tool is off", tool_off, ci_watch(), vec!["switched off", "set it to Allow", "`how`"]),
        (
            "a tool the server never listed",
            unlisted,
            invented,
            vec!["no tool called", "find_connection_tools"],
        ),
    ];
    for (name, change, args, wanted) in cases {
        let dir = tempfile::tempdir().unwrap();
        let vault = vault(dir.path());
        github(&vault, &*change);
        let ctx = asking(&vault, ConversationId::new());

        let why = refused(&ctx, "start_watch", args);
        for wanted in wanted {
            assert!(why.contains(wanted), "{name}: should say {wanted:?}: {why}");
        }
        assert!(every_job(&vault).is_empty(), "{name}: nothing was started");
    }
}

#[test]
fn a_watch_on_a_connection_that_is_not_there_says_which_there_are() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = asking(&vault, ConversationId::new());

    let why = refused(&ctx, "start_watch", ci_watch());
    assert!(why.contains("none are set up"), "{why}");
    assert!(why.contains("Settings \u{2192} Connections") && why.contains("`how`"), "{why}");

    github(&vault, |_| {});
    let mut args = ci_watch();
    args["connection"] = json!("Home Linear");
    let why = refused(&ctx, "start_watch", args);
    assert!(why.contains("Home Linear") && why.contains("Theirs are: Work GitHub"), "{why}");
    assert!(why.contains("find_connection_tools"), "{why}");

    // A server that has not been asked what it has yet.
    let dir = tempfile::tempdir().unwrap();
    let fresh = self::vault(dir.path());
    let mut connection = Connection::new(
        "Work GitHub",
        Transport::Http { url: "https://mcp.example.com/mcp".into(), headers: vec![] },
        AuthMethod::None,
    );
    connection.unattended = true;
    fresh.save_connection(connection).unwrap();
    let why = refused(&asking(&fresh, ConversationId::new()), "start_watch", ci_watch());
    assert!(why.contains("not listed its tools yet"), "{why}");
    assert!(why.contains("find_connection_tools"), "{why}");

    assert!(every_job(&vault).is_empty() && every_job(&fresh).is_empty());
}

#[test]
fn a_watch_that_does_not_say_how_to_look_or_when_to_tell_is_refused_with_what_to_send() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    github(&vault, |_| {});
    let ctx = asking(&vault, ConversationId::new());
    let without = |key: &str| {
        let mut args = lease_watch();
        args.as_object_mut().unwrap().remove(key);
        args
    };
    let with = |key: &str, value: Value| {
        let mut args = lease_watch();
        args[key] = value;
        args
    };

    let cases: Vec<(Value, &str)> = vec![
        (without("what"), "`what` is required"),
        (with("what", json!("   ")), "`what` is required"),
        (without("notify_when"), "`notify_when` is required"),
        (with("notify_when", json!(null)), "`notify_when` is required"),
        (with("notify_when", json!(7)), "`notify_when` must be text"),
        (without("how"), "say how to look"),
        (with("connection", json!("Work GitHub")), "not both"),
        (json!({ "what": "x", "notify_when": "", "connection": "Work GitHub" }), "go together"),
        (json!({ "what": "x", "notify_when": "", "tool": "get_pull_request" }), "go together"),
        (with("arguments", json!({ "n": 1 })), "go with `connection` and `tool`"),
        (with("hours", json!(0)), "from 1 to 168"),
        (with("hours", json!(169)), "from 1 to 168"),
        (with("every_minutes", json!(5)), "15"),
        (with("every_minutes", json!(1441)), "1440"),
        (with("what", json!("w".repeat(301))), "300"),
        (with("how", json!("h".repeat(1001))), "1000"),
        (with("act", json!("a".repeat(1001))), "what to do"),
    ];
    for (args, wanted) in cases {
        let why = refused(&ctx, "start_watch", args.clone());
        assert!(why.contains(wanted), "{args}: should say {wanted:?}: {why}");
    }

    let mut bad = ci_watch();
    bad["arguments"] = json!([1, 2]);
    assert!(refused(&ctx, "start_watch", bad).contains("must be an object"));
    let mut bad = ci_watch();
    bad["arguments"] = json!("not json");
    assert!(refused(&ctx, "start_watch", bad).contains("must be an object"));

    assert!(every_job(&vault).is_empty(), "none of them left a watch behind");
}

#[test]
fn starting_the_same_watch_twice_in_one_conversation_is_one_watch() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let here = ConversationId::new();
    let ctx = asking(&vault, here);

    let first = run(&ctx, "start_watch", lease_watch());
    let mut again = lease_watch();
    again["what"] = json!("  the LEASE ");
    again["notify_when"] = json!("something else");
    let twice = run(&ctx, "start_watch", again);
    assert_eq!(twice["action"], "already_started");
    assert_eq!(twice["id"], first["id"], "it points at the watch already going");
    let note = twice["note"].as_str().unwrap();
    assert!(note.contains("Do not start another") && note.contains("update_job"), "{note}");
    assert_eq!(every_job(&vault).len(), 1);
    assert_eq!(
        vault.job(id_of(&first)).unwrap().rules.notify_when,
        "when Sam replies",
        "and the second call changed nothing"
    );

    // Another conversation is owed its own, and so is a different thing.
    let elsewhere = asking(&vault, ConversationId::new());
    run(&elsewhere, "start_watch", lease_watch());
    let mut other = lease_watch();
    other["what"] = json!("The flat");
    run(&ctx, "start_watch", other);
    assert_eq!(every_job(&vault).len(), 3);

    // A question and a project of the same words are not a watch of them.
    run(&ctx, "start_research", json!({ "question": "The flat", "brief": "" }));
    run(&ctx, "start_project", json!({ "goal": "The lease", "brief": "" }));
    assert_eq!(every_job(&vault).len(), 5);

    // Once it has been stopped, asking again is a new watch.
    run(&ctx, "cancel_job", json!({ "id": first["id"] }));
    let rerun = run(&ctx, "start_watch", lease_watch());
    assert_eq!(rerun["action"], "started");
    assert_ne!(rerun["id"], first["id"]);
}

#[test]
fn nothing_that_starts_or_steers_background_work_is_open_to_an_unattended_run_or_mcp() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let watching = run(&asking(&vault, ConversationId::new()), "start_watch", lease_watch());
    let id = watching["id"].as_str().unwrap().to_string();
    let before = vault.job(id_of(&watching)).unwrap();

    let calls = [
        ("start_watch", lease_watch(), "may not start a watch"),
        ("start_project", json!({ "goal": "Plan a trip", "brief": "" }), "may not start a project"),
        ("update_job", json!({ "id": id, "notify_when": "always" }), "may not change a watch"),
    ];
    for unattended in [
        ToolContext { unattended: true, ..asking(&vault, ConversationId::new()) },
        ToolContext { unattended: true, ..ctx(&vault) },
    ] {
        for (tool, args, wanted) in &calls {
            let why = refused(&unattended, tool, args.clone());
            assert!(why.contains(wanted), "{tool}: got {why}");
            assert!(why.contains("of its own"), "{tool}: {why}");
        }
    }

    let client = Caller::Mcp { client: "device-1".into() };
    let mcp = ToolContext { caller: Some(client), ..ctx(&vault) };
    for (tool, args) in [
        ("start_watch", lease_watch()),
        ("start_project", json!({ "goal": "Plan a trip", "brief": "" })),
        ("update_job", json!({ "id": id, "notify_when": "always" })),
        ("report_progress", json!({ "text": "Done." })),
    ] {
        let why = refused(&mcp, tool, args);
        assert!(why.contains("MCP") && why.contains("in the app"), "{tool}: {why}");
    }

    assert_eq!(every_job(&vault).len(), 1, "nothing was started");
    assert_eq!(vault.job(before.id).unwrap(), before, "and the watch is as it was");
}

// ---- start_project ----------------------------------------------------------

#[test]
fn starting_a_project_queues_a_job_that_comes_back_to_the_conversation_that_asked() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let conversation = ConversationId::new();
    let ctx = asking(&vault, conversation);

    let out = run(
        &ctx,
        "start_project",
        json!({
            "goal": "  Plan a week in Lisbon in late March  ",
            "brief": "Two adults, no car, about 2000 euros; likes food markets.",
        }),
    );
    assert_eq!(out["ok"], true);
    assert_eq!(out["action"], "started");
    assert_eq!(out["kind"], "project");
    assert_eq!(out["status"], "queued");
    assert_eq!(out["goal"], "Plan a week in Lisbon in late March");
    assert_eq!(out["name"], "Plan a week in Lisbon in late March");
    let note = out["note"].as_str().unwrap();
    for wanted in [
        "same reply",
        "ask them things in their chat",
        "short updates",
        "come back to this conversation",
        "do not wait",
    ] {
        assert!(note.contains(wanted), "should tell the model {wanted:?}: {note}");
    }

    let job = vault.job(id_of(&out)).unwrap();
    assert_eq!(job.status, JobStatus::Queued, "the worker picks it up");
    assert_eq!(job.asked_in, Some(conversation));
    assert_eq!(
        job.kind,
        JobKind::Project {
            goal: "Plan a week in Lisbon in late March".into(),
            brief: "Two adults, no car, about 2000 euros; likes food markets.".into(),
        }
    );
    assert!(job.rules.is_empty() && job.wake_at.is_none() && job.observed.is_none());
    assert_eq!(vault.jobs(&JobQuery::unfinished()).unwrap().len(), 1);

    // Started from a script, there is nowhere to come back to.
    let out = run(&self::ctx(&vault), "start_project", json!({ "goal": "Tidy the notes" }));
    let note = out["note"].as_str().unwrap();
    assert!(note.contains("Assistant app") && !note.contains("come back to this conversation"));
    assert_eq!(vault.job(id_of(&out)).unwrap().asked_in, None);
}

#[test]
fn starting_the_same_project_twice_in_one_conversation_is_one_project() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = asking(&vault, ConversationId::new());

    let first = run(&ctx, "start_project", json!({ "goal": "Plan a trip", "brief": "a" }));
    let again = run(&ctx, "start_project", json!({ "goal": " plan A TRIP ", "brief": "b" }));
    assert_eq!(again["action"], "already_started");
    assert_eq!(again["id"], first["id"]);
    assert!(again["note"].as_str().unwrap().contains("Do not start another"));
    assert_eq!(every_job(&vault).len(), 1);

    // A watch of the same words is not the project.
    let mut watch = lease_watch();
    watch["what"] = json!("Plan a trip");
    run(&ctx, "start_watch", watch);
    assert_eq!(every_job(&vault).len(), 2);
}

#[test]
fn a_project_needs_a_goal_that_fits() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = asking(&vault, ConversationId::new());

    let err = refused(&ctx, "start_project", json!({ "brief": "x" }));
    assert!(err.contains("`goal` is required"), "{err}");
    let err = refused(&ctx, "start_project", json!({ "goal": "  ", "brief": "x" }));
    assert!(err.contains("`goal` is required"), "{err}");
    let too_long = "g".repeat(karmayoga_core::job::MAX_GOAL_BYTES + 1);
    let err = refused(&ctx, "start_project", json!({ "goal": too_long, "brief": "" }));
    assert!(err.contains("2000") && err.contains("brief"), "{err}");
    let brief = "b".repeat(karmayoga_core::job::MAX_BRIEF_BYTES + 1);
    let err = refused(&ctx, "start_project", json!({ "goal": "Plan", "brief": brief }));
    assert!(err.contains("brief"), "{err}");
    assert!(every_job(&vault).is_empty());
}

// ---- update_job ------------------------------------------------------------

#[test]
fn update_job_changes_what_a_watch_tells_how_often_it_looks_and_when_it_stops() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = asking(&vault, ConversationId::new());
    let started = run(&ctx, "start_watch", lease_watch());
    let id = started["id"].as_str().unwrap();
    let before = vault.job(id_of(&started)).unwrap();

    let out = run(
        &ctx,
        "update_job",
        json!({
            "id": id,
            "notify_when": "  only if Sam says no  ",
            "act": "draft a reply",
            "every_minutes": 20,
            "hours": 3,
            "call_me": true,
        }),
    );
    assert_eq!(out["ok"], true);
    assert_eq!(out["action"], "updated");
    assert_eq!(out["kind"], "watch");
    assert_eq!(out["status"], "waiting");
    assert_eq!(out["what"], "The lease");
    assert_eq!(out["changed"], json!(["notify_when", "act", "every_minutes", "hours", "call_me"]));
    let rules = out["rules"].as_str().unwrap();
    for wanted in
        ["tells them: only if Sam says no", "draft a reply", "looks every 20 minutes", "rings them"]
    {
        assert!(rules.contains(wanted), "should say {wanted:?}: {rules}");
    }
    assert!(out["until"].is_string());
    let note = out["note"].as_str().unwrap();
    assert!(note.contains("Confirm") && note.contains("sentence"), "{note}");

    let job = vault.job(before.id).unwrap();
    assert_eq!(job.rules.notify_when, "only if Sam says no");
    assert_eq!(job.rules.act, "draft a reply");
    assert_eq!(job.rules.every_minutes, 20);
    assert!(job.rules.call_me);
    let until = job.rules.until.unwrap();
    let from_now = until.as_second() - Timestamp::now().as_second();
    assert!((3 * 3600 - 30..=3 * 3600).contains(&from_now), "three hours from now: {from_now}");
    assert_eq!(job.kind, before.kind, "what it watches is not a rule");
    assert_eq!(job.status, JobStatus::Waiting);
    assert_eq!(job.created_at, before.created_at);

    // An empty string goes back to the defaults, and the rest is left alone.
    let out = run(&ctx, "update_job", json!({ "id": id, "notify_when": "", "act": "" }));
    assert_eq!(out["changed"], json!(["notify_when", "act"]));
    let job = vault.job(before.id).unwrap();
    assert_eq!((job.rules.notify_when.as_str(), job.rules.act.as_str()), ("", ""));
    assert_eq!(job.rules.every_minutes, 20, "not mentioned, so not changed");
    assert!(job.rules.call_me);
    assert!(out["rules"].as_str().unwrap().contains("tells them about any change worth knowing"));

    // And ringing can be switched off again.
    run(&ctx, "update_job", json!({ "id": id, "call_me": false }));
    assert!(!vault.job(before.id).unwrap().rules.call_me);
}

#[test]
fn a_watch_made_to_look_more_often_looks_sooner_and_one_told_to_look_now_looks_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    github(&vault, |_| {});
    let ctx = asking(&vault, ConversationId::new());
    let started = run(&ctx, "start_watch", ci_watch());
    let id = started["id"].as_str().unwrap();

    // It last looked a moment ago and is asleep for an hour.
    let mut job = vault.job(id_of(&started)).unwrap();
    let in_an_hour = Timestamp::now() + SignedDuration::from_hours(1);
    job.sleep_until(in_an_hour);
    vault.save_job(&job).unwrap();

    // Looking more often brings the next look in to the new pace.
    run(&ctx, "update_job", json!({ "id": id, "every_minutes": 2 }));
    let woken = vault.job(job.id).unwrap().wake_at.unwrap();
    let from_now = woken.as_second() - Timestamp::now().as_second();
    assert!((60..=120).contains(&from_now), "two minutes from now, not an hour: {from_now}");

    // Looking less often does not push a look that is sooner back.
    run(&ctx, "update_job", json!({ "id": id, "every_minutes": 60 }));
    assert_eq!(vault.job(job.id).unwrap().wake_at, Some(woken));

    // And look_now is now.
    let before = Timestamp::now();
    let out = run(&ctx, "update_job", json!({ "id": id, "look_now": true }));
    assert_eq!(out["changed"], json!(["look_now"]));
    let job = vault.job(job.id).unwrap();
    assert!(job.wake_at.unwrap() >= before && job.is_due(Timestamp::now()));
    assert_eq!(job.status, JobStatus::Waiting, "still asleep, and due");
}

#[test]
fn a_change_that_is_refused_leaves_the_watch_exactly_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = asking(&vault, ConversationId::new());
    let started = run(&ctx, "start_watch", lease_watch());
    let id = started["id"].as_str().unwrap();
    let before = vault.job(id_of(&started)).unwrap();

    for (args, wanted) in [
        (json!({ "id": id }), "nothing to change"),
        (json!({ "id": id, "look_now": false }), "nothing to change"),
        // A look that is a model turn is no more often than every 15 minutes.
        (json!({ "id": id, "notify_when": "x", "every_minutes": 5 }), "15"),
        (json!({ "id": id, "every_minutes": 5000 }), "1440"),
        (json!({ "id": id, "hours": 0 }), "from 1 to 168"),
        (json!({ "id": id, "hours": 500 }), "from 1 to 168"),
        (json!({ "id": id, "act": "a".repeat(1001) }), "what to do"),
        (json!({ "id": id, "notify_when": 5 }), "must be text"),
        (json!({ "id": JobId::new().to_string(), "look_now": true }), "no job with id"),
        (json!({ "id": "the lease one", "look_now": true }), "must be a job id"),
        (json!({ "look_now": true }), "`id` is required"),
    ] {
        let why = refused(&ctx, "update_job", args.clone());
        assert!(why.contains(wanted), "{args}: should say {wanted:?}: {why}");
        assert_eq!(vault.job(before.id).unwrap(), before, "{args} rewrote it");
    }
}

#[test]
fn only_a_watch_that_is_still_going_can_be_changed() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = asking(&vault, ConversationId::new());

    // Research and a project have no rules, and the answer says what to use.
    let research = queued_at(&vault, "Which stove?", 0);
    let (project, _) = running_project(&vault);
    for (job, kind) in [(&research, "research"), (&project, "project")] {
        let why = refused(&ctx, "update_job", json!({ "id": job.id.to_string(), "hours": 2 }));
        assert!(why.contains(&format!("a {kind} job has no rules")), "{why}");
        assert!(why.contains("cancel_job"), "{why}");
        assert_eq!(&vault.job(job.id).unwrap(), job);
    }

    // One that has ended is over.
    let started = run(&ctx, "start_watch", lease_watch());
    let mut done = vault.job(id_of(&started)).unwrap();
    done.finish("I've stopped watching The lease: the 8 hours are up.");
    vault.save_job(&done).unwrap();
    let why = refused(&ctx, "update_job", json!({ "id": done.id.to_string(), "hours": 2 }));
    assert!(why.contains("already done") && why.contains("start a new one"), "{why}");
    assert_eq!(vault.job(done.id).unwrap(), done);
}

#[test]
fn a_watch_that_has_run_its_time_but_not_been_closed_can_still_be_told_to_carry_on() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = asking(&vault, ConversationId::new());
    let started = run(&ctx, "start_watch", lease_watch());
    let id = started["id"].as_str().unwrap();

    // Made nine hours ago with the default eight, and not looked at since.
    let mut job = vault.job(id_of(&started)).unwrap();
    let nine_hours_ago = Timestamp::now() - SignedDuration::from_hours(9);
    job.created_at = nine_hours_ago;
    job.rules.until = Some(nine_hours_ago + SignedDuration::from_hours(8));
    vault.save_job(&job).unwrap();
    assert!(job.has_expired(Timestamp::now()));

    // A rule can change without the end moving, and the end can be moved on.
    run(&ctx, "update_job", json!({ "id": id, "notify_when": "only if it fails" }));
    run(&ctx, "update_job", json!({ "id": id, "hours": 4 }));
    let job = vault.job(job.id).unwrap();
    assert!(!job.has_expired(Timestamp::now()));
    assert_eq!(job.rules.notify_when, "only if it fails");
}

// ---- report_progress ---------------------------------------------------------

#[test]
fn only_a_projects_own_turn_can_report_progress() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let text = json!({ "text": "Flights found." });

    // Nobody, and a thread nobody opened a project in.
    let why = refused(&ctx(&vault), "report_progress", text.clone());
    assert!(why.contains("only for a project's own turn"), "{why}");
    let chat = Conversation::new();
    vault.save_conversation(&chat).unwrap();
    for conversation in [ConversationId::new(), chat.id] {
        let why = refused(&asking(&vault, conversation), "report_progress", text.clone());
        assert!(why.contains("only for a project's own turn"), "{why}");
        assert!(why.contains("say it in your reply"), "it should say what to do: {why}");
    }

    // The transcript of a job that is not a project.
    let mut research = queued_at(&vault, "Which stove?", 0);
    let transcript = Conversation::for_job(research.id, "Which stove?");
    vault.save_conversation(&transcript).unwrap();
    research.begin();
    research.conversation_id = Some(transcript.id);
    vault.save_job(&research).unwrap();
    let why = refused(&in_the_project(&vault, &transcript), "report_progress", text.clone());
    assert!(why.contains("only for a project's own turn"), "{why}");

    // A project that is not running: queued, or over.
    let (mut project, transcript) = running_project(&vault);
    project.status = JobStatus::Queued;
    vault.save_job(&project).unwrap();
    refused(&in_the_project(&vault, &transcript), "report_progress", text.clone());
    project.finish("Planned.");
    vault.save_job(&project).unwrap();
    refused(&in_the_project(&vault, &transcript), "report_progress", text.clone());

    // A watch's look is not a project's turn either.
    let started = run(&asking(&vault, ConversationId::new()), "start_watch", lease_watch());
    assert!(vault.job(id_of(&started)).unwrap().updates.is_empty());
    for job in every_job(&vault) {
        assert!(job.updates.is_empty(), "nothing was reported to {}", job.title());
    }
}

#[test]
fn a_project_reports_progress_to_be_sent_on_and_at_most_one_every_three_minutes() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let (project, transcript) = running_project(&vault);
    let ctx = in_the_project(&vault, &transcript);

    let said = json!({ "text": "  Flights found; now on places to stay. " });
    let out = run(&ctx, "report_progress", said);
    assert_eq!(out["ok"], true);
    assert_eq!(out["action"], "reported");
    assert_eq!(out["kind"], "project");
    assert_eq!(out["id"], project.id.to_string());
    let note = out["note"].as_str().unwrap();
    assert!(note.contains("within seconds") && note.contains("next milestone"), "{note}");

    let job = vault.job(project.id).unwrap();
    assert_eq!(job.updates.len(), 1);
    assert_eq!(job.updates[0].text, "Flights found; now on places to stay.");
    assert!(!job.updates[0].delivered, "the worker is what sends it");
    assert_eq!(job.undelivered().count(), 1);
    assert_eq!(job.status, JobStatus::Running, "and it carries on");

    // Another straight away is too soon, whether or not the first has gone.
    let why = refused(&ctx, "report_progress", json!({ "text": "Booked the flights." }));
    assert!(why.contains("every three minutes") && why.contains("next milestone"), "{why}");
    assert_eq!(vault.job(project.id).unwrap().updates.len(), 1);
    let mut sent = vault.job(project.id).unwrap();
    sent.updates[0].delivered = true;
    vault.save_job(&sent).unwrap();
    refused(&ctx, "report_progress", json!({ "text": "Booked the flights." }));

    // Four minutes on it is fine, and both are kept in order.
    sent.updates[0].at = Timestamp::now() - SignedDuration::from_mins(4);
    vault.save_job(&sent).unwrap();
    run(&ctx, "report_progress", json!({ "text": "Booked the flights." }));
    let job = vault.job(project.id).unwrap();
    let texts: Vec<&str> = job.updates.iter().map(|u| u.text.as_str()).collect();
    assert_eq!(texts, ["Flights found; now on places to stay.", "Booked the flights."]);
    assert_eq!(job.undelivered().count(), 1, "only the new one is still to go");

    let why = refused(&ctx, "report_progress", json!({ "text": "   " }));
    assert!(why.contains("`text` is required"), "{why}");
}

// ---- job_status and cancel_job, for the new kinds -----------------------------

#[test]
fn job_status_says_how_a_watch_looks_what_it_has_seen_and_what_it_told_them() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    github(&vault, |_| {});
    let ctx = asking(&vault, ConversationId::new());
    let started = run(&ctx, "start_watch", ci_watch());

    let mut job = vault.job(id_of(&started)).unwrap();
    job.observed = Some(Observed {
        fingerprint: "abc".into(),
        last: "state: running, 2 of 5 checks passed".into(),
        checked_at: Some(Timestamp::now() - SignedDuration::from_mins(2)),
        checks: 7,
        failures: 1,
        trouble: "the server timed out".into(),
    });
    job.sleep_until(Timestamp::now() + SignedDuration::from_mins(3));
    job.add_update("CI is running: 2 of 5 checks passed.", Timestamp::now(), true);
    job.add_update("A check failed.", Timestamp::now(), false);
    vault.save_job(&job).unwrap();

    let out = run(&ctx, "job_status", json!({ "id": job.id.to_string() }));
    assert_eq!(out["kind"], "watch");
    assert_eq!(out["title"], "CI on pull request 412");
    assert_eq!(out["status"], "waiting");
    assert_eq!(out["looks"], "calls get_pull_request on Work GitHub");
    assert_eq!(out["probe"]["type"], "connection");
    assert_eq!(out["probe"]["tool"], "get_pull_request");
    assert_eq!(out["probe"]["arguments"], json!({ "number": 412 }));
    let rules = out["rules"].as_str().unwrap();
    assert!(rules.contains("tells them: when it finishes or fails"), "{rules}");
    assert!(rules.contains("looks every 5 minutes") && rules.contains("stops at"), "{rules}");
    // Three minutes off, give or take the second it took to ask.
    assert!(out["next_look"].as_str().unwrap().contains(" minutes)"), "{out}");
    assert!(out["until"].as_str().unwrap().contains("hours"), "{out}");
    assert_eq!(out["checks"], 7);
    assert_eq!(out["failed_looks_in_a_row"], 1);
    assert!(out["last_checked"].as_str().unwrap().contains("2 minutes ago"), "{out}");
    assert_eq!(out["updates"], 2);
    assert!(out["last_update_at"].is_string());

    // What a service said, and what was made of it, is not a plain fact.
    let untrusted = &out["untrusted_text"];
    assert_eq!(untrusted["trouble"], "the server timed out");
    assert_eq!(untrusted["last_observation"], "state: running, 2 of 5 checks passed");
    assert_eq!(untrusted["recent_updates"][0]["text"], "CI is running: 2 of 5 checks passed.");
    assert_eq!(untrusted["recent_updates"][0]["sent"], true);
    assert_eq!(untrusted["recent_updates"][1]["sent"], false);
    for plain in ["trouble", "last_observation", "recent_updates", "summary"] {
        assert!(out.get(plain).is_none(), "{plain} is not a plain field: {out}");
    }

    // A list is shorter, and a watch asleep is in it with when it next looks.
    queued_at(&vault, "Which stove?", -10);
    let list = run(&ctx, "job_status", json!({}));
    assert_eq!(list["count"], 2);
    let row = list["jobs"].as_array().unwrap().iter().find(|j| j["kind"] == "watch").unwrap();
    assert_eq!(row["status"], "waiting");
    assert!(row["next_look"].is_string() && row["until"].is_string());
    for absent in ["looks", "probe", "rules", "untrusted_text"] {
        assert!(row.get(absent).is_none(), "{absent} is not for a list: {row}");
    }
}

#[test]
fn job_status_for_a_project_shows_its_brief_and_what_it_has_told_them() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let (mut project, _) = running_project(&vault);
    project.progress = "Looking at places to stay".into();
    project.add_update("Flights found.", Timestamp::now(), true);
    vault.save_job(&project).unwrap();

    let out = run(&ctx(&vault), "job_status", json!({ "id": project.id.to_string() }));
    assert_eq!(out["kind"], "project");
    assert_eq!(out["title"], "Plan a week in Lisbon");
    assert_eq!(out["status"], "running");
    assert_eq!(out["brief"], "Late March, two adults.");
    assert_eq!(out["progress"], "Looking at places to stay");
    assert_eq!(out["updates"], 1);
    assert_eq!(out["untrusted_text"]["recent_updates"][0]["text"], "Flights found.");
    for absent in ["rules", "probe", "next_look", "until"] {
        assert!(out.get(absent).is_none(), "{absent} is for a watch: {out}");
    }
}

#[test]
fn what_a_watch_says_is_left_out_for_a_caller_kept_from_a_domain() {
    static PRIVATE: [Domain; 4] =
        [Domain::Money, Domain::ScreenTime, Domain::Files, Domain::Health];
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let started = run(&asking(&vault, ConversationId::new()), "start_watch", lease_watch());
    let mut job = vault.job(id_of(&started)).unwrap();
    job.observed = Some(Observed {
        last: "balance: 12,400".into(),
        checks: 2,
        trouble: "could not read the balance".into(),
        ..Default::default()
    });
    job.add_update("The balance moved.", Timestamp::now(), true);
    vault.save_job(&job).unwrap();

    let kept_from = ToolContext { withheld: &PRIVATE, ..ctx(&vault) };
    let out = run(&kept_from, "job_status", json!({ "id": job.id.to_string() }));
    assert_eq!(out["title"], "a watch job", "what is watched may name anything");
    assert_eq!(out["probe"], json!({ "type": "assistant" }));
    assert!(out["withheld"].is_string());
    for left_out in ["looks", "rules", "untrusted_text", "updates", "last_update_at"] {
        assert!(out.get(left_out).is_none(), "{left_out} should be left out: {out}");
    }
    assert!(out["next_look"].is_string() && out["until"].is_string());
    let text = out.to_string();
    for secret in ["The lease", "Sam", "balance", "inbox"] {
        assert!(!text.contains(secret), "{secret:?} leaked: {text}");
    }
}

#[test]
fn cancelling_a_watch_asleep_stops_it_for_good_and_says_it_will_not_report_again() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let conversation = ConversationId::new();
    let ctx = asking(&vault, conversation);
    let started = run(&ctx, "start_watch", lease_watch());
    let id = id_of(&started);

    // It has been at it for hours: asleep, with looks behind it.
    let mut job = vault.job(id).unwrap();
    job.begin();
    job.sleep_until(Timestamp::now() + SignedDuration::from_mins(10));
    job.add_update("Sam has not replied yet.", Timestamp::now(), true);
    vault.save_job(&job).unwrap();
    assert_eq!(vault.job(id).unwrap().status, JobStatus::Waiting);

    let stop = json!({ "id": id.to_string(), "reason": "they have signed" });
    let out = run(&ctx, "cancel_job", stop);
    assert_eq!(out["action"], "cancelled");
    assert_eq!(out["kind"], "watch");
    assert_eq!(out["status"], "cancelled");
    assert_eq!(out["what"], "The lease");
    assert_eq!(out["name"], "The lease");
    let note = out["note"].as_str().unwrap();
    assert!(note.contains("Stopped watching") && note.contains("will not report again"), "{note}");
    assert!(!note.contains("before it started"), "it had been looking for a while: {note}");

    let job = vault.job(id).unwrap();
    assert_eq!(job.status, JobStatus::Cancelled);
    assert_eq!(job.reason, "they have signed");
    assert!(job.finished_at.is_some());
    assert_eq!(job.updates.len(), 1, "what it had told them is kept");
    assert!(!job.owes_answer(), "a watch never comes back with an answer");
    assert!(vault.jobs(&JobQuery::waiting()).unwrap().is_empty(), "nothing for the loop");
    assert!(vault.jobs(&JobQuery::live()).unwrap().is_empty());

    // It cannot be steered once it is over, nor stopped twice.
    let why = refused(&ctx, "update_job", json!({ "id": id.to_string(), "look_now": true }));
    assert!(why.contains("already cancelled"), "{why}");
    let why = refused(&ctx, "cancel_job", json!({ "id": id.to_string() }));
    assert!(why.contains("already cancelled"), "{why}");
}

#[test]
fn cancelling_a_running_project_keeps_what_it_wrote_and_is_told_apart_from_one_not_started() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ctx = asking(&vault, ConversationId::new());
    let (project, _) = running_project(&vault);

    let out = run(&ctx, "cancel_job", json!({ "id": project.id.to_string() }));
    assert_eq!(out["kind"], "project");
    assert_eq!(out["goal"], "Plan a week in Lisbon");
    let note = out["note"].as_str().unwrap();
    assert!(note.contains("stop within moments") && note.contains("will not come back"), "{note}");
    assert_eq!(vault.job(project.id).unwrap().status, JobStatus::Cancelled);

    let queued = run(&ctx, "start_project", json!({ "goal": "Draft a proposal", "brief": "" }));
    let out = run(&ctx, "cancel_job", json!({ "id": queued["id"] }));
    assert!(out["note"].as_str().unwrap().contains("before it started"));
}

#[test]
fn a_call_to_cancel_names_a_watch_by_what_it_watches() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let started = run(&asking(&vault, ConversationId::new()), "start_watch", lease_watch());

    assert_eq!(
        tools::describe(&ctx(&vault), "cancel_job", &json!({ "id": started["id"] })).as_deref(),
        Some("Cancelled watch: The lease")
    );
}
