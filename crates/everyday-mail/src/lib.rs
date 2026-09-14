//! Mail, from the bytes a server sends to the rows a vault keeps, and back.
//!
//! Two halves, kept apart inside one crate so a change to either is easy to
//! find and so the half that needs no network can be tested in microseconds:
//!
//! * **What happens to a message.** [`mime`] reads RFC 822 bytes into the
//!   parts a client draws; [`sanitize`] turns untrusted HTML into something a
//!   sandboxed frame can show, with every remote reference rewritten to the
//!   app's own protocol; [`text`] produces the snippet, the quoted and
//!   signature ranges, and the plain text an assistant reads; [`threading`]
//!   groups messages into conversations when the server does not. None of it
//!   opens a socket.
//! * **Talking to a server.** [`session`] is the trait the sync engine is
//!   written against, so that the library underneath is a detail an adapter
//!   hides; [`imap`] is the first adapter, and [`smtp`] sends what
//!   [`compose`] built. [`compose`] itself opens no socket — it is what
//!   turns a compose box, a reply or a forward into RFC 5322 bytes and an
//!   envelope, sitting beside [`mime`] as the "what happens to a message"
//!   half rather than the "talking to a server" half. [`imap`] and [`smtp`]
//!   are the only modules with an async runtime or a TLS stack in them.
//!
//! `everyday-core` stays synchronous and offline, as it always has: the
//! records and the store traits a mailbox lands in live there, and this crate
//! is what fills them. See `docs/plans/mail.md`.

pub mod compose;
pub mod imap;
pub mod mime;
pub mod sanitize;
pub mod session;
pub mod smtp;
pub mod text;
pub mod threading;
