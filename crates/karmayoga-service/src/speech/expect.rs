//! The words to expect, from the vault: what a recogniser is told it is
//! likely to hear while somebody talks to the assistant or writes by voice.
//! Which words, how many and in what form each recogniser takes them is
//! `karmayoga_core::speech::words`; this gathers what it ranks
//! ([`gather`]) and keeps it fresh for a listener that runs all day
//! ([`ExpectedWords`]).
//!
//! A meeting's words come from its invite instead, through the meeting
//! pipeline (`words::for_meeting`), and a podcast's from its show.
//!
//! What is gathered never leaves this computer for a recogniser here; for one
//! on a server it is the prompt that goes with the audio, as a meeting's
//! names always have.

use crate::error::CommandResult;
use crate::service::Service;
use jiff::{Timestamp, ToSpan};
use karmayoga_core::Vault;
use karmayoga_core::meeting::{EventRef, people};
use karmayoga_core::speech::words::{self, WordSources};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

pub use karmayoga_core::speech::words::{Expected, Occasion};

/// How long gathered words are told before they are gathered again: the wake
/// word listens all day, and the day's people and events move on.
const REFRESH: Duration = Duration::from_secs(10 * 60);

/// How far back an event still counts as coming up: one running late.
const EVENTS_SINCE_HOURS: i64 = 1;

/// How far ahead events are looked for: the rest of today and tomorrow.
const EVENTS_UNTIL_HOURS: i64 = 36;

/// The most events looked at: a full day of meetings.
const MAX_EVENTS: usize = 12;

/// How many days of screen time say which apps somebody uses.
const APP_DAYS: i64 = 7;

/// The most of each list gathered, before [`words::expected`] ranks and cuts
/// them much further: enough to rank, few enough to read quickly.
const MAX_GATHERED: usize = 30;

/// The words one listener is told to expect, gathered when first asked for
/// and again once [`REFRESH`] old.
pub struct ExpectedWords {
    source: Source,
    gathered: Mutex<Gathered>,
}

enum Source {
    Vault {
        /// Weak: a listener can run all day, and must not keep a vault that
        /// has been closed -- and its write lock -- alive after it.
        vault: Weak<Vault>,
        occasion: Occasion,
        wake: Vec<String>,
        now: Arc<dyn Fn() -> Timestamp + Send + Sync>,
    },
    Fixed(Arc<Expected>),
}

/// What [`ExpectedWords`] last gathered, and whether it is gathering again.
#[derive(Default)]
struct Gathered {
    words: Option<(Instant, Arc<Expected>)>,
    refreshing: bool,
}

impl ExpectedWords {
    /// Always `expected`, never gathered: for tests, and a caller that knows.
    pub fn fixed(expected: Expected) -> Arc<Self> {
        Arc::new(Self { source: Source::Fixed(Arc::new(expected)), gathered: Mutex::default() })
    }

    /// Nothing to expect.
    pub fn none() -> Arc<Self> {
        Self::fixed(Expected::default())
    }

    /// The words to tell now. The first call gathers them, blocking on the
    /// vault; after that the words last gathered come back at once, and once
    /// they are older than [`REFRESH`] they are gathered again on a thread of
    /// their own, so no utterance waits on the vault for them. A vault that
    /// has since been closed or locked leaves the words it last gave, or none.
    pub fn current(self: &Arc<Self>) -> Arc<Expected> {
        if let Source::Fixed(expected) = &self.source {
            return expected.clone();
        }
        let mut gathered = self.gathered.lock().unwrap_or_else(|e| e.into_inner());
        let Some((at, words)) = gathered.words.clone() else {
            // The first time: gathered while the lock is held, so that warming
            // up and the first utterance gather once between them.
            let words = self.gather(None);
            gathered.words = Some((Instant::now(), words.clone()));
            return words;
        };
        if at.elapsed() >= REFRESH && !gathered.refreshing {
            gathered.refreshing = true;
            let this = self.clone();
            let stale = words.clone();
            let spawned =
                std::thread::Builder::new().name("expected-words".into()).spawn(move || {
                    let fresh = this.gather(Some(stale));
                    let mut gathered = this.gathered.lock().unwrap_or_else(|e| e.into_inner());
                    gathered.words = Some((Instant::now(), fresh));
                    gathered.refreshing = false;
                });
            if spawned.is_err() {
                gathered.refreshing = false;
            }
        }
        words
    }

    /// The words from the vault as it is now, or `stale` -- else none -- when
    /// it is closed or locked. Blocking.
    fn gather(&self, stale: Option<Arc<Expected>>) -> Arc<Expected> {
        let Source::Vault { vault, occasion, wake, now } = &self.source else {
            return stale.unwrap_or_default();
        };
        match vault.upgrade().filter(|v| v.is_unlocked()) {
            Some(vault) => Arc::new(words::expected(*occasion, &gather(&vault, wake, now()))),
            None => stale.unwrap_or_default(),
        }
    }
}

impl Service {
    /// The words to expect while listening for `occasion`, gathered from the
    /// vault on first use.
    pub fn expected_words(
        self: &Arc<Self>,
        occasion: Occasion,
    ) -> CommandResult<Arc<ExpectedWords>> {
        let vault = Arc::downgrade(&self.require_unlocked()?);
        let wake = self.wake_names.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let clock = self.clock();
        let now: Arc<dyn Fn() -> Timestamp + Send + Sync> = Arc::new(move || clock.now());
        Ok(Arc::new(ExpectedWords {
            source: Source::Vault { vault, occasion, wake, now },
            gathered: Mutex::default(),
        }))
    }

    /// Remember the names the wake word listens for, so that voice mode and
    /// voice typing are told to expect them too -- somebody who calls the
    /// assistant "Jarvis" says it there as well.
    pub(crate) fn remember_wake_names(&self, names: &[String]) {
        *self.wake_names.lock().unwrap_or_else(|e| e.into_inner()) = names.to_vec();
    }

    /// Forget them: the wake word was switched off, or the vault closed, and
    /// its names are nothing to expect any more.
    pub fn forget_wake_names(&self) {
        self.wake_names.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

/// Everything the vault knows that a recogniser may mishear, each list most
/// relevant first: see [`WordSources`]. `wake` are the wake word's names;
/// `now` decides which events are coming up. Anything that cannot be read is
/// left out rather than failing the rest. Blocking.
pub fn gather(vault: &Vault, wake: &[String], now: Timestamp) -> WordSources {
    let mut sources = WordSources::default();

    if let Ok(agent) = vault.agent_settings() {
        sources.assistant.push(agent.name);
    }
    sources.assistant.extend(wake.iter().cloned());

    let owner = crate::meeting::pipeline::owner_of(vault);
    sources.owner = owner.name.clone();

    if let Ok(meetings) = vault.meeting_settings() {
        sources.own = karmayoga_core::meeting::vocab::clean_terms(&meetings.vocabulary);
    }

    // Events coming up, soonest first: their titles, their places and the
    // people on them, who come before anybody's circles.
    let tz = vault.person_tz();
    let zone = jiff::tz::TimeZone::get(&tz).unwrap_or(jiff::tz::TimeZone::UTC);
    let since = now - EVENTS_SINCE_HOURS.hours();
    let until = now + EVENTS_UNTIL_HOURS.hours();
    let query = karmayoga_core::store::calendars::EventQuery {
        visible_only: true,
        ..karmayoga_core::store::calendars::EventQuery::between(
            since.to_zoned(zone.clone()).date(),
            until.to_zoned(zone.clone()).date(),
        )
    };
    let mut events: Vec<_> = vault
        .events(&query)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.status != karmayoga_core::calendar::EventStatus::Cancelled)
        .filter(|e| e.end >= since && e.start <= until)
        .collect();
    events.sort_by_key(|e| e.start);
    events.truncate(MAX_EVENTS);
    for event in &events {
        sources.events.push(event.title.clone());
        if !event.location.trim().is_empty() {
            sources.places.push(event.location.clone());
        }
        let reference = EventRef::from_event(event, "");
        sources.people.extend(people::invitees(&reference, &owner).into_iter().map(|i| i.name));
    }

    // Then everybody in a circle, the closest circles first.
    if let (Ok(contacts), Ok(touch)) = (vault.contacts(), vault.keep_in_touch()) {
        let circles = &touch.settings.circles;
        let mut circled: Vec<(usize, String)> = contacts
            .into_iter()
            .filter_map(|item| {
                let circle =
                    item.facts.get(karmayoga_core::keepintouch::FACT_CIRCLE)?.trim().to_string();
                let rank = circles.iter().position(|c| c.name.eq_ignore_ascii_case(&circle))?;
                Some((rank, item.title))
            })
            .collect();
        circled.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        sources.people.extend(circled.into_iter().map(|(_, name)| name).take(MAX_GATHERED));
    }

    // What they are working on: projects, then goals, then areas.
    if let Ok(projects) = vault.projects() {
        sources.work.extend(
            projects
                .into_iter()
                .filter(|p| p.status == karmayoga_core::task::ProjectStatus::Active)
                .map(|p| p.name)
                .take(MAX_GATHERED),
        );
    }
    let active = karmayoga_core::store::purpose::GoalQuery {
        statuses: vec![karmayoga_core::purpose::GoalStatus::Active],
        limit: Some(MAX_GATHERED as u32),
        ..Default::default()
    };
    if let Ok(goals) = vault.goals(&active) {
        sources.work.extend(goals.into_iter().map(|g| g.title));
    }
    if let Ok(areas) = vault.areas() {
        sources.work.extend(areas.into_iter().filter(|a| !a.archived).map(|a| a.name));
    }

    if let Ok(routines) = vault.routines() {
        sources.routines.extend(routines.into_iter().filter(|r| r.enabled).map(|r| r.name));
    }

    // The apps they use most this week, then the services connected to the
    // assistant ("ask Linear"): what they ask it to open and to use.
    if vault.supports_screen_time() {
        let today = now.to_zoned(zone.clone()).date();
        let report = today.checked_sub(APP_DAYS.days()).ok().and_then(|from| {
            let query = karmayoga_core::screentime::ScreenQuery {
                from,
                to: today,
                tz: tz.clone(),
                devices: Vec::new(),
                category: None,
                item: None,
            };
            vault.screen_report(&query).ok()
        });
        if let Some(report) = report {
            let mut apps: Vec<_> = report
                .items
                .into_iter()
                .filter(|i| i.kind == karmayoga_core::screentime::SpanKind::App)
                .collect();
            apps.sort_by_key(|i| std::cmp::Reverse(i.secs));
            sources.apps.extend(apps.into_iter().map(|i| i.name).take(MAX_GATHERED));
        }
    }
    if let Ok(connections) = vault.connection_settings() {
        sources
            .apps
            .extend(connections.connections.into_iter().filter(|c| c.enabled).map(|c| c.name));
    }

    sources
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_vault() -> (Arc<Vault>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let vault = karmayoga_vault::create(
            dir.path(),
            karmayoga_core::VaultConfig { password: None, ..Default::default() },
        )
        .unwrap();
        (Arc::new(vault), dir)
    }

    const ICS: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\n\
        BEGIN:VEVENT\r\nUID:design-1\r\nDTSTART:20261125T100000Z\r\nDTEND:20261125T110000Z\r\n\
        SUMMARY:Design review with Acme\r\nLOCATION:Konark Café\\, 12 MG Road\r\n\
        ORGANIZER;CN=Hari Govardhanam:mailto:hari@example.com\r\n\
        ATTENDEE;CN=Priya Shah:mailto:priya@acme.com\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:old-1\r\nDTSTART:20261120T100000Z\r\nDTEND:20261120T110000Z\r\n\
        SUMMARY:Long ago\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:off-1\r\nDTSTART:20261125T150000Z\r\nDTEND:20261125T160000Z\r\n\
        SUMMARY:Called off\r\nSTATUS:CANCELLED\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn the_vault_gives_the_names_people_places_and_work_to_expect() {
        let (vault, _dir) = test_vault();
        let mut agent = vault.agent_settings().unwrap();
        agent.name = "Karma".into();
        agent.timezone = Some("UTC".into());
        vault.save_agent_settings(&agent).unwrap();
        let mut profile = vault.profile().unwrap();
        profile.first_name = "Hari".into();
        profile.last_name = "Govardhanam".into();
        vault.save_profile(&profile).unwrap();
        let mut meetings = vault.meeting_settings().unwrap();
        meetings.vocabulary = vec!["OKR".into(), " Tailscale ".into()];
        vault.save_meeting_settings(&meetings).unwrap();
        vault.save_project(&karmayoga_core::task::Project::new("Konark wheel")).unwrap();
        let calendar =
            karmayoga_core::calendar::Calendar::subscribed("Work", "https://example.com/w.ics");
        vault.save_calendar(&calendar).unwrap();
        let window = (jiff::civil::date(2026, 11, 1), jiff::civil::date(2026, 12, 31));
        vault.sync_calendar_from_ics(calendar.id, ICS, window, "UTC").unwrap();

        let now: Timestamp = "2026-11-24T20:00:00Z".parse().unwrap();
        let sources = gather(&vault, &["Jarvis".to_string()], now);
        assert_eq!(sources.assistant, vec!["Karma".to_string(), "Jarvis".to_string()]);
        assert_eq!(sources.owner, "Hari Govardhanam");
        assert_eq!(sources.own, vec!["OKR".to_string(), "Tailscale".to_string()]);
        // Only the event coming up, not the one long gone or the one called off.
        assert_eq!(sources.events, vec!["Design review with Acme".to_string()]);
        assert_eq!(sources.places, vec!["Konark Café, 12 MG Road".to_string()]);
        assert!(sources.people.contains(&"Priya Shah".to_string()), "{:?}", sources.people);
        assert!(sources.work.contains(&"Konark wheel".to_string()), "{:?}", sources.work);

        let told = words::expected(Occasion::Conversation, &sources);
        assert!(
            told.prompt.starts_with("Hari Govardhanam talks to Karma or Jarvis."),
            "{}",
            told.prompt
        );
        for word in ["Karma", "Jarvis", "OKR", "Tailscale", "Priya Shah", "Konark Café", "Acme"] {
            assert!(told.boosted.iter().any(|w| w == word), "{word} not in {:?}", told.boosted);
        }
    }

    #[test]
    fn a_listener_does_not_keep_a_closed_vault_and_refreshes_without_waiting() {
        let (vault, _dir) = test_vault();
        let mut agent = vault.agent_settings().unwrap();
        agent.name = "Karma".into();
        vault.save_agent_settings(&agent).unwrap();
        let words = Arc::new(ExpectedWords {
            source: Source::Vault {
                vault: Arc::downgrade(&vault),
                occasion: Occasion::Wake,
                wake: Vec::new(),
                now: Arc::new(Timestamp::now),
            },
            gathered: Mutex::default(),
        });
        let first = words.current();
        assert!(first.boosted.contains(&"Karma".to_string()), "{first:?}");

        // Closed: the listener's hold on it is not what keeps it.
        let weak = Arc::downgrade(&vault);
        drop(vault);
        assert!(weak.upgrade().is_none(), "the listener kept the vault alive");

        // Stale now: the old words come back at once, and the refresh, with
        // no vault to read, leaves them.
        {
            let mut gathered = words.gathered.lock().unwrap();
            let (at, _) = gathered.words.as_mut().unwrap();
            *at = Instant::now().checked_sub(REFRESH + Duration::from_secs(1)).unwrap();
        }
        assert_eq!(words.current(), first);
        let began = Instant::now();
        while words.gathered.lock().unwrap().refreshing {
            assert!(began.elapsed() < Duration::from_secs(10), "the refresh never finished");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(words.current(), first);
    }

    #[test]
    fn fixed_words_are_what_was_given() {
        let expected =
            Expected { prompt: "Talking to Karma.".into(), boosted: vec!["Karma".into()] };
        assert_eq!(*ExpectedWords::fixed(expected.clone()).current(), expected);
        assert!(ExpectedWords::none().current().is_empty());
    }
}
