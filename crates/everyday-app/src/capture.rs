//! Native audio capture for meeting notes: two cpal streams, resampled and
//! framed into the chunks `everyday_service::meeting::spool` expects.
//!
//! See `docs/plans/meeting-notes.md`'s "Decisions already made" and "The
//! shape of it". This module is the app's half of that picture: it owns the
//! microphone and the loopback device, and hands 16 kHz mono PCM to a
//! [`ChunkSink`] -- which, in the shipped app, is [`RecordingSink`], sending
//! it to the service through the same command path the webview uses (see
//! `crate::remote::Session::call`), so in-process and remote-client mode run
//! identical code. `crates/everyday-app/examples/capture.rs` is the spike
//! that first proved the approach on this machine; its module doc has the
//! findings.
//!
//! # The loopback trick
//!
//! `cpal` 0.18 opens both tracks with the same call: `build_input_stream` on
//! the default *input* device for the microphone, and on the default
//! *output* device for everything else. On Windows this transparently
//! enables WASAPI's loopback mode; on macOS 14.6+ it opens a Core Audio
//! process tap; on Linux, with the `pipewire` feature enabled (Linux only --
//! see `Cargo.toml`), the PipeWire host exposes a sink node as a duplex
//! device, and opening an *input* stream on it sets `STREAM_CAPTURE_SINK`,
//! handing back that sink's monitor (`cpal`'s `host/pipewire/device.rs`, the
//! comment above its `Role::Sink => DeviceDirection::Duplex` match arm).
//! `open_system_stream` below is the one place this crate relies on it.
//!
//! # Threads
//!
//! `cpal`'s data callback runs on the platform's own realtime audio thread
//! and must not block or allocate heavily, so it only downmixes to mono and
//! pushes the result through a bounded channel (dropping the block if the
//! channel is full, rather than ever blocking the audio thread). Everything
//! else -- resampling, chunking, level meters, and the calls a [`ChunkSink`]
//! makes -- happens on one dedicated `std::thread` ("the capture thread"),
//! spawned by [`start`] and joined by [`CaptureHandle::stop`].
//!
//! `cpal::Stream` is not guaranteed `Send` as of 0.18 (that bound arrives in
//! an unreleased version), and some backends are thread-affine regardless
//! (WASAPI's COM apartment, in particular), so every `Stream` this module
//! opens is created, played and dropped on the capture thread and never
//! moves off it. The bounded channel and the shared status the rest of the
//! app reads carry only plain data, which has no such restriction.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SupportedStreamConfig};
use everyday_core::id::{RecordingId, TemplateId};
use everyday_core::meeting::{CHUNK_SECONDS, SAMPLE_RATE, Track};
use everyday_service::error::{CommandError, CommandResult};
use everyday_service::events::EventSink;
use jiff::Timestamp;
use rubato::{Fft, FixedSync, Indexing, Resampler as _};
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter};

/// Below this RMS (of a full-scale signal, 0..1) a track counts as silent:
/// `CaptureStatus::system_silent_ms`, the mic-only fallback's own level
/// meter, and the auto-stop decision all use it.
const SILENCE_RMS: f32 = 0.02;

/// How much recent audio a level meter averages over.
const LEVEL_WINDOW_MS: u64 = 300;

/// How often `meeting-status` is re-emitted while recording.
pub const STATUS_INTERVAL: Duration = Duration::from_millis(250);

/// How long both tracks must stay quiet, after the event ends, before
/// auto-stop acts. See [`auto_stop_decision`].
pub const AUTO_STOP_QUIET: Duration = Duration::from_secs(120);

/// How long past the event's end, still recording, before the UI is asked
/// whether to keep going (`meeting-still-on`).
pub const AUTO_STOP_ASK_AFTER: Duration = Duration::from_secs(30 * 60);

/// How often [`run`] tries to reopen the system track once it has failed --
/// on the first open, or after a `cpal::Error` mid-call. A device that was
/// not there yet (a Bluetooth headset still pairing, PipeWire still
/// starting up) or dropped out and comes back should not need the person to
/// restart the recording to pick it up again; see [`CaptureStatus::system_unavailable`],
/// which this clears on the retry that succeeds.
const SYSTEM_RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// Blocks in flight between a `cpal` callback and the capture thread. Small
/// on purpose: a full channel means the worker is behind, and dropping the
/// odd block is a click, not a gap you'd notice in a transcript. `try_send`
/// never blocks the realtime callback either way.
const RAW_CHANNEL_CAPACITY: usize = 64;

/// At most this much unsent audio, per track, is kept in memory while a
/// [`ChunkSink`] cannot take a chunk -- the case that matters is a
/// remote-client session whose connection has gone quiet. Recording keeps
/// running past this; the sink gives up and reports it, which the capture
/// thread turns into a stop. See [`RecordingSink`].
const MAX_PENDING: Duration = Duration::from_secs(10 * 60);

const CHUNK_SAMPLES: usize = (CHUNK_SECONDS * SAMPLE_RATE) as usize;

/// A `meeting-status` payload, matching `ui/src/lib/types.ts`'s
/// `CaptureStatus`, field for field.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStatus {
    pub recording_id: RecordingId,
    pub title: String,
    pub automatic: bool,
    pub elapsed_ms: u64,
    /// 0..1, recent RMS.
    pub mic_level: f32,
    pub system_level: f32,
    /// The call track has been flat this long while the mic was live.
    pub system_silent_ms: u64,
    /// System audio could not be opened on this machine; mic only. Retried
    /// every [`SYSTEM_RETRY_INTERVAL`] in the background -- see [`run`] --
    /// so this clears itself, without a restart, the moment a retry
    /// succeeds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_unavailable: Option<String>,
    /// When auto-stop will act, if armed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_stop_at: Option<Timestamp>,
}

/// The Tauri event carrying [`CaptureStatus`] (or `null`, on stop).
pub const STATUS_EVENT: &str = "meeting-status";

/// Asks the interface whether to keep recording past
/// [`AUTO_STOP_ASK_AFTER`].
pub const STILL_ON_EVENT: &str = "meeting-still-on";

/// A plain, displayable error from the capture engine. Not
/// [`everyday_service::error::CommandError`]: this crosses a `std::thread`
/// boundary and an audio backend's errors have no vault-command code to
/// carry.
#[derive(Debug, Clone)]
pub struct CaptureError(pub String);

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CaptureError {}

impl From<CommandError> for CaptureError {
    fn from(e: CommandError) -> Self {
        Self(e.to_string())
    }
}

impl From<CaptureError> for CommandError {
    fn from(e: CaptureError) -> Self {
        CommandError::new("capture", e.0)
    }
}

// ---- the sink --------------------------------------------------------

/// Where framed chunks go. The worker on the capture thread calls this
/// directly and synchronously -- it is *not* the realtime audio thread, so
/// a sink that blocks (a network round trip, a file write) is fine.
pub trait ChunkSink: Send {
    /// Called once, before the first chunk.
    fn begin(&mut self) -> Result<(), CaptureError>;
    /// One chunk of one track. Idempotent on `(track, seq)`: a sink is free
    /// to retry a failed send without risking a duplicate.
    fn append(
        &mut self,
        track: Track,
        seq: u32,
        start_ms: u64,
        samples: Vec<i16>,
    ) -> Result<(), CaptureError>;
    /// Called once, after the last chunk, on an ordinary stop.
    fn finish(&mut self) -> Result<(), CaptureError>;
    /// Called instead of `finish` when the recording is discarded.
    fn discard(&mut self) -> Result<(), CaptureError>;
}

/// The shipped sink: sends chunks to the service through a plain function
/// call, so it works the same whether that call lands in this process
/// ([`everyday_service::Service::call`]) or crosses the wire to a paired
/// desktop (`everyday_server::client::RemoteClient::call`). See
/// `crate::remote::Session::call`, which both of those implement.
///
/// Real-time safety is not a concern here -- this runs on the capture
/// thread, never the audio callback -- but *availability* is: a remote
/// vault can go quiet for minutes at a time (a laptop closing its lid), and
/// recording should not stop just because the last chunk did not land yet.
/// So a failed send is retried with backoff, and if it keeps failing the
/// chunk (and everything after it) waits in `pending` rather than being
/// lost. `pending` is capped at [`MAX_PENDING`] of audio; past that,
/// `append` gives up and reports it, which the capture thread turns into a
/// stop -- an unbounded backlog in memory is worse than losing the tail of
/// a recording nobody can reach anyway.
/// A synchronous way to run a named service command: `name`, args in,
/// answer out, the same shape `Session::call` takes once its `Ctx` and
/// `async` are stripped away. `meeting.rs` builds one of these around a
/// `Session` and `tauri::async_runtime::block_on`, so the same closure
/// serves a local vault and a remote one identically.
pub type CommandCaller = dyn Fn(&str, Value) -> CommandResult<Value> + Send;

pub struct RecordingSink {
    call: Box<CommandCaller>,
    id: RecordingId,
    pending: VecDeque<PendingChunk>,
    pending_ms: u64,
}

struct PendingChunk {
    track: Track,
    seq: u32,
    start_ms: u64,
    samples: Vec<i16>,
}

impl PendingChunk {
    fn duration_ms(&self) -> u64 {
        self.samples.len() as u64 * 1000 / SAMPLE_RATE as u64
    }
}

impl RecordingSink {
    pub fn new(id: RecordingId, call: Box<CommandCaller>) -> Self {
        Self { call, id, pending: VecDeque::new(), pending_ms: 0 }
    }

    /// Try once to send one chunk. Transient failures get a few retries with
    /// growing backoff; this is called from the capture thread, not the
    /// audio callback, so blocking here costs nothing but a little latency
    /// on the next chunk.
    fn send_with_retry(&self, chunk: &PendingChunk) -> CommandResult<()> {
        let mut delay = Duration::from_millis(50);
        let mut last_err = None;
        for attempt in 0..4 {
            if attempt > 0 {
                std::thread::sleep(delay);
                delay = (delay * 3).min(Duration::from_secs(2));
            }
            match self.send_once(chunk) {
                Ok(()) => return Ok(()),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.expect("looped at least once"))
    }

    fn send_once(&self, chunk: &PendingChunk) -> CommandResult<()> {
        let mut bytes = Vec::with_capacity(chunk.samples.len() * 2);
        for &s in &chunk.samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let pcm = base64::engine::general_purpose::STANDARD.encode(bytes);
        (self.call)(
            "append_recording_chunk",
            json!({
                "id": self.id,
                "track": chunk.track.as_str(),
                "seq": chunk.seq,
                "startMs": chunk.start_ms,
                "pcm": pcm,
            }),
        )
        .map(|_| ())
    }

    /// Drain as much of the backlog as will go, oldest first. Stops at the
    /// first failure, leaving it (and everything after it) in `pending`.
    fn drain_pending(&mut self) {
        while let Some(chunk) = self.pending.front() {
            match self.send_with_retry(chunk) {
                Ok(()) => {
                    let chunk = self.pending.pop_front().expect("front just matched");
                    self.pending_ms = self.pending_ms.saturating_sub(chunk.duration_ms());
                }
                Err(_) => break,
            }
        }
    }
}

impl ChunkSink for RecordingSink {
    fn begin(&mut self) -> Result<(), CaptureError> {
        Ok(())
    }

    fn append(
        &mut self,
        track: Track,
        seq: u32,
        start_ms: u64,
        samples: Vec<i16>,
    ) -> Result<(), CaptureError> {
        let chunk = PendingChunk { track, seq, start_ms, samples };
        self.pending_ms += chunk.duration_ms();
        self.pending.push_back(chunk);
        self.drain_pending();
        if self.pending_ms > MAX_PENDING.as_millis() as u64 {
            return Err(CaptureError(format!(
                "{} minutes of audio could not be sent to the vault; stopping",
                MAX_PENDING.as_secs() / 60
            )));
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), CaptureError> {
        self.drain_pending();
        if !self.pending.is_empty() {
            return Err(CaptureError("could not send every chunk before finishing".into()));
        }
        (self.call)("finish_recording", json!({ "id": self.id })).map(|_| ()).map_err(Into::into)
    }

    fn discard(&mut self) -> Result<(), CaptureError> {
        (self.call)("discard_recording", json!({ "id": self.id })).map(|_| ()).map_err(Into::into)
    }
}

/// A sink over plain WAV files, one per track: what the capture spike
/// (`examples/capture.rs`) and this module's own tests write chunks
/// through instead of the service. Never on the path anything shipped
/// takes.
pub struct FileSink {
    dir: std::path::PathBuf,
    writers: std::collections::HashMap<Track, hound::WavWriter<std::io::BufWriter<std::fs::File>>>,
}

impl FileSink {
    pub fn new(dir: impl Into<std::path::PathBuf>) -> Self {
        Self { dir: dir.into(), writers: std::collections::HashMap::new() }
    }

    fn writer(
        &mut self,
        track: Track,
    ) -> Result<&mut hound::WavWriter<std::io::BufWriter<std::fs::File>>, CaptureError> {
        if !self.writers.contains_key(&track) {
            std::fs::create_dir_all(&self.dir).map_err(|e| CaptureError(e.to_string()))?;
            let path = self.dir.join(format!("{}.wav", track.as_str()));
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: SAMPLE_RATE,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let writer =
                hound::WavWriter::create(&path, spec).map_err(|e| CaptureError(e.to_string()))?;
            self.writers.insert(track, writer);
        }
        Ok(self.writers.get_mut(&track).expect("just inserted"))
    }
}

impl ChunkSink for FileSink {
    fn begin(&mut self) -> Result<(), CaptureError> {
        std::fs::create_dir_all(&self.dir).map_err(|e| CaptureError(e.to_string()))
    }

    fn append(
        &mut self,
        track: Track,
        _seq: u32,
        _start_ms: u64,
        samples: Vec<i16>,
    ) -> Result<(), CaptureError> {
        let writer = self.writer(track)?;
        for s in samples {
            writer.write_sample(s).map_err(|e| CaptureError(e.to_string()))?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), CaptureError> {
        for (_, writer) in self.writers.drain() {
            writer.finalize().map_err(|e| CaptureError(e.to_string()))?;
        }
        Ok(())
    }

    fn discard(&mut self) -> Result<(), CaptureError> {
        self.writers.clear();
        let _ = std::fs::remove_dir_all(&self.dir);
        Ok(())
    }
}

// ---- resampling --------------------------------------------------------

/// One track's resampler: whatever rate the device runs at, down to
/// [`SAMPLE_RATE`], mono. Stateful across the whole life of a stream --
/// `push` accumulates until there is enough for one of rubato's internal
/// chunks, which has nothing to do with the 30 s chunks this module frames
/// on top; `flush` drains the last partial one, padded with silence, at
/// stop or before a reopen.
struct TrackResampler {
    inner: Fft<f32>,
    chunk_size: usize,
    input_buf: Vec<f32>,
    out_scratch: Vec<f32>,
    out_len: usize,
}

impl TrackResampler {
    fn new(source_rate: u32) -> Result<Self, CaptureError> {
        let chunk_size = 1024;
        let inner = Fft::<f32>::new(
            source_rate as usize,
            SAMPLE_RATE as usize,
            chunk_size,
            1,
            FixedSync::Input,
        )
        .map_err(|e| CaptureError(format!("could not build resampler: {e}")))?;
        let out_len = inner.output_frames_max();
        Ok(Self {
            inner,
            chunk_size,
            input_buf: Vec::new(),
            out_scratch: vec![0.0; out_len],
            out_len,
        })
    }

    /// Run one call to the inner resampler over the first `self.chunk_size`
    /// frames of `input_buf` (padded with silence first, for the final
    /// partial call). Returns how many input frames it actually consumed
    /// and how many output frames it produced -- `rubato`'s own examples
    /// re-query this every call rather than assume it equals `chunk_size`,
    /// since the synchronous `Fft` resampler adjusts it slightly to keep
    /// the resampling ratio exact for a given input/output rate pair.
    fn process(&mut self, partial_len: Option<usize>) -> Result<(usize, usize), CaptureError> {
        let in_adapter = audioadapter_buffers::direct::InterleavedSlice::new(
            &self.input_buf[..self.chunk_size],
            1,
            self.chunk_size,
        )
        .map_err(|e| CaptureError(e.to_string()))?;
        let mut out_adapter = audioadapter_buffers::direct::InterleavedSlice::new_mut(
            &mut self.out_scratch,
            1,
            self.out_len,
        )
        .map_err(|e| CaptureError(e.to_string()))?;
        let indexing = partial_len.map(|n| Indexing::new().partial_len(n));
        self.inner
            .process_into_buffer(&in_adapter, &mut out_adapter, indexing.as_ref())
            .map_err(|e| CaptureError(format!("resample: {e}")))
    }

    /// Feed more raw mono samples at the source rate. Returns whatever
    /// 16 kHz audio is now available -- often nothing, if not enough has
    /// accumulated yet for a full internal chunk.
    fn push(&mut self, samples: &[f32]) -> Result<Vec<f32>, CaptureError> {
        self.input_buf.extend_from_slice(samples);
        let mut out = Vec::new();
        while self.input_buf.len() >= self.chunk_size {
            let (nbr_in, nbr_out) = self.process(None)?;
            out.extend_from_slice(&self.out_scratch[..nbr_out]);
            self.input_buf.drain(..nbr_in);
        }
        Ok(out)
    }

    /// Drain whatever is left, padded with silence. Called at a genuine
    /// stop or before rebuilding the resampler for a reopened device --
    /// after this, `input_buf` is empty and a fresh `push` starts clean.
    fn flush(&mut self) -> Result<Vec<f32>, CaptureError> {
        if self.input_buf.is_empty() {
            return Ok(Vec::new());
        }
        let remaining = self.input_buf.len();
        self.input_buf.resize(self.chunk_size, 0.0);
        let (_nbr_in, nbr_out) = self.process(Some(remaining))?;
        self.input_buf.clear();
        Ok(self.out_scratch[..nbr_out].to_vec())
    }
}

fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

// ---- framing into 30 s chunks -----------------------------------------

/// One chunk ready to hand to a [`ChunkSink`].
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkOut {
    pub track: Track,
    pub seq: u32,
    pub start_ms: u64,
    pub samples: Vec<i16>,
}

/// Groups one track's 16 kHz mono samples into [`CHUNK_SECONDS`] pieces.
///
/// `start_ms` is stamped from wall-clock at the moment a chunk's first
/// sample arrives, not accumulated from a running sample count -- so it is
/// re-anchored at every chunk boundary and drift between the audio clock
/// and the wall clock never carries past one chunk. A gap (the stream was
/// down for a reopen) simply shows up as a jump in the next chunk's
/// `start_ms`, exactly the way a real gap should.
pub struct ChunkFramer {
    track: Track,
    seq: u32,
    buf: Vec<i16>,
    start_ms: Option<u64>,
}

impl ChunkFramer {
    pub fn new(track: Track) -> Self {
        Self { track, seq: 0, buf: Vec::with_capacity(CHUNK_SAMPLES), start_ms: None }
    }

    /// Feed newly resampled 16 kHz mono samples, arriving at wall-clock
    /// `now_ms` (milliseconds since the recording started). Returns every
    /// chunk this call completed -- ordinarily zero or one.
    pub fn push(&mut self, samples: &[i16], now_ms: u64) -> Vec<ChunkOut> {
        let mut out = Vec::new();
        for &s in samples {
            if self.buf.is_empty() {
                self.start_ms.get_or_insert(now_ms);
            }
            self.buf.push(s);
            if self.buf.len() >= CHUNK_SAMPLES {
                out.push(self.take_chunk());
            }
        }
        out
    }

    fn take_chunk(&mut self) -> ChunkOut {
        let samples = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK_SAMPLES));
        let seq = self.seq;
        self.seq += 1;
        let start_ms = self.start_ms.take().unwrap_or(0);
        ChunkOut { track: self.track, seq, start_ms, samples }
    }

    /// The last, short chunk, if there is unsent audio -- "a final partial
    /// chunk is flushed on stop".
    pub fn flush(&mut self) -> Option<ChunkOut> {
        if self.buf.is_empty() { None } else { Some(self.take_chunk()) }
    }
}

// ---- level meters -------------------------------------------------------

/// RMS over the last ~300 ms, 0..1. Recomputed on each push rather than
/// kept as a running sum: the window is a few thousand samples, cheap
/// enough on the capture thread a few times a second, and immune to the
/// floating-point drift a running sum accumulates over an hour-long call.
struct LevelMeter {
    window: VecDeque<i16>,
    capacity: usize,
}

impl LevelMeter {
    fn new() -> Self {
        let capacity = (LEVEL_WINDOW_MS as u32 * SAMPLE_RATE / 1000) as usize;
        Self { window: VecDeque::with_capacity(capacity), capacity }
    }

    fn push(&mut self, samples: &[i16]) {
        for &s in samples {
            if self.window.len() == self.capacity {
                self.window.pop_front();
            }
            self.window.push_back(s);
        }
    }

    fn level(&self) -> f32 {
        if self.window.is_empty() {
            return 0.0;
        }
        let sum_sq: f64 = self
            .window
            .iter()
            .map(|&s| {
                let f = s as f64 / i16::MAX as f64;
                f * f
            })
            .sum();
        (sum_sq / self.window.len() as f64).sqrt() as f32
    }

    fn is_silent(&self) -> bool {
        self.level() < SILENCE_RMS
    }
}

/// Tracks how long a track has been continuously silent, in wall-clock
/// milliseconds since the recording started.
#[derive(Default)]
struct SilenceTracker {
    silent_since_ms: Option<u64>,
}

impl SilenceTracker {
    fn update(&mut self, silent: bool, now_ms: u64) -> u64 {
        if silent {
            let since = *self.silent_since_ms.get_or_insert(now_ms);
            now_ms.saturating_sub(since)
        } else {
            self.silent_since_ms = None;
            0
        }
    }
}

// ---- auto-stop, a pure decision -----------------------------------------

/// What auto-stop should do right now. A pure function of the event's end,
/// the current time, and how long each track has been quiet -- no clock,
/// no audio device, so it is exercised directly in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoStopDecision {
    /// Nothing to do: before the event's end, or one of the tracks is
    /// still live.
    Continue,
    /// Both tracks have been quiet for [`AUTO_STOP_QUIET`] since the event
    /// ended: stop.
    Stop,
    /// Past [`AUTO_STOP_ASK_AFTER`] since the event ended and still going:
    /// ask, rather than keep guessing.
    AskToContinue,
}

/// `mic_silent_ms`/`system_silent_ms`: how long each track has been
/// continuously quiet, as of `now`. `event_end`: the calendar event's end,
/// if this recording has one -- without it, auto-stop never acts, because
/// there is nothing to measure "past" against. `quiet`: how long both
/// tracks must stay silent before acting -- [`AUTO_STOP_QUIET`] in
/// production, threaded through explicitly rather than read from the
/// constant so a test can shorten it without changing production's own
/// value; see [`start`]'s `auto_stop_quiet` parameter.
pub fn auto_stop_decision(
    event_end: Option<Timestamp>,
    now: Timestamp,
    mic_silent_ms: u64,
    system_silent_ms: u64,
    quiet: Duration,
) -> AutoStopDecision {
    let Some(end) = event_end else { return AutoStopDecision::Continue };
    if now < end {
        return AutoStopDecision::Continue;
    }
    let quiet_ms = quiet.as_millis() as u64;
    if mic_silent_ms >= quiet_ms && system_silent_ms >= quiet_ms {
        return AutoStopDecision::Stop;
    }
    let ask_after = jiff::SignedDuration::new(AUTO_STOP_ASK_AFTER.as_secs() as i64, 0);
    if let Ok(threshold) = end.checked_add(ask_after)
        && now >= threshold
    {
        return AutoStopDecision::AskToContinue;
    }
    AutoStopDecision::Continue
}

/// When auto-stop *will* act, if both tracks are currently silent past the
/// event's end -- `CaptureStatus::auto_stop_at`. `None` unless armed: past
/// the event's end, and both tracks silent right now. `quiet`: see
/// [`auto_stop_decision`].
pub fn auto_stop_at(
    event_end: Option<Timestamp>,
    now: Timestamp,
    mic_silent_ms: u64,
    system_silent_ms: u64,
    quiet: Duration,
) -> Option<Timestamp> {
    let end = event_end?;
    if now < end || mic_silent_ms == 0 || system_silent_ms == 0 {
        return None;
    }
    let quietest = mic_silent_ms.min(system_silent_ms);
    let since = now.checked_sub(jiff::SignedDuration::from_millis(quietest as i64)).ok()?;
    let quiet = jiff::SignedDuration::new(quiet.as_secs() as i64, 0);
    since.checked_add(quiet).ok()
}

// ---- driving cpal ---------------------------------------------------------

/// What one `cpal` callback (or its error callback) hands to the capture
/// thread. `Audio` is pushed with `try_send` and may be dropped under
/// backpressure; `Error` and `Control` use a blocking `send` -- both are
/// rare, and losing one matters more than losing an audio block.
enum Event {
    Audio { track: Track, samples: Vec<f32>, source_rate: u32 },
    Error { track: Track, kind: cpal::ErrorKind, message: String },
    Control(ControlMsg),
}

enum ControlMsg {
    Stop { discard: bool },
}

/// Interleaved samples of any `cpal`-supported format, down to mono `f32`
/// in `-1.0..=1.0`, written into `out` (cleared first, so its capacity is
/// kept rather than freed) instead of being returned as a fresh `Vec` --
/// see [`build_stream`]'s own doc on why the realtime callback must not
/// allocate one every block.
fn downmix_into<T>(data: &[T], channels: cpal::ChannelCount, out: &mut Vec<f32>)
where
    T: Sample,
    f32: FromSample<T>,
{
    let channels = channels.max(1) as usize;
    out.clear();
    out.extend(data.chunks(channels).map(|frame| {
        let sum: f32 = frame.iter().map(|s| f32::from_sample(*s)).sum();
        sum / frame.len() as f32
    }));
}

/// How many spare buffers [`build_stream`]'s pool starts with -- and the
/// most it will ever hold, since [`RAW_CHANNEL_CAPACITY`] is also the most
/// blocks that can be in flight (held by the audio channel, or briefly by
/// the capture thread before it is returned) at once.
const BUFFER_POOL_CAPACITY: usize = RAW_CHANNEL_CAPACITY;

/// Build and start an input stream on `device`, downmixing every callback
/// to mono and pushing it through `tx`. Shared by the microphone (an
/// ordinary input device) and the system track (an output device opened as
/// input -- the loopback trick this module's doc explains).
///
/// The realtime callback must not allocate (this module's own doc, "the
/// loopback trick" section notwithstanding -- see "Threads" instead), but
/// downmixing a block still needs somewhere to write its samples, and that
/// `Vec` has to move out of the callback whole -- ownership, not a
/// copy -- to reach the capture thread through `tx` without a second
/// allocation there. The two together mean a fresh buffer is needed every
/// callback unless *something* hands old ones back. That something is the
/// `SyncSender<Vec<f32>>` this returns alongside the stream: the capture
/// thread (`run`, or `record_mic_only`'s own loop) sends a buffer back
/// through it once done reading from it, the callback's own `Receiver`
/// half draws from that pool first and only allocates -- `unwrap_or_default`
/// -- on the rare block where the pool is empty (cold start, or a run of
/// blocks the capture thread has not caught up on yet). Steady state, once
/// the pool has warmed up, costs no allocation at all.
fn build_stream(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
    track: Track,
    tx: SyncSender<Event>,
) -> Result<(cpal::Stream, SyncSender<Vec<f32>>), CaptureError> {
    let channels = config.channels();
    let source_rate = config.sample_rate();
    let tx_err = tx.clone();
    let err_fn = move |e: cpal::Error| {
        let _ = tx_err.try_send(Event::Error { track, kind: e.kind(), message: e.to_string() });
    };

    let (free_tx, free_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(BUFFER_POOL_CAPACITY);

    macro_rules! build {
        ($t:ty) => {{
            let tx = tx.clone();
            let free_rx = free_rx;
            device.build_input_stream(
                config.clone().into(),
                move |data: &[$t], _: &cpal::InputCallbackInfo| {
                    let mut samples = free_rx.try_recv().unwrap_or_default();
                    downmix_into::<$t>(data, channels, &mut samples);
                    // Never blocks the realtime callback: a full channel
                    // means the capture thread is behind, and dropping this
                    // block is a missed fraction of a second, not a stall.
                    if let Err(TrySendError::Disconnected(_)) =
                        tx.try_send(Event::Audio { track, samples, source_rate })
                    {
                        // The capture thread is gone; nothing more to do.
                    }
                },
                err_fn,
                None,
            )
        }};
    }

    let stream = match config.sample_format() {
        SampleFormat::I8 => build!(i8),
        SampleFormat::I16 => build!(i16),
        SampleFormat::I32 => build!(i32),
        SampleFormat::F32 => build!(f32),
        other => return Err(CaptureError(format!("unsupported sample format {other}"))),
    }
    .map_err(|e| CaptureError(format!("could not open stream: {e}")))?;

    stream.play().map_err(|e| CaptureError(format!("could not start stream: {e}")))?;
    Ok((stream, free_tx))
}

fn open_mic(
    host: &cpal::Host,
    tx: SyncSender<Event>,
) -> Result<(cpal::Stream, SyncSender<Vec<f32>>), CaptureError> {
    let device =
        host.default_input_device().ok_or_else(|| CaptureError("no microphone found".into()))?;
    let config = device
        .default_input_config()
        .map_err(|e| CaptureError(format!("microphone config: {e}")))?;
    build_stream(&device, &config, Track::Mic, tx)
}

/// Record the microphone alone, for `seconds`, resampled to [`SAMPLE_RATE`]
/// mono exactly like a call's mic track -- what `meeting.rs`'s `voice_enrol`
/// command hands the service. No system track, no chunk framing, no sink: a
/// voiceprint sample is one short clip, not a call, and it must not pick up
/// whoever the microphone is not.
///
/// Blocking for the whole of `seconds`; call this off the async runtime the
/// way every other disk- or device-bound call in this crate is called.
pub fn record_mic_only(seconds: u32) -> Result<Vec<i16>, CaptureError> {
    let host = cpal::default_host();
    if host.default_input_device().is_none() {
        return Err(CaptureError("no microphone found".into()));
    }
    let (tx, rx) = std::sync::mpsc::sync_channel::<Event>(RAW_CHANNEL_CAPACITY);
    // A one-shot clip has no long steady state for the buffer pool to pay
    // off in, so its returned buffers are simply left unreturned -- see
    // `build_stream`'s own doc on the pool; running dry just means it falls
    // back to allocating, exactly as it did before the pool existed.
    let (stream, _returns) = open_mic(&host, tx)?;

    let mut resampler: Option<TrackResampler> = None;
    let mut out: Vec<i16> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(u64::from(seconds));
    let outcome: Result<(), CaptureError> = (|| {
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Event::Audio { track: Track::Mic, samples, source_rate }) => {
                    if resampler.is_none() {
                        resampler = Some(TrackResampler::new(source_rate)?);
                    }
                    let resampled = resampler.as_mut().expect("just set").push(&samples)?;
                    out.extend(resampled.into_iter().map(f32_to_i16));
                }
                Ok(Event::Audio { .. }) => {} // the system track, if somehow raised: ignored
                Ok(Event::Error { message, .. }) => return Err(CaptureError(message)),
                Ok(Event::Control(_)) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        Ok(())
    })();

    drop(stream);
    outcome?;
    if let Some(mut resampler) = resampler {
        let tail = resampler.flush()?;
        out.extend(tail.into_iter().map(f32_to_i16));
    }
    Ok(out)
}

/// Words a person can read, without a device model number in them, for why
/// the system track did not open. Platform-specific because the reason
/// (and the fix, if there is one) differs by desktop.
fn system_unavailable_reason(detail: &str) -> String {
    if cfg!(target_os = "linux") {
        format!("System audio needs PipeWire; recording your microphone only ({detail}).")
    } else if cfg!(target_os = "macos") {
        format!(
            "System audio needs macOS 14.6 or later; recording your microphone only ({detail})."
        )
    } else {
        format!("System audio could not be opened; recording your microphone only ({detail}).")
    }
}

/// The loopback trick, in one call: open the default *output* device as an
/// input. `supports_input()` is false for a plain output-only device on
/// most backends, so the config comes from the output side instead --
/// `cpal`'s WASAPI and Core Audio backends detect that this is an
/// output-direction device and enable their own loopback mode; PipeWire's
/// host exposes the device as duplex to begin with.
fn open_system(
    host: &cpal::Host,
    tx: SyncSender<Event>,
) -> Result<(cpal::Stream, SyncSender<Vec<f32>>), CaptureError> {
    let device = host
        .default_output_device()
        .ok_or_else(|| CaptureError(system_unavailable_reason("no default output device")))?;
    let config = if device.supports_input() {
        device.default_input_config()
    } else {
        device.default_output_config()
    }
    .map_err(|e| CaptureError(system_unavailable_reason(&e.to_string())))?;
    build_stream(&device, &config, Track::System, tx)
        .map_err(|e| CaptureError(system_unavailable_reason(&e.0)))
}

/// Whether a `cpal` error means the stream is dead and must be rebuilt, as
/// opposed to one it already recovered from on its own (`DeviceChanged`: a
/// default-device stream that just silently rerouted) or one worth noting
/// but not acting on (`Xrun`, `RealtimeDenied`).
fn needs_reopen(kind: cpal::ErrorKind) -> bool {
    matches!(
        kind,
        cpal::ErrorKind::DeviceNotAvailable
            | cpal::ErrorKind::StreamInvalidated
            | cpal::ErrorKind::HostUnavailable
            | cpal::ErrorKind::BackendError
    )
}

// ---- the capture thread ---------------------------------------------------

/// What a recording needs to know before capture starts: the record
/// `begin_recording` already created, so `append`/`finish` name the right
/// id and auto-stop can watch the right event.
pub struct RecordingHandle {
    pub id: RecordingId,
    pub title: String,
    pub automatic: bool,
    pub template_id: Option<TemplateId>,
    pub event_end: Option<Timestamp>,
}

/// A running (or just-stopped) capture, held by the shell for as long as a
/// recording is in progress. `stop` joins the capture thread, so the sink's
/// `finish`/`discard` has run and every chunk it could send has been sent
/// by the time it returns.
pub struct CaptureHandle {
    control: SyncSender<Event>,
    status: Arc<Mutex<Option<CaptureStatus>>>,
    join: Option<std::thread::JoinHandle<()>>,
    pub recording_id: RecordingId,
}

impl CaptureHandle {
    pub fn status(&self) -> Option<CaptureStatus> {
        self.status.lock().unwrap().clone()
    }

    /// Stop capture and join its thread, running the sink's `finish` (or
    /// `discard`) to completion first. Blocking -- call this off the async
    /// runtime, the way `everyday_service::service::blocking` does for
    /// every other disk- or network-bound command.
    pub fn stop(mut self, discard: bool) {
        let _ = self.control.send(Event::Control(ControlMsg::Stop { discard }));
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for CaptureHandle {
    /// A backstop for a `CaptureHandle` dropped without an explicit
    /// [`CaptureHandle::stop`] -- today, only reachable if a bug lets two
    /// starts race past `meeting.rs`'s "already recording" check and the
    /// second's `begin` overwrites the slot holding the first (the reason
    /// that check now reserves its slot *before* the await that begins a
    /// recording, rather than after -- see `meeting.rs`'s `begin_headless`).
    /// This exists behind that fix, not instead of it: belt and braces, so
    /// a capture thread is never simply orphaned, still holding the
    /// microphone and recording to a row nothing will ever call `finish` on,
    /// no matter how a `CaptureHandle` comes to be dropped.
    ///
    /// Sends the same stop signal `stop()` does, but does not block this
    /// thread to join it -- whoever is dropping this handle (an `Option`
    /// being overwritten, mid some other lock) is not expecting to wait for
    /// a capture thread it never asked to stop, and blocking a lock holder
    /// on an audio-thread join is its own way to wedge the app. The join
    /// handle is instead joined on a short-lived helper thread, purely so
    /// the OS thread does not leak; nothing waits on that helper either.
    /// Safe to run even after an ordinary `stop()` already did this same
    /// send-and-join: the channel's receiver is long gone by then, so the
    /// second send is a harmless, immediate `Err`, and `join` is already
    /// `None`.
    fn drop(&mut self) {
        let _ = self.control.send(Event::Control(ControlMsg::Stop { discard: false }));
        if let Some(join) = self.join.take() {
            let _ = std::thread::Builder::new().name("meeting-capture-drop-join".into()).spawn(
                move || {
                    let _ = join.join();
                },
            );
        }
    }
}

/// What the shell holds for the life of a recording -- `AppState::capture`
/// in `everyday-app`'s own state, or the equivalent in a window-free
/// harness (`meeting_e2e.rs`'s `Harness`). Not just `Option<CaptureHandle>`
/// any more: `Starting` is a placeholder reserved *before* the network
/// round trip `begin_recording` makes, so two starts racing each other
/// cannot both observe an empty slot, both proceed, and have the second
/// overwrite the first's `CaptureHandle` out from under it -- see
/// `meeting.rs`'s `begin_headless`, the only place this is constructed or
/// cleared.
pub enum CaptureSlot {
    /// Reserved: a `begin_recording` call (or the `capture::start` that
    /// follows it) is in flight, but no `CaptureHandle` exists yet. Nothing
    /// to stop and no status to report while a slot is in this state.
    Starting,
    /// Capture is under way (or just stopped, mid-`stop()`, on its way out
    /// of the slot).
    Recording(CaptureHandle),
}

impl CaptureSlot {
    /// The handle, if capture has actually started -- `None` for
    /// `Starting` as well as for an empty slot, so `meeting_status` and the
    /// "already recording" checks can treat "reserved" and "running" alike
    /// without matching on the variant themselves.
    pub fn handle(&self) -> Option<&CaptureHandle> {
        match self {
            CaptureSlot::Recording(handle) => Some(handle),
            CaptureSlot::Starting => None,
        }
    }
}

/// Start capturing. Checks that a microphone exists before doing anything
/// else -- the one failure worth reporting synchronously, since a machine
/// with literally no input device cannot record a call no matter what the
/// system track does. Everything past that (a permission refusal, a device
/// that vanishes moments after open) is reported through `status` and a
/// notification on `events`, because it can only be discovered on the
/// capture thread once streams are actually opened there.
///
/// `app` is `None` for a window-free caller -- an E2E harness driving this
/// module the same way `everyday-app`'s `meeting::begin` does, but with no
/// Tauri window to emit `STATUS_EVENT`/`STILL_ON_EVENT` to. `status` (and
/// `events`, already `Option`) are how such a caller still observes what is
/// happening.
///
/// `auto_stop_quiet`: `None` turns auto-stop off, as `auto_stop_enabled:
/// false` did before; `Some(quiet)` turns it on with `quiet` as how long
/// both tracks must stay silent past the event's end before acting --
/// [`AUTO_STOP_QUIET`] in production. Threaded through explicitly, rather
/// than read from that constant deep inside [`run`], so a test can shorten
/// it without touching the constant every real caller gets.
pub fn start(
    recording: RecordingHandle,
    sink: Box<dyn ChunkSink>,
    events: Option<Arc<dyn EventSink>>,
    app: Option<AppHandle>,
    auto_stop_quiet: Option<Duration>,
) -> Result<CaptureHandle, CaptureError> {
    let host = cpal::default_host();
    if host.default_input_device().is_none() {
        return Err(CaptureError("no microphone found".into()));
    }

    let (tx, rx) = std::sync::mpsc::sync_channel::<Event>(RAW_CHANNEL_CAPACITY);
    let status = Arc::new(Mutex::new(None));
    let recording_id = recording.id;
    let control = tx.clone();

    let status_for_thread = status.clone();
    let join = std::thread::Builder::new()
        .name("meeting-capture".into())
        .spawn(move || {
            run(recording, sink, tx, rx, status_for_thread, events, app, auto_stop_quiet)
        })
        .map_err(|e| CaptureError(format!("could not start the capture thread: {e}")))?;

    Ok(CaptureHandle { control, status, join: Some(join), recording_id })
}

struct TrackState {
    resampler: Option<TrackResampler>,
    framer: ChunkFramer,
    meter: LevelMeter,
    silence: SilenceTracker,
    source_rate: u32,
}

impl TrackState {
    fn new(track: Track) -> Self {
        Self {
            resampler: None,
            framer: ChunkFramer::new(track),
            meter: LevelMeter::new(),
            silence: SilenceTracker::default(),
            source_rate: 0,
        }
    }

    fn ingest(
        &mut self,
        samples: &[f32],
        source_rate: u32,
        elapsed_ms: u64,
        sink: &mut dyn ChunkSink,
    ) -> Result<(), CaptureError> {
        if self.resampler.is_none() || self.source_rate != source_rate {
            self.source_rate = source_rate;
            self.resampler = Some(TrackResampler::new(source_rate)?);
        }
        let resampled = self.resampler.as_mut().expect("just set").push(samples)?;
        self.consume(&resampled, elapsed_ms, sink)
    }

    fn consume(
        &mut self,
        resampled_f32: &[f32],
        elapsed_ms: u64,
        sink: &mut dyn ChunkSink,
    ) -> Result<(), CaptureError> {
        if resampled_f32.is_empty() {
            return Ok(());
        }
        let pcm: Vec<i16> = resampled_f32.iter().map(|&s| f32_to_i16(s)).collect();
        self.meter.push(&pcm);
        self.silence.update(self.meter.is_silent(), elapsed_ms);
        for chunk in self.framer.push(&pcm, elapsed_ms) {
            sink.append(chunk.track, chunk.seq, chunk.start_ms, chunk.samples)?;
        }
        Ok(())
    }

    fn flush(&mut self, elapsed_ms: u64, sink: &mut dyn ChunkSink) -> Result<(), CaptureError> {
        if let Some(resampler) = &mut self.resampler {
            let tail = resampler.flush()?;
            self.consume(&tail, elapsed_ms, sink)?;
        }
        if let Some(chunk) = self.framer.flush() {
            sink.append(chunk.track, chunk.seq, chunk.start_ms, chunk.samples)?;
        }
        Ok(())
    }
}

/// `app.emit`, guarded for the window-free case -- see [`start`]'s doc on
/// `app`. A no-op when there is no window to tell.
fn emit<T: serde::Serialize + Clone>(app: &Option<AppHandle>, name: &str, payload: T) {
    if let Some(app) = app {
        let _ = app.emit(name, payload);
    }
}

/// How many times [`run`]'s stop path retries `sink.finish()` before
/// leaving the recording for the service's own stale-recording recovery to
/// pick up -- see the comment where this is called.
const FINISH_RETRIES: u32 = 4;

/// Retry `sink.finish()` a few times with growing backoff before giving up.
/// Mirrors `RecordingSink::send_with_retry`'s own shape (and, for a
/// `RecordingSink`, sits on top of it: `finish` calls `drain_pending`,
/// which already retries each chunk) but one level up, for the "finish"
/// call itself -- a vault that is briefly unreachable (a remote session's
/// connection stuttering right as the person hangs up) should not cost the
/// whole recording when every chunk might land fine a few seconds later.
///
/// Blocking is fine here: this runs on the capture thread, at the very end
/// of its life, after every chunk has already been sent or given up on.
fn finish_with_retry(sink: &mut dyn ChunkSink) -> Result<(), CaptureError> {
    finish_with_retry_from(sink, Duration::from_millis(500))
}

/// [`finish_with_retry`], with the starting backoff broken out so a test
/// can shrink it without a real recording ever needing to.
fn finish_with_retry_from(
    sink: &mut dyn ChunkSink,
    mut delay: Duration,
) -> Result<(), CaptureError> {
    let mut last_err = None;
    for attempt in 0..FINISH_RETRIES {
        if attempt > 0 {
            std::thread::sleep(delay);
            delay = (delay * 3).min(Duration::from_secs(10));
        }
        match sink.finish() {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.expect("looped at least once"))
}

#[allow(clippy::too_many_arguments)]
fn run(
    recording: RecordingHandle,
    mut sink: Box<dyn ChunkSink>,
    tx: SyncSender<Event>,
    rx: Receiver<Event>,
    status: Arc<Mutex<Option<CaptureStatus>>>,
    events: Option<Arc<dyn EventSink>>,
    app: Option<AppHandle>,
    auto_stop_quiet: Option<Duration>,
) {
    let started_at = Instant::now();
    let elapsed_ms = || started_at.elapsed().as_millis() as u64;

    if let Err(e) = sink.begin() {
        tracing::warn!(error = %e, "meeting capture: sink refused to begin");
        emit(&app, STATUS_EVENT, Option::<CaptureStatus>::None);
        return;
    }

    let host = cpal::default_host();
    tracing::info!(host = %host.id().name(), "meeting capture: starting");

    let (mic_stream, mic_returns) = match open_mic(&host, tx.clone()) {
        Ok((s, returns)) => (Some(s), Some(returns)),
        Err(e) => {
            tracing::warn!(error = %e, "meeting capture: could not open the microphone");
            let _ = sink.discard();
            emit(&app, STATUS_EVENT, Option::<CaptureStatus>::None);
            return;
        }
    };

    let mut system_unavailable = None;
    let (system_stream, system_returns) = match open_system(&host, tx.clone()) {
        Ok((s, returns)) => (Some(s), Some(returns)),
        Err(e) => {
            tracing::info!(error = %e, "meeting capture: system audio unavailable, mic only");
            system_unavailable = Some(e.0);
            (None, None)
        }
    };
    let mut mic_stream = mic_stream;
    let mut system_stream = system_stream;
    let mut mic_returns = mic_returns;
    let mut system_returns = system_returns;

    let mut mic = TrackState::new(Track::Mic);
    let mut system = TrackState::new(Track::System);
    let mut stopped_reason: Option<String> = None;
    let mut discard_on_stop = false;
    let mut last_status_emit = Instant::now() - STATUS_INTERVAL;
    let mut last_system_retry = Instant::now();

    'outer: loop {
        match rx.recv_timeout(STATUS_INTERVAL) {
            Ok(Event::Audio { track, samples, source_rate }) => {
                let (state, returns) = match track {
                    Track::Mic => (&mut mic, &mic_returns),
                    Track::System => (&mut system, &system_returns),
                };
                let result = state.ingest(&samples, source_rate, elapsed_ms(), sink.as_mut());
                // Hand the buffer back to `build_stream`'s pool now that
                // this thread is done reading it -- see that function's own
                // doc. Best-effort: a full pool (the callback outrunning
                // this thread) just means the buffer is dropped instead of
                // reused, not lost audio.
                if let Some(returns) = returns {
                    let _ = returns.try_send(samples);
                }
                if let Err(e) = result {
                    stopped_reason = Some(e.0);
                    break 'outer;
                }
            }
            Ok(Event::Error { track, kind, message }) => {
                tracing::warn!(?track, ?kind, %message, "meeting capture: stream error");
                if needs_reopen(kind) {
                    match track {
                        Track::Mic => {
                            mic_stream = None;
                            // Not reset to `None` here the way `system_returns`
                            // is below: this arm either replaces it with a
                            // fresh pool on success or breaks `'outer`
                            // immediately on failure (a dead microphone ends
                            // the recording), so there is no window where a
                            // stale sender could be read.
                            match open_mic(&host, tx.clone()) {
                                Ok((s, returns)) => {
                                    mic_stream = Some(s);
                                    mic_returns = Some(returns);
                                }
                                Err(e) => {
                                    stopped_reason = Some(e.0);
                                    break 'outer;
                                }
                            }
                        }
                        Track::System => {
                            system_stream = None;
                            system_returns = None;
                            match open_system(&host, tx.clone()) {
                                Ok((s, returns)) => {
                                    system_stream = Some(s);
                                    system_returns = Some(returns);
                                }
                                Err(e) => system_unavailable = Some(e.0),
                            }
                        }
                    }
                }
            }
            Ok(Event::Control(ControlMsg::Stop { discard })) => {
                discard_on_stop = discard;
                break 'outer;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Nothing arrived this tick. On a backend that stops
                // calling the data callback while idle (WASAPI, notably --
                // see this module's doc), this is what keeps the loop
                // (and `meeting-status`) alive regardless.
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break 'outer,
        }

        // The system track failed to open at all (no `Event::Error` will
        // ever arrive for a stream that never existed) or dropped out and
        // could not be reopened above -- either way, retry it slowly in the
        // background rather than leaving the call mic-only until it is
        // restarted by hand. See `SYSTEM_RETRY_INTERVAL`'s own doc.
        if system_stream.is_none() && last_system_retry.elapsed() >= SYSTEM_RETRY_INTERVAL {
            last_system_retry = Instant::now();
            match open_system(&host, tx.clone()) {
                Ok((s, returns)) => {
                    tracing::info!("meeting capture: system audio became available; reopened");
                    system_stream = Some(s);
                    system_returns = Some(returns);
                    system_unavailable = None;
                }
                Err(e) => system_unavailable = Some(e.0),
            }
        }

        if last_status_emit.elapsed() >= STATUS_INTERVAL {
            last_status_emit = Instant::now();
            let now_ms = elapsed_ms();
            let now = Timestamp::now();
            let mic_silent = mic.silence.silent_since_ms.map_or(0, |s| now_ms.saturating_sub(s));
            let system_silent =
                system.silence.silent_since_ms.map_or(0, |s| now_ms.saturating_sub(s));
            let auto_stop_at_value = match auto_stop_quiet {
                Some(quiet) => {
                    auto_stop_at(recording.event_end, now, mic_silent, system_silent, quiet)
                }
                None => None,
            };
            let current = CaptureStatus {
                recording_id: recording.id,
                title: recording.title.clone(),
                automatic: recording.automatic,
                elapsed_ms: now_ms,
                mic_level: mic.meter.level(),
                system_level: system.meter.level(),
                system_silent_ms: system_silent,
                system_unavailable: system_unavailable.clone(),
                auto_stop_at: auto_stop_at_value,
            };
            emit(&app, STATUS_EVENT, &current);
            *status.lock().unwrap() = Some(current);

            if let Some(quiet) = auto_stop_quiet {
                match auto_stop_decision(recording.event_end, now, mic_silent, system_silent, quiet)
                {
                    AutoStopDecision::Stop => break 'outer,
                    AutoStopDecision::AskToContinue => {
                        emit(&app, STILL_ON_EVENT, recording.id);
                    }
                    AutoStopDecision::Continue => {}
                }
            }
        }
    }

    // Streams are dropped here, on the thread that opened them -- see this
    // module's doc on why they never leave it.
    drop(mic_stream);
    drop(system_stream);

    let now_ms = elapsed_ms();
    if let Some(reason) = &stopped_reason {
        tracing::warn!(error = %reason, "meeting capture: stopped");
        if let Some(events) = &events {
            events.notify(
                everyday_service::events::Notification::warning("Recording stopped")
                    .body(reason.clone())
                    .for_user(),
            );
        }
        let _ = sink.discard();
    } else {
        let flushed =
            mic.flush(now_ms, sink.as_mut()).and_then(|()| system.flush(now_ms, sink.as_mut()));
        let outcome = if discard_on_stop {
            sink.discard()
        } else {
            match flushed {
                Ok(()) => finish_with_retry(sink.as_mut()),
                Err(e) => {
                    tracing::warn!(error = %e, "meeting capture: could not flush the last chunk");
                    sink.discard()
                }
            }
        };
        // `discard_on_stop`'s own `sink.discard()` above is never retried
        // past whatever `RecordingSink` itself already does (a person who
        // asked to throw a recording away should not be kept waiting on a
        // vault that has gone quiet); only the ordinary "finish" path below
        // gets the extra backoff, because unlike a discard it has real
        // audio worth waiting for.
        if let Err(e) = outcome {
            // `finish_with_retry` exhausted its attempts (or the flush
            // itself failed, in which case `sink.discard()` ran instead and
            // this is *its* error). Either way, nothing here deletes the
            // spool: every chunk `RecordingSink::append` already sent has
            // landed on the vault regardless of what `finish` does next,
            // and the sink is simply dropped, not asked to discard. The
            // service is left holding a row still in `Stage::Recording`
            // with no live capture behind it -- exactly what
            // `everyday_service::meeting::spool`'s own recovery already
            // exists to notice: once `RECOVERY_STALE` (two minutes) passes
            // with no further chunk appended, `begin`'s own reclaiming pass
            // (or the periodic sweep at the next unlock) moves it on to
            // `Stage::Transcribing` from wherever its last chunk left off,
            // so a fresh recording is never blocked on this for more than
            // that. The notification below says as much, so the person
            // sees the audio as saved rather than lost.
            tracing::warn!(
                error = %e,
                "meeting capture: could not finish the recording after retrying; it will be \
                 recovered automatically"
            );
            if let Some(events) = &events {
                events.notify(
                    everyday_service::events::Notification::warning(
                        "Recording saved but not finished",
                    )
                    .body("It will be finished automatically once the vault is reachable again.")
                    .for_user(),
                );
            }
        }
    }

    *status.lock().unwrap() = None;
    emit(&app, STATUS_EVENT, Option::<CaptureStatus>::None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(n: usize, value: i16) -> Vec<i16> {
        vec![value; n]
    }

    // ---- ChunkFramer ---------------------------------------------------

    #[test]
    fn frames_full_chunks_with_increasing_seq() {
        let mut framer = ChunkFramer::new(Track::Mic);
        let mut chunks = framer.push(&samples(CHUNK_SAMPLES, 100), 0);
        assert_eq!(chunks.len(), 1);
        let first = chunks.remove(0);
        assert_eq!(first.seq, 0);
        assert_eq!(first.start_ms, 0);
        assert_eq!(first.samples.len(), CHUNK_SAMPLES);

        let mut chunks = framer.push(&samples(CHUNK_SAMPLES, 200), 30_000);
        let second = chunks.remove(0);
        assert_eq!(second.seq, 1);
        assert_eq!(second.start_ms, 30_000);
    }

    #[test]
    fn flushes_a_final_partial_chunk_on_stop() {
        let mut framer = ChunkFramer::new(Track::System);
        assert!(framer.push(&samples(100, 1), 0).is_empty());
        let flushed = framer.flush().expect("partial chunk");
        assert_eq!(flushed.seq, 0);
        assert_eq!(flushed.samples.len(), 100);
        assert!(framer.flush().is_none(), "nothing left to flush twice");
    }

    #[test]
    fn reanchors_start_ms_to_wall_clock_at_each_chunk_start() {
        // Feed a chunk's worth of samples but claim, via `now_ms`, that
        // real time took longer than the sample count alone would imply --
        // the way a device that briefly starved the buffer would. The next
        // chunk's `start_ms` must reflect that, not `seq * CHUNK_SECONDS *
        // 1000`.
        let mut framer = ChunkFramer::new(Track::Mic);
        framer.push(&samples(CHUNK_SAMPLES, 1), 0);
        let mut chunks = framer.push(&samples(10, 1), 31_500);
        let partial_start = chunks.pop().map(|_| ()).is_none(); // no full chunk yet
        assert!(partial_start);
        let flushed = framer.flush().unwrap();
        assert_eq!(flushed.start_ms, 31_500, "re-anchored to wall clock, not 30_000");
    }

    #[test]
    fn a_gap_shows_up_as_a_jump_in_the_next_starts_ms() {
        let mut framer = ChunkFramer::new(Track::System);
        framer.push(&samples(CHUNK_SAMPLES, 1), 0);
        // The stream was down for a reopen between chunks; audio resumes
        // well after 30_000ms would have suggested.
        let mut chunks = framer.push(&samples(CHUNK_SAMPLES, 1), 45_000);
        let second = chunks.remove(0);
        assert_eq!(second.start_ms, 45_000);
    }

    // ---- resampling ------------------------------------------------------

    fn sine(rate: u32, seconds: f32, freq: f32, amplitude: f32) -> Vec<f32> {
        let n = (rate as f32 * seconds) as usize;
        (0..n)
            .map(|i| amplitude * (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin())
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn resamples_48k_mono_to_16k() {
        let input = sine(48_000, 1.0, 440.0, 0.5);
        let mut resampler = TrackResampler::new(48_000).unwrap();
        let mut out = resampler.push(&input).unwrap();
        out.extend(resampler.flush().unwrap());
        // 48kHz -> 16kHz is exactly 3:1; within a chunk or two of a second
        // of input, one second of output at the new rate.
        let expected = SAMPLE_RATE as usize;
        assert!(
            out.len().abs_diff(expected) < 2000,
            "expected roughly {expected} samples, got {}",
            out.len()
        );
        // The tone survives the trip: not silence, not clipped away.
        assert!(rms(&out) > 0.1, "resampled tone should not be near-silent, rms={}", rms(&out));
    }

    #[test]
    fn resamples_44_1k_to_16k() {
        let input = sine(44_100, 0.5, 440.0, 0.5);
        let mut resampler = TrackResampler::new(44_100).unwrap();
        let mut out = resampler.push(&input).unwrap();
        out.extend(resampler.flush().unwrap());
        let expected = (SAMPLE_RATE as f32 * 0.5) as usize;
        assert!(out.len().abs_diff(expected) < 2000);
    }

    #[test]
    fn downmixes_stereo_to_mono() {
        // Left full-scale, right silent: mono should land at half.
        let interleaved: Vec<f32> = (0..200).flat_map(|_| [1.0f32, 0.0f32]).collect();
        let mut mono = Vec::new();
        downmix_into::<f32>(&interleaved, 2, &mut mono);
        assert_eq!(mono.len(), 200);
        assert!((mono[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn downmix_into_reuses_the_buffer_it_is_given_rather_than_growing_it_unboundedly() {
        // The whole point of `downmix_into` over the old `downmix` is that
        // a buffer handed back for reuse keeps its capacity -- `clear`, not
        // a fresh `Vec` -- so a steady stream of same-sized blocks settles
        // into zero further allocation. Prove that directly: capacity after
        // a second, same-sized call must not have grown past the first.
        let interleaved: Vec<f32> = (0..200).flat_map(|_| [1.0f32, 0.0f32]).collect();
        let mut buf = Vec::new();
        downmix_into::<f32>(&interleaved, 2, &mut buf);
        let capacity_after_first = buf.capacity();
        downmix_into::<f32>(&interleaved, 2, &mut buf);
        assert_eq!(buf.capacity(), capacity_after_first, "same-sized block must not reallocate");
        assert_eq!(buf.len(), 200);
    }

    // ---- level meter / silence -------------------------------------------

    #[test]
    fn level_meter_reports_silence_and_signal() {
        let mut meter = LevelMeter::new();
        meter.push(&samples(1000, 0));
        assert!(meter.is_silent());
        meter.push(&samples(1000, i16::MAX));
        assert!(!meter.is_silent());
        assert!(meter.level() > SILENCE_RMS);
    }

    #[test]
    fn silence_tracker_measures_continuous_quiet() {
        let mut tracker = SilenceTracker::default();
        assert_eq!(tracker.update(true, 1_000), 0);
        assert_eq!(tracker.update(true, 5_000), 4_000);
        assert_eq!(tracker.update(false, 6_000), 0, "a live sample resets it");
        assert_eq!(tracker.update(true, 6_500), 0, "silence starts fresh again");
    }

    // ---- auto-stop ---------------------------------------------------------

    fn ts(seconds_from_epoch: i64) -> Timestamp {
        Timestamp::from_second(seconds_from_epoch).unwrap()
    }

    #[test]
    fn auto_stop_never_fires_before_the_event_ends() {
        let end = ts(1000);
        let decision = auto_stop_decision(Some(end), ts(999), 999_999, 999_999, AUTO_STOP_QUIET);
        assert_eq!(decision, AutoStopDecision::Continue);
    }

    #[test]
    fn auto_stop_waits_for_both_tracks_to_be_quiet() {
        let end = ts(1000);
        // Past the end, but only one track has been quiet long enough.
        let decision = auto_stop_decision(Some(end), ts(1130), 130_000, 10_000, AUTO_STOP_QUIET);
        assert_eq!(decision, AutoStopDecision::Continue);
    }

    #[test]
    fn auto_stop_fires_after_two_minutes_of_mutual_quiet() {
        let end = ts(1000);
        let decision = auto_stop_decision(Some(end), ts(1130), 125_000, 121_000, AUTO_STOP_QUIET);
        assert_eq!(decision, AutoStopDecision::Stop);
    }

    #[test]
    fn auto_stop_asks_after_thirty_minutes_if_still_going() {
        let end = ts(1000);
        let decision = auto_stop_decision(Some(end), ts(1000 + 1800), 0, 0, AUTO_STOP_QUIET);
        assert_eq!(decision, AutoStopDecision::AskToContinue);
    }

    #[test]
    fn auto_stop_does_nothing_without_an_event() {
        let decision = auto_stop_decision(None, ts(100_000), 999_999, 999_999, AUTO_STOP_QUIET);
        assert_eq!(decision, AutoStopDecision::Continue);
    }

    #[test]
    fn auto_stop_at_is_armed_only_once_both_tracks_are_currently_silent() {
        let end = ts(1000);
        assert_eq!(
            auto_stop_at(Some(end), ts(1010), 0, 5_000, AUTO_STOP_QUIET),
            None,
            "mic is currently live"
        );
        let armed = auto_stop_at(Some(end), ts(1010), 5_000, 5_000, AUTO_STOP_QUIET);
        assert!(armed.is_some());
    }

    // ---- mic-only fallback --------------------------------------------------

    #[test]
    fn system_unavailable_reason_names_the_platform() {
        let reason = system_unavailable_reason("no default output device");
        assert!(reason.contains("microphone only"));
        #[cfg(target_os = "linux")]
        assert!(reason.contains("PipeWire"));
    }

    // ---- ChunkSink implementations -----------------------------------------

    struct VecSink {
        chunks: Vec<(Track, u32, u64, usize)>,
        finished: bool,
        discarded: bool,
    }

    impl VecSink {
        fn new() -> Self {
            Self { chunks: Vec::new(), finished: false, discarded: false }
        }
    }

    impl ChunkSink for VecSink {
        fn begin(&mut self) -> Result<(), CaptureError> {
            Ok(())
        }
        fn append(
            &mut self,
            track: Track,
            seq: u32,
            start_ms: u64,
            samples: Vec<i16>,
        ) -> Result<(), CaptureError> {
            self.chunks.push((track, seq, start_ms, samples.len()));
            Ok(())
        }
        fn finish(&mut self) -> Result<(), CaptureError> {
            self.finished = true;
            Ok(())
        }
        fn discard(&mut self) -> Result<(), CaptureError> {
            self.discarded = true;
            Ok(())
        }
    }

    #[test]
    fn track_state_ingest_frames_and_forwards_to_the_sink() {
        let mut state = TrackState::new(Track::Mic);
        let mut sink = VecSink::new();
        let one_chunk_of_source_audio = sine(48_000, 30.5, 440.0, 0.5);
        state.ingest(&one_chunk_of_source_audio, 48_000, 0, &mut sink).unwrap();
        state.flush(31_000, &mut sink).unwrap();
        assert!(!sink.chunks.is_empty());
        assert_eq!(sink.chunks[0].0, Track::Mic);
        assert_eq!(sink.chunks[0].1, 0);
        // The chunker's own tests cover exact sizing; here just confirm the
        // seam moves data through.
        assert!(sink.chunks.iter().map(|c| c.3).sum::<usize>() > 0);
    }

    #[test]
    fn file_sink_writes_and_can_be_discarded() {
        let dir =
            std::env::temp_dir().join(format!("everyday-capture-test-{}", std::process::id()));
        let mut sink = FileSink::new(&dir);
        sink.begin().unwrap();
        sink.append(Track::Mic, 0, 0, vec![0; 1600]).unwrap();
        sink.finish().unwrap();
        assert!(dir.join("mic.wav").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- finish_with_retry --------------------------------------------------

    /// A sink whose `finish` fails `fail_times` times before succeeding --
    /// standing in for `RecordingSink::finish` when the vault (or the final
    /// `finish_recording` round trip specifically) is briefly unreachable.
    struct FlakyFinishSink {
        fail_times: u32,
        attempts: u32,
    }

    impl ChunkSink for FlakyFinishSink {
        fn begin(&mut self) -> Result<(), CaptureError> {
            Ok(())
        }
        fn append(
            &mut self,
            _track: Track,
            _seq: u32,
            _start_ms: u64,
            _samples: Vec<i16>,
        ) -> Result<(), CaptureError> {
            Ok(())
        }
        fn finish(&mut self) -> Result<(), CaptureError> {
            self.attempts += 1;
            if self.attempts <= self.fail_times {
                Err(CaptureError("vault unreachable".into()))
            } else {
                Ok(())
            }
        }
        fn discard(&mut self) -> Result<(), CaptureError> {
            Ok(())
        }
    }

    #[test]
    fn finish_with_retry_succeeds_once_the_transient_failure_clears() {
        let mut sink = FlakyFinishSink { fail_times: 2, attempts: 0 };
        finish_with_retry_from(&mut sink, Duration::from_millis(1)).unwrap();
        assert_eq!(sink.attempts, 3, "two failures, then the try that finally lands");
    }

    #[test]
    fn finish_with_retry_gives_up_after_its_own_cap_without_discarding() {
        // A sink that never recovers: `finish_with_retry` must stop after
        // `FINISH_RETRIES` attempts (not loop forever) and hand the error
        // back rather than silently swallowing it -- `run`'s caller is what
        // decides not to discard on this error, but this proves the retry
        // loop itself is bounded.
        let mut sink = FlakyFinishSink { fail_times: u32::MAX, attempts: 0 };
        let err = finish_with_retry_from(&mut sink, Duration::from_millis(1)).unwrap_err();
        assert_eq!(sink.attempts, FINISH_RETRIES);
        assert!(err.0.contains("vault unreachable"));
    }

    // ---- RecordingSink retry/backlog ---------------------------------------

    #[test]
    fn recording_sink_retries_then_succeeds() {
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let attempts_for_closure = attempts.clone();
        let call = Box::new(move |name: &str, _args: Value| -> CommandResult<Value> {
            assert_eq!(name, "append_recording_chunk");
            let n = attempts_for_closure.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n < 2 { Err(CommandError::new("transient", "not yet")) } else { Ok(Value::Null) }
        });
        let mut sink = RecordingSink::new(RecordingId::new(), call);
        sink.append(Track::Mic, 0, 0, vec![1, 2, 3]).unwrap();
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert!(sink.pending.is_empty());
    }

    #[test]
    fn recording_sink_buffers_when_the_sink_cannot_take_a_chunk() {
        let call = Box::new(|_: &str, _: Value| -> CommandResult<Value> {
            Err(CommandError::new("network", "unreachable"))
        });
        let mut sink = RecordingSink::new(RecordingId::new(), call);
        // One 30s chunk of silence at 16kHz: below the 10-minute cap on its
        // own, so this should buffer rather than error.
        let one_chunk = vec![0i16; CHUNK_SAMPLES];
        sink.append(Track::Mic, 0, 0, one_chunk).unwrap();
        assert_eq!(sink.pending.len(), 1);
    }

    #[test]
    fn recording_sink_gives_up_past_ten_minutes_of_backlog() {
        let call = Box::new(|_: &str, _: Value| -> CommandResult<Value> {
            Err(CommandError::new("network", "unreachable"))
        });
        let mut sink = RecordingSink::new(RecordingId::new(), call);
        // Five-minute "chunks" -- unrealistic on their own, but all this
        // sink cares about is total duration, and a couple of large ones
        // reach the ten-minute cap in far fewer retry-storms than real 30s
        // chunks would need.
        let five_minutes = vec![0i16; SAMPLE_RATE as usize * 5 * 60];
        let mut last = Ok(());
        for seq in 0..3 {
            // 3 * 5min = 15min, past the 10-minute cap.
            last = sink.append(Track::Mic, seq, seq as u64 * 300_000, five_minutes.clone());
            if last.is_err() {
                break;
            }
        }
        assert!(last.is_err(), "should give up once the backlog passes ten minutes");
    }

    // ---- real hardware ------------------------------------------------------

    /// Opens the real loopback device on this machine and asserts it hears a
    /// tone `pw-play` is asked to play to the default sink. Not run by
    /// default -- CI, and most dev boxes, have no speakers and no PipeWire
    /// session to speak of -- but the one test in this module that exercises
    /// the actual claim `docs/plans/meeting-notes.md` makes, rather than a
    /// synthetic sine wave. See `examples/capture.rs`'s module doc for the
    /// findings this was first verified against, including the one quirk it
    /// ran into: a machine with no real output device (a "null"/"dummy"
    /// fallback sink) can leave `pw-play`'s *default*-target resolution
    /// pointed somewhere its monitor is not, which reads as loopback being
    /// broken when it is really an artifact of that machine having no
    /// speakers at all. A real desktop's actual default sink does not have
    /// this problem.
    ///
    /// Run with `EVERYDAY_TEST_AUDIO=1 cargo test -p everyday-app --lib -- \
    /// --ignored real_loopback_hears_a_played_tone`.
    #[test]
    #[ignore = "needs real audio hardware and a PipeWire session"]
    fn real_loopback_hears_a_played_tone() {
        if std::env::var("EVERYDAY_TEST_AUDIO").as_deref() != Ok("1") {
            eprintln!("skipped: set EVERYDAY_TEST_AUDIO=1 to run this against real hardware");
            return;
        }

        let tone_path = std::env::temp_dir().join("everyday-capture-test-tone.wav");
        {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: 44_100,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer = hound::WavWriter::create(&tone_path, spec).unwrap();
            // Two seconds, quiet on purpose -- see this crate's environment
            // rules on keeping test tones short and quiet.
            for s in sine(44_100, 2.0, 440.0, 0.15) {
                writer.write_sample(f32_to_i16(s)).unwrap();
            }
            writer.finalize().unwrap();
        }

        let host = cpal::default_host();
        let (tx, rx) = std::sync::mpsc::sync_channel(RAW_CHANNEL_CAPACITY);
        let (stream, _returns) =
            open_system(&host, tx).expect("the system track should open on a real desktop");

        let mut player = std::process::Command::new("pw-play")
            .arg(&tone_path)
            .spawn()
            .expect("pw-play should be on PATH");

        let mut resampler: Option<TrackResampler> = None;
        let mut heard = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Event::Audio { samples, source_rate, .. }) => {
                    let resampler =
                        resampler.get_or_insert_with(|| TrackResampler::new(source_rate).unwrap());
                    heard.extend(resampler.push(&samples).unwrap());
                }
                Ok(Event::Error { message, .. }) => panic!("stream error: {message}"),
                _ => {}
            }
        }
        drop(stream);
        let _ = player.wait();
        let _ = std::fs::remove_file(&tone_path);

        assert!(!heard.is_empty(), "should have heard something at all");
        assert!(
            rms(&heard) > SILENCE_RMS,
            "system track should not be silent while a tone plays, rms={}",
            rms(&heard)
        );
    }
}
