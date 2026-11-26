//! New mail, and invitations, on the panel: who it is from, what it says, and
//! a reply typed there and sent, without opening the app.
//!
//! # What comes here
//!
//! The service announces a lone new message as a notification that also
//! carries the message, structured ([`NoticeCard::Mail`],
//! `karmayoga_service::arrivals::new_mail_notice`); several at once stay one
//! line and never reach this module. [`offer`] takes it when Settings → Panel
//! routes mail to the panel, and then the notification goes no further -- no
//! banner as well as the card. Unless the main window turns out to be in front
//! of the person, visible and focused: there a toast in the window is the
//! right answer, so the notification is handed back to the window as
//! `events.rs` would have and no card goes up. The same when the desktop is
//! holding notifications back (Do Not Disturb, `events::desktop_quiet`): a
//! card would float over a full-screen window or a shared screen, where the
//! notification waits quietly in the desktop's list. Asking the window waits
//! on the main thread, and asking the desktop on its settings daemon, and
//! [`offer`] is reached from the service's event sink in the middle of a
//! sync, so both are asked on a blocking thread of their own and [`offer`]
//! answers at once. No card goes up once the vault has locked: it names who
//! wrote and what they said.
//!
//! # The card
//!
//! `mail:{messageId}`: the message as a message (who, the subject, its first
//! words, when, how many files), a box to reply in, and Archive, Mark read
//! and Open. It goes by itself after Settings' time unless the person is
//! reading it -- the hub keeps the time and holds it while the pointer is on
//! it or the box has the keyboard. One that goes before the person did
//! anything with it hands the notification it stood in for on to the window
//! then, so the message is still announced in the desktop's list for
//! somebody who was away. Which of the person's addresses it came to,
//! and the replies the mail AI suggests when the account lets it write, are
//! asked for once the card is up and drawn when they come: neither ever holds
//! the card back, and either failing leaves it as it was.
//!
//! # A reply
//!
//! Typed words go out the way a reply from the thread does: `new_draft` in
//! reply to the message (its recipients, its subject, the signature and the
//! quote it comes with), the words put above the signature as the editor's
//! own lines, `save_draft`, the person's checks before sending
//! (`check_draft`, Settings → Mail writing) as compose asks them, then
//! `send_draft` with the undo window. The card says "Sending…" with Undo
//! until that window closes, then "Sent", and goes. Undo stops it and leaves
//! the draft in Mail, where it can be finished. A check that did not pass, or
//! checks that could not run, stop it as they stop compose: the card says
//! what they found, and the reply waits as a draft, to be sent anyway or
//! opened in Mail and changed. A reply that cannot be sent puts the words
//! back in the box with why.
//!
//! # An invitation
//!
//! A message carrying an invitation that asks for an answer is drawn as the
//! event -- when, where, who is coming and their answers, the person's own --
//! with Accept, Maybe and Decline, which answer it as the thread's banner
//! does (`respond_to_invite`).
//!
//! Everything a card shows is built by plain functions of what this module
//! holds ([`card`]), so what a card says at each step is tested without an
//! app; the steps themselves are the calls between.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use karmayoga_core::mail::compose::{find_signature, quote_start};
use karmayoga_core::mail::{AttendeeResponse, UNDO_SEND_DEFAULT_SECONDS};
use karmayoga_core::markup::escape_html;
use karmayoga_service::Ctx;
use karmayoga_service::arrivals::mailbox_key;
use karmayoga_service::error::{CommandError, CommandResult};
use karmayoga_service::events::{InviteNotice, MailNotice, NoticeCard, Notification};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};

use crate::panel::settings::Route;
use crate::panel::{
    self, Action, Block, EventCard, Expiry, Input, Kind, MailCard, Mode, Owner, Panel, Person,
    Rsvp, Suggestion, Tone,
};
use crate::state::AppState;

/// Who the service is told is calling: the panel's own window.
const CALLER: &str = "panel";

/// What the hub tells this module when a card's time is up.
const EXPIRE: &str = "expire";

/// How long a reply waits before it goes, for Undo: the service's own default
/// window, asked for by name so that a server with another default still
/// waits exactly as long as the card says.
const UNDO_SECONDS: u32 = UNDO_SEND_DEFAULT_SECONDS;

/// How long "Sent", or an invitation answered, stays up.
const DONE_MS: u64 = 3_000;

/// The shortest time a mail card stays up unread, whatever `panel.json` says:
/// a card gone before it can be read is no card.
const LEAST_SECONDS: u32 = 5;

/// How much of a message's first words the card shows.
const SNIPPET_CHARS: usize = 280;

/// The most people an invitation's card names: an all-hands is a row of
/// faces, not a register.
const MOST_PEOPLE: usize = 50;

// ---- what this module keeps ---------------------------------------------------

/// Where a mail card is up to.
#[derive(Debug, Clone, PartialEq)]
enum Stage {
    /// Up, waiting for the person.
    Reading,
    /// A reply being made into a draft and queued: the box waits.
    Replying(String),
    /// Queued, to go when the undo window closes.
    Sending { draft: String, text: String },
    /// Undo pressed, and the service not yet answered.
    Undoing { draft: String, text: String },
    /// The checks before sending found something, or could not run: the
    /// reply waits as the draft `draft`, to be sent anyway or opened in Mail.
    /// `going` once Send anyway is pressed, until the service answers.
    Checked { draft: String, text: String, findings: Findings, going: bool },
    /// Undone: the draft is back in Mail.
    Stopped,
    /// Gone.
    Sent,
    /// Archive, Mark read, or an answer to an invitation, under way.
    Working,
    /// An invitation answered.
    Answered(Rsvp),
}

/// What the checks before sending said about a reply, when it is not
/// "send": `check_draft`'s answer, as much of it as the card says.
#[derive(Debug, Clone, PartialEq, Default)]
struct Findings {
    /// The checks that did not pass, in Settings' order, each with what to
    /// change where the model said.
    failed: Vec<(String, Option<String>)>,
    /// Why the checks did not run this time, as a sentence.
    skipped: Option<String>,
}

/// One mail card, as this module holds it: the message, and where the person
/// is with it.
#[derive(Debug, Clone)]
struct Held {
    notice: MailNotice,
    /// The notification the card stands in for, handed on to the window if
    /// the card goes before the person did anything with it ([`expire`]).
    notification: Notification,
    stage: Stage,
    /// The person has pressed something on the card or sent from its box,
    /// whether or not it worked.
    acted: bool,
    /// The replies the mail AI wrote for it, once it has.
    suggestions: Vec<Suggestion>,
    /// Which of the person's addresses it came to, where they have several.
    account: Option<String>,
    /// The person's own mailboxes ([`mailbox_key`]), to tell them apart among
    /// an invitation's guests: the address it came to, from the notice, and
    /// the account's own and its identities' once the accounts are read.
    own: Vec<String>,
    /// Why the last reply did not go: shown in the box, which puts the words
    /// back.
    reply_error: Option<String>,
    /// Why the checks before sending cannot run as things are set up
    /// (`check_draft`'s `off`): not a reason to hold the reply, but said
    /// while it goes, as compose says it.
    checks_off: Option<String>,
    /// Why anything else failed: under the title.
    trouble: Option<String>,
    /// How long the card stays up unread, from Settings when it went up.
    seconds: u32,
}

impl Held {
    fn new(notice: MailNotice, notification: Notification, seconds: u32) -> Held {
        Held {
            account: notice.to_address.clone(),
            own: notice.to_address.iter().map(|a| mailbox_key(a)).collect(),
            notice,
            notification,
            stage: Stage::Reading,
            acted: false,
            suggestions: Vec::new(),
            reply_error: None,
            checks_off: None,
            trouble: None,
            seconds,
        }
    }
}

/// Every mail card that is up, by card id. A static rather than state the app
/// manages: this module is the only one that reads it, and the cards are the
/// process's, whichever vault is open.
static CARDS: LazyLock<Mutex<HashMap<String, Held>>> = LazyLock::new(Mutex::default);

fn cards() -> MutexGuard<'static, HashMap<String, Held>> {
    CARDS.lock().unwrap_or_else(|e| e.into_inner())
}

/// How many times the vault has locked. Read when a card is offered and
/// again, under [`CARDS`]' lock, just before it goes up: in between the
/// window is asked whether it is in front, a wait on the main thread, and a
/// lock in the meantime must not be followed by a card naming who wrote and
/// what they said. A count, not a look at the vault, because a window on
/// another computer's vault has no vault here to look at; the lock reaches
/// [`on_locked`] all the same. Bumped under the same lock, so either the card
/// is refused or it is up before [`on_locked`] takes everything down.
static LOCKS: AtomicU64 = AtomicU64::new(0);

fn card_id(message_id: &str) -> String {
    format!("mail:{message_id}")
}

// ---- the shell's side -----------------------------------------------------------

/// A notification the service raised that carries [`NoticeCard::Mail`]
/// (`WindowSink::notify`). True if the panel takes it -- then the notification
/// is not passed on to become a system one; false leaves it to the
/// notification as before. Never waits.
///
/// Taken whenever mail is routed to the panel, even though the window may yet
/// turn out to be in front, or the desktop to be holding notifications back:
/// then it is handed to the window after all, from the thread that found that
/// out. See the module docs.
pub fn offer(app: &AppHandle, notification: &Notification) -> bool {
    let Some(NoticeCard::Mail(mail)) = &notification.card else { return false };
    let settings = crate::panel::settings::get();
    if settings.mail != Route::Panel {
        return false;
    }
    let locks = LOCKS.load(Ordering::SeqCst);
    let (app, notification, mail) = (app.clone(), notification.clone(), (**mail).clone());
    tauri::async_runtime::spawn_blocking(move || {
        if window_in_front(&app) || crate::events::desktop_quiet() {
            hand_on(&app, notification);
            return;
        }
        put_up(&app, notification, mail, settings.mail_seconds, locks);
    });
    true
}

/// A press on a mail card, or text sent from its box.
pub fn on_panel_action(app: &AppHandle, id: &str, action: &str, text: Option<String>) {
    match action {
        EXPIRE => expire(app, id),
        "reply" => reply(app, id, text.unwrap_or_default()),
        "sendAnyway" => send_anyway(app, id),
        "undo" => undo(app, id),
        "archive" => thread_op(app, id, "archive"),
        "read" => thread_op(app, id, "mark_read"),
        "open" => open(app, id),
        "accepted" | "tentative" | "declined" => answer(app, id, action),
        other => tracing::debug!(id, action = other, "a mail card does not know this press"),
    }
}

/// The vault locked: take every mail card down, and any being put up stays
/// down (see [`LOCKS`]). What was being sent stays queued; the cards only
/// name what the vault holds.
pub fn on_locked(app: &AppHandle) {
    let mut cards = cards();
    LOCKS.fetch_add(1, Ordering::SeqCst);
    cards.clear();
    drop(cards);
    panel::dismiss_owner(app, Owner::Mail);
}

/// The notification a card stands in for, on to the window as `events.rs`
/// would have sent it without the panel: `notify.svelte.ts` makes it a toast
/// or puts it in the desktop's list, as it does any other.
fn hand_on(app: &AppHandle, notification: Notification) {
    if let Err(e) = app.emit(crate::events::NOTIFY_EVENT, notification) {
        tracing::debug!(error = %e, "a mail notification could not reach the interface");
    }
}

fn window_in_front(app: &AppHandle) -> bool {
    app.get_webview_window(crate::MAIN_WINDOW)
        .is_some_and(|w| w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false))
}

fn session(app: &AppHandle) -> crate::remote::Session {
    app.state::<AppState>().session().as_session()
}

/// Put the card for `notice` up, and ask for what can be added to it. Not
/// put up if the vault has locked since it was offered, when the lock count
/// read then, `locks`, is no longer the count.
fn put_up(
    app: &AppHandle,
    notification: Notification,
    notice: MailNotice,
    seconds: u32,
    locks: u64,
) {
    let id = card_id(&notice.message_id);
    let held = Held::new(notice.clone(), notification, seconds);
    {
        let mut cards = cards();
        if LOCKS.load(Ordering::SeqCst) != locks {
            return;
        }
        // Announced again while its card is still up: the card as it is --
        // a reply half way out included -- is the one to keep.
        if cards.contains_key(&id) && panel::get(app, &id).is_some() {
            return;
        }
        let drawn = card(&held, Timestamp::now(), &TimeZone::system());
        cards.insert(id.clone(), held);
        panel::show(app, drawn);
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move { add_what_is_known(&app, &id, &notice).await });
}

/// Change the card `id` as this module holds it, and draw it again. False if
/// it is no longer up -- it went by itself, the person took it down, the vault
/// locked -- and then it is forgotten and `edit` is not run.
///
/// The card is drawn under this module's lock, so two changes are drawn in
/// the order they were made; [`panel::update`] never waits, and its closure
/// takes no lock of this module's.
fn change(app: &AppHandle, id: &str, edit: impl FnOnce(&mut Held)) -> bool {
    let mut cards = cards();
    let Some(held) = cards.get_mut(id) else { return false };
    if panel::get(app, id).is_none() {
        cards.remove(id);
        return false;
    }
    edit(held);
    let drawn = card(held, Timestamp::now(), &TimeZone::system());
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

/// The card's time ran out: down it goes. If the person never did anything
/// with it -- they may well have been away -- the notification it stood in
/// for goes on to the window now ([`hand_on`]), so the message is still
/// announced where announcements are kept: the desktop's list, or a toast if
/// they are at the window by now.
fn expire(app: &AppHandle, id: &str) {
    let held = cards().remove(id);
    panel::dismiss(app, id);
    if let Some(held) = held.filter(untouched) {
        hand_on(app, held.notification);
    }
}

/// Which address it came to, and the replies the mail AI suggests, asked for
/// once the card is up. Any of it failing is silent: the card is complete
/// without it.
async fn add_what_is_known(app: &AppHandle, id: &str, notice: &MailNotice) {
    let session = session(app);
    let accounts = match session.call(Ctx::window(CALLER), "list_accounts", json!({})).await {
        Ok(accounts) => accounts,
        Err(e) => {
            tracing::debug!(error = %e, "a mail card could not read the accounts");
            return;
        }
    };
    let facts = account_facts(&accounts, notice);
    let (label, own) = (facts.label.clone(), facts.own.clone());
    if !change(app, id, |held| {
        held.account = label;
        for mailbox in own {
            if !held.own.contains(&mailbox) {
                held.own.push(mailbox);
            }
        }
    }) {
        return;
    }
    if !facts.writing || notice.invite.is_some() {
        return;
    }
    let thread = json!({ "id": notice.thread_id });
    let needed = session
        .call(Ctx::window(CALLER), "needs_reply", thread.clone())
        .await
        .ok()
        .and_then(|v| v.get("needed").and_then(Value::as_bool))
        .unwrap_or(false);
    if !needed || panel::get(app, id).is_none() {
        return;
    }
    let written = match session.call(Ctx::window(CALLER), "suggest_replies", thread).await {
        Ok(written) => suggestions_from(&written),
        Err(e) => {
            tracing::debug!(error = %e, "a mail card could not have replies written");
            return;
        }
    };
    if !written.is_empty() {
        change(app, id, |held| held.suggestions = written);
    }
}

// ---- presses ----------------------------------------------------------------------

/// Send `text` as a reply to the card's message. See the module docs' "a
/// reply".
fn reply(app: &AppHandle, id: &str, text: String) {
    if text.trim().is_empty() {
        return;
    }
    let mut notice = None;
    change(app, id, |held| {
        if held.stage == Stage::Reading && held.notice.invite.is_none() {
            held.stage = Stage::Replying(text.clone());
            held.acted = true;
            held.reply_error = None;
            held.trouble = None;
            notice = Some(held.notice.clone());
        }
    });
    let Some(notice) = notice else { return };
    let (app, id) = (app.clone(), id.to_string());
    tauri::async_runtime::spawn(async move {
        let sent = send_reply(&session(&app), &notice, &text).await;
        match sent {
            Ok(Replied::Queued { draft, off }) => {
                wait_out_undo(&app, &id, draft, text, off).await;
            }
            Ok(Replied::Held { draft, findings }) => {
                // The card went while it was checked: the reply stays a
                // draft in Mail, which is where it would be sent from.
                change(&app, &id, |held| {
                    if held.stage == Stage::Replying(text.clone()) {
                        held.stage = Stage::Checked { draft, text, findings, going: false };
                    }
                });
            }
            Err(e) => {
                tracing::debug!(error = %e, "a reply from the panel did not go");
                change(&app, &id, |held| {
                    held.stage = Stage::Reading;
                    held.reply_error = Some(sentence(&e.message));
                });
            }
        }
    });
}

/// Send anyway, from what the checks found: the draft they were about, as it
/// is -- changed in Mail since, perhaps -- and not checked again, as compose's
/// own Send anyway does. One that cannot be queued stays a draft, with why.
fn send_anyway(app: &AppHandle, id: &str) {
    let mut kept = None;
    change(app, id, |held| {
        if let Stage::Checked { draft, text, going, .. } = &mut held.stage
            && !*going
        {
            *going = true;
            held.trouble = None;
            kept = Some((draft.clone(), text.clone()));
        }
    });
    let Some((draft, text)) = kept else { return };
    let (app, id) = (app.clone(), id.to_string());
    tauri::async_runtime::spawn(async move {
        let queued = session(&app)
            .call(
                Ctx::window(CALLER),
                "send_draft",
                json!({ "id": draft, "delaySeconds": UNDO_SECONDS }),
            )
            .await;
        match queued {
            Ok(_) => wait_out_undo(&app, &id, draft, text, None).await,
            Err(e) => {
                tracing::debug!(error = %e, "a reply sent anyway from the panel did not go");
                change(&app, &id, |held| {
                    if let Stage::Checked { draft: kept, going, .. } = &mut held.stage
                        && *kept == draft
                    {
                        *going = false;
                        held.trouble = Some(sentence(&e.message));
                    }
                });
            }
        }
    });
}

/// A reply just queued as `draft`: the card says "Sending…" with Undo until
/// the undo window closes, then "Sent". The card went meanwhile (its time
/// cannot run out while a reply is under way, so the lock): the reply still
/// goes, and there is nothing left to say so on.
async fn wait_out_undo(
    app: &AppHandle,
    id: &str,
    draft: String,
    text: String,
    off: Option<String>,
) {
    let mut queued = false;
    change(app, id, |held| {
        if was_going(&held.stage, &draft, &text) {
            held.stage = Stage::Sending { draft: draft.clone(), text };
            held.checks_off = off;
            queued = true;
        }
    });
    if !queued {
        return;
    }
    tokio::time::sleep(Duration::from_secs(u64::from(UNDO_SECONDS))).await;
    change(app, id, |held| {
        if matches!(&held.stage, Stage::Sending { draft: going, .. } if *going == draft) {
            held.stage = Stage::Sent;
        }
    });
}

/// Whether the card at `stage` was waiting for the reply `text`, as the draft
/// `draft`, to be queued: typed and being made, or sent anyway.
fn was_going(stage: &Stage, draft: &str, text: &str) -> bool {
    match stage {
        Stage::Replying(typed) => typed == text,
        Stage::Checked { draft: kept, going: true, .. } => kept == draft,
        _ => false,
    }
}

/// What became of a reply: queued, with why the checks could not run where
/// they cannot as things are set up; or held as a draft for what the checks
/// found.
#[derive(Debug)]
enum Replied {
    Queued { draft: String, off: Option<String> },
    Held { draft: String, findings: Findings },
}

/// Make the reply a draft, check it, and queue it. The draft is edited as the
/// JSON the service sent, not decoded into this build's `Draft`: a field a
/// newer vault's draft has, which this build does not know, would otherwise
/// be saved away. A draft made for a reply that could not then be queued is
/// thrown away, so that trying again does not leave a copy behind in Drafts
/// each time; one the checks stopped is kept, to be sent anyway or opened.
async fn send_reply(
    session: &crate::remote::Session,
    notice: &MailNotice,
    text: &str,
) -> CommandResult<Replied> {
    let mut draft = session
        .call(
            Ctx::window(CALLER),
            "new_draft",
            json!({ "account": notice.account_id, "inReplyTo": notice.message_id }),
        )
        .await?;
    let odd = || CommandError::new("invalid", "new_draft answered oddly");
    let id = draft.get("id").and_then(Value::as_str).map(str::to_string).ok_or_else(odd)?;
    let body = draft.get("bodyHtml").and_then(Value::as_str).unwrap_or_default().to_string();
    draft
        .as_object_mut()
        .ok_or_else(odd)?
        .insert("bodyHtml".into(), json!(reply_body(&body, text)));
    let made = async {
        session.call(Ctx::window(CALLER), "save_draft", json!({ "draft": draft })).await?;
        // The checks read the draft as stored, so they come after the save.
        let off = match check(session, &id).await {
            Verdict::Go { off } => off,
            Verdict::Hold(findings) => {
                return Ok(Replied::Held { draft: id.clone(), findings });
            }
        };
        session
            .call(
                Ctx::window(CALLER),
                "send_draft",
                json!({ "id": id, "delaySeconds": UNDO_SECONDS }),
            )
            .await?;
        Ok::<_, CommandError>(Replied::Queued { draft: id.clone(), off })
    }
    .await;
    if made.is_err() {
        let _ = session.call(Ctx::window(CALLER), "discard_draft", json!({ "id": id })).await;
    }
    made
}

/// Put the person's checks before sending to the stored draft `draft`, as
/// compose does when Send is pressed. An answer that is an error -- the
/// checks timing out at the server, a draft gone -- is never taken for a
/// pass: it is checks that could not run.
async fn check(session: &crate::remote::Session, draft: &str) -> Verdict {
    match session.call(Ctx::window(CALLER), "check_draft", json!({ "id": draft })).await {
        Ok(report) => verdict(&report),
        Err(e) => {
            tracing::debug!(error = %e, "a reply from the panel could not be checked");
            Verdict::Hold(Findings { failed: Vec::new(), skipped: Some(sentence(&e.message)) })
        }
    }
}

/// Undo a reply on its way. Too late, it says so and says it went.
fn undo(app: &AppHandle, id: &str) {
    let mut sending = None;
    change(app, id, |held| {
        if let Stage::Sending { draft, text } = &held.stage {
            sending = Some(draft.clone());
            held.stage = Stage::Undoing { draft: draft.clone(), text: text.clone() };
        }
    });
    let Some(draft) = sending else { return };
    let (app, id) = (app.clone(), id.to_string());
    tauri::async_runtime::spawn(async move {
        let stopped =
            session(&app).call(Ctx::window(CALLER), "undo_send", json!({ "draftId": draft })).await;
        change(&app, &id, |held| {
            if !matches!(&held.stage, Stage::Undoing { draft: undoing, .. } if *undoing == draft) {
                return;
            }
            match stopped {
                Ok(_) => held.stage = Stage::Stopped,
                Err(e) => {
                    held.stage = Stage::Sent;
                    held.trouble = Some(format!("Too late to stop it: {}", e.message));
                }
            }
        });
    });
}

/// Archive the thread, or mark it read, and take the card down.
fn thread_op(app: &AppHandle, id: &str, command: &'static str) {
    let mut thread = None;
    change(app, id, |held| {
        if held.stage == Stage::Reading && held.notice.invite.is_none() {
            thread = Some(held.notice.thread_id.clone());
            held.stage = Stage::Working;
            held.acted = true;
            held.trouble = None;
        }
    });
    let Some(thread) = thread else { return };
    let (app, id) = (app.clone(), id.to_string());
    tauri::async_runtime::spawn(async move {
        let done =
            session(&app).call(Ctx::window(CALLER), command, json!({ "threads": [thread] })).await;
        match done {
            Ok(_) => take_down(&app, &id),
            Err(e) => {
                tracing::debug!(error = %e, command, "a mail card's press failed");
                change(&app, &id, |held| {
                    held.stage = Stage::Reading;
                    held.trouble = Some(sentence(&e.message));
                });
            }
        }
    });
}

/// The thread in the main window, and the card down -- a reply the checks
/// held waits in it as a draft. Not while a reply is on its way: taking the
/// card down then would take its Undo, and any word of it failing, with it.
fn open(app: &AppHandle, id: &str) {
    let thread = cards()
        .get(id)
        .filter(|held| may_open(&held.stage))
        .map(|held| held.notice.thread_id.clone());
    let Some(thread) = thread else { return };
    if let Err(e) = crate::windows::open_item(app, "thread", &thread) {
        tracing::debug!(error = %e, "a mail card could not open its thread");
    }
    take_down(app, id);
}

/// Answer the card's invitation.
fn answer(app: &AppHandle, id: &str, response: &str) {
    let Some(rsvp) = rsvp_of(response) else { return };
    let mut message = None;
    change(app, id, |held| {
        if held.stage == Stage::Reading && held.notice.invite.is_some() {
            message = Some(held.notice.message_id.clone());
            held.stage = Stage::Working;
            held.acted = true;
            held.trouble = None;
        }
    });
    let Some(message) = message else { return };
    let (app, id, response) = (app.clone(), id.to_string(), response.to_string());
    tauri::async_runtime::spawn(async move {
        let answered = session(&app)
            .call(
                Ctx::window(CALLER),
                "respond_to_invite",
                json!({ "messageId": message, "response": response }),
            )
            .await;
        change(&app, &id, |held| match answered {
            Ok(_) => held.stage = Stage::Answered(rsvp),
            Err(e) => {
                held.stage = Stage::Reading;
                held.trouble = Some(sentence(&e.message));
            }
        });
    });
}

// ---- the card, without an app ------------------------------------------------------

/// Whether Open may take the card down at `stage`: not while a reply is on
/// its way -- being made, queued with Undo, being undone, or sent anyway.
fn may_open(stage: &Stage) -> bool {
    !matches!(
        stage,
        Stage::Replying(_)
            | Stage::Sending { .. }
            | Stage::Undoing { .. }
            | Stage::Checked { going: true, .. }
    )
}

/// Whether the person has done nothing with the card. A card read and let go
/// counts as untouched as well -- the hub does not tell owners when a card is
/// held -- so it is announced a second time when it goes: the lesser loss
/// beside a message never announced to somebody who was away.
fn untouched(held: &Held) -> bool {
    held.stage == Stage::Reading && !held.acted
}

/// What to do about `check_draft`'s answer, `report`, read as the JSON it is:
/// send at once when nothing failed and nothing kept the checks from running
/// -- which includes no check switched on -- and otherwise hold the reply
/// for what was found, as compose's `checksPass` does. A result that does not
/// say it passed is taken as one that did not.
fn verdict(report: &Value) -> Verdict {
    let words = |v: &Value| v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let failed: Vec<(String, Option<String>)> = report
        .get("results")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|r| r.get("passed").and_then(Value::as_bool) != Some(true))
        .map(|r| {
            let check = r.get("check").and_then(words).unwrap_or_else(|| "A check".into());
            (check, r.get("note").and_then(words))
        })
        .collect();
    let skipped = report.get("skipped").and_then(words);
    if failed.is_empty() && skipped.is_none() {
        Verdict::Go { off: report.get("off").and_then(words) }
    } else {
        Verdict::Hold(Findings { failed, skipped })
    }
}

/// See [`verdict`].
#[derive(Debug, PartialEq)]
enum Verdict {
    /// Send it; with why the checks cannot run as things are set up, where
    /// that is so.
    Go { off: Option<String> },
    /// Hold it for the person.
    Hold(Findings),
}

/// The heading over what the checks found: what the warning in compose says.
fn findings_heading(findings: &Findings) -> String {
    match findings.failed.len() {
        0 => "Checks couldn't run".into(),
        1 => "1 check didn't pass".into(),
        n => format!("{n} checks didn't pass"),
    }
}

/// What the checks found, a line each: the check, and what to change where
/// the model said; why they could not run; and where the reply is meanwhile.
fn findings_text(findings: &Findings) -> String {
    let mut lines: Vec<String> = findings
        .failed
        .iter()
        .map(|(check, note)| match note {
            Some(note) => format!("{check} \u{2014} {note}"),
            None => check.clone(),
        })
        .collect();
    lines.extend(findings.skipped.clone());
    lines.push("Your reply is kept as a draft.".into());
    lines.join("\n")
}

/// The card for `held`, as it should be drawn now.
fn card(held: &Held, now: Timestamp, zone: &TimeZone) -> Panel {
    let mut panel = match &held.notice.invite {
        Some(invite) => invite_card(held, invite, now, zone),
        None => mail_card(held, now, zone),
    };
    if let Some(trouble) = &held.trouble {
        panel.tone = Tone::Trouble;
        panel.body = Some(trouble.clone());
    }
    panel
}

fn mail_card(held: &Held, now: Timestamp, zone: &TimeZone) -> Panel {
    let notice = &held.notice;
    let mut panel = Panel::new(
        card_id(&notice.message_id),
        Owner::Mail,
        Kind::Mail,
        "New mail",
        subject_or(&notice.subject),
    );
    panel.blocks.push(Block::Mail(mail_block(held, now, zone)));
    match &held.stage {
        Stage::Reading | Stage::Replying(_) | Stage::Working => {
            panel.blocks.push(Block::Input(reply_box(held)));
            // None while a reply is being made: Archive and Mark read could
            // not be done then, and Open would take the card down with the
            // reply's Undo still to come. A card with no buttons drops no
            // press.
            if !matches!(held.stage, Stage::Replying(_)) {
                panel.actions = vec![
                    Action::new("archive", "Archive").icon("archive"),
                    Action::new("read", "Mark read").icon("read"),
                    Action::new("open", "Open").icon("open"),
                ];
            }
            panel.busy = held.stage == Stage::Working;
            // Only while it waits for the person: a card does not go in the
            // middle of what they asked it to do.
            if held.stage == Stage::Reading {
                panel.expires = Some(expiry(u64::from(held.seconds.max(LEAST_SECONDS)) * 1000));
            }
        }
        Stage::Checked { text, findings, going, .. } => {
            panel.tone = Tone::Attention;
            panel
                .blocks
                .push(Block::Quote { label: Some("Your reply".into()), text: text.clone() });
            panel.blocks.push(Block::Text {
                heading: Some(findings_heading(findings)),
                text: findings_text(findings),
                folded: false,
            });
            if *going {
                panel.busy = true;
            } else {
                // Opening it to change it is the safe answer, so it is the
                // primary one, as Keep editing is in compose's warning.
                panel.actions = vec![
                    Action::new("open", "Open draft").primary().icon("open"),
                    Action::new("sendAnyway", "Send anyway").icon("send"),
                ];
                // Going by itself leaves the draft in Mail, as it says.
                panel.expires = Some(expiry(u64::from(held.seconds.max(LEAST_SECONDS)) * 1000));
            }
        }
        Stage::Sending { text, .. } | Stage::Undoing { text, .. } => {
            panel.tone = Tone::Working;
            // Checks that cannot run as things are set up do not hold a reply
            // back, but are said, as compose says them.
            panel.body = held.checks_off.clone();
            panel
                .blocks
                .push(Block::Quote { label: Some("Your reply".into()), text: text.clone() });
            panel.blocks.push(Block::Activity {
                mode: Mode::Sending,
                label: Some("Sending\u{2026}".into()),
                tool: None,
            });
            if matches!(held.stage, Stage::Sending { .. }) {
                panel.actions = vec![Action::new("undo", "Undo")];
            } else {
                panel.busy = true;
            }
        }
        Stage::Stopped => {
            panel.body = Some("Sending stopped \u{2014} the draft is in Mail".into());
            panel.actions = vec![Action::new("open", "Open").icon("open")];
            panel.expires = Some(expiry(u64::from(held.seconds.max(LEAST_SECONDS)) * 1000));
        }
        Stage::Sent | Stage::Answered(_) => {
            panel.tone = Tone::Success;
            panel.blocks.push(Block::Activity {
                mode: Mode::Done,
                label: Some("Sent".into()),
                tool: None,
            });
            panel.expires = Some(expiry(DONE_MS));
        }
    }
    panel
}

fn invite_card(held: &Held, invite: &InviteNotice, now: Timestamp, zone: &TimeZone) -> Panel {
    let notice = &held.notice;
    let title = match invite.summary.trim() {
        "" => subject_or(&notice.subject),
        summary => summary.to_string(),
    };
    let mut panel =
        Panel::new(card_id(&notice.message_id), Owner::Mail, Kind::Invite, "Invitation", title);
    panel.tone = Tone::Attention;
    let answered = match held.stage {
        Stage::Answered(rsvp) => Some(rsvp),
        _ => None,
    };
    panel.blocks.push(Block::Event(event_block(notice, invite, &held.own, answered, now, zone)));
    if let Some(line) = sent_by(notice, invite) {
        panel.blocks.push(Block::Text { heading: None, text: line, folded: false });
    }
    match held.stage {
        Stage::Answered(rsvp) => {
            panel.tone = Tone::Success;
            panel.blocks.push(Block::Activity {
                mode: Mode::Done,
                label: Some(answered_words(rsvp).into()),
                tool: None,
            });
            panel.expires = Some(expiry(DONE_MS));
        }
        _ => {
            panel.actions = vec![
                Action::new("accepted", "Accept").primary().icon("check"),
                Action::new("tentative", "Maybe"),
                Action::new("declined", "Decline").danger().icon("x"),
            ];
            panel.busy = held.stage == Stage::Working;
            if held.stage == Stage::Reading {
                panel.expires = Some(expiry(u64::from(held.seconds.max(LEAST_SECONDS)) * 1000));
            }
        }
    }
    panel
}

fn expiry(after_ms: u64) -> Expiry {
    Expiry { after_ms, action: EXPIRE.into(), remaining_ms: 0, held: false }
}

fn mail_block(held: &Held, now: Timestamp, zone: &TimeZone) -> MailCard {
    let notice = &held.notice;
    let address = notice.from_address.trim();
    let name = match notice.from_name.trim() {
        "" => address.to_string(),
        name => name.to_string(),
    };
    let snippet = notice.snippet.trim();
    // A one-line message is often its subject again.
    let repeats = snippet.eq_ignore_ascii_case(notice.subject.trim());
    MailCard {
        from: Person {
            email: (!address.is_empty() && address != name).then(|| address.to_string()),
            name,
            detail: None,
            status: None,
            organizer: false,
            you: false,
        },
        subject: subject_or(&notice.subject),
        snippet: (!snippet.is_empty() && !repeats).then(|| clip(snippet, SNIPPET_CHARS)),
        received: Some(received_words(notice.date, now, zone)),
        account: held.account.clone(),
        attachments: notice.attachments,
    }
}

fn reply_box(held: &Held) -> Input {
    let first = first_name(&held.notice.from_name);
    Input {
        id: "reply".into(),
        placeholder: if first.is_empty() {
            "Reply\u{2026}".into()
        } else {
            format!("Reply to {first}\u{2026}")
        },
        submit: "Send".into(),
        multiline: true,
        suggestions: held.suggestions.clone(),
        modes: Vec::new(),
        busy: matches!(held.stage, Stage::Replying(_)),
        error: held.reply_error.clone(),
    }
}

/// The invitation as an event: when, where, who is coming and their answers,
/// and the person's own answer -- the one just given, once there is one.
fn event_block(
    notice: &MailNotice,
    invite: &InviteNotice,
    own: &[String],
    answered: Option<Rsvp>,
    now: Timestamp,
    zone: &TimeZone,
) -> EventCard {
    let location = invite.location.as_deref().map(str::trim).filter(|l| !l.is_empty());
    // A meeting held on a call names its link as its place: that is the
    // service's name, not a place.
    let join = location
        .filter(|l| l.starts_with("http"))
        .and_then(karmayoga_core::meeting::brief::join_service);
    EventCard {
        title: match invite.summary.trim() {
            "" => subject_or(&notice.subject),
            summary => summary.to_string(),
        },
        when: when_words(invite, zone),
        date: invite_day(invite, zone).map(|day| day_words(day, now.to_zoned(zone.clone()).date())),
        place: if join.is_some() { None } else { location.map(str::to_string) },
        join: join.map(str::to_string),
        calendar: None,
        people: invite_people(invite, own),
        rsvp: answered.or(invite.my_response.map(rsvp_from)),
    }
}

/// The organiser first, then whoever else is invited, each with their answer.
/// The organiser is often on the list of attendees too, and is named once.
/// The person themself is told apart by their own mailboxes, `own`
/// ([`mailbox_key`]s), so an alias or a `+tag` address of theirs is them too.
fn invite_people(invite: &InviteNotice, own: &[String]) -> Vec<Person> {
    let organizer = invite.organizer.trim();
    let is_organizer = |name: &str, email: &str| {
        !organizer.is_empty()
            && (organizer.eq_ignore_ascii_case(name.trim())
                || organizer.eq_ignore_ascii_case(email.trim()))
    };
    let is_you = |email: &str| {
        let email = email.trim();
        !email.is_empty() && own.contains(&mailbox_key(email))
    };
    let listed = invite.attendees.iter().find(|a| is_organizer(&a.name, &a.email));
    let mut people = Vec::new();
    if !organizer.is_empty() {
        people.push(Person {
            name: listed.map_or_else(|| organizer.to_string(), |a| a.name.clone()),
            email: listed.map(|a| a.email.clone()).filter(|e| !e.is_empty()),
            detail: None,
            status: listed.and_then(|a| a.response).map(rsvp_from),
            organizer: true,
            you: listed.is_some_and(|a| is_you(&a.email)),
        });
    }
    for attendee in &invite.attendees {
        if is_organizer(&attendee.name, &attendee.email) {
            continue;
        }
        let name = match attendee.name.trim() {
            "" => attendee.email.trim().to_string(),
            name => name.to_string(),
        };
        let email = attendee.email.trim();
        people.push(Person {
            email: (!email.is_empty() && email != name).then(|| email.to_string()),
            name,
            detail: None,
            status: attendee.response.map(rsvp_from),
            organizer: false,
            you: is_you(email),
        });
    }
    people.truncate(MOST_PEOPLE);
    people
}

/// "Sent by Ada" under an invitation that came from somebody who is not its
/// organiser -- an assistant, a forward. Nothing when it came from them.
fn sent_by(notice: &MailNotice, invite: &InviteNotice) -> Option<String> {
    let organizer = invite.organizer.trim();
    let name = notice.from_name.trim();
    let address = notice.from_address.trim();
    if organizer.is_empty()
        || organizer.eq_ignore_ascii_case(name)
        || organizer.eq_ignore_ascii_case(address)
    {
        return None;
    }
    let who = if name.is_empty() { address } else { name };
    (!who.is_empty()).then(|| format!("Sent by {who}"))
}

/// "10:00–10:30", "23:00–Fri 01:00" across midnight, or "All day".
fn when_words(invite: &InviteNotice, zone: &TimeZone) -> String {
    if invite.all_day {
        return "All day".into();
    }
    let Some(start) = invite.start.map(|s| s.to_zoned(zone.clone())) else {
        return String::new();
    };
    let from = start.strftime("%H:%M").to_string();
    match invite.end.map(|e| e.to_zoned(zone.clone())) {
        Some(end) if end.date() == start.date() => {
            format!("{from}\u{2013}{}", end.strftime("%H:%M"))
        }
        Some(end) => format!("{from}\u{2013}{}", end.strftime("%a %H:%M")),
        None => from,
    }
}

/// The day an invitation is on: an all-day one's own date, which the service
/// keeps as midnight UTC (`Invite`'s all-day convention), and a timed one's
/// start, here.
fn invite_day(invite: &InviteNotice, zone: &TimeZone) -> Option<Date> {
    let start = invite.start?;
    Some(if invite.all_day {
        start.to_zoned(TimeZone::UTC).date()
    } else {
        start.to_zoned(zone.clone()).date()
    })
}

/// "Today", "Tomorrow", or "Thu 27 Nov".
fn day_words(day: Date, today: Date) -> String {
    if day == today {
        "Today".into()
    } else if today.tomorrow().ok() == Some(day) {
        "Tomorrow".into()
    } else {
        day.strftime("%a %-d %b").to_string()
    }
}

/// "2:14 pm" for mail that came today, "Tue 2:14 pm" for mail that did not.
fn received_words(date: Timestamp, now: Timestamp, zone: &TimeZone) -> String {
    let at = date.to_zoned(zone.clone());
    if at.date() == now.to_zoned(zone.clone()).date() {
        at.strftime("%-I:%M %P").to_string()
    } else {
        at.strftime("%a %-I:%M %P").to_string()
    }
}

/// What a card says once an invitation is answered.
fn answered_words(rsvp: Rsvp) -> &'static str {
    match rsvp {
        Rsvp::Accepted => "Accepted",
        Rsvp::Tentative => "You said maybe",
        Rsvp::Declined => "Declined",
        Rsvp::NeedsAction => "Answered",
    }
}

fn rsvp_from(response: AttendeeResponse) -> Rsvp {
    match response {
        AttendeeResponse::Accepted => Rsvp::Accepted,
        AttendeeResponse::Tentative => Rsvp::Tentative,
        AttendeeResponse::Declined => Rsvp::Declined,
        AttendeeResponse::NeedsAction => Rsvp::NeedsAction,
    }
}

/// The answer a button stands for: its id is `respond_to_invite`'s own word.
fn rsvp_of(response: &str) -> Option<Rsvp> {
    match response {
        "accepted" => Some(Rsvp::Accepted),
        "tentative" => Some(Rsvp::Tentative),
        "declined" => Some(Rsvp::Declined),
        _ => None,
    }
}

fn subject_or(subject: &str) -> String {
    match subject.trim() {
        "" => "(no subject)".into(),
        subject => subject.to_string(),
    }
}

/// The name to greet in the reply box: "Ada" for "Ada Lovelace", and for
/// "Lovelace, Ada"; an address, where that is all there is, as it stands.
fn first_name(name: &str) -> String {
    let name = name.trim().trim_matches(['"', '\'']).trim();
    if name.is_empty() || name.contains('@') {
        return name.to_string();
    }
    let given = match name.split_once(',') {
        Some((_, given)) if !given.trim().is_empty() => given.trim(),
        _ => name,
    };
    given.split_whitespace().next().unwrap_or(given).to_string()
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

/// What the card needs of the person's accounts.
#[derive(Debug, Default, PartialEq)]
struct AccountFacts {
    /// The account lets the mail AI write replies (`MailAi::writing`).
    writing: bool,
    /// The address to name on the card, where naming one tells the person
    /// something: the one it came to, where the account has several, or the
    /// account's own, where there are several accounts.
    label: Option<String>,
    /// The account's own addresses and its identities', as [`mailbox_key`]s:
    /// who the person is among an invitation's guests. The service tells the
    /// card which address a message came to only where an account has
    /// several mailboxes, so the usual one-address account is known by this.
    own: Vec<String>,
}

/// [`AccountFacts`] from `list_accounts`' answer, read as the JSON it is so a
/// newer server's accounts still read.
fn account_facts(accounts: &Value, notice: &MailNotice) -> AccountFacts {
    let list = accounts.as_array().map(Vec::as_slice).unwrap_or_default();
    let mail_accounts =
        list.iter().filter(|a| a["services"]["mail"].as_bool() == Some(true)).count();
    let this = list.iter().find(|a| a["id"].as_str() == Some(notice.account_id.as_str()));
    let writing = this.and_then(|a| a["mailAi"]["writing"].as_bool()).unwrap_or(false);
    let address = this
        .and_then(|a| a["address"].as_str())
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string);
    let mut own: Vec<String> = Vec::new();
    let identities = this
        .and_then(|a| a["identities"].as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|identity| identity["address"].as_str());
    for found in address.as_deref().into_iter().chain(identities) {
        let mailbox = mailbox_key(found);
        if !found.trim().is_empty() && !own.contains(&mailbox) {
            own.push(mailbox);
        }
    }
    let label = notice.to_address.clone().or(if mail_accounts > 1 { address } else { None });
    AccountFacts { writing, label, own }
}

/// The replies `suggest_replies` wrote, as the box's ready-made texts: the
/// plain text, which is what the box holds.
fn suggestions_from(written: &Value) -> Vec<Suggestion> {
    let Some(list) = written.get("suggestions").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|s| {
            let label = s.get("label")?.as_str()?.trim();
            let text = s.get("bodyText")?.as_str()?.trim();
            (!label.is_empty() && !text.is_empty())
                .then(|| Suggestion { label: label.into(), text: text.into() })
        })
        .take(3)
        .collect()
}

// ---- the reply's body, without an app -----------------------------------------------
//
// What `ui/src/lib/mailwrite.ts`'s `splitQuoted`, `joinQuoted` and
// `withOwnWords` do for `MailQuickReplies.svelte`, here: the draft
// `new_draft` makes holds the person's own part (an empty line and the
// signature) and the quote under it, and the words typed replace the empty
// line, above the signature, with the quote left as it came.

/// A reply draft's body told apart into its own part and the quoted parent
/// under it (from the "On ... wrote:" line), by the same rule the service uses
/// to send it (`compose::quote_start`). No quote: all of it is its own.
fn split_quoted(html: &str) -> (&str, &str) {
    html.split_at(quote_start(html).unwrap_or(html.len()))
}

/// The own part of a draft with its words replaced by `words_html`, and the
/// signature, if it still has one, kept under them.
fn with_own_words(own: &str, words_html: &str) -> String {
    format!("{words_html}{}", find_signature(own).unwrap_or_default())
}

/// Plain text typed in the box as the lines a draft holds: a `<p>` a line and
/// an empty one a blank line, the compose editor's own model
/// (`Draft::body_html`). Escaped: it is the person's words, never markup.
/// Blank lines at either end are dropped.
fn text_as_lines(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let Some(first) = lines.iter().position(|l| !l.trim().is_empty()) else {
        return String::new();
    };
    let last = lines.iter().rposition(|l| !l.trim().is_empty()).unwrap_or(first);
    lines[first..=last]
        .iter()
        .map(|line| match line.trim() {
            "" => "<p></p>".to_string(),
            _ => format!("<p>{}</p>", escape_html(line)),
        })
        .collect()
}

/// The body of a reply draft once `text` is put in it.
fn reply_body(draft_html: &str, text: &str) -> String {
    let (own, quoted) = split_quoted(draft_html);
    format!("{}{quoted}", with_own_words(own, &text_as_lines(text)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use karmayoga_service::events::InviteeNotice;

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_second(secs).unwrap()
    }

    /// 2023-11-14 22:13:20 UTC, a Tuesday.
    const NOW: i64 = 1_700_000_000;

    fn notice() -> MailNotice {
        MailNotice {
            account_id: "acct".into(),
            message_id: "m1".into(),
            thread_id: "t1".into(),
            from_name: "Ada Lovelace".into(),
            from_address: "ada@example.com".into(),
            subject: "The proof".into(),
            snippet: "Here is the second half.".into(),
            date: ts(NOW - 600),
            to_address: None,
            attachments: 2,
            invite: None,
        }
    }

    fn invite() -> InviteNotice {
        InviteNotice {
            summary: "Design sync".into(),
            start: Some(ts(NOW + 3_600)),
            end: Some(ts(NOW + 5_400)),
            all_day: false,
            location: Some("Room 4".into()),
            organizer: "Grace Hopper".into(),
            attendees: vec![
                InviteeNotice {
                    name: "Grace Hopper".into(),
                    email: "grace@example.com".into(),
                    response: Some(AttendeeResponse::Accepted),
                },
                InviteeNotice {
                    name: "".into(),
                    email: "me@example.com".into(),
                    response: Some(AttendeeResponse::NeedsAction),
                },
                InviteeNotice {
                    name: "Edsger".into(),
                    email: "edsger@example.com".into(),
                    response: Some(AttendeeResponse::Declined),
                },
            ],
            my_response: Some(AttendeeResponse::NeedsAction),
        }
    }

    /// The card for `notice` as it goes up, announced as the service does.
    fn held(notice: MailNotice, seconds: u32) -> Held {
        let notification =
            Notification::new(karmayoga_service::events::Level::Info, "Ada Lovelace: The proof")
                .card(NoticeCard::Mail(Box::new(notice.clone())));
        Held::new(notice, notification, seconds)
    }

    fn draw(held: &Held) -> Panel {
        card(held, ts(NOW), &TimeZone::UTC)
    }

    fn ids(panel: &Panel) -> Vec<&str> {
        panel.actions.iter().map(|a| a.id.as_str()).collect()
    }

    fn input(panel: &Panel) -> Option<&Input> {
        panel.blocks.iter().find_map(|b| match b {
            Block::Input(input) => Some(input),
            _ => None,
        })
    }

    #[test]
    fn new_mail_is_drawn_as_a_message_with_a_box_to_reply_in() {
        let held = held(notice(), 45);
        let panel = draw(&held);
        assert_eq!(panel.id, "mail:m1");
        assert_eq!(panel.owner, Owner::Mail);
        assert_eq!(panel.kind, Kind::Mail);
        assert_eq!(panel.kicker, "New mail");
        assert_eq!(panel.title, "The proof");
        let Block::Mail(mail) = &panel.blocks[0] else { panic!("{:?}", panel.blocks) };
        assert_eq!(mail.from.name, "Ada Lovelace");
        assert_eq!(mail.from.email.as_deref(), Some("ada@example.com"));
        assert_eq!(mail.snippet.as_deref(), Some("Here is the second half."));
        assert_eq!(mail.received.as_deref(), Some("10:03 pm"));
        assert_eq!(mail.attachments, 2);
        let reply = input(&panel).expect("a box");
        assert_eq!(reply.id, "reply");
        assert_eq!(reply.placeholder, "Reply to Ada\u{2026}");
        assert_eq!(reply.submit, "Send");
        assert!(reply.multiline && !reply.busy && reply.error.is_none());
        assert_eq!(ids(&panel), ["archive", "read", "open"]);
        assert!(panel.actions.iter().all(|a| a.icon.is_some()), "each with its mark");
        assert_eq!(panel.expires, Some(expiry(45_000)));
    }

    #[test]
    fn suggestions_and_the_address_it_came_to_are_drawn_when_they_come() {
        let mut held = held(notice(), 45);
        held.suggestions = vec![Suggestion { label: "Thanks".into(), text: "Thanks, Ada!".into() }];
        held.account = Some("hari+maths@example.com".into());
        let panel = draw(&held);
        assert_eq!(input(&panel).unwrap().suggestions.len(), 1);
        let Block::Mail(mail) = &panel.blocks[0] else { panic!() };
        assert_eq!(mail.account.as_deref(), Some("hari+maths@example.com"));
        assert_eq!(panel.expires, Some(expiry(45_000)), "the same expiry: the time is not reset");
    }

    #[test]
    fn a_reply_waits_then_offers_undo_then_says_sent() {
        let mut held = held(notice(), 45);
        held.stage = Stage::Replying("Yes, thanks".into());
        let panel = draw(&held);
        assert!(input(&panel).unwrap().busy, "the box waits");
        assert_eq!(panel.expires, None, "and the card does not go meanwhile");

        held.stage = Stage::Sending { draft: "d1".into(), text: "Yes, thanks".into() };
        let panel = draw(&held);
        assert!(input(&panel).is_none());
        assert!(panel.blocks.contains(&Block::Quote {
            label: Some("Your reply".into()),
            text: "Yes, thanks".into()
        }));
        assert!(
            panel.blocks.iter().any(|b| matches!(b, Block::Activity { mode: Mode::Sending, .. }))
        );
        assert_eq!(ids(&panel), ["undo"]);
        assert_eq!(panel.expires, None);

        held.stage = Stage::Undoing { draft: "d1".into(), text: "Yes, thanks".into() };
        let panel = draw(&held);
        assert!(panel.busy && panel.actions.is_empty(), "Undo pressed once");

        held.stage = Stage::Sent;
        let panel = draw(&held);
        assert_eq!(panel.tone, Tone::Success);
        assert!(panel.blocks.iter().any(|b| matches!(b, Block::Activity { mode: Mode::Done, .. })));
        assert_eq!(panel.expires, Some(expiry(DONE_MS)));
    }

    #[test]
    fn a_reply_that_did_not_go_says_why_in_the_box_and_undo_leaves_the_draft() {
        let mut held = held(notice(), 45);
        held.reply_error = Some("A message needs at least one recipient".into());
        let panel = draw(&held);
        assert_eq!(
            input(&panel).unwrap().error.as_deref(),
            Some("A message needs at least one recipient")
        );

        held.reply_error = None;
        held.stage = Stage::Stopped;
        let panel = draw(&held);
        assert_eq!(panel.body.as_deref(), Some("Sending stopped \u{2014} the draft is in Mail"));
        assert_eq!(ids(&panel), ["open"]);
        assert!(panel.expires.is_some());
    }

    #[test]
    fn trouble_is_said_under_the_title() {
        let mut held = held(notice(), 45);
        held.trouble = Some("The server is not answering".into());
        let panel = draw(&held);
        assert_eq!(panel.tone, Tone::Trouble);
        assert_eq!(panel.body.as_deref(), Some("The server is not answering"));
    }

    #[test]
    fn a_card_stays_up_long_enough_to_be_read() {
        let held = held(notice(), 0);
        assert_eq!(draw(&held).expires, Some(expiry(u64::from(LEAST_SECONDS) * 1000)));
    }

    #[test]
    fn a_snippet_that_says_the_subject_again_is_not_shown_and_a_long_one_is_cut() {
        let mut same = notice();
        same.snippet = "the proof".into();
        let Block::Mail(mail) = &draw(&held(same, 45)).blocks[0] else { panic!() };
        assert_eq!(mail.snippet, None);

        let mut long = notice();
        long.snippet = "word ".repeat(100);
        let Block::Mail(mail) = &draw(&held(long, 45)).blocks[0] else { panic!() };
        let snippet = mail.snippet.as_deref().unwrap();
        assert!(snippet.ends_with('\u{2026}') && snippet.chars().count() <= SNIPPET_CHARS + 1);

        let mut nameless = notice();
        nameless.from_name = String::new();
        nameless.subject = "  ".into();
        let panel = draw(&held(nameless, 45));
        assert_eq!(panel.title, "(no subject)");
        let Block::Mail(mail) = &panel.blocks[0] else { panic!() };
        assert_eq!(mail.from.name, "ada@example.com");
        assert_eq!(mail.from.email, None, "not said twice");
    }

    #[test]
    fn an_invitation_is_drawn_as_its_event_with_answers_to_press() {
        let mut with_invite = notice();
        with_invite.invite = Some(invite());
        with_invite.to_address = Some("me@example.com".into());
        let held = held(with_invite, 45);
        let panel = draw(&held);
        assert_eq!(panel.kind, Kind::Invite);
        assert_eq!(panel.kicker, "Invitation");
        assert_eq!(panel.title, "Design sync");
        assert!(input(&panel).is_none(), "an invitation is answered, not replied to");
        assert_eq!(ids(&panel), ["accepted", "tentative", "declined"]);
        assert!(panel.actions[0].primary && panel.actions[2].danger);
        let Block::Event(event) = &panel.blocks[0] else { panic!("{:?}", panel.blocks) };
        assert_eq!(event.when, "23:13\u{2013}23:43");
        assert_eq!(event.date.as_deref(), Some("Today"));
        assert_eq!(event.place.as_deref(), Some("Room 4"));
        assert_eq!(event.rsvp, Some(Rsvp::NeedsAction));
        let names: Vec<&str> = event.people.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["Grace Hopper", "me@example.com", "Edsger"],
            "the organiser once, first"
        );
        assert!(event.people[0].organizer);
        assert_eq!(event.people[0].status, Some(Rsvp::Accepted));
        assert!(event.people[1].you);
        assert_eq!(event.people[2].status, Some(Rsvp::Declined));
        assert!(
            panel.blocks.contains(&Block::Text {
                heading: None,
                text: "Sent by Ada Lovelace".into(),
                folded: false
            }),
            "it came from somebody who is not its organiser"
        );

        let mut answered = held.clone();
        answered.stage = Stage::Answered(Rsvp::Tentative);
        let panel = draw(&answered);
        assert_eq!(panel.tone, Tone::Success);
        assert!(panel.actions.is_empty());
        let Block::Event(event) = &panel.blocks[0] else { panic!() };
        assert_eq!(event.rsvp, Some(Rsvp::Tentative), "the answer just given");
        assert_eq!(panel.expires, Some(expiry(DONE_MS)));
    }

    #[test]
    fn an_invitation_on_a_call_names_the_service_and_an_all_day_one_its_own_day() {
        let mut on_a_call = invite();
        on_a_call.location = Some("https://meet.google.com/abc-defg-hij".into());
        on_a_call.organizer = "Ada Lovelace".into();
        let mut with_invite = notice();
        with_invite.invite = Some(on_a_call);
        let panel = draw(&held(with_invite.clone(), 45));
        let Block::Event(event) = &panel.blocks[0] else { panic!() };
        assert_eq!(event.join.as_deref(), Some("Google Meet"));
        assert_eq!(event.place, None);
        assert!(
            !panel.blocks.iter().any(|b| matches!(b, Block::Text { .. })),
            "sent by its organiser: nothing to add"
        );

        let mut all_day = invite();
        all_day.all_day = true;
        // Midnight UTC on Thursday 16 November: the day itself, wherever this is.
        all_day.start = Some(ts(1_700_092_800));
        let zone = TimeZone::fixed(jiff::tz::offset(-8));
        assert_eq!(when_words(&all_day, &zone), "All day");
        assert_eq!(invite_day(&all_day, &zone), Some(jiff::civil::date(2023, 11, 16)));
    }

    #[test]
    fn days_and_times_are_said_as_a_person_would() {
        let today = jiff::civil::date(2023, 11, 14);
        assert_eq!(day_words(today, today), "Today");
        assert_eq!(day_words(jiff::civil::date(2023, 11, 15), today), "Tomorrow");
        assert_eq!(day_words(jiff::civil::date(2023, 11, 23), today), "Thu 23 Nov");
        assert_eq!(received_words(ts(NOW - 600), ts(NOW), &TimeZone::UTC), "10:03 pm");
        assert_eq!(received_words(ts(NOW - 86_400), ts(NOW), &TimeZone::UTC), "Mon 10:13 pm");

        let mut late = invite();
        late.start = Some(ts(NOW + 2 * 3_600));
        late.end = Some(ts(NOW + 4 * 3_600));
        assert_eq!(when_words(&late, &TimeZone::UTC), "00:13\u{2013}02:13");
        late.start = Some(ts(NOW));
        assert_eq!(when_words(&late, &TimeZone::UTC), "22:13\u{2013}Wed 02:13");
    }

    #[test]
    fn the_box_greets_the_sender_by_their_first_name() {
        assert_eq!(first_name("Ada Lovelace"), "Ada");
        assert_eq!(first_name("Lovelace, Ada"), "Ada");
        assert_eq!(first_name(" \"Grace Hopper\" "), "Grace");
        assert_eq!(first_name("ada@example.com"), "ada@example.com");
        assert_eq!(first_name(""), "");
        let mut nameless = notice();
        nameless.from_name = String::new();
        assert_eq!(reply_box(&held(nameless, 45)).placeholder, "Reply\u{2026}");
    }

    #[test]
    fn typed_words_become_the_editors_lines_escaped() {
        assert_eq!(
            text_as_lines("Yes <b>please</b> & thanks"),
            "<p>Yes &lt;b&gt;please&lt;/b&gt; &amp; thanks</p>"
        );
        assert_eq!(
            text_as_lines("\n\nHi Ada,\r\n\r\nSee you then.\nH\n\n"),
            "<p>Hi Ada,</p><p></p><p>See you then.</p><p>H</p>"
        );
        assert_eq!(text_as_lines("  \n "), "");
    }

    #[test]
    fn a_reply_goes_above_the_signature_and_the_quote() {
        let signature = r#"<div data-signature=""><p>Hari</p></div>"#;
        let quote = r#"<p>On Tue, 14 Nov 2023 at 22:03, Ada Lovelace wrote:</p><blockquote type="cite"><p>Here is the proof.</p></blockquote>"#;
        let draft = format!("<p></p>{signature}{quote}");
        assert_eq!(split_quoted(&draft), (format!("<p></p>{signature}").as_str(), quote));
        assert_eq!(reply_body(&draft, "Thanks!"), format!("<p>Thanks!</p>{signature}{quote}"));

        // No signature: the words, then the quote.
        let bare = format!("<p></p>{quote}");
        assert_eq!(reply_body(&bare, "Thanks!"), format!("<p>Thanks!</p>{quote}"));

        // No quote either -- the message could not be read -- just the words.
        assert_eq!(reply_body("", "Thanks!"), "<p>Thanks!</p>");

        // Words that begin like an attribution are the person's own.
        let tricky = reply_body(&draft, "On Monday we could meet.");
        assert!(tricky.starts_with("<p>On Monday we could meet.</p><div data-signature"));
        assert_eq!(with_own_words("<p>old</p>", "<p>new</p>"), "<p>new</p>");
    }

    #[test]
    fn the_accounts_say_whether_replies_may_be_written_and_which_address_to_name() {
        let accounts = json!([
            { "id": "acct", "address": "hari@example.com", "services": { "mail": true },
              "mailAi": { "writing": true } },
            { "id": "other", "address": "me@work.com", "services": { "mail": true },
              "mailAi": { "writing": false } },
        ]);
        let facts = account_facts(&accounts, &notice());
        assert_eq!(
            facts,
            AccountFacts {
                writing: true,
                label: Some("hari@example.com".into()),
                own: vec!["hari@example.com".into()],
            },
            "two mail accounts: say which"
        );

        let one = json!([{ "id": "acct", "address": "hari@example.com",
                           "services": { "mail": true }, "mailAi": {} }]);
        assert_eq!(
            account_facts(&one, &notice()),
            AccountFacts { own: vec!["hari@example.com".into()], ..AccountFacts::default() },
            "one account with one address: nothing to name, but who the person is"
        );

        let mut to_alias = notice();
        to_alias.to_address = Some("hari+maths@example.com".into());
        assert_eq!(
            account_facts(&one, &to_alias).label.as_deref(),
            Some("hari+maths@example.com"),
            "the address it came to, where the account has several"
        );
        assert_eq!(account_facts(&json!(null), &notice()), AccountFacts::default());
    }

    #[test]
    fn suggested_replies_become_the_boxs_ready_made_texts() {
        let written = json!({ "suggestions": [
            { "label": "Sounds good", "bodyText": "Sounds good, see you then.", "bodyHtml": "<p>x</p>" },
            { "label": "", "bodyText": "nameless" },
            { "label": "Later", "bodyText": "Can we do Friday?" },
            { "label": "Three", "bodyText": "c" },
            { "label": "Four", "bodyText": "d" },
        ]});
        let got = suggestions_from(&written);
        let labels: Vec<&str> = got.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["Sounds good", "Later", "Three"], "named ones, three at most");
        assert_eq!(got[0].text, "Sounds good, see you then.");
        assert!(suggestions_from(&json!({})).is_empty());
    }

    #[test]
    fn answers_and_errors_read_as_words() {
        assert_eq!(rsvp_of("accepted"), Some(Rsvp::Accepted));
        assert_eq!(rsvp_of("tentative"), Some(Rsvp::Tentative));
        assert_eq!(rsvp_of("declined"), Some(Rsvp::Declined));
        assert_eq!(rsvp_of("open"), None);
        assert_eq!(rsvp_from(AttendeeResponse::NeedsAction), Rsvp::NeedsAction);
        assert_eq!(
            sentence("a message needs at least one recipient"),
            "A message needs at least one recipient"
        );
        assert_eq!(sentence("  "), "Something went wrong");
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("a long line of words", 6), "a long\u{2026}");
    }

    #[test]
    fn while_a_reply_is_made_the_card_offers_nothing_that_would_drop_it() {
        let mut held = held(notice(), 45);
        held.stage = Stage::Replying("Yes, thanks".into());
        let panel = draw(&held);
        assert!(panel.actions.is_empty(), "no Archive, Mark read or Open to press meanwhile");
        assert!(input(&panel).unwrap().busy);

        held.stage = Stage::Working;
        assert_eq!(ids(&draw(&held)), ["archive", "read", "open"], "busy, but still there");

        assert!(may_open(&Stage::Reading));
        assert!(may_open(&Stage::Stopped));
        assert!(!may_open(&Stage::Replying("x".into())));
        assert!(!may_open(&Stage::Sending { draft: "d".into(), text: "x".into() }));
        assert!(!may_open(&Stage::Undoing { draft: "d".into(), text: "x".into() }));
        let checked = |going| Stage::Checked {
            draft: "d".into(),
            text: "x".into(),
            findings: Findings::default(),
            going,
        };
        assert!(may_open(&checked(false)), "the held draft, opened to be changed");
        assert!(!may_open(&checked(true)), "but not while it is being sent anyway");
    }

    #[test]
    fn the_checks_answer_says_whether_a_reply_goes() {
        assert_eq!(verdict(&json!({ "results": [] })), Verdict::Go { off: None }, "none on");
        assert_eq!(
            verdict(&json!({ "results": [{ "check": "Friendly", "passed": true }] })),
            Verdict::Go { off: None }
        );
        assert_eq!(
            verdict(&json!({ "results": [], "off": " Writing help is off for this account. " })),
            Verdict::Go { off: Some("Writing help is off for this account.".into()) },
            "cannot run as things are: said, but not held for"
        );
        assert_eq!(
            verdict(&json!({ "results": [
                { "check": "Friendly", "passed": true },
                { "check": "Mentions the attachment", "passed": false, "note": "Say which file." },
                { "check": "Has a date", "passed": false, "note": "" },
                { "passed": "maybe" },
            ]})),
            Verdict::Hold(Findings {
                failed: vec![
                    ("Mentions the attachment".into(), Some("Say which file.".into())),
                    ("Has a date".into(), None),
                    ("A check".into(), None),
                ],
                skipped: None,
            }),
            "a result that does not say it passed did not"
        );
        assert_eq!(
            verdict(&json!({ "results": [], "skipped": "The checks could not run: slow." })),
            Verdict::Hold(Findings {
                failed: Vec::new(),
                skipped: Some("The checks could not run: slow.".into()),
            }),
            "never taken for a pass"
        );
    }

    #[test]
    fn a_reply_the_checks_held_waits_to_be_sent_anyway_or_opened() {
        let mut held = held(notice(), 45);
        held.stage = Stage::Checked {
            draft: "d1".into(),
            text: "Yes, thanks".into(),
            findings: Findings {
                failed: vec![("Mentions the attachment".into(), Some("Say which file.".into()))],
                skipped: None,
            },
            going: false,
        };
        let panel = draw(&held);
        assert_eq!(panel.tone, Tone::Attention);
        assert!(input(&panel).is_none(), "the words are in the draft now");
        assert!(panel.blocks.contains(&Block::Quote {
            label: Some("Your reply".into()),
            text: "Yes, thanks".into()
        }));
        assert!(panel.blocks.contains(&Block::Text {
            heading: Some("1 check didn't pass".into()),
            text: "Mentions the attachment \u{2014} Say which file.\nYour reply is kept as a draft."
                .into(),
            folded: false,
        }));
        assert_eq!(ids(&panel), ["open", "sendAnyway"]);
        assert!(panel.actions[0].primary && !panel.actions[1].primary);
        assert_eq!(panel.actions[0].label, "Open draft");
        assert!(panel.expires.is_some(), "it may go: the draft stays in Mail");

        if let Stage::Checked { going, .. } = &mut held.stage {
            *going = true;
        }
        let panel = draw(&held);
        assert!(panel.busy && panel.actions.is_empty() && panel.expires.is_none());

        assert!(was_going(&held.stage, "d1", "Yes, thanks"));
        assert!(!was_going(&held.stage, "d2", "Yes, thanks"));
        assert!(was_going(&Stage::Replying("Yes".into()), "d9", "Yes"));
        assert!(!was_going(&Stage::Reading, "d1", "Yes"));
    }

    #[test]
    fn what_the_checks_found_reads_as_compose_says_it() {
        let two = Findings {
            failed: vec![("Friendly".into(), None), ("Has a date".into(), Some("Add one.".into()))],
            skipped: None,
        };
        assert_eq!(findings_heading(&two), "2 checks didn't pass");
        assert_eq!(
            findings_text(&two),
            "Friendly\nHas a date \u{2014} Add one.\nYour reply is kept as a draft."
        );
        let skipped = Findings { failed: Vec::new(), skipped: Some("Too slow.".into()) };
        assert_eq!(findings_heading(&skipped), "Checks couldn't run");
        assert_eq!(findings_text(&skipped), "Too slow.\nYour reply is kept as a draft.");
    }

    #[test]
    fn checks_that_cannot_run_are_said_while_the_reply_goes() {
        let mut held = held(notice(), 45);
        held.stage = Stage::Sending { draft: "d1".into(), text: "Yes".into() };
        held.checks_off = Some("Writing help is off for this account.".into());
        assert_eq!(draw(&held).body.as_deref(), Some("Writing help is off for this account."));
    }

    #[test]
    fn only_a_card_nobody_touched_is_announced_when_it_goes() {
        let mut held = held(notice(), 45);
        assert!(untouched(&held));
        held.acted = true;
        held.reply_error = Some("No recipient".into());
        assert!(!untouched(&held), "a reply that failed was still something done");
        held.acted = false;
        held.stage = Stage::Stopped;
        assert!(!untouched(&held));
    }

    #[test]
    fn the_person_is_told_apart_among_the_guests_by_their_own_mailboxes() {
        // One address: the service names no address it came to.
        let mut with_invite = notice();
        with_invite.invite = Some(invite());
        let mut held = held(with_invite, 45);
        let Block::Event(event) = &draw(&held).blocks[0] else { panic!() };
        assert!(event.people.iter().all(|p| !p.you), "not known until the accounts are read");

        // The account's own address, as the accounts say it, and a tag on it.
        let accounts = json!([{ "id": "acct", "address": "Me+cal@Example.com",
                                "identities": [{ "name": "Me", "address": "me@work.org" }],
                                "services": { "mail": true } }]);
        held.own = account_facts(&accounts, &held.notice).own;
        assert_eq!(held.own, ["me@example.com", "me@work.org"]);
        let Block::Event(event) = &draw(&held).blocks[0] else { panic!() };
        let you: Vec<&str> =
            event.people.iter().filter(|p| p.you).map(|p| p.name.as_str()).collect();
        assert_eq!(you, ["me@example.com"]);
    }
}
