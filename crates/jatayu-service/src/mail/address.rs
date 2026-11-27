//! Addresses as the mail modules read them: how one is shown to a model, and
//! which of them are the person's own.

use std::collections::HashSet;

use jatayu_core::account::Account;
use jatayu_core::mail::Address;

/// "Dana Scully <dana@example.com>", or the bare address when there is no name.
pub(crate) fn display_address(a: &Address) -> String {
    if a.name.is_empty() { a.email.clone() } else { format!("{} <{}>", a.name, a.email) }
}

/// The account's own addresses, lower-cased -- what
/// [`jatayu_core::mail::authorship::own_addresses`] says it is, under the
/// name the mail modules ask it by.
pub(crate) fn own_addresses(account: &Account) -> HashSet<String> {
    jatayu_core::mail::authorship::own_addresses(account)
}

/// The account's addresses as [`mailbox_key`](crate::arrivals::mailbox_key)s
/// -- what `own`, everywhere in the mail modules, is a set of. Built once for a
/// pass, so that no message has the whole set read again to be compared with.
pub(crate) fn own_mailboxes(account: &Account) -> HashSet<String> {
    own_addresses(account).iter().map(|a| crate::arrivals::mailbox_key(a)).collect()
}

/// Whether `email` is one of `own` ([`own_mailboxes`]) as the mail provider
/// delivers it: `h.ari+lists@googlemail.com` is `hari@gmail.com`.
pub(crate) fn is_own(email: &str, own: &HashSet<String>) -> bool {
    own.contains(&crate::arrivals::mailbox_key(email))
}
