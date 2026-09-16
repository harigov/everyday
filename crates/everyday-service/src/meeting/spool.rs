//! The spool: where a recording's audio actually lives, from the moment the
//! microphone opens until the note that comes of it exists.
//!
//! One directory per recording, one sealed file per chunk:
//!
//! ```text
//!   <vault dir>/spool/recordings/<recording id>/<track>-<seq>.sealed
//! ```
//!
//! Never `/tmp`, and never the backend's own blob store -- see
//! [`everyday_core::vault::Vault::seal_spool_bytes`]'s own docs for why a
//! spool chunk is a plain file beside the vault rather than a row a Postgres
//! vault would have nowhere on this machine to put. Each file is sealed
//! under the vault's own key with [`chunk_aad`] as associated data, so a
//! chunk cannot be swapped for another recording's, another track's, or
//! another sequence number's without the seal failing to open. Writes go
//! through [`everyday_core::fsutil::write_atomic`]: a crash mid-write
//! leaves the previous complete chunk (or nothing) in place, never a
//! half-written one.
//!
//! # Idempotency
//!
//! [`append`] is safe to call twice with the same `(id, track, seq)`. The
//! shell's own sink retries a chunk that failed to land -- see
//! `everyday-app`'s `capture::RecordingSink` -- and a retry that actually
//! succeeded the first time must not be treated as a new chunk: the file is
//! simply rewritten (an atomic write over itself costs nothing extra to
//! guard against) and the existing [`ChunkMeta`] is updated in place rather
//! than duplicated. Only a genuinely new `(track, seq)` wakes the pipeline
//! through [`crate::meeting::pipeline::chunk_closed`].
//!
//! # Recovery
//!
//! A recording can be abandoned mid-capture -- the app quit, the machine
//! slept through an auto-lock, this process crashed -- and left sitting in
//! `Stage::Recording` for ever, since nothing but the shell's own capture
//! thread would otherwise move it on. [`recover`] is called from
//! [`crate::service::Service::unlocked`], the same moment mail's account
//! tasks pick back up: any recording still `Recording` with no chunk
//! appended *in this process* within [`RECOVERY_STALE`] is finished where
//! its last chunk left it, and anything already mid-pipeline
//! (`Transcribing`, `Identifying`, `Summarising`) is handed back to
//! [`crate::meeting::pipeline::enqueue`], since a restart drops whatever
//! supervised task was working on it. [`begin`] runs the identical
//! reclaiming pass before refusing "a recording is already in progress" --
//! a stale row must not block a fresh one for ever just because nobody has
//! unlocked the vault since the crash that left it there.
//!
//! # Failed expiry
//!
//! A recording that reached `Stage::Failed` keeps its spool until a retry
//! or a discard -- or until [`FAILED_SPOOL_DAYS`] pass, at which point
//! [`expire_failed_tick`] (run from the scheduler's minute tick, but doing
//! real work only once an hour) removes the audio and the row, warning the
//! day before so the loss is not a surprise.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use everyday_core::id::{EventId, RecordingId, TemplateId};
use everyday_core::meeting::{
    CHUNK_SECONDS, ChunkMeta, EventRef, FAILED_SPOOL_DAYS, Recording, SAMPLE_RATE, Stage, Track,
    chunk_aad,
};
use everyday_core::store::meetings::RecordingQuery;
use everyday_core::{Error, Vault};
use jiff::Timestamp;

use crate::error::{CommandError, CommandResult, codes};
use crate::events::{Change, Kind, Notification, Op};
use crate::meeting::pipeline;
use crate::service::Service;

/// Above this much unfinished spool, [`begin`] refuses to start another
/// recording. 2 GB: a full working day of two-track 16 kHz audio is a
/// fraction of this, so the cap is there for the case that actually matters
/// -- a transcriber that has been failing for a week and piling up spools
/// nobody has looked at -- not for ordinary use.
pub const SPOOL_CAP_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// How long, in this process, a recording in `Stage::Recording` may go
/// without an appended chunk before it is treated as abandoned rather than
/// merely between chunks. Twice [`CHUNK_SECONDS`] would already cover an
/// ordinary gap; this is far more generous on purpose; see [`recover`]'s
/// own docs for what "in this process" means for a chunk appended before a
/// restart.
pub const RECOVERY_STALE: Duration = Duration::from_secs(120);

/// How often [`expire_failed_tick`] actually does anything, however often
/// it is called. The failed-spool expiry it runs only ever matters once a
/// week at most -- [`FAILED_SPOOL_DAYS`] is measured in days -- so there is
/// nothing to gain from running its query every minute along with
/// everything else the scheduler does then.
const EXPIRE_SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

static LAST_EXPIRE_SWEEP: Mutex<Option<Instant>> = Mutex::new(None);

/// What [`begin`] needs: an event to tie the recording to (or none, for a
/// call the calendar never knew about), and what the person or the watcher
/// asked for beyond that.
#[derive(Debug, Clone, Default)]
pub struct BeginArgs {
    pub event_id: Option<EventId>,
    pub title: Option<String>,
    pub template_id: Option<TemplateId>,
    pub automatic: bool,
}

// ---- paths ------------------------------------------------------------

fn recordings_root(vault: &Vault) -> PathBuf {
    vault.path().join("spool").join("recordings")
}

fn recording_dir(vault: &Vault, id: RecordingId) -> PathBuf {
    recordings_root(vault).join(id.to_string())
}

fn chunk_path(vault: &Vault, id: RecordingId, track: Track, seq: u32) -> PathBuf {
    recording_dir(vault, id).join(format!("{}-{seq:06}.sealed", track.as_str()))
}

// ---- chunk bytes --------------------------------------------------------

fn write_chunk_file(
    vault: &Vault,
    id: RecordingId,
    track: Track,
    seq: u32,
    pcm: &[i16],
) -> CommandResult<()> {
    let dir = recording_dir(vault, id);
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let mut bytes = Vec::with_capacity(pcm.len() * 2);
    for &sample in pcm {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    let sealed = vault.seal_spool_bytes(&chunk_aad(id, track, seq), &bytes)?;
    let path = chunk_path(vault, id, track, seq);
    everyday_core::fsutil::write_atomic(&path, &sealed, &everyday_core::fsutil::unique_tag())?;
    Ok(())
}

/// Read one chunk back, unsealed. What the pipeline reads speech from; not
/// called by anything in this module itself.
pub fn read_chunk(
    vault: &Vault,
    id: RecordingId,
    track: Track,
    seq: u32,
) -> CommandResult<Vec<i16>> {
    let path = chunk_path(vault, id, track, seq);
    let sealed = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
    let bytes = vault.open_spool_bytes(&chunk_aad(id, track, seq), &sealed)?;
    if bytes.len() % 2 != 0 {
        return Err(CommandError::new(
            codes::INVALID,
            format!("{} held an odd number of bytes", path.display()),
        ));
    }
    Ok(bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect())
}

/// Delete a recording's whole spool directory. Not an error if there is
/// nothing there -- a recording with no chunks yet, or one already cleaned
/// up. Called by [`discard`] and by the pipeline once a note exists.
pub fn remove_audio(vault: &Vault, id: RecordingId) -> CommandResult<()> {
    let dir = recording_dir(vault, id);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(&dir, e).into()),
    }
}

/// How much of the spool every recording together is using right now.
/// Best-effort: a directory that cannot be read (permissions, a race with a
/// concurrent delete) contributes nothing rather than failing the whole
/// count, since this only ever gates a soft cap, never correctness.
pub fn spool_bytes(vault: &Vault) -> u64 {
    dir_size(&recordings_root(vault))
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut total = 0u64;
    for entry in entries.flatten() {
        match entry.metadata() {
            Ok(meta) if meta.is_dir() => total += dir_size(&entry.path()),
            Ok(meta) => total += meta.len(),
            Err(_) => {}
        }
    }
    total
}

// ---- begin / append / finish / discard / retry -------------------------

/// Start a recording: check that nothing else is already being captured,
/// that the spool has room, and that the switch is on and usable, then mint
/// the row.
///
/// Checked in that order deliberately: whether a microphone is already
/// spoken for and whether the spool has room are facts about this machine's
/// resources, true or false whatever the settings say, and cost nothing to
/// answer; whether meeting notes are configured at all is the one question
/// that needs a transcriber to weigh in, through
/// [`crate::domains::meetings::view`]'s own "usable" check -- which, for
/// every backend, local or remote, also requires the speech kit to be
/// genuinely installed on disk (VAD needs it before anything else can
/// happen). Refusing on the cheap facts first is also what keeps them
/// answerable in a plain unit test, which does not download ~40 MB of
/// models just to construct a vault; see this file's own `env` for where
/// that line is drawn.
///
/// Title and calendar details come from `args.event_id`'s event when one is
/// given (`args.title` still overrides the displayed title, for the notes
/// app's own "record a call" dialog), and `args.template_id` falls back to
/// the settings' own default the way [`everyday_core::meeting::MeetingSettings::template`]
/// already does for a template that is later asked for by id.
pub fn begin(svc: &Arc<Service>, args: BeginArgs) -> CommandResult<Recording> {
    let vault = svc.require()?;

    if !reclaim_stale(svc, &vault)?.is_empty() {
        return Err(CommandError::new("already_running", "a recording is already in progress"));
    }

    let used = spool_bytes(&vault);
    if used >= SPOOL_CAP_BYTES {
        return Err(CommandError::new(
            "spool_full",
            format!(
                "the meeting spool already holds {:.1} GB of unfinished recordings -- finish, \
                 discard or retry one before starting another",
                used as f64 / (1024.0 * 1024.0 * 1024.0)
            ),
        ));
    }

    let settings = vault.meeting_settings()?;
    if !settings.enabled {
        return Err(Error::Invalid("meeting notes are turned off".into()).into());
    }
    let checked = crate::domains::meetings::view(&vault, settings.clone())?;
    if !checked.usable {
        let why = checked.problem.unwrap_or_else(|| "the transcriber is not usable".into());
        return Err(Error::Invalid(format!("cannot start recording: {why}")).into());
    }

    let (event_ref, default_title) = match args.event_id {
        Some(id) => {
            let event = vault.event(id)?;
            let calendar_name = vault
                .calendars()?
                .into_iter()
                .find(|c| c.id == event.calendar_id)
                .map(|c| c.name)
                .unwrap_or_default();
            let title = event.title.clone();
            (Some(EventRef::from_event(&event, &calendar_name)), title)
        }
        None => (None, "Recording".to_string()),
    };
    let title = args.title.filter(|t| !t.trim().is_empty()).unwrap_or(default_title);
    let template_id = args.template_id.unwrap_or_else(|| settings.template(None).id);

    let mut recording = Recording::new(title, event_ref, template_id);
    recording.automatic = args.automatic;
    vault.save_recording(&recording)?;
    Ok(recording)
}

/// Largest a chunk's samples may be: [`CHUNK_SECONDS`] at [`SAMPLE_RATE`],
/// mono. Anything bigger is not a chunk this spool's own layout can be
/// trusted to hold cleanly, whatever produced it.
pub const MAX_CHUNK_SAMPLES: usize = (CHUNK_SECONDS * SAMPLE_RATE) as usize;

/// Spool one chunk of one track. Idempotent on `(id, track, seq)` -- see
/// this module's own docs.
pub fn append(
    svc: &Arc<Service>,
    id: RecordingId,
    track: Track,
    seq: u32,
    start_ms: u64,
    pcm: &[i16],
) -> CommandResult<()> {
    let vault = svc.require()?;

    if pcm.len() > MAX_CHUNK_SAMPLES {
        return Err(CommandError::new(
            codes::INVALID,
            format!(
                "a chunk cannot hold more than {MAX_CHUNK_SAMPLES} samples \
                 ({CHUNK_SECONDS}s at {SAMPLE_RATE}Hz); this one has {}",
                pcm.len()
            ),
        ));
    }

    let mut recording = vault.recording(id)?;
    if !matches!(recording.stage, Stage::Recording) {
        return Err(CommandError::new(
            codes::INVALID,
            format!("recording is {} and cannot take more audio", recording.stage.as_str()),
        ));
    }

    write_chunk_file(&vault, id, track, seq, pcm)?;

    let is_new = match recording.chunks.iter_mut().find(|c| c.track == track && c.seq == seq) {
        Some(meta) => {
            // A resend of a chunk already spooled: refresh the timing in
            // case it differs, but leave `transcribed` alone -- the
            // pipeline may already have used this chunk, and a retry must
            // not undo that.
            meta.start_ms = start_ms;
            meta.samples = pcm.len() as u64;
            false
        }
        None => {
            recording.chunks.push(ChunkMeta {
                track,
                seq,
                start_ms,
                samples: pcm.len() as u64,
                transcribed: false,
            });
            true
        }
    };
    recording.updated_at = Timestamp::now();
    vault.save_recording(&recording)?;
    svc.meeting_touch_append(id);

    if is_new {
        pipeline::chunk_closed(svc, id, track, seq);
    }
    Ok(())
}

/// End a recording ordinarily: stamp when it ended, move it to
/// `Stage::Transcribing`, and hand it to the pipeline.
pub fn finish(svc: &Arc<Service>, id: RecordingId) -> CommandResult<Recording> {
    let vault = svc.require()?;
    let mut recording = vault.recording(id)?;
    if !matches!(recording.stage, Stage::Recording) {
        return Err(CommandError::new(
            codes::INVALID,
            format!("recording is {} and cannot be finished", recording.stage.as_str()),
        ));
    }
    recording.ended_at = Some(Timestamp::now());
    recording.stage = Stage::Transcribing;
    recording.updated_at = Timestamp::now();
    vault.save_recording(&recording)?;
    svc.meeting_forget_append(id);
    pipeline::enqueue(svc, id);
    Ok(recording)
}

/// Throw a recording away: its spool, and its history row. Any stage but
/// `Done` -- a finished note has nothing left in the spool to discard, and
/// is deleted through `delete_recording` instead.
pub fn discard(svc: &Arc<Service>, id: RecordingId) -> CommandResult<()> {
    let vault = svc.require()?;
    let recording = vault.recording(id)?;
    if recording.stage.is_finished() {
        return Err(CommandError::new(
            codes::INVALID,
            "a finished recording cannot be discarded, only deleted from the history",
        ));
    }
    remove_audio(&vault, id)?;
    vault.delete_recording(id)?;
    svc.meeting_forget_append(id);
    Ok(())
}

/// Retry a failed recording: back to whichever pipeline stage it had
/// reached when it failed (or `Transcribing`, if that was somehow still
/// `Recording` -- the spool's own part of the job is always done by the
/// time anything can fail), and re-enqueued.
pub fn retry(svc: &Arc<Service>, id: RecordingId) -> CommandResult<Recording> {
    let vault = svc.require()?;
    let mut recording = vault.recording(id)?;
    let Stage::Failed { at, .. } = recording.stage.clone() else {
        return Err(CommandError::new(codes::INVALID, "only a failed recording can be retried"));
    };
    recording.stage = match *at {
        Stage::Recording => Stage::Transcribing,
        other => other,
    };
    recording.updated_at = Timestamp::now();
    vault.save_recording(&recording)?;
    pipeline::enqueue(svc, id);
    Ok(recording)
}

// ---- recovery -----------------------------------------------------------

/// Reclaim every recording in `Stage::Recording` that looks abandoned (see
/// [`RECOVERY_STALE`]), moving each to `Stage::Transcribing` at wherever its
/// last chunk left it and handing it to the pipeline. Returns whichever
/// ones were *not* reclaimed -- still plausibly live -- so [`begin`] can
/// refuse against them by fact rather than merely by count, and [`recover`]
/// can tell the difference between "nothing to do" and "one is genuinely
/// still going".
fn reclaim_stale(svc: &Arc<Service>, vault: &Vault) -> CommandResult<Vec<Recording>> {
    let running = vault.recordings(&RecordingQuery {
        stages: vec![Stage::Recording.as_str().to_string()],
        ..Default::default()
    })?;
    let mut still_live = Vec::new();
    for recording in running {
        let stale =
            svc.meeting_since_append(recording.id).is_none_or(|since| since > RECOVERY_STALE);
        if stale {
            finish_stuck(svc, vault, recording)?;
        } else {
            still_live.push(recording);
        }
    }
    Ok(still_live)
}

fn finish_stuck(svc: &Arc<Service>, vault: &Vault, mut recording: Recording) -> CommandResult<()> {
    let id = recording.id;
    recording.ended_at.get_or_insert_with(Timestamp::now);
    recording.stage = Stage::Transcribing;
    recording.updated_at = Timestamp::now();
    vault.save_recording(&recording)?;
    svc.meeting_forget_append(id);
    svc.events().changed(Change::new(Kind::Recording, Op::Updated));
    pipeline::enqueue(svc, id);
    Ok(())
}

/// Called once, from [`crate::service::Service::unlocked`]: reclaim any
/// recording capture abandoned (see [`reclaim_stale`]), and re-enqueue
/// anything left mid-pipeline -- `Transcribing`, `Identifying` or
/// `Summarising` -- since a restart drops whatever supervised task was
/// working on it and nothing else will pick it back up.
///
/// Failures are logged, not propagated: recovery running late is a delay,
/// not a reason to refuse the unlock that triggered it.
pub fn recover(svc: &Arc<Service>, vault: &Arc<Vault>) {
    if let Err(e) = recover_inner(svc, vault) {
        tracing::warn!(error = %e, "meeting spool: recovery pass failed");
    }
}

fn recover_inner(svc: &Arc<Service>, vault: &Vault) -> CommandResult<()> {
    reclaim_stale(svc, vault)?;

    let pending = vault.recordings(&RecordingQuery {
        stages: vec![
            Stage::Transcribing.as_str().to_string(),
            Stage::Identifying.as_str().to_string(),
            Stage::Summarising.as_str().to_string(),
        ],
        ..Default::default()
    })?;
    for recording in pending {
        pipeline::enqueue(svc, recording.id);
    }
    Ok(())
}

// ---- failed expiry --------------------------------------------------------

/// Run [`expire_failed`], but only once every [`EXPIRE_SWEEP_INTERVAL`]
/// however often this is called. Meant to be called from the scheduler's
/// minute tick alongside everything else it does while the vault is
/// unlocked and writable.
pub fn expire_failed_tick(svc: &Arc<Service>, vault: &Vault) {
    {
        let mut last = LAST_EXPIRE_SWEEP.lock().unwrap();
        if last.is_some_and(|t| t.elapsed() < EXPIRE_SWEEP_INTERVAL) {
            return;
        }
        *last = Some(Instant::now());
    }
    if let Err(e) = expire_failed(svc, vault) {
        tracing::warn!(error = %e, "meeting spool: failed-recording expiry pass failed");
    }
}

/// Failed recordings older than [`FAILED_SPOOL_DAYS`] have their audio
/// removed and their row deleted; ones one day short of that get a
/// [`Notification`], once per process, so the loss is not a surprise.
///
/// Age is measured from `updated_at`, which is exactly when a recording
/// entered `Stage::Failed`: nothing else touches a failed row until a
/// retry, a discard, or this sweep does.
fn expire_failed(svc: &Arc<Service>, vault: &Vault) -> CommandResult<()> {
    let now = Timestamp::now();
    let failed = vault.recordings(&RecordingQuery {
        // `Stage::as_str` answers "failed" for every `Stage::Failed { .. }`
        // regardless of its fields, so the literal here is exactly what a
        // real instance would produce -- see that method's own doc on why
        // `stage` is also a plaintext column at all.
        stages: vec!["failed".to_string()],
        ..Default::default()
    })?;
    let mut deleted = false;
    for recording in failed {
        let age_days = (now.as_second() - recording.updated_at.as_second()) / 86_400;
        if age_days >= FAILED_SPOOL_DAYS {
            remove_audio(vault, recording.id)?;
            vault.delete_recording(recording.id)?;
            svc.meeting_forget_append(recording.id);
            deleted = true;
            continue;
        }
        if age_days >= FAILED_SPOOL_DAYS - 1 && svc.meeting_expiry_warn_once(recording.id) {
            svc.events().notify(
                Notification::warning(
                    "A recording that could not be turned into a note will be deleted tomorrow",
                )
                .body(recording.title.clone())
                .for_user()
                .key(format!("meeting-expiry:{}", recording.id)),
            );
        }
    }
    if deleted {
        svc.events().changed(Change::new(Kind::Recording, Op::Deleted));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventSink, Notification};
    use everyday_core::meeting::MeetingSettings;
    use std::sync::Mutex as StdMutex;

    /// A vault and a service around it, in a temporary directory. Meeting
    /// notes are left off, and no transcriber is configured -- see
    /// [`begin`]'s own doc on why a plain unit test does not set up a
    /// genuinely usable one: `usable` in `crate::domains::meetings` requires
    /// the speech kit to be installed on disk for every backend, which this
    /// helper does not download. `begin`'s success path is exercised by
    /// `meeting_pipeline_e2e.rs` instead; the tests below seed a recording
    /// directly in `Stage::Recording` or `Stage::Failed` -- exactly the
    /// state a successful `begin` would have left.
    fn env() -> (Arc<Service>, Arc<Vault>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        // A real password, not `None`: an unencrypted vault's cipher is a
        // no-op, and the round-trip test below exists specifically to
        // prove a chunk is sealed rather than merely moved.
        let vault = everyday_vault::create(
            dir.path(),
            everyday_core::VaultConfig {
                password: Some("correct horse battery staple".into()),
                kdf: everyday_core::crypto::KdfParams::insecure_fast(),
                ..Default::default()
            },
        )
        .unwrap();
        let svc = Arc::new(Service::new());
        let vault = svc.set(vault);
        (svc, vault, dir)
    }

    fn enabled_settings() -> MeetingSettings {
        MeetingSettings { enabled: true, ..Default::default() }
    }

    /// Seed a recording directly, bypassing `begin` -- see [`env`]'s own
    /// doc for why `begin`'s own path is not exercised in this module.
    fn seed(vault: &Vault, stage: Stage) -> Recording {
        let mut recording = Recording::new("Design sync", None, TemplateId::new());
        recording.stage = stage;
        vault.save_recording(&recording).unwrap();
        recording
    }

    fn samples(n: usize, value: i16) -> Vec<i16> {
        vec![value; n]
    }

    #[derive(Clone, Default)]
    struct TestSink {
        notifications: Arc<StdMutex<Vec<Notification>>>,
        changes: Arc<StdMutex<Vec<Change>>>,
    }

    impl EventSink for TestSink {
        fn notify(&self, notification: Notification) {
            self.notifications.lock().unwrap().push(notification);
        }
        fn changed(&self, change: Change) {
            self.changes.lock().unwrap().push(change);
        }
    }

    // ---- chunk round trip ---------------------------------------------

    #[test]
    fn a_chunk_round_trips_through_the_spool_sealed_on_disk() {
        let (svc, vault, _dir) = env();
        let recording = seed(&vault, Stage::Recording);
        let pcm = samples(1000, 4242);

        append(&svc, recording.id, Track::Mic, 0, 0, &pcm).unwrap();

        let read_back = read_chunk(&vault, recording.id, Track::Mic, 0).unwrap();
        assert_eq!(read_back, pcm);

        // The bytes on disk must not be the plaintext PCM -- that is the
        // whole point of sealing them.
        let raw = std::fs::read(chunk_path(&vault, recording.id, Track::Mic, 0)).unwrap();
        let mut plain = Vec::with_capacity(pcm.len() * 2);
        for s in &pcm {
            plain.extend_from_slice(&s.to_le_bytes());
        }
        assert_ne!(raw, plain, "a spooled chunk must be sealed, not plaintext");
    }

    #[test]
    fn append_is_idempotent_on_the_same_track_and_sequence() {
        let (svc, vault, _dir) = env();
        let recording = seed(&vault, Stage::Recording);
        let pcm = samples(500, 7);

        append(&svc, recording.id, Track::Mic, 3, 90_000, &pcm).unwrap();
        append(&svc, recording.id, Track::Mic, 3, 90_000, &pcm).unwrap();

        let reloaded = vault.recording(recording.id).unwrap();
        assert_eq!(reloaded.chunks.len(), 1, "a resend must not duplicate the chunk");
        assert_eq!(read_chunk(&vault, recording.id, Track::Mic, 3).unwrap(), pcm);
    }

    #[test]
    fn append_refuses_a_chunk_bigger_than_the_chunk_length_allows() {
        let (svc, vault, _dir) = env();
        let recording = seed(&vault, Stage::Recording);
        let too_big = samples(MAX_CHUNK_SAMPLES + 1, 1);

        let err = append(&svc, recording.id, Track::Mic, 0, 0, &too_big).unwrap_err();
        assert_eq!(err.code, "invalid");
    }

    #[test]
    fn append_refuses_once_the_recording_has_left_stage_recording() {
        let (svc, vault, _dir) = env();
        let recording = seed(&vault, Stage::Transcribing);

        let err = append(&svc, recording.id, Track::Mic, 0, 0, &samples(10, 1)).unwrap_err();
        assert_eq!(err.code, "invalid");
    }

    // ---- begin refusals -------------------------------------------------

    #[test]
    fn begin_refuses_when_the_switch_is_off() {
        let (svc, vault, _dir) = env();
        vault.save_meeting_settings(&MeetingSettings::default()).unwrap();

        let err = begin(&svc, BeginArgs::default()).unwrap_err();
        assert!(err.message.contains("turned off"), "{}", err.message);
    }

    #[test]
    fn begin_refuses_when_no_transcriber_has_been_chosen() {
        let (svc, vault, _dir) = env();
        vault.save_meeting_settings(&enabled_settings()).unwrap();

        let err = begin(&svc, BeginArgs::default()).unwrap_err();
        assert!(err.message.contains("cannot start recording"), "{}", err.message);
    }

    #[test]
    fn begin_refuses_while_another_recording_is_live() {
        let (svc, vault, _dir) = env();
        let recording = seed(&vault, Stage::Recording);
        svc.meeting_touch_append(recording.id);

        let err = begin(&svc, BeginArgs::default()).unwrap_err();
        assert_eq!(err.code, "already_running");
    }

    #[test]
    fn begin_reclaims_a_stale_recording_rather_than_refusing_for_ever() {
        let (svc, vault, _dir) = env();
        let stale = seed(&vault, Stage::Recording);
        // No `meeting_touch_append`: exactly what a fresh process (after a
        // crash or a quit) looks like.

        // Still refused -- meeting notes are off in this vault -- but for a
        // *different* reason, proving the stale row was reclaimed on the
        // way rather than blocking with `already_running`.
        let err = begin(&svc, BeginArgs::default()).unwrap_err();
        assert_eq!(err.code, "invalid");
        assert!(err.message.contains("turned off"), "{}", err.message);

        let reloaded = vault.recording(stale.id).unwrap();
        assert_eq!(reloaded.stage, Stage::Transcribing);
    }

    #[test]
    fn begin_refuses_over_the_spool_cap() {
        let (svc, vault, _dir) = env();
        // A sparse file: its *logical* length is the cap, but it costs
        // almost nothing on disk, so the test does not actually write two
        // gigabytes.
        let dir = recording_dir(&vault, RecordingId::new());
        std::fs::create_dir_all(&dir).unwrap();
        let file = std::fs::File::create(dir.join("mic-000000.sealed")).unwrap();
        file.set_len(SPOOL_CAP_BYTES).unwrap();

        let err = begin(&svc, BeginArgs::default()).unwrap_err();
        assert_eq!(err.code, "spool_full");
    }

    // ---- finish / discard / retry ---------------------------------------

    #[test]
    fn finish_moves_to_transcribing_and_stamps_when_it_ended() {
        let (svc, vault, _dir) = env();
        let recording = seed(&vault, Stage::Recording);

        let finished = finish(&svc, recording.id).unwrap();
        assert_eq!(finished.stage, Stage::Transcribing);
        assert!(finished.ended_at.is_some());
        assert!(svc.meeting_since_append(recording.id).is_none(), "forgotten on finish");
    }

    #[test]
    fn finish_refuses_a_recording_that_is_not_recording() {
        let (svc, vault, _dir) = env();
        let recording = seed(&vault, Stage::Done);
        let err = finish(&svc, recording.id).unwrap_err();
        assert_eq!(err.code, "invalid");
    }

    #[test]
    fn discard_deletes_the_files_and_the_row() {
        let (svc, vault, _dir) = env();
        let recording = seed(&vault, Stage::Recording);
        append(&svc, recording.id, Track::Mic, 0, 0, &samples(10, 1)).unwrap();
        assert!(recording_dir(&vault, recording.id).exists());

        discard(&svc, recording.id).unwrap();

        assert!(!recording_dir(&vault, recording.id).exists());
        assert!(vault.recording(recording.id).is_err());
    }

    #[test]
    fn discard_refuses_a_finished_recording() {
        let (svc, vault, _dir) = env();
        let recording = seed(&vault, Stage::Done);
        let err = discard(&svc, recording.id).unwrap_err();
        assert_eq!(err.code, "invalid");
    }

    #[test]
    fn retry_returns_a_failed_recording_to_the_stage_it_failed_at() {
        let (svc, vault, _dir) = env();
        let recording = seed(
            &vault,
            Stage::Failed { reason: "no reply".into(), at: Box::new(Stage::Identifying) },
        );

        let retried = retry(&svc, recording.id).unwrap();
        assert_eq!(retried.stage, Stage::Identifying);
    }

    #[test]
    fn retry_refuses_a_recording_that_has_not_failed() {
        let (svc, vault, _dir) = env();
        let recording = seed(&vault, Stage::Transcribing);
        let err = retry(&svc, recording.id).unwrap_err();
        assert_eq!(err.code, "invalid");
    }

    // ---- recovery ---------------------------------------------------------

    #[test]
    fn recovery_finishes_a_recording_with_no_live_capture_in_this_process() {
        let (svc, vault, _dir) = env();
        let stale = seed(&vault, Stage::Recording);
        // Never touched: a fresh process, per this module's own doc.

        recover(&svc, &vault);

        let reloaded = vault.recording(stale.id).unwrap();
        assert_eq!(reloaded.stage, Stage::Transcribing);
    }

    #[test]
    fn recovery_leaves_a_recording_alone_while_it_is_still_being_appended_to() {
        let (svc, vault, _dir) = env();
        let live = seed(&vault, Stage::Recording);
        svc.meeting_touch_append(live.id);

        recover(&svc, &vault);

        let reloaded = vault.recording(live.id).unwrap();
        assert_eq!(reloaded.stage, Stage::Recording);
    }

    // ---- failed expiry ----------------------------------------------------

    fn days_ago(days: i64) -> Timestamp {
        Timestamp::from_second(Timestamp::now().as_second() - days * 86_400).unwrap()
    }

    #[test]
    fn expire_failed_deletes_a_recording_past_the_grace_and_removes_its_audio() {
        let (svc, vault, _dir) = env();
        let mut recording = seed(
            &vault,
            Stage::Failed { reason: "boom".into(), at: Box::new(Stage::Transcribing) },
        );
        // Seed a chunk file directly -- `append` itself refuses a failed
        // recording, the same guard this row is already past.
        write_chunk_file(&vault, recording.id, Track::Mic, 0, &samples(10, 1)).unwrap();
        recording.updated_at = days_ago(FAILED_SPOOL_DAYS + 1);
        vault.save_recording(&recording).unwrap();

        expire_failed(&svc, &vault).unwrap();

        assert!(vault.recording(recording.id).is_err(), "the row should be gone");
        assert!(!recording_dir(&vault, recording.id).exists(), "and its audio with it");
    }

    #[test]
    fn expire_failed_warns_once_the_day_before_and_does_not_delete_yet() {
        let (svc, vault, _dir) = env();
        let sink = TestSink::default();
        svc.set_events(Arc::new(sink.clone()));
        let mut recording = seed(
            &vault,
            Stage::Failed { reason: "boom".into(), at: Box::new(Stage::Transcribing) },
        );
        recording.updated_at = days_ago(FAILED_SPOOL_DAYS - 1);
        vault.save_recording(&recording).unwrap();

        expire_failed(&svc, &vault).unwrap();

        assert!(vault.recording(recording.id).is_ok(), "not deleted yet");
        let notifications = sink.notifications.lock().unwrap();
        assert_eq!(notifications.len(), 1);
        assert!(notifications[0].title.contains("deleted tomorrow"));

        drop(notifications);
        // A second pass must not warn again -- `meeting_expiry_warn_once`.
        expire_failed(&svc, &vault).unwrap();
        assert_eq!(sink.notifications.lock().unwrap().len(), 1);
    }

    #[test]
    fn expire_failed_leaves_a_recently_failed_recording_alone() {
        let (svc, vault, _dir) = env();
        let recording = seed(
            &vault,
            Stage::Failed { reason: "boom".into(), at: Box::new(Stage::Transcribing) },
        );

        expire_failed(&svc, &vault).unwrap();

        assert!(vault.recording(recording.id).is_ok());
    }
}
