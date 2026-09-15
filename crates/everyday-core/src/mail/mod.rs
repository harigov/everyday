//! Mail: the records a synced mailbox is made of, and the pure rules over
//! them.
//!
//! `docs/plans/mail.md` calls this phase 2 ("reading mail") plus the
//! draft/outbox records phase 3 adds early, "so that there is only ever one
//! way a message leaves." Nothing in this module speaks IMAP or SMTP, opens
//! a socket, or knows what a vault is — that is `everyday-mail` and
//! `crate::store::mail` respectively. What lives here is the shape of the
//! data (`records`), the state machine and the reversible local writes an
//! outbox needs (`outbox`), and the rate limiting an unattended caller is
//! held to (`rate_limit`): all of it synchronous, all of it offline, all of
//! it testable without a database.
//!
//! # Why `MailMessageId` and not `MessageId`
//!
//! `crate::id::MessageId` already names one turn in a chat with the
//! assistant (`crate::agent::Message`). A mail message is an unrelated thing
//! that happens to want the same English word, so its id and its record are
//! spelled `MailMessageId` and, inside this module's own namespace,
//! [`records::Message`] — reached as `mail::Message`, never as a bare
//! `Message` at the crate root, so the two can never be confused at a call
//! site that imports both.
//!
//! # What is sealed and what is not
//!
//! Every record here follows the rule the rest of this crate's storage
//! layer already keeps: an id can be looked up, a date can be sorted on, a
//! flag can be counted — and nothing a person wrote, or anyone's address,
//! ever sits outside the envelope. See `crate::store::mail` for exactly
//! which columns each table leaves in the clear; this module only defines
//! the shapes that get sealed.

pub mod contacts;
pub mod outbox;
pub mod rate_limit;
pub mod records;

pub use contacts::{ContactBook, MailContact};
pub use outbox::{RETRY_BACKOFF, apply_optimistic, backoff_for_attempt, revert, undo_send_delay};
pub use rate_limit::{RateLimitState, TokenBucket};
pub use records::*;
