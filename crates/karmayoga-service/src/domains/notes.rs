//! Notes: writing that is not a day.
//!
//! Seven commands and nothing surprising in any of them. The one thing worth
//! knowing is that `save_note` is conditional and `save_note_force` is not,
//! exactly as the entry pair is: a note is a document somebody types into for
//! minutes at a time and the interface autosaves, so two windows on one vault
//! meet the conflict dialog rather than losing an edit between them.

use super::Nothing;
use crate::command;
use crate::ctx::Ctx;
use crate::error::CommandResult;
use crate::service::Service;
use jiff::Timestamp;
use karmayoga_core::model::{local_date_in, system_tz};
use karmayoga_core::note::{Note, NoteSummary};
use karmayoga_core::store::notes::NoteQuery;
use karmayoga_core::template::Context;
use karmayoga_core::{NoteId, TemplateId};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notes {
    #[serde(default)]
    pub query: NoteQuery,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRef {
    pub id: NoteId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveNote {
    pub note: Note,
    /// The `updated_at` the caller last read, or absent for a new note.
    #[serde(default)]
    pub expect: Option<Timestamp>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForceNote {
    pub note: Note,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewNote {
    /// Start it from this template, filled in for today. Absent for a blank
    /// note.
    #[serde(default)]
    pub template_id: Option<TemplateId>,
}

async fn list_notes(svc: Arc<Service>, _ctx: Ctx, args: Notes) -> CommandResult<Vec<NoteSummary>> {
    svc.on_vault(move |vault| vault.notes(&args.query)).await
}

async fn get_note(svc: Arc<Service>, _ctx: Ctx, args: NoteRef) -> CommandResult<Note> {
    svc.on_vault(move |vault| vault.note(args.id)).await
}

/// Mint a note without saving it, blank or started from a template.
///
/// The same shape as `new_entry`: the id and the timestamps are the core's to
/// make, never the interface's, and an empty note that was never typed into
/// should not be a row.
async fn new_note(svc: Arc<Service>, _ctx: Ctx, args: NewNote) -> CommandResult<Note> {
    let Some(id) = args.template_id else {
        return Ok(Note::new(""));
    };
    svc.on_vault(move |vault| {
        let template = vault.template(id)?;
        let today = local_date_in(Timestamp::now(), &system_tz());
        let filled = template.render(&Context { date: today, journal: None });
        let mut note = Note::new(filled.title);
        note.body = filled.body;
        note.tags = filled.tags;
        Ok(note)
    })
    .await
}

async fn save_note(svc: Arc<Service>, _ctx: Ctx, args: SaveNote) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.save_note(&args.note, args.expect)).await
}

async fn save_note_force(svc: Arc<Service>, _ctx: Ctx, args: ForceNote) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.overwrite_note(&args.note)).await
}

async fn delete_note(svc: Arc<Service>, _ctx: Ctx, args: NoteRef) -> CommandResult<()> {
    svc.on_vault(move |vault| vault.delete_note(args.id)).await
}

async fn note_tags(svc: Arc<Service>, _ctx: Ctx, _args: Nothing) -> CommandResult<Vec<String>> {
    svc.on_vault(move |vault| Ok(vault.note_tags()?.into_iter().map(|(tag, _)| tag).collect()))
        .await
}

pub static COMMANDS: &[crate::command::Command] = &[
    command! {
        name: "list_notes", scope: Notes, effect: Read,
        args: Notes, returns: "NoteSummary[]",
        signature: &[("query", "NoteQuery", false)],
        run: list_notes,
    },
    command! {
        name: "get_note", scope: Notes, effect: Read,
        args: NoteRef, returns: "Note",
        signature: &[("id", "NoteId", true)],
        run: get_note,
    },
    command! {
        name: "new_note", scope: Notes, effect: Read,
        args: NewNote, returns: "Note",
        signature: &[("templateId", "TemplateId | null", false)],
        run: new_note,
    },
    command! {
        name: "save_note", scope: Notes, effect: Write,
        change: Note / Updated,
        id: |a: &SaveNote| Some(a.note.id.to_string()),
        args: SaveNote, returns: "void",
        signature: &[("note", "Note", true), ("expect", "string | null", false)],
        run: save_note,
    },
    command! {
        name: "save_note_force", scope: Notes, effect: Write,
        change: Note / Updated,
        id: |a: &ForceNote| Some(a.note.id.to_string()),
        args: ForceNote, returns: "void",
        signature: &[("note", "Note", true)],
        run: save_note_force,
    },
    command! {
        name: "delete_note", scope: Notes, effect: Destructive,
        change: Note / Deleted,
        id: |a: &NoteRef| Some(a.id.to_string()),
        args: NoteRef, returns: "void",
        signature: &[("id", "NoteId", true)],
        run: delete_note,
    },
    command! {
        name: "note_tags", scope: Notes, effect: Read,
        args: Nothing, returns: "string[]", signature: &[],
        run: note_tags,
    },
];
