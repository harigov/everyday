//! One writer, several readers.
//!
//! Until several clients could reach a vault at once, this crate held exactly
//! one connection behind one mutex, and that was the right shape: a single
//! window asks one thing at a time. It stops being the right shape the moment
//! a server is answering three machines, because every read then queues
//! behind every other read, and one slow query -- a tag count across the
//! vault, a Postgres round trip over a wet piece of string -- stalls the lot.
//!
//! So reads and writes are separated. Writes still take a single connection
//! under a single mutex, which is what keeps "one write lands at a time" true
//! and is what the vault's own write lock already promised. Reads take a
//! connection from a pool, and several of them can be in flight at once.
//! SQLite in WAL mode allows exactly that -- many readers beside one writer,
//! with no reader ever blocking the writer -- and Postgres wants a pool
//! anyway.
//!
//! # The pool is lazy
//!
//! A local vault opened by one window never needs a second connection, and
//! for Postgres a connection is a TCP session and a TLS handshake. So readers
//! are made when a read finds none idle, up to a cap, and never at unlock.
//! The single-window case therefore pays for exactly one reader, created the
//! first time anything reads.
//!
//! # What happens when a reader cannot be made
//!
//! Fall back to the writer, which is always there. A driver may not be able
//! to open a second connection at all (it says so by answering `None`), and a
//! database that is briefly unreachable will fail to open one. Neither is
//! worth failing a read that the writer could have served. A failure is not
//! retried for a minute, so a server that has gone away does not cost every
//! subsequent read a connection timeout.
//!
//! # The one rule for callers
//!
//! Do not hold a guard while asking for another. The mutexes are not
//! reentrant and the pool has a finite cap, so a read taken while a write is
//! held -- or a second read taken while the first is held, on a saturated
//! pool -- is a deadlock. Every domain module here takes one guard, uses it,
//! and drops it, which is also why the queries in this crate materialise
//! their rows rather than streaming them.

use crate::conn::{Connection, Row, Sql, Value};
use crate::{Driver, lock};
use everyday_core::error::Result;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long a failure to open a reader suppresses the next attempt.
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// The connections a store has, and the rules for handing them out.
pub(crate) struct Pool {
    driver: Arc<dyn Driver>,
    writer: Mutex<Box<dyn Connection>>,
    readers: Mutex<Readers>,
    /// Signalled when a reader goes back, so a waiter can take it.
    returned: Condvar,
}

struct Readers {
    idle: Vec<Box<dyn Connection>>,
    /// Readers in existence, idle or checked out.
    live: usize,
    /// Most readers this store may open. Zero means reads share the writer.
    cap: usize,
    /// When it is worth trying to open a reader again, after one failed.
    retry_at: Option<Instant>,
}

impl Pool {
    pub(crate) fn new(driver: Arc<dyn Driver>, writer: Box<dyn Connection>) -> Self {
        let cap = driver.read_pool_size();
        Self {
            driver,
            writer: Mutex::new(writer),
            readers: Mutex::new(Readers { idle: Vec::new(), live: 0, cap, retry_at: None }),
            returned: Condvar::new(),
        }
    }

    /// The write connection. Blocks until whoever else is writing is done.
    pub(crate) fn write(&self) -> MutexGuard<'_, Box<dyn Connection>> {
        lock(&self.writer)
    }

    /// A connection to read on: a pooled reader, or the writer if there is
    /// none to be had.
    pub(crate) fn read(&self) -> ReadGuard<'_> {
        let mut readers = lock(&self.readers);
        loop {
            if let Some(conn) = readers.idle.pop() {
                return ReadGuard::Pooled { pool: self, conn: Some(conn) };
            }

            let may_open = readers.live < readers.cap
                && readers.retry_at.is_none_or(|at| Instant::now() >= at);
            if may_open {
                readers.live += 1;
                drop(readers);
                match self.driver.connect() {
                    Ok(Some(conn)) => return ReadGuard::Pooled { pool: self, conn: Some(conn) },
                    // The driver cannot make readers at all. Stop asking.
                    Ok(None) => {
                        let mut readers = lock(&self.readers);
                        readers.live -= 1;
                        readers.cap = 0;
                        drop(readers);
                        return ReadGuard::Writer(self.write());
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "could not open a reader; reads will share the write connection"
                        );
                        let mut readers = lock(&self.readers);
                        readers.live -= 1;
                        readers.retry_at = Some(Instant::now() + RETRY_AFTER);
                        drop(readers);
                        return ReadGuard::Writer(self.write());
                    }
                }
            }

            // No idle reader, and none may be opened. Either the pool is off
            // -- share the writer -- or it is saturated, and waiting for one
            // to come back beats serialising behind the writer.
            if readers.live == 0 {
                drop(readers);
                return ReadGuard::Writer(self.write());
            }
            readers = self.returned.wait(readers).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn give_back(&self, conn: Box<dyn Connection>) {
        lock(&self.readers).idle.push(conn);
        self.returned.notify_one();
    }
}

/// A connection borrowed for one read.
pub(crate) enum ReadGuard<'a> {
    Pooled {
        pool: &'a Pool,
        /// Always `Some` until the guard is dropped.
        conn: Option<Box<dyn Connection>>,
    },
    Writer(MutexGuard<'a, Box<dyn Connection>>),
}

impl Sql for ReadGuard<'_> {
    /// Refused, always.
    ///
    /// A read guard is not merely a different connection; it is one this
    /// store makes no promises about ordering on. Writing through it would
    /// escape the single-writer rule silently and race the vault's own
    /// conditional saves, which is a corruption bug that would show up as a
    /// lost edit weeks later. Better to be a loud failure the conformance
    /// suite catches the first time a query is filed under the wrong verb.
    fn execute(&mut self, sql: &str, args: &[Value]) -> Result<u64> {
        let _ = args;
        Err(everyday_core::error::Error::Invalid(format!(
            "a read connection was asked to run a statement that changes something: {sql}"
        )))
    }

    fn query(&mut self, sql: &str, args: &[Value]) -> Result<Vec<Row>> {
        self.sql().query(sql, args)
    }
}

impl ReadGuard<'_> {
    fn sql(&mut self) -> &mut dyn Sql {
        match self {
            // The `Option` is only ever taken by `drop`.
            ReadGuard::Pooled { conn, .. } => conn.as_mut().expect("reader is in use").as_mut(),
            ReadGuard::Writer(guard) => guard.as_mut(),
        }
    }
}

impl Drop for ReadGuard<'_> {
    fn drop(&mut self) {
        if let ReadGuard::Pooled { pool, conn } = self
            && let Some(conn) = conn.take()
        {
            pool.give_back(conn);
        }
    }
}
