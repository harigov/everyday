//! Unit tests for the sync engine, against a fake [`MailSession`] and a
//! real, throwaway vault -- SQLite, the real pack store, the real search
//! index, everything but the socket.

mod arrivals;
mod categorisation;
mod compaction;
mod fixtures;
mod incremental;
mod notifications;
mod sync;
mod unsubscribe_header;
mod wiring;
