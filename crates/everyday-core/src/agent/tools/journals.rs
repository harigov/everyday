//! Orientation, and journals and entries.
//!
//! `overview` and `search` travel with this file rather than living apart:
//! both are [`Domain::Journals`], and neither is big enough on its own to
//! want a file of its own.

use serde_json::{Value, json};

use super::{
    Args, Tool, ToolContext, day, done, empty_schema, flag, limit_arg, list, schema, text,
};
use crate::error::{Error, Result};
use crate::id::{EntryId, JournalId};
use crate::model::{Entry, Journal};
use crate::richtext::RichDoc;
use crate::search::{Found, SearchScope};
use crate::store::purpose::GoalQuery;
use crate::store::{EntryQuery, SortOrder};
use jiff::Timestamp;

pub(super) static TOOLS: &[Tool] = &[
    // ---- orientation ------------------------------------------------------
    tool!(
        "overview",
        Read,
        Journals,
        empty_schema(),
        "What this vault contains: its journals, projects, shelves and trackers, \
         with counts, plus what is due today. Call this first in a new conversation \
         when you do not yet know what exists.",
        run_overview
    ),
    tool!(
        "search",
        Read,
        Journals,
        schema(
            vec![
                ("query", text("Words to look for. Full-text over entry titles and bodies.")),
                ("journal_id", text("Restrict to one journal. Omit to search all of them.")),
                limit_arg(),
            ],
            &["query"]
        ),
        "Full-text search across journal entries. The way to find an entry when \
         you know roughly what it said but not when it was written. Searches \
         entries only \u{2014} use list_tasks or list_items to find those.",
        run_search
    ),
    // ---- journals and entries -----------------------------------------------
    tool!(
        "list_journals",
        Read,
        Journals,
        empty_schema(),
        "Every journal in the vault, with its id, name and description.",
        run_list_journals
    ),
    tool!(
        "list_entries",
        Read,
        Journals,
        schema(
            vec![
                ("journal_id", text("Restrict to one journal.")),
                ("from", day("Earliest date, inclusive.")),
                ("to", day("Latest date, inclusive.")),
                ("tags", list("Keep only entries carrying every one of these tags.")),
                ("starred", flag("Keep only starred entries.")),
                limit_arg(),
            ],
            &[]
        ),
        "Journal entries matching a filter, newest first. Returns summaries \u{2014} \
         id, date, title, tags \u{2014} not the text. Call get_entry for the body.",
        run_list_entries
    ),
    tool!(
        "get_entry",
        Read,
        Journals,
        schema(vec![("entry_id", text("Id from list_entries or search."))], &["entry_id"]),
        "One entry in full, its body rendered as Markdown.",
        run_get_entry
    ),
    tool!(
        "create_entry",
        Write,
        Journals,
        schema(
            vec![
                ("journal_id", text("Which journal to file it in. From list_journals.")),
                ("body", text("The entry text. Markdown: paragraphs, headings, lists.")),
                ("title", text("Optional heading. Omit and the first line becomes one.")),
                ("date", day("File it under this day instead of today. Back-dating is fine.")),
                ("tags", list("Tags to attach.")),
            ],
            &["journal_id", "body"]
        ),
        "Write a new journal entry. Use this when asked to record, draft or write \
         something up \u{2014} not for a task, which is create_task.",
        run_create_entry
    ),
    tool!(
        "update_entry",
        Write,
        Journals,
        schema(
            vec![
                ("entry_id", text("Id of the entry to change.")),
                ("body", text("Replaces the whole body. Markdown. Omit to leave it alone.")),
                ("title", text("Replaces the title. Omit to leave it alone.")),
                ("date", day("Re-file under a different day.")),
                ("tags", list("Replaces the tags entirely. Omit to leave them alone.")),
                ("starred", flag("Star or unstar it.")),
            ],
            &["entry_id"]
        ),
        "Change an existing entry. Every field is optional and omitted fields are \
         left alone \u{2014} but `body` and `tags` replace rather than append, so read \
         the entry first if you mean to add to it.",
        run_update_entry
    ),
    tool!(
        "delete_entry",
        Destructive,
        Journals,
        schema(vec![("entry_id", text("Id of the entry to delete."))], &["entry_id"]),
        "Permanently delete a journal entry. There is no undo. Only do this when \
         explicitly asked to delete that specific entry.",
        run_delete_entry,
        Some(describe_delete_entry)
    ),
];

fn describe_delete_entry(ctx: &ToolContext<'_>, args: &Args<'_>) -> Option<String> {
    let id: EntryId = args.opt_id("entry_id", "entry").ok()??;
    ctx.vault.entry(id).ok().map(|e| e.display_title())
}

// ---- projections ----------------------------------------------------------

fn entry_summary_json(e: &crate::model::EntrySummary) -> Value {
    let mut v = json!({
        "id": e.id.to_string(),
        "date": e.local_date.to_string(),
        "title": e.title,
        "journal_id": e.journal_id.to_string(),
    });
    let m = v.as_object_mut().unwrap();
    if !e.tags.is_empty() {
        m.insert("tags".into(), json!(e.tags));
    }
    if e.starred {
        m.insert("starred".into(), json!(true));
    }
    v
}

fn entry_json(e: &Entry) -> Value {
    json!({
        "id": e.id.to_string(),
        "journal_id": e.journal_id.to_string(),
        "date": e.local_date.to_string(),
        "title": e.display_title(),
        // Markdown rather than the ProseMirror document. The model reads and
        // writes prose; handing it a node tree would spend hundreds of
        // tokens on braces and invite it to edit them.
        "body": e.body.to_markdown(),
        "tags": e.tags,
        "starred": e.starred,
    })
}

/// One hit, saying which kind of thing it is.
///
/// The kind is not decoration: it tells the model which tool to reach for
/// next. `get_entry` on a note id would fail, and a model that had no way to
/// tell them apart would have to guess.
fn search_hit_json(h: &crate::search::SearchHit) -> Value {
    let mut out = json!({
        "title": h.title,
        "excerpt": h.snippet,
    });
    let map = out.as_object_mut().expect("built as an object");
    match h.found {
        Found::Entry { id, local_date, .. } => {
            map.insert("kind".into(), json!("entry"));
            map.insert("id".into(), json!(id.to_string()));
            map.insert("date".into(), json!(local_date.to_string()));
        }
        Found::Note { id } => {
            map.insert("kind".into(), json!("note"));
            map.insert("id".into(), json!(id.to_string()));
        }
    }
    out
}

// ---- implementations --------------------------------------------------

fn run_overview(ctx: &ToolContext<'_>, _args: &Args<'_>) -> Result<Value> {
    let vault = ctx.vault;
    let mut out = json!({ "today": ctx.today.to_string(), "time_zone": ctx.tz });
    let m = out.as_object_mut().unwrap();

    m.insert(
        "journals".into(),
        json!(
            vault
                .journals()?
                .iter()
                .map(|j| json!({ "id": j.id.to_string(), "name": j.name }))
                .collect::<Vec<_>>()
        ),
    );

    if vault.supports_tasks() {
        let stats = vault.task_stats(ctx.today)?;
        m.insert(
            "tasks".into(),
            json!({
                "open": stats.open_tasks,
                "done": stats.done_tasks,
                // `overdue` is a subset of `due_today` rather than a
                // separate bucket, so it is reported as one -- adding them
                // would double-count every late task.
                "due_today_or_overdue": stats.due_today,
                "overdue": stats.overdue,
            }),
        );
        m.insert(
            "projects".into(),
            json!(
                vault
                    .projects()?
                    .iter()
                    .filter(|p| p.status.is_open())
                    .map(|p| json!({ "id": p.id.to_string(), "name": p.name, "status": p.status.as_str() }))
                    .collect::<Vec<_>>()
            ),
        );
    }

    if vault.supports_library() {
        m.insert(
            "shelves".into(),
            json!(
                vault
                    .kinds()?
                    .iter()
                    .map(|k| json!({ "id": k.id.to_string(), "name": k.name }))
                    .collect::<Vec<_>>()
            ),
        );
    }

    if vault.supports_trackers() {
        let trackers: Vec<Value> = vault
            .trackers()?
            .iter()
            .filter(|t| !t.archived)
            .map(|t| json!({ "id": t.id.to_string(), "name": t.name }))
            .collect();
        m.insert("trackers".into(), json!(trackers));
    }

    if vault.supports_purpose() {
        // The parts of a life and what is wanted from each, so a model
        // knows the vocabulary before it is asked a question in it. Names
        // and ids only -- the hours are `time_by_role`'s answer and the
        // per-goal detail is `list_goals`'.
        let roles: Vec<Value> = vault
            .roles()?
            .iter()
            .filter(|r| !r.archived)
            .map(|r| json!({ "id": r.id.to_string(), "name": r.name }))
            .collect();
        let goals: Vec<Value> = vault
            .goals(&GoalQuery::open())?
            .iter()
            .map(|g| {
                json!({
                    "id": g.id.to_string(),
                    "title": g.title,
                    "role_id": g.role_id.to_string(),
                    "status": g.status.as_str(),
                })
            })
            .collect();
        m.insert("roles".into(), json!(roles));
        m.insert("open_goals".into(), json!(goals));
    }

    Ok(out)
}

fn run_search(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let journal: Option<JournalId> = args.opt_id("journal_id", "journal")?;
    // Naming a journal narrows to entries, because a note is in no journal
    // and silently returning some anyway would answer a different question.
    let scope = match journal {
        Some(id) => SearchScope::Entries(Some(id)),
        None => SearchScope::Everything,
    };
    let hits = ctx.vault.search(args.str("query")?, scope, args.limit() as usize)?;
    Ok(json!({
        "count": hits.len(),
        "results": hits.iter().map(search_hit_json).collect::<Vec<_>>(),
    }))
}

fn run_list_journals(ctx: &ToolContext<'_>, _args: &Args<'_>) -> Result<Value> {
    Ok(json!(
        ctx.vault
            .journals()?
            .iter()
            .map(|j: &Journal| json!({
                "id": j.id.to_string(),
                "name": j.name,
                "description": j.description,
            }))
            .collect::<Vec<_>>()
    ))
}

fn run_list_entries(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let query = EntryQuery {
        journal_id: args.opt_id("journal_id", "journal")?,
        from: args.opt_date("from")?,
        to: args.opt_date("to")?,
        tags: args.strings("tags"),
        starred: args.opt_bool("starred"),
        sort: SortOrder::DateDesc,
        offset: 0,
        limit: Some(args.limit()),
    };
    let rows = ctx.vault.entries(&query)?;
    Ok(json!({
        "count": rows.len(),
        "entries": rows.iter().map(entry_summary_json).collect::<Vec<_>>(),
    }))
}

fn run_get_entry(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: EntryId = args.id("entry_id", "entry")?;
    Ok(entry_json(&ctx.vault.entry(id)?))
}

fn run_create_entry(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let journal_id: JournalId = args.id("journal_id", "journal")?;
    // Read the journal rather than trusting the id, so an entry cannot be
    // filed into a journal that does not exist and then be invisible.
    let journal = ctx.vault.journal(journal_id)?;

    let mut entry = Entry::new(journal_id, ctx.tz);
    entry.body = RichDoc::from_markdown(args.str("body")?);
    entry.title = args.opt_str("title").unwrap_or_default().to_string();
    entry.tags = args.strings("tags");
    if let Some(d) = args.opt_date("date")? {
        entry.local_date = d;
    }

    ctx.vault.save_entry(&entry, None)?;
    done(
        "created",
        "entry",
        &format!("{} in {}", entry.display_title(), journal.name),
        entry.id.to_string(),
    )
}

fn run_update_entry(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: EntryId = args.id("entry_id", "entry")?;
    let mut entry = ctx.vault.entry(id)?;

    if let Some(body) = args.opt_str("body") {
        // A body arrives whole or not at all, so replacing one that holds
        // photographs would take them off the page -- the blobs survive, via
        // `Entry::attachments`, but the entry stops showing them. Refused
        // rather than silently done, because there is no undo and no way for
        // the model to know what it dropped: it was handed the body as
        // Markdown, in which an attachment is a link it cannot put back.
        if !entry.body.blob_refs().is_empty() {
            return Err(Error::Invalid(
                "this entry has photographs or files in it, and replacing its text would \
                 take them off the page. Edit it in the app, or say what to change and \
                 leave the body alone."
                    .into(),
            ));
        }
        entry.body = RichDoc::from_markdown(body);
    }
    if let Some(title) = args.opt_str("title") {
        entry.title = title.to_string();
    }
    if let Some(d) = args.opt_date("date")? {
        entry.local_date = d;
    }
    if args.get("tags").is_some() {
        entry.tags = args.strings("tags");
    }
    if let Some(starred) = args.opt_bool("starred") {
        entry.starred = starred;
    }

    // Stamped here because the vault does not do it: every writer owns its
    // own `updated_at`, and the interface stamps one before each autosave.
    // Without this the record's timestamp never moves, so an editor that
    // still has the entry open keeps matching on the version it loaded and
    // its next save overwrites the assistant's edit with no conflict
    // reported -- the exact loss `put_entry_if` exists to prevent.
    entry.updated_at = Timestamp::now();

    // Unconditional rather than optimistic. The optimistic path exists for
    // two people editing the same entry in two windows; here the other
    // writer is the person sitting in front of it, and a failed tool call
    // they would have to resolve by hand is worse than the last write
    // winning -- which is what the interface's own autosave does anyway.
    ctx.vault.overwrite_entry(&entry)?;
    done("updated", "entry", &entry.display_title(), entry.id.to_string())
}

fn run_delete_entry(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: EntryId = args.id("entry_id", "entry")?;
    // Read it first, so the confirmation and the reply can name what went
    // rather than quoting an id at somebody.
    let entry = ctx.vault.entry(id)?;
    ctx.vault.delete_entry(id)?;
    done("deleted", "entry", &entry.display_title(), id.to_string())
}
