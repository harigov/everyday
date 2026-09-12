//! Archives in flight, on their way out of a vault or into one.
//!
//! An export is built whole before any of it can be saved, and an import
//! arrives whole before any of it can be read. Both therefore need somewhere
//! to sit while a client moves them a chunk at a time, and this is it.
//!
//! # In memory, not in a file
//!
//! An archive is *plaintext*. Every other byte this application writes to disk
//! is sealed, and spilling an unencrypted copy of somebody's diary into
//! `/tmp` -- where it would outlive the session, the vault's lock and very
//! possibly the person's memory of having made it -- would undo the premise of
//! the whole program to save some memory. So it is held here, and:
//!
//! * it is dropped the moment the vault locks, along with the key;
//! * it is dropped when it has been read to the end;
//! * it is dropped after [`EXPIRY`] whatever happens, because a window that
//!   was closed mid-download cannot say so.
//!
//! The cost is that an export is bounded by memory. That bound is stated in
//! the interface rather than discovered: attachments are a switch, their size
//! is shown before the switch is thrown, and `everyday export` on the command
//! line writes straight to disk with no ceiling at all for the vault where
//! that is not enough.

use crate::error::{CommandError, CommandResult, codes};
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// How long an unfinished transfer is kept.
///
/// Long enough for somebody to be interrupted by a phone call while choosing
/// where to save it; short enough that a forgotten one is not resident all
/// afternoon.
const EXPIRY: Duration = Duration::from_secs(30 * 60);

/// The most an import may be. Past this the answer is the command line, which
/// streams rather than holding the whole of it.
pub const MAX_IMPORT: u64 = 2 * 1024 * 1024 * 1024;

/// How many transfers may be in flight at once.
///
/// A cap rather than a courtesy. A client says how big an upload will be and
/// this holds what arrives, so without one a paired device could open
/// transfers until the machine holding the vault ran out of memory -- and the
/// weakest client in this system is the one most likely to be somebody
/// else's. Four is more than a person can be in the middle of.
const MAX_IN_FLIGHT: usize = 4;

/// How much is allocated up front for an upload, however big it says it is.
///
/// The rest grows as it actually arrives. Trusting the stated size would let
/// one call reserve two gigabytes before a single byte had been sent.
const PREALLOCATE: usize = 8 * 1024 * 1024;

struct Held {
    bytes: Vec<u8>,
    /// What the whole thing will be, for an upload still arriving. Equal to
    /// `bytes.len()` for a finished export.
    expected: u64,
    started: Instant,
}

/// The transfers this session is in the middle of.
#[derive(Default)]
pub struct Transfers {
    held: RwLock<HashMap<String, Held>>,
    counter: RwLock<u64>,
}

impl Transfers {
    /// Take an archive in, answering with the handle it is known by.
    pub fn put(&self, bytes: Vec<u8>, expected: u64) -> CommandResult<String> {
        self.sweep();
        let handle = self.mint();
        let mut held = self.held.write().unwrap();
        if held.len() >= MAX_IN_FLIGHT {
            return Err(CommandError::new(
                codes::BUSY,
                "too many transfers are already in progress; finish or cancel one first",
            ));
        }
        held.insert(handle.clone(), Held { bytes, expected, started: Instant::now() });
        Ok(handle)
    }

    /// Somewhere for an upload of `expected` bytes to land.
    pub fn expect(&self, expected: u64) -> CommandResult<String> {
        self.put(Vec::with_capacity(PREALLOCATE.min(expected as usize)), expected)
    }

    /// How big the archive behind `handle` is.
    pub fn len(&self, handle: &str) -> CommandResult<u64> {
        self.with(handle, |held| Ok(held.bytes.len() as u64))
    }

    /// A slice of it. Reading past the end answers with what is there rather
    /// than an error, so a client that asks for one chunk too many is told it
    /// is done instead of being told it is wrong.
    pub fn read(&self, handle: &str, offset: u64, len: u64) -> CommandResult<Vec<u8>> {
        self.with(handle, |held| {
            let from = (offset as usize).min(held.bytes.len());
            let to = from.saturating_add(len as usize).min(held.bytes.len());
            Ok(held.bytes[from..to].to_vec())
        })
    }

    /// Append to an upload. `offset` must be where the last chunk ended: a
    /// client that retries a chunk it already sent is answered from what is
    /// held rather than corrupting it, and one that skips ahead is refused.
    pub fn write(&self, handle: &str, offset: u64, chunk: &[u8]) -> CommandResult<u64> {
        let mut held = self.held.write().unwrap();
        let entry = held.get_mut(handle).ok_or_else(gone)?;
        let at = entry.bytes.len() as u64;
        if offset == at {
            entry.bytes.extend_from_slice(chunk);
        } else if offset + chunk.len() as u64 > at {
            return Err(CommandError::new(
                codes::INVALID,
                format!("this file's parts arrived out of order: expected {at}, got {offset}"),
            ));
        }
        if entry.bytes.len() as u64 > entry.expected {
            return Err(CommandError::new(
                codes::INVALID,
                "more of this file arrived than was promised",
            ));
        }
        Ok(entry.bytes.len() as u64)
    }

    /// The whole of an upload, once all of it has arrived.
    pub fn complete(&self, handle: &str) -> CommandResult<Vec<u8>> {
        self.with(handle, |held| {
            if (held.bytes.len() as u64) < held.expected {
                return Err(CommandError::new(
                    codes::INVALID,
                    format!(
                        "only {} of {} bytes of this file have arrived",
                        held.bytes.len(),
                        held.expected
                    ),
                ));
            }
            Ok(held.bytes.clone())
        })
    }

    pub fn drop_one(&self, handle: &str) {
        self.held.write().unwrap().remove(handle);
    }

    /// Forget everything. Called when the vault locks: the key is gone, and a
    /// plaintext copy of what it was protecting must go with it.
    pub fn clear(&self) {
        self.held.write().unwrap().clear();
    }

    pub fn is_empty(&self) -> bool {
        self.held.read().unwrap().is_empty()
    }

    fn with<T>(&self, handle: &str, f: impl FnOnce(&Held) -> CommandResult<T>) -> CommandResult<T> {
        self.sweep();
        let held = self.held.read().unwrap();
        f(held.get(handle).ok_or_else(gone)?)
    }

    fn sweep(&self) {
        let mut held = self.held.write().unwrap();
        held.retain(|_, entry| entry.started.elapsed() < EXPIRY);
    }

    /// An unguessable handle.
    ///
    /// A counter would do for a window talking to its own process, and would
    /// not do for a paired device: two clients share one service, and a
    /// handle that can be guessed is a handle another client can read an
    /// archive out of. Hashed rather than random because the core already
    /// carries the hash and does not carry a random-number generator this
    /// crate can reach.
    fn mint(&self) -> String {
        let mut counter = self.counter.write().unwrap();
        *counter += 1;
        let seed = format!(
            "{}:{:?}:{:p}",
            *counter,
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default(),
            self
        );
        everyday_core::BlobId::of(seed.as_bytes()).to_hex()[..24].to_string()
    }
}

fn gone() -> CommandError {
    CommandError::new(
        codes::NOT_FOUND,
        "that transfer is no longer in progress -- it may have finished, been cancelled, \
         or been dropped when the vault locked",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_archive_is_read_back_in_pieces() {
        let transfers = Transfers::default();
        let handle = transfers.put(b"abcdefghij".to_vec(), 10).unwrap();
        assert_eq!(transfers.len(&handle).unwrap(), 10);
        assert_eq!(transfers.read(&handle, 0, 4).unwrap(), b"abcd");
        assert_eq!(transfers.read(&handle, 8, 4).unwrap(), b"ij");
        assert!(transfers.read(&handle, 20, 4).unwrap().is_empty());
    }

    #[test]
    fn an_upload_arrives_in_order_or_not_at_all() {
        let transfers = Transfers::default();
        let handle = transfers.expect(6).unwrap();
        assert_eq!(transfers.write(&handle, 0, b"abc").unwrap(), 3);
        // A retry of what already landed is not an error and does not double.
        assert_eq!(transfers.write(&handle, 0, b"abc").unwrap(), 3);
        assert!(transfers.write(&handle, 5, b"f").is_err(), "a gap was accepted");
        assert!(transfers.complete(&handle).is_err(), "an unfinished upload was handed over");
        assert_eq!(transfers.write(&handle, 3, b"def").unwrap(), 6);
        assert_eq!(transfers.complete(&handle).unwrap(), b"abcdef");
    }

    #[test]
    fn locking_forgets_everything() {
        let transfers = Transfers::default();
        let handle = transfers.put(b"secret".to_vec(), 6).unwrap();
        transfers.clear();
        assert!(transfers.is_empty());
        assert_eq!(transfers.len(&handle).unwrap_err().code, "not_found");
    }

    #[test]
    fn there_is_a_ceiling_on_what_is_held_at_once() {
        let transfers = Transfers::default();
        for _ in 0..MAX_IN_FLIGHT {
            transfers.put(Vec::new(), 0).unwrap();
        }
        assert_eq!(transfers.put(Vec::new(), 0).unwrap_err().code, "busy");
    }

    #[test]
    fn giving_one_up_frees_the_slot_it_held() {
        // The ceiling only works if the client releases what it abandons. A
        // pane that picked four wrong files and dropped each handle wedged
        // here for half an hour, which is what `endImport` on the way out of
        // the interface is for.
        let transfers = Transfers::default();
        let mut handles = Vec::new();
        for _ in 0..MAX_IN_FLIGHT {
            handles.push(transfers.put(Vec::new(), 0).unwrap());
        }
        assert!(transfers.put(Vec::new(), 0).is_err());
        transfers.drop_one(&handles[0]);
        transfers.put(Vec::new(), 0).expect("a released slot was not reusable");
    }

    #[test]
    fn an_upload_does_not_reserve_what_it_merely_claims() {
        let transfers = Transfers::default();
        let handle = transfers.expect(MAX_IMPORT).unwrap();
        assert_eq!(transfers.len(&handle).unwrap(), 0);
    }

    #[test]
    fn two_handles_are_never_the_same() {
        let transfers = Transfers::default();
        let a = transfers.put(Vec::new(), 0).unwrap();
        let b = transfers.put(Vec::new(), 0).unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 24);
    }
}
