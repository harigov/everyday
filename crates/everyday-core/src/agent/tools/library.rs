//! The library: shelves, the things on them, and the log of what happened
//! to each.

use serde_json::{Value, json};

use super::{
    Args, Tool, ToolContext, day, decimal, done, empty_schema, flag, limit_arg, list, number,
    one_of, schema, text,
};
use crate::error::Result;
use crate::id::ItemId;
use crate::library::{Item, ItemStatus, LogEntry, LogEvent};
use crate::store::library::ItemQuery;

const ITEM_STATUSES: &[&str] = &["wishlist", "active", "paused", "done", "abandoned"];

pub(super) static TOOLS: &[Tool] = &[
    tool!(
        "list_shelves",
        Read,
        Library,
        empty_schema(),
        "The shelves in the library \u{2014} Books, Films, whatever else was made \u{2014} \
         with their ids and how many things are on them. Call this before \
         list_items or create_item, which need a shelf id.",
        run_list_kinds
    ),
    tool!(
        "list_items",
        Read,
        Library,
        schema(
            vec![
                ("shelf_id", text("Restrict to one shelf. From list_shelves.")),
                ("status", one_of("Restrict to one status.", ITEM_STATUSES)),
                ("text", text("Case-insensitive substring of the title or creator.")),
                ("favourite", flag("Keep only favourites.")),
                ("finished_from", day("Finished on or after this day.")),
                ("finished_to", day("Finished on or before this day.")),
                limit_arg(),
            ],
            &[]
        ),
        "Things on the shelves: books, films, games, whatever this vault tracks. \
         For 'what did I read this year', filter on finished_from and finished_to.",
        run_list_items
    ),
    tool!(
        "create_item",
        Write,
        Library,
        schema(
            vec![
                ("shelf_id", text("Which shelf. From list_shelves.")),
                ("title", text("What it is called.")),
                ("creator", text("Author, director, developer.")),
                ("status", one_of("Defaults to wishlist.", ITEM_STATUSES)),
                ("year", number("Year it came out.")),
                ("rating", decimal("Out of 10, so 8 means four stars.")),
                ("notes", text("Anything worth saying about it.")),
                ("tags", list("Tags to attach.")),
            ],
            &["shelf_id", "title"]
        ),
        "Put something on a shelf. Does not look anything up online \u{2014} it records \
         what you tell it.",
        run_create_item
    ),
    tool!(
        "update_item",
        Write,
        Library,
        schema(
            vec![
                ("item_id", text("Id from list_items.")),
                ("title", text("Rename it.")),
                ("creator", text("Change the author or director.")),
                (
                    "status",
                    one_of("Move it. `done` records that you got through it.", ITEM_STATUSES)
                ),
                ("rating", decimal("Out of 10.")),
                ("favourite", flag("Mark or unmark a favourite.")),
                ("notes", text("Replace the notes.")),
                ("started_on", day("The day you started.")),
                ("finished_on", day("The day you finished.")),
                ("tags", list("Replaces the tags entirely.")),
            ],
            &["item_id"]
        ),
        "Change something on a shelf: finish it, rate it, make it a favourite.",
        run_update_item
    ),
    tool!(
        "delete_item",
        Destructive,
        Library,
        schema(vec![("item_id", text("Id from list_items."))], &["item_id"]),
        "Permanently delete something from a shelf, and its whole history with it. \
         To record giving up on something, use update_item with status abandoned.",
        run_delete_item,
        Some(describe_delete_item)
    ),
];

fn describe_delete_item(ctx: &ToolContext<'_>, args: &Args<'_>) -> Option<String> {
    let id: ItemId = args.opt_id("item_id", "item").ok()??;
    ctx.vault.item(id).ok().map(|i| i.title)
}

/// A rating as the tools speak it, converted to how the vault stores it.
///
/// The library stores `0..=100` so that four stars from one site and 82%
/// from another are comparable. Nobody says "give it an eighty", so the tool
/// takes a score out of ten and multiplies -- which also means a model that
/// sends 8 and a model that sends 80 do not silently record wildly different
/// opinions, because the second is refused.
fn rating_out_of_ten(args: &Args<'_>) -> Result<Option<u8>> {
    let Some(raw) = args.opt_f64("rating") else { return Ok(None) };
    if !(0.0..=10.0).contains(&raw) {
        return Err(args.bad(format!("`rating` is out of 10, got {raw}")));
    }
    Ok(Some((raw * 10.0).round() as u8))
}

fn item_json(i: &Item) -> Value {
    let mut v = json!({
        "id": i.id.to_string(),
        "shelf_id": i.kind_id.to_string(),
        "title": i.title,
        "status": i.status.as_str(),
    });
    let m = v.as_object_mut().unwrap();
    if !i.creator.trim().is_empty() {
        m.insert("creator".into(), json!(i.creator));
    }
    if let Some(y) = i.year {
        m.insert("year".into(), json!(y));
    }
    if let Some(r) = i.rating {
        m.insert("rating_out_of_10".into(), json!(f64::from(r) / 10.0));
    }
    if i.favourite {
        m.insert("favourite".into(), json!(true));
    }
    if let Some(d) = i.finished_on {
        m.insert("finished_on".into(), json!(d.to_string()));
    }
    if !i.tags.is_empty() {
        m.insert("tags".into(), json!(i.tags));
    }
    v
}

fn run_list_kinds(ctx: &ToolContext<'_>, _args: &Args<'_>) -> Result<Value> {
    Ok(json!(
        ctx.vault
            .kinds()?
            .iter()
            .map(|k| json!({
                "id": k.id.to_string(),
                "name": k.name,
                "singular": k.singular,
            }))
            .collect::<Vec<_>>()
    ))
}

fn run_list_items(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let query = ItemQuery {
        kind_id: args.opt_id("shelf_id", "shelf")?,
        statuses: args.opt_enum::<ItemStatus>("status", ITEM_STATUSES)?.into_iter().collect(),
        text: args.opt_str("text").unwrap_or_default().to_string(),
        favourite: args.opt_bool("favourite"),
        finished_from: args.opt_date("finished_from")?,
        finished_to: args.opt_date("finished_to")?,
        limit: Some(args.limit()),
        ..Default::default()
    };
    let rows = ctx.vault.items(&query)?;
    Ok(json!({
        "count": rows.len(),
        "items": rows.iter().map(item_json).collect::<Vec<_>>(),
    }))
}

fn run_create_item(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let kind_id = args.id("shelf_id", "shelf")?;
    let kind = ctx.vault.kind(kind_id)?;

    let mut item = Item::new(kind_id, args.str("title")?);
    item.creator = args.opt_str("creator").unwrap_or_default().to_string();
    if let Some(s) = args.opt_enum("status", ITEM_STATUSES)? {
        item.set_status(s, ctx.today);
    }
    item.year = args.opt_u32("year").map(|y| y as i16);
    item.rating = rating_out_of_ten(args)?;
    item.notes = args.opt_str("notes").unwrap_or_default().to_string();
    item.tags = args.strings("tags");

    ctx.vault.save_item(&item)?;
    done(
        "created",
        &kind.singular,
        &format!("{} on {}", item.title, kind.name),
        item.id.to_string(),
    )
}

fn run_update_item(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: ItemId = args.id("item_id", "item")?;
    let mut item = ctx.vault.item(id)?;
    let was = item.status;

    if let Some(title) = args.opt_str("title") {
        item.title = title.to_string();
    }
    if let Some(creator) = args.opt_str("creator") {
        item.creator = creator.to_string();
    }
    if let Some(s) = args.opt_enum("status", ITEM_STATUSES)? {
        // `set_status` is what makes the dates mean something: starting
        // something records when, finishing it records when, and putting it
        // back on the wishlist clears both -- otherwise an item bounced
        // through "done" by a misreading keeps a finish date it never
        // earned and is counted in the year's tally forever.
        item.set_status(s, ctx.today);
    }
    if let Some(r) = rating_out_of_ten(args)? {
        item.rating = Some(r);
    }
    if let Some(f) = args.opt_bool("favourite") {
        item.favourite = f;
    }
    if let Some(notes) = args.opt_str("notes") {
        item.notes = notes.to_string();
    }
    if let Some(d) = args.opt_date("started_on")? {
        item.started_on = Some(d);
    }
    if let Some(d) = args.opt_date("finished_on")? {
        item.finished_on = Some(d);
    }
    if args.get("tags").is_some() {
        item.tags = args.strings("tags");
    }

    item.updated_at = jiff::Timestamp::now();
    ctx.vault.save_item(&item)?;

    // The same transitions the interface logs, and for its reason: the
    // library's year in review is built from the log, so a status change
    // that skips it is a book that silently missed the list. Going back to
    // the wishlist is a correction rather than an event and logs nothing.
    if item.status != was {
        let event = match item.status {
            ItemStatus::Active => Some(LogEvent::Started),
            ItemStatus::Done => Some(LogEvent::Finished),
            ItemStatus::Paused | ItemStatus::Abandoned => Some(LogEvent::Stopped),
            ItemStatus::Wishlist => None,
        };
        if let Some(event) = event {
            let when = item.finished_on.filter(|_| item.status == ItemStatus::Done);
            let log = LogEntry::new(item.id, event, when.unwrap_or(ctx.today), ctx.tz);
            // The item is already saved. Failing the whole call because the
            // history line did not land would report "nothing happened" for
            // a change that did.
            if let Err(e) = ctx.vault.save_log(&log) {
                tracing::warn!(error = %e, "could not log a status change the assistant made");
            }
        }
    }
    done("updated", "item", &item.title, item.id.to_string())
}

fn run_delete_item(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id: ItemId = args.id("item_id", "item")?;
    let item = ctx.vault.item(id)?;
    ctx.vault.delete_item(id)?;
    done("deleted", "item", &item.title, id.to_string())
}
