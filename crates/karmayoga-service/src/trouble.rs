//! Telling somebody that an account or a calendar has stopped syncing.
//!
//! The account view in Settings has always said when an account needs signing
//! in again or its last sync failed, and the calendar sidebar says so beside a
//! calendar. Both are only seen by somebody looking. Mail and calendars sync
//! with nobody looking, and what they bring in is what the notifications, the
//! reminders and the assistant work from, so an account that has quietly
//! stopped is a reminder at an old meeting time or a reply nobody saw. This is
//! the notification for that:
//!
//! * **Sign in again** -- at once, because it will not fix itself, and once per
//!   account per session, whichever of mail, a calendar or a send noticed it:
//!   one revoked token is one fact. Asked again once the account has synced in
//!   between, so a second revocation is news.
//! * **Mail has stopped**, and **a calendar has stopped** -- once failures have
//!   gone on for [`OUTAGE_WORTH_SAYING`], so a network blink, a server restart
//!   or the first pass after a laptop wakes says nothing. Once per outage; mail
//!   that lands afterwards says it is back, under the same key, so the warning
//!   is replaced rather than left standing. Calendars say theirs beside the
//!   calendar pass (see `domains::calendars`), since what is stale there is a
//!   calendar, not an account.
//!
//! # A machine that is offline is not every account failing
//!
//! A failure that could not reach the server at all -- no route, no answer,
//! a timeout -- is said only once something else this session syncs has been
//! reached since the outage began (see [`SyncTrouble::reached`]). A laptop on
//! a train fails every account and every calendar at once and reaches none of
//! them, so it hears nothing, and nothing on reconnecting either; one server
//! that is down while the others answer is said. A server that answered and
//! refused is said whatever else is reachable.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::Mutex;

use jiff::Timestamp;
use karmayoga_core::Vault;
use karmayoga_core::account::Account;
use karmayoga_core::id::{AccountId, CalendarId};

use crate::events::{Level, Notification};
use crate::service::Service;

/// How long something has to have been failing before anybody is told.
/// Quarter of an hour outlasts a network blink, a server having a bad moment
/// and the first few retries, and is well inside the time a moved meeting's
/// reminder would go out at its old time or a reply would sit unseen.
pub const OUTAGE_WORTH_SAYING: i64 = 15 * 60;

/// Failures further apart than this are not one outage. Mail is retried at
/// most five minutes apart and a failing calendar every five minutes, so a gap
/// like this is a machine that was asleep, and nothing is known about what
/// was going on meanwhile: it is a new outage, said again if it lasts.
const OUTAGE_GAP: i64 = 30 * 60;

/// Longest a server's own words may run in a notification.
const REASON_CHARS: usize = 140;

/// One thing that keeps failing.
struct Outage {
    since: Timestamp,
    last: Timestamp,
    said: bool,
}

/// What is failing right now, by key -- an account's mail or a calendar -- and
/// which of those failures have been said. The one rule both follow.
pub struct Outages<K>(Mutex<HashMap<K, Outage>>);

impl<K> Default for Outages<K> {
    fn default() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
}

impl<K: Eq + Hash + Copy> Outages<K> {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<K, Outage>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Note that `key` failed at `now`. How long its outage has lasted, in
    /// seconds, when it is worth saying now: long enough, not said yet, and --
    /// for a failure that never reached the server (`unreachable`) -- with
    /// something else reached since it began (`reached`). Saying it is the
    /// caller's; [`Outages::said`] records that it was, once it has been.
    pub fn failed(
        &self,
        key: K,
        now: Timestamp,
        unreachable: bool,
        reached: Option<Timestamp>,
    ) -> Option<i64> {
        let mut map = self.map();
        let outage = map.entry(key).or_insert(Outage { since: now, last: now, said: false });
        if now.as_second() - outage.last.as_second() > OUTAGE_GAP {
            *outage = Outage { since: now, last: now, said: false };
        }
        outage.last = now;
        let lasted = now.as_second() - outage.since.as_second();
        let online = !unreachable || reached.is_some_and(|at| at > outage.since);
        (!outage.said && lasted >= OUTAGE_WORTH_SAYING && online).then_some(lasted)
    }

    /// Record that `key`'s outage has been said.
    pub fn said(&self, key: K) {
        if let Some(outage) = self.map().get_mut(&key) {
            outage.said = true;
        }
    }

    /// `key` worked. True when its outage had been said, so the end of it is
    /// worth saying too.
    pub fn ended(&self, key: K) -> bool {
        self.map().remove(&key).is_some_and(|o| o.said)
    }

    /// Whether `key` may be tried again at `now`: always, unless it is failing
    /// and its last attempt was less than `every` seconds ago.
    pub fn retry_due(&self, key: K, now: Timestamp, every: i64) -> bool {
        self.map().get(&key).is_none_or(|o| now.as_second() - o.last.as_second() >= every)
    }

    /// Every key whose outage has been said and is still going on.
    pub fn said_keys(&self) -> Vec<K> {
        self.map().iter().filter(|(_, o)| o.said).map(|(k, _)| *k).collect()
    }

    fn clear(&self) {
        self.map().clear();
    }
}

/// This session's telling about accounts and calendars. Session state, like
/// the rest of [`Service`]'s reporting -- cleared when the vault closes, so
/// opening the app again is a moment to be told again.
#[derive(Default)]
pub struct SyncTrouble {
    mail: Outages<AccountId>,
    calendars: Outages<CalendarId>,
    sign_in: Mutex<HashSet<AccountId>>,
    reached: Mutex<Option<Timestamp>>,
}

impl SyncTrouble {
    /// Calendars that are failing to refresh: what `domains::calendars` asks.
    pub fn calendars(&self) -> &Outages<CalendarId> {
        &self.calendars
    }

    /// The last time anything this session syncs -- an account's mail, any
    /// calendar -- answered: what tells an offline machine from one server
    /// being down. See the module docs.
    pub fn reached(&self) -> Option<Timestamp> {
        *self.reached.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Something synced at `now`.
    pub fn note_reached(&self, now: Timestamp) {
        let mut reached = self.reached.lock().unwrap_or_else(|e| e.into_inner());
        if reached.is_none_or(|at| at < now) {
            *reached = Some(now);
        }
    }

    fn sign_in(&self) -> std::sync::MutexGuard<'_, HashSet<AccountId>> {
        self.sign_in.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Forget everything: the vault closed.
    pub fn clear(&self) {
        self.mail.clear();
        self.calendars.clear();
        self.sign_in().clear();
        *self.reached.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

fn key(id: AccountId, what: &str) -> String {
    format!("account:{id}:{what}")
}

/// What stopped, in a sentence's worth of words: the services the account
/// syncs.
fn what_stopped(account: &Account) -> &'static str {
    match (account.services.mail, account.services.calendar) {
        (true, true) => "Its mail and calendars have stopped syncing",
        (false, true) => "Its calendars have stopped syncing",
        _ => "Its mail has stopped syncing",
    }
}

/// The server's own words, cut to a line.
fn short(reason: &str) -> String {
    let reason = reason.trim();
    if reason.chars().count() <= REASON_CHARS {
        return reason.to_string();
    }
    let cut: String = reason.chars().take(REASON_CHARS - 1).collect();
    format!("{}\u{2026}", cut.trim_end())
}

/// `account_id` needs somebody to sign in to it again: say so, once. Called by
/// whatever noticed -- the mail task, a calendar's token refresh, a send.
/// Recorded as said only once it has been: an account that cannot be read
/// right now is asked about the next time.
pub fn sign_in_needed(svc: &Service, vault: &Vault, account_id: AccountId) {
    let trouble = svc.sync_trouble();
    if trouble.sign_in().contains(&account_id) {
        return;
    }
    let Ok(account) = vault.account(account_id) else { return };
    if !trouble.sign_in().insert(account_id) {
        return;
    }
    svc.events().notify(
        Notification::warning(format!("Sign in to {} again", account.address))
            .body(format!(
                "{} until you do. Sign in from Settings \u{2192} Accounts.",
                what_stopped(&account)
            ))
            .for_user()
            .key(key(account_id, "sign-in")),
    );
}

/// One attempt at syncing `account_id`'s mail failed with `reason` --
/// `unreachable` when it never reached the server. Says so once the failures
/// have gone on for [`OUTAGE_WORTH_SAYING`]; see the module docs for when an
/// unreachable server is not said at all.
pub fn mail_failed(
    svc: &Service,
    vault: &Vault,
    account_id: AccountId,
    reason: &str,
    unreachable: bool,
) {
    let trouble = svc.sync_trouble();
    let Some(lasted) = trouble.mail.failed(account_id, svc.now(), unreachable, trouble.reached())
    else {
        return;
    };
    let Ok(account) = vault.account(account_id) else { return };
    if !account.services.mail {
        return;
    }
    svc.events().notify(
        Notification::warning(format!("Mail from {} is not syncing", account.address))
            .body(format!(
                "Nothing new has come in for {} minutes. The last try said: {}",
                lasted / 60,
                short(reason)
            ))
            .for_user()
            .key(key(account_id, "mail")),
    );
    trouble.mail.said(account_id);
}

/// A pass over `account_id`'s mail landed. Says so when it ends an outage
/// that was said, replacing that warning.
pub fn mail_synced(svc: &Service, vault: &Vault, account_id: AccountId) {
    let trouble = svc.sync_trouble();
    trouble.note_reached(svc.now());
    trouble.sign_in().remove(&account_id);
    if !trouble.mail.ended(account_id) {
        return;
    }
    let Ok(account) = vault.account(account_id) else { return };
    svc.events().notify(
        Notification::new(
            Level::Success,
            format!("Mail from {} is syncing again", account.address),
        )
        .for_user()
        .key(key(account_id, "mail")),
    );
}

/// One of `account_id`'s calendars synced: a sign-in asked for before is no
/// longer outstanding, so the next one is news.
pub fn calendar_synced(svc: &Service, account_id: AccountId) {
    svc.sync_trouble().sign_in().remove(&account_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mins(m: i64) -> Timestamp {
        Timestamp::from_second(1_800_000_000 + m * 60).unwrap()
    }

    #[test]
    fn an_outage_is_said_once_it_has_lasted_and_once_said() {
        let outages = Outages::default();
        let id = AccountId::new();
        assert_eq!(outages.failed(id, mins(0), false, None), None);
        assert_eq!(outages.failed(id, mins(5), false, None), None);
        assert_eq!(outages.failed(id, mins(15), false, None), Some(15 * 60));
        assert_eq!(
            outages.failed(id, mins(16), false, None),
            Some(16 * 60),
            "not said yet: the caller may have failed to say it"
        );
        outages.said(id);
        assert_eq!(outages.failed(id, mins(20), false, None), None, "once per outage");
        assert!(outages.ended(id), "its end is worth saying");
        assert!(!outages.ended(id), "and only once");
    }

    #[test]
    fn failures_far_apart_are_two_outages_each_said() {
        let outages = Outages::default();
        let id = CalendarId::new();
        assert_eq!(outages.failed(id, mins(0), false, None), None);
        assert!(outages.failed(id, mins(15), false, None).is_some());
        outages.said(id);
        assert_eq!(outages.failed(id, mins(60), false, None), None, "a machine that slept");
        assert_eq!(outages.failed(id, mins(70), false, None), None);
        assert_eq!(outages.failed(id, mins(75), false, None), Some(15 * 60), "a new outage");
    }

    #[test]
    fn an_unreachable_server_is_said_only_once_something_else_answered() {
        let outages = Outages::default();
        let id = AccountId::new();
        assert_eq!(outages.failed(id, mins(0), true, Some(mins(-1))), None);
        assert_eq!(
            outages.failed(id, mins(20), true, Some(mins(-1))),
            None,
            "nothing reached since it began: this machine is offline"
        );
        assert_eq!(outages.failed(id, mins(25), true, Some(mins(22))), Some(25 * 60));
        let refused = AccountId::new();
        assert!(outages.failed(refused, mins(0), false, None).is_none());
        assert!(
            outages.failed(refused, mins(15), false, None).is_some(),
            "a server that answered and refused is said whatever else is reachable"
        );
    }

    #[test]
    fn a_failing_key_is_retried_only_every_so_often() {
        let outages = Outages::default();
        let id = CalendarId::new();
        assert!(outages.retry_due(id, mins(0), 300));
        outages.failed(id, mins(0), false, None);
        assert!(!outages.retry_due(id, mins(2), 300));
        assert!(outages.retry_due(id, mins(5), 300));
    }

    #[test]
    fn only_said_outages_are_listed_as_said() {
        let outages = Outages::default();
        let (a, b) = (CalendarId::new(), CalendarId::new());
        outages.failed(a, mins(0), false, None);
        outages.failed(b, mins(0), false, None);
        outages.said(a);
        assert_eq!(outages.said_keys(), vec![a]);
    }

    #[test]
    fn the_latest_moment_anything_answered_is_kept() {
        let trouble = SyncTrouble::default();
        assert_eq!(trouble.reached(), None);
        trouble.note_reached(mins(5));
        trouble.note_reached(mins(3));
        assert_eq!(trouble.reached(), Some(mins(5)));
        trouble.clear();
        assert_eq!(trouble.reached(), None);
    }

    #[test]
    fn a_long_reason_is_cut_to_a_line() {
        assert_eq!(short("  refused  "), "refused");
        let long = "x".repeat(400);
        let cut = short(&long);
        assert_eq!(cut.chars().count(), REASON_CHARS);
        assert!(cut.ends_with('\u{2026}'));
    }
}
