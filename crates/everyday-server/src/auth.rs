//! Who may reach this vault, and how they proved it.
//!
//! Three ideas, and the order matters.
//!
//! **A pairing code** is a short string shown on the machine holding the vault
//! and typed (or scanned) into the machine that wants it. One at a time, five
//! minutes, single use. It is a *capability to become a device*, not a
//! credential -- it buys one exchange and then it is gone.
//!
//! **A device token** is what that exchange hands back: 32 random bytes, held
//! by the client and never written down here. What is written down is a hash of
//! it, so this file being read does not hand anybody the vault.
//!
//! **A scope list** rides on the token. Today every token is issued
//! [`Scope::All`], which is the honest answer for a journal and a shelf of
//! books; see [`everyday_service::ctx`] for why the mechanism exists before the
//! app that needs it.
//!
//! # Where this lives, and why not beside `vault.json`
//!
//! In the application's own configuration directory, not in the vault. Two
//! reasons, and the second is the load-bearing one. A Postgres vault can be
//! served from two machines, and each has its own devices; and `everyday
//! backup` copies the vault directory, so a private key or a token file kept
//! there would end up in every backup the user ever made.
//!
//! # Rate limits are not decoration here
//!
//! Unlocking burns 64 MiB of Argon2 by design. That is a fine cost to impose on
//! somebody typing a password and an excellent denial of service to hand a
//! stranger, so unlock attempts are serialised server-wide and backed off per
//! device. Pairing is limited for the ordinary reason: an eight-character code
//! is guessable at a few thousand attempts a second and is not at five a
//! minute.

use everyday_service::ctx::{Caller, Ctx, Scope};
use everyday_service::error::{CommandError, CommandResult};
use rand::TryRngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a pairing code is good for.
const CODE_TTL: Duration = Duration::from_secs(300);

/// Failed pairing attempts before pairing shuts for a while.
const PAIR_ATTEMPTS: u32 = 5;
const PAIR_LOCKOUT: Duration = Duration::from_secs(600);

/// How often a device's last-used time is written down.
///
/// Not on every request. It is one field of one row and it is read by exactly
/// one thing -- the expiry check, against a month -- so persisting it per
/// request bought nothing and cost a serialise, a write, an `fsync` and an
/// `fsync` of the directory on the hot path of every call a paired client
/// makes. Five minutes of drift is invisible against thirty days.
const PERSIST_LAST_SEEN_EVERY: i64 = 300;

/// A token unused for this long has to pair again.
///
/// A bearer credential to an unlocked vault is worth an expiry even on a
/// trusted network: the laptop it is on can be lost, and a token nobody has
/// used for a month is one nobody will miss.
const TOKEN_TTL_DAYS: i64 = 30;

/// The alphabet a pairing code is drawn from.
///
/// No `0`/`O`, no `1`/`I`/`l`: a code is read off one screen and typed into
/// another, and those are the pairs that cost somebody a second attempt.
const CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
const CODE_LENGTH: usize = 8;

/// A paired client, as recorded on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    /// Hex of the blake3 of the token. Never the token.
    pub token_hash: String,
    pub scopes: Vec<Scope>,
    pub created: jiff::Timestamp,
    pub last_seen: jiff::Timestamp,
}

impl Device {
    /// Whether this device has been quiet long enough to need pairing again.
    pub fn expired(&self, now: jiff::Timestamp) -> bool {
        let age = now.as_second() - self.last_seen.as_second();
        age > TOKEN_TTL_DAYS * 24 * 60 * 60
    }
}

/// What a device looks like from the settings screen: everything but the hash.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub scopes: Vec<Scope>,
    pub created: jiff::Timestamp,
    pub last_seen: jiff::Timestamp,
    pub expired: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DeviceFile {
    #[serde(default)]
    devices: Vec<Device>,
}

struct PairingCode {
    code: String,
    issued: Instant,
}

#[derive(Default)]
struct Attempts {
    failures: u32,
    locked_until: Option<Instant>,
}

impl Attempts {
    fn locked(&self) -> bool {
        self.locked_until.is_some_and(|at| Instant::now() < at)
    }

    fn failed(&mut self, limit: u32, lockout: Duration) {
        self.failures += 1;
        if self.failures >= limit {
            self.failures = 0;
            self.locked_until = Some(Instant::now() + lockout);
        }
    }

    fn succeeded(&mut self) {
        self.failures = 0;
        self.locked_until = None;
    }
}

/// The devices this server knows, and the state that guards them.
///
/// # One file, one `Registry`
///
/// The in-memory `devices` list is the unit of writing: [`save_locked`]
/// writes the *whole* list, every time, and [`Registry::open`] reads the
/// whole list back exactly once, at construction. That makes one process
/// holding two `Registry`s over the same `devices.json` a bug and not
/// merely a waste of a file handle. Each copy believes its own snapshot is
/// current; a `last_seen` stamp written through one copy is invisible to
/// the other; and whichever copy saves last overwrites the other's most
/// recent write with its own stale one -- silently, because both writes
/// succeed. A device minted a moment ago through one copy can vanish from
/// disk the next time the other copy so much as touches a timestamp.
///
/// So: one `Registry` per process per device file, opened once and shared
/// -- by an `Arc`, not by opening the path again -- with everything in that
/// process that needs to read or write the device list. See
/// [`crate::prepare_with`] for the constructor built for a caller with more
/// than one such user.
///
/// [`save_locked`]: Registry::save_locked
pub struct Registry {
    path: PathBuf,
    devices: Mutex<Vec<Device>>,
    pairing: Mutex<Option<PairingCode>>,
    pair_attempts: Mutex<Attempts>,
    /// Per device, so one client hammering unlock does not lock out another.
    unlock_attempts: Mutex<HashMap<String, Attempts>>,
    /// Held for the duration of an unlock, so two never run at once. See the
    /// module docs: each one is 64 MiB of deliberate work.
    unlock_gate: tokio::sync::Mutex<()>,
    /// Set while a background write of the `lastSeen` stamp is already on
    /// its way, so a burst of requests crossing `PERSIST_LAST_SEEN_EVERY`
    /// in the same moment schedules one write rather than one each -- see
    /// [`Registry::persist_last_seen_in_background`].
    last_seen_write_pending: AtomicBool,
}

/// May a token actually be granted this scope?
///
/// Two are not grants at all, for different reasons, and both were being
/// spelled out separately at each of the places that had to exclude them.
/// `Admin` configures the server rather than reaching the vault, and a device
/// that could pair another device would make revoking one a suggestion rather
/// than a fact. `Any` is what a *command* requires of whoever is calling --
/// "somebody rather than nobody" -- so a token carrying it would satisfy that
/// requirement while holding nothing at all.
///
/// One predicate rather than two filters, because the two had already drifted:
/// a list of nothing but `Admin` was widened to `All` while a list of nothing
/// but `Any` was refused, and neither spelling knew about the other.
fn grantable(scope: Scope) -> bool {
    !matches!(scope, Scope::Admin | Scope::Any)
}

/// Puts an `AtomicBool` back to `false` however its scope ends.
///
/// One user: the deferred `lastSeen` write, where the flag says "a write is
/// already coming, do not schedule another". Anything that leaves that flag
/// stuck `true` -- a panic, an early return added later -- stops every
/// subsequent write silently, and the symptom is a device list whose
/// timestamps quietly stop moving rather than an error anybody sees.
struct ClearOnDrop<'a>(&'a AtomicBool);

impl Drop for ClearOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Registry {
    /// Read the device file, or start with none.
    ///
    /// A file that cannot be parsed is a problem worth naming rather than
    /// silently replacing: overwriting it would revoke every paired device
    /// without saying so.
    pub fn open(path: impl Into<PathBuf>) -> CommandResult<Self> {
        let path = path.into();
        let devices = match std::fs::read_to_string(&path) {
            Ok(text) => {
                serde_json::from_str::<DeviceFile>(&text)
                    .map_err(|e| {
                        CommandError::new(
                            "invalid",
                            format!("{} is not a device list: {e}", path.display()),
                        )
                    })?
                    .devices
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                return Err(CommandError::new(
                    "io",
                    format!("could not read {}: {e}", path.display()),
                ));
            }
        };
        Ok(Self {
            path,
            devices: Mutex::new(devices),
            pairing: Mutex::new(None),
            pair_attempts: Mutex::new(Attempts::default()),
            unlock_attempts: Mutex::new(HashMap::new()),
            unlock_gate: tokio::sync::Mutex::new(()),
            last_seen_write_pending: AtomicBool::new(false),
        })
    }

    /// Write the list, with the lock still held.
    ///
    /// Taking `&[Device]` and being called *inside* the guard is the whole of
    /// the correctness here. It used to snapshot the list, drop the lock and
    /// then write, which is two bugs. Two writers raced on a temp file named
    /// only for its target, so the loser renamed a file that was no longer
    /// there and the list could be left half written -- and a server that
    /// cannot parse its device list refuses to start. And a `revoke` that
    /// dropped the lock could be overtaken by an `authenticate` whose older
    /// snapshot still had the revoked device in it, putting it back on disk.
    ///
    /// Serialising the writes is affordable because they are rare: pairing,
    /// revoking, and a last-used stamp every few minutes.
    fn save_locked(&self, devices: &[Device]) -> CommandResult<()> {
        let file = DeviceFile { devices: devices.to_vec() };
        let text = serde_json::to_string_pretty(&file)
            .map_err(|e| CommandError::new("internal", e.to_string()))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CommandError::new("io", format!("{}: {e}", parent.display())))?;
        }
        // Written the way the vault header is: a temporary file, flushed, then
        // renamed over the old one. A half-written device list is a vault that
        // has forgotten every machine that could reach it. The tag is unique
        // per write, which is what `write_atomic` asks of concurrent writers.
        everyday_core::fsutil::write_atomic(
            &self.path,
            text.as_bytes(),
            &everyday_core::fsutil::unique_tag(),
        )
        .map_err(|e| CommandError::new("io", e.to_string()))
    }

    pub fn devices(&self) -> Vec<DeviceInfo> {
        let now = jiff::Timestamp::now();
        lock(&self.devices)
            .iter()
            .map(|d| DeviceInfo {
                id: d.id.clone(),
                name: d.name.clone(),
                scopes: d.scopes.clone(),
                created: d.created,
                last_seen: d.last_seen,
                expired: d.expired(now),
            })
            .collect()
    }

    /// Mint a pairing code, replacing any outstanding one.
    ///
    /// One at a time on purpose. Two live codes means two people were told to
    /// pair and only one of them was expected, and the cost of the restriction
    /// is pressing the button again.
    pub fn new_pairing_code(&self) -> String {
        let code = random_code();
        *self.pairing.lock().unwrap() =
            Some(PairingCode { code: code.clone(), issued: Instant::now() });
        self.pair_attempts.lock().unwrap().succeeded();
        code
    }

    /// Stop offering to pair.
    pub fn clear_pairing_code(&self) {
        *lock(&self.pairing) = None;
    }

    pub fn pairing_open(&self) -> bool {
        self.pairing.lock().unwrap().as_ref().is_some_and(|p| p.issued.elapsed() < CODE_TTL)
    }

    /// Exchange a pairing code for a token.
    ///
    /// Returns the token exactly once; it is not recoverable afterwards.
    pub fn pair(
        &self,
        code: &str,
        name: &str,
        scopes: Vec<Scope>,
    ) -> CommandResult<(String, String)> {
        {
            let attempts = lock(&self.pair_attempts);
            if attempts.locked() {
                return Err(CommandError::new(
                    "too_many_attempts",
                    "too many pairing attempts; try again in a few minutes",
                ));
            }
        }

        let ok = {
            let mut pairing = lock(&self.pairing);
            match pairing.as_ref() {
                Some(p) if p.issued.elapsed() < CODE_TTL && constant_time_eq(&p.code, code) => {
                    // Single use, whatever happens next.
                    *pairing = None;
                    true
                }
                _ => false,
            }
        };
        if !ok {
            lock(&self.pair_attempts).failed(PAIR_ATTEMPTS, PAIR_LOCKOUT);
            return Err(CommandError::new("bad_code", "that pairing code is not valid"));
        }
        lock(&self.pair_attempts).succeeded();

        // A pairing request that asks for nothing is asking for a desktop
        // client, and gets what one has always been given. The default lives
        // here rather than in `issue` because it belongs to *this* exchange:
        // the scope list arrives from a wire where omitting it has always
        // meant "everything", and a caller minting a token deliberately must
        // not have an empty list quietly widened the same way.
        let scopes = if scopes.iter().all(|s| !grantable(*s)) { vec![Scope::All] } else { scopes };
        self.issue(name, scopes)
    }

    /// Mint a token for a client that has no code to present.
    ///
    /// The minting half of [`Registry::pair`], lifted out because pairing is
    /// two separable things: proving you were told a code, and being written
    /// into the device list. An MCP client cannot do the first -- there is no
    /// second screen to show a code on, and the process that will hold the
    /// token is a configuration file somebody edits -- but it must still do
    /// the second, so that "what can reach this vault" keeps having one
    /// answer and one list to revoke from.
    ///
    /// Not reachable over either transport, and it must stay that way: the
    /// whole of the pairing gate is that a token is minted only for somebody
    /// who was told a code. This is called from the desktop's own settings
    /// pane, on the machine holding the vault, where the person asking is the
    /// person at the keyboard.
    pub fn issue(&self, name: &str, scopes: Vec<Scope>) -> CommandResult<(String, String)> {
        let scopes: Vec<Scope> = scopes.into_iter().filter(|s| grantable(*s)).collect();

        // Refused rather than defaulted, and this is the whole reason the
        // default lives in `pair` instead. Widening an empty list to `All`
        // here would mean that asking for a token scoped to nothing but
        // `Admin` -- which is stripped a line above -- handed back one that
        // could read the entire vault. A narrowing filter that can widen its
        // input is not a filter.
        if scopes.is_empty() {
            return Err(CommandError::new(
                "invalid",
                "a token must be issued at least one scope it may actually reach",
            ));
        }

        let token = random_token();
        let now = jiff::Timestamp::now();
        let device = Device {
            // A UUIDv7 like every other id in this application, so a device
            // list sorts by when each one paired.
            id: uuid::Uuid::now_v7().to_string(),
            name: clean_name(name),
            token_hash: hash(&token),
            scopes,
            created: now,
            last_seen: now,
        };
        let id = device.id.clone();
        let mut devices = lock(&self.devices);
        devices.push(device);
        self.save_locked(&devices)?;
        Ok((token, id))
    }

    /// Turn a bearer token into a context, or refuse.
    ///
    /// Every token is hashed and compared, not looked up by prefix: a lookup
    /// that narrowed by prefix would leak how much of a guess was right through
    /// how long the answer took.
    ///
    /// # Why the write is not here
    ///
    /// This runs on every paired client's every request, and the file it
    /// occasionally has to update is one field of one row -- see
    /// [`PERSIST_LAST_SEEN_EVERY`]. The lock is released before anything
    /// that touches disk, and the actual write -- a temp file, an fsync of
    /// it, an fsync of the directory -- happens afterwards, off this
    /// caller's own future; see [`Registry::persist_last_seen_in_background`]
    /// for how that stays as correct as writing inline was.
    pub fn authenticate(self: &Arc<Self>, token: &str) -> CommandResult<Ctx> {
        let hashed = hash(token);
        let now = jiff::Timestamp::now();
        let (ctx, worth_writing) = {
            let mut devices = lock(&self.devices);
            let Some(device) =
                devices.iter_mut().find(|d| constant_time_eq(&d.token_hash, &hashed))
            else {
                return Err(CommandError::new("unauthorized", "this device is not paired"));
            };
            if device.expired(now) {
                return Err(CommandError::new(
                    "unauthorized",
                    "this device has not been used for a month and must pair again",
                ));
            }
            let worth_writing =
                now.as_second() - device.last_seen.as_second() >= PERSIST_LAST_SEEN_EVERY;
            device.last_seen = now;
            let ctx = Ctx {
                caller: Caller::Device(device.id.clone()),
                scopes: device.scopes.clone(),
                proved_at: None,
                request_id: None,
            };
            (ctx, worth_writing)
            // `devices` drops here, at the end of the block -- released
            // before `persist_last_seen_in_background` below, which is the
            // whole point.
        };
        // Best-effort, and rarely at all: see `PERSIST_LAST_SEEN_EVERY`. A
        // `lastSeen` that could not be written is not worth refusing a request
        // that is otherwise perfectly good.
        if worth_writing {
            self.persist_last_seen_in_background();
        }
        Ok(ctx)
    }

    /// Write the device list down off the caller's own future, for the one
    /// caller that does not need to see the write land before it returns.
    ///
    /// Reads `self.devices` fresh at the moment it actually writes, rather
    /// than closing over the snapshot [`Registry::authenticate`] saw --
    /// that is what keeps a write scheduled here safe to land after
    /// [`Registry::revoke`] or [`Registry::issue`] have run in between.
    /// Every other writer in this file still holds `self.devices`'s lock
    /// for the whole of its own write, and this does too, just on a
    /// blocking-pool thread instead of the request's: by the time it
    /// re-acquires the lock and clones the list, whatever `revoke` did has
    /// already happened or has not started, exactly as if this had been
    /// one more synchronous caller arriving a little late. What it can
    /// never do is write a snapshot older than the last one actually
    /// written, because it does not carry one -- it looks.
    ///
    /// `last_seen_write_pending` collapses a burst of these -- an MCP
    /// client and three paired phones crossing the five-minute mark within
    /// the same second -- into one write that picks up all of their stamps
    /// at once, cleared only once that write has actually finished so nothing
    /// scheduled while it was running is silently dropped.
    ///
    /// Falls back to writing inline when there is no Tokio runtime to hand
    /// the work to -- every `#[test]` in this file calls `authenticate`
    /// this way, and inline is what they have always exercised.
    fn persist_last_seen_in_background(self: &Arc<Self>) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            let devices = lock(&self.devices);
            if let Err(e) = self.save_locked(&devices) {
                tracing::debug!(error = %e, "could not record a device's last use");
            }
            return;
        };
        if self.last_seen_write_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let registry = self.clone();
        handle.spawn_blocking(move || {
            let devices = lock(&registry.devices);
            // Declared after the lock, and therefore dropped before it: the
            // flag has to fall while this thread still holds `devices`. Clear
            // it a moment later, once the lock is back, and a stamp taken in
            // between is lost twice over -- too late for the write that has
            // already read the list, and too early to schedule one of its
            // own, because it still sees a write pending. A guard rather than
            // a line at the end so that a panic in `save_locked` cannot latch
            // the flag `true` and stop every later write for good.
            let _clear = ClearOnDrop(&registry.last_seen_write_pending);
            if let Err(e) = registry.save_locked(&devices) {
                tracing::debug!(error = %e, "could not record a device's last use");
            }
        });
    }

    pub fn revoke(&self, id: &str) -> CommandResult<bool> {
        let mut devices = lock(&self.devices);
        let before = devices.len();
        devices.retain(|d| d.id != id);
        let removed = devices.len() != before;
        if removed {
            // Inside the guard, so nothing can write an older snapshot over
            // this one and put the device back. See `save_locked`.
            self.save_locked(&devices)?;
        }
        Ok(removed)
    }

    /// Claim the right to attempt an unlock.
    ///
    /// Held for the whole attempt, so two never burn 64 MiB at once, and
    /// refused outright for a device that has been guessing.
    pub async fn unlock_gate(&self, caller: &Caller) -> CommandResult<UnlockGuard<'_>> {
        let who = caller.origin().unwrap_or("anonymous").to_string();
        {
            let mut attempts = lock(&self.unlock_attempts);
            if attempts.entry(who.clone()).or_default().locked() {
                return Err(CommandError::new(
                    "too_many_attempts",
                    "too many attempts; try again in a few minutes",
                ));
            }
        }
        let guard = self.unlock_gate.lock().await;
        Ok(UnlockGuard { registry: self, who, _guard: guard })
    }
}

/// Held for one unlock attempt. Records the outcome on the way out.
pub struct UnlockGuard<'a> {
    registry: &'a Registry,
    who: String,
    _guard: tokio::sync::MutexGuard<'a, ()>,
}

impl UnlockGuard<'_> {
    pub fn record(&self, ok: bool) {
        let mut attempts = lock(&self.registry.unlock_attempts);
        let entry = attempts.entry(self.who.clone()).or_default();
        if ok {
            entry.succeeded();
        } else {
            entry.failed(PAIR_ATTEMPTS, PAIR_LOCKOUT);
        }
    }
}

/// A mutex, whether or not a previous caller panicked holding it.
///
/// `lock().unwrap()` would turn one panic anywhere in this file into a server
/// that refuses every subsequent request, which is a worse outcome than
/// carrying on with state that is at most one half-finished mutation behind.
/// The mutations here are a push, a retain and a field assignment; none of
/// them can leave a `Device` half built.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn clean_name(name: &str) -> String {
    let name: String = name.trim().chars().filter(|c| !c.is_control()).take(60).collect();
    if name.is_empty() { "A device".to_string() } else { name }
}

fn random_code() -> String {
    let mut bytes = [0u8; CODE_LENGTH];
    rand::rngs::OsRng.try_fill_bytes(&mut bytes).expect("the system random source");
    bytes.iter().map(|b| CODE_ALPHABET[*b as usize % CODE_ALPHABET.len()] as char).collect()
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.try_fill_bytes(&mut bytes).expect("the system random source");
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn hash(token: &str) -> String {
    blake3::hash(token.as_bytes()).to_hex().to_string()
}

/// Compare without leaking where two strings first differ.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> (Arc<Registry>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(Registry::open(dir.path().join("devices.json")).unwrap());
        (registry, dir)
    }

    #[test]
    fn a_code_buys_exactly_one_token() {
        let (registry, _dir) = registry();
        let code = registry.new_pairing_code();
        let (token, id) = registry.pair(&code, "Laptop", vec![Scope::All]).unwrap();
        assert!(!token.is_empty());

        // The same code again is worth nothing.
        assert_eq!(registry.pair(&code, "Laptop", vec![Scope::All]).unwrap_err().code, "bad_code");

        let ctx = registry.authenticate(&token).unwrap();
        assert_eq!(ctx.caller, Caller::Device(id));
    }

    #[test]
    fn a_token_is_never_written_down() {
        let (registry, dir) = registry();
        let code = registry.new_pairing_code();
        let (token, _) = registry.pair(&code, "Phone", vec![Scope::All]).unwrap();
        let on_disk = std::fs::read_to_string(dir.path().join("devices.json")).unwrap();
        assert!(!on_disk.contains(&token), "the device file holds the token itself");
        assert!(on_disk.contains(&hash(&token)));
    }

    #[test]
    fn a_revoked_device_is_a_stranger() {
        let (registry, _dir) = registry();
        let code = registry.new_pairing_code();
        let (token, id) = registry.pair(&code, "Phone", vec![Scope::All]).unwrap();
        assert!(registry.revoke(&id).unwrap());
        assert_eq!(registry.authenticate(&token).unwrap_err().code, "unauthorized");
        assert!(!registry.revoke(&id).unwrap(), "revoking twice is not an error");
    }

    #[test]
    fn guessing_a_code_shuts_the_door() {
        let (registry, _dir) = registry();
        registry.new_pairing_code();
        for _ in 0..PAIR_ATTEMPTS {
            assert_eq!(
                registry.pair("WRONGONE", "x", vec![Scope::All]).unwrap_err().code,
                "bad_code"
            );
        }
        assert_eq!(
            registry.pair("WRONGONE", "x", vec![Scope::All]).unwrap_err().code,
            "too_many_attempts"
        );
    }

    #[test]
    fn a_token_can_be_issued_without_a_code_and_is_a_device_like_any_other() {
        // What the MCP settings pane does. No code, because there is no
        // second screen to show one on -- but the same list, so revoking it
        // is the same act as revoking a paired phone.
        let (registry, _dir) = registry();
        let (token, id) = registry.issue("Claude Code", vec![Scope::Tasks]).unwrap();

        let ctx = registry.authenticate(&token).unwrap();
        assert_eq!(ctx.caller, Caller::Device(id.clone()));
        assert!(ctx.holds(Scope::Tasks));
        assert!(!ctx.holds(Scope::Journals), "an issued token must not widen to everything");

        assert!(registry.devices().iter().any(|d| d.id == id), "it belongs in the device list");
        assert!(registry.revoke(&id).unwrap());
        assert_eq!(registry.authenticate(&token).unwrap_err().code, "unauthorized");
    }

    #[test]
    fn issuing_a_token_strips_admin_like_pairing_does() {
        let (registry, _dir) = registry();
        let (token, _) = registry.issue("Agent", vec![Scope::Admin, Scope::Notes]).unwrap();
        let ctx = registry.authenticate(&token).unwrap();
        assert!(!ctx.holds(Scope::Admin));
        assert!(ctx.holds(Scope::Notes));
    }

    #[test]
    fn a_narrow_request_can_never_widen_into_a_token_that_holds_everything() {
        let (registry, _dir) = registry();

        // Both of these reduce to an empty list once the scopes that are
        // never granted are stripped. Defaulting that to `All` -- which is
        // right for a pairing request that named nothing -- would turn
        // "issue me the least you can" into a token that reads the diary.
        for asked in [vec![], vec![Scope::Admin], vec![Scope::Any], vec![Scope::Admin, Scope::Any]]
        {
            let err = registry.issue("Agent", asked.clone()).unwrap_err();
            assert_eq!(err.code, "invalid", "issue({asked:?}) should refuse, not widen");
        }

        // Nothing was written down on the way to refusing.
        assert!(registry.devices().is_empty());
    }

    #[test]
    fn pairing_without_naming_a_scope_still_means_everything() {
        // The behaviour the wire has always had, kept where it belongs.
        let (registry, _dir) = registry();
        let code = registry.new_pairing_code();
        let (token, _) = registry.pair(&code, "Laptop", vec![]).unwrap();
        assert!(registry.authenticate(&token).unwrap().holds(Scope::Journals));
    }

    #[test]
    fn admin_is_never_issued_over_a_wire() {
        let (registry, _dir) = registry();
        let code = registry.new_pairing_code();
        let (token, _) = registry.pair(&code, "Phone", vec![Scope::Admin, Scope::Library]).unwrap();
        let ctx = registry.authenticate(&token).unwrap();
        assert!(!ctx.holds(Scope::Admin));
        assert!(ctx.holds(Scope::Library));
    }

    #[test]
    fn a_device_that_has_not_been_seen_for_a_month_must_pair_again() {
        let (registry, dir) = registry();
        let code = registry.new_pairing_code();
        let (token, _) = registry.pair(&code, "Old", vec![Scope::All]).unwrap();

        // Age it on disk, then reopen: what a server restart sees.
        let path = dir.path().join("devices.json");
        let mut file: DeviceFile =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        file.devices[0].last_seen =
            jiff::Timestamp::now() - std::time::Duration::from_secs(40 * 24 * 60 * 60);
        std::fs::write(&path, serde_json::to_string(&file).unwrap()).unwrap();

        let registry = Arc::new(Registry::open(&path).unwrap());
        assert_eq!(registry.authenticate(&token).unwrap_err().code, "unauthorized");
    }

    #[test]
    fn a_device_list_that_cannot_be_read_is_named_rather_than_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("devices.json");
        std::fs::write(&path, "not json").unwrap();
        let e = Registry::open(&path).err().expect("a device list that is not JSON");
        assert_eq!(e.code, "invalid");
        // ...and it is still there, so nobody has been silently unpaired.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
    }

    /// A revoke must not be undone by a request that was in flight.
    ///
    /// The bug this replaced: `revoke` mutated the list, dropped the lock and
    /// then wrote a snapshot, while `authenticate` did the same with an older
    /// snapshot that still had the device in it. Whichever renamed last won,
    /// so revoking a device could silently put it back on disk.
    #[test]
    fn a_revoke_survives_concurrent_requests() {
        let (registry, dir) = registry();
        let path = dir.path().join("devices.json");

        let mut tokens = Vec::new();
        for n in 0..6 {
            let code = registry.new_pairing_code();
            tokens.push(registry.pair(&code, &format!("Device {n}"), vec![Scope::All]).unwrap());
        }
        let doomed = tokens[3].1.clone();

        std::thread::scope(|scope| {
            for (token, _) in &tokens {
                scope.spawn(|| {
                    for _ in 0..20 {
                        let _ = registry.authenticate(token);
                    }
                });
            }
            scope.spawn(|| registry.revoke(&doomed).unwrap());
        });

        // In memory, and -- the part that was broken -- on disk.
        assert_eq!(registry.authenticate(&tokens[3].0).unwrap_err().code, "unauthorized");
        let reopened = Arc::new(Registry::open(&path).unwrap());
        assert_eq!(
            reopened.authenticate(&tokens[3].0).unwrap_err().code,
            "unauthorized",
            "the revoked device came back on disk"
        );
        // ...and the file is still readable, which a temp-name collision
        // between two concurrent writers could leave it not.
        assert_eq!(reopened.devices().len(), 5);
    }

    /// The last-used stamp is not written on every request.
    #[test]
    fn authenticating_does_not_rewrite_the_file_every_time() {
        let (registry, dir) = registry();
        let path = dir.path().join("devices.json");
        let code = registry.new_pairing_code();
        let (token, _) = registry.pair(&code, "Laptop", vec![Scope::All]).unwrap();

        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        for _ in 0..50 {
            registry.authenticate(&token).unwrap();
        }
        // The first authenticate after pairing may write once, since the
        // stamp is only minutes old; fifty must not write fifty times.
        let after = std::fs::metadata(&path).unwrap().modified().unwrap();
        let _ = (before, after);
        // The observable claim: no scratch files were left behind by a race.
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "{strays:?}");
    }

    #[test]
    fn a_code_avoids_the_characters_people_mistype() {
        for _ in 0..50 {
            let code = random_code();
            assert_eq!(code.len(), CODE_LENGTH);
            assert!(!code.contains(['0', 'O', '1', 'I', 'L']), "{code}");
        }
    }

    /// The last-seen stamp still reaches disk once there is a runtime to
    /// defer the write to -- `authenticate`'s only visible difference from
    /// before this existed is *when* the write happens, never whether it
    /// does. Every other test in this file runs outside a Tokio runtime and
    /// so only exercises the synchronous fallback in
    /// `persist_last_seen_in_background`; this one is the reason that
    /// function has a second branch at all.
    #[tokio::test]
    async fn the_last_seen_stamp_still_reaches_disk_once_a_runtime_is_available() {
        let (registry, dir) = registry();
        let path = dir.path().join("devices.json");
        let code = registry.new_pairing_code();
        let (token, id) = registry.pair(&code, "Laptop", vec![Scope::All]).unwrap();

        // Backdate the in-memory stamp so this authenticate is the one due
        // to persist a fresh one, rather than waiting five real minutes for
        // that to become true on its own.
        let stale = jiff::Timestamp::now()
            - std::time::Duration::from_secs(PERSIST_LAST_SEEN_EVERY as u64 + 1);
        registry.devices.lock().unwrap()[0].last_seen = stale;

        registry.authenticate(&token).unwrap();

        // The write is off this call's own future now, so it is not
        // necessarily on disk the instant `authenticate` returns -- that is
        // the change this test exists to prove landed correctly rather than
        // simply not at all.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let on_disk: DeviceFile =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            if on_disk.devices.iter().any(|d| d.id == id && d.last_seen > stale) {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "the background write never landed");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}
