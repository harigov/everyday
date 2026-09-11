//! Notes: writing that is not filed under a day.

use serde_json::{Value, json};

use super::{Args, Tool, ToolContext, done, flag, limit_arg, list, schema, text};
use crate::error::{Error, Result};
use crate::id::NoteId;
use crate::note::{Note, NoteSummary};
use crate::richtext::RichDoc;
use crate::store::notes::{NoteQuery, NoteSort};
use jiff::Timestamp;

pub(super) static TOOLS: &[Tool] = &[
    tool!(
        "list_notes",
        Read,
        Notes,
        schema(
            vec![
                ("tags", list("Keep only notes carrying every one of these tags.")),
                ("pinned", flag("Keep only pinned notes.")),
                limit_arg(),
            ],
            &[]
        ),
        "Notes matching a filter, most recently changed first. Returns summaries \u{2014} \
         id, title, tags, the first line \u{2014} not the text. Call get_note for the body.",
        run_list_notes
    ),
    tool!(
        "get_note",
        Read,
        Notes,
        schema(vec![("note_id", text("Id from list_notes or search."))], &["note_id"]),
        "One note in full, its body rendered as Markdown.",
        run_get_note
    ),
    tool!(
        "create_note",
        Write,
        Notes,
        schema(
            vec![
                ("title", text("What to call it. Worth setting: notes are found by name.")),
                ("body", text("The text. Markdown: paragraphs, headings, lists, tables.")),
                ("tags", list("Tags to attach.")),
                ("pinned", flag("Keep it at the top of the list.")),
            ],
            &["body"]
        ),
        "Write a note. This is where anything longer than a couple of paragraphs \
         belongs \u{2014} a report, a plan, a summary, a list of what you found. Prefer it \
         to a journal entry for writing of your own: an entry is somebody's record of \
         a day they lived, and filling their journal with your reports spoils it.",
        run_create_note
    ),
    tool!(
        "update_note",
        Write,
        Notes,
        schema(
            vec![
                ("note_id", text("Id of the note to change.")),
                ("body", text("Replaces the whole body. Markdown. Omit to leave it alone.")),
                ("title", text("Replaces the title. Omit to leave it alone.")),
                ("tags", list("Replaces the tags entirely. Omit to leave them alone.")),
                ("pinned", flag("Pin or unpin it.")),
            ],
            &["note_id"]
        ),
        "Change an existing note. Every field is optional and omitted fields are left \
         alone \u{2014} but `body` and `tags` replace rather than append, so read the note \
         first if you mean to add to it.",
        run_update_note
    ),
    tool!(
        "delete_note",
        Destructive,
        Notes,
        schema(vec![("note_id", text("Id of the note to delete."))], &["note_id"]),
        "Permanently delete a note. There is no undo. Only do this when explicitly \
         asked to delete that specific note.",
        run_delete_note,
        Some(describe_delete_note)
    ),
];

fn describe_delete_note(ctx: &ToolContext<'_>, args: &Args<'_>) -> Option<String> {
    let id: NoteId = args.opt_id("note_id", "note").ok()??;
    ctx.vault.note(id).ok().map(|n| n.display_title())
}

fn note_json(n: &NoteSummary) -> Value {
    json!({
        "id": n.id.to_string(),
        "title": n.title,
        "excerpt": n.excerpt,
        "tags": n.tags,
        "pinned": n.pinned,
        "updated": n.updated_at.to_string(),
    })
}

fn run_list_notes(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let query = NoteQuery {
        tags: args.strings("tags"),
        pinned: args.opt_bool("pinned"),
        sort: NoteSort::UpdatedDesc,
        offset: 0,
        limit: Some(args.limit()),
    };
    let notes = ctx.vault.notes(&query)?;
    Ok(json!({
        "count": notes.len(),
        "notes": notes.iter().map(note_json).collect::<Vec<_>>(),
    }))
}

fn run_get_note(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: NoteId = args.id("note_id", "note")?;
    let n = ctx.vault.note(id)?;
    Ok(json!({
        "id": n.id.to_string(),
        "title": n.display_title(),
        "tags": n.tags,
        "pinned": n.pinned,
        "created": n.created_at.to_string(),
        "updated": n.updated_at.to_string(),
        // Markdown rather than the ProseMirror document, for the reason
        // `get_entry` gives: the model reads and writes prose, and the
        // editor's own tree is not its business.
        "body": n.body.to_markdown(),
    }))
}

fn run_create_note(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let mut note = Note::written(args.opt_str("title").unwrap_or_default(), args.str("body")?);
    note.tags = args.strings("tags");
    note.pinned = args.bool_or("pinned", false);
    ctx.vault.save_note(&note, None)?;
    done("created", "note", &note.display_title(), note.id.to_string())
}

fn run_update_note(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: NoteId = args.id("note_id", "note")?;
    let mut note = ctx.vault.note(id)?;

    if let Some(body) = args.opt_str("body") {
        // The same refusal `update_entry` makes, for the same reason: a body
        // arrives whole or not at all, and the model was handed Markdown, in
        // which a photograph is a link it cannot put back.
        if !note.body.blob_refs().is_empty() {
            return Err(Error::Invalid(
                "this note has photographs or files in it, and replacing its text would \
                 take them off the page. Edit it in the app, or say what to change and \
                 leave the body alone."
                    .into(),
            ));
        }
        note.body = RichDoc::from_markdown(body);
    }
    if let Some(title) = args.opt_str("title") {
        note.title = title.to_string();
    }
    if args.get("tags").is_some() {
        note.tags = args.strings("tags");
    }
    if let Some(pinned) = args.opt_bool("pinned") {
        note.pinned = pinned;
    }
    // Stamped here for the reason `update_entry` gives: every writer owns its
    // own `updated_at`, and an editor still holding this note would otherwise
    // keep matching the version it loaded and overwrite this edit silently.
    note.updated_at = Timestamp::now();

    // Unconditional, as `update_entry` is: the other writer here is the
    // person sitting in front of it, and a failed tool call they would have
    // to resolve by hand is worse than the last write winning.
    ctx.vault.overwrite_note(&note)?;
    done("updated", "note", &note.display_title(), note.id.to_string())
}

fn run_delete_note(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: NoteId = args.id("note_id", "note")?;
    // Read it first, so the confirmation card and the reply can name what
    // went rather than quoting an id at somebody.
    let note = ctx.vault.note(id)?;
    ctx.vault.delete_note(id)?;
    done("deleted", "note", &note.display_title(), id.to_string())
}
