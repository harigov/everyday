//! The assistant: what you asked it, what it did, and what it remembers.
//!
//! ```text
//!   assistant/
//!     conversations/2026-09-10-what-did-i-do-in-august.md
//!     routines/Morning-brief.md
//!     memories.md
//! ```
//!
//! # This part does not import
//!
//! It is the one that answers no, and the reason is worth stating rather than
//! implying. A transcript is a record of something that happened: writing one
//! into a vault would be minuting a conversation nobody had there, and a
//! vault whose history could be authored from a file is a vault whose history
//! means nothing.
//!
//! Its standing work and its memories *could* come back, and deliberately do
//! not come back this way either. An export is for leaving with your writing;
//! restoring a working assistant -- its schedule, what it has learnt about
//! you, the key it talks to a model with -- is what `everyday backup` is for,
//! and that copies the vault sealed rather than turning it into text. Two
//! verbs, two jobs.
//!
//! What is here is therefore a *reading* copy: a folder of transcripts you
//! can search with `grep`, and a page saying what the thing knows about you,
//! which is the question anybody wants answered about an assistant they have
//! been talking to for a year.

use crate::text::{FrontMatter, safe_name};
use crate::{Files, Options, Portable, Spec};
use everyday_core::Result;
use everyday_core::agent::Role;
use everyday_core::store::JournalStore;
use everyday_core::store::agent::ConversationQuery;
use std::fmt::Write as _;

pub struct AssistantPart;
pub static ASSISTANT: AssistantPart = AssistantPart;

static SPEC: Spec = Spec {
    id: "assistant",
    label: "Assistant",
    summary: "Every conversation as a readable transcript, the standing work you set it, \
              and what it has been asked to remember. A reading copy: this part is not \
              read back in.",
    format: "Markdown, one file per conversation",
    media: false,
    imports: false,
};

impl Portable for AssistantPart {
    fn spec(&self) -> &'static Spec {
        &SPEC
    }

    fn tally(&self, store: &dyn JournalStore) -> Result<Option<u64>> {
        let Some(agent) = store.agent() else { return Ok(None) };
        let threads = agent.list_conversations(&ConversationQuery::default())?.len() as u64;
        Ok(Some(threads + agent.list_memories()?.len() as u64))
    }

    fn export(&self, store: &dyn JournalStore, out: &mut Files<'_>, _opts: &Options) -> Result<()> {
        let Some(agent) = store.agent() else { return Ok(()) };

        for conversation in agent.list_conversations(&ConversationQuery::default())? {
            let messages = agent.list_messages(conversation.id)?;
            let mut front = FrontMatter::new();
            front
                .set("title", &conversation.title)
                .always("id", conversation.id.to_string())
                .set("started", super::doc::stamp(conversation.created_at))
                .set("updated", super::doc::stamp(conversation.updated_at))
                .flag("routine", conversation.run_id.is_some());

            let mut body = front.render();
            let title = match conversation.title.trim() {
                "" => "Untitled conversation".to_string(),
                title => title.to_string(),
            };
            let _ = write!(body, "# {title}\n\n");

            for message in &messages {
                let who = match message.role {
                    Role::User => "You",
                    Role::Assistant => "Assistant",
                    Role::Tool => "Tool result",
                    Role::System => "Every Day",
                };
                let _ = write!(body, "## {who}\n\n");
                if !message.content.trim().is_empty() {
                    let _ = write!(body, "{}\n\n", message.content.trim());
                }
                // What it did, rather than only what it said. A transcript
                // that showed the prose and hid the writes would be a
                // flattering record rather than an accurate one.
                for call in &message.tool_calls {
                    let _ = writeln!(body, "> ran `{}`", call.name);
                }
                if !message.tool_calls.is_empty() {
                    body.push('\n');
                }
                if message.failed {
                    body.push_str("> *that did not work*\n\n");
                }
            }

            let day = conversation.created_at.to_zoned(jiff::tz::TimeZone::system()).date();
            let name =
                format!("conversations/{day}-{}-{}.md", safe_name(&title), conversation.id.short());
            out.records(&name, body, 1)?;
        }

        if let Some(routines) = store.routines() {
            for routine in routines.list_routines()? {
                let mut front = FrontMatter::new();
                front
                    .always("routine", &routine.name)
                    .always("id", routine.id.to_string())
                    .always("when", routine.trigger.describe())
                    .always("enabled", routine.enabled.to_string())
                    .set_opt("last_run", routine.last_run_at)
                    .set("created", super::doc::stamp(routine.created_at));
                let body = format!(
                    "{}# {}\n\n*{}*\n\n## What it is asked to do\n\n{}\n",
                    front.render(),
                    routine.name,
                    routine.trigger.describe(),
                    routine.instructions.trim()
                );
                let name =
                    format!("routines/{}-{}.md", safe_name(&routine.name), routine.id.short());
                out.records(&name, body, 1)?;
            }
        }

        let memories = agent.list_memories()?;
        if !memories.is_empty() {
            let mut body = String::from("# What the assistant remembers\n\n");
            for memory in &memories {
                let pin = if memory.pinned { " *(yours)*" } else { "" };
                let _ = writeln!(body, "- {}{pin}", memory.text.trim());
            }
            out.records("memories.md", body, memories.len() as u64)?;
        }
        Ok(())
    }
}
