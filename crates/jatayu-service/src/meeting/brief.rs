//! Preparing a meeting's brief: gathering what the vault knows, writing it up,
//! and handing it to whoever shows it.
//!
//! The brief itself, and what it holds, is [`jatayu_core::meeting::brief`].
//! This is when and how one is made:
//!
//! * [`tick`], from the scheduler's minute, prepares one for every meeting
//!   with other people in it that starts within
//!   [`MeetingBriefSettings::lead_minutes`](jatayu_core::meeting::brief::MeetingBriefSettings),
//!   once per occurrence per session, and raises it through
//!   [`EventSink::meeting_brief`](crate::events::EventSink::meeting_brief) --
//!   the desktop shell puts it on the meeting card.
//! * [`prepare`] makes one on demand, for an event the person opened or asked
//!   the assistant about.
//! * [`cached`] answers with the one in memory, if there is one.
//! * [`for_assistant`] answers the assistant, with what it needs to know of
//!   how the brief was read.
//!
//! # Gathering is code, writing is a model
//!
//! What goes into a brief is found by code, not by asking a model to go and
//! look: the invitation (its agenda, its place), who is on it and what the
//! Contacts shelf says of each, the open tasks somebody there is to be told
//! about, recent mail with them, notes that mention them or the meeting, the
//! last meeting with the same people, and -- only when the setting allows -- a
//! web search for an organisation nothing else mentions. A model is then given
//! those facts, fenced as other people's writing, and asked for a short
//! summary, what is open between the person and the people coming, the
//! invitation's agenda, a line on each person and a few lines of context,
//! which are checked against the facts before they are kept
//! ([`jatayu_core::meeting::brief::parse_answer`]). With no model, or when
//! it fails, the facts are the brief, and a line in
//! [`MeetingBrief::gaps`](jatayu_core::meeting::brief::MeetingBrief::gaps)
//! says why.
//!
//! # What may be read, and what leaves the machine
//!
//! * **Mail** is read only from accounts the assistant may read on the model
//!   that writes the brief -- mail switched on, the person's acknowledgement
//!   for that provider, reading allowed -- the same three things
//!   [`crate::brief::extras`] asks before the daily brief reads any. With no
//!   such account, or no model, none is read, and the brief says so.
//! * **The web** is asked about an organisation only when
//!   [`MeetingBriefSettings::web`](jatayu_core::meeting::brief::MeetingBriefSettings)
//!   is on, only for an attendee's domain that is not a public mail provider
//!   or the person's own and that nothing gathered mentions, at most
//!   [`MAX_WEB_LOOKUPS`] of them, one search each, under a short timeout. What
//!   is sent is the domain -- `acme.com` -- and nothing else: not a person's
//!   name, not a word from mail.
//! * **The model** is the Medium tier, the same door the assistant's own turns
//!   use, so what it is shown is what the person has already let the assistant
//!   see.
//!
//! # Once per occurrence
//!
//! The offer's key, `event:{calendar}:{uid}`, is shared by every occurrence
//! of a repeating meeting, so a brief is kept for each occurrence by that key
//! *and its start*. [`Briefs`] remembers which occurrences have a brief, which
//! are being prepared and which have failed, so a minute's tick never starts
//! the same one twice, Tuesday's standup is prepared though Monday's was, and
//! one whose preparation keeps failing is given up on after
//! [`MAX_ATTEMPTS`]. A brief whose meeting has ended is let go, and
//! [`cached`], asked by the shared key, answers with the occurrence on now or
//! next: never one that is over, which would give today's card yesterday's
//! agenda. Preparation runs on a task of its own: the scheduler's minute never
//! waits on a model. The generation counter is what keeps a task that was
//! still writing when the vault locked from putting its brief -- mail, notes
//! and all -- back in memory after [`Briefs::clear`] emptied it.
//!
//! # What the assistant is told about mail
//!
//! A brief the assistant asks for ([`for_assistant`]) comes with the provider
//! its mail was read for ([`ForAssistant::mail_read_for`]), kept beside it:
//! the model the assistant is on when it is asked may not be the one the brief
//! was gathered under, and what was read for one provider is not the person's
//! to hand to another.
//!
//! See `docs/plans/panel.md`.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jatayu_core::account::Permission;
use jatayu_core::calendar::{Calendar, Event};
use jatayu_core::id::{AccountId, EventId, ThreadId};
use jatayu_core::keepintouch::match_guest;
use jatayu_core::llm::{ModelAccess, ModelTier};
use jatayu_core::mailsearch::{MailQuery, MailSearch};
use jatayu_core::meeting::brief::{
    self as mb, Facts, Guest, LAST_MEETING_DAYS, LastMeeting, MAIL_DAYS, MAX_MAIL, MAX_NOTES,
    MAX_WEB_LOOKUPS, MailFact, MeetingBrief, MeetingBriefSettings, NoteFact, PersonFacts, Waiting,
    WebFact, Written, answer_schema, key_for, parse_answer,
};
use jatayu_core::meeting::{MeetingSettings, Stage};
use jatayu_core::search::{Found, SearchScope};
use jatayu_core::store::calendars::EventQuery;
use jatayu_core::store::library::LogQuery;
use jatayu_core::store::meetings::RecordingQuery;
use jatayu_core::store::tasks::TaskQuery;
use jatayu_core::websearch::{SearchRequest, SearchResult, Source};
use jatayu_core::{Result as CoreResult, Vault};
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
use serde_json::Value;

use crate::error::{CommandError, CommandResult, codes};
use crate::service::{Service, blocking};

/// How long after a meeting has started its brief may still be prepared: the
/// watcher's own window for offering to take notes, since that is when the
/// card the brief rides on is up.
const LOOKBACK_SECS: i64 = 2 * 60;

/// The longest lead the tick honours, however far a setting says. The tick
/// reads the calendar a day either side of today.
const MAX_LEAD_MINUTES: u32 = 12 * 60;

/// How many times a meeting's brief is tried before the tick gives up on it
/// for the session. The calendar and the assistant can still ask for one.
const MAX_ATTEMPTS: u8 = 3;

/// How long a web search for an organisation may take before the brief goes
/// without it. A brief arriving at ten to is worth more than a brief with a
/// company's blurb at ten past.
const WEB_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the model may take to write a brief. Generous: nobody is waiting
/// on it, and a model told to think takes a while.
const WRITE_TIMEOUT: Duration = Duration::from_secs(180);

/// How many of the people coming have their mail searched.
const MAIL_PEOPLE: usize = 4;

/// How many of the people coming have their name searched for in notes.
const NOTE_PEOPLE: usize = 4;

/// The fewest characters a name or a title may have to be worth a search.
const MIN_NAME_CHARS: usize = 5;

/// Titles too common to say anything: a search for one finds every note that
/// ever had a "sync" in it.
const GENERIC_TITLES: &[&str] = &[
    "sync",
    "1:1",
    "1-1",
    "meeting",
    "call",
    "catch up",
    "catch-up",
    "catchup",
    "check in",
    "check-in",
    "standup",
    "stand-up",
    "chat",
    "touch base",
    "weekly",
    "daily",
];

type Fut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The two things a brief asks of the outside world, so that a test can stand
/// where the internet and a model are. [`Live`] is the real ones.
pub(crate) trait Outside: Send + Sync {
    /// A web search for `organisation`, which is a domain and nothing else.
    fn search<'a>(&'a self, organisation: &'a str) -> Fut<'a, Result<Vec<SearchResult>, String>>;

    /// One structured ask of a model: `system`, `user`, and the schema of the
    /// answer to submit.
    fn write<'a>(
        &'a self,
        access: &'a ModelAccess,
        system: &'a str,
        user: &'a str,
        schema: Value,
    ) -> Fut<'a, Result<Value, String>>;
}

/// The internet and the person's own model.
pub(crate) struct Live;

impl Outside for Live {
    fn search<'a>(&'a self, organisation: &'a str) -> Fut<'a, Result<Vec<SearchResult>, String>> {
        Box::pin(async move {
            let request = SearchRequest::new(organisation).on(Source::Web).limit(5);
            crate::websearch::search(&request).await.map_err(|e| e.message)
        })
    }

    fn write<'a>(
        &'a self,
        access: &'a ModelAccess,
        system: &'a str,
        user: &'a str,
        schema: Value,
    ) -> Fut<'a, Result<Value, String>> {
        Box::pin(async move {
            let client = crate::llm::client_for(access)
                .map_err(|e| format!("could not reach the model: {e}"))?;
            crate::quick::run_prompt_within(
                client,
                &access.config.model,
                system,
                user,
                schema,
                WRITE_TIMEOUT,
            )
            .await
            .map_err(|e| e.message)
        })
    }
}

/// A brief in memory, with what goes with it: who is at the meeting -- the
/// identities its talking points are matched against, which are more than the
/// few people the brief describes -- and the provider under which any mail on
/// it was read.
#[derive(Clone)]
struct Held {
    brief: MeetingBrief,
    guests: Vec<Guest>,
    /// [`Made::mail_read_for`].
    mail_read_for: Option<String>,
}

/// One occurrence of a meeting: the key its offer and its brief share, and
/// when this one starts. A repeating meeting's occurrences share a key, so
/// the key alone would hold last Monday's brief where Tuesday's belongs.
type Occurrence = (String, i64);

fn occurrence(key: &str, start: Timestamp) -> Occurrence {
    (key.to_string(), start.as_second())
}

/// How long after an occurrence has started a failure to prepare it is
/// remembered, which is as long as anybody could still ask.
const FAILURE_MEMORY_SECS: i64 = 24 * 60 * 60;

#[derive(Default)]
struct Inner {
    held: HashMap<Occurrence, Held>,
    in_flight: HashSet<Occurrence>,
    failures: HashMap<Occurrence, u8>,
    /// Counted up by [`Briefs::clear`]. A preparation keeps the number it
    /// began under, and what it made is kept only if that is still the
    /// number.
    generation: u64,
}

impl Inner {
    /// Let go of what is over: a brief whose meeting has ended, and a failure
    /// from long before anybody could be waiting on it.
    fn prune(&mut self, now: Timestamp) {
        self.held.retain(|_, h| h.brief.end > now);
        self.failures.retain(|(_, start), _| *start > now.as_second() - FAILURE_MEMORY_SECS);
    }
}

/// The briefs prepared this session, one for each occurrence of a meeting
/// that has not yet ended. Not kept across a restart: a brief is cheap to
/// make again and goes stale anyway.
#[derive(Default)]
pub(crate) struct Briefs {
    inner: Mutex<Inner>,
}

impl Briefs {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("briefs lock")
    }

    /// The brief for the occurrence of `key` that is on now or comes next --
    /// the earliest start among those that have not ended -- never one that
    /// is over.
    fn held_now(&self, key: &str, now: Timestamp) -> Option<Held> {
        self.lock()
            .held
            .iter()
            .filter(|((k, _), h)| k == key && h.brief.end > now)
            .min_by_key(|((_, start), _)| *start)
            .map(|(_, h)| h.clone())
    }

    /// The brief for the occurrence of `key` that starts at `start`.
    fn held_at(&self, key: &str, start: Timestamp) -> Option<Held> {
        self.lock().held.get(&occurrence(key, start)).cloned()
    }

    /// Forget them all: the vault locked or closed.
    pub(crate) fn clear(&self) {
        let mut inner = self.lock();
        inner.held.clear();
        inner.in_flight.clear();
        inner.failures.clear();
        inner.generation += 1;
    }

    /// Let go of what is over as of `now`.
    fn prune(&self, now: Timestamp) {
        self.lock().prune(now);
    }

    fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// Claim the occurrence of `key` that starts at `start` for a
    /// preparation, answering the generation to finish it under; `None` when
    /// that occurrence has a brief, is being prepared, or has failed enough
    /// times. Another occurrence of the same meeting is another claim.
    fn begin(&self, key: &str, start: Timestamp) -> Option<u64> {
        let which = occurrence(key, start);
        let mut inner = self.lock();
        let tried = inner.failures.get(&which).copied().unwrap_or(0);
        if inner.held.contains_key(&which)
            || inner.in_flight.contains(&which)
            || tried >= MAX_ATTEMPTS
        {
            return None;
        }
        inner.in_flight.insert(which);
        Some(inner.generation)
    }

    /// Keep what a preparation made, and release its claim. `false` -- and
    /// nothing kept -- when the vault was locked in the meantime.
    fn finish(&self, generation: u64, made: Made, now: Timestamp) -> bool {
        let which = occurrence(&made.brief.key, made.brief.start);
        let mut inner = self.lock();
        inner.in_flight.remove(&which);
        if inner.generation != generation {
            return false;
        }
        inner.failures.remove(&which);
        inner.prune(now);
        inner.held.insert(which, made.into_held());
        true
    }

    /// Release a preparation's claim, counting it against the occurrence.
    fn fail(&self, key: &str, start: Timestamp, generation: u64) {
        let which = occurrence(key, start);
        let mut inner = self.lock();
        inner.in_flight.remove(&which);
        if inner.generation == generation {
            *inner.failures.entry(which).or_insert(0) += 1;
        }
    }

    /// Keep what a preparation made on request, not claimed by the tick --
    /// unless the vault locked since `generation`, or the meeting is already
    /// over.
    fn keep(&self, generation: u64, made: Made, now: Timestamp) {
        let which = occurrence(&made.brief.key, made.brief.start);
        let mut inner = self.lock();
        inner.prune(now);
        if inner.generation == generation && made.brief.end > now {
            inner.held.insert(which, made.into_held());
        }
    }

    /// Replace the talking points of the brief held for that occurrence.
    fn set_points(&self, key: &str, start: Timestamp, points: Vec<mb::TalkingPoint>) {
        if let Some(held) = self.lock().held.get_mut(&occurrence(key, start)) {
            held.brief.talking_points = points;
        }
    }
}

/// A brief made, with what goes with it. See [`Held`].
pub(crate) struct Made {
    brief: MeetingBrief,
    guests: Vec<Guest>,
    mail_read_for: Option<String>,
}

impl Made {
    fn into_held(self) -> Held {
        Held { brief: self.brief, guests: self.guests, mail_read_for: self.mail_read_for }
    }
}

/// A brief for the assistant, and what it has to know to decide what of it
/// may be shown.
#[derive(Debug, Clone)]
pub struct ForAssistant {
    pub brief: MeetingBrief,
    /// The provider acknowledgement -- [`ModelAccess::acknowledgement`] --
    /// of the model this brief was gathered under, when mail was read for it:
    /// the one the accounts' acknowledgements were matched against. `None`
    /// when no mail was read, so nothing on the brief came from mail.
    ///
    /// Not the model the assistant is on now, which may be another by the time
    /// the brief is asked for: mail read for one provider is not the
    /// person's to hand to a different one.
    pub mail_read_for: Option<String>,
}

// ---- when ---------------------------------------------------------------

/// One pass of the scheduler's minute: prepare the brief of every meeting
/// about to start that has none yet -- each occurrence of a repeating meeting
/// is its own -- and let go of those whose meeting is over. Returns once the work is started; the
/// writing goes on in tasks of their own and is raised when done.
pub async fn tick(svc: &Arc<Service>, vault: &Arc<Vault>) {
    tick_with(svc, vault, Arc::new(Live)).await;
}

/// [`tick`] over `outside`, which is the internet and a model unless a test
/// says otherwise.
pub(crate) async fn tick_with(svc: &Arc<Service>, vault: &Arc<Vault>, outside: Arc<dyn Outside>) {
    svc.meeting_briefs().prune(svc.now());
    let due = {
        let (svc, vault) = (svc.clone(), vault.clone());
        blocking(move || Ok(due_meetings(&svc, &vault))).await
    };
    let events = match due {
        Ok(events) => events,
        Err(e) => {
            tracing::warn!(error = %e.message, "meeting briefs: could not look for meetings");
            return;
        }
    };
    for event in events {
        let key = key_for(event.calendar_id, &event.uid);
        let start = event.start;
        let Some(generation) = svc.meeting_briefs().begin(&key, start) else { continue };
        let (svc, vault, outside) = (svc.clone(), vault.clone(), outside.clone());
        // On a task of its own: this is one meeting's, and one failing is
        // not a reason for another, or the minute, to wait.
        tokio::spawn(async move {
            match make(&svc, &vault, event, outside.as_ref()).await {
                Ok(made) => {
                    let raised = made.brief.clone();
                    if svc.meeting_briefs().finish(generation, made, svc.now()) {
                        svc.events().meeting_brief(raised);
                    }
                }
                Err(e) => {
                    svc.meeting_briefs().fail(&key, start, generation);
                    tracing::warn!(
                        error = %e.message,
                        meeting = %key,
                        "meeting briefs: could not prepare a brief"
                    );
                }
            }
        });
    }
}

/// The meetings the tick should prepare for: starting within the lead, or
/// only just begun, and a meeting by the watcher's own definition -- timed,
/// busy, not cancelled, somebody else on it -- on a calendar the settings let
/// through and not one the person said never to bother with.
fn due_meetings(svc: &Service, vault: &Vault) -> Vec<Event> {
    let found = || -> CoreResult<Vec<Event>> {
        if !vault.supports_meetings() || !vault.supports_calendars() {
            return Ok(Vec::new());
        }
        let settings = vault.meeting_settings()?;
        if !settings.brief.enabled {
            return Ok(Vec::new());
        }
        let now = svc.now();
        let lead = i64::from(settings.brief.lead_minutes.clamp(1, MAX_LEAD_MINUTES)) * 60;

        let today = now.to_zoned(TimeZone::system()).date();
        let events = vault.events(&EventQuery {
            from: today.checked_sub(Span::new().days(1)).ok(),
            to: today.checked_add(Span::new().days(1)).ok(),
            ..Default::default()
        })?;
        let mut due: Vec<Event> = events
            .into_iter()
            .filter(|e| {
                let starts = e.start.as_second();
                starts >= now.as_second() - LOOKBACK_SECS && starts <= now.as_second() + lead
            })
            .collect();
        // The accounts and calendars are read only when something is about
        // to start: most minutes nothing is.
        if due.is_empty() {
            return Ok(due);
        }
        let owners: Vec<String> = jatayu_core::brief::own_addresses(vault).into_iter().collect();
        let calendars = vault.calendars()?;
        due.retain(|e| is_briefable(&settings, &owners, &calendars, e));
        due.sort_by_key(|e| e.start);
        Ok(due)
    };
    found().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "meeting briefs: could not read the calendar");
        Vec::new()
    })
}

/// The watcher's own test of an event ([`super::watch::a_meeting_of_ours`]),
/// less its check for a recording already made: a brief is wanted before one
/// is.
fn is_briefable(
    settings: &MeetingSettings,
    owners: &[String],
    calendars: &[Calendar],
    event: &Event,
) -> bool {
    super::watch::a_meeting_of_ours(settings, owners, calendars, event).is_some()
}

// ---- asking for one -----------------------------------------------------

/// The brief in memory for `key` (`event:{calendarId}:{uid}`), if one was
/// prepared this session, with its talking points as the tasks stand now: a
/// point added since is on it, one ticked off is on it done.
///
/// A repeating meeting's occurrences share a key. This answers with the one on
/// now or coming next -- never one whose meeting is over, which would hand
/// Tuesday's card Monday's agenda and times.
///
/// Reads the vault's tasks, so a caller on the async runtime runs it on the
/// blocking pool, as the `meeting_brief` command does.
pub fn cached(svc: &Service, key: &str) -> Option<MeetingBrief> {
    let now = svc.now();
    svc.meeting_briefs().prune(now);
    let held = svc.meeting_briefs().held_now(key, now)?;
    Some(refreshed(svc, held).brief)
}

/// The brief in memory for the occurrence of `key` that starts at `start`,
/// as [`cached`] has it.
fn cached_at(svc: &Service, key: &str, start: Timestamp) -> Option<Held> {
    svc.meeting_briefs().prune(svc.now());
    let held = svc.meeting_briefs().held_at(key, start)?;
    Some(refreshed(svc, held))
}

/// `held` with its talking points as the tasks stand now, and the same kept.
fn refreshed(svc: &Service, mut held: Held) -> Held {
    if let Some(vault) = svc.get()
        && let Some(points) = fresh_points(&vault, &held.brief, &held.guests)
        && points != held.brief.talking_points
    {
        held.brief.talking_points = points.clone();
        svc.meeting_briefs().set_points(&held.brief.key, held.brief.start, points);
    }
    held
}

/// `brief`'s talking points brought up to date, or `None` when the tasks
/// could not be read.
fn fresh_points(
    vault: &Vault,
    brief: &MeetingBrief,
    guests: &[Guest],
) -> Option<Vec<mb::TalkingPoint>> {
    if !vault.supports_tasks() {
        return None;
    }
    let mut tasks = vault.tasks(&TaskQuery::open()).ok()?;
    // Whatever a held point names, in the state it is in now: a task ticked
    // off is no longer in the open ones.
    for point in &brief.talking_points {
        if !tasks.iter().any(|t| t.id == point.task_id)
            && let Ok(task) = vault.task(point.task_id)
        {
            tasks.push(task);
        }
    }
    Some(mb::refresh_points(&brief.talking_points, &tasks, guests))
}

/// Prepare the brief for `event` now, whether or not one is in memory, keep
/// it -- as that occurrence's, by its own start -- and answer with it.
pub async fn prepare(svc: &Arc<Service>, event: EventId) -> CommandResult<MeetingBrief> {
    prepare_with(svc, event, &Live).await
}

pub(crate) async fn prepare_with(
    svc: &Arc<Service>,
    event: EventId,
    outside: &dyn Outside,
) -> CommandResult<MeetingBrief> {
    // Not from the lock screen: a brief reads the vault and may ask the web
    // and a model, none of which is for a locked vault.
    let vault = svc.require_unlocked()?;
    let event = {
        let vault = vault.clone();
        blocking(move || Ok(vault.event(event)?)).await?
    };
    Ok(prepare_event(svc, &vault, event, outside).await?.brief)
}

/// [`prepare_with`], for an event already read.
async fn prepare_event(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    event: Event,
    outside: &dyn Outside,
) -> CommandResult<ForAssistant> {
    let generation = svc.meeting_briefs().generation();
    let made = make(svc, vault, event, outside).await?;
    let answer =
        ForAssistant { brief: made.brief.clone(), mail_read_for: made.mail_read_for.clone() };
    svc.meeting_briefs().keep(generation, made, svc.now());
    Ok(answer)
}

/// The brief the assistant is asked for: the one for `event`, or for the next
/// meeting `person` -- a name or an address -- is at. The one in memory for
/// that occurrence if there is one, otherwise prepared now.
pub async fn for_assistant(
    svc: &Arc<Service>,
    event: Option<EventId>,
    person: Option<&str>,
) -> CommandResult<ForAssistant> {
    let vault = svc.require_unlocked()?;
    let event = match (event, person) {
        (Some(id), _) => id,
        (None, Some(person)) => {
            let (vault, svc, person) = (vault.clone(), svc.clone(), person.to_string());
            blocking(move || next_meeting_with(&svc, &vault, &person)).await?
        }
        (None, None) => {
            return Err(CommandError::new(
                codes::INVALID,
                "say which meeting (an event id) or who it is with",
            ));
        }
    };
    let (event, held) = {
        let (vault, svc) = (vault.clone(), svc.clone());
        blocking(move || {
            let event = vault.event(event)?;
            let held = cached_at(&svc, &key_for(event.calendar_id, &event.uid), event.start);
            Ok((event, held))
        })
        .await?
    };
    match held {
        Some(held) => Ok(ForAssistant { brief: held.brief, mail_read_for: held.mail_read_for }),
        None => prepare_event(svc, &vault, event, &Live).await,
    }
}

/// The soonest meeting from now on that somebody matching `person` is at.
fn next_meeting_with(svc: &Service, vault: &Vault, person: &str) -> CommandResult<EventId> {
    let now = svc.now();
    let today = now.to_zoned(TimeZone::system()).date();
    let owners = jatayu_core::brief::own_addresses(vault);
    let wanted = jatayu_core::task::TaskPerson::said(person);
    let mut events = vault.events(&EventQuery {
        from: Some(today),
        to: today.checked_add(Span::new().days(60)).ok(),
        visible_only: true,
        limit: Some(500),
        ..Default::default()
    })?;
    events.retain(|e| {
        e.start > now && !e.all_day && e.status != jatayu_core::calendar::EventStatus::Cancelled
    });
    events.sort_by_key(|e| e.start);
    events
        .into_iter()
        .find(|e| mb::guests_of(e, &owners).iter().any(|g| g.matches(&wanted)))
        .map(|e| e.id)
        .ok_or_else(|| {
            CommandError::new(
                codes::NOT_FOUND,
                format!("no meeting with {person} is on the calendar in the next two months"),
            )
        })
}

// ---- making one ---------------------------------------------------------

/// What a preparation reads off the vault before it begins: the setting, the
/// model that would write it, the accounts it may read mail from, and who
/// the person is.
pub(crate) struct Setup {
    pub settings: MeetingBriefSettings,
    /// The Medium model, or why there is not one.
    pub access: Result<ModelAccess, String>,
    /// Accounts the assistant may read mail from on that model.
    pub readable: Vec<AccountId>,
    pub owners: HashSet<String>,
    pub tz: String,
}

impl Setup {
    fn read(vault: &Vault) -> CoreResult<Self> {
        let settings = vault.meeting_settings()?.brief;
        let access = match vault.agent_credentials(ModelTier::Medium) {
            Ok((_, access)) => Ok(access),
            Err(e) => {
                let off = vault.agent_settings().map(|s| !s.enabled).unwrap_or(false);
                Err(if off {
                    "the assistant is switched off, so no model wrote this".to_string()
                } else {
                    format!("no model is set up for the assistant ({e}), so none wrote this")
                })
            }
        };
        // The gate the daily brief puts mail behind (`crate::brief::extras`):
        // mail switched on for the account, the person's acknowledgement of
        // this provider, and reading allowed. A brief with no model has no
        // provider to have acknowledged, so reads none.
        let provider = access.as_ref().ok().map(|a| a.acknowledgement.clone());
        let readable = if vault.supports_accounts() {
            vault
                .accounts()
                .unwrap_or_default()
                .into_iter()
                .filter(|a| a.services.mail)
                .filter(|a| {
                    provider.as_deref().is_some_and(|p| a.assistant_acknowledged_for(p))
                        && a.assistant_access.permits(Permission::Read)
                })
                .map(|a| a.id)
                .collect()
        } else {
            Vec::new()
        };
        Ok(Self {
            settings,
            access,
            readable,
            owners: jatayu_core::brief::own_addresses(vault),
            tz: vault.person_tz(),
        })
    }
}

/// Gather, write and assemble the brief for `event`, with the guests it
/// matches talking points against.
async fn make(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    event: Event,
    outside: &dyn Outside,
) -> CommandResult<Made> {
    let setup = {
        let vault = vault.clone();
        blocking(move || Ok(Setup::read(&vault)?)).await?
    };
    make_with(svc, vault, event, setup, outside).await
}

pub(crate) async fn make_with(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    event: Event,
    setup: Setup,
    outside: &dyn Outside,
) -> CommandResult<Made> {
    let now = svc.now();
    let index = svc.mail_index();
    let gathered = {
        let vault = vault.clone();
        let (event, settings, readable, owners, tz) = (
            event,
            setup.settings.clone(),
            setup.readable.clone(),
            setup.owners.clone(),
            setup.tz.clone(),
        );
        let model_gap = setup.access.as_ref().err().cloned();
        blocking(move || {
            Ok(gather(
                &vault,
                index.as_deref(),
                &event,
                &Inputs {
                    settings: &settings,
                    readable: &readable,
                    owners: &owners,
                    tz: &tz,
                    model_gap,
                },
            )?)
        })
        .await?
    };
    let Gathered { mut facts, guests, organisations } = gathered;

    for organisation in organisations {
        look_up(outside, &organisation, &mut facts).await;
    }

    let written = match &setup.access {
        Ok(access) => match write(outside, access, &facts).await {
            Ok(written) => Some(written),
            Err(why) => {
                facts.gaps.push(format!(
                    "A model could not write it up ({why}); this is the facts alone."
                ));
                None
            }
        },
        // The reason there is no model is already a gap: `gather` put it there.
        Err(_) => None,
    };
    // What the accounts' acknowledgements were matched against, if any mail
    // came of it: the brief is only as shareable as the provider it was read
    // for.
    let mail_read_for = match (&setup.access, facts.mail.is_empty()) {
        (Ok(access), false) => Some(access.acknowledgement.clone()),
        _ => None,
    };
    Ok(Made { brief: facts.into_brief(written, now), guests, mail_read_for })
}

/// What reading the vault for a brief found.
struct Gathered {
    facts: Facts,
    /// Everybody at the meeting, with their cards: what talking points are
    /// matched against, now and when the brief is refreshed.
    guests: Vec<Guest>,
    /// Organisations to ask the web about.
    organisations: Vec<String>,
}

struct Inputs<'a> {
    settings: &'a MeetingBriefSettings,
    readable: &'a [AccountId],
    owners: &'a HashSet<String>,
    tz: &'a str,
    /// Why no model will write the brief, when none will.
    model_gap: Option<String>,
}

/// Everything about the meeting that is in the vault, found by code. Blocking.
fn gather(
    vault: &Vault,
    index: Option<&dyn MailSearch>,
    event: &Event,
    inputs: &Inputs<'_>,
) -> CoreResult<Gathered> {
    let mut facts = Facts::for_event(event);
    facts.tz = inputs.tz.to_string();
    if let Some(gap) = &inputs.model_gap {
        facts.gaps.push(format!("{}.", capitalise(gap)));
    }

    // Who is coming, and the cards of those who have one.
    let contacts =
        if vault.supports_library() { vault.contacts().unwrap_or_default() } else { Vec::new() };
    let mut everyone: Vec<PersonFacts> = Vec::new();
    for guest in mb::guests_of(event, inputs.owners) {
        let card = guest
            .email
            .as_deref()
            .and_then(|e| match_guest(e, &contacts))
            .or_else(|| {
                (!guest.name.trim().is_empty())
                    .then(|| match_guest(&guest.name, &contacts))
                    .flatten()
            })
            .and_then(|id| contacts.iter().find(|c| c.id == id));
        everyone.push(match card {
            Some(card) => {
                let logs = vault.logs(&LogQuery::for_item(card.id)).unwrap_or_default();
                PersonFacts::from_card(guest, card, &logs)
            }
            None => PersonFacts::stranger(guest),
        });
    }
    let guests: Vec<Guest> = everyone.iter().map(|p| p.guest.clone()).collect();

    // What the person meant to raise with any of them -- and every open task
    // by its title, by which a step of the last meeting is found to be still
    // open.
    let open_tasks =
        if vault.supports_tasks() { vault.tasks(&TaskQuery::open())? } else { Vec::new() };
    if vault.supports_tasks() {
        facts.talking_points = mb::talking_points_for(&open_tasks, &guests);
    }

    // The last time with these people, and notes that mention them.
    let zone = TimeZone::get(inputs.tz).unwrap_or(TimeZone::UTC);
    let last = last_meeting(vault, event, &guests, &zone, &open_tasks);
    facts.notes = notes_about(vault, event, &everyone, last.as_ref().map(|l| l.note.id.as_str()));
    facts.last_meeting = last;

    // Mail with them, from accounts the assistant may read on this model.
    if everyone.iter().any(|p| p.guest.email.is_some()) {
        if inputs.model_gap.is_some() {
            facts.gaps.push("Mail was not read: no model is set up to write the brief.".into());
        } else if inputs.readable.is_empty() {
            facts.gaps.push(
                "Mail is not shared with the assistant on this model, so none was read.".into(),
            );
        } else if let Some(index) = index {
            facts.mail = mail_with(vault, index, inputs.readable, &everyone, inputs.owners, &zone);
        } else {
            facts.gaps.push("Mail search is not available right now, so none was read.".into());
        }
    }

    facts.set_people(everyone);

    // Which organisations the web is to be asked about, if it is.
    let organisations = if inputs.settings.web {
        mb::organisations_to_look_up(
            &guests,
            &mb::own_domains(inputs.owners),
            &facts,
            MAX_WEB_LOOKUPS,
        )
        .into_iter()
        .filter(|domain| !vault_knows(vault, domain, &contacts))
        .collect()
    } else {
        Vec::new()
    };
    Ok(Gathered { facts, guests, organisations })
}

/// The most recent meeting note with the same people, read for where it
/// landed and what each side took on: its topics, what it decided and left
/// open, and its next steps -- the person's own marked when an open task
/// carries their title, which is what accepting the task proposal the note
/// made gives them. See [`LastMeeting::read`].
fn last_meeting(
    vault: &Vault,
    event: &Event,
    guests: &[Guest],
    zone: &TimeZone,
    open_tasks: &[jatayu_core::task::Task],
) -> Option<LastMeeting> {
    if guests.is_empty() || !vault.supports_meetings() {
        return None;
    }
    let cutoff =
        Timestamp::from_second(event.start.as_second() - LAST_MEETING_DAYS * 86_400).ok()?;
    let recordings = vault
        .recordings(&RecordingQuery {
            stages: vec![Stage::Done.as_str().to_string()],
            from: Some(cutoff),
            // A hard cap on rows read, whatever was recorded in the window.
            limit: Some(200),
            ..Default::default()
        })
        .ok()?;
    let recording =
        mb::last_meeting_with(&recordings, guests, &event.uid, event.start, LAST_MEETING_DAYS)?;
    let note = vault.note(recording.note_id?).ok()?;
    let day = recording.started_at.to_zoned(zone.clone()).date();
    let fact = NoteFact {
        id: note.id.to_string(),
        title: one_line(&note.display_title(), 120),
        excerpt: String::new(),
        date: Some(day),
    };
    let owner = crate::meeting::pipeline::owner_of(vault);
    let others: Vec<String> = guests.iter().map(Guest::display).collect();
    let titles: Vec<String> = open_tasks.iter().map(|t| t.title.clone()).collect();
    Some(LastMeeting::read(fact, &note.body.to_markdown(), day, &owner, &others, &titles))
}

/// A few notes that mention the meeting by name or somebody coming by their
/// whole name -- found by the index, then kept only if the words really are
/// in what it showed, so a search for "Josh Smith" does not bring every note
/// with a Smith in it.
fn notes_about(
    vault: &Vault,
    event: &Event,
    people: &[PersonFacts],
    except: Option<&str>,
) -> Vec<NoteFact> {
    if !vault.supports_notes() {
        return Vec::new();
    }
    let mut queries: Vec<String> = Vec::new();
    let title = event.title.trim();
    if title.chars().count() >= MIN_NAME_CHARS
        && !GENERIC_TITLES.contains(&title.to_lowercase().as_str())
    {
        queries.push(title.to_string());
    }
    for p in people.iter().take(NOTE_PEOPLE) {
        let name = p.guest.name.trim();
        if name.contains(' ') && !name.contains('@') && name.chars().count() >= MIN_NAME_CHARS {
            queries.push(name.to_string());
        }
    }

    let mut out: Vec<NoteFact> = Vec::new();
    for query in queries {
        let Ok(hits) = vault.search(&query, SearchScope::Notes, 3) else { continue };
        for hit in hits {
            let Found::Note { id } = hit.found else { continue };
            let id = id.to_string();
            if Some(id.as_str()) == except || out.iter().any(|n| n.id == id) {
                continue;
            }
            let seen = format!("{} {}", hit.title, hit.snippet).to_lowercase();
            if !seen.contains(&query.to_lowercase()) {
                continue;
            }
            out.push(NoteFact {
                id,
                title: one_line(&hit.title, 120),
                excerpt: one_line(&hit.snippet, 300),
                date: None,
            });
        }
        if out.len() >= MAX_NOTES {
            break;
        }
    }
    out.truncate(MAX_NOTES);
    out
}

/// Recent mail with a few of the people coming, newest first, one thread
/// each: from the accounts in `accounts` and nowhere else, never from Trash
/// or Spam.
///
/// Each thread is read as it stands now, not as the search hit that found it:
/// the newest message that is not a draft, who sent it (`owners` are the
/// person's own addresses, who are "you") and what the follow-up pass says of
/// it -- somebody waiting on the person's reply, or the person on somebody's.
fn mail_with(
    vault: &Vault,
    index: &dyn MailSearch,
    accounts: &[AccountId],
    people: &[PersonFacts],
    owners: &HashSet<String>,
    zone: &TimeZone,
) -> Vec<MailFact> {
    // Where a thread is filed says whether it is on its way out.
    let empties: HashSet<jatayu_core::id::MailboxId> = accounts
        .iter()
        .flat_map(|a| vault.mailboxes(*a).unwrap_or_default())
        .filter(|m| m.role.empties())
        .map(|m| m.id)
        .collect();
    let scope: Vec<String> = accounts.iter().map(ToString::to_string).collect();

    let mut found: Vec<(Timestamp, MailFact, ThreadId)> = Vec::new();
    for person in people.iter().filter(|p| p.guest.email.is_some()).take(MAIL_PEOPLE) {
        let Some(email) = person.guest.email.as_deref() else { continue };
        // An address is no place for a quote: it would end the one around it.
        if email.contains(['"', ' ']) {
            continue;
        }
        let window = format!("newer_than:{MAIL_DAYS}d");
        let text = format!("from:\"{email}\" {window} OR to:\"{email}\" {window}");
        let query = MailQuery::parse(&text).with_accounts(scope.clone());
        let Ok(page) = index.search(&query, 10, None) else { continue };
        for hit in page.hits {
            let Ok(thread) = hit.thread_key.parse::<ThreadId>() else { continue };
            if found.iter().any(|(_, _, t)| *t == thread) {
                continue;
            }
            let Ok(message_id) = hit.message_key.parse::<jatayu_core::id::MailMessageId>() else {
                continue;
            };
            let Ok(message) = vault.mail_message(message_id) else { continue };
            let mut fact = MailFact {
                thread_id: hit.thread_key.clone(),
                subject: one_line(&message.subject, 120),
                snippet: one_line(&message.snippet, 160),
                with: person.guest.display(),
                date: hit.date.to_zoned(zone.clone()).date(),
                from: String::new(),
                waiting: None,
            };
            let mut at = hit.date;
            if let Ok((thread_record, messages)) = vault.thread(thread) {
                if !thread_record.subject.trim().is_empty() {
                    fact.subject = one_line(&thread_record.subject, 120);
                }
                let newest = messages
                    .iter()
                    .filter(|m| !m.flags.draft && !m.flags.deleted)
                    .max_by_key(|m| m.date);
                if let Some(newest) = newest {
                    at = newest.date;
                    fact.date = newest.date.to_zoned(zone.clone()).date();
                    fact.snippet = one_line(&newest.snippet, 160);
                    fact.from = if owners.contains(&newest.from.email.trim().to_ascii_lowercase()) {
                        "you".to_string()
                    } else if newest.from.name.trim().is_empty() {
                        one_line(&newest.from.email, 80)
                    } else {
                        one_line(&newest.from.name, 80)
                    };
                }
                fact.waiting = thread_record.follow_up.as_ref().map(|f| Waiting {
                    kind: f.kind,
                    since: f.since.to_zoned(zone.clone()).date(),
                });
            }
            found.push((at, fact, thread));
        }
    }
    if !empties.is_empty() && !found.is_empty() {
        let threads: Vec<ThreadId> = found.iter().map(|(_, _, t)| *t).collect();
        let filings = vault.mail_thread_filings(&threads).unwrap_or_default();
        // On its way out only if every place it is filed is.
        let leaving: HashSet<ThreadId> = threads
            .iter()
            .filter(|t| {
                let places: Vec<_> = filings.iter().filter(|(th, ..)| th == *t).collect();
                !places.is_empty()
                    && places.iter().all(|(_, _, mailbox, _)| empties.contains(mailbox))
            })
            .copied()
            .collect();
        found.retain(|(_, _, t)| !leaving.contains(t));
    }
    found.sort_by_key(|(at, ..)| std::cmp::Reverse(*at));
    found.into_iter().map(|(_, fact, _)| fact).take(MAX_MAIL).collect()
}

/// Whether the vault already says something of an organisation -- a contact
/// who works there, or a note that names it -- beyond what this brief
/// gathered. The web is for what nobody has written down.
fn vault_knows(vault: &Vault, domain: &str, contacts: &[jatayu_core::Item]) -> bool {
    let label = mb::organisation_label(domain).to_lowercase();
    if label.is_empty() {
        return false;
    }
    let works_there = contacts.iter().any(|c| {
        c.facts.get("company").is_some_and(|company| {
            company.to_lowercase().split(|ch: char| !ch.is_alphanumeric()).any(|w| w == label)
        })
    });
    if works_there {
        return true;
    }
    vault.supports_notes()
        && vault.search(&label, SearchScope::Notes, 3).is_ok_and(|hits| {
            hits.iter().any(|h| {
                format!("{} {}", h.title, h.snippet)
                    .to_lowercase()
                    .split(|ch: char| !ch.is_alphanumeric())
                    .any(|w| w == label)
            })
        })
}

/// One web search for `domain`, kept as a fact on `facts` or, failing that,
/// as a line on what could not be looked at. What is sent is the domain.
async fn look_up(outside: &dyn Outside, domain: &str, facts: &mut Facts) {
    let answer = tokio::time::timeout(WEB_TIMEOUT, outside.search(domain)).await;
    let hits = match answer {
        Ok(Ok(hits)) => hits,
        Ok(Err(_)) | Err(_) => {
            facts.gaps.push(format!("A web search for {domain} did not answer."));
            return;
        }
    };
    // The hit on the organisation's own site if there is one, since a
    // search for a bare domain can land anywhere.
    let own = |hit: &SearchResult| {
        hit.url.split("://").nth(1).and_then(|rest| rest.split(['/', '?', '#']).next()).is_some_and(
            |host| mb::registrable_domain(host.split(':').next().unwrap_or(host)) == domain,
        )
    };
    let best = hits.iter().find(|h| own(h)).or_else(|| hits.first());
    match best {
        Some(hit) => facts.web.push(WebFact {
            organisation: domain.to_string(),
            title: one_line(&hit.title, 120),
            url: hit.url.clone(),
            summary: one_line(&hit.summary, 240),
        }),
        None => facts.gaps.push(format!("A web search for {domain} found nothing.")),
    }
}

/// Ask the model to write `facts` up, and keep what it says that is about
/// who was there.
async fn write(
    outside: &dyn Outside,
    access: &ModelAccess,
    facts: &Facts,
) -> Result<Written, String> {
    let user = jatayu_core::prompt::fence(
        "the meeting: its invitation, who is coming and what is known of them, mail, notes and \
         web pages",
        &facts.prompt(),
    );
    let answer = outside.write(access, mb::BRIEF_SYSTEM, &user, answer_schema()).await?;
    let written = parse_answer(&answer, facts);
    if written.is_empty() {
        return Err("it had nothing to add".into());
    }
    Ok(written)
}

/// `text` on one line, hidden characters out, cut at `max` characters.
fn one_line(text: &str, max: usize) -> String {
    let flat =
        jatayu_core::hidden::strip_hidden(text).split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut out: String = flat.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventSink;
    use jatayu_core::calendar::{
        AccountCalendarSource, AccountSyncCursor, CalendarOrigin, EventStatus,
    };
    use jatayu_core::id::CalendarId;
    use jatayu_core::task::{Task, TaskPerson};
    use serde_json::json;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn env() -> (Arc<Service>, Arc<Vault>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let vault = jatayu_vault::create(
            dir.path(),
            jatayu_core::VaultConfig {
                password: Some("correct horse battery staple".into()),
                kdf: jatayu_core::crypto::KdfParams::insecure_fast(),
                ..Default::default()
            },
        )
        .unwrap();
        let svc = Arc::new(Service::new());
        let vault = svc.set(vault);
        (svc, vault, dir)
    }

    #[derive(Clone, Default)]
    struct Sink {
        briefs: Arc<StdMutex<Vec<MeetingBrief>>>,
    }

    impl EventSink for Sink {
        fn meeting_brief(&self, brief: MeetingBrief) {
            self.briefs.lock().unwrap().push(brief);
        }
    }

    /// The internet and a model, as a test would have them: every search
    /// asked for and every prompt sent is kept, and the answers are given.
    #[derive(Default)]
    struct Fake {
        searches: StdMutex<Vec<String>>,
        prompts: StdMutex<Vec<String>>,
        answer: StdMutex<Option<Result<Value, String>>>,
        hits: StdMutex<Vec<SearchResult>>,
        writes: AtomicUsize,
    }

    impl Outside for Fake {
        fn search<'a>(
            &'a self,
            organisation: &'a str,
        ) -> Fut<'a, Result<Vec<SearchResult>, String>> {
            self.searches.lock().unwrap().push(organisation.to_string());
            let hits = self.hits.lock().unwrap().clone();
            Box::pin(async move { Ok(hits) })
        }

        fn write<'a>(
            &'a self,
            _access: &'a ModelAccess,
            _system: &'a str,
            user: &'a str,
            _schema: Value,
        ) -> Fut<'a, Result<Value, String>> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.prompts.lock().unwrap().push(user.to_string());
            let answer = self.answer.lock().unwrap().clone().unwrap_or(Err("no answer".into()));
            Box::pin(async move { answer })
        }
    }

    fn hit(title: &str, url: &str, summary: &str) -> SearchResult {
        SearchResult {
            title: title.into(),
            url: url.into(),
            summary: summary.into(),
            ..Default::default()
        }
    }

    fn account_calendar(vault: &Vault) -> Calendar {
        let mut calendar = Calendar::subscribed("Work", "https://example.com/work.ics");
        calendar.origin = CalendarOrigin::Account {
            account_id: AccountId::new(),
            remote_id: "primary".into(),
            remote_name: "Work".into(),
            source: AccountCalendarSource::Google,
        };
        vault.save_calendar(&calendar).unwrap();
        calendar
    }

    /// A meeting `offset_secs` from now, with `attendees` on it.
    fn meeting(calendar_id: CalendarId, offset_secs: i64, attendees: &[&str]) -> Event {
        let now = Timestamp::now();
        let start = Timestamp::from_second(now.as_second() + offset_secs).unwrap();
        let end = Timestamp::from_second(start.as_second() + 1_800).unwrap();
        let day = |at: Timestamp| at.to_zoned(TimeZone::system()).date();
        Event {
            id: EventId::new(),
            calendar_id,
            uid: format!("evt-{}", EventId::new()),
            title: "Design sync".into(),
            description: "Agenda:\n- Roadmap\n- Hiring".into(),
            location: String::new(),
            start,
            end,
            local_date: day(start),
            end_date: day(end),
            tz: "UTC".into(),
            all_day: false,
            status: EventStatus::Confirmed,
            organizer: String::new(),
            attendees: attendees.iter().map(|a| a.to_string()).collect(),
            guests: Vec::new(),
            url: String::new(),
            busy: true,
            series: None,
            own: None,
            alerts: None,
            updated_at: now,
        }
    }

    fn seed(vault: &Vault, event: &Event) {
        vault
            .sync_account_calendar(
                event.calendar_id,
                std::slice::from_ref(event),
                &[],
                AccountSyncCursor::default(),
            )
            .unwrap();
    }

    fn settings(web: bool) -> MeetingBriefSettings {
        MeetingBriefSettings { enabled: true, lead_minutes: 15, web }
    }

    /// A setup with no model: what a vault that has not set the assistant up
    /// gets.
    fn no_model(web: bool) -> Setup {
        Setup {
            settings: settings(web),
            access: Err("no model is set up for the assistant (test), so none wrote this".into()),
            readable: Vec::new(),
            owners: HashSet::new(),
            tz: "UTC".into(),
        }
    }

    fn a_model(web: bool) -> Setup {
        Setup {
            access: Ok(ModelAccess {
                tier: ModelTier::Medium,
                config: jatayu_core::llm::LLMConfig::medium(),
                key: None,
                acknowledgement: String::new(),
            }),
            ..no_model(web)
        }
    }

    // ---- the web ---------------------------------------------------------

    #[tokio::test]
    async fn with_the_web_switched_off_nothing_is_searched() {
        let (svc, vault, _dir) = env();
        let calendar = account_calendar(&vault);
        let event = meeting(calendar.id, 600, &["Lee <lee@unknown-org.example>"]);
        seed(&vault, &event);
        let fake = Fake::default();
        *fake.hits.lock().unwrap() =
            vec![hit("Unknown Org", "https://unknown-org.example", "Makes things")];

        let brief = make_with(&svc, &vault, event, no_model(false), &fake).await.unwrap().brief;

        assert!(fake.searches.lock().unwrap().is_empty(), "no search was sent");
        assert!(brief.sources.iter().all(|s| s.kind != mb::MeetingBriefSourceKind::Web));
    }

    #[tokio::test]
    async fn the_web_is_asked_about_an_organisation_by_its_domain_and_nothing_else() {
        let (svc, vault, _dir) = env();
        let calendar = account_calendar(&vault);
        let event = meeting(
            calendar.id,
            600,
            &["Lee Wong <lee.wong@unknown-org.example>", "Pat <pat@gmail.com>"],
        );
        seed(&vault, &event);
        let fake = Fake::default();
        *fake.hits.lock().unwrap() = vec![
            hit("Somebody's blog", "https://blog.example/post", "Mentions unknown-org"),
            hit(
                "Unknown Org: tools",
                "https://www.unknown-org.example/about",
                "Makes tools for sheds.",
            ),
        ];

        let brief = make_with(&svc, &vault, event, no_model(true), &fake).await.unwrap().brief;

        let sent = fake.searches.lock().unwrap().clone();
        assert_eq!(sent, ["unknown-org.example"], "one search, for the domain, none for gmail");
        for word in ["Lee", "Wong", "lee.wong", "Design", "Pat"] {
            assert!(sent.iter().all(|q| !q.contains(word)), "{word} must not be sent: {sent:?}");
        }
        let web = brief
            .sources
            .iter()
            .find(|s| s.kind == mb::MeetingBriefSourceKind::Web)
            .expect("the web is a source");
        assert_eq!(web.id, "https://www.unknown-org.example/about", "the organisation's own page");
        assert!(
            brief.context.iter().any(|l| l.contains("Makes tools for sheds")),
            "{:?}",
            brief.context
        );
    }

    #[tokio::test]
    async fn an_organisation_the_vault_already_knows_is_not_searched_for() {
        let (svc, vault, _dir) = env();
        let calendar = account_calendar(&vault);
        vault
            .save_note(
                &jatayu_core::Note::written("Notes on Acme", "Acme builds the widgets we use."),
                None,
            )
            .unwrap();
        let event = meeting(calendar.id, 600, &["Lee <lee@acme.com>", "Sam <sam@newco.io>"]);
        seed(&vault, &event);
        let fake = Fake::default();

        make_with(&svc, &vault, event, no_model(true), &fake).await.unwrap();

        assert_eq!(*fake.searches.lock().unwrap(), ["newco.io"], "acme is in a note already");
    }

    // ---- the model -------------------------------------------------------

    #[tokio::test]
    async fn a_model_is_given_the_facts_fenced_and_what_it_says_about_nobody_is_dropped() {
        let (svc, vault, _dir) = env();
        let calendar = account_calendar(&vault);
        let event = meeting(calendar.id, 600, &["Josh Smith <josh@acme.com>"]);
        seed(&vault, &event);
        let fake = Fake::default();
        *fake.answer.lock().unwrap() = Some(Ok(json!({
            "summary": "A weekly design sync.",
            "open": ["You owe Josh the roadmap draft."],
            "agenda": [],
            "people": [
                { "email": "josh@acme.com", "line": "Runs design at Acme." },
                { "name": "Nobody Invited", "line": "Made up." },
            ],
            "context": ["Nothing else to know."],
        })));

        let brief = make_with(&svc, &vault, event, a_model(false), &fake).await.unwrap().brief;

        assert_eq!(fake.writes.load(Ordering::SeqCst), 1, "one ask");
        let prompt = fake.prompts.lock().unwrap()[0].clone();
        assert!(prompt.contains("BEGIN TEXT WRITTEN BY SOMEBODY ELSE"), "{prompt}");
        assert!(prompt.contains("josh@acme.com") && prompt.contains("Design sync"), "{prompt}");
        assert!(brief.written);
        assert_eq!(brief.summary.as_deref(), Some("A weekly design sync."));
        assert_eq!(brief.open, ["You owe Josh the roadmap draft."]);
        assert_eq!(brief.agenda, ["Roadmap", "Hiring"], "the invitation's, the model gave none");
        assert_eq!(brief.people.len(), 1);
        assert_eq!(brief.people[0].line, "Runs design at Acme.");
        assert_eq!(brief.context, ["Nothing else to know."]);
        assert!(brief.gaps.iter().all(|g| !g.contains("could not write")), "{:?}", brief.gaps);
    }

    #[tokio::test]
    async fn a_model_that_fails_leaves_the_facts_and_says_so() {
        let (svc, vault, _dir) = env();
        let calendar = account_calendar(&vault);
        let event = meeting(calendar.id, 600, &["Josh Smith <josh@acme.com>"]);
        seed(&vault, &event);
        let fake = Fake::default();
        *fake.answer.lock().unwrap() = Some(Err("the model took longer than 180s".into()));

        let brief = make_with(&svc, &vault, event, a_model(false), &fake).await.unwrap().brief;

        assert!(!brief.written);
        assert_eq!(brief.agenda, ["Roadmap", "Hiring"]);
        assert_eq!(brief.people.len(), 1);
        assert!(
            brief.gaps.iter().any(|g| g.contains("could not write it up") && g.contains("180s")),
            "{:?}",
            brief.gaps
        );
    }

    #[tokio::test]
    async fn an_empty_answer_is_a_brief_from_the_facts() {
        let (svc, vault, _dir) = env();
        let calendar = account_calendar(&vault);
        let event = meeting(calendar.id, 600, &["Josh Smith <josh@acme.com>"]);
        seed(&vault, &event);
        let fake = Fake::default();
        *fake.answer.lock().unwrap() =
            Some(Ok(json!({ "summary": "", "agenda": [], "people": [], "context": [] })));

        let brief = make_with(&svc, &vault, event, a_model(false), &fake).await.unwrap().brief;
        assert!(!brief.written, "a model with nothing to say has not written it");
    }

    /// A finished recording of an earlier meeting with Josh, whose note is `body`.
    fn earlier_meeting(vault: &Vault, calendar_id: CalendarId, body: &str) {
        let started = Timestamp::from_second(Timestamp::now().as_second() - 6 * 86_400).unwrap();
        let note = jatayu_core::Note::written_by(
            jatayu_core::NoteOrigin::Meeting,
            "Design sync: pricing slips",
            body,
        );
        vault.save_note(&note, None).unwrap();
        let reference = jatayu_core::meeting::EventRef {
            calendar_id,
            uid: "earlier".into(),
            title: "Design sync".into(),
            start: started,
            end: Timestamp::from_second(started.as_second() + 1_800).unwrap(),
            tz: "UTC".into(),
            organizer: String::new(),
            attendees: vec!["Josh Smith <josh@acme.com>".into()],
            guests: Vec::new(),
            join_url: String::new(),
            calendar_name: "Work".into(),
            series: None,
        };
        let mut recording = jatayu_core::meeting::Recording::new(
            "Design sync",
            Some(reference),
            jatayu_core::id::TemplateId::new(),
        );
        recording.started_at = started;
        recording.stage = Stage::Done;
        recording.note_id = Some(note.id);
        vault.save_recording(&recording).unwrap();
    }

    #[tokio::test]
    async fn the_last_meetings_outcomes_and_the_steps_still_open_are_what_the_model_reads() {
        let (svc, vault, _dir) = env();
        vault
            .save_profile(&jatayu_core::Profile {
                first_name: "Hari".into(),
                last_name: "Govardhanam".into(),
                ..Default::default()
            })
            .unwrap();
        let calendar = account_calendar(&vault);
        earlier_meeting(
            &vault,
            calendar.id,
            "- **When:** Tuesday 18 November 2026, 15:00\u{2013}15:45 (UTC)\n\
             - **Join link:** https://meet.example/abc\n\n\
             ## Pricing for the Acme renewal\n\
             - Decided: no paid placements for now\n\
             - Open: who owns the migration\n\n\
             ## Next steps\n\
             - **Hari Govardhanam:** Send Acme the revised pricing deck, by Fri 21 Nov\n\
             - **Hari Govardhanam:** Book the room\n\
             - **Josh Smith:** Get legal sign-off\n",
        );
        // Accepting the note's task proposal made this one; the room was booked.
        vault.save_task(&Task::new("Send Acme the revised pricing deck")).unwrap();
        let event = meeting(calendar.id, 600, &["Josh Smith <josh@acme.com>"]);
        seed(&vault, &event);
        let fake = Fake::default();
        *fake.answer.lock().unwrap() = Some(Ok(json!({ "summary": "Renewal." })));

        let brief =
            make_with(&svc, &vault, event.clone(), a_model(false), &fake).await.unwrap().brief;

        let prompt = fake.prompts.lock().unwrap()[0].clone();
        for want in [
            "Topics: Pricing for the Acme renewal",
            "Decided: no paid placements for now",
            "Open: who owns the migration",
            "- **You:** Send Acme the revised pricing deck, by 21 Nov (still open in your tasks)",
            "- **You:** Book the room\n",
            "- **Josh Smith:** Get legal sign-off\n",
        ] {
            assert!(prompt.contains(want), "missing {want:?} in:\n{prompt}");
        }
        assert!(!prompt.contains("Join link"), "the details block is not the note: {prompt}");
        // The model said nothing of what is open, so the facts do.
        assert_eq!(
            brief.open,
            [
                "You still owe: Send Acme the revised pricing deck (said by 21 Nov; from the \
                 18 Nov meeting)",
                "Josh Smith took on: Get legal sign-off (from the 18 Nov meeting)",
                "Still open from last time: who owns the migration",
            ]
            .iter()
            .map(|l| l.replace("18 Nov", &expected_day()))
            .collect::<Vec<_>>()
        );
        assert_eq!(brief.tz, "UTC");
    }

    /// The day `earlier_meeting` was held, as a brief writes it: six days ago.
    fn expected_day() -> String {
        let at = Timestamp::from_second(Timestamp::now().as_second() - 6 * 86_400).unwrap();
        at.to_zoned(TimeZone::UTC).strftime("%-d %b").to_string()
    }

    #[tokio::test]
    async fn with_no_model_none_is_asked_and_the_gap_says_why() {
        let (svc, vault, _dir) = env();
        let calendar = account_calendar(&vault);
        let event = meeting(calendar.id, 600, &["Josh Smith <josh@acme.com>"]);
        seed(&vault, &event);
        let fake = Fake::default();

        let brief = make_with(&svc, &vault, event, no_model(false), &fake).await.unwrap().brief;

        assert_eq!(fake.writes.load(Ordering::SeqCst), 0);
        assert!(!brief.written);
        assert!(brief.gaps[0].starts_with("No model is set up"), "{:?}", brief.gaps);
        assert!(brief.gaps.iter().any(|g| g.starts_with("Mail was not read")), "{:?}", brief.gaps);
    }

    // ---- the tick --------------------------------------------------------

    async fn settled(sink: &Sink, count: usize) -> Vec<MeetingBrief> {
        for _ in 0..200 {
            if sink.briefs.lock().unwrap().len() >= count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        // And a moment more, in case something else is on its way.
        tokio::time::sleep(Duration::from_millis(100)).await;
        sink.briefs.lock().unwrap().clone()
    }

    fn enable(vault: &Vault, brief: MeetingBriefSettings) {
        let mut settings = vault.meeting_settings().unwrap();
        settings.brief = brief;
        vault.save_meeting_settings(&settings).unwrap();
    }

    #[tokio::test]
    async fn the_tick_prepares_each_meeting_once_and_raises_it() {
        let (svc, vault, _dir) = env();
        let sink = Sink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, settings(false));
        let calendar = account_calendar(&vault);
        let soon = meeting(calendar.id, 5 * 60, &["Josh Smith <josh@acme.com>"]);
        let started = meeting(calendar.id, -60, &["Priya <priya@acme.com>"]);
        seed(&vault, &soon);
        seed(&vault, &started);
        let fake = Arc::new(Fake::default());

        tick_with(&svc, &vault, fake.clone()).await;
        tick_with(&svc, &vault, fake.clone()).await;
        let raised = settled(&sink, 2).await;
        tick_with(&svc, &vault, fake.clone()).await;
        let again = settled(&sink, 2).await;

        assert_eq!(raised.len(), 2, "one each");
        assert_eq!(again.len(), 2, "and no more on the next minute");
        let keys: HashSet<&str> = raised.iter().map(|b| b.key.as_str()).collect();
        assert!(keys.contains(key_for(calendar.id, &soon.uid).as_str()));
        assert!(keys.contains(key_for(calendar.id, &started.uid).as_str()));
        assert!(cached(&svc, &key_for(calendar.id, &soon.uid)).is_some(), "and kept for the card");
    }

    #[tokio::test]
    async fn the_tick_leaves_alone_what_is_not_due_wanted_or_a_meeting() {
        let (svc, vault, _dir) = env();
        let sink = Sink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, settings(false));
        let calendar = account_calendar(&vault);
        let mut cancelled = meeting(calendar.id, 300, &["A <a@x.example>"]);
        cancelled.status = EventStatus::Cancelled;
        let mut free = meeting(calendar.id, 300, &["B <b@x.example>"]);
        free.busy = false;
        let mut all_day = meeting(calendar.id, 300, &["C <c@x.example>"]);
        all_day.all_day = true;
        let alone = meeting(calendar.id, 300, &[]);
        let later = meeting(calendar.id, 3 * 3600, &["D <d@x.example>"]);
        let over = meeting(calendar.id, -600, &["E <e@x.example>"]);
        let mut skipped = meeting(calendar.id, 300, &["F <f@x.example>"]);
        skipped.series = Some("never-series".into());
        for e in [&cancelled, &free, &all_day, &alone, &later, &over, &skipped] {
            seed(&vault, e);
        }
        let mut meeting_settings = vault.meeting_settings().unwrap();
        meeting_settings.skipped_series.insert("never-series".into());
        vault.save_meeting_settings(&meeting_settings).unwrap();

        tick_with(&svc, &vault, Arc::new(Fake::default())).await;

        assert!(settled(&sink, 1).await.is_empty(), "nothing was prepared");
    }

    #[tokio::test]
    async fn the_tick_does_nothing_with_briefs_switched_off() {
        let (svc, vault, _dir) = env();
        let sink = Sink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, MeetingBriefSettings { enabled: false, ..settings(false) });
        let calendar = account_calendar(&vault);
        seed(&vault, &meeting(calendar.id, 300, &["A <a@x.example>"]));

        tick_with(&svc, &vault, Arc::new(Fake::default())).await;

        assert!(settled(&sink, 1).await.is_empty());
    }

    #[tokio::test]
    async fn the_lead_comes_from_the_setting() {
        let (svc, vault, _dir) = env();
        let sink = Sink::default();
        svc.set_events(Arc::new(sink.clone()));
        enable(&vault, MeetingBriefSettings { lead_minutes: 60, ..settings(false) });
        let calendar = account_calendar(&vault);
        seed(&vault, &meeting(calendar.id, 45 * 60, &["A <a@x.example>"]));

        tick_with(&svc, &vault, Arc::new(Fake::default())).await;

        assert_eq!(settled(&sink, 1).await.len(), 1);
    }

    // ---- what is remembered ----------------------------------------------

    /// A brief for the occurrence of `key` that lasts from `start` to `end`.
    fn bare_brief(key: &str, start: Timestamp, end: Timestamp) -> MeetingBrief {
        MeetingBrief {
            key: key.into(),
            event_id: EventId::new(),
            calendar_id: CalendarId::new(),
            uid: "u".into(),
            title: "T".into(),
            start,
            end,
            location: None,
            summary: None,
            open: Vec::new(),
            agenda: Vec::new(),
            people: Vec::new(),
            context: Vec::new(),
            talking_points: Vec::new(),
            sources: Vec::new(),
            written: false,
            gaps: Vec::new(),
            prepared_at: start,
            tz: String::new(),
        }
    }

    fn made(brief: MeetingBrief) -> Made {
        Made { brief, guests: Vec::new(), mail_read_for: None }
    }

    fn after(at: Timestamp, secs: i64) -> Timestamp {
        Timestamp::from_second(at.as_second() + secs).unwrap()
    }

    const HALF_AN_HOUR: i64 = 30 * 60;
    const DAY: i64 = 24 * 60 * 60;

    #[test]
    fn a_meeting_is_claimed_once_and_given_up_on_after_three_failures() {
        let briefs = Briefs::default();
        let (now, start) = (Timestamp::now(), after(Timestamp::now(), 600));
        let first = briefs.begin("k", start).expect("claimed");
        assert!(briefs.begin("k", start).is_none(), "already being prepared");
        briefs.fail("k", start, first);
        for _ in 1..MAX_ATTEMPTS {
            let generation = briefs.begin("k", start).expect("tried again");
            briefs.fail("k", start, generation);
        }
        assert!(briefs.begin("k", start).is_none(), "given up on");
        assert!(
            briefs.begin("other", start).is_some(),
            "one meeting failing does not stop another"
        );
        assert!(
            briefs.begin("k", after(start, DAY)).is_some(),
            "and the next occurrence of the one that failed is a new start"
        );

        let generation = briefs.begin("done", start).unwrap();
        let brief = bare_brief("done", start, after(start, HALF_AN_HOUR));
        assert!(briefs.finish(generation, made(brief), now));
        assert!(briefs.begin("done", start).is_none(), "it has a brief");
    }

    #[test]
    fn each_occurrence_of_a_repeating_meeting_has_its_own_brief() {
        let briefs = Briefs::default();
        let now = Timestamp::now();
        let (monday, tuesday) = (after(now, 600), after(now, DAY + 600));
        for start in [monday, tuesday] {
            let generation = briefs.begin("k", start).expect("each occurrence is claimed");
            let brief = bare_brief("k", start, after(start, HALF_AN_HOUR));
            assert!(briefs.finish(generation, made(brief), now));
        }
        assert!(briefs.begin("k", monday).is_none() && briefs.begin("k", tuesday).is_none());

        // The one on now or next, and only ever one that is not over.
        assert_eq!(briefs.held_now("k", now).unwrap().brief.start, monday);
        let monday_over = after(monday, HALF_AN_HOUR + 60);
        assert_eq!(briefs.held_now("k", monday_over).unwrap().brief.start, tuesday);
        assert_eq!(
            briefs.held_at("k", monday).unwrap().brief.start,
            monday,
            "and by its own start"
        );
        assert!(briefs.held_now("k", after(tuesday, DAY)).is_none(), "none left that is not over");

        // Over is let go.
        briefs.prune(monday_over);
        assert!(briefs.held_at("k", monday).is_none());
        assert!(briefs.held_at("k", tuesday).is_some());
        briefs.prune(after(tuesday, DAY));
        assert!(briefs.held_at("k", tuesday).is_none());
    }

    #[test]
    fn a_brief_for_a_meeting_that_is_over_is_not_kept() {
        let briefs = Briefs::default();
        let now = Timestamp::now();
        let generation = briefs.generation();
        let over = bare_brief("k", after(now, -2 * HALF_AN_HOUR), after(now, -HALF_AN_HOUR));
        briefs.keep(generation, made(over.clone()), now);
        assert!(briefs.held_at("k", over.start).is_none());
        let current = bare_brief("k", after(now, -60), after(now, HALF_AN_HOUR));
        briefs.keep(generation, made(current.clone()), now);
        assert!(briefs.held_at("k", current.start).is_some());
    }

    #[test]
    fn a_brief_finished_after_the_vault_locked_is_not_kept() {
        let briefs = Briefs::default();
        let now = Timestamp::now();
        let start = after(now, 600);
        let brief = bare_brief("k", start, after(start, HALF_AN_HOUR));
        let generation = briefs.begin("k", start).unwrap();
        briefs.clear();
        assert!(!briefs.finish(generation, made(brief.clone()), now));
        assert!(briefs.held_at("k", start).is_none(), "nothing came back after the lock");
        assert!(briefs.begin("k", start).is_some(), "and the claim went with it");
        briefs.keep(generation, made(brief), now);
        assert!(
            briefs.held_at("k", start).is_none(),
            "a brief asked for before the lock is not kept either"
        );
    }

    #[test]
    fn the_points_on_a_held_brief_follow_the_tasks() {
        let (svc, vault, _dir) = env();
        let josh = vec![Guest::new("Josh Smith", Some("josh@acme.com".into()))];
        let mut ticked = Task::new("Ticked later");
        ticked.raise_with = vec![TaskPerson::said("Josh")];
        let mut kept = Task::new("Kept");
        kept.raise_with = vec![TaskPerson::said("josh@acme.com")];
        vault.save_task(&ticked).unwrap();
        vault.save_task(&kept).unwrap();

        let start = after(svc.now(), 600);
        let mut brief = bare_brief("event:k:u", start, after(start, HALF_AN_HOUR));
        brief.talking_points = mb::talking_points_for(&[ticked.clone(), kept.clone()], &josh);
        assert_eq!(brief.talking_points.len(), 2);
        let generation = svc.meeting_briefs().begin(&brief.key, start).unwrap();
        let held = Made { brief, guests: josh, mail_read_for: None };
        svc.meeting_briefs().finish(generation, held, svc.now());

        // One ticked off, one added, one not for anybody there.
        jatayu_core::completable::Completable::set_status(
            &mut ticked,
            jatayu_core::task::TaskStatus::Done,
        );
        vault.save_task(&ticked).unwrap();
        let mut added = Task::new("Added since");
        added.raise_with = vec![TaskPerson::said("Josh Smith")];
        vault.save_task(&added).unwrap();
        let mut elsewhere = Task::new("Sam only");
        elsewhere.raise_with = vec![TaskPerson::said("Sam")];
        vault.save_task(&elsewhere).unwrap();

        let fresh = cached(&svc, "event:k:u").unwrap();
        let state: Vec<(&str, bool)> =
            fresh.talking_points.iter().map(|p| (p.text.as_str(), p.done)).collect();
        assert!(state.contains(&("Ticked later", true)), "{state:?}");
        assert!(state.contains(&("Kept", false)), "{state:?}");
        assert!(state.contains(&("Added since", false)), "{state:?}");
        assert!(!state.iter().any(|(t, _)| t.starts_with("Sam")), "{state:?}");
        // And what is held now is what was asked for.
        assert_eq!(
            svc.meeting_briefs().held_at("event:k:u", start).unwrap().brief.talking_points,
            fresh.talking_points
        );
    }

    // ---- a repeating meeting ---------------------------------------------

    #[tokio::test]
    async fn two_occurrences_of_one_uid_are_both_briefed_and_the_cache_answers_the_right_one() {
        let (svc, vault, _dir) = env();
        let calendar = account_calendar(&vault);
        let key_of = |e: &Event| key_for(e.calendar_id, &e.uid);
        let mut today = meeting(calendar.id, 600, &["Josh Smith <josh@acme.com>"]);
        today.uid = "standup@series".into();
        let mut tomorrow = meeting(calendar.id, 600 + 24 * 3600, &["Josh Smith <josh@acme.com>"]);
        tomorrow.uid = today.uid.clone();
        assert_eq!(key_of(&today), key_of(&tomorrow));
        seed(&vault, &today);
        seed(&vault, &tomorrow);
        let fake = Fake::default();
        let key = key_of(&today);

        let first = prepare_event(&svc, &vault, today.clone(), &fake).await.unwrap().brief;
        assert_eq!(first.start, today.start);
        // The first is held; asking for the second is not answered by it.
        let second = prepare_event(&svc, &vault, tomorrow.clone(), &fake).await.unwrap().brief;
        assert_eq!(second.start, tomorrow.start);

        // The card by key gets the one that is on now or next.
        assert_eq!(cached(&svc, &key).unwrap().start, today.start);
        // The assistant, by event, gets that event's own.
        let a = for_assistant(&svc, Some(today.id), None).await.unwrap();
        let b = for_assistant(&svc, Some(tomorrow.id), None).await.unwrap();
        assert_eq!((a.brief.start, b.brief.start), (today.start, tomorrow.start));
        assert_eq!((a.brief.agenda.clone(), b.brief.agenda), (first.agenda, second.agenda));
        assert_eq!(a.mail_read_for, None, "no mail was read");
    }
}
