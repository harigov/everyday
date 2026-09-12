//! Range parsing, response sizing and type sniffing for served attachments.
//!
//! Moved to [`everyday_core::media`] so `everyday-transfer` could sniff a
//! blob's type without depending on a storage backend to do it. The
//! re-export stays so nothing above this crate -- the Tauri protocol
//! handler, the HTTP server -- had to learn a new path for something that
//! did not otherwise change.
pub use everyday_core::media::*;
