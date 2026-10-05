//! Proposals in conversation: seeing what a dream prepared, and answering it
//! when the person says so in chat.
//!
//! A [`Proposal`] is a finished record the assistant built on its own and did
//! not save — a task, a block of time, a memory, a routine, a note — or a
//! mail draft it wrote but did not send, or a deletion it wants, waiting
//! where the real thing would land: a dotted row at the foot of the todo
//! list, a ghost on the calendar grid. `list_proposals` and `get_proposal`
//! let the assistant read that list, and the record behind any one of them,
//! so it can tell the person what is waiting and what it would actually do —
//! `docs/plans/dreaming.md` is the whole design.
//!
//! `accept_proposal` and `decline_proposal` let the person answer from chat
//! instead of opening the card itself: "yes, do that" is a sentence, and the
//! assistant already heard it. Accepting is narrow on purpose. Only a
//! `Create` or a `Replace` goes through here — exactly the two payload shapes
//! every other tool in this catalogue is already allowed to make without
//! asking, because a wrongly-made task or note can be fixed by another
//! write. A `Delete` or a `SendMail` is refused: not because the person
//! cannot accept one, but because the same reasoning that makes
//! [`Effect::Destructive`](super::Effect::Destructive) and
//! [`Effect::Outward`](super::Effect::Outward) insist on a card for every
//! other tool applies here too — a deletion or a message that is already
//! gone must not be one plausible misreading of a chat message away. There
//! is no accept-with-edits either: a proposal that is close but not right is
//! declined and remade correctly with the ordinary tools, rather than
//! patched through an argument schema nobody can see the result of before it
//! lands.
//!
//! Neither tool has a `build`, so [`dispatch_drafting`](super::dispatch_drafting)
//! refuses outright if a dream ever tries to call one of them — there is no
//! sensible proposal *about* answering a proposal, and that refusal is the
//! same one `create_routine` already gets for trying to make more of itself.
//! `accept_proposal` additionally checks [`ToolContext::unattended`](super::ToolContext::unattended)
//! itself, which an ordinary person-authored routine can still set true
//! without ever going through drafting mode (only a dream does that): the
//! person's own "yes" is the one thing a routine running with nobody
//! watching must not stand in for.
//!
//! # Domain
//!
//! A proposal has no [`RecordKind`](crate::record::RecordKind) of its own —
//! [`TryFrom<RecordKind> for Domain`](super::Domain) answers `Proposal` with
//! `Err(())` — so nothing forces these tools into a domain the way a record
//! kind usually would. [`Vault::supports_proposals`](crate::vault::Vault::supports_proposals)
//! is backed by the same optional store as [`Vault::supports_agent`](crate::vault::Vault::supports_agent)
//! and [`Vault::supports_routines`](crate::vault::Vault::supports_routines)
//! already are, in every backend that carries any of the three today, so
//! availability alone does not decide between [`Domain::Agent`](super::Domain::Agent)
//! and [`Domain::Routines`](super::Domain::Routines). This file picks `Agent`: a
//! proposal can come from a conversation's own "later" button
//! ([`ProposalSource::Conversation`]) as well as from a dream, so tying these
//! tools to `Routines` would hide them from a vault that has a rail but no
//! dreaming switched on. `AgentSettings::dreaming` and its proposal policy
//! already live behind `supports_agent`, which is the other half of the same
//! argument: nothing proposal-shaped can exist in a vault that cannot already
//! do everything else `Domain::Agent` gates.

use serde_json::{Value, json};

use super::{Args, Tool, ToolContext, done, limit_arg, one_of, schema, text};
use crate::error::{Error, Result};
use crate::id::ProposalId;
use crate::proposal::{
    DeclineReason, Outcome, Payload, Proposal, ProposalKind, ProposalSource, ProposalState,
};
use crate::store::proposals::ProposalQuery;

pub(super) static TOOLS: &[Tool] = &[
    tool!(
        "list_proposals",
        Read,
        Agent,
        schema(
            vec![
                (
                    "kind",
                    one_of(
                        "Only proposals that would create or change this kind of record.",
                        &kind_names(),
                    )
                ),
                (
                    "status",
                    one_of(
                        "Which to include: pending (the default) is still waiting for an \
                         answer, answered is accepted, declined or expired, and all is every \
                         proposal there is.",
                        &["pending", "answered", "all"],
                    )
                ),
                limit_arg(),
            ],
            &[]
        ),
        "Work a dream prepared and left for them to answer: a task, a block of time, a \
         memory, a routine, a note, or a mail draft it wrote but did not save or send. \
         Pending by default \u{2014} reach for this on \"what are you waiting on me for\" or \
         \"what have you been up to\". Each row is a caption and why, not the finished \
         record; call get_proposal before telling them what one would actually contain.",
        run_list_proposals
    ),
    tool!(
        "get_proposal",
        Read,
        Agent,
        schema(vec![("proposal_id", text("Id from list_proposals."))], &["proposal_id"]),
        "One proposal in full: why it was made, when it expires, and the whole record it \
         would save or change \u{2014} so you can say what a proposed note, task or routine \
         would actually contain before anyone accepts it. For a deletion or a mail send, what \
         it would act on rather than a record of its own.",
        run_get_proposal
    ),
    tool!(
        "accept_proposal",
        Write,
        Agent,
        schema(vec![("proposal_id", text("Id from list_proposals."))], &["proposal_id"]),
        "Say yes to a proposal on the person's behalf, the way tapping its card would \u{2014} \
         use this when they tell you in chat to go ahead with something a dream prepared. Only \
         a proposal that would create or change a record can be accepted here: a task, a \
         block of time, a memory, a routine or a note. One that would delete something or \
         send mail is refused \u{2014} those are answered from their own card, never from a \
         sentence in chat, because a misheard \"yes\" must not be the only thing standing \
         between somebody and a deletion or a message that is already gone. There is no way \
         to accept with changes: if it is close but not right, decline it and make what they \
         actually want with the ordinary tools instead. Refused during a scheduled run, since \
         answering for the person is exactly what a routine with nobody watching must not do.",
        run_accept_proposal
    ),
    tool!(
        "decline_proposal",
        Write,
        Agent,
        schema(
            vec![
                ("proposal_id", text("Id from list_proposals.")),
                (
                    "reason",
                    one_of(
                        "Why, only if they said so \u{2014} omit rather than guess.",
                        &["not_now", "wrong_time", "never_this", "other"],
                    )
                ),
                ("note", text("One line, only with reason \"other\": what they actually said.")),
            ],
            &["proposal_id"]
        ),
        "Say no to a pending proposal. Leaves everything exactly as it was \u{2014} nothing is \
         created, changed, sent or deleted \u{2014} so declining has nothing of its own to \
         undo later.",
        run_decline_proposal
    ),
];

/// Every stored spelling of [`ProposalKind`], for the `kind` filter's schema
/// and its error message. Built from [`ProposalKind::ALL`] rather than
/// written out a second time, so a kind added there — `skill`, soon — is
/// offered and explained here without this file changing.
fn kind_names() -> Vec<&'static str> {
    ProposalKind::ALL.iter().map(|k| k.as_str()).collect()
}

/// The `kind` argument, read and checked. `None` when it was left unsaid,
/// which [`run_list_proposals`] reads as "every kind".
fn parse_kind_filter(args: &Args<'_>) -> Result<Option<ProposalKind>> {
    let Some(raw) = args.opt_str("kind") else { return Ok(None) };
    ProposalKind::parse(raw).map(Some).ok_or_else(|| {
        args.bad(format!("`kind` must be one of {}, got {raw:?}", kind_names().join(", ")))
    })
}

/// The `status` argument, read and checked, as the [`ProposalState`]s
/// [`ProposalQuery::states`] wants. Defaults to pending, which is the
/// question this tool is for: "what is waiting on me", not "what has
/// happened".
fn parse_status_filter(args: &Args<'_>) -> Result<Vec<ProposalState>> {
    let raw = args.opt_str("status").unwrap_or("pending");
    match raw.trim().to_lowercase().as_str() {
        "pending" => Ok(vec![ProposalState::Pending]),
        "answered" => {
            Ok(vec![ProposalState::Accepted, ProposalState::Declined, ProposalState::Expired])
        }
        // Empty means any state at all -- see `ProposalQuery::states`.
        "all" => Ok(Vec::new()),
        _ => Err(args.bad(format!("`status` must be one of pending, answered, all, got {raw:?}"))),
    }
}

fn run_list_proposals(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let kind = parse_kind_filter(args)?;
    let query = ProposalQuery {
        kinds: kind.into_iter().collect(),
        states: parse_status_filter(args)?,
        limit: Some(args.limit()),
        ..Default::default()
    };
    let proposals = ctx.vault.proposals(&query)?;
    Ok(json!({
        "count": proposals.len(),
        "proposals": proposals.iter().map(|p| proposal_summary(ctx, p)).collect::<Vec<_>>(),
    }))
}

/// Whether a payload would create, change, delete or send something — the
/// one word every proposal's row leads with, whatever kind of record it
/// carries.
fn does(payload: &Payload) -> &'static str {
    match payload {
        Payload::Create { .. } => "create",
        Payload::Replace { .. } => "change",
        Payload::Delete { .. } => "delete",
        Payload::SendMail { .. } => "send",
    }
}

/// Who proposed this, in words rather than an id: the dream's own name if
/// the run that made it can still be found, or the conversation it was
/// parked from. `None` for a proposal with nothing recorded either way,
/// which does not happen today but is not this function's problem to
/// enforce.
fn made_by_label(ctx: &ToolContext<'_>, source: Option<ProposalSource>) -> Option<String> {
    match source? {
        ProposalSource::Run { run_id } => {
            Some(ctx.vault.run(run_id).map(|r| r.routine_name).unwrap_or_else(|_| "a dream".into()))
        }
        // Parked from the rail's own "later" button -- see
        // `docs/plans/dreaming.md`'s Phase 5 -- rather than a run.
        ProposalSource::Conversation { .. } => Some("a conversation".into()),
    }
}

/// [`Outcome`] as a compact object, or `None` while it is still pending —
/// which both [`proposal_summary`] and [`run_get_proposal`] read as "leave
/// the field out", since an unanswered proposal has no outcome worth
/// showing yet.
fn outcome_json(outcome: &Outcome) -> Option<Value> {
    match outcome {
        Outcome::Pending => None,
        Outcome::Accepted { at, saved_as, edited } => Some(json!({
            "state": "accepted",
            "at": at.to_string(),
            "saved_as": saved_as,
            "edited": edited,
        })),
        Outcome::Declined { at, reason } => Some(json!({
            "state": "declined",
            "at": at.to_string(),
            "reason": reason,
        })),
        Outcome::Expired { at } => Some(json!({
            "state": "expired",
            "at": at.to_string(),
        })),
    }
}

/// The compact row [`run_list_proposals`] shows for one proposal: enough to
/// say what it is, why it is there and whether it still needs an answer,
/// without the full record [`run_get_proposal`] carries.
fn proposal_summary(ctx: &ToolContext<'_>, p: &Proposal) -> Value {
    let mut v = json!({
        "id": p.id.to_string(),
        "kind": p.kind.as_str(),
        "caption": p.caption,
        "does": does(&p.payload),
        "made_at": p.made_at.to_string(),
    });
    let map = v.as_object_mut().expect("built as an object");
    if !p.why.trim().is_empty() {
        map.insert("why".into(), json!(p.why));
    }
    if let Some(label) = made_by_label(ctx, p.made_by) {
        map.insert("made_by".into(), json!(label));
    }
    if let Some(day) = p.target_date {
        map.insert("target_date".into(), json!(day.to_string()));
    }
    if let Some(outcome) = outcome_json(&p.outcome) {
        map.insert("outcome".into(), outcome);
    }
    v
}

/// The record or reference a payload carries, as JSON, for
/// [`run_get_proposal`]. `record` is the finished [`crate::proposal::ProposedRecord`]
/// a `Create` or `Replace` would save, serialised rather than rebuilt field
/// by field, so a kind this file has never heard of — `skill`, once it
/// exists — reads here the moment it reads anywhere else.
fn payload_json(payload: &Payload) -> Result<Value> {
    Ok(match payload {
        Payload::Create { record } => json!({ "record": serde_json::to_value(record)? }),
        Payload::Replace { record, expected_updated_at } => json!({
            "record": serde_json::to_value(record)?,
            // What it was checked against: a record changed since is a
            // stale `Replace`, surfaced as a plain refusal by
            // `accept_proposal` rather than by anything reading this field.
            "current_as_of": expected_updated_at.to_string(),
        }),
        Payload::Delete { kind, id } => json!({ "kind": kind.as_str(), "id": id }),
        Payload::SendMail { draft_id } => json!({ "draft_id": draft_id.to_string() }),
    })
}

fn run_get_proposal(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: ProposalId = args.id("proposal_id", "proposal")?;
    let p = ctx.vault.proposal(id)?;
    let mut v = proposal_summary(ctx, &p);
    let map = v.as_object_mut().expect("built as an object");
    map.insert("expires_at".into(), json!(p.expires_at.to_string()));
    map.insert("payload".into(), payload_json(&p.payload)?);
    Ok(v)
}

/// The two payload shapes [`run_accept_proposal`] will not act on, whatever
/// the person said. A `Create` or a `Replace` is let through: both are as
/// recoverable as any other write this catalogue already makes without
/// asking, by another write. A `Delete` and a `SendMail` are refused in
/// words the model can relay, because each is one plausible misreading of a
/// chat message away from losing or sending something for good, and the
/// card they were proposed on is already the place that asks properly, with
/// the record on screen rather than paraphrased.
fn refuse_if_not_acceptable_from_chat(payload: &Payload) -> Result<()> {
    let verb = match payload {
        Payload::Create { .. } | Payload::Replace { .. } => return Ok(()),
        Payload::Delete { .. } => "delete something",
        Payload::SendMail { .. } => "send a message",
    };
    Err(Error::Invalid(format!(
        "a proposal that would {verb} is answered from its own card, not from chat. Tell them \
         to open it there and accept or decline it directly."
    )))
}

fn run_accept_proposal(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    // The one refusal `dispatch_drafting` cannot make for us: this tool has
    // no `build`, which stops a *dream* from calling it, but an ordinary
    // routine runs unattended without ever going through drafting mode at
    // all. The person's own "yes" is not a routine's to give.
    if ctx.unattended {
        return Err(Error::Invalid(
            "a scheduled run may not accept a proposal; that is the person's own decision, \
             made when somebody is actually there to make it."
                .into(),
        ));
    }
    let id: ProposalId = args.id("proposal_id", "proposal")?;
    let proposal = ctx.vault.proposal(id)?;
    refuse_if_not_acceptable_from_chat(&proposal.payload)?;

    let accepted = ctx.vault.accept_proposal(id, None, false, jiff::Timestamp::now())?;
    let saved_as = match &accepted.outcome {
        Outcome::Accepted { saved_as, .. } => saved_as.clone(),
        // `Vault::accept_proposal` only returns `Ok` with this outcome --
        // any other result comes back as `Err` and never reaches here. This
        // arm exists so the match stays total rather than panicking if that
        // ever stops being true.
        _ => accepted.id.to_string(),
    };
    done("accepted", accepted.kind.as_str(), &accepted.caption, saved_as)
}

/// The `reason` and `note` arguments, read into a [`DeclineReason`]. `None`
/// when no reason was given, which [`run_decline_proposal`] passes straight
/// through -- declining without a reason is one tap in the interface and
/// stays one call here.
fn parse_decline_reason(args: &Args<'_>) -> Result<Option<DeclineReason>> {
    let Some(raw) = args.opt_str("reason") else { return Ok(None) };
    Ok(Some(match raw.trim().to_lowercase().as_str() {
        "not_now" => DeclineReason::NotNow,
        "wrong_time" => DeclineReason::WrongTime,
        "never_this" => DeclineReason::NeverThis,
        "other" => DeclineReason::Other {
            text: args.opt_str("note").unwrap_or_default().trim().to_string(),
        },
        _ => {
            return Err(args.bad(format!(
                "`reason` must be one of not_now, wrong_time, never_this, other, got {raw:?}"
            )));
        }
    }))
}

fn run_decline_proposal(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: ProposalId = args.id("proposal_id", "proposal")?;
    let reason = parse_decline_reason(args)?;
    let proposal = ctx.vault.decline_proposal(id, reason, jiff::Timestamp::now())?;
    done("declined", proposal.kind.as_str(), &proposal.caption, proposal.id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: Value) -> (&'static str, Value) {
        ("test_tool", v)
    }

    // ---- kind -------------------------------------------------------------

    #[test]
    fn every_stored_kind_spelling_parses_back_to_itself() {
        for k in ProposalKind::ALL {
            let (name, v) = args(json!({ "kind": k.as_str() }));
            assert_eq!(parse_kind_filter(&Args::new(name, &v)).unwrap(), Some(k));
        }
    }

    #[test]
    fn an_absent_kind_filters_nothing() {
        let (name, v) = args(json!({}));
        assert_eq!(parse_kind_filter(&Args::new(name, &v)).unwrap(), None);
    }

    #[test]
    fn an_unknown_kind_lists_the_real_ones_and_quotes_what_it_got() {
        let (name, v) = args(json!({ "kind": "event" }));
        let err = parse_kind_filter(&Args::new(name, &v)).unwrap_err().to_string();
        assert!(err.contains("task"), "should list the real kinds: {err}");
        assert!(err.contains("\"event\""), "should quote what it got: {err}");
    }

    // ---- status -------------------------------------------------------------

    #[test]
    fn status_defaults_to_pending() {
        let (name, v) = args(json!({}));
        assert_eq!(
            parse_status_filter(&Args::new(name, &v)).unwrap(),
            vec![ProposalState::Pending]
        );
    }

    #[test]
    fn answered_covers_every_state_but_pending() {
        let (name, v) = args(json!({ "status": "Answered" }));
        assert_eq!(
            parse_status_filter(&Args::new(name, &v)).unwrap(),
            vec![ProposalState::Accepted, ProposalState::Declined, ProposalState::Expired],
        );
    }

    #[test]
    fn all_means_no_filter_at_all() {
        let (name, v) = args(json!({ "status": "all" }));
        assert_eq!(parse_status_filter(&Args::new(name, &v)).unwrap(), Vec::new());
    }

    #[test]
    fn an_unknown_status_is_refused() {
        let (name, v) = args(json!({ "status": "overdue" }));
        let err = parse_status_filter(&Args::new(name, &v)).unwrap_err().to_string();
        assert!(err.contains("pending, answered, all"), "got {err}");
    }

    // ---- decline reason -----------------------------------------------------

    #[test]
    fn no_reason_given_is_none() {
        let (name, v) = args(json!({}));
        assert_eq!(parse_decline_reason(&Args::new(name, &v)).unwrap(), None);
    }

    #[test]
    fn a_preset_reason_is_case_insensitive() {
        let (name, v) = args(json!({ "reason": "Wrong_Time" }));
        assert_eq!(
            parse_decline_reason(&Args::new(name, &v)).unwrap(),
            Some(DeclineReason::WrongTime)
        );
    }

    #[test]
    fn other_carries_the_note_trimmed() {
        let (name, v) = args(json!({ "reason": "other", "note": "  it's already done  " }));
        assert_eq!(
            parse_decline_reason(&Args::new(name, &v)).unwrap(),
            Some(DeclineReason::Other { text: "it's already done".into() })
        );
    }

    #[test]
    fn an_unknown_reason_is_refused() {
        let (name, v) = args(json!({ "reason": "boredom" }));
        let err = parse_decline_reason(&Args::new(name, &v)).unwrap_err().to_string();
        assert!(err.contains("not_now, wrong_time, never_this, other"), "got {err}");
    }

    // ---- accepting from chat -------------------------------------------------

    #[test]
    fn create_and_replace_payloads_may_be_accepted_from_chat() {
        use crate::task::Task;

        refuse_if_not_acceptable_from_chat(&Payload::Create {
            record: crate::proposal::ProposedRecord::Task(Task::new("Book the dentist")),
        })
        .unwrap();
        refuse_if_not_acceptable_from_chat(&Payload::Replace {
            record: crate::proposal::ProposedRecord::Task(Task::new("Book the dentist")),
            expected_updated_at: jiff::Timestamp::now(),
        })
        .unwrap();
    }

    #[test]
    fn a_deletion_is_refused_from_chat() {
        let err = refuse_if_not_acceptable_from_chat(&Payload::Delete {
            kind: ProposalKind::Task,
            id: "x".into(),
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("own card"), "got {err}");
    }

    #[test]
    fn a_mail_send_is_refused_from_chat() {
        let err = refuse_if_not_acceptable_from_chat(&Payload::SendMail {
            draft_id: crate::id::DraftId::new(),
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("own card"), "got {err}");
    }
}
