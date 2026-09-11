//! What a day produced in numbers rather than in prose: habits, doses,
//! symptoms, counts.

use serde_json::{Value, json};
use std::collections::BTreeMap;

use super::{
    Args, Tool, ToolContext, Window, day, decimal, empty_schema, flag, limit_arg, schema, text,
};
use crate::error::Result;
use crate::id::TrackerId;
use crate::store::trackers::ReadingQuery;
use crate::tracker::Reading;

pub(super) static TOOLS: &[Tool] = &[
    tool!(
        "list_trackers",
        Read,
        Trackers,
        empty_schema(),
        "What this vault records in numbers rather than prose \u{2014} habits, doses, \
         symptoms, counts \u{2014} with what each one's values mean and how often it \
         is meant to happen.",
        run_list_trackers
    ),
    tool!(
        "log_reading",
        Write,
        Trackers,
        schema(
            vec![
                ("tracker_id", text("From list_trackers.")),
                ("value", decimal("The number. For a yes/no tracker, 1 or 0.")),
                ("date", day("The day it belongs to. Defaults to today.")),
                ("note", text("Anything worth noting alongside it.")),
            ],
            &["tracker_id", "value"]
        ),
        "Record one reading against a tracker. The value is clamped to whatever \
         the tracker's scale allows.",
        run_log_reading
    ),
    tool!(
        "tracker_summary",
        Read,
        Trackers,
        schema(
            vec![
                ("tracker_id", text("From list_trackers. Omit for every tracker.")),
                ("from", day("Start of the window, inclusive. Defaults to 30 days back.")),
                ("to", day("End of the window, inclusive. Defaults to today.")),
            ],
            &[]
        ),
        "One row per tracker per day over a window, already aggregated. Use this \
         rather than reading every reading: it is what answers 'how has my sleep \
         been' without pulling a year of rows through the conversation.",
        run_tracker_summary
    ),
    tool!(
        "list_readings",
        Read,
        Trackers,
        schema(
            vec![
                ("tracker_id", text("From list_trackers. Omit for every tracker.")),
                ("from", day("Start of the window, inclusive. Defaults to 30 days back.")),
                ("to", day("End of the window, inclusive. Defaults to today.")),
                ("timed_only", flag("Only readings that know their time of day.")),
                limit_arg(),
            ],
            &[]
        ),
        "Every individual reading over a window, with its day and \u{2014} where it is \
         known \u{2014} its time. Prefer tracker_summary for 'how has my sleep been': \
         this is for questions that need the readings themselves, such as lining up \
         the days one thing happened against what another said the day after.",
        run_list_readings
    ),
];

fn run_list_trackers(ctx: &ToolContext<'_>, _args: &Args<'_>) -> Result<Value> {
    let rows: Vec<Value> = ctx
        .vault
        .trackers()?
        .into_iter()
        .filter(|t| !t.archived)
        .map(|t| {
            json!({
                "id": t.id.to_string(),
                "name": t.name,
                "kind": t.kind.as_str(),
                "unit": t.unit,
                "scale_max": t.scale_max,
                "cadence": t.cadence.map(|c| c.describe()),
            })
        })
        .collect();
    Ok(json!({ "count": rows.len(), "trackers": rows }))
}

fn run_log_reading(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let tracker_id: TrackerId = args.id("tracker_id", "tracker")?;
    let value = args
        .opt_f64("value")
        .ok_or_else(|| args.bad("`value` is required and must be a number"))?;
    let date = args.opt_date("date")?.unwrap_or(ctx.today);

    let mut reading = Reading::on(tracker_id, date, value);
    reading.note = args.opt_str("note").unwrap_or_default().to_string();
    // The vault clamps to the tracker's scale on the way in, which is why
    // the stored value is read back rather than echoed: a model told it
    // logged 99 when the vault stored 10 would report the wrong thing.
    ctx.vault.save_reading(&reading)?;
    let stored = ctx.vault.reading(reading.id)?;

    let name = ctx.vault.tracker(tracker_id).ok().map(|t| t.name);
    Ok(json!({
        "ok": true,
        "action": "recorded",
        "kind": "reading",
        "name": name.unwrap_or_default(),
        "id": reading.id.to_string(),
        "date": date.to_string(),
        "value": stored.value,
    }))
}

fn run_tracker_summary(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let (from, to) = args.window(ctx, Window::Back(30))?;

    let mut query = ReadingQuery::between(from, to);
    if let Some(id) = args.opt_id::<TrackerId>("tracker_id", "tracker")? {
        query.tracker_ids = vec![id];
    }

    let days = ctx.vault.tracker_days(&query)?;

    // A day's one number depends on what the tracker *is*: doses sum, a
    // severity scale takes the worst of the day, a habit counts. Picking one
    // here would make half the charts wrong, so each day is reduced by its
    // own tracker's aggregate -- and the aggregate is named in the reply, so
    // the model can say "3 doses" rather than "3".
    let aggregates: BTreeMap<TrackerId, crate::tracker::Aggregate> =
        ctx.vault.trackers()?.iter().map(|t| (t.id, t.kind.aggregate())).collect();

    Ok(json!({
        "from": from.to_string(),
        "to": to.to_string(),
        "count": days.len(),
        "days": days
            .iter()
            .map(|d| {
                // A tracker that has been deleted still has readings; sum
                // is the least wrong answer for one whose definition is gone.
                let aggregate = aggregates
                    .get(&d.tracker_id)
                    .copied()
                    .unwrap_or(crate::tracker::Aggregate::Sum);
                json!({
                    "tracker_id": d.tracker_id.to_string(),
                    "date": d.date.to_string(),
                    "value": d.value_for(aggregate),
                    "aggregate": format!("{aggregate:?}").to_lowercase(),
                    "readings": d.count,
                })
            })
            .collect::<Vec<_>>(),
    }))
}

fn run_list_readings(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let (from, to) = args.window(ctx, Window::Back(30))?;

    let mut query = ReadingQuery::between(from, to);
    query.limit = Some(args.limit());
    query.timed_only = args.bool_or("timed_only", false);
    if let Some(id) = args.opt_id::<TrackerId>("tracker_id", "tracker")? {
        query.tracker_ids = vec![id];
    }

    // Names, once, so a hundred rows do not each carry one -- and so the
    // model can say "swimming" rather than a uuid.
    let names: BTreeMap<TrackerId, String> =
        ctx.vault.trackers()?.into_iter().map(|t| (t.id, t.name)).collect();

    let rows: Vec<Value> = ctx
        .vault
        .readings(&query)?
        .into_iter()
        .map(|r| {
            let mut row = json!({
                "date": r.local_date.to_string(),
                "value": r.value,
                "tracker": names.get(&r.tracker_id).cloned().unwrap_or_default(),
                "tracker_id": r.tracker_id.to_string(),
            });
            let m = row.as_object_mut().expect("just built an object");
            // The time only when there is one. A reading ticked while
            // writing up yesterday knows its day and nothing about 23:04,
            // and a defaulted instant is the answer that poisons every
            // hour-of-day question anybody asks of this data.
            if let Some(at) = r.at {
                m.insert("at".into(), json!(at.to_string()));
            }
            if !r.note.is_empty() {
                m.insert("note".into(), json!(r.note));
            }
            row
        })
        .collect();

    Ok(json!({
        "from": from.to_string(),
        "to": to.to_string(),
        "count": rows.len(),
        "readings": rows,
    }))
}
