//! The open vault, and everything a caller can do to it.
//!
//! This is what the desktop shell's `AppState` was, minus the window. It holds
//! the vault, the sink its remarks go to, the confirmations the assistant is
//! waiting on, and the record of which requests have already been answered.
//!
//! Three entry points, because three kinds of thing cross the boundary and only
//! one of them is JSON: [`Service::call`] for every ordinary command in the
//! table -- which is most of the surface, and is why the number is not written
//! here; [`Service::put_blob`], [`Service::blob_len`] and
//! [`Service::blob_range`] for bytes; and [`Service::send_message`] for a turn
//! of the assistant, which answers with a stream rather than a value.
//!
//! # What is *not* here
//!
//! Opening a vault, creating one, and choosing which one this session is
//! about. Those decide what the service *is*, and they belong to whatever owns
//! the process -- the shell that has a file picker, or the `serve` command that
//! was given a path. A service is handed a vault; it does not go looking.

use crate::agent::Pending;
use crate::command;
use crate::ctx::Ctx;
use crate::error::{CommandError, CommandResult};
use crate::events::{EventSink, Silent};
use crate::idempotency::{Claim, Idempotency};
use everyday_core::{BlobId, CalendarId, Vault};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// The version of the command surface this build speaks.
///
/// A client and a server are the same binary, so skew is a matter of time
/// rather than of configuration. A mismatch is refused with both numbers
/// named, the way a newer database schema is refused rather than written into.
///
/// Bump it when a command is removed, an argument is removed or narrowed, or a
/// result changes shape. Adding a command, or an optional argument, does not:
/// an older client simply does not call it.
pub const PROTOCOL: u32 = 1;

/// Holds the open vault, if any.
///
/// `None` means no vault has been opened this session. A vault that is open but
/// *locked* is still `Some`: the vault's own lock state governs access, and
/// keeping the handle lets a lock screen name the vault it is guarding.
pub struct Service {
    vault: RwLock<Option<Arc<Vault>>>,
    last_path: RwLock<Option<PathBuf>>,
    events: RwLock<Arc<dyn EventSink>>,
    pending: Arc<Pending>,
    idempotency: Idempotency,
    /// Subscriptions whose background refresh is failing and which the user
    /// has already been told about.
    ///
    /// The background pass runs every few minutes for as long as the app is
    /// open, so a feed that has been revoked fails again, and again, and again.
    /// Notifying each time would turn one fact -- this calendar has stopped
    /// answering -- into a notification every five minutes until somebody muted
    /// the application. Held here rather than on the subscription because it is
    /// a fact about *this session's* telling, not about the calendar: the vault
    /// already records the failure itself, and reopening the app is a
    /// reasonable moment to be told again.
    reported_feeds: RwLock<HashSet<CalendarId>>,
    /// Routines whose failure has already been reported this session. See
    /// [`Service::routine_failed`].
    reported_routines: RwLock<HashSet<String>>,
    /// Runs this process is carrying out, by id.
    ///
    /// A row in the vault reading `Running` says one of two things and cannot
    /// tell them apart on its own: a run happening now, or one a process that
    /// died left behind. This says which, because it exists only in memory --
    /// a run this process is not carrying out is not in here, whatever the
    /// row says.
    claimed_runs: RwLock<HashSet<String>>,
    /// The routine running right now, by name, if one is.
    ///
    /// Here rather than derived from a `Running` row, because a row is also
    /// what a run abandoned by a dead process looks like. This is in memory
    /// and therefore cannot lie about the present.
    running_routine: RwLock<Option<String>>,
}

impl Default for Service {
    fn default() -> Self {
        Self::new()
    }
}

impl Service {
    pub fn new() -> Self {
        Self {
            vault: RwLock::new(None),
            last_path: RwLock::new(None),
            events: RwLock::new(Arc::new(Silent)),
            pending: Arc::default(),
            idempotency: Idempotency::default(),
            reported_feeds: RwLock::new(HashSet::new()),
            reported_routines: RwLock::new(HashSet::new()),
            claimed_runs: RwLock::new(HashSet::new()),
            running_routine: RwLock::new(None),
        }
    }

    /// Send what this service has to say somewhere.
    ///
    /// Settable after construction rather than taken in `new`, because the
    /// shell's sink needs an `AppHandle` that does not exist until Tauri has
    /// built the app, and the service has to exist before that to be handed to
    /// it. A service with no sink drops its remarks, which is the right
    /// behaviour for the CLI.
    pub fn set_events(&self, sink: Arc<dyn EventSink>) {
        *self.events.write().unwrap() = sink;
    }

    pub fn events(&self) -> Arc<dyn EventSink> {
        self.events.read().unwrap().clone()
    }

    pub fn pending(&self) -> Arc<Pending> {
        self.pending.clone()
    }

    // ---- the vault ------------------------------------------------------

    pub fn set(&self, vault: Vault) -> Arc<Vault> {
        self.remember(vault.path());
        let vault = Arc::new(vault);
        *self.vault.write().unwrap() = Some(vault.clone());
        vault
    }

    /// Close the open vault, releasing its write lock.
    ///
    /// Must happen *before* another vault is opened, and matters even when the
    /// other vault is the same one. The lock is an OS lock on an open file
    /// description, so a second `open` of a path this process already holds
    /// conflicts with itself: without this, choosing the currently-open vault
    /// from the picker would quietly reopen it read-only.
    ///
    /// Only this handle is dropped. A command already running still holds its
    /// own `Arc`, and the lock goes when that finishes -- which is why the open
    /// that follows must tolerate losing the race and coming up read-only
    /// rather than failing.
    pub fn close(&self) {
        self.reported_feeds.write().unwrap().clear();
        self.reported_routines.write().unwrap().clear();
        self.claimed_runs.write().unwrap().clear();
        self.running_routine.write().unwrap().take();
        let previous = self.vault.write().unwrap().take();
        if let Some(vault) = &previous {
            // Drop the key and the decrypted index now rather than whenever the
            // last `Arc` happens to go.
            vault.lock();
        }
        drop(previous);
    }

    pub fn get(&self) -> Option<Arc<Vault>> {
        self.vault.read().unwrap().clone()
    }

    /// The open vault, or a `no_vault` error a caller can route on.
    pub fn require(&self) -> CommandResult<Arc<Vault>> {
        self.get().ok_or_else(|| CommandError::new("no_vault", "no vault is open"))
    }

    /// The open vault, refusing if it is locked.
    ///
    /// For the commands that put a request on the network. A locked vault is
    /// not a technical obstacle to a web search -- nothing about it touches
    /// storage -- but the lock screen must not be a place from which requests
    /// leave the machine.
    pub fn require_unlocked(&self) -> CommandResult<Arc<Vault>> {
        let vault = self.require()?;
        if !vault.is_unlocked() {
            return Err(CommandError::from(everyday_core::Error::Locked));
        }
        Ok(vault)
    }

    /// The vault to open on startup: the one this session already touched, or
    /// the one the previous session left behind.
    pub fn last_path(&self) -> Option<PathBuf> {
        let in_memory = self.last_path.read().unwrap().clone();
        in_memory.or_else(everyday_vault::last_vault)
    }

    /// Record `path` as the vault to reopen, in memory and on disk.
    ///
    /// Failing to write the pointer is not worth failing the open that prompted
    /// it: the vault is fine, the next launch just starts at the default
    /// location.
    pub fn remember(&self, path: &Path) {
        *self.last_path.write().unwrap() = Some(path.to_path_buf());
        if let Err(e) = everyday_vault::remember_vault(path) {
            tracing::warn!(error = %e, "could not record the last vault path");
        }
    }

    // ---- feeds ----------------------------------------------------------

    /// Note that `id`'s refresh has failed. True the first time, so the caller
    /// says something once per outage rather than once per attempt.
    pub fn feed_failed(&self, id: CalendarId) -> bool {
        self.reported_feeds.write().unwrap().insert(id)
    }

    /// Note that `id`'s refresh worked, so the next outage is news again.
    pub fn feed_recovered(&self, id: CalendarId) {
        self.reported_feeds.write().unwrap().remove(&id);
    }

    // ---- routines --------------------------------------------------------

    /// Note that a routine's run failed. True the first time, so a routine
    /// whose endpoint has been unreachable every morning for a week says so
    /// once rather than seven times. The same courtesy `feed_failed` extends
    /// to a calendar that has stopped answering, and the same session scope:
    /// cleared when the vault closes, so the next outage is news again.
    pub fn routine_failed(&self, id: String) -> bool {
        self.reported_routines.write().unwrap().insert(id)
    }

    /// Note that a routine ran, so its next failure is news.
    pub fn routine_recovered(&self, id: String) {
        self.reported_routines.write().unwrap().remove(&id);
    }

    /// Take this run for the life of the returned guard.
    ///
    /// Released on drop, including on a panic or an early return, which is
    /// what makes "this process is carrying it out" a fact rather than a flag
    /// somebody has to remember to clear.
    pub fn claim_run(self: &Arc<Self>, id: String) -> RunClaim {
        self.claimed_runs.write().unwrap().insert(id.clone());
        RunClaim { service: self.clone(), id }
    }

    /// Is this process carrying out that run?
    pub fn claims_run(&self, id: &str) -> bool {
        self.claimed_runs.read().unwrap().contains(id)
    }

    /// Say which routine is running, or `None` when none is.
    ///
    /// Read by the tray, so that a process staying resident to keep its
    /// appointments can say what it is doing rather than merely being there.
    pub fn set_running_routine(&self, name: Option<String>) {
        *self.running_routine.write().unwrap() = name;
    }

    pub fn running_routine(&self) -> Option<String> {
        self.running_routine.read().unwrap().clone()
    }

    // ---- dispatch -------------------------------------------------------

    /// Run a command by name.
    ///
    /// The scope check, the idempotency record and the change event all happen
    /// here rather than in any command body, which is what stops the eighty-first
    /// command forgetting one of them.
    pub async fn call(self: &Arc<Self>, ctx: Ctx, name: &str, args: Value) -> CommandResult<Value> {
        let Some(command) = command::find(name) else {
            return Err(CommandError::new(
                "unknown_command",
                format!("no command called {name:?}"),
            ));
        };
        if command.streams {
            return Err(CommandError::new(
                "unknown_command",
                format!("{name} answers with a stream and cannot be called for a value"),
            ));
        }

        // Only writes are worth recording. A repeated read is cheap and
        // answering it from a cache would mean serving a stale list to whoever
        // retried, which is the opposite of what they asked for.
        let key = match (&ctx.request_id, command.effect.is_write()) {
            (Some(id), true) => {
                Some(format!("{}:{id}", ctx.caller.origin().unwrap_or("anonymous")))
            }
            _ => None,
        };

        let Some(key) = key else {
            return command.invoke(self.clone(), ctx, args).await;
        };

        let slot = match self.idempotency.claim(&key) {
            Claim::Mine(slot) => slot,
            Claim::Theirs(slot) => return crate::idempotency::wait(slot).await,
        };
        // The guard is what makes a *cancelled* request safe.
        //
        // A future that is dropped mid-await -- which is what a client hanging
        // up looks like, and axum drops the handler -- would otherwise leave
        // the slot claimed and unanswered for ever, and the retry that
        // followed would park in `wait` and never come back. The guard
        // abandons it on the way out unless it was fulfilled, so the retry is
        // told to ask again rather than left waiting.
        let guard = self.idempotency.guard(&key, slot);
        let answer = command.invoke(self.clone(), ctx, args).await;
        guard.fulfil(answer.clone());
        answer
    }

    /// Say something to the assistant, streaming what it says back.
    ///
    /// The one command that does not answer with a value. A turn takes seconds
    /// and runs tools while it does, and a caller that could draw none of that
    /// until the end would read as a hang -- so events go to `sink` as they
    /// happen and this resolves when the turn is over.
    ///
    /// Separate from [`Service::call`] rather than a variant of it because the
    /// two have genuinely different shapes, and a `call` that could sometimes
    /// answer with a stream would oblige every caller to handle a case almost
    /// none of them can reach.
    pub async fn send_message(
        self: &Arc<Self>,
        ctx: Ctx,
        args: Value,
        sink: crate::agent::Sink,
    ) -> CommandResult<()> {
        ctx.require(crate::ctx::Scope::Agent)?;
        let args: crate::domains::assistant::SendMessage =
            crate::command::parse("send_message", args)?;
        let vault = self.require()?;
        let turned = crate::agent::run_turn(crate::agent::Turn {
            vault,
            pending: self.pending(),
            conversation: args.conversation_id,
            prompt: args.prompt,
            context: args.context,
            channel: sink,
            // Somebody is sitting in front of this one, so a destructive call
            // stops and asks them.
            unattended: None,
        })
        .await?;
        let origin = ctx.caller.origin().map(str::to_string);
        // What its tools wrote, before the thread itself. The assistant reaches
        // the vault directly rather than back through here, so these are writes
        // nothing else on this path can see -- and the list a person asked it
        // to add a task to is open in front of them.
        for kind in turned.wrote {
            self.events().changed(crate::events::Change {
                kind,
                op: crate::events::Op::Updated,
                id: None,
                origin: origin.clone(),
            });
        }
        self.events().changed(crate::events::Change {
            kind: crate::events::Kind::Conversation,
            op: crate::events::Op::Updated,
            id: Some(args.conversation_id.to_string()),
            origin,
        });
        Ok(())
    }

    // ---- bytes ----------------------------------------------------------

    /// Largest file accepted as an attachment.
    ///
    /// Not a storage limit -- the blob store chunks and streams happily past
    /// this -- but a bound on the single allocation this makes, since the
    /// payload arrives as one buffer.
    pub const MAX_ATTACHMENT_BYTES: usize = 512 * 1024 * 1024;

    pub async fn put_blob(&self, ctx: &Ctx, bytes: Vec<u8>) -> CommandResult<String> {
        ctx.require(crate::ctx::Scope::Journals)?;
        if bytes.len() > Self::MAX_ATTACHMENT_BYTES {
            return Err(CommandError::new(
                "too_large",
                format!(
                    "attachment is {} MB; the limit is {} MB",
                    bytes.len() / 1_048_576,
                    Self::MAX_ATTACHMENT_BYTES / 1_048_576
                ),
            ));
        }
        let vault = self.require()?;
        blocking(move || Ok(vault.put_blob(&bytes)?.to_hex())).await
    }

    /// How long an attachment is, for a range request's arithmetic.
    pub fn blob_len(&self, id: BlobId) -> CommandResult<u64> {
        let vault = self.require()?;
        Ok(vault.with_store(|s| s.blob_len(id))?)
    }

    /// Part of an attachment. Decrypts the chunks the range touches and no
    /// others, which is what lets a video seek.
    pub fn blob_range(&self, id: BlobId, offset: u64, len: u64) -> CommandResult<Vec<u8>> {
        let vault = self.require()?;
        Ok(vault.with_store(|s| s.get_blob_range(id, offset, len))?)
    }
}

/// Run blocking vault work off the async runtime.
///
/// Every command that touches storage goes through here. Vault operations
/// touch disk and, when unlocking, deliberately burn ~64 MiB of memory in
/// Argon2; doing that on the runtime's async threads would stall every other
/// task, and in the desktop shell doing it inline would freeze the window
/// mid-keystroke.
///
/// What stays off it is only what never touches storage -- reading a status
/// word, stamping activity against an atomic, or minting a record in memory
/// for a caller to fill in -- where a hop to another thread and back would cost
/// more than the work.
pub async fn blocking<T, F>(f: F) -> CommandResult<T>
where
    F: FnOnce() -> CommandResult<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| CommandError::new("panic", format!("background task failed: {e}")))?
}

/// A run this process has taken, released when it is dropped.
///
/// See [`Service::claim_run`]. The whole value of it is the `Drop`: a claim
/// that had to be released by hand would be a claim somebody eventually
/// forgets to release on the error path, and the symptom would be a run stuck
/// on the app bar until the application was restarted.
pub struct RunClaim {
    service: Arc<Service>,
    id: String,
}

impl Drop for RunClaim {
    fn drop(&mut self) {
        self.service.claimed_runs.write().unwrap().remove(&self.id);
    }
}
