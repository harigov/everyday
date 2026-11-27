//! A meeting's notes written, and the action items found in them, on the
//! panel: accept or reject each, or let them wait as proposals.
//!
//! The meeting pipeline writes the note, proposes the person's own next steps
//! as tasks, and then announces the note with what it is about, structured
//! ([`NoticeCard::MeetingNotes`]: the note, the first lines of its summary,
//! the proposals' ids). [`offer`] takes that when Settings → Panel routes
//! notes to the panel, and puts up `notes:{recordingId}`: the note, and each
//! action item with its due day, to be taken (`accept_proposal`, which makes
//! it a task) or left (`decline_proposal`) with one press, or all taken at
//! once. Once every one is decided the card says so and goes a moment later.
//! Nobody deciding is an answer too: after Settings' time the card goes, and
//! what was not decided stays a proposal, where the main window lists it. A
//! note with no action items is a short card that offers to open it.
//!
//! A card that goes without the person having touched it hands the
//! notification it stood in for on to the window then, as it would have gone
//! without the panel: they may have been away, and the desktop's list is
//! where they will look for what happened meanwhile. While the desktop's Do
//! Not Disturb is on no card goes up at all, and the notification goes on
//! at once (`events::desktop_quiet`). Neither goes up once the vault has
//! locked: the card names the meeting and what was said in it.
//!
//! The proposals are read from the service as the JSON it sends, not decoded
//! into this build's records, so a newer server's still read; what the card
//! shows of them is built by plain functions, tested without an app.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};

use jatayu_service::Ctx;
use jatayu_service::events::{MeetingNotesNotice, NoticeCard, Notification};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};

use crate::panel::settings::Route;
use crate::panel::{
    self, Action, Block, Decision, Expiry, Kind, NoteCard, Owner, Panel, Proposal, Tone,
};
use crate::state::AppState;

/// Who the service is told is calling: the panel's own window.
const CALLER: &str = "panel";

/// What the hub tells this module when a card's time is up.
const EXPIRE: &str = "expire";

/// How long the card stays once every action item is decided.
const SORTED_MS: u64 = 4_000;

/// How long a note with no action items stays up.
const NOTHING_TO_DECIDE_MS: u64 = 30_000;

/// The shortest time the action items stay up, whatever `panel.json` says.
const LEAST_SECONDS: u32 = 10;

/// How much of the summary the card shows.
const EXCERPT_CHARS: usize = 320;

/// How much of a task's notes stands under its title.
const DETAIL_CHARS: usize = 120;

// ---- what this module keeps ---------------------------------------------------

/// One notes card, as this module holds it.
#[derive(Debug, Clone)]
struct Held {
    notice: MeetingNotesNotice,
    /// The notification the card stands in for, handed on to the window if
    /// the card goes before the person did anything with it ([`expire`]).
    notification: Notification,
    /// The action items, as the card shows them, with where each stands.
    items: Vec<Proposal>,
    /// The ones being accepted or declined right now, by proposal id.
    deciding: Vec<String>,
    /// The person has decided something on the card, or tried to.
    acted: bool,
    /// Why the last decision failed: under the title.
    trouble: Option<String>,
    /// How long the card stays up undecided, from Settings when it went up.
    seconds: u32,
}

/// Every notes card that is up, by card id. See `mailcard`'s own for why a
/// static.
static CARDS: LazyLock<Mutex<HashMap<String, Held>>> = LazyLock::new(Mutex::default);

fn cards() -> MutexGuard<'static, HashMap<String, Held>> {
    CARDS.lock().unwrap_or_else(|e| e.into_inner())
}

/// How many times the vault has locked. Read when a card is offered and
/// again, under [`CARDS`]' lock, just before it goes up: reading the action
/// items waits on the service, and a lock in the meantime must not be
/// followed by a card naming the meeting. A count, not a look at the vault,
/// because a window on another computer's vault has no vault here to look
/// at; the lock reaches [`on_locked`] all the same. Bumped under the same
/// lock, so either the card is refused or it is up before [`on_locked`]
/// takes everything down.
static LOCKS: AtomicU64 = AtomicU64::new(0);

fn card_id(recording_id: &str) -> String {
    format!("notes:{recording_id}")
}

// ---- the shell's side -----------------------------------------------------------

/// Notes the service announced (`WindowSink::notify`, a notification carrying
/// `NoticeCard::MeetingNotes`). True if the panel takes it. Never waits: the
/// action items are read, and the card put up, on a task of its own -- which
/// first asks whether the desktop is holding notifications back, and if it
/// is hands the notification on to the window instead, as it would have gone
/// without the panel.
pub fn offer(app: &AppHandle, notification: &Notification) -> bool {
    let Some(NoticeCard::MeetingNotes(notes)) = &notification.card else { return false };
    let settings = crate::panel::settings::get();
    if settings.notes != Route::Panel {
        return false;
    }
    let locks = LOCKS.load(Ordering::SeqCst);
    let (app, notes, notification) = (app.clone(), notes.clone(), notification.clone());
    tauri::async_runtime::spawn(async move {
        // GSettings waits on the settings daemon: not on the runtime.
        let quiet = tauri::async_runtime::spawn_blocking(crate::events::desktop_quiet)
            .await
            .unwrap_or(false);
        if quiet {
            hand_on(&app, notification);
            return;
        }
        put_up(&app, notification, notes, settings.notes_seconds, locks).await;
    });
    true
}

/// A press on a notes card.
pub fn on_panel_action(app: &AppHandle, id: &str, action: &str, text: Option<String>) {
    let _ = text;
    if let Some(proposal) = action.strip_prefix("accept:") {
        decide(app, id, vec![proposal.to_string()], true);
        return;
    }
    if let Some(proposal) = action.strip_prefix("reject:") {
        decide(app, id, vec![proposal.to_string()], false);
        return;
    }
    match action {
        "acceptAll" => {
            let pending = cards().get(id).map(undecided).unwrap_or_default();
            decide(app, id, pending, true);
        }
        "open" => open(app, id),
        EXPIRE => expire(app, id),
        other => tracing::debug!(id, action = other, "a notes card does not know this press"),
    }
}

/// The vault locked: take every notes card down, and any being put up stays
/// down (see [`LOCKS`]).
pub fn on_locked(app: &AppHandle) {
    let mut cards = cards();
    LOCKS.fetch_add(1, Ordering::SeqCst);
    cards.clear();
    drop(cards);
    panel::dismiss_owner(app, Owner::Notes);
}

/// The notification a card stands in for, on to the window as `events.rs`
/// would have sent it without the panel: `notify.svelte.ts` makes it a toast
/// or puts it in the desktop's list, as it does any other.
fn hand_on(app: &AppHandle, notification: Notification) {
    if let Err(e) = app.emit(crate::events::NOTIFY_EVENT, notification) {
        tracing::debug!(error = %e, "a notes notification could not reach the interface");
    }
}

fn session(app: &AppHandle) -> crate::remote::Session {
    app.state::<AppState>().session().as_session()
}

/// Read the action items and put the card up. Reading them failing puts up
/// the note alone: the proposals are still in the main window. Not put up if
/// the vault has locked since it was offered, when the lock count read then,
/// `locks`, is no longer the count.
async fn put_up(
    app: &AppHandle,
    notification: Notification,
    notice: MeetingNotesNotice,
    seconds: u32,
    locks: u64,
) {
    let today = Timestamp::now().to_zoned(TimeZone::system()).date();
    let items = match action_items(&session(app), &notice).await {
        Ok(found) => pick(&found, &notice.proposals, &notice.title, today),
        Err(e) => {
            tracing::debug!(error = %e, "a notes card could not read its action items");
            Vec::new()
        }
    };
    let id = card_id(&notice.recording_id);
    let held = Held {
        notice,
        notification,
        items,
        deciding: Vec::new(),
        acted: false,
        trouble: None,
        seconds,
    };
    let drawn = card(&held);
    let mut cards = cards();
    if LOCKS.load(Ordering::SeqCst) != locks {
        return;
    }
    cards.insert(id, held);
    panel::show(app, drawn);
}

/// The pending task proposals made from the note: those about it, and any of
/// the ids the notice named that are not (a proposal saved without saying
/// what it is about), each read on its own.
async fn action_items(
    session: &crate::remote::Session,
    notice: &MeetingNotesNotice,
) -> jatayu_service::error::CommandResult<Vec<Value>> {
    let mut found = Vec::new();
    if !notice.note_id.trim().is_empty() {
        let query = json!({ "query": {
            "aboutKind": "note",
            "aboutId": notice.note_id,
            "states": ["pending"],
            "kinds": ["task"],
        }});
        let listed = session.call(Ctx::window(CALLER), "list_proposals", query).await?;
        found = listed.as_array().cloned().unwrap_or_default();
    }
    for wanted in &notice.proposals {
        if found.iter().any(|p| p.get("id").and_then(Value::as_str) == Some(wanted.as_str())) {
            continue;
        }
        if let Ok(one) =
            session.call(Ctx::window(CALLER), "get_proposal", json!({ "id": wanted })).await
        {
            found.push(one);
        }
    }
    Ok(found)
}

/// Change the card `id` as this module holds it, and draw it again. False,
/// and `edit` not run, if it is no longer up. See `mailcard::change`.
fn change(app: &AppHandle, id: &str, edit: impl FnOnce(&mut Held)) -> bool {
    let mut cards = cards();
    let Some(held) = cards.get_mut(id) else { return false };
    if panel::get(app, id).is_none() {
        cards.remove(id);
        return false;
    }
    edit(held);
    let drawn = card(held);
    if !panel::update(app, id, move |shown| *shown = drawn) {
        cards.remove(id);
        return false;
    }
    true
}

fn take_down(app: &AppHandle, id: &str) {
    cards().remove(id);
    panel::dismiss(app, id);
}

/// The card's time ran out: down it goes, and what was not decided stays a
/// proposal. If the person never touched it -- they may have been away -- the
/// notification it stood in for goes on to the window now ([`hand_on`]), so
/// that the notes are still announced where announcements are kept.
fn expire(app: &AppHandle, id: &str) {
    let held = cards().remove(id);
    panel::dismiss(app, id);
    if let Some(held) = held.filter(untouched) {
        hand_on(app, held.notification);
    }
}

/// The note in the main window, and the card down. What was not decided stays
/// a proposal.
fn open(app: &AppHandle, id: &str) {
    let note = cards().get(id).map(|held| held.notice.note_id.clone());
    let Some(note) = note else { return };
    if let Err(e) = crate::windows::open_item(app, "note", &note) {
        tracing::debug!(error = %e, "a notes card could not open its note");
    }
    take_down(app, id);
}

/// Accept or decline `proposals`, one after another, drawing each answer as it
/// comes. One already decided, or being decided, is left alone.
fn decide(app: &AppHandle, id: &str, proposals: Vec<String>, accept: bool) {
    let mut taken = Vec::new();
    change(app, id, |held| {
        for proposal in proposals {
            let pending = held
                .items
                .iter()
                .any(|item| item.id == proposal && item.state == Decision::Pending);
            if pending && !held.deciding.contains(&proposal) {
                held.deciding.push(proposal.clone());
                taken.push(proposal);
            }
        }
        if !taken.is_empty() {
            held.trouble = None;
            held.acted = true;
        }
    });
    if taken.is_empty() {
        return;
    }
    let (app, id) = (app.clone(), id.to_string());
    tauri::async_runtime::spawn(async move {
        let session = session(&app);
        let (command, state) = if accept {
            ("accept_proposal", Decision::Accepted)
        } else {
            ("decline_proposal", Decision::Rejected)
        };
        for proposal in taken {
            let answered =
                session.call(Ctx::window(CALLER), command, json!({ "id": proposal })).await;
            let up = change(&app, &id, |held| {
                held.deciding.retain(|p| *p != proposal);
                match answered {
                    Ok(_) => {
                        if let Some(item) = held.items.iter_mut().find(|i| i.id == proposal) {
                            item.state = state;
                        }
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, command, "a notes card's decision failed");
                        held.trouble = Some(sentence(&e.message));
                    }
                }
            });
            if !up {
                // The card went (its time, the lock): the rest stay proposals.
                return;
            }
        }
    });
}

// ---- the card, without an app ------------------------------------------------------

/// The ids of the items nobody has decided, in order.
fn undecided(held: &Held) -> Vec<String> {
    held.items.iter().filter(|i| i.state == Decision::Pending).map(|i| i.id.clone()).collect()
}

/// Whether the person has done nothing with the card: nothing decided, or
/// tried to be. Opening the note takes the card down, so it never gets here.
///
/// The hub does not tell owners when a card is held, so one that was read
/// and let go counts as untouched too, and is announced a second time when it
/// goes: the lesser loss beside notes never announced to somebody who was
/// away.
fn untouched(held: &Held) -> bool {
    !held.acted
}

/// The card for `held`, as it should be drawn now.
fn card(held: &Held) -> Panel {
    let notice = &held.notice;
    let title = match notice.title.trim() {
        "" => "Your meeting".to_string(),
        title => title.to_string(),
    };
    let mut panel = Panel::new(
        card_id(&notice.recording_id),
        Owner::Notes,
        Kind::Notes,
        "Meeting notes",
        title.clone(),
    );
    let excerpt = notice
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| clip(s, EXCERPT_CHARS));
    panel.blocks.push(Block::Note(NoteCard { title, excerpt, detail: None }));
    let open = Action::new("open", "Open notes").icon("open");
    if held.items.is_empty() {
        panel.actions = vec![open];
        panel.expires = Some(expiry(NOTHING_TO_DECIDE_MS));
    } else {
        panel.blocks.push(Block::Proposals {
            heading: Some("Action items".into()),
            items: held.items.clone(),
        });
        let left = undecided(held).len();
        if left == 0 {
            let taken = held.items.iter().filter(|i| i.state == Decision::Accepted).count();
            panel.tone = Tone::Success;
            panel.title = "All sorted".into();
            panel.body = match taken {
                0 => None,
                1 => Some("1 added to your tasks".into()),
                n => Some(format!("{n} added to your tasks")),
            };
            panel.actions = vec![open];
            panel.expires = Some(expiry(SORTED_MS));
        } else {
            let all = if left == 1 { "Accept" } else { "Accept all" };
            panel.actions = vec![Action::new("acceptAll", all).primary().icon("check"), open];
            // Only while it waits for the person: a card does not go in the
            // middle of what they asked it to do, or "Accept all" would stop
            // at whichever item the clock ran out on. Once the last answer is
            // in, the full time starts again for what is left.
            if held.deciding.is_empty() {
                panel.expires = Some(expiry(u64::from(held.seconds.max(LEAST_SECONDS)) * 1000));
            }
        }
    }
    panel.busy = !held.deciding.is_empty();
    if let Some(trouble) = &held.trouble {
        panel.tone = Tone::Trouble;
        panel.body = Some(trouble.clone());
    }
    panel
}

fn expiry(after_ms: u64) -> Expiry {
    Expiry { after_ms, action: EXPIRE.into(), remaining_ms: 0, held: false }
}

/// The action items to show, from the proposals read: in the order the notice
/// named them where it named any, and otherwise as they were read; only
/// pending task proposals, each once.
fn pick(found: &[Value], wanted: &[String], meeting: &str, today: Date) -> Vec<Proposal> {
    let items: Vec<Proposal> =
        found.iter().filter_map(|p| action_item(p, meeting, today)).collect();
    let mut picked: Vec<Proposal> = if wanted.is_empty() {
        items
    } else {
        wanted.iter().filter_map(|id| items.iter().find(|i| i.id == *id).cloned()).collect()
    };
    let mut seen = Vec::new();
    picked.retain(|item| {
        let first = !seen.contains(&item.id);
        seen.push(item.id.clone());
        first
    });
    picked
}

/// One proposal as an action item: a pending proposal to create a task. Its
/// due day in words, and under the title the first line of the task's notes,
/// or why it was proposed where that says more than the meeting's name, which
/// the card already shows.
fn action_item(proposal: &Value, meeting: &str, today: Date) -> Option<Proposal> {
    let id = proposal.get("id")?.as_str()?.trim();
    let pending = proposal.pointer("/outcome/type").and_then(Value::as_str) == Some("pending");
    let record = proposal.pointer("/payload/record")?;
    if id.is_empty() || !pending || record.get("kind")?.as_str()? != "task" {
        return None;
    }
    let task = record.get("value")?;
    let title = task.get("title")?.as_str()?.trim();
    if title.is_empty() {
        return None;
    }
    let due = task
        .get("dueDate")
        .and_then(Value::as_str)
        .and_then(|d| d.parse::<Date>().ok())
        .map(|d| due_words(d, today));
    let notes = task
        .get("notes")
        .and_then(Value::as_str)
        .and_then(|n| n.lines().map(str::trim).find(|l| !l.is_empty()));
    let why =
        proposal.get("why").and_then(Value::as_str).map(str::trim).filter(|w| {
            !w.is_empty() && !w.eq_ignore_ascii_case(&format!("From {}", meeting.trim()))
        });
    Some(Proposal {
        id: id.to_string(),
        title: title.to_string(),
        detail: notes.or(why).map(|d| clip(d, DETAIL_CHARS)),
        due,
        state: Decision::Pending,
    })
}

/// "Today", "Tomorrow", "Yesterday", the weekday within the week ahead
/// ("Fri"), and otherwise the date ("Thu 4 Dec"), with its year when it is
/// not this one's.
fn due_words(due: Date, today: Date) -> String {
    let days = (due - today).get_days();
    match days {
        0 => "Today".into(),
        1 => "Tomorrow".into(),
        -1 => "Yesterday".into(),
        2..=6 => due.strftime("%a").to_string(),
        _ if due.year() == today.year() => due.strftime("%a %-d %b").to_string(),
        _ => due.strftime("%-d %b %Y").to_string(),
    }
}

/// `text` cut to `most` characters, with an ellipsis where it was cut.
fn clip(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_string();
    }
    let cut: String = text.chars().take(most).collect();
    format!("{}\u{2026}", cut.trim_end())
}

/// A service's error as a card says it: a sentence, capitalised.
fn sentence(message: &str) -> String {
    let message = message.trim();
    let mut chars = message.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Something went wrong".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice() -> MeetingNotesNotice {
        MeetingNotesNotice {
            recording_id: "r1".into(),
            note_id: "n1".into(),
            title: "Design sync".into(),
            summary: Some("We agreed the new layout.".into()),
            proposals: vec!["p1".into(), "p2".into()],
        }
    }

    fn proposal(id: &str, title: &str, due: Option<&str>, why: &str) -> Value {
        let mut task = json!({ "id": format!("task-{id}"), "title": title, "notes": "" });
        if let Some(due) = due {
            task["dueDate"] = json!(due);
        }
        json!({
            "id": id,
            "kind": "task",
            "payload": { "type": "create", "record": { "kind": "task", "value": task } },
            "caption": title,
            "why": why,
            "outcome": { "type": "pending" },
        })
    }

    fn item(id: &str, state: Decision) -> Proposal {
        Proposal { id: id.into(), title: format!("Do {id}"), detail: None, due: None, state }
    }

    fn held(items: Vec<Proposal>) -> Held {
        let notification = Notification::new(
            jatayu_service::events::Level::Info,
            "Notes from Design sync are ready",
        )
        .card(NoticeCard::MeetingNotes(notice()));
        Held {
            notice: notice(),
            notification,
            items,
            deciding: Vec::new(),
            acted: false,
            trouble: None,
            seconds: 120,
        }
    }

    fn ids(panel: &Panel) -> Vec<&str> {
        panel.actions.iter().map(|a| a.id.as_str()).collect()
    }

    fn today() -> Date {
        // A Wednesday.
        jiff::civil::date(2026, 11, 25)
    }

    #[test]
    fn notes_with_action_items_offer_each_to_take_or_leave() {
        let panel = card(&held(vec![item("p1", Decision::Pending), item("p2", Decision::Pending)]));
        assert_eq!(panel.id, "notes:r1");
        assert_eq!(panel.owner, Owner::Notes);
        assert_eq!(panel.kind, Kind::Notes);
        assert_eq!(panel.kicker, "Meeting notes");
        assert_eq!(panel.title, "Design sync");
        assert_eq!(
            panel.blocks[0],
            Block::Note(NoteCard {
                title: "Design sync".into(),
                excerpt: Some("We agreed the new layout.".into()),
                detail: None,
            })
        );
        let Block::Proposals { items, .. } = &panel.blocks[1] else { panic!("{:?}", panel.blocks) };
        assert_eq!(items.len(), 2);
        assert_eq!(ids(&panel), ["acceptAll", "open"]);
        assert!(panel.actions[0].primary);
        assert_eq!(panel.actions[0].label, "Accept all");
        assert_eq!(panel.expires, Some(expiry(120_000)));
    }

    #[test]
    fn once_every_item_is_decided_the_card_says_so_and_goes() {
        let mut some = held(vec![item("p1", Decision::Accepted), item("p2", Decision::Pending)]);
        let panel = card(&some);
        assert_eq!(panel.actions[0].label, "Accept", "one left");
        assert_eq!(panel.tone, Tone::Neutral);

        some.items[1].state = Decision::Rejected;
        let panel = card(&some);
        assert_eq!(panel.tone, Tone::Success);
        assert_eq!(panel.title, "All sorted");
        assert_eq!(panel.body.as_deref(), Some("1 added to your tasks"));
        assert_eq!(ids(&panel), ["open"]);
        assert_eq!(panel.expires, Some(expiry(SORTED_MS)));

        let none_taken = held(vec![item("p1", Decision::Rejected)]);
        assert_eq!(card(&none_taken).body, None);
    }

    #[test]
    fn a_note_with_nothing_to_decide_offers_to_open_it_for_a_while() {
        let mut quiet = held(Vec::new());
        quiet.notice.summary = None;
        let panel = card(&quiet);
        assert_eq!(panel.blocks.len(), 1);
        assert_eq!(ids(&panel), ["open"]);
        assert_eq!(panel.expires, Some(expiry(NOTHING_TO_DECIDE_MS)));
        let Block::Note(note) = &panel.blocks[0] else { panic!() };
        assert_eq!(note.excerpt, None);
    }

    #[test]
    fn a_decision_under_way_and_one_that_failed_show() {
        let mut busy = held(vec![item("p1", Decision::Pending)]);
        busy.deciding.push("p1".into());
        assert!(card(&busy).busy);
        busy.deciding.clear();
        busy.trouble = Some("That proposal has expired".into());
        let panel = card(&busy);
        assert!(!panel.busy);
        assert_eq!(panel.tone, Tone::Trouble);
        assert_eq!(panel.body.as_deref(), Some("That proposal has expired"));
        assert_eq!(undecided(&busy), ["p1"]);
    }

    #[test]
    fn the_card_does_not_go_while_its_decisions_are_under_way() {
        let mut busy = held(vec![
            item("p1", Decision::Pending),
            item("p2", Decision::Pending),
            item("p3", Decision::Pending),
        ]);
        busy.deciding = vec!["p1".into(), "p2".into(), "p3".into()];
        let panel = card(&busy);
        assert_eq!(panel.expires, None, "Accept all is not cut short by the clock");
        assert_eq!(ids(&panel), ["acceptAll", "open"]);

        // The first answer in: still deciding the rest.
        busy.items[0].state = Decision::Accepted;
        busy.deciding.remove(0);
        assert_eq!(card(&busy).expires, None);

        // A failed one leaves an item pending and nothing under way: the
        // time starts again for it.
        busy.deciding.clear();
        busy.trouble = Some("That proposal has expired".into());
        assert_eq!(card(&busy).expires, Some(expiry(120_000)));

        // Nothing to decide is no reason to keep the card.
        let mut none = held(Vec::new());
        none.deciding.push("p1".into());
        assert_eq!(card(&none).expires, Some(expiry(NOTHING_TO_DECIDE_MS)));
    }

    #[test]
    fn only_a_card_nobody_touched_is_announced_when_it_goes() {
        let mut card = held(vec![item("p1", Decision::Pending)]);
        assert!(untouched(&card));
        card.acted = true;
        assert!(!untouched(&card), "something decided, or tried: nothing to announce");
        assert!(untouched(&held(Vec::new())), "a note with nothing to decide, left alone");
    }

    #[test]
    fn the_settings_time_has_a_floor() {
        let mut short = held(vec![item("p1", Decision::Pending)]);
        short.seconds = 1;
        assert_eq!(card(&short).expires, Some(expiry(u64::from(LEAST_SECONDS) * 1000)));
    }

    #[test]
    fn a_proposal_reads_as_an_action_item() {
        let mut with_notes =
            proposal("p1", " Send Ada the deck ", Some("2026-11-27"), "From Design sync");
        with_notes["payload"]["record"]["value"]["notes"] = json!("\n  Both versions\nand more");
        let got = action_item(&with_notes, "Design sync", today()).unwrap();
        assert_eq!(got.id, "p1");
        assert_eq!(got.title, "Send Ada the deck");
        assert_eq!(got.due.as_deref(), Some("Fri"));
        assert_eq!(got.detail.as_deref(), Some("Both versions"));
        assert_eq!(got.state, Decision::Pending);

        let plain = proposal("p2", "Book the room", None, "From Design sync");
        let got = action_item(&plain, "Design sync", today()).unwrap();
        assert_eq!(got.due, None);
        assert_eq!(got.detail, None, "the meeting's name the card already shows");

        let because = proposal("p3", "Call Grace", None, "Grace asked for it by Friday");
        assert_eq!(
            action_item(&because, "Design sync", today()).unwrap().detail.as_deref(),
            Some("Grace asked for it by Friday")
        );

        let mut decided = proposal("p4", "Old", None, "");
        decided["outcome"] = json!({ "type": "accepted", "at": "2026-11-25T10:00:00Z" });
        assert!(action_item(&decided, "Design sync", today()).is_none());

        let mut memory = proposal("p5", "Remember", None, "");
        memory["payload"]["record"]["kind"] = json!("memory");
        assert!(action_item(&memory, "Design sync", today()).is_none());
        assert!(action_item(&json!({ "id": "p6" }), "Design sync", today()).is_none());
    }

    #[test]
    fn the_items_are_those_the_notice_named_in_its_order() {
        let found = vec![
            proposal("p2", "Second", None, ""),
            proposal("other", "Not this note's", None, ""),
            proposal("p1", "First", None, ""),
            proposal("p1", "First again", None, ""),
        ];
        let wanted = vec!["p1".to_string(), "p2".to_string(), "gone".to_string()];
        let picked = pick(&found, &wanted, "Design sync", today());
        let titles: Vec<&str> = picked.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["First", "Second"]);

        let any = pick(&found[..2], &[], "Design sync", today());
        assert_eq!(any.len(), 2, "the notice named none: what is about the note");
    }

    #[test]
    fn due_days_are_said_as_a_person_would() {
        let today = today();
        let on = |m: i8, d: i8| jiff::civil::date(2026, m, d);
        assert_eq!(due_words(on(11, 25), today), "Today");
        assert_eq!(due_words(on(11, 26), today), "Tomorrow");
        assert_eq!(due_words(on(11, 24), today), "Yesterday");
        assert_eq!(due_words(on(11, 27), today), "Fri");
        assert_eq!(due_words(on(12, 1), today), "Tue");
        assert_eq!(due_words(on(12, 4), today), "Fri 4 Dec");
        assert_eq!(due_words(on(11, 2), today), "Mon 2 Nov");
        assert_eq!(due_words(jiff::civil::date(2027, 1, 8), today), "8 Jan 2027");
    }

    #[test]
    fn errors_and_long_text_read_well() {
        assert_eq!(sentence("proposal is no longer pending"), "Proposal is no longer pending");
        assert_eq!(sentence(""), "Something went wrong");
        assert_eq!(clip("abc", 5), "abc");
        assert_eq!(clip("abc def", 4), "abc\u{2026}");
    }
}
