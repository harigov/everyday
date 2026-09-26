//! What a day produced in numbers rather than in prose: habits, doses,
//! symptoms, counts.

use serde_json::{Value, json};
use std::collections::BTreeMap;

use super::{
    Args, Tool, ToolContext, Window, day, decimal, empty_schema, flag, limit_arg, one_of, schema,
    text,
};
use crate::error::Result;
use crate::id::{GoalId, KindId, RoleId, TrackerId};
use crate::purpose::Purpose;
use crate::store::trackers::ReadingQuery;
use crate::tracker::{
    Period, Reading, Standing, Tally, Target, TargetProgress, Tracker, TrackerKind, TrackerSource,
};

const PERIODS: [&str; 5] = ["day", "week", "month", "quarter", "year"];

pub(super) static TOOLS: &[Tool] = &[
    tool!(
        "list_trackers",
        Read,
        Trackers,
        empty_schema(),
        "What this vault records in numbers rather than prose \u{2014} habits, doses, \
         symptoms, counts \u{2014} with what each one's values mean, which goal or role \
         it is filed under, and its targets (e.g. 'at most 60 min a day', 'at least 12 \
         a year'), each measured over its current period: the period's dates, the \
         value so far, whether that is short, within or over, and for a period longer \
         than a day what an even pace would have reached by today. Weeks run Monday \
         to Sunday. A tracker whose source is not 'manual' is worked out from other \
         records \u{2014} 'time' is minutes of logged time filed under its goal or \
         role, 'finished' is library items finished or re-read \u{2014} and cannot be \
         logged to.",
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
         the tracker's scale allows. Only for trackers whose source is 'manual': \
         the others are worked out from time blocks and the library, and are \
         changed by logging time or finishing something there.",
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
         been' without pulling a year of rows through the conversation. Derived \
         trackers ('time', 'finished') are included, one row per day they have \
         anything. For how a target is going this period, list_trackers or \
         list_goals with include_activity already says.",
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
    tool!(
        "set_target",
        Write,
        Trackers,
        schema(
            vec![
                (
                    "tracker_id",
                    text(
                        "An existing tracker to set a target on. From list_trackers. \
                         Omit to make a new one with `measure`."
                    )
                ),
                (
                    "measure",
                    one_of(
                        "When there is no tracker_id, what to count: 'time' is minutes of \
                         logged time filed under the goal or role; 'finished' is things \
                         finished off a shelf (a re-read counts); 'new' makes a tracker \
                         recorded by hand, named by `name`.",
                        &["time", "finished", "new"]
                    )
                ),
                ("goal_id", text("File the tracker under this goal. From list_goals.")),
                ("role_id", text("Or under this role directly. From list_roles.")),
                (
                    "shelf_id",
                    text("For 'finished': which shelf. From list_shelves. Omit for every shelf.")
                ),
                ("name", text("For 'new': what is being recorded, e.g. 'TV'.")),
                ("unit", text("For 'new': the unit, e.g. 'min', 'pages'. Empty for a count.")),
                (
                    "kind",
                    one_of(
                        "For 'new': 'check' for a yes/no habit, 'amount' for a quantity \
                         (the default), 'dose' for medication.",
                        &["check", "amount", "dose"]
                    )
                ),
                ("per", one_of("The calendar period the target is counted over.", &PERIODS)),
                ("min", decimal("At least this much per period. In the tracker's unit.")),
                ("max", decimal("At most this much per period. In the tracker's unit.")),
                (
                    "count_days",
                    flag(
                        "Count days with something recorded rather than adding values up \
                         \u{2014} 'run 3 times a week' is min 3, per week, count_days."
                    )
                ),
                (
                    "remove",
                    flag("Remove the tracker's target for this period instead of setting it.")
                ),
            ],
            &["per"]
        ),
        "Say how much of something is wanted per day, week, month, quarter or year, \
         and so how a goal is measured: 'between 1 and 2 hours of piano a week' is \
         measure 'time', goal_id of the piano goal, per week, min 60, max 120; \
         'twelve books this year' is measure 'finished' on the books shelf, per year, \
         min 12; 'no more than an hour of TV a day' is measure 'new', name 'TV', unit \
         'min', per day, max 60. Time is always in minutes. A tracker holds one target \
         per period and tally, so setting one again replaces it \u{2014} and a measure \
         that already has a tracker (time on the same goal, the same shelf, a tracker \
         of the same name) reuses it rather than making another. Give min, max or \
         both. Passing goal_id or role_id with an existing tracker also files it there.",
        run_set_target
    ),
];

fn run_list_trackers(ctx: &ToolContext<'_>, _args: &Args<'_>) -> Result<Value> {
    let live: Vec<Tracker> = ctx.vault.trackers()?.into_iter().filter(|t| !t.archived).collect();
    let measured = ctx.vault.target_progress(&live, ctx.today)?;
    let rows: Vec<Value> = live
        .iter()
        .map(|t| {
            let mut row = json!({
                "id": t.id.to_string(),
                "name": t.name,
                "kind": t.kind.as_str(),
                "unit": t.unit,
                "scale_max": t.scale_max,
                "source": t.source.as_str(),
            });
            let m = row.as_object_mut().expect("just built an object");
            if let Some(label) = filed_under(ctx, t.purpose) {
                m.insert("filed_under".into(), json!(label));
            }
            if let Some((_, each)) = measured.iter().find(|(m, _)| m.id == t.id) {
                m.insert(
                    "targets".into(),
                    each.iter().map(|(g, p)| progress_json(t, g, p)).collect(),
                );
            }
            row
        })
        .collect();
    Ok(json!({ "count": rows.len(), "trackers": rows }))
}

/// The goal's title or the role's name a tracker is filed under.
fn filed_under(ctx: &ToolContext<'_>, purpose: Option<Purpose>) -> Option<String> {
    match purpose? {
        Purpose::Goal { id } => ctx.vault.goal(id).ok().map(|g| format!("goal: {}", g.title)),
        Purpose::Role { id } => ctx.vault.role(id).ok().map(|r| format!("role: {}", r.name)),
    }
}

/// One target and how its current period stands, the way every tool that
/// reports a target says it: the sentence, the dates counted, the number so
/// far, and -- for a period longer than a day -- what an even pace would
/// have reached. Numbers are rounded so a model does not read 0.1 + 0.2.
pub(super) fn progress_json(tracker: &Tracker, target: &Target, p: &TargetProgress) -> Value {
    let round = |v: f64| (v * 10.0).round() / 10.0;
    let mut out = json!({
        "target": target.describe(&tracker.unit),
        "from": p.from.to_string(),
        "to": p.to.to_string(),
        "so_far": round(p.value),
        "standing": match p.standing {
            Standing::Short => "short",
            Standing::Within => "within",
            Standing::Over => "over",
        },
    });
    if let Some(pace) = p.on_pace {
        out.as_object_mut()
            .expect("just built an object")
            .insert("on_pace".into(), json!(round(pace)));
    }
    out
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

fn run_set_target(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let per: Period = args
        .opt_enum("per", &PERIODS)?
        .ok_or_else(|| args.bad("`per` is required: day, week, month, quarter or year"))?;
    let remove = args.bool_or("remove", false);
    let count_days = args.bool_or("count_days", false);
    let (min, max) = (args.opt_f64("min"), args.opt_f64("max"));
    if !remove && min.is_none() && max.is_none() {
        return Err(args.bad("give `min`, `max` or both, or `remove`"));
    }

    let goal_id = args.opt_id::<GoalId>("goal_id", "goal")?;
    let role_id = args.opt_id::<RoleId>("role_id", "role")?;
    if goal_id.is_some() && role_id.is_some() {
        return Err(args.bad("pass at most one of `goal_id` and `role_id`"));
    }
    // Checked before anything is written, so a bad id is a refusal rather
    // than a tracker filed under nothing.
    let purpose = match (goal_id, role_id) {
        (Some(id), _) => Some(ctx.vault.goal(id).map(|_| Purpose::Goal { id })?),
        (_, Some(id)) => Some(ctx.vault.role(id).map(|_| Purpose::Role { id })?),
        _ => None,
    };

    let mut tracker = match args.opt_id::<TrackerId>("tracker_id", "tracker")? {
        Some(id) => {
            if args.opt_str("measure").is_some() {
                return Err(args.bad("pass `tracker_id` or `measure`, not both"));
            }
            let mut t = ctx.vault.tracker(id)?;
            if purpose.is_some() {
                t.purpose = purpose;
            }
            t
        }
        // The tracker this measure would make, if it has been made already:
        // saying "an hour of piano a week" twice is changing one target,
        // not starting a second tracker counting the same hours.
        None => match existing_for(ctx, args, purpose)? {
            Some(mut t) => {
                if purpose.is_some() {
                    t.purpose = purpose;
                }
                t
            }
            None if remove => {
                return Err(
                    args.bad("there is no tracker for that measure to remove a target from")
                );
            }
            None => new_tracker(ctx, args, purpose)?,
        },
    };

    let tally =
        if count_days || tracker.kind == TrackerKind::Check { Tally::Days } else { Tally::Value };
    // One target per period and tally: saying it again is changing it.
    tracker.targets.retain(|g| !(g.per == per && g.tally == tally));
    if !remove {
        let mut target = Target { min, max, per, tally };
        if !target.normalize() {
            return Err(args.bad("`min` and `max` must be numbers, and not both zero or empty"));
        }
        tracker.targets.push(target);
    }
    ctx.vault.save_tracker(&tracker)?;
    let saved = ctx.vault.tracker(tracker.id)?;

    Ok(json!({
        "ok": true,
        "action": if remove { "removed target" } else { "set target" },
        "kind": "tracker",
        "name": saved.name,
        "id": saved.id.to_string(),
        "source": saved.source.as_str(),
        "filed_under": filed_under(ctx, saved.purpose),
        "targets": saved.targets.iter().map(|g| g.describe(&saved.unit)).collect::<Vec<_>>(),
    }))
}

/// The live tracker a `measure` names, if there already is one.
///
/// Time is one per goal or role; finished is one per shelf and filing; a new
/// tracker is one per name, among the ones recorded by hand. Anything else
/// -- a measure with nothing to match on -- answers `None`, and
/// [`new_tracker`] is where it is refused.
fn existing_for(
    ctx: &ToolContext<'_>,
    args: &Args<'_>,
    purpose: Option<Purpose>,
) -> Result<Option<Tracker>> {
    let Some(measure) = args.opt_str("measure").map(|m| m.trim().to_lowercase()) else {
        return Ok(None);
    };
    let live = ctx.vault.trackers()?.into_iter().filter(|t| !t.archived);
    let found = match measure.as_str() {
        "time" if purpose.is_some() => {
            live.into_iter().find(|t| t.source == TrackerSource::Time && t.purpose == purpose)
        }
        "finished" => {
            let wanted = TrackerSource::Finished { kind_id: args.opt_id("shelf_id", "shelf")? };
            live.into_iter().find(|t| t.source == wanted && t.purpose == purpose)
        }
        "new" => {
            let name = args.opt_str("name").map(str::trim).unwrap_or_default();
            live.into_iter()
                .find(|t| t.is_manual() && !name.is_empty() && t.name.eq_ignore_ascii_case(name))
        }
        _ => None,
    };
    Ok(found)
}

/// The tracker `set_target` makes when it is not given one.
fn new_tracker(
    ctx: &ToolContext<'_>,
    args: &Args<'_>,
    purpose: Option<Purpose>,
) -> Result<Tracker> {
    let measure = args
        .opt_str("measure")
        .ok_or_else(|| args.bad("pass `tracker_id`, or `measure` to make a new one"))?;
    let named = |p: Option<Purpose>| -> Option<String> {
        match p? {
            Purpose::Goal { id } => ctx.vault.goal(id).ok().map(|g| g.title),
            Purpose::Role { id } => ctx.vault.role(id).ok().map(|r| r.name),
        }
    };
    let mut tracker = match measure.trim().to_lowercase().as_str() {
        "time" => {
            // Time on nothing is not a measure of anything.
            let Some(on) = named(purpose) else {
                return Err(args.bad(
                    "'time' counts what is filed under a goal or role: pass `goal_id` or `role_id`",
                ));
            };
            let mut t = Tracker::new(format!("Time on {on}"), TrackerKind::Amount);
            t.source = TrackerSource::Time;
            t.icon = "clock".into();
            t
        }
        "finished" => {
            let kind_id = args.opt_id::<KindId>("shelf_id", "shelf")?;
            let name = match kind_id {
                Some(id) => {
                    let shelf = ctx.vault.kind(id)?;
                    format!("{}: {}", shelf.name, shelf.verbs.done.to_lowercase())
                }
                None => "Things finished".to_string(),
            };
            let mut t = Tracker::new(name, TrackerKind::Amount);
            t.source = TrackerSource::Finished { kind_id };
            t.icon = "trophy".into();
            t
        }
        "new" => {
            let name = args.opt_str("name").map(str::trim).unwrap_or_default();
            if name.is_empty() {
                return Err(args.bad("'new' needs a `name`"));
            }
            let kind = match args.opt_str("kind").map(|k| k.trim().to_lowercase()).as_deref() {
                Some("check") => TrackerKind::Check,
                Some("dose") => TrackerKind::Dose,
                _ => TrackerKind::Amount,
            };
            let mut t = Tracker::new(name, kind);
            t.unit = args.opt_str("unit").unwrap_or_default().trim().to_string();
            t.icon = "target".into();
            t
        }
        other => {
            return Err(args.bad(format!("`measure` must be time, finished or new, not {other:?}")));
        }
    };
    tracker.purpose = purpose;
    Ok(tracker)
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
