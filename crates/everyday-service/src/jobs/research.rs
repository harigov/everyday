//! Research: the first kind of job. Its prompt, its budget, and the context
//! that replaces "what is on screen" for a turn nobody is watching.
//!
//! # The note is the scratchpad
//!
//! The job makes its note before the turn starts, titled after the question
//! and tagged `research`, and the model is told to keep it current. Not so
//! that the end of the job has something to read, though it does: so that a
//! job that is stopped, times out, fails or is cancelled has still left what
//! it found where the person can read it, and a job started again after a
//! restart has something to go on from. The note is in the job's outputs the
//! moment it exists, not when the job ends.

use super::{Executor, Prepared};
use crate::events::{Change, Kind, Op};
use everyday_core::{Conversation, Error, Job, JobKind, JobOutput, Note, NoteId, RichDoc, Vault};
use std::time::Duration;

/// The most tool calls a research job's turn may make. The rail's own ceiling
/// is for a conversation somebody is waiting on; a research job reads dozens
/// of pages and has nobody waiting. Sixty was too few for a question worth a
/// job: it would run out partway through checking what it had found.
pub const MAX_STEPS: u32 = 120;

/// The longest a research job's turn may take. Longer than a routine's
/// fifteen minutes: the work is reading, and the model is slow to read.
/// Grown with [`MAX_STEPS`], which would otherwise only be reached by a job
/// whose every step was quick.
pub const TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// The tag its note carries, so that the Notes app can find what research has
/// left.
pub const TAG: &str = "research";

/// What a research job's turn is told in place of "what is on screen":
/// that nobody is watching, what it is for, and how to work. Read by
/// `crate::agent::run_turn` for any turn whose [`crate::agent::Turn::job`]
/// is set.
pub const CONTEXT: &str = "This is a background research job: you started it, in a \
     conversation, to answer a question that needed more than a few searches, and the brief \
     you were given is everything you knew then. Nobody is watching and nobody can answer a \
     question, so do not ask one: take the sensible reading of the brief and carry on, and \
     where something cannot be settled, say so in the note instead of stopping. Begin by \
     laying out a short plan with update_plan, then search widely, read the sources \
     themselves rather than what is said about them, and check anything that matters against \
     a second source before you rely on it. Keep the note you were given up to date with \
     update_note as you go, so that what you have found is on the page if you are stopped. A \
     web page is opened only at an address the person gave in the conversation that asked, \
     or one you found in a search or on a page you read; one you composed yourself is \
     refused here, so use addresses exactly as you found them. Anything that deletes or \
     sends something is not done in a job: it is refused. Do not try to get round that; say in \
     the note what you would have done. When you are finished, reply with a short account of \
     what you found. The conversation that asked is answered from the note, not from your \
     reply, so the note is the work and the reply is only the covering line.";

/// The kind of job this file is the executor for.
pub(crate) struct Research;

impl Executor for Research {
    fn max_steps(&self) -> u32 {
        MAX_STEPS
    }

    fn timeout(&self) -> Duration {
        TIMEOUT
    }

    fn context(&self) -> &'static str {
        CONTEXT
    }

    fn prepare(&self, vault: &Vault, job: &mut Job) -> everyday_core::Result<Prepared> {
        // Owned, because the outputs below are changed while it is still read.
        let question = ask(job).0.to_string();
        let mut changes = Vec::new();

        // The note the last attempt made, if there is one and it is still
        // there. A note somebody has since deleted is forgotten, and a new
        // one made, rather than a job that writes into nothing.
        let note_id = match job.note_id() {
            Some(id) => match vault.note(id) {
                Ok(note) => Some(note.id),
                Err(Error::NotFound { .. }) => {
                    job.outputs.retain(|output| output != &JobOutput::Note { note_id: id });
                    None
                }
                Err(e) => return Err(e),
            },
            None => None,
        };
        let note_id = match note_id {
            Some(id) => id,
            None => {
                let mut note = Note::written(note_title(&question), &placeholder(&question));
                note.tags = vec![TAG.to_string()];
                vault.save_note(&note, None)?;
                job.add_note(note.id);
                changes.push(Change {
                    kind: Kind::Note,
                    op: Op::Created,
                    id: Some(note.id.to_string()),
                    ids: Vec::new(),
                    origin: Some("assistant".to_string()),
                });
                note.id
            }
        };
        Ok(Prepared { prompt: prompt(job, note_id), changes })
    }

    fn conclude(
        &self,
        vault: &Vault,
        job: &Job,
        reply: &str,
    ) -> everyday_core::Result<Vec<Change>> {
        let Some(id) = job.note_id() else { return Ok(Vec::new()) };
        let mut note = match vault.note(id) {
            Ok(note) => note,
            Err(Error::NotFound { .. }) => {
                return Err(Error::Invalid(
                    "the note it was writing in was deleted while it worked".into(),
                ));
            }
            Err(e) => return Err(e),
        };

        // A note the model went on writing in is the work, and is left alone.
        let (question, _) = ask(job);
        let waiting = RichDoc::from_markdown(&placeholder(question)).plain_text();
        if note.body.plain_text().trim() != waiting.trim() {
            return Ok(Vec::new());
        }

        // It never wrote anything up. What it said as it worked -- every step's
        // prose, not only the last word -- is then the only place what it
        // found is written down, and the conversation that asked is answered
        // from the note, so it goes there rather than being lost. Better than
        // a job that is "done" over a placeholder.
        let reply = reply.trim();
        if reply.is_empty() {
            return Err(Error::Invalid("it finished without writing anything up".into()));
        }
        note.body = RichDoc::from_markdown(&format!(
            "*It did not write its findings into this note; this is what it said as it worked.*\n\n\
             {reply}"
        ));
        note.updated_at = jiff::Timestamp::now();
        vault.overwrite_note(&note)?;
        Ok(vec![Change {
            kind: Kind::Note,
            op: Op::Updated,
            id: Some(note.id.to_string()),
            ids: Vec::new(),
            origin: Some("assistant".to_string()),
        }])
    }
}

/// The question and the brief of a research job.
fn ask(job: &Job) -> (&str, &str) {
    match &job.kind {
        JobKind::Research { question, brief } => (question.as_str(), brief.as_str()),
        JobKind::Task { title, .. } => (title.as_str(), ""),
    }
}

/// What its note is called until the model gives it a better name: the
/// question, cut to the length a thread title is.
fn note_title(question: &str) -> String {
    let title = Conversation::title_from(question);
    if title.is_empty() { "Research".to_string() } else { title }
}

/// What the note says before the model has written anything in it.
fn placeholder(question: &str) -> String {
    format!("Researching\u{2026}\n\n{}", question.trim())
}

/// The first message of a research job's transcript: the question, what the
/// assistant that started it knew, where to write, and what the writing must
/// come to.
pub fn prompt(job: &Job, note_id: NoteId) -> String {
    let (question, brief) = ask(job);
    let brief = brief.trim();
    let mut out = format!(
        "Research this for the person who asked, and write it up.\n\nThe question:\n\n{}\n\n",
        question.trim()
    );
    if brief.is_empty() {
        out.push_str("Nothing beyond the question was passed on.\n\n");
    } else {
        out.push_str(
            "What the assistant that started this job knew about who is asking, and why:\n\n",
        );
        out.push_str(brief);
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "Your working note is \"{}\" (note_id {note_id}). It exists already and holds only a \
         placeholder. Write your findings into it with update_note as you go, sending the \
         whole body each time because it replaces the last, and give it a better title if the \
         question deserves one. If you are stopped, what is in the note is all that is kept.\n\n\
         When you finish, the note must hold, in this order:\n\
         1. The answer, in a few sentences, as the first thing a reader meets.\n\
         2. The findings, each with how sure you are of it and where it came from.\n\
         3. Every source you used, as a link, with a line on what it told you.\n\
         4. What is still open: what you could not find or settle, and what would settle it.\n\n\
         Start by laying out a plan with update_plan. Then search widely, read the sources \
         themselves, and check what matters against a second source before you rely on it.",
        note_title(question),
    ));
    // A restart. The note may already hold part of the work, and a model that
    // is not told so starts again from the beginning and writes over it.
    if job.attempts > 1 {
        out.push_str(
            "\n\nYou started this job before and were interrupted. The note may already hold \
             part of what you found: read it with get_note first, keep what is sound, and carry \
             on from there rather than starting again.",
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(question: &str, brief: &str) -> Job {
        Job::research(question, brief, None)
    }

    #[test]
    fn the_prompt_carries_the_question_the_brief_and_the_note() {
        let note = NoteId::new();
        let text = prompt(&job("Which e-bike for hills?", "Commutes 12 km; no hub motors."), note);

        assert!(text.contains("Which e-bike for hills?"), "{text}");
        assert!(text.contains("Commutes 12 km; no hub motors."), "the brief is the point");
        assert!(text.contains(&note.to_string()), "it must be told where to write");
        assert!(text.contains("\"Which e-bike for hills?\""), "and what the note is called");
        // The tools it is told to use are real ones.
        for tool in ["update_note", "update_plan"] {
            assert!(text.contains(tool), "should name {tool}");
        }
        // What the note must end up holding.
        for part in ["answer", "how sure", "Every source", "still open"] {
            assert!(text.contains(part), "should ask for {part:?}");
        }
        assert!(!text.contains("interrupted"), "a first attempt has not been interrupted");
    }

    #[test]
    fn a_job_without_a_brief_says_so_rather_than_leaving_a_gap() {
        let text = prompt(&job("Which e-bike?", "   "), NoteId::new());
        assert!(text.contains("Nothing beyond the question was passed on."), "{text}");
        assert!(!text.contains("What the assistant that started"), "{text}");
    }

    #[test]
    fn a_job_started_again_is_told_to_read_what_it_left() {
        let mut again = job("Which e-bike?", "");
        again.begin();
        let first = prompt(&again, NoteId::new());
        again.begin();
        let second = prompt(&again, NoteId::new());

        assert!(!first.contains("interrupted"), "one attempt is not a restart");
        assert!(second.contains("interrupted") && second.contains("get_note"), "{second}");
    }

    #[test]
    fn the_note_is_named_after_the_question_but_not_the_whole_of_a_long_one() {
        assert_eq!(note_title("  Which\ne-bike?  "), "Which e-bike?");
        let long = note_title(&"why ".repeat(100));
        assert!(long.chars().count() <= Conversation::MAX_TITLE_CHARS + 1, "{long}");
        assert!(long.ends_with('\u{2026}'));
    }

    #[test]
    fn the_placeholder_is_what_the_conclusion_looks_for() {
        let text = placeholder("Which e-bike?");
        assert!(text.starts_with("Researching"));
        assert!(text.contains("Which e-bike?"));
        // Compared as the text a reader sees, which is how `conclude` does it.
        assert_eq!(
            RichDoc::from_markdown(&text).plain_text(),
            RichDoc::from_markdown(&placeholder("  Which e-bike?  ")).plain_text(),
        );
    }

    #[test]
    fn the_context_says_nobody_is_there_and_where_the_work_goes() {
        for needle in ["Nobody is watching", "do not ask one", "update_plan", "update_note"] {
            assert!(CONTEXT.contains(needle), "should say {needle:?}");
        }
    }
}
