//! Mail sync: one supervised task per account, and the storage it opens
//! before the first one can run.
//!
//! `docs/plans/mail.md`'s phase 2, "how a mailbox arrives" and "the pack
//! store", built on top of the trait `everyday-mail::session::MailSession`
//! gives the engine and the stores `everyday-core::store::mail` and
//! `everyday-core::packstore`/`everyday-mailindex` already provide. Nothing
//! in `everyday-core` or `everyday-mail` knows this module exists -- the
//! inversion runs the other way, same as everywhere else in this tree: this
//! module is what *drives* those traits, on a schedule, against a live
//! server.
//!
//! # Layout
//!
//! - [`wiring`] -- opening a pack store and a mail search index when the
//!   vault unlocks, deriving their keys from the vault's own, and
//!   registering one supervised task per account with `services.mail` on.
//!   `Service::mail_index` and `Service::packs` are how the rest of the
//!   service -- search commands, the protocol routes, the outbox -- reach
//!   them.
//! - [`credential`] -- turning an [`everyday_core::Account`]'s
//!   [`everyday_core::AuthMethod`] and stored secret into an
//!   [`everyday_mail::session::Credential`] a [`MailSession`] can
//!   authenticate with, refreshing an OAuth token through the service's
//!   [`crate::token_cache::TokenCache`] and persisting a rotated refresh
//!   token as it goes.
//! - [`discovery`] -- `LIST`ing an account's mailboxes and deciding which
//!   ones to sync: every selectable folder on an ordinary server, five
//!   fixed ones on Gmail (see that module's docs for the label mapping).
//! - [`ingest`] -- turning one fetched header into a [`everyday_core::MailMessage`]
//!   row, including the part neither `everyday-mail::threading` nor
//!   `everyday-core::store::mail` can do alone: finding which existing
//!   thread (and, on Gmail especially, whether it is even a *new* message
//!   rather than one this account already has under another folder) a
//!   message belongs to, without re-scanning every message this account
//!   has ever stored.
//! - [`passes`] -- the three-pass first sync and the steady-state sweep,
//!   generic over [`MailSession`] so the same code runs against
//!   `everyday-mail::imap::ImapSession` in production and a fake in
//!   [`tests`].
//! - [`sender`] -- [`sender::LazySmtpSender`], the `everyday_mail::outbox::Sender`
//!   one account's task hands to `crate::outbox::drain_outbox`: an SMTP
//!   connection opened on the first `OpKind::Send`, from the same credential
//!   resolution as IMAP, and kept pooled after that.
//! - [`task`] -- [`task::run_account`], the whole of one account's
//!   supervised task: connect, sync, drain the outbox, hold `IDLE`,
//!   reconnect, repeat.
//! - [`status`] -- what `sync_status` reads: each account's current phase,
//!   progress and last error, kept in memory and updated as a sync runs.
//! - [`unread_cache`] -- [`unread_cache::UnreadCache`], caching
//!   `MailStore::unread_counts` and invalidated by the two writes that can
//!   change it: the sync engine's own headers pass, and a person's batch
//!   actions in `crate::domains::mail`.
//!
//! # Why generic over `MailSession` rather than boxed
//!
//! [`MailSession`]'s own docs explain why it is not `dyn`-safe (an
//! `async fn` in a public trait cannot promise `Send` without being written
//! out by hand, which this trait's own callers -- this module included --
//! do not need to ask of a stranger's generic code). Every function here
//! that drives a session is generic over `S: MailSession` for the same
//! reason: one connection type at a time, chosen once where the task is
//! built ([`task::run_account`]'s caller in [`wiring`], for
//! `everyday_mail::imap::ImapSession`; each test in [`tests`], for its own
//! fake).

pub mod contacts;
pub mod credential;
pub mod discovery;
pub mod ingest;
pub mod passes;
pub mod sender;
pub mod status;
pub mod task;
pub mod unread_cache;
pub mod wiring;

#[cfg(test)]
mod tests;
