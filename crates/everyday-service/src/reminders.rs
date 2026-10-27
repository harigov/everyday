//! Reminders: a task's [`remind_at`](everyday_core::Task::remind_at) coming
//! round, on the scheduler's minute.
//!
//! What happens then depends on whose task it is.
//!
//! - **The person's.** They are sent the task -- on the desktop, in their
//!   chat, or both, as [`ReminderSettings::yours`] says. No model is asked
//!   anything: a reminder is the one thing here that must not fail the way a
//!   routine can, and "Call the dentist" needs no writing.
//! - **The assistant's.** It starts on it: a [`JobKind::Task`] is queued for
//!   the job worker, which does the task with the assistant's tools and sends
//!   what it did as [`ReminderSettings::assistants`] says -- see
//!   `jobs::task`. Late or not: the work is still owed.
//!
//! Either way the task is stamped [`reminded_at`](everyday_core::Task::reminded_at)
//! before anything is sent, so a reminder goes once: a crash between the
//! stamp and the send loses one rather than repeating it every minute.
//!
//! # Late
//!
//! The app may have been shut, or the machine asleep, when a reminder came
//! due. One of theirs less than [`MISSED_AFTER`] late is still sent, saying
//! when it was for; anything older is stamped and left, because a week of
//! reminders arriving at once the morning somebody opens their laptop is
//! noise, and the tasks themselves are still there to be seen. The
//! assistant's own task is started however late, and told so.
//!
//! # In the chat
//!
//! A reminder sent to the chat gets a thread of its own, holding what was
//! sent, so that "done" or "again in an hour" typed back is answered by a
//! turn that knows which task it is about -- the same way a reply to a
//! routine's message carries on in the run's thread.
//!
//! [`ReminderSettings::yours`]: everyday_core::ReminderSettings::yours
//! [`ReminderSettings::assistants`]: everyday_core::ReminderSettings::assistants

use crate::chat::{ChatOutlet, FromRun};
use crate::events::{Change, Kind, Level, Notification, Op};
use crate::service::{Service, blocking};
use everyday_core::store::jobs::JobQuery;
use everyday_core::store::tasks::TaskQuery;
use everyday_core::{
    Assignee, Conversation, ConversationId, JobKind, Message, ReminderTo, Task, Vault,
};
use jiff::{SignedDuration, Timestamp};
use std::sync::Arc;

/// How late a reminder may be and still be sent. A day: the evening's
/// reminder is still worth having the next morning, last week's is not.
pub const MISSED_AFTER: SignedDuration = SignedDuration::from_hours(24);

/// What the chat calls whoever a reminder of theirs is from, the way it names
/// a routine.
const FROM: &str = "Reminder";

/// One pass: everything whose reminder is due, sent or started. Called by
/// the scheduler's tick, which has already checked the vault is open and
/// writable.
pub(crate) async fn sweep(service: &Arc<Service>) {
    let Some(vault) = service.get() else { return };
    let now = service.now();
    let found = blocking({
        let vault = vault.clone();
        move || Ok(take_due(&vault, now)?)
    })
    .await;
    let due = match found {
        Ok(due) => due,
        Err(e) => {
            tracing::warn!(error = %e.message, "could not look for reminders that are due");
            return;
        }
    };
    if due.changed {
        service.events().changed(Change::new(Kind::Task, Op::Updated));
    }
    if due.queued {
        service.events().changed(Change::new(Kind::Job, Op::Created));
        service.wake_jobs();
    }
    for reminder in due.send {
        // Off the tick's path: a message to the chat waits behind whatever
        // the owner's chat is doing, which can be minutes, and the routines
        // due this same minute should not wait for it.
        let (service, vault) = (service.clone(), vault.clone());
        tokio::spawn(async move { send(&service, &vault, reminder, now).await });
    }
}

/// What [`take_due`] found to do.
#[derive(Default)]
struct Due {
    /// The person's reminders, to send.
    send: Vec<Reminder>,
    /// Whether any task was written.
    changed: bool,
    /// Whether any of the assistant's tasks was handed to the job worker.
    queued: bool,
}

/// A reminder of one of the person's tasks, as it is sent.
struct Reminder {
    task: Task,
    /// When it was for.
    at: Timestamp,
    to: ReminderTo,
}

/// Stamp every task whose reminder is due, and say what is to be done about
/// each: a reminder to send, or a job queued for one of the assistant's.
fn take_due(vault: &Vault, now: Timestamp) -> everyday_core::Result<Due> {
    let mut due = Due::default();
    if !vault.supports_tasks() {
        return Ok(due);
    }
    let routes = vault.agent_settings().map(|s| s.reminders).unwrap_or_default();
    let query = TaskQuery { reminders_due_by: Some(now), ..TaskQuery::open() };
    for task in vault.tasks(&query)? {
        // One task that cannot be dealt with -- the vault locked half way,
        // a job that would not queue -- is left for the next minute, and
        // does not take the ones already stamped with it: those are sent.
        let id = task.id;
        if let Err(e) = take_one(vault, task, now, routes, &mut due) {
            tracing::warn!(task = %id, error = %e, "could not deal with a reminder that is due");
        }
    }
    Ok(due)
}

/// [`take_due`] for one task.
fn take_one(
    vault: &Vault,
    mut task: Task,
    now: Timestamp,
    routes: everyday_core::ReminderSettings,
    due: &mut Due,
) -> everyday_core::Result<()> {
    let Some(at) = task.next_reminder() else { return Ok(()) };
    match task.assignee {
        // The assistant's: queued, however late. The work is still owed, the
        // task is still open, and the job is told when it was meant for. Its
        // status moves when the job actually starts -- see `jobs::task` --
        // so one cancelled before then leaves nothing claiming to be under
        // way.
        Assignee::Assistant => {
            if !vault.supports_jobs() {
                return Ok(());
            }
            if !already_on_it(vault, &task)? {
                vault.queue_job(
                    JobKind::Task { task_id: task.id, title: task.title.clone() },
                    None,
                )?;
                due.queued = true;
            }
            stamp(vault, &mut task, now)?;
            due.changed = true;
        }
        Assignee::Owner => {
            stamp(vault, &mut task, now)?;
            due.changed = true;
            if now.duration_since(at) > MISSED_AFTER {
                tracing::info!(task = %task.id, "a reminder was missed by more than a day; not sent");
            } else {
                due.send.push(Reminder { at, to: routes.yours, task });
            }
        }
    }
    Ok(())
}

/// Write down that `task`'s reminder went out, as a change like any other --
/// its `updated_at` moves, so a proposal drafted against the task before
/// now is seen to be out of date rather than writing the stamp back out.
fn stamp(vault: &Vault, task: &mut Task, now: Timestamp) -> everyday_core::Result<()> {
    task.reminded_at = Some(now);
    everyday_core::Timestamped::touch(task);
    vault.save_task(task)
}

/// Whether a job for this task is already queued or running -- a second one
/// for the same task would do it twice.
fn already_on_it(vault: &Vault, task: &Task) -> everyday_core::Result<bool> {
    Ok(vault.jobs(&JobQuery::unfinished())?.iter().any(|job| job.kind.task_id() == Some(task.id)))
}

/// Send one reminder where it is to go. A chat that is not connected, or
/// does not take the message, sends it to the desktop instead, so a reminder
/// is never lost to a chat that is down.
async fn send(service: &Arc<Service>, vault: &Arc<Vault>, reminder: Reminder, now: Timestamp) {
    let zone = vault
        .agent_settings()
        .map(|s| s.now().time_zone().clone())
        .unwrap_or(jiff::tz::TimeZone::UTC);
    let late = (now.duration_since(reminder.at) > SignedDuration::from_mins(5))
        .then(|| reminder.at.to_zoned(zone.clone()).strftime("%-I:%M %P").to_string());
    let detail = detail(&reminder.task, late.as_deref(), &zone);
    let mut desktop = reminder.to.desktop();
    if reminder.to.chat() {
        let message = chat_message(&reminder.task.title, &detail);
        let sent = to_chat(service, vault, FROM, &reminder.task.title, message, None).await;
        if let Err(why) = sent {
            tracing::info!(task = %reminder.task.id, %why, "a reminder went to the desktop instead");
            desktop = true;
        }
    }
    if desktop {
        let mut notice = Notification::new(Level::Info, reminder.task.title.trim())
            .for_user()
            .key(format!("reminder:{}", reminder.task.id));
        if !detail.is_empty() {
            notice = notice.body(detail);
        }
        service.events().notify(notice);
    }
}

/// The line under a reminder's title: when it is due, and when it was for if
/// it is late.
fn detail(task: &Task, late: Option<&str>, zone: &jiff::tz::TimeZone) -> String {
    let mut parts = Vec::new();
    if let Some(day) = task.due_date {
        let today = Timestamp::now().to_zoned(zone.clone()).date();
        let when = match day {
            d if d == today => "today".to_string(),
            d if d < today => format!("was due {}", d.strftime("%a %-d %b")),
            d => d.strftime("%a %-d %b").to_string(),
        };
        let when = if when.starts_with("was") { when } else { format!("Due {when}") };
        match task.due_time {
            Some(time) => parts.push(format!("{when} at {}", time.strftime("%-I:%M %P"))),
            None => parts.push(when),
        }
    }
    if let Some(at) = late {
        parts.push(format!("This was for {at}"));
    }
    parts.join(" \u{b7} ")
}

/// A reminder as the chat shows it.
fn chat_message(title: &str, detail: &str) -> String {
    let mut out = format!("\u{23f0} **{}**", crate::chat::escape_markdown(title.trim()));
    if !detail.is_empty() {
        out.push_str(&format!("\n\n{detail}"));
    }
    out
}

/// Send `message` to the person's chat, in a thread a reply carries on in.
///
/// `conversation` is that thread when there already is one -- a job's own
/// transcript. Otherwise one is made, titled `title` and holding the message
/// as the assistant's, so a reply has it to go on.
pub(crate) async fn to_chat(
    service: &Arc<Service>,
    vault: &Arc<Vault>,
    from: &str,
    title: &str,
    message: String,
    conversation: Option<ConversationId>,
) -> Result<(), String> {
    let Some(outlet) = owner_outlet(service, vault).await else {
        return Err("no chat link is connected".into());
    };
    let conversation = match conversation {
        Some(id) => Some(id),
        None => blocking({
            let (vault, title, message) = (vault.clone(), title.to_string(), message.clone());
            move || {
                let conversation = Conversation {
                    title: Conversation::title_from(&format!("{FROM}: {title}")),
                    ..Conversation::new()
                };
                vault.save_conversation(&conversation)?;
                vault.save_message(&Message::assistant(conversation.id, message))?;
                Ok(conversation.id)
            }
        })
        .await
        .ok(),
    };
    let post = outlet.post(FromRun { routine: from.to_string(), conversation }, message);
    match tokio::time::timeout(crate::delivery::POST_TIMEOUT, post).await {
        Ok(sent) => sent,
        Err(_) => Err("the chat did not take the message in time".into()),
    }
}

/// The way to the person's chat: the first of their chat links, in the order
/// Settings lists them, that is switched on and connected here.
async fn owner_outlet(service: &Arc<Service>, vault: &Arc<Vault>) -> Option<Arc<dyn ChatOutlet>> {
    let links = blocking({
        let vault = vault.clone();
        move || Ok(vault.chat_settings()?.links)
    })
    .await
    .ok()?;
    links.iter().filter(|l| l.enabled).find_map(|l| service.chat_outlet(l.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reminder_in_the_chat_is_its_title_and_the_line_under_it() {
        assert_eq!(chat_message("Call the dentist", ""), "\u{23f0} **Call the dentist**");
        assert_eq!(
            chat_message("Pay *rent*", "Due today"),
            "\u{23f0} **Pay \\*rent\\***\n\nDue today",
            "a title is the person's words, not markup",
        );
    }

    #[test]
    fn the_line_under_says_when_it_is_due_and_when_it_was_for() {
        let zone = jiff::tz::TimeZone::UTC;
        let today = Timestamp::now().to_zoned(zone.clone()).date();
        let mut task = Task::new("Call the dentist");
        assert_eq!(detail(&task, None, &zone), "");
        task.due_date = Some(today);
        task.due_time = Some(jiff::civil::time(17, 0, 0, 0));
        assert_eq!(detail(&task, None, &zone), "Due today at 5:00 pm");
        assert_eq!(
            detail(&task, Some("9:00 am"), &zone),
            "Due today at 5:00 pm \u{b7} This was for 9:00 am"
        );
    }
}
