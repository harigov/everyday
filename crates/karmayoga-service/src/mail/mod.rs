//! Mail, as the service runs it: keeping accounts in sync, sending what is
//! waiting, showing a message, and everything a model is asked to do with it.
//!
//! The protocols themselves -- IMAP, SMTP, OAuth, MIME, sanitising -- are
//! `karmayoga-mail`'s, which knows nothing about a vault; the records and the
//! rules about them are `karmayoga_core::mail`'s. This module is the seam: it
//! drives the first against the second, on a schedule and on request. The
//! commands that reach it are in [`crate::domains`] (`mail*.rs`), and its
//! session state -- the open pack store and index, the outbox's wake handles,
//! the model budgets -- is [`crate::runtime::mail::MailRuntime`].
//!
//! | module        | what it holds                                                          |
//! |---------------|------------------------------------------------------------------------|
//! | [`sync`]      | one supervised sync task per account, and the storage it opens          |
//! | [`outbox`]    | draining an account's outbox through `karmayoga_mail::outbox::execute`  |
//! | [`view`]      | turning a stored message into what a sandboxed frame shows             |
//! | [`preview`]   | looking at an attachment without saving it first                       |
//! | [`ai`]        | categorising, priority, summaries and auto-drafts on the quick model   |
//! | [`sort`]      | sorting into the person's own categories on a decision model           |
//! | [`followup`]  | mail waiting on the person, and on somebody else                       |
//! | [`check`]     | the checks Settings → Mail writing asks for before a message is sent   |
//! | [`write`]     | writing help on request: replies, rewrites                             |
//! | [`voice`]     | the person's own voice, gathered from the vault for a prompt           |
//! | [`voicelearn`]| learning how the person replies, from the replies they sent            |
//!
//! `outbox` here is the vault's half of sending; `karmayoga_mail::outbox` is
//! the protocol half it calls. Same word, two layers.

pub mod ai;
pub mod check;
pub mod followup;
pub mod outbox;
pub mod preview;
pub mod sort;
pub mod sync;
pub mod view;
pub mod voice;
pub mod voicelearn;
pub mod write;
