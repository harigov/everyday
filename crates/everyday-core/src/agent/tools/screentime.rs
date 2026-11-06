//! Screen time: how long went on which apps and sites.
//!
//! Read-only. Answers totals by app, site and category over a range -- never
//! a window or tab title, the part of screen time a stranger writes most
//! freely (a page's title, a message's subject).
//!
//! An app's or site's *name* is handed over under `untrusted_text` all the
//! same. It arrives from a collector holding a write-only heartbeats token,
//! and such a token can leak from a browser profile, so a name may be
//! anybody's words; and every read here arms the service's fetch check, as a
//! mail or money read does (`reads_strangers_words`).
//!
//! # What is handed over
//!
//! Every projection below is written field by field from the vault's
//! [`ScreenReport`], never by serialising it whole, for the reason Money's
//! tools give: a model cannot leak what it was never shown. So:
//!
//! - **No title, ever.** The report carries none, and nothing here reads a
//!   timeline, which can.
//! - **No device name unless asked.** "laptop" is harmless; "Hari's work
//!   MacBook" says where somebody works. `by_device` is the one way to see
//!   them, for "how much was on my laptop?".
//! - **Minutes, not seconds.** Rounded to the nearest whole minute: the
//!   unit a person asks in, and the one goal targets are set in.

use serde_json::{Map, Value, json};

use super::{Args, MAX_LIMIT, Tool, ToolContext, Window, day, flag, number, schema, text};
use crate::error::{Error, Result};
use crate::id::ScreenCategoryId;
use crate::screentime::{ScreenCategory, ScreenQuery, ScreenReport, SpanKind, domain};

/// How many apps and sites an answer lists when nobody says.
const DEFAULT_ITEMS: u32 = 10;

pub(super) static TOOLS: &[Tool] = &[tool!(
    "screen_time",
    Read,
    ScreenTime,
    schema(
        vec![
            ("from", day("First day, inclusive. Defaults to `to`.")),
            ("to", day("Last day, inclusive. Defaults to today. At most 400 days in all.")),
            (
                "category",
                text(
                    "Only this screen time category, by name (e.g. \"Social\", \"Work\"), or \
                     \"uncategorized\" for apps and sites in none."
                )
            ),
            (
                "item",
                text(
                    "Only this app or site: its key as an earlier answer gave it (app:firefox, \
                     site:youtube.com), its name as shown (\"YouTube\"), or a web address."
                )
            ),
            ("limit", number("Most apps and sites to list. Defaults to 10, capped at 200.")),
            (
                "by_device",
                flag("Also split the total by computer. Only when they ask about one machine.")
            ),
        ],
        &[]
    ),
    "How long they spent on which apps and websites, from the Screen time app, which \
     records what was focused on each of their computers \u{2014} never what was typed or \
     shown. Answers total_minutes for the range, the top apps and sites (key, name, app or \
     site, minutes, category), minutes per category, minutes per day when the range is \
     longer than a day, and previous_total_minutes for the range of the same length just \
     before, for 'more or less than last week?'. A browser's time is split by site where \
     the browser extension is installed; browser_other marks what was left over in the \
     browser itself. Defaults to today. Window titles are never available. To set a limit \
     or a goal on screen time, use set_target with measure 'screen'.",
    run_screen_time
)];

fn run_screen_time(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let (from, to) = args.window(ctx, Window::Back(0))?;
    let categories = ctx.vault.screen_categories()?;
    let limit = args.opt_u32("limit").unwrap_or(DEFAULT_ITEMS).clamp(1, MAX_LIMIT) as usize;

    // "uncategorized" cannot be a query's category -- `None` there is "every
    // category" -- so it is applied to the answer's items instead.
    let mut only_uncategorized = false;
    let mut category = None;
    if let Some(raw) = args.opt_str("category") {
        if means_none(raw) {
            only_uncategorized = true;
        } else {
            category = Some(find_category(args, &categories, raw)?);
        }
    }

    let mut query =
        ScreenQuery { from, to, tz: ctx.tz.to_string(), devices: Vec::new(), category, item: None };
    let mut note = None;
    if let Some(raw) = args.opt_str("item") {
        match item_key(ctx, &query, raw)? {
            Some(key) => query.item = Some(key),
            None => {
                note = Some(format!(
                    "Nothing called {raw:?} had any screen time from {from} to {to}. Leave out \
                     `item` to see what did."
                ));
                // Still answer, narrowed to a key nothing has, so the shape
                // of the reply -- zeros -- says the same as the note.
                query.item = Some(format!("app:{}", raw.trim().to_lowercase()));
            }
        }
    }

    let report = ctx.vault.screen_report(&query)?;
    let mut out = answer(&report, &categories, limit, only_uncategorized);
    if args.bool_or("by_device", false) {
        out["devices"] = report
            .devices
            .iter()
            .map(|d| json!({ "device": d.device, "minutes": minutes(d.secs) }))
            .collect::<Vec<_>>()
            .into();
    }
    if let Some(note) = note {
        out["note"] = json!(note);
    }
    Ok(out)
}

/// The reply, field by field. `only_uncategorized` keeps the items with no
/// category and recounts the totals from them.
fn answer(
    report: &ScreenReport,
    categories: &[ScreenCategory],
    limit: usize,
    only_uncategorized: bool,
) -> Value {
    let items: Vec<_> =
        report.items.iter().filter(|i| !only_uncategorized || i.category.is_none()).collect();
    let total_secs: u64 =
        if only_uncategorized { items.iter().map(|i| i.secs).sum() } else { report.total_secs };

    let rows: Vec<Value> = items
        .iter()
        .take(limit)
        .map(|item| {
            let mut m = Map::new();
            m.insert("key".into(), json!(item.key));
            // Apart, as Money hands over a bank's line: see the module doc.
            m.insert("untrusted_text".into(), json!({ "name": item.name }));
            m.insert(
                "kind".into(),
                json!(match item.kind {
                    SpanKind::App => "app",
                    SpanKind::Site => "site",
                }),
            );
            m.insert("minutes".into(), json!(minutes(item.secs)));
            m.insert("category".into(), json!(category_name(categories, item.category)));
            if item.browser_rest {
                m.insert("browser_other".into(), json!(true));
            }
            Value::Object(m)
        })
        .collect();

    let mut out = json!({
        "from": report.from.to_string(),
        "to": report.to.to_string(),
        "total_minutes": minutes(total_secs),
        "items": rows,
    });
    if items.len() > limit {
        out["more_items"] = json!(items.len() - limit);
    }
    if only_uncategorized {
        // The previous range and the per-day split are of everything; with
        // only part of it kept, they would compare two different things.
        out["note"] = json!(
            "Only apps and sites in no category. Per-day and previous-range totals are left \
             out: ask without `category` for those."
        );
        return out;
    }
    out["categories"] = report
        .categories
        .iter()
        .map(|c| {
            json!({
                "category": category_name(categories, c.category)
                    .unwrap_or_else(|| "uncategorized".to_string()),
                "minutes": minutes(c.secs),
            })
        })
        .collect::<Vec<_>>()
        .into();
    if report.days.len() > 1 {
        out["days"] = report
            .days
            .iter()
            .map(|d| json!({ "date": d.date.to_string(), "minutes": minutes(d.secs) }))
            .collect::<Vec<_>>()
            .into();
    }
    out["previous_total_minutes"] = json!(minutes(report.previous_total_secs));
    out
}

/// The key `raw` names: a key as given, a name some item in the range goes
/// by, or a web address. `None` when it names nothing that was used.
///
/// A name is matched against the range's own items -- the person's label
/// or the collector's name, case-insensitively, or a key's text -- because
/// a name is only an answer if something by it was there to count.
fn item_key(ctx: &ToolContext<'_>, query: &ScreenQuery, raw: &str) -> Result<Option<String>> {
    let raw = raw.trim();
    let lowered = raw.to_lowercase();
    if lowered.starts_with("app:") {
        return Ok(Some(lowered));
    }
    let everything = ctx.vault.screen_report(query)?;
    if let Some(rest) = lowered.strip_prefix("site:") {
        // A host is an item of its own only when its domain is split, and
        // then the range's items name it; otherwise it is its domain, and
        // the report takes in every host of a domain asked for by name.
        let host = domain::normalize_host(rest);
        let own = format!("site:{host}");
        if everything.items.iter().any(|item| item.key == own) {
            return Ok(Some(own));
        }
        return Ok(Some(format!("site:{}", domain::registrable(&host))));
    }
    let found = everything.items.iter().find(|item| {
        item.name.trim().to_lowercase() == lowered
            || crate::screentime::key_text(&item.key) == lowered
    });
    if let Some(item) = found {
        return Ok(Some(item.key.clone()));
    }
    // "youtube.com", "https://www.youtube.com/watch": a site, as a report
    // names it.
    if raw.contains('.') && !raw.contains(char::is_whitespace) {
        let site = domain::registrable(raw);
        if !site.is_empty() {
            return Ok(Some(format!("site:{site}")));
        }
    }
    Ok(None)
}

fn find_category(
    args: &Args<'_>,
    categories: &[ScreenCategory],
    raw: &str,
) -> Result<ScreenCategoryId> {
    let wanted = raw.trim();
    if let Some(found) = categories
        .iter()
        .find(|c| c.name.trim().eq_ignore_ascii_case(wanted) || c.id.to_string() == wanted)
    {
        return Ok(found.id);
    }
    let names: Vec<&str> = categories.iter().map(|c| c.name.as_str()).collect();
    Err(if names.is_empty() {
        Error::Invalid(
            "screen_time: there are no screen time categories yet; leave out `category`".into(),
        )
    } else {
        args.bad(format!(
            "there is no screen time category called {wanted:?}. The categories are: {}",
            names.join(", ")
        ))
    })
}

fn category_name(categories: &[ScreenCategory], id: Option<ScreenCategoryId>) -> Option<String> {
    let id = id?;
    categories.iter().find(|c| c.id == id).map(|c| c.name.clone())
}

fn means_none(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "none" | "uncategorized" | "uncategorised" | "no category"
    )
}

/// Seconds as whole minutes, to the nearest.
fn minutes(secs: u64) -> u64 {
    (secs + 30) / 60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minutes_round_to_the_nearest() {
        assert_eq!(minutes(0), 0);
        assert_eq!(minutes(29), 0);
        assert_eq!(minutes(30), 1);
        assert_eq!(minutes(89), 1);
        assert_eq!(minutes(3600), 60);
    }

    #[test]
    fn the_words_for_no_category() {
        assert!(means_none("Uncategorized"));
        assert!(means_none(" none "));
        assert!(!means_none("Social"));
    }
}
