//! Task proposals from a meeting note's next steps.
//!
//! Run after the note is committed, and a failure here is logged rather
//! than returned: the note already exists, and undoing it because a
//! proposal could not be saved would orphan the meeting. A step is proposed
//! only when it is the owner's. The decision model is asked one step at a
//! time, with the line, the meeting's title and the owner's name -- not the
//! transcript. No model, a refusal, or an unsure answer falls back to the
//! name. A sure "not mine" is dropped even when the name matches.
//!
//! A step that already has a pending proposal, or an open task, with the
//! same title for this note is not proposed again, so a retry of the
//! pipeline does not fill the list twice. The pending cap stops the rest
//! rather than failing the note.

use std::sync::Arc;

use jiff::civil::Date;
use karmayoga_core::meeting::people::Owner;
use karmayoga_core::meeting::steps::{self, ModelSay, NextStep};
use karmayoga_core::proposal::{About, AboutKind, Payload, Proposal, ProposalKind, ProposedRecord};
use karmayoga_core::store::proposals::ProposalQuery;
use karmayoga_core::store::tasks::TaskQuery;
use karmayoga_core::{NoteId, ProposalState, Task, Vault};
use serde_json::{Map, json};

use crate::decision::{self, ChoiceOption, Content};
use crate::error::CommandResult;
use crate::events::{Change, EventSink, Kind, Op};
use crate::service::blocking;

use super::stages::owner_of;

const MINE: &str = "mine";
const NOT_MINE: &str = "not mine";

const INSTRUCTIONS: &str = "\
The step is a line from the \"Next steps\" of the person's meeting notes, \
written as \"**Owner:** what they will do\". Decide whether the person given \
under person is the one who will do it. Mine: the owner is them -- their \
name, an email address of theirs, or \"You\" or \"Me\" -- or it is theirs \
together with others (\"Hari and Priya\"). Not mine: it names somebody else, \
even somebody who shares their first name, a team or group that does not name \
them, or nobody (\"Unassigned\").";

/// Propose the owner's next steps from `body`. `on` dates a step that names
/// a month and a day without a year. `others` are the other people invited,
/// so a shared first name is not treated as the owner's.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn from_note(
    vault: &Arc<Vault>,
    note_id: NoteId,
    body: &str,
    meeting: &str,
    others: &[String],
    on: Date,
    tz: &str,
    events: &dyn EventSink,
) {
    let steps = steps::next_steps(body, on);
    if steps.is_empty() {
        return;
    }
    let prepared = {
        let vault = vault.clone();
        match blocking(move || prepare(&vault)).await {
            Ok(Some(prepared)) => prepared,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(error = %e, "meeting pipeline: could not prepare task proposals");
                return;
            }
        }
    };
    let mut made = 0u32;
    for step in steps {
        if steps::is_unassigned(&step.owner) {
            continue;
        }
        let said = ask(&prepared.access, &prepared.person, meeting, &step).await;
        if !steps::keep(said, steps::assigned_to_owner(&step.owner, &prepared.owner, others)) {
            continue;
        }
        if prepared.already.iter().any(|have| have == &fold(&step.action)) {
            continue;
        }
        match save(vault, note_id, meeting, tz, &step, events).await {
            Save::Wrote => made += 1,
            Save::Full | Save::Failed => break,
        }
    }
    if made > 0 {
        tracing::info!(meeting, proposals = made, "meeting pipeline: proposed tasks from the note");
    }
}

struct Prepared {
    owner: Owner,
    /// The person, as the decision model is told about them.
    person: String,
    access: Option<karmayoga_core::llm::DecisionAccess>,
    /// Folded titles already proposed for this note, or open as tasks.
    already: Vec<String>,
}

fn prepare(vault: &Vault) -> CommandResult<Option<Prepared>> {
    if !vault.supports_proposals() || !vault.supports_tasks() {
        return Ok(None);
    }
    if !vault.agent_settings().unwrap_or_default().proposals.allows(ProposalKind::Task) {
        return Ok(None);
    }
    let owner = owner_of(vault);
    let person = person_of(&owner);
    let access = vault.decision_access().ok();
    let mut already = Vec::new();
    let pending = vault.proposals(&ProposalQuery {
        kinds: vec![ProposalKind::Task],
        states: vec![ProposalState::Pending],
        ..ProposalQuery::default()
    })?;
    for proposal in pending {
        if let Some(title) = proposal.payload.record().and_then(|r| match r {
            ProposedRecord::Task(task) => Some(task.title.as_str()),
            _ => None,
        }) {
            already.push(fold(title));
        }
    }
    for task in vault.tasks(&TaskQuery::open())? {
        already.push(fold(&task.title));
    }
    Ok(Some(Prepared { owner, person, access, already }))
}

fn person_of(owner: &Owner) -> String {
    let name = owner.name.trim();
    let name = if name.is_empty() { "the owner of these notes" } else { name };
    if owner.addresses.is_empty() {
        name.to_string()
    } else {
        let addresses: Vec<&str> = owner.addresses.iter().map(String::as_str).collect();
        format!("{name} ({})", addresses.join(", "))
    }
}

async fn ask(
    access: &Option<karmayoga_core::llm::DecisionAccess>,
    person: &str,
    meeting: &str,
    step: &NextStep,
) -> ModelSay {
    let Some(access) = access else { return ModelSay::Unsure };
    let line = format!("**{}:** {}", step.owner, step.action);
    let mut fields = Map::new();
    fields.insert("meeting".into(), json!(meeting));
    fields.insert("person".into(), json!(person));
    fields.insert("step".into(), json!(line));
    let content = Content {
        text: format!("Meeting: {meeting}\nPerson: {person}\nNext step: {line}"),
        fields,
    };
    let options = [
        ChoiceOption {
            name: MINE.into(),
            description: "The person will do it, alone or with others.".into(),
        },
        ChoiceOption {
            name: NOT_MINE.into(),
            description: "Somebody else will do it, or nobody took it on.".into(),
        },
    ];
    match decision::choose(access, &content, INSTRUCTIONS, &options).await {
        Ok(choice) => steps::model_says(&choice.name, choice.probability),
        Err(e) => {
            tracing::debug!(error = %e, "meeting pipeline: the decision model did not say whose step this is");
            ModelSay::Unsure
        }
    }
}

enum Save {
    Wrote,
    Full,
    Failed,
}

async fn save(
    vault: &Arc<Vault>,
    note_id: NoteId,
    meeting: &str,
    tz: &str,
    step: &NextStep,
    events: &dyn EventSink,
) -> Save {
    let mut task = Task::new(clip(&step.action, karmayoga_core::proposal::MAX_CAPTION_CHARS));
    task.due_date = step.due;
    let caption = task.title.clone();
    let why = clip(&format!("From {meeting}"), karmayoga_core::proposal::MAX_WHY_CHARS);
    let proposal = Proposal::new(
        Payload::Create { record: ProposedRecord::Task(task) },
        caption,
        jiff::Timestamp::now(),
        tz,
    )
    .with_why(why)
    .with_about(Some(About { kind: AboutKind::Note, id: note_id.to_string() }));
    let id = proposal.id;
    let vault = vault.clone();
    let saved = blocking(move || match vault.save_proposal(&proposal) {
        Ok(()) => Ok(Save::Wrote),
        Err(karmayoga_core::Error::Invalid(reason)) => {
            tracing::debug!(%reason, "meeting pipeline: stopped proposing tasks");
            Ok(Save::Full)
        }
        Err(e) => {
            tracing::warn!(error = %e, "meeting pipeline: a task proposal was not saved");
            Ok(Save::Failed)
        }
    })
    .await;
    match saved {
        Ok(Save::Wrote) => {
            let mut change = Change::new(Kind::Proposal, Op::Created);
            change.id = Some(id.to_string());
            events.changed(change);
            Save::Wrote
        }
        Ok(other) => other,
        Err(e) => {
            tracing::warn!(error = %e, "meeting pipeline: a task proposal was not saved");
            Save::Failed
        }
    }
}

fn clip(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    trimmed.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

fn fold(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use karmayoga_core::Profile;

    fn vault() -> (tempfile::TempDir, Arc<Vault>) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = karmayoga_core::VaultConfig {
            password: Some("correct horse battery".into()),
            kdf: karmayoga_core::crypto::KdfParams::insecure_fast(),
            ..Default::default()
        };
        let vault = karmayoga_vault::create(dir.path(), cfg).unwrap();
        (dir, Arc::new(vault))
    }

    #[tokio::test]
    async fn only_the_owners_steps_are_proposed_and_a_second_pass_adds_nothing() {
        let (_dir, vault) = vault();
        let profile = Profile {
            first_name: "Hari".into(),
            last_name: "Govardhanam".into(),
            ..Default::default()
        };
        vault.save_profile(&profile).unwrap();
        let note = NoteId::new();
        let body = "\
## Next steps
- **Hari:** send the draft, by 12 May 2026
- **Priya:** review it, by Friday
- **Unassigned:** book the room
";
        let others = vec!["Priya Raman".to_string()];
        let events = crate::events::Silent;
        from_note(
            &vault,
            note,
            body,
            "Design sync",
            &others,
            Date::new(2026, 5, 4).unwrap(),
            "UTC",
            &events,
        )
        .await;
        let pending = vault.proposals(&ProposalQuery::pending()).unwrap();
        assert_eq!(pending.len(), 1, "{pending:?}");
        let task = match &pending[0].payload {
            Payload::Create { record: ProposedRecord::Task(task) } => task,
            other => panic!("expected a task, got {other:?}"),
        };
        assert_eq!(task.title, "send the draft");
        assert_eq!(task.due_date, Some(Date::new(2026, 5, 12).unwrap()));
        assert_eq!(pending[0].why, "From Design sync");
        assert_eq!(
            pending[0].about.as_ref().map(|a| a.id.as_str()),
            Some(note.to_string().as_str())
        );

        from_note(
            &vault,
            note,
            body,
            "Design sync",
            &others,
            Date::new(2026, 5, 4).unwrap(),
            "UTC",
            &events,
        )
        .await;
        assert_eq!(vault.proposals(&ProposalQuery::pending()).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn with_no_name_in_the_profile_the_notes_you_lines_are_proposed() {
        let (_dir, vault) = vault();
        // No name typed: the microphone's lines, and so the notes, say "You".
        let note = NoteId::new();
        let body = "\
## Next steps
- **You:** Send Acme the revised pricing deck, by Fri 8 May
- **Priya:** Review the pricing deck, by Mon 11 May
- **Unassigned (offered by someone on the call):** Book the room
";
        let others = vec!["Priya Raman".to_string()];
        let events = crate::events::Silent;
        from_note(
            &vault,
            note,
            body,
            "Design sync",
            &others,
            Date::new(2026, 5, 4).unwrap(),
            "UTC",
            &events,
        )
        .await;
        let pending = vault.proposals(&ProposalQuery::pending()).unwrap();
        assert_eq!(pending.len(), 1, "{pending:?}");
        let task = match &pending[0].payload {
            Payload::Create { record: ProposedRecord::Task(task) } => task,
            other => panic!("expected a task, got {other:?}"),
        };
        assert_eq!(task.title, "Send Acme the revised pricing deck");
        assert_eq!(task.due_date, Some(Date::new(2026, 5, 8).unwrap()), "a weekday and a date");
    }

    #[test]
    fn the_decision_models_instructions_guard_a_shared_first_name() {
        assert!(INSTRUCTIONS.contains("even somebody who shares their first name"));
        assert!(INSTRUCTIONS.contains("\"You\" or \"Me\""));
    }
}
