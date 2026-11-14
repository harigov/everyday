//! Templates: the shapes a journal entry, a note or a meeting's notes start
//! from. See `karmayoga_core::template` for what a body means and how a
//! journal picks one for a day.
//!
//! Their own scope, [`Scope::Templates`](crate::ctx::Scope::Templates),
//! rather than a corner of `Journals`, `Notes` or `Meetings`: one record
//! serves all three, and a client that edits templates has no business
//! inheriting any of those domains' writing for it. Applying one is not
//! here: `new_entry` and `new_note` fill a template in themselves, under
//! their own scopes, so a client that may start a journal entry gets its
//! journal's template without being able to read or change the list.

use super::Nothing;
use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::service::{Service, blocking};
use jiff::Timestamp;
use jiff::civil::Date;
use karmayoga_core::model::{local_date_in, system_tz};
use karmayoga_core::template::{self, Context, Rendered, Template, TemplateUse};
use karmayoga_core::{JournalId, TemplateId};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTemplate {
    /// Where it will be offered. Decides what text it starts with.
    #[serde(default)]
    pub uses: Vec<TemplateUse>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveTemplate {
    pub template: Template,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateRef {
    pub id: TemplateId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LintTemplate {
    pub template: Template,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderTemplate {
    /// The template as the caller has it -- possibly edited and not yet
    /// saved, which is what a live preview wants.
    pub template: Template,
    /// The day to fill it in for. Today when absent.
    #[serde(default)]
    pub date: Option<Date>,
    /// The journal whose name `{{journal}}` is. Absent for a note.
    #[serde(default)]
    pub journal_id: Option<JournalId>,
}

async fn list_templates(
    svc: Arc<Service>,
    _ctx: Ctx,
    _args: Nothing,
) -> CommandResult<Vec<Template>> {
    svc.on_vault(move |vault| vault.templates()).await
}

/// Mint a template without saving it. The id is the core's to allocate, for
/// the reason `new_note` and `new_routine` give; the text is
/// [`template::starting_body`]'s, so nobody faces an empty box.
async fn new_template(_svc: Arc<Service>, _ctx: Ctx, args: NewTemplate) -> CommandResult<Template> {
    let mut t = Template::new("New template");
    t.body = template::starting_body(&args.uses);
    t.uses = args.uses;
    Ok(t)
}

async fn save_template(svc: Arc<Service>, _ctx: Ctx, args: SaveTemplate) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_template(&args.template)).await
}

async fn delete_template(svc: Arc<Service>, _ctx: Ctx, args: TemplateRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_template(args.id)).await
}

/// What is wrong with a template, in words, for the editor to show as it is
/// typed. Pure: the core decides, and nothing is saved. Never a reason to
/// refuse a save -- see [`template::lint`].
async fn lint_template(
    _svc: Arc<Service>,
    _ctx: Ctx,
    args: LintTemplate,
) -> CommandResult<Vec<String>> {
    Ok(template::lint(&args.template))
}

/// A template filled in for a day: what "Insert template" puts at the
/// cursor, and what the editor's preview shows for the journal and notes.
/// A meeting template's preview is `preview_meeting_template`, which runs
/// the model.
async fn render_template(
    svc: Arc<Service>,
    _ctx: Ctx,
    args: RenderTemplate,
) -> CommandResult<Rendered> {
    let vault = svc.require()?;
    blocking(move || {
        let date = args.date.unwrap_or_else(|| local_date_in(Timestamp::now(), &system_tz()));
        let journal = match args.journal_id {
            Some(id) => Some(vault.journal(id)?.name),
            None => None,
        };
        Ok(args.template.render(&Context { date, journal }))
    })
    .await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_templates", scope: Templates, effect: Read,
        args: Nothing, returns: "Template[]", signature: &[],
        run: list_templates,
    },
    command! {
        name: "new_template", scope: Templates, effect: Read,
        args: NewTemplate, returns: "Template",
        signature: &[("uses", "TemplateUse[]", false)],
        run: new_template,
    },
    command! {
        name: "save_template", scope: Templates, effect: Write,
        change: Template / Updated,
        id: |a: &SaveTemplate| Some(a.template.id.to_string()),
        args: SaveTemplate, returns: "void",
        signature: &[("template", "Template", true)],
        run: save_template,
    },
    command! {
        name: "delete_template", scope: Templates, effect: Destructive,
        change: Template / Deleted,
        id: |a: &TemplateRef| Some(a.id.to_string()),
        args: TemplateRef, returns: "void",
        signature: &[("id", "TemplateId", true)],
        run: delete_template,
    },
    command! {
        name: "lint_template", scope: Templates, effect: Read,
        args: LintTemplate, returns: "string[]",
        signature: &[("template", "Template", true)],
        run: lint_template,
    },
    command! {
        name: "render_template", scope: Templates, effect: Read,
        args: RenderTemplate, returns: "RenderedTemplate",
        signature: &[
            ("template", "Template", true),
            ("date", "string | null", false),
            ("journalId", "JournalId | null", false),
        ],
        run: render_template,
    },
];
