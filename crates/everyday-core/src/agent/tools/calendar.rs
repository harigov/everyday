//! Putting events on a calendar, and changing them there.
//!
//! [`super::time`] reads events and writes *time blocks* -- an hour set
//! aside for a task, or recorded against one. This file is the other kind of
//! appointment: an event somebody asked for by name, with a time, perhaps a
//! repeat, perhaps guests. Where it goes is the person's choice, made once,
//! not each time: [`crate::vault::Vault::default_calendar`] is one of an
//! account's calendars or, when nothing is chosen, this computer -- where
//! the event becomes their own planned time, a block like any other. A call
//! that names a calendar overrides that, and `list_calendars` is how the
//! model learns the names.
//!
//! An account's calendar is reached through [`ToolContext::calendar_writer`],
//! the same functions a person's own click uses -- see [`CalendarWriter`] --
//! and the server does what servers do: it emails every guest. So a call
//! with guests on it is [`Effect::Outward`](super::Effect::Outward) for that
//! call alone (see [`Tool::outward_when`]), confirmed in chat like a sent
//! message and refused outright on a scheduled run with nobody to ask. A call
//! without guests changes only the person's own calendar and runs at once,
//! the way archiving a message does.
//!
//! None of these four can be proposed while drafting: an event on Google is
//! not a record this vault holds, so there is nothing for a proposal to save
//! when it is accepted. A dream that wants to suggest time uses
//! `create_time_block`, which can.

use serde_json::{Value, json};

use super::{Args, CalendarWriter, Tool, ToolContext, day, flag, list, one_of, schema, text};
use crate::calendar::{
    Attendee, Calendar, CalendarAccess, CalendarOrigin, EditableEvent, EventDraft, EventScope,
};
use crate::error::{Error, Result};
use crate::id::{CalendarId, EventId};
use crate::recurrence::{Frequency, Recurrence, Weekday};
use crate::task::{BlockSubject, TimeBlock};

/// What `calendar_id` is called when it means this computer rather than an
/// account's calendar.
const THIS_COMPUTER: &str = "this_computer";

pub(super) static TOOLS: &[Tool] = &[
    tool!(
        "list_calendars",
        Read,
        Calendars,
        schema(vec![], &[]),
        "Every calendar here and whether new events can go on it: this computer \
         (the person's own planned time), each signed-in account's calendars (Google, \
         Microsoft, iCloud and the like), and subscribed feeds, which are read-only. Says \
         which one new events go to by default. Call it when the person names a calendar, \
         to find its id for create_event.",
        run_list_calendars
    ),
    Tool {
        outward_when: Some(invites_anybody),
        ..tool!(
            "create_event",
            Write,
            Calendars,
            schema(
                vec![
                    ("title", text("What the event is called.")),
                    ("date", day("The day it starts.")),
                    (
                        "start_time",
                        text("Start, as HH:MM in 24-hour time. Required unless all_day.")
                    ),
                    ("end_time", text("End, as HH:MM. Defaults to an hour after start_time.")),
                    ("all_day", flag("A whole-day event, with no times.")),
                    ("end_date", day("The last day of an all-day event that runs several days.")),
                    (
                        "calendar_id",
                        text(
                            "Which calendar, from list_calendars: an id, or this_computer. \
                             Leave it out to use the default calendar -- which is what the \
                             person means unless they name another."
                        )
                    ),
                    ("description", text("Notes or an agenda, shown with the event.")),
                    ("location", text("Where it is: a place, a room, or a call link.")),
                    (
                        "attendees",
                        list(
                            "Email addresses of people to invite. The calendar's server emails \
                             each of them an invitation as soon as this runs, so only include \
                             people the person asked to invite. Only possible on an account's \
                             calendar, not on this computer."
                        )
                    ),
                    ("repeat", repeat_schema("How the event repeats. Leave it out for a one-off.")),
                ],
                &["title", "date"]
            ),
            "Put an event on a calendar -- the default one unless the person names another \
             (see list_calendars). For an appointment, a meeting, or anything they call an \
             event, use this rather than create_time_block, which is for time set aside for \
             a task. On this computer it becomes their own planned time; on an account's \
             calendar it is created on that account's server, with any guests invited by it. \
             Give `repeat` for a recurring event. A call with attendees is confirmed with the \
             person first, because invitations cannot be unsent.",
            run_create_event,
            Some(describe_create_event)
        )
    },
    Tool {
        outward_when: Some(notifies_anybody),
        ..tool!(
            "update_event",
            Write,
            Calendars,
            schema(
                vec![
                    ("event_id", text("The event, from list_events.")),
                    (
                        "scope",
                        one_of(
                            "For a repeating event: occurrence changes only this one, series \
                             changes every one. Defaults to occurrence. Changing how it repeats \
                             always means series.",
                            &["occurrence", "series"]
                        )
                    ),
                    ("title", text("A new title.")),
                    ("date", day("Move it to this day.")),
                    ("start_time", text("New start, as HH:MM in 24-hour time.")),
                    ("end_time", text("New end, as HH:MM.")),
                    ("description", text("Replace the description with this.")),
                    ("location", text("Replace the location with this.")),
                    (
                        "add_attendees",
                        list(
                            "Email addresses to invite. Each is emailed an invitation by the \
                             calendar's server."
                        )
                    ),
                    (
                        "remove_attendees",
                        list("Email addresses to take off the guest list. They are told.")
                    ),
                    (
                        "repeat",
                        repeat_schema("Make it repeat like this, replacing any rule it had.")
                    ),
                    ("stop_repeating", flag("Make a repeating event a one-off.")),
                ],
                &["event_id"]
            ),
            "Change an event on an account's calendar: its title, time, description, \
             location, guests or how it repeats. Only names what changes; everything left out \
             stays as it is. Only events the person organised can be changed. Guests hear about \
             the change from the calendar's server, so a change to an event with guests is \
             confirmed with the person first. For your own planned time, use \
             update_time_block instead.",
            run_update_event,
            Some(describe_update_event)
        )
    },
    Tool {
        outward_when: Some(cancels_on_anybody),
        ..tool!(
            "delete_event",
            Destructive,
            Calendars,
            schema(
                vec![
                    ("event_id", text("The event, from list_events.")),
                    (
                        "scope",
                        one_of(
                            "For a repeating event: occurrence deletes only this one, series \
                             deletes every one. Defaults to occurrence.",
                            &["occurrence", "series"]
                        )
                    ),
                ],
                &["event_id"]
            ),
            "Delete an event from an account's calendar -- one occurrence, or a whole \
             repeating series. Guests are sent a cancellation by the calendar's server. For \
             your own planned time, use delete_time_block instead.",
            run_delete_event,
            Some(describe_delete_event)
        )
    },
];

/// The `repeat` argument's schema, shared by `create_event` and
/// `update_event`. Spelled out as an object rather than an `RRULE` string,
/// because a model asked for `{"frequency": "weekly", "weekdays":
/// ["monday"]}` gets it right far more often than one asked to write
/// `BYDAY=MO` -- see [`crate::recurrence`].
fn repeat_schema(desc: &str) -> Value {
    json!({
        "type": "object",
        "description": desc,
        "properties": {
            "frequency": {
                "type": "string",
                "enum": ["daily", "weekly", "monthly", "yearly"],
                "description": "How often it comes round.",
            },
            "interval": {
                "type": "integer",
                "description": "Every how many: 2 with weekly is every other week. Defaults to 1.",
            },
            "weekdays": {
                "type": "array",
                "items": {
                    "type": "string",
                    "enum": [
                        "monday", "tuesday", "wednesday", "thursday", "friday", "saturday",
                        "sunday",
                    ],
                },
                "description": "Weekly: the days it falls on (leave out for the start's own \
                    day; monday to friday for every weekday). Monthly with week_of_month: the \
                    one day.",
            },
            "week_of_month": {
                "type": "integer",
                "description": "Monthly only: 1 to 4, or -1 for the last -- as in 'the second \
                    Tuesday' (2, with weekdays [tuesday]). Leave out for the same date every \
                    month.",
            },
            "count": {
                "type": "integer",
                "description": "Stop after this many times. Leave out, with until, to repeat \
                    indefinitely.",
            },
            "until": {
                "type": "string",
                "description": "The last day it may happen. Format: YYYY-MM-DD.",
            },
        },
        "required": ["frequency"],
        "additionalProperties": false,
    })
}

// ---- where an event goes -------------------------------------------------

/// Where a new event is going: this computer, or one of an account's
/// calendars that can take it.
enum Target {
    ThisComputer,
    Account(Box<Calendar>),
}

impl Target {
    fn name(&self) -> String {
        match self {
            Target::ThisComputer => "this computer".to_string(),
            Target::Account(calendar) => calendar.name.clone(),
        }
    }
}

/// Read `calendar_id`, or fall back to the default calendar. Refuses, with
/// what to do instead, a calendar that cannot take events.
fn target(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Target> {
    let Some(raw) = args.opt_str("calendar_id") else {
        return Ok(match ctx.vault.default_calendar()? {
            Some(calendar) => Target::Account(Box::new(calendar)),
            None => Target::ThisComputer,
        });
    };
    if is_this_computer(raw) {
        return Ok(Target::ThisComputer);
    }
    let id: CalendarId = args.id("calendar_id", "calendar")?;
    let calendar = ctx.vault.calendar(id).map_err(|_| {
        Error::Invalid(format!(
            "no calendar with id {id}. Call list_calendars and use an id from it."
        ))
    })?;
    match access(ctx, &calendar) {
        CalendarAccess::Writable => Ok(Target::Account(Box::new(calendar))),
        CalendarAccess::NeedsSignIn => Err(Error::Invalid(format!(
            "{} cannot take new events until its account is signed in again with permission \
             to change calendars (Settings \u{2192} Accounts). Say so, or use another calendar.",
            calendar.name
        ))),
        CalendarAccess::ReadOnly => Err(Error::Invalid(format!(
            "{} is read-only. Pick a calendar list_calendars says can take events, or \
             this_computer.",
            calendar.name
        ))),
    }
}

fn is_this_computer(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().replace(' ', "_").as_str(),
        THIS_COMPUTER | "local" | "computer"
    )
}

/// Whether new events can go on `calendar` -- the same answer the calendar
/// picker gives, worked out the same way.
fn access(ctx: &ToolContext<'_>, calendar: &Calendar) -> CalendarAccess {
    let CalendarOrigin::Account { account_id, .. } = &calendar.origin else {
        return CalendarAccess::ReadOnly;
    };
    if calendar.read_only {
        return CalendarAccess::ReadOnly;
    }
    match ctx.vault.account(*account_id) {
        Ok(account) if account.can_write_calendars() => CalendarAccess::Writable,
        Ok(_) => CalendarAccess::NeedsSignIn,
        Err(_) => CalendarAccess::ReadOnly,
    }
}

fn writer<'a>(ctx: &ToolContext<'a>) -> Result<&'a dyn CalendarWriter> {
    ctx.calendar_writer
        .ok_or(Error::Unsupported("changing an account's calendar is not available right now"))
}

// ---- list_calendars ------------------------------------------------------

fn run_list_calendars(ctx: &ToolContext<'_>, _args: &Args<'_>) -> Result<Value> {
    let calendars = ctx.vault.calendars()?;
    let default =
        calendars.iter().find(|c| c.is_default && access(ctx, c) == CalendarAccess::Writable);
    let mut rows = vec![json!({
        "id": THIS_COMPUTER,
        "name": "This computer",
        "kind": "local",
        "can_add_events": true,
        "default": default.is_none(),
        "note": "The person's own planned time. Cannot invite guests.",
    })];
    for calendar in &calendars {
        let access = access(ctx, calendar);
        let (kind, account) = match &calendar.origin {
            CalendarOrigin::Account { account_id, .. } => {
                ("account", ctx.vault.account(*account_id).ok().map(|a| a.address))
            }
            CalendarOrigin::Url { .. } => ("subscribed feed", None),
            CalendarOrigin::File { .. } => ("imported file", None),
        };
        let mut row = json!({
            "id": calendar.id.to_string(),
            "name": calendar.name,
            "kind": kind,
            "can_add_events": access == CalendarAccess::Writable,
            "default": default.is_some_and(|d| d.id == calendar.id),
            "shown": calendar.visible,
        });
        if let Some(address) = account {
            row["account"] = json!(address);
        }
        match access {
            CalendarAccess::NeedsSignIn => {
                row["note"] = json!(
                    "Its account was signed in to read calendars only; it needs signing in \
                     again to take new events."
                );
            }
            CalendarAccess::ReadOnly if kind == "account" => {
                row["note"] = json!("Its server does not let this account change it.");
            }
            _ => {}
        }
        rows.push(row);
    }
    Ok(json!({
        "default": default.map_or(THIS_COMPUTER.to_string(), |c| c.id.to_string()),
        "calendars": rows,
    }))
}

// ---- reading what a call asks for ----------------------------------------

/// `repeat`, read into a [`Recurrence`] -- leniently about spelling
/// (`"Mon"`, `"MO"`), strictly about meaning.
fn repeat_from_args(args: &Args<'_>, key: &str) -> Result<Option<Recurrence>> {
    let Some(raw) = args.get(key) else { return Ok(None) };
    let Some(object) = raw.as_object() else {
        return Err(
            args.bad(format!("`{key}` must be an object like {{\"frequency\": \"weekly\"}}"))
        );
    };
    let frequency = object
        .get("frequency")
        .and_then(Value::as_str)
        .and_then(|f| Frequency::parse(&f.trim().to_ascii_lowercase()))
        .ok_or_else(|| {
            args.bad(format!("`{key}.frequency` must be one of daily, weekly, monthly, yearly"))
        })?;
    let number = |name: &str| -> Option<i64> {
        let v = object.get(name).filter(|v| !v.is_null())?;
        v.as_i64().or_else(|| v.as_str()?.trim().parse().ok())
    };
    let mut weekdays = Vec::new();
    match object.get("weekdays") {
        Some(Value::Array(days)) => {
            for day in days {
                let name = day.as_str().unwrap_or_default();
                let parsed = Weekday::parse(name).ok_or_else(|| {
                    args.bad(format!("`{key}.weekdays` has {name:?}, which is not a weekday"))
                })?;
                weekdays.push(parsed);
            }
        }
        Some(Value::String(days)) => {
            for name in days.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let parsed = Weekday::parse(name).ok_or_else(|| {
                    args.bad(format!("`{key}.weekdays` has {name:?}, which is not a weekday"))
                })?;
                weekdays.push(parsed);
            }
        }
        _ => {}
    }
    let until = match object.get("until").and_then(Value::as_str).map(str::trim) {
        Some(raw) if !raw.is_empty() => Some(raw.parse::<jiff::civil::Date>().map_err(|_| {
            args.bad(format!("`{key}.until` must be a date like 2026-12-31, got {raw:?}"))
        })?),
        _ => None,
    };
    let rule = Recurrence {
        frequency,
        interval: number("interval").map_or(1, |n| n.clamp(1, 999) as u32),
        weekdays,
        week_of_month: number("week_of_month").map(|n| n.clamp(-1, 5) as i8),
        count: number("count").map(|n| n.clamp(0, i64::from(u32::MAX)) as u32),
        until,
    };
    rule.validate().map_err(|e| args.bad(format!("`{key}`: {e}")))?;
    Ok(Some(rule))
}

fn clock(args: &Args<'_>, key: &str) -> Result<Option<jiff::civil::Time>> {
    args.opt_time(key)
}

/// A wall-clock moment on `date` in `tz`, as an instant. A time skipped by a
/// change of clocks resolves to the later side of the gap, the way every
/// calendar app does.
fn at(tz: &str, date: jiff::civil::Date, time: jiff::civil::Time) -> Result<jiff::Timestamp> {
    let zone = jiff::tz::TimeZone::get(tz).unwrap_or(jiff::tz::TimeZone::UTC);
    zone.to_ambiguous_zoned(date.to_datetime(time))
        .later()
        .map(|z| z.timestamp())
        .map_err(|e| Error::Invalid(format!("{date} {time} does not exist in {tz}: {e}")))
}

/// The emails in `key`, as attendees, refusing anything that is not one.
fn attendees_from(args: &Args<'_>, key: &str) -> Result<Vec<Attendee>> {
    let mut out = Vec::new();
    for raw in args.strings(key) {
        // "Sam Lee <sam@example.com>" is how people paste an address.
        let (name, email) = match (raw.rfind('<'), raw.rfind('>')) {
            (Some(open), Some(close)) if open < close => {
                (raw[..open].trim().trim_matches('"').to_string(), raw[open + 1..close].trim())
            }
            _ => (String::new(), raw.trim()),
        };
        let guest = Attendee { email: email.to_string(), name, response: None };
        if !guest.looks_valid() {
            return Err(args.bad(format!(
                "`{key}` has {raw:?}, which is not an email address. Ask the person for it if \
                 you do not have it."
            )));
        }
        out.push(guest);
    }
    Ok(out)
}

/// Everything `create_event` asks for, as a draft, plus where it goes.
fn draft_from_args(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<(EventDraft, Target)> {
    let title = args.str("title")?.trim().to_string();
    let date = args.date("date")?;
    let all_day = args.bool_or("all_day", false);
    let (start, end) = if all_day {
        let last = args.opt_date("end_date")?.unwrap_or(date);
        if last < date {
            return Err(args.bad("`end_date` is before `date`"));
        }
        let after = last.tomorrow().map_err(|e| Error::Invalid(e.to_string()))?;
        (
            at(ctx.tz, date, jiff::civil::Time::midnight())?,
            at(ctx.tz, after, jiff::civil::Time::midnight())?,
        )
    } else {
        let start_time = clock(args, "start_time")?
            .ok_or_else(|| args.bad("`start_time` is required unless the event is all_day"))?;
        let start = at(ctx.tz, date, start_time)?;
        let end = match clock(args, "end_time")? {
            Some(end_time) => at(ctx.tz, date, end_time)?,
            None => start + jiff::SignedDuration::from_hours(1),
        };
        if end <= start {
            return Err(args.bad("`end_time` must be after `start_time`"));
        }
        (start, end)
    };
    let mut draft = EventDraft {
        title,
        description: args.opt_str("description").unwrap_or_default().trim().to_string(),
        location: args.opt_str("location").unwrap_or_default().trim().to_string(),
        start,
        end,
        all_day,
        tz: ctx.tz.to_string(),
        attendees: attendees_from(args, "attendees")?,
        recurrence: repeat_from_args(args, "repeat")?,
    };
    draft.validate().map_err(|e| args.bad(e))?;
    Ok((draft, target(ctx, args)?))
}

/// "Tue 7 Oct, 09:00–10:00" or "Tue 7 Oct, all day", in the draft's own
/// zone -- how a confirmation card and a reply say when.
fn when(draft: &EventDraft) -> String {
    let start = draft.local_start();
    let day = start.date().strftime("%a %-d %b").to_string();
    if draft.all_day {
        let last = draft.end_date_exclusive().yesterday().unwrap_or(draft.start_date());
        if last > start.date() {
            return format!("{day} to {}, all day", last.strftime("%a %-d %b"));
        }
        return format!("{day}, all day");
    }
    let end = draft.local_end();
    format!(
        "{day}, {:02}:{:02}\u{2013}{:02}:{:02}",
        start.hour(),
        start.minute(),
        end.hour(),
        end.minute()
    )
}

// ---- create_event ----------------------------------------------------------

/// Guests on a call mean invitations sent -- see [`Tool::outward_when`].
fn invites_anybody(_ctx: &ToolContext<'_>, args: &Args<'_>) -> bool {
    !args.strings("attendees").is_empty()
}

fn describe_create_event(ctx: &ToolContext<'_>, args: &Args<'_>) -> Option<String> {
    let (draft, target) = draft_from_args(ctx, args).ok()?;
    let mut out =
        format!("Create \u{201c}{}\u{201d} on {}, {}", draft.title, target.name(), when(&draft));
    if let Some(rule) = &draft.recurrence {
        out.push_str(&format!(", repeating {}", rule.describe()));
    }
    if !draft.attendees.is_empty() {
        let guests: Vec<&str> = draft.attendees.iter().map(|a| a.email.as_str()).collect();
        out.push_str(&format!(", and email an invitation to {}", guests.join(", ")));
    }
    Some(out)
}

fn run_create_event(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let (draft, target) = draft_from_args(ctx, args)?;
    if !draft.attendees.is_empty() && ctx.unattended {
        return Err(Error::Invalid(
            "a scheduled run may not send invitations. Create the event without guests, and \
             say in your reply who still needs inviting."
                .into(),
        ));
    }
    let repeats = draft.recurrence.as_ref().map(Recurrence::describe);
    match target {
        Target::ThisComputer => {
            if !draft.attendees.is_empty() {
                return Err(args.bad(
                    "guests can only be invited to an event on an account's calendar. Use \
                     list_calendars to find one, or leave the guests out.",
                ));
            }
            let minutes = draft.minutes();
            let mut block = TimeBlock::new(BlockSubject::Adhoc, draft.start, minutes, &draft.tz);
            // A block has no location of its own, so where it is rides in its
            // title -- "Dentist — High Street" -- which is where the command
            // bar and the calendar's own editor put one too.
            block.title = if draft.location.is_empty() {
                draft.title.clone()
            } else {
                format!("{} \u{2014} {}", draft.title, draft.location)
            };
            block.all_day = draft.all_day;
            block.notes = draft.description.clone();
            let written = match &draft.recurrence {
                Some(rule) => ctx.vault.save_block_series(&block, Some(rule))?,
                None => {
                    ctx.vault.save_block(&block)?;
                    vec![block.clone()]
                }
            };
            Ok(json!({
                "ok": true,
                "action": "created",
                "kind": "time block",
                "name": draft.title,
                "id": block.id.to_string(),
                "calendar": "this computer",
                "when": when(&draft),
                "repeats": repeats,
                "occurrences_written": written.len(),
            }))
        }
        Target::Account(calendar) => {
            let created = writer(ctx)?.create(calendar.id, &draft)?;
            Ok(json!({
                "ok": true,
                "action": "created",
                "kind": "event",
                "name": draft.title,
                "id": created.map(|e| e.id.to_string()),
                "calendar": calendar.name,
                "when": when(&draft),
                "repeats": repeats,
                "invited": draft.attendees.iter().map(|a| a.email.clone()).collect::<Vec<_>>(),
            }))
        }
    }
}

// ---- update_event ----------------------------------------------------------

fn event_id(args: &Args<'_>) -> Result<EventId> {
    args.id("event_id", "event")
}

fn scope_from(args: &Args<'_>) -> Result<EventScope> {
    Ok(args
        .opt_enum::<EventScope>("scope", &["occurrence", "series"])?
        .unwrap_or(EventScope::Occurrence))
}

/// Changing an event that has guests tells them -- and so does adding or
/// removing one.
fn notifies_anybody(ctx: &ToolContext<'_>, args: &Args<'_>) -> bool {
    !args.strings("add_attendees").is_empty()
        || !args.strings("remove_attendees").is_empty()
        || has_guests(ctx, args)
}

/// Deleting an event with guests sends each of them a cancellation.
fn cancels_on_anybody(ctx: &ToolContext<'_>, args: &Args<'_>) -> bool {
    has_guests(ctx, args)
}

/// Whether the stored copy of the event names anybody besides its
/// organiser. Read from the vault rather than the server, because a gate
/// has to answer without a network round trip; a guest list that changed
/// since the last sync errs towards asking, never towards not asking,
/// because an unknown event reads as having guests.
fn has_guests(ctx: &ToolContext<'_>, args: &Args<'_>) -> bool {
    let Ok(id) = event_id(args) else { return true };
    match ctx.vault.event(id) {
        Ok(event) => !event.attendees.is_empty(),
        Err(_) => true,
    }
}

/// The event as its server has it now, refusing one the person cannot
/// change.
fn editable(ctx: &ToolContext<'_>, id: EventId) -> Result<EditableEvent> {
    let editable = writer(ctx)?.load(id)?;
    if !editable.own {
        let who = if editable.organizer.is_empty() {
            "whoever organised it".to_string()
        } else {
            editable.organizer.clone()
        };
        return Err(Error::Invalid(format!(
            "this event was organised by {who}, so only they can change it. To answer the \
             invitation, use respond_to_invite on the message that brought it."
        )));
    }
    Ok(editable)
}

/// Apply every change `update_event` names to `editable`'s draft, and work
/// out which scope it really needs.
fn apply_update(
    ctx: &ToolContext<'_>,
    args: &Args<'_>,
    editable: &EditableEvent,
) -> Result<(EventDraft, EventScope)> {
    let mut draft = editable.draft.clone();
    let mut scope = scope_from(args)?;
    if let Some(title) = args.opt_str("title") {
        draft.title = title.trim().to_string();
    }
    if args.has_key("description") {
        draft.description = args.opt_str("description").unwrap_or_default().trim().to_string();
    }
    if args.has_key("location") {
        draft.location = args.opt_str("location").unwrap_or_default().trim().to_string();
    }

    // A new day, start or end is read in the person's own zone, and any of
    // the three left unsaid is read back off the event as it stands.
    let new_date = args.opt_date("date")?;
    let new_start = clock(args, "start_time")?;
    let new_end = clock(args, "end_time")?;
    if new_date.is_some() || new_start.is_some() || new_end.is_some() {
        let zone = jiff::tz::TimeZone::get(ctx.tz).unwrap_or(jiff::tz::TimeZone::UTC);
        let old_start = draft.start.to_zoned(zone.clone());
        let old_end = draft.end.to_zoned(zone);
        let length = draft.end.duration_since(draft.start);
        let date = new_date.unwrap_or(old_start.date());
        if draft.all_day {
            let days = old_end.date().since(old_start.date()).map(|s| s.get_days()).unwrap_or(1);
            let after = date
                .checked_add(jiff::Span::new().days(i64::from(days.max(1))))
                .map_err(|e| Error::Invalid(e.to_string()))?;
            draft.start = at(ctx.tz, date, jiff::civil::Time::midnight())?;
            draft.end = at(ctx.tz, after, jiff::civil::Time::midnight())?;
        } else {
            let start = at(ctx.tz, date, new_start.unwrap_or(old_start.time()))?;
            let end = match new_end {
                Some(end) => at(ctx.tz, date, end)?,
                // Moving an event keeps its length unless the end is named.
                None => start + length,
            };
            if end <= start {
                return Err(args.bad("the event would end before it starts"));
            }
            draft.start = start;
            draft.end = end;
        }
    }

    let removing: Vec<String> =
        args.strings("remove_attendees").iter().map(|e| e.trim().to_ascii_lowercase()).collect();
    let unknown: Vec<&str> = removing
        .iter()
        .filter(|r| !draft.attendees.iter().any(|a| a.key() == **r))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        // Said rather than ignored: a model that thinks it removed somebody
        // who was never there has the wrong guest list in mind.
        return Err(args.bad(format!(
            "{} {} not on the guest list",
            unknown.join(", "),
            if unknown.len() == 1 { "is" } else { "are" }
        )));
    }
    draft.attendees.retain(|a| !removing.contains(&a.key()));
    for guest in attendees_from(args, "add_attendees")? {
        if !draft.attendees.iter().any(|a| a.key() == guest.key()) {
            draft.attendees.push(guest);
        }
    }

    let changes_rule = args.has_key("repeat") || args.bool_or("stop_repeating", false);
    if changes_rule {
        if editable.custom_recurrence {
            return Err(args.bad(
                "this event repeats in a way that can only be changed in the calendar it came \
                 from",
            ));
        }
        draft.recurrence = if args.bool_or("stop_repeating", false) {
            None
        } else {
            repeat_from_args(args, "repeat")?
        };
        if editable.recurring {
            scope = EventScope::Series;
        }
    }
    if !editable.recurring {
        // A one-off has no series to speak of; "occurrence" is the event.
        scope = EventScope::Occurrence;
    }
    draft.validate().map_err(|e| args.bad(e))?;
    Ok((draft, scope))
}

fn describe_update_event(ctx: &ToolContext<'_>, args: &Args<'_>) -> Option<String> {
    let id = event_id(args).ok()?;
    let event = ctx.vault.event(id).ok()?;
    let mut parts = Vec::new();
    for (key, label) in [
        ("title", "rename it"),
        ("date", "move it"),
        ("start_time", "change its time"),
        ("end_time", "change its time"),
        ("description", "change its description"),
        ("location", "change its location"),
        ("repeat", "change how it repeats"),
        ("stop_repeating", "stop it repeating"),
    ] {
        if args.has_key(key) && !parts.contains(&label.to_string()) {
            parts.push(label.to_string());
        }
    }
    let adding = args.strings("add_attendees");
    if !adding.is_empty() {
        parts.push(format!("invite {}", adding.join(", ")));
    }
    let removing = args.strings("remove_attendees");
    if !removing.is_empty() {
        parts.push(format!("take {} off the guest list", removing.join(", ")));
    }
    let scope = match scope_from(args).ok()? {
        EventScope::Series => " (every occurrence)",
        EventScope::Occurrence => "",
    };
    let changes = if parts.is_empty() { "change it".to_string() } else { parts.join(", ") };
    let mut out =
        format!("\u{201c}{}\u{201d} on {}{scope}: {changes}", event.title, event.local_date);
    if !event.attendees.is_empty() {
        out.push_str(&format!(". Its guests are told: {}", event.attendees.join(", ")));
    }
    Some(out)
}

fn run_update_event(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id = event_id(args)?;
    let editable = editable(ctx, id)?;
    let (draft, scope) = apply_update(ctx, args, &editable)?;
    let added = draft
        .attendees
        .iter()
        .any(|a| !editable.draft.attendees.iter().any(|b| b.key() == a.key()));
    if ctx.unattended && (added || !draft.attendees.is_empty()) {
        return Err(Error::Invalid(
            "a scheduled run may not change an event that has guests. Say in your reply what \
             needs changing and leave it to them."
                .into(),
        ));
    }
    writer(ctx)?.update(id, &draft, scope)?;
    Ok(json!({
        "ok": true,
        "action": "updated",
        "kind": "event",
        "name": draft.title,
        "id": id.to_string(),
        "scope": scope.as_str(),
        "when": when(&draft),
        "repeats": draft.recurrence.as_ref().map(Recurrence::describe),
        "guests": draft.attendees.iter().map(|a| a.email.clone()).collect::<Vec<_>>(),
    }))
}

// ---- delete_event ----------------------------------------------------------

fn describe_delete_event(ctx: &ToolContext<'_>, args: &Args<'_>) -> Option<String> {
    let id = event_id(args).ok()?;
    let event = ctx.vault.event(id).ok()?;
    let scope = match scope_from(args).ok()? {
        EventScope::Series => " and every other occurrence of it",
        EventScope::Occurrence => "",
    };
    let mut out = format!("\u{201c}{}\u{201d} on {}{scope}", event.title, event.local_date);
    if !event.attendees.is_empty() {
        out.push_str(&format!(", sending a cancellation to {}", event.attendees.join(", ")));
    }
    Some(out)
}

fn run_delete_event(ctx: &ToolContext<'_>, args: &Args<'_>) -> Result<Value> {
    let id = event_id(args)?;
    let event = ctx.vault.event(id).map_err(|_| {
        Error::Invalid(format!("no event with id {id}. Call list_events and use an id from it."))
    })?;
    if ctx.unattended && !event.attendees.is_empty() {
        return Err(Error::Invalid(
            "a scheduled run may not cancel an event that has guests. Say in your reply that it \
             needs cancelling and leave it to them."
                .into(),
        ));
    }
    let scope = scope_from(args)?;
    writer(ctx)?.delete(id, scope)?;
    Ok(json!({
        "ok": true,
        "action": "deleted",
        "kind": "event",
        "name": event.title,
        "id": id.to_string(),
        "scope": scope.as_str(),
    }))
}
