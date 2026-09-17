//! What the service says without being asked.
//!
//! Three kinds of thing, one trait. The shell turns them into window events;
//! the server turns them into server-sent events; a test collects them in a
//! `Vec`. Nothing that raises one knows which of those is listening, which is
//! the property that lets the same command body serve a window and a wire.
//!
//! # Notifications
//!
//! The shell does work nobody asked for and nobody is watching -- refreshing
//! a subscribed calendar on a timer, most of all -- and before this existed
//! the only thing it could do about a result was write a log line.
//!
//! It only *emits*. Whether a notification should become a banner in the
//! operating system or a toast inside the window depends on whether the
//! window is in front of the user, on whether the notification carries a
//! button, and on whether permission was ever granted -- and the interface is
//! the only side that knows the first two. Answering that in two places would
//! mean answering it differently within a release. So this states the facts
//! and `ui/src/lib/notify.svelte.ts` decides where they go.
//!
//! # Changes
//!
//! A write that landed, named by what it touched. One window's save is
//! another window's stale list, and this is what lets the second one find
//! out. Stamped with the caller that made it so a client is not told about
//! its own writes and does not flicker.
//!
//! # Lock state
//!
//! The vault locked or unlocked. Not a change to a record and not a
//! notification: every client has to leave the screen it is on.
//!
//! # Meeting offers
//!
//! "Take notes for Design sync?" is neither a [`Change`] -- nothing has been
//! written, and an "ask" offer may never be acted on -- nor a plain
//! [`Notification`], which carries no structure for a listener to draw a
//! banner from (a title, a time range, which calendar) or to act on (the
//! desktop shell starting capture itself when "Always" chose for you). It
//! gets its own event, [`MeetingOffer`], raised by
//! `everyday_service::meeting::watch` alongside an ordinary [`Notification`]
//! with [`Reach::User`] so the offer still reaches somebody who is not
//! looking at the window.

use everyday_core::id::{CalendarId, EventId};
use everyday_core::record::RecordKind;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// How loud a notification is. Mirrors `NotifyLevel` in the interface.
///
/// All four variants exist because this is one half of a wire contract, and a
/// mirror with holes in it is worse than no mirror.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

/// How far a notification has to travel to have done its job.
///
/// `App` is about something the user is looking at and never leaves the
/// window. `User` is for what must reach a person who may not have this app
/// in front of them, and is the only reach allowed out to the operating
/// system. Defaulting to `App` is what keeps a chatty background task from
/// becoming a chatty notification centre.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reach {
    App,
    User,
}

/// What the service hands a listener. Mirrors `ShellNotification` in
/// `ui/src/lib/types.ts`.
///
/// No action button, on purpose: the service can say a calendar stopped
/// answering, but "Open settings" is a thing only the interface can do, and a
/// payload that could carry a callback would be a payload that could carry
/// anything.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    pub level: Level,
    pub reach: Reach,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    /// Identity for a condition that can recur, so a fault that is still a
    /// fault replaces its own message rather than stacking a second copy.
    #[serde(default)]
    pub key: Option<String>,
}

impl Notification {
    pub fn new(level: Level, title: impl Into<String>) -> Self {
        Self { level, reach: Reach::App, title: title.into(), body: None, key: None }
    }

    pub fn warning(title: impl Into<String>) -> Self {
        Self::new(Level::Warning, title)
    }

    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Mark this as something that must reach the user even if they are not
    /// looking at the window.
    pub fn for_user(mut self) -> Self {
        self.reach = Reach::User;
        self
    }

    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }
}

/// What a write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Op {
    Created,
    Updated,
    Deleted,
}

/// What a write touched.
///
/// Coarser than a table on purpose. A listener uses this to decide which list
/// to reload, and the lists in the interface are per app, not per table -- so
/// "something in the task domain moved" is the useful granularity and "row
/// 4f3a of `time_blocks` was updated" is not.
///
/// Adding an app adds a variant here, and the interface's router gains one
/// arm. That is the whole cost of a new domain on this side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Journal,
    Entry,
    /// Writing that is not a day. See `everyday_core::note`.
    Note,
    Project,
    Task,
    Block,
    Calendar,
    Event,
    Shelf,
    Item,
    Log,
    Tracker,
    Reading,
    /// Who you are being. The axis every balance chart is drawn against.
    Role,
    /// An outcome under a role.
    Goal,
    /// A mailbox provider signed in to. See `everyday_core::account`.
    Account,
    /// A folder or a Gmail label, synced from an account. See
    /// `everyday_core::mail`.
    Mailbox,
    /// A conversation of mail messages -- not to be confused with
    /// [`Kind::Conversation`], the assistant's own.
    Thread,
    /// A message being written -- by a person, by the assistant, or by the
    /// auto-draft pass phase 7 adds.
    Draft,
    Conversation,
    /// The assistant's standing work.
    Routine,
    /// One run of it. What the count on the app bar is drawn from.
    RoutineRun,
    /// Work the assistant prepared and did not do. See
    /// `everyday_core::proposal`.
    Proposal,
    Memory,
    /// A call being recorded, or the history row of one that was. See
    /// `everyday_core::meeting`.
    Recording,
    /// What was said in a call, kept beside its note.
    Transcript,
    /// A voice the meetings feature can recognise.
    Voiceprint,
    /// The vault's own settings: auto-lock, the assistant's configuration.
    Settings,
    /// One of [`crate::supervisor::Supervisor`]'s long-lived keyed tasks --
    /// today nothing but the tests, since phase 0 registers no factories;
    /// later, one IMAP account's sync.
    ///
    /// Not tied to any command's `change:` -- see `command.rs`'s module
    /// doc -- because nothing calls one yet. `ui/scripts/gen-api.mjs`
    /// therefore leaves it out of the generated `ChangeKind` union until a
    /// command exposes a task's status; the event still carries the string
    /// correctly over the wire in the meantime, a client simply has nothing
    /// typed to switch on. This is deliberate groundwork, not an oversight:
    /// the variant has to exist before the first task does, and the first
    /// task is mail's, not phase 0's.
    BackgroundTask,
}

/// [`Kind`] is coarser than [`RecordKind`] on the mail side (a mail message
/// or a mail op never gets its own change event -- both are reported as
/// `Kind::Thread`, see `domains::mail`) and carries two things that are not
/// records at all (`Settings`, `BackgroundTask`). So neither direction is
/// total; both are still an exhaustive match with no wildcard, so a variant
/// added to either enum without a line here fails to compile rather than
/// silently falling through.
impl TryFrom<Kind> for RecordKind {
    type Error = ();

    fn try_from(kind: Kind) -> Result<RecordKind, ()> {
        match kind {
            Kind::Journal => Ok(RecordKind::Journal),
            Kind::Entry => Ok(RecordKind::Entry),
            Kind::Note => Ok(RecordKind::Note),
            Kind::Project => Ok(RecordKind::Project),
            Kind::Task => Ok(RecordKind::Task),
            Kind::Block => Ok(RecordKind::Block),
            Kind::Calendar => Ok(RecordKind::Calendar),
            Kind::Event => Ok(RecordKind::Event),
            Kind::Shelf => Ok(RecordKind::Kind),
            Kind::Item => Ok(RecordKind::Item),
            Kind::Log => Ok(RecordKind::Log),
            Kind::Tracker => Ok(RecordKind::Tracker),
            Kind::Reading => Ok(RecordKind::Reading),
            Kind::Role => Ok(RecordKind::Role),
            Kind::Goal => Ok(RecordKind::Goal),
            Kind::Account => Ok(RecordKind::Account),
            Kind::Mailbox => Ok(RecordKind::Mailbox),
            Kind::Thread => Ok(RecordKind::Thread),
            Kind::Draft => Ok(RecordKind::Draft),
            Kind::Conversation => Ok(RecordKind::Conversation),
            Kind::Routine => Ok(RecordKind::Routine),
            Kind::RoutineRun => Ok(RecordKind::RoutineRun),
            Kind::Proposal => Ok(RecordKind::Proposal),
            Kind::Memory => Ok(RecordKind::Memory),
            Kind::Recording => Ok(RecordKind::Recording),
            Kind::Transcript => Ok(RecordKind::Transcript),
            Kind::Voiceprint => Ok(RecordKind::Voiceprint),
            // Not a record: the vault's own settings.
            Kind::Settings => Err(()),
            // Not a record: a supervisor task's status.
            Kind::BackgroundTask => Err(()),
        }
    }
}

/// The reverse of [`TryFrom<Kind> for RecordKind`]. `Err(())` for the three
/// kinds no `Change` is ever raised for by their own name: a mail message
/// and a mail op are both folded into `Kind::Thread`, and the assistant's
/// own message is folded into `Kind::Conversation` -- see `agent.rs`'s
/// `written` and `domains::mail`.
impl TryFrom<RecordKind> for Kind {
    type Error = ();

    fn try_from(kind: RecordKind) -> Result<Kind, ()> {
        match kind {
            RecordKind::Journal => Ok(Kind::Journal),
            RecordKind::Entry => Ok(Kind::Entry),
            RecordKind::Note => Ok(Kind::Note),
            RecordKind::Project => Ok(Kind::Project),
            RecordKind::Task => Ok(Kind::Task),
            RecordKind::Block => Ok(Kind::Block),
            RecordKind::Calendar => Ok(Kind::Calendar),
            RecordKind::Event => Ok(Kind::Event),
            RecordKind::Kind => Ok(Kind::Shelf),
            RecordKind::Item => Ok(Kind::Item),
            RecordKind::Log => Ok(Kind::Log),
            RecordKind::Tracker => Ok(Kind::Tracker),
            RecordKind::Reading => Ok(Kind::Reading),
            RecordKind::Role => Ok(Kind::Role),
            RecordKind::Goal => Ok(Kind::Goal),
            RecordKind::Account => Ok(Kind::Account),
            RecordKind::Mailbox => Ok(Kind::Mailbox),
            RecordKind::Thread => Ok(Kind::Thread),
            RecordKind::Draft => Ok(Kind::Draft),
            RecordKind::Conversation => Ok(Kind::Conversation),
            RecordKind::Routine => Ok(Kind::Routine),
            RecordKind::RoutineRun => Ok(Kind::RoutineRun),
            RecordKind::Proposal => Ok(Kind::Proposal),
            RecordKind::Memory => Ok(Kind::Memory),
            RecordKind::Recording => Ok(Kind::Recording),
            RecordKind::Transcript => Ok(Kind::Transcript),
            RecordKind::Voiceprint => Ok(Kind::Voiceprint),
            // Folded into `Kind::Thread` -- see this impl's docs.
            RecordKind::MailMessage | RecordKind::Op => Err(()),
            // Folded into `Kind::Conversation` -- see this impl's docs.
            RecordKind::Message => Err(()),
        }
    }
}

/// Does a [`Change`] announced as `kind` cover a write the collector saw
/// against `record`?
///
/// Almost always the single [`RecordKind`] `TryFrom<Kind>` names -- but
/// `Kind` is coarser than `RecordKind` in exactly the two places
/// `TryFrom<RecordKind> for Kind`'s own doc names: a mail message and an
/// `Op` both surface as `Kind::Thread`, and the assistant's own message
/// surfaces as `Kind::Conversation`. Shared by [`command::assert_declared_matches_touched`](crate::command)
/// and [`emit_touched`], so the two cannot answer this question
/// differently from each other.
pub(crate) fn kind_covers(kind: Kind, record: RecordKind) -> bool {
    match kind {
        Kind::Thread => {
            matches!(record, RecordKind::Thread | RecordKind::MailMessage | RecordKind::Op)
        }
        Kind::Conversation => matches!(record, RecordKind::Conversation | RecordKind::Message),
        other => RecordKind::try_from(other) == Ok(record),
    }
}

/// Raise one [`Change`] for every id the collector saw touched under
/// `kind`'s own family (see [`kind_covers`]), batching them exactly the way
/// a command's own `change:` row does: `id` alone for one, `ids` for more
/// than one, and nothing at all when there is nothing to report.
///
/// What replaces a hand-built [`Change`] whose `id`/`ids` used to be
/// computed from a return value or a closure's own bookkeeping -- see
/// `docs/plans/architecture-refactor.md`'s Phase 7. The *kind* and *op* are
/// still named at the call site: which family of record a write announces,
/// and whether it reads as created, updated or deleted, are decisions this
/// function has no business making.
pub fn emit_touched(
    sink: &dyn EventSink,
    origin: Option<String>,
    touched: &[(RecordKind, String)],
    kind: Kind,
    op: Op,
) {
    let mut ids: Vec<String> =
        touched.iter().filter(|(k, _)| kind_covers(kind, *k)).map(|(_, id)| id.clone()).collect();
    match ids.len() {
        0 => {}
        1 => sink.changed(Change { kind, op, id: ids.pop(), ids: Vec::new(), origin }),
        _ => sink.changed(Change { kind, op, id: None, ids, origin }),
    }
}

/// One write, on its way to everyone who did not make it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub kind: Kind,
    pub op: Op,
    /// The record, when there is exactly one. Absent for a batch -- a board
    /// reorder writes forty tasks and the useful statement is "tasks moved".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The records, when a single write touched more than one -- the board
    /// reorder above, or a mailbox's sync landing a page of messages at
    /// once. Empty rather than `id` filled with the first of many: a client
    /// that only reads `id` must not quietly act as though the rest were
    /// never written, and an empty array is what `#[serde(default)]` gives
    /// an older client that has never heard of this field, which is exactly
    /// the "batch: no single id" case it already had to handle.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
    /// Who made it. A client compares this with its own id and ignores a
    /// match, so a save does not make the window that saved it reload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

impl Change {
    pub fn new(kind: Kind, op: Op) -> Self {
        Self { kind, op, id: None, ids: Vec::new(), origin: None }
    }
}

/// "Take notes for Design sync?" -- a detected call starting now. Mirrors
/// the TS `MeetingOfferPayload` in `ui/src/lib/api.ts`, minus the shell-only
/// `recordingId` that type also carries: this event exists before any
/// recording does, and the desktop shell is what fills that field in once
/// (and if) one starts.
///
/// Raised by `everyday_service::meeting::watch`, never stored: a listener
/// that missed one because no window was open missed nothing worth
/// recovering, unlike a [`Change`] to a record.
///
/// Carries `calendar_id` and `uid` alongside `event_id` on purpose:
/// `event_id` is what starts a recording (`begin_recording` looks the event
/// up by it, to read its attendees and title), but it is a feed event's own
/// id, not stable across a resync -- see `meeting::watch`'s own doc. A
/// dismissal that arrived after a resync had changed it would look up the
/// wrong event, or none. `dismiss_meeting_offer` is built against the pair
/// that *is* durable instead, the same one `meeting::watch::already_recorded`
/// already keys a recording's own history lookup by.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingOffer {
    pub event_id: EventId,
    pub calendar_id: CalendarId,
    pub uid: String,
    /// The event's own `Event::series` (`everyday_core::calendar::Event`),
    /// carried along so `dismiss_meeting_offer` can compute the same series
    /// key `meeting::watch`'s own tick checked the event against, rather
    /// than only ever being able to derive one from `uid` -- which, for a
    /// recurring Google or Graph event, is unique per occurrence and would
    /// only ever let "never for this meeting" skip the one occurrence it
    /// was pressed on. See `detect::series_key_of`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    pub title: String,
    pub start: Timestamp,
    pub end: Timestamp,
    pub calendar_name: String,
    /// Detected on a calendar set to "Always": the shell has already begun
    /// recording, and this is a toast rather than a question.
    pub automatic: bool,
}

/// Where the service's own remarks go.
///
/// Implemented by the shell (window events), the server (server-sent events)
/// and tests (a vector). The default implementations do nothing, so a
/// listener that cares about one kind is not obliged to write four methods.
pub trait EventSink: Send + Sync {
    fn notify(&self, notification: Notification) {
        let _ = notification;
    }

    fn changed(&self, change: Change) {
        let _ = change;
    }

    fn lock_state(&self, locked: bool) {
        let _ = locked;
    }

    fn meeting_offer(&self, offer: MeetingOffer) {
        let _ = offer;
    }
}

/// A sink that drops everything. What a service runs with until one is set.
pub struct Silent;

impl EventSink for Silent {}

#[cfg(test)]
mod tests {
    //! Phase 6's `Kind` <-> `RecordKind` conversions. Both `TryFrom` impls
    //! are exhaustive matches with no wildcard, so the compiler already
    //! proves every variant is accounted for; these tests pin *where* each
    //! one goes, and that the exceptions on both sides are exactly the ones
    //! the docs on those impls name.
    use super::*;

    const RECORD_KINDS: [Kind; 27] = [
        Kind::Journal,
        Kind::Entry,
        Kind::Note,
        Kind::Project,
        Kind::Task,
        Kind::Block,
        Kind::Calendar,
        Kind::Event,
        Kind::Shelf,
        Kind::Item,
        Kind::Log,
        Kind::Tracker,
        Kind::Reading,
        Kind::Role,
        Kind::Goal,
        Kind::Account,
        Kind::Mailbox,
        Kind::Thread,
        Kind::Draft,
        Kind::Conversation,
        Kind::Routine,
        Kind::RoutineRun,
        Kind::Proposal,
        Kind::Memory,
        Kind::Recording,
        Kind::Transcript,
        Kind::Voiceprint,
    ];

    #[test]
    fn every_record_backed_change_kind_round_trips_through_record_kind() {
        for kind in RECORD_KINDS {
            let record = RecordKind::try_from(kind).unwrap_or_else(|()| {
                panic!("{kind:?} is one of the 27 record-backed kinds and should convert")
            });
            assert_eq!(Kind::try_from(record), Ok(kind), "{kind:?}");
        }
    }

    #[test]
    fn settings_and_background_task_name_no_record() {
        assert_eq!(RecordKind::try_from(Kind::Settings), Err(()));
        assert_eq!(RecordKind::try_from(Kind::BackgroundTask), Err(()));
    }

    #[test]
    fn a_mail_message_a_mail_op_and_the_assistants_own_message_have_no_change_kind_of_their_own() {
        // All three are real record kinds -- see `RecordDescriptor` in
        // `everyday-core` -- but no `Change` is ever raised under their own
        // name; a mail message and a mail op both surface as
        // `Kind::Thread`, and the assistant's own message as
        // `Kind::Conversation`. See `TryFrom<RecordKind> for Kind`'s docs.
        assert_eq!(Kind::try_from(RecordKind::MailMessage), Err(()));
        assert_eq!(Kind::try_from(RecordKind::Op), Err(()));
        assert_eq!(Kind::try_from(RecordKind::Message), Err(()));
    }
}
