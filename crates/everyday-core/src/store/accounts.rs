//! Storage for accounts: the vault-level record of a mailbox you have signed
//! in to.
//!
//! Four methods, the same shape as [`routines::RoutineStore`](super::routines)
//! for the same reason — there are a handful of accounts in even a busy
//! vault, so filtering, sorting and paging are the caller's business rather
//! than this trait's.
//!
//! # The one cascade this trait owns
//!
//! Deleting an account takes its secret with it — a refresh token or a
//! password left behind once the account that used it is gone would be a
//! credential this application can no longer even show the owner of — and
//! the pointer rows in `account_calendars` that say which subscribed
//! calendars came from it, laid down in phase 1 for phase 6 to use. Both live
//! outside this trait's own table (the secret in
//! [`SecretStore`](super::secrets::SecretStore), the calendar pointers beside
//! `calendars`), so [`AccountStore::delete_account`] is the one method here
//! that reaches into storage this trait does not otherwise own — the same
//! trade [`routines::RoutineStore::delete_routine`](super::routines::RoutineStore::delete_routine)
//! makes for a routine's runs.
//!
//! # What stays sealed
//!
//! The whole record is sealed. Unlike almost everything else in this crate,
//! an account has no clear column beyond `created_us` and `updated_us`: there
//! is no list to filter or sort by provider, status or address at the scale
//! that would make an index worth the leak, and "how many accounts, and
//! roughly when each was added" is already more than this trait needs to
//! leak to draw a settings page. See [`account_aad`].

use crate::account::Account;
use crate::error::Result;
use crate::id::AccountId;

/// The persistence contract for accounts.
pub trait AccountStore: Send + Sync {
    /// Every account, in the order they were added. A handful of rows, so
    /// sorting for display is the caller's business.
    fn list_accounts(&self) -> Result<Vec<Account>>;

    fn get_account(&self, id: AccountId) -> Result<Account>;

    /// Insert or replace. Implementations must be idempotent.
    fn put_account(&self, account: &Account) -> Result<()>;

    /// Delete the account, its secret, and every calendar and event that
    /// came from it, found through the pointer rows in `account_calendars`.
    /// Deleting one that is not there is not an error, the same contract
    /// every other domain's delete makes.
    ///
    /// Calendars are taken because [`CalendarOrigin::Account`](crate::calendar::CalendarOrigin::Account)
    /// makes them exactly as dependent on the account as a feed's events are
    /// on the feed: read-only, and meaningless once the credential that read
    /// them is gone. This is a deliberate change from phase 1, when this
    /// same doc said the opposite -- there was nothing to take yet.
    /// Mailboxes, messages and threads, which mail's later phases add under
    /// an account, are each a cascade of their own domain's and belong to
    /// whichever store owns that domain, not to this one.
    fn delete_account(&self, id: AccountId) -> Result<()>;
}

/// Associated data an account's sealed payload is bound to. See
/// [`entry_aad`](super::entry_aad) for why records are bound to their own id.
pub fn account_aad(id: AccountId) -> Vec<u8> {
    format!("everyday.account.v1:{id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_aad_differs_by_id() {
        let a = account_aad(AccountId::new());
        let b = account_aad(AccountId::new());
        assert_ne!(a, b);
        assert!(String::from_utf8(a).unwrap().starts_with("everyday.account."));
    }
}
