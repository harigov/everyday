//! Everything a client can ask a vault to do, with no window in sight.
//!
//! This crate is the middle of the application. Below it, [`everyday_core`]
//! knows about journals, encryption and storage and has no async runtime, no
//! TLS stack and no way to open a socket. Above it sit the things that own a
//! process: the desktop shell, the `serve` command, the command-line tool. This
//! is what they all call.
//!
//! ```text
//!   everyday-core      the domain: records, crypto, storage, iCalendar, search
//!   everyday-vault     wires the core to a backend; platform paths
//!   everyday-service   THIS: the command surface, the feeds, the assistant
//!   everyday-app       a window, a tray, a media protocol
//!   everyday-server    a socket, a certificate, and paired devices
//!   everyday-cli       a terminal
//! ```
//!
//! # One surface
//!
//! [`Service::call`] runs any command by name from JSON and answers with JSON.
//! The interface calls it, the CLI calls it, the server exposes it over a
//! socket and over TLS, and the assistant's tools sit beside it. There is
//! deliberately no second API: a browser extension saving an article calls the
//! same `add_item` the library's capture line calls, and inherits the same
//! rules about what a lookup may overwrite.
//!
//! Three things are not JSON in and JSON out, and each has its own entry point:
//! attachments ([`Service::put_blob`], [`Service::blob_range`]), because a
//! hundred megabytes of video should not be an array of numbers; and a turn of
//! the assistant ([`Service::send_message`]), because it answers with a stream.
//!
//! A fourth is JSON and is chunked for the same reason as the first: an
//! export of a whole vault. See [`domains::transfer`] and [`transfers`].
//!
//! # What is deliberately absent
//!
//! Opening a vault, creating one, and deciding which one this session is about.
//! Those decide what a service *is*, and they belong to whatever owns the
//! process. A service is handed a vault; it does not go looking for one.

pub mod agent;
pub mod command;
pub mod ctx;
pub mod domains;
pub mod error;
pub mod events;
pub mod feeds;
pub mod http;
pub mod idempotency;
pub mod llm;
pub mod quick;
pub mod scheduler;
pub mod service;
pub mod transfers;
pub mod websearch;

pub use command::{Command, Signature};
pub use ctx::{Caller, Ctx, Scope};
pub use error::{CommandError, CommandResult};
pub use events::{Change, EventSink, Kind, Level, Notification, Op, Reach, Silent};
pub use service::{PROTOCOL, Service, blocking};
