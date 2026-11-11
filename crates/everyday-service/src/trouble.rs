//! Telling somebody that an account has stopped syncing.
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
//! * **Mail has stopped** -- once the account's sync has been failing for
//!   [`OUTAGE_WORTH_SAYING`], so a network blink, a server restart or the
//!   first pass after a laptop wakes says nothing. Once per outage; a pass that
//!   lands afterwards says it is back, under the same key, so the warning is
//!   replaced rather than left standing.
//!
//! Calendars keep their own outage rule beside the calendar pass (see
//! `domains::calendars::sync_due` and [`crate::service::Service::feed_failed`]),
//! since what is stale there is a calendar, not an account.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use everyday_core::Vault;
use everyday_core::account::Account;
use everyday_core::id::AccountId;
use jiff::Timestamp;

use crate::events::{Level, Notification};
use crate::service::Service;

/// How long an account's mail has to have been failing before anybody is
/// told. Long enough to outlast a network blink and the supervisor's first
/// few retries, short enough that a stopped inbox is noticed within the hour
/// rather than at the end of the day.
pub const OUTAGE_WORTH_SAYING: i64 = 15 * 60;

/// Failures further apart than this are not one outage: the supervisor
/// retries a failing account at most five minutes apart, so a gap like this
/// is a machine that was asleep.
const OUTAGE_GAP: i64 = 30 * 60;

/// Longest a server's own words may run in a notification.
const REASON_CHARS: usize = 140;

/// An account whose mail sync is failing.
struct Outage {
    since: Timestamp,
    last: Timestamp,
    said: bool,
}

/// This session's telling about accounts: which outages are going on and have
/// been said, and which accounts have been asked to sign in. Session state, like
/// the rest of [`Service`]'s reporting -- cleared when the vault closes, so
/// opening the app again is a moment to be told again.
#[derive(Default)]
pub struct SyncTrouble {
    mail: Mutex<HashMap<AccountId, Outage>>,
    sign_in: Mutex<HashSet<AccountId>>,
}

impl SyncTrouble {
    /// Note that `id`'s mail sync failed at `now`. How long the outage has
    /// lasted, in seconds, the first time that is [`OUTAGE_WORTH_SAYING`] or
    /// more -- once per outage, and `None` every other time.
    fn mail_failed(&self, id: AccountId, now: Timestamp) -> Option<i64> {
        let mut outages = self.mail.lock().unwrap_or_else(|e| e.into_inner());
        let outage = outages.entry(id).or_insert(Outage { since: now, last: now, said: false });
        if now.as_second() - outage.last.as_second() > OUTAGE_GAP {
            *outage = Outage { since: now, last: now, said: false };
        }
        outage.last = now;
        let lasted = now.as_second() - outage.since.as_second();
        if outage.said || lasted < OUTAGE_WORTH_SAYING {
            return None;
        }
        outage.said = true;
        Some(lasted)
    }

    /// Note that `id` synced. True when an outage had been said, so its end is
    /// worth saying too.
    fn synced(&self, id: AccountId) -> bool {
        self.sign_in.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        self.mail.lock().unwrap_or_else(|e| e.into_inner()).remove(&id).is_some_and(|o| o.said)
    }

    /// True the first time this session `id` is found needing a sign-in since
    /// it last synced.
    fn needs_sign_in(&self, id: AccountId) -> bool {
        self.sign_in.lock().unwrap_or_else(|e| e.into_inner()).insert(id)
    }

    /// Forget everything: the vault closed.
    pub fn clear(&self) {
        self.mail.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.sign_in.lock().unwrap_or_else(|e| e.into_inner()).clear();
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
pub fn sign_in_needed(svc: &Service, vault: &Vault, account_id: AccountId) {
    if !svc.sync_trouble().needs_sign_in(account_id) {
        return;
    }
    let Ok(account) = vault.account(account_id) else { return };
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

/// One attempt at syncing `account_id`'s mail failed with `reason`. Says so
/// once the failures have gone on for [`OUTAGE_WORTH_SAYING`].
pub fn mail_failed(svc: &Service, vault: &Vault, account_id: AccountId, reason: &str) {
    let Some(lasted) = svc.sync_trouble().mail_failed(account_id, svc.now()) else { return };
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
}

/// A pass over `account_id`'s mail landed. Says so when it ends an outage
/// that was said, replacing that warning.
pub fn mail_synced(svc: &Service, vault: &Vault, account_id: AccountId) {
    if !svc.sync_trouble().synced(account_id) {
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

/// One of `account_id`'s calendars synced: a sign-in asked for before it is
/// no longer outstanding, so the next one is news.
pub fn calendar_synced(svc: &Service, account_id: AccountId) {
    svc.sync_trouble().sign_in.lock().unwrap_or_else(|e| e.into_inner()).remove(&account_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_second(1_800_000_000 + secs).unwrap()
    }

    #[test]
    fn a_mail_outage_is_said_once_it_has_lasted() {
        let trouble = SyncTrouble::default();
        let id = AccountId::new();
        assert_eq!(trouble.mail_failed(id, at(0)), None);
        assert_eq!(trouble.mail_failed(id, at(5 * 60)), None);
        assert_eq!(trouble.mail_failed(id, at(15 * 60)), Some(15 * 60));
        assert_eq!(trouble.mail_failed(id, at(20 * 60)), None, "once per outage");
        assert!(trouble.synced(id), "its end is worth saying");
        assert!(!trouble.synced(id), "and only once");
    }

    #[test]
    fn failures_far_apart_are_not_one_outage() {
        let trouble = SyncTrouble::default();
        let id = AccountId::new();
        assert_eq!(trouble.mail_failed(id, at(0)), None);
        assert_eq!(trouble.mail_failed(id, at(40 * 60)), None, "a machine that slept");
        assert_eq!(trouble.mail_failed(id, at(50 * 60)), None);
        assert_eq!(trouble.mail_failed(id, at(55 * 60)), Some(15 * 60));
    }

    #[test]
    fn a_sync_with_nothing_said_says_nothing() {
        let trouble = SyncTrouble::default();
        let id = AccountId::new();
        assert_eq!(trouble.mail_failed(id, at(0)), None);
        assert!(!trouble.synced(id));
    }

    #[test]
    fn a_sign_in_is_asked_for_once_until_the_account_syncs() {
        let trouble = SyncTrouble::default();
        let id = AccountId::new();
        assert!(trouble.needs_sign_in(id));
        assert!(!trouble.needs_sign_in(id), "mail and a calendar noticing is one fact");
        trouble.synced(id);
        assert!(trouble.needs_sign_in(id), "a second revocation is news");
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
