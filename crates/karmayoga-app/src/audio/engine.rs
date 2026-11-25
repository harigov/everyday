//! A microphone, a speaker, or both, for whatever talks and listens.
//!
//! One thread, "audio-engine", opens the default input and output devices at
//! their default configurations -- only the ones asked for -- plays and
//! records through them and drops them, as `capture.rs` does and for the same
//! reasons: `cpal::Stream` is not reliably `Send`, and some backends are
//! thread-affine. Everything else here is plain data moving through bounded
//! channels. The engine knows nothing of what it is used for: it is given
//! speech to play, and calls a sink with what the microphone heard and what
//! finished playing.
//!
//! ```text
//!  microphone callback ──▶ Event::Mic ────┐
//!  speaker callback ─────▶ Event::Reference ─┤   engine thread
//!       ▲  (what it just wrote)            ├─▶ resample ─▶ cleaner ─▶ EngineEvent::Microphone
//!       │                                  │
//!   PlaybackQueue ◀── Control::Play ◀──────┘ (resampled to the device's rate here)
//! ```
//!
//! The microphone is always cleaned of rumble, hum and steady hiss
//! ([`Cleaning::Noise`]). The reference, and cancelling echo against it
//! ([`Cleaning::Echo`]), exist only when both devices are open and echo
//! cancellation was asked for.
//!
//! # Other voices and stronger noise
//!
//! What listens for the person can ask for more ([`Cleanup`]): another
//! person's voice taken out, or a stronger noise filter, both models from
//! `karmayoga_service::speech::cleanup`. They run on a thread of their own,
//! "mic-cleanup", after everything above, so that a model taking its time
//! never holds up the echo canceller, which has to see the reference and the
//! microphone in step:
//!
//! ```text
//!  cleaner ─▶ batch ─▶ (bounded) ─▶ mic-cleanup: models ─▶ EngineEvent::Microphone
//! ```
//!
//! Loading them takes a second or two; what the microphone hears meanwhile
//! waits in the channel and is cleaned when they are ready, so the first
//! words are late, not lost. A computer that cannot keep up with them gets
//! them turned off for the rest of that microphone, audio passed on as the
//! ordinary filter left it, and Settings says so ([`cleanup_trouble`]).
//!
//! The noise filter takes the canceller's own noise suppression's place, so
//! it is loaded first, on this thread, a tenth of a second: the suppression
//! stands aside only for a filter that is there, and comes back if the models
//! are turned off. Each model is loaded on its own, so one that fails leaves
//! the other.
//!
//! [`AudioEngine::finish`] waits for the models to hand over everything they
//! hold, for something that wants the last words; [`AudioEngine::stop`] and
//! dropping the engine tell their thread to stop at once.
//!
//! # Realtime rules
//!
//! The callbacks neither block nor allocate in steady state. They talk to the
//! engine thread through a bounded channel with `try_send` (a full channel
//! drops a block, which is a click, not a stall) and recycle their buffers
//! through a pool the engine thread refills -- see `input.rs`. The playback
//! queue is shared behind a mutex the callback only `try_lock`s: if the engine
//! thread happens to hold it, that callback plays silence, and the silence is
//! what is reported as the reference, since it is what was played.
//!
//! # Echo
//!
//! Whatever the output callback wrote, silence included, is copied out
//! downmixed, brought to 16 kHz and handed to the canceller as the reference
//! for the microphone. See `echo.rs`. Silence is not all it can be: the working
//! sound and the listening sound are mixed into the block before it is
//! copied, so the canceller hears them as it hears speech, and a chime the
//! microphone picks up is taken out of it like any other echo.
//!
//! # Failures
//!
//! A device that goes away or breaks mid-session is not reopened: the sink is
//! told which one it was, in [`EngineEvent::Failed`], and the engine closes
//! both. A new one is one press of the microphone button away.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, SupportedStreamConfig};
use karmayoga_service::speech::SAMPLE_RATE;
use karmayoga_service::speech::cleanup::{Cleanup, MicCleanup, NoiseFilter, VoiceIsolation};
use karmayoga_service::voice::cue::{Bed, listening_chime};

use super::AudioError;
use super::echo::{Canceller, Cleaning};
use super::input;
use super::playback::PlaybackQueue;
use super::resample::{self, StreamResampler};

/// Blocks in flight between a callback and the engine thread, per stream and
/// the pool each draws from. Dropping the odd one is a click.
const CHANNEL_CAPACITY: usize = 64;

/// How microphone audio is batched for the sink: about 30 ms.
const BATCH_SAMPLES: usize = (SAMPLE_RATE as usize * 30) / 1000;

/// How long the engine thread waits for audio before looking at its control
/// queue again. Pause and Clear take effect within this.
const POLL: Duration = Duration::from_millis(5);

/// Most events handled in one turn of the loop, so a backlog cannot starve
/// the control queue.
const MAX_EVENTS_PER_TURN: usize = 32;

/// How often the echo delay hint is recomputed from the device latencies.
const DELAY_REFRESH: Duration = Duration::from_millis(500);

/// The delay hint when the devices say nothing about their latency. The
/// canceller finds the remainder itself; this only has to start it near.
const FALLBACK_DELAY_MS: i32 = 60;

/// How long something that wants the last words waits for them in
/// [`AudioEngine::finish`]: the models' whole backlog, worked off at twice
/// real time or more, and some.
pub const FINISH_WITHIN: Duration = Duration::from_secs(6);

/// Microphone batches waiting for the models (about 30 ms each): room for
/// them to load and prime, about two seconds, and some.
const CLEANUP_BACKLOG: usize = 256;

/// How much audio the models' pace is judged over, and the share of real
/// time past which they are falling behind: see [`Pace`].
const PACE_WINDOW: Duration = Duration::from_secs(10);
const PACE_LIMIT: f64 = 0.9;

/// Why the models were last turned off part-way, for Settings: see the
/// module doc.
static CLEANUP_TROUBLE: Mutex<Option<String>> = Mutex::new(None);

/// Why the microphone's models were last turned off part-way, if they were.
pub fn cleanup_trouble() -> Option<String> {
    CLEANUP_TROUBLE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Forget it: something about them has changed.
pub fn clear_cleanup_trouble() {
    *CLEANUP_TROUBLE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn cleanup_failed(why: String) {
    tracing::warn!(%why, "audio: the microphone's models are off");
    *CLEANUP_TROUBLE.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
}

/// How much of a latency a callback reports is believed. Anything longer is a
/// backend guessing, and a wrong hint is worse than the fallback.
const MAX_LATENCY_US: u128 = 500_000;

/// Which devices an engine opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineOptions {
    /// Listen: the default microphone, as [`EngineEvent::Microphone`].
    pub microphone: bool,
    /// Play: the default speaker, for [`AudioEngine::play`].
    pub speaker: bool,
    /// Take what the speaker played out of what the microphone hears. It
    /// needs both devices, and does nothing without them.
    pub echo_cancellation: bool,
}

impl EngineOptions {
    /// Whether the canceller runs: it has nothing to cancel, or nothing to
    /// cancel it from, unless both devices are open.
    fn cancels_echo(self) -> bool {
        self.echo_cancellation && self.microphone && self.speaker
    }
}

/// What an engine reports, from its own thread.
#[derive(Debug)]
pub enum EngineEvent {
    /// About 30 ms of the microphone, mono 16-bit at [`SAMPLE_RATE`], cleaned
    /// of rumble and steady noise, and with the speaker's echo taken out if it
    /// was asked to.
    Microphone(Vec<i16>),
    /// The speaker has finished playing this utterance. One that was cleared
    /// before the end is never reported.
    Played(u64),
    /// A device failed and the engine has stopped, in words for the person.
    /// The last event there will be.
    Failed(String),
}

/// Where an engine's events go. Called on the engine thread, so it must not
/// block: hand the event on and return.
pub type EngineSink = Arc<dyn Fn(EngineEvent) + Send + Sync>;

/// What the pump asks of the engine thread, in order.
enum Control {
    Play {
        utterance: u64,
        samples: Vec<i16>,
        sample_rate: u32,
    },
    Pause,
    Resume,
    Clear,
    Working(bool),
    Chime,
    Stop,
    /// Stop, once everything heard has reached the sink; then say so.
    Finish(tokio::sync::oneshot::Sender<()>),
}

/// A running audio engine. Dropping it asks the thread to stop and lets a
/// short-lived helper thread reap it, so nothing waits on the audio thread
/// from wherever the drop happens (an async worker, mid-lock).
pub struct AudioEngine {
    control: Sender<Control>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl AudioEngine {
    /// Open the devices `options` asks for and start. Fails, saying which
    /// device and why, if any cannot be opened; nothing is left running then.
    ///
    /// `sink` is called from the engine thread with what the microphone hears
    /// and what the speaker has finished playing. A device failing sends
    /// [`EngineEvent::Failed`] and closes both.
    ///
    /// `cleanup`, for a microphone, is what to take out of it besides what is
    /// always taken out: see the module doc.
    pub async fn start(
        options: EngineOptions,
        cleanup: Option<Arc<Cleanup>>,
        sink: EngineSink,
    ) -> Result<AudioEngine, AudioError> {
        if !options.microphone && !options.speaker {
            return Err(AudioError("no audio device was asked for".into()));
        }
        let (control, control_rx) = std::sync::mpsc::channel();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let join = std::thread::Builder::new()
            .name("audio-engine".into())
            .spawn(move || run(options, cleanup, sink, ready_tx, control_rx))
            .map_err(|e| AudioError(format!("could not start the audio thread: {e}")))?;
        // From here a failure drops `engine`, which reaps the thread.
        let engine = AudioEngine { control, join: Some(join) };
        match ready_rx.await {
            Ok(Ok(())) => Ok(engine),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AudioError("the audio thread ended before it was ready".into())),
        }
    }

    /// Queue speech to be played after whatever is queued. Nothing is played,
    /// or reported, by an engine without a speaker.
    pub fn play(&self, utterance: u64, samples: Vec<i16>, sample_rate: u32) {
        let _ = self.control.send(Control::Play { utterance, samples, sample_rate });
    }

    pub fn pause(&self) {
        let _ = self.control.send(Control::Pause);
    }

    pub fn resume(&self) {
        let _ = self.control.send(Control::Resume);
    }

    pub fn clear(&self) {
        let _ = self.control.send(Control::Clear);
    }

    /// Play the sound of the assistant working while nothing else is
    /// playing, or stop it. See `karmayoga_service::voice::cue`.
    pub fn working(&self, on: bool) {
        let _ = self.control.send(Control::Working(on));
    }

    /// Play the listening sound (`karmayoga_service::voice::cue::listening_chime`)
    /// once, now. Not speech: nothing is reported when it has played, and it
    /// is heard by the echo canceller as everything played is. Nothing is
    /// played by an engine without a speaker.
    ///
    /// It is mixed over whatever is playing rather than queued behind it, so
    /// it never waits on speech nor cuts any off, and [`clear`](Self::clear)
    /// ends it.
    pub fn chime(&self) {
        let _ = self.control.send(Control::Chime);
    }

    /// Close the devices. Does not wait for it; safe to say twice.
    pub fn stop(&self) {
        let _ = self.control.send(Control::Stop);
    }

    /// Close the devices once all the microphone has heard has reached the
    /// sink -- the models' backlog and what they hold back, if any run -- or
    /// `within` has passed, whichever is first. For something that wants the
    /// last words before it moves on.
    pub async fn finish(&self, within: Duration) {
        let (done, finished) = tokio::sync::oneshot::channel();
        if self.control.send(Control::Finish(done)).is_err() {
            return;
        }
        let _ = tokio::time::timeout(within, finished).await;
    }
}

#[cfg(test)]
impl AudioEngine {
    /// An engine with no thread and no devices, for the tests of what holds
    /// one. Everything said to it is dropped.
    pub fn detached() -> AudioEngine {
        let (control, _unheard) = std::sync::mpsc::channel();
        AudioEngine { control, join: None }
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.control.send(Control::Stop);
        if let Some(join) = self.join.take() {
            let _ = std::thread::Builder::new().name("audio-engine-join".into()).spawn(move || {
                let _ = join.join();
            });
        }
    }
}

// ---- what the callbacks hand over ---------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Device {
    Microphone,
    Speaker,
}

impl Device {
    fn name(self) -> &'static str {
        match self {
            Device::Microphone => "microphone",
            Device::Speaker => "speaker",
        }
    }

    /// The same, to begin a sentence with.
    fn title(self) -> &'static str {
        match self {
            Device::Microphone => "The microphone",
            Device::Speaker => "The speaker",
        }
    }
}

enum Event {
    /// A block of the microphone, mono, at the microphone's own rate.
    Mic(Vec<f32>),
    /// A block of what the speaker was just given, mono, at the speaker's rate.
    /// Only sent when there is a canceller to use it.
    Reference(Vec<f32>),
    Fault {
        device: Device,
        kind: cpal::ErrorKind,
        message: String,
    },
}

impl Event {
    /// The order a batch of events is handled in: what was played before what
    /// was heard, so the canceller has the reference before the microphone it
    /// could have leaked into.
    fn rank(&self) -> u8 {
        match self {
            Event::Reference(_) => 0,
            Event::Mic(_) => 1,
            Event::Fault { .. } => 2,
        }
    }
}

/// Whether a `cpal` error means the stream is dead, as opposed to a reroute it
/// recovered from on its own (`DeviceChanged`) or a hiccup (`Xrun`).
fn is_fatal(kind: cpal::ErrorKind) -> bool {
    matches!(
        kind,
        cpal::ErrorKind::DeviceNotAvailable
            | cpal::ErrorKind::StreamInvalidated
            | cpal::ErrorKind::HostUnavailable
            | cpal::ErrorKind::BackendError
    )
}

/// How long a sound takes between the speaker callback and the air, and
/// between the air and the microphone callback, as the devices last said.
#[derive(Default)]
struct Latencies {
    output_us: AtomicU32,
    input_us: AtomicU32,
}

fn micros(d: Duration) -> u32 {
    d.as_micros().min(MAX_LATENCY_US) as u32
}

/// The delay to tell the canceller, in milliseconds: from a frame being
/// handed over as reference to the same sound arriving in a frame of
/// microphone.
///
/// The devices' latencies are the bulk. The resamplers matter by their
/// difference: the microphone's delays what is heard, which makes the echo
/// later, and the reference's delays what is handed over, which makes it
/// earlier. With no latency from either device the delay is a constant that
/// does for most desktops.
fn stream_delay_ms(
    output_us: u32,
    input_us: u32,
    microphone_resampler_ms: u32,
    reference_resampler_ms: u32,
) -> i32 {
    let devices_ms = ((output_us as u64 + input_us as u64) / 1000) as i32;
    if devices_ms == 0 {
        return FALLBACK_DELAY_MS;
    }
    (devices_ms + microphone_resampler_ms as i32 - reference_resampler_ms as i32).clamp(0, 500)
}

// ---- opening the devices -----------------------------------------------

/// One open device: the stream, which must be dropped on this thread, the rate
/// it runs at, and where spent buffers go back to its callback.
struct Side {
    _stream: cpal::Stream,
    rate: u32,
    pool: SyncSender<Vec<f32>>,
}

/// Everything `open` leaves running, which the loop needs and which must be
/// dropped on this thread. A side is there only if it was asked for.
struct Opened {
    speaker: Option<Side>,
    microphone: Option<Side>,
    events: Receiver<Event>,
    queue: Arc<Mutex<PlaybackQueue>>,
    /// Raised by the speaker callback when an utterance has finished, so the
    /// loop takes the queue's lock only then.
    finished: Arc<AtomicBool>,
    latencies: Arc<Latencies>,
}

fn open(host: &cpal::Host, options: EngineOptions) -> Result<Opened, AudioError> {
    let microphone = if options.microphone {
        let device =
            host.default_input_device().ok_or_else(|| AudioError("no microphone found".into()))?;
        let config = device
            .default_input_config()
            .map_err(|e| AudioError(format!("microphone config: {e}")))?;
        Some((device, config))
    } else {
        None
    };
    let speaker = if options.speaker {
        let device =
            host.default_output_device().ok_or_else(|| AudioError("no speaker found".into()))?;
        let config = device
            .default_output_config()
            .map_err(|e| AudioError(format!("speaker config: {e}")))?;
        Some((device, config))
    } else {
        None
    };

    let (tx, events) = std::sync::mpsc::sync_channel::<Event>(CHANNEL_CAPACITY);
    // A speaker can play the sound of working and the listening sound, made
    // now for its rate rather than when first wanted, under the lock its
    // callback tries for. One with no speaker has neither to play.
    let queue = Arc::new(Mutex::new(match &speaker {
        Some((_, config)) => {
            let rate = config.sample_rate();
            PlaybackQueue::with_working_sound(Bed::new(rate)).with_chime(listening_chime(rate))
        }
        None => PlaybackQueue::new(),
    }));
    let finished = Arc::new(AtomicBool::new(false));
    let latencies = Arc::new(Latencies::default());

    let speaker = speaker
        .map(|(device, config)| {
            let reference = options.cancels_echo();
            build_speaker(
                &device,
                &config,
                &Playing { queue: &queue, finished: &finished, latencies: &latencies, reference },
                tx.clone(),
            )
        })
        .transpose()?;
    let microphone = microphone
        .map(|(device, config)| build_microphone(&device, &config, &latencies, tx.clone()))
        .transpose()?;

    Ok(Opened { speaker, microphone, events, queue, finished, latencies })
}

/// The microphone: every callback downmixed to mono and pushed through `tx`,
/// by the same stream `capture.rs` opens, with this module's own event.
fn build_microphone(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
    latencies: &Arc<Latencies>,
    tx: SyncSender<Event>,
) -> Result<Side, AudioError> {
    let tx_err = tx.clone();
    let latencies = latencies.clone();
    let (stream, pool) = input::open_input(
        device,
        config,
        "microphone",
        CHANNEL_CAPACITY,
        move |samples, info| {
            let stamp = info.timestamp();
            latencies
                .input_us
                .store(micros(stamp.callback.duration_since(stamp.capture)), Ordering::Relaxed);
            // Never blocks the realtime callback: a full channel is a
            // dropped block, not a stall.
            let _: Result<(), TrySendError<Event>> = tx.try_send(Event::Mic(samples));
        },
        move |e: cpal::Error| {
            let _ = tx_err.try_send(Event::Fault {
                device: Device::Microphone,
                kind: e.kind(),
                message: e.to_string(),
            });
        },
    )?;
    Ok(Side { _stream: stream, rate: config.sample_rate(), pool })
}

/// What a speaker's callback shares with the engine thread.
struct Playing<'a> {
    queue: &'a Arc<Mutex<PlaybackQueue>>,
    finished: &'a Arc<AtomicBool>,
    latencies: &'a Arc<Latencies>,
    /// Whether to send back a copy of what was written. A canceller is the
    /// only thing that wants it.
    reference: bool,
}

/// The speaker: each callback takes mono audio from the queue (silence if it
/// is contended or empty), writes it to every channel in the device's own
/// sample format, and, if there is a canceller to use it, sends a copy of the
/// mono block back as the reference.
fn build_speaker(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
    playing: &Playing<'_>,
    tx: SyncSender<Event>,
) -> Result<Side, AudioError> {
    let tx_err = tx.clone();
    let on_error = move |e: cpal::Error| {
        let _ = tx_err.try_send(Event::Fault {
            device: Device::Speaker,
            kind: e.kind(),
            message: e.to_string(),
        });
    };
    let (free_tx, free_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(CHANNEL_CAPACITY);
    let channels = config.channels().max(1) as usize;

    let stream = match config.sample_format() {
        SampleFormat::I8 => {
            build_output::<i8, _>(device, config, channels, playing, free_rx, tx, on_error)
        }
        SampleFormat::I16 => {
            build_output::<i16, _>(device, config, channels, playing, free_rx, tx, on_error)
        }
        SampleFormat::I32 => {
            build_output::<i32, _>(device, config, channels, playing, free_rx, tx, on_error)
        }
        SampleFormat::F32 => {
            build_output::<f32, _>(device, config, channels, playing, free_rx, tx, on_error)
        }
        other => return Err(AudioError(format!("unsupported speaker sample format {other}"))),
    }
    .map_err(|e| AudioError(format!("could not open the speaker: {e}")))?;
    stream.play().map_err(|e| AudioError(format!("could not start the speaker: {e}")))?;
    Ok(Side { _stream: stream, rate: config.sample_rate(), pool: free_tx })
}

/// [`build_speaker`] for one sample format, which is what the callback has to
/// be written against.
fn build_output<T, E>(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
    channels: usize,
    playing: &Playing<'_>,
    free_rx: Receiver<Vec<f32>>,
    tx: SyncSender<Event>,
    on_error: E,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32> + 'static,
    E: FnMut(cpal::Error) + Send + 'static,
{
    let queue = playing.queue.clone();
    let finished = playing.finished.clone();
    let latencies = playing.latencies.clone();
    let reference = playing.reference;
    // Room for any callback a device is likely to ask for; one that wants more
    // grows it, once.
    let mut mono: Vec<f32> = Vec::with_capacity(8192);
    device.build_output_stream(
        (*config).into(),
        move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
            let stamp = info.timestamp();
            latencies
                .output_us
                .store(micros(stamp.playback.duration_since(stamp.callback)), Ordering::Relaxed);
            write_block::<T>(data, channels, &mut mono, &queue, &finished);
            if reference {
                let mut copy = free_rx.try_recv().unwrap_or_default();
                copy.clear();
                copy.extend_from_slice(&mono);
                let _: Result<(), TrySendError<Event>> = tx.try_send(Event::Reference(copy));
            }
        },
        on_error,
        None,
    )
}

/// One output callback's worth: fill `mono` from the queue, then write it to
/// every channel of `data`. Realtime: `mono` is reused and the lock is only
/// tried.
fn write_block<T>(
    data: &mut [T],
    channels: usize,
    mono: &mut Vec<f32>,
    queue: &Mutex<PlaybackQueue>,
    finished: &AtomicBool,
) where
    T: SizedSample + FromSample<f32>,
{
    let frames = data.len() / channels;
    mono.clear();
    mono.resize(frames, 0.0);
    if let Ok(mut queue) = queue.try_lock() {
        queue.fill(&mut mono[..]);
        if queue.has_finished() {
            finished.store(true, Ordering::Release);
        }
    }
    for (frame, &sample) in data.chunks_mut(channels).zip(mono.iter()) {
        frame.fill(<T as Sample>::from_sample(sample));
    }
}

// ---- the engine thread --------------------------------------------------

fn lock(queue: &Mutex<PlaybackQueue>) -> MutexGuard<'_, PlaybackQueue> {
    queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn run(
    options: EngineOptions,
    cleanup: Option<Arc<Cleanup>>,
    sink: EngineSink,
    ready: tokio::sync::oneshot::Sender<Result<(), AudioError>>,
    control: Receiver<Control>,
) {
    let host = cpal::default_host();
    tracing::info!(host = %host.id().name(), ?options, "audio: starting");
    let opened = match open(&host, options) {
        Ok(opened) => opened,
        Err(e) => {
            tracing::warn!(error = %e, "audio: could not open the devices");
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut pipeline = match Pipeline::new(
        opened.microphone.as_ref().map(|side| side.rate),
        opened.speaker.as_ref().map(|side| side.rate),
        options.cancels_echo(),
    ) {
        Ok(pipeline) => pipeline,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    if let Some(cleanup) = cleanup.filter(|_| opened.microphone.is_some()) {
        pipeline.clean_with(cleanup, sink.clone());
    }
    if ready.send(Ok(())).is_err() {
        // Nobody is waiting any more; the start was abandoned.
        return;
    }
    let failure = pipeline.run(&opened, &sink, &control);
    pipeline.end(&sink);
    if let Some(message) = failure {
        tracing::warn!(%message, "audio: stopped");
        sink(EngineEvent::Failed(message));
    }
    // `opened` drops here, on this thread, closing the streams.
}

/// What cancelling echo needs besides the canceller: the speaker's output
/// brought to its rate.
struct Echo {
    reference: StreamResampler,
    /// The delay last given to the canceller.
    last_delay: Option<i32>,
}

/// The engine thread's working state: everything that is not a device.
struct Pipeline {
    /// Present when there is a microphone.
    microphone: Option<StreamResampler>,
    /// Present when there is a microphone: everything it hears is cleaned
    /// here, of the speaker's echo too when `echo` is present.
    cleaner: Option<Canceller>,
    /// Present when echo is cancelled, which is when both devices are open
    /// and it was asked for.
    echo: Option<Echo>,
    /// Microphone audio waiting to make a batch.
    batch: Vec<i16>,
    /// The models' thread, when there is one: see [`Pipeline::clean_with`].
    worker: Option<Worker>,
    /// The canceller's own noise suppression is off, for the noise filter.
    noise_handed_over: bool,
    /// Told to finish rather than stop: who to tell when it has.
    finishing: Option<tokio::sync::oneshot::Sender<()>>,
    /// Batches dropped because that thread had fallen this far behind.
    dropped: u64,
    // Scratch, reused so the loop allocates nothing in steady state.
    resampled: Vec<f32>,
    finished: Vec<u64>,
    spent: Vec<Vec<f32>>,
}

impl Pipeline {
    /// For devices running at these rates, `None` for one that is not open.
    /// The microphone is always cleaned of rumble and steady noise; with
    /// `echo` and both rates, of the speaker's echo as well.
    fn new(
        microphone_rate: Option<u32>,
        speaker_rate: Option<u32>,
        echo: bool,
    ) -> Result<Self, AudioError> {
        let microphone =
            microphone_rate.map(|rate| StreamResampler::new(rate, SAMPLE_RATE)).transpose()?;
        let echo = match (microphone_rate, speaker_rate) {
            (Some(_), Some(speaker)) if echo => Some(Echo {
                reference: StreamResampler::new(speaker, SAMPLE_RATE)?,
                last_delay: None,
            }),
            _ => None,
        };
        let cleaning = if echo.is_some() { Cleaning::Echo } else { Cleaning::Noise };
        let cleaner = microphone.as_ref().map(|_| Canceller::new(cleaning));
        Ok(Self {
            microphone,
            cleaner,
            echo,
            batch: Vec::with_capacity(BATCH_SAMPLES * 2),
            worker: None,
            noise_handed_over: false,
            finishing: None,
            dropped: 0,
            resampled: Vec::new(),
            finished: Vec::new(),
            spent: Vec::new(),
        })
    }

    /// Hand every batch to `cleanup`'s models, on a thread of their own,
    /// which give `sink` what they make of it. The noise filter is loaded
    /// here and replaces the canceller's own noise suppression; voice
    /// isolation, slow to load, is loaded on that thread. See the module doc.
    fn clean_with(&mut self, cleanup: Arc<Cleanup>, sink: EngineSink) {
        clear_cleanup_trouble();
        let noise = if cleanup.strong_noise {
            match NoiseFilter::load() {
                Ok(noise) => Some(noise),
                Err(e) => {
                    cleanup_failed(e.to_string());
                    None
                }
            }
        } else {
            None
        };
        if noise.is_some() {
            self.suppress_noise(false);
        }
        let sample = cleanup.voice_sample.clone();
        if noise.is_none() && sample.is_none() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<i16>>(CLEANUP_BACKLOG);
        let flags = Flags::default();
        let theirs = flags.clone();
        let spawned = std::thread::Builder::new()
            .name("mic-cleanup".into())
            .spawn(move || clean(noise, sample, &rx, &sink, &theirs));
        match spawned {
            Ok(join) => self.worker = Some(Worker { batches: Some(tx), join: Some(join), flags }),
            Err(e) => {
                cleanup_failed(format!("could not start the microphone's models: {e}"));
                self.suppress_noise(true);
            }
        }
    }

    /// Rebuild the canceller with its noise suppression on or off. Its echo
    /// canceller starts learning again, so this is done before anything is
    /// heard, or when the models give up.
    fn suppress_noise(&mut self, on: bool) {
        if self.noise_handed_over != on {
            return;
        }
        let cleaning = if self.echo.is_some() { Cleaning::Echo } else { Cleaning::Noise };
        self.cleaner =
            self.microphone.as_ref().map(|_| Canceller::with_noise_suppression(cleaning, on));
        self.noise_handed_over = !on;
    }

    /// A finished batch, to the models if they are running, else the sink.
    fn deliver(&mut self, audio: Vec<i16>, sink: &EngineSink) {
        if self.worker.as_ref().is_some_and(|w| w.flags.gave_up.load(Ordering::Acquire)) {
            // The models are off: the canceller suppresses noise again.
            self.suppress_noise(true);
        }
        let Some(batches) = self.worker.as_ref().and_then(|w| w.batches.as_ref()) else {
            sink(EngineEvent::Microphone(audio));
            return;
        };
        match batches.try_send(audio) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped += 1;
                if self.dropped == 1 || self.dropped.is_multiple_of(100) {
                    tracing::warn!(
                        dropped = self.dropped,
                        "audio: the microphone's models are behind"
                    );
                }
            }
            Err(TrySendError::Disconnected(audio)) => {
                // Their thread is gone: as the ordinary filter left it.
                self.worker = None;
                self.suppress_noise(true);
                sink(EngineEvent::Microphone(audio));
            }
        }
    }

    /// The loop is over. Told to finish: what is left of a batch goes on,
    /// the models' thread is waited for while it cleans its backlog and what
    /// it holds, and whoever asked is told. Otherwise that thread is told to
    /// stop at once, and not waited for.
    fn end(&mut self, sink: &EngineSink) {
        let Some(done) = self.finishing.take() else {
            if let Some(worker) = &self.worker {
                worker.flags.cancel.store(true, Ordering::Release);
            }
            self.worker = None;
            return;
        };
        if !self.batch.is_empty() {
            let rest = std::mem::take(&mut self.batch);
            self.deliver(rest, sink);
        }
        if let Some(mut worker) = self.worker.take() {
            drop(worker.batches.take());
            if let Some(join) = worker.join.take() {
                let _ = join.join();
            }
        }
        let _ = done.send(());
    }

    /// Run until told to stop or a device fails; the last of those is
    /// returned, as words for the person.
    fn run(
        &mut self,
        opened: &Opened,
        sink: &EngineSink,
        control: &Receiver<Control>,
    ) -> Option<String> {
        let mut last_refresh = Instant::now() - DELAY_REFRESH;
        let mut events: Vec<Event> = Vec::with_capacity(MAX_EVENTS_PER_TURN);
        loop {
            // What was asked of us first, in the order it was asked.
            loop {
                match control.try_recv() {
                    Ok(Control::Stop) | Err(TryRecvError::Disconnected) => return None,
                    Ok(Control::Finish(done)) => {
                        self.finishing = Some(done);
                        return None;
                    }
                    Ok(message) => self.apply(message, opened),
                    Err(TryRecvError::Empty) => break,
                }
            }

            events.clear();
            match opened.events.recv_timeout(POLL) {
                Ok(event) => events.push(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return None,
            }
            while events.len() < MAX_EVENTS_PER_TURN {
                match opened.events.try_recv() {
                    Ok(event) => events.push(event),
                    Err(_) => break,
                }
            }
            events.sort_by_key(Event::rank);

            for event in events.drain(..) {
                match event {
                    Event::Reference(samples) => {
                        self.hear_reference(&samples);
                        give_back(&opened.speaker, samples);
                    }
                    Event::Mic(samples) => {
                        self.hear_microphone(&samples, sink);
                        give_back(&opened.microphone, samples);
                    }
                    Event::Fault { device, kind, message } => {
                        tracing::warn!(device = device.name(), ?kind, %message, "audio: stream error");
                        if is_fatal(kind) {
                            return Some(format!("{} stopped working: {message}", device.title()));
                        }
                    }
                }
            }

            if opened.finished.swap(false, Ordering::Acquire) {
                self.report_played(opened, sink);
            }

            if last_refresh.elapsed() >= DELAY_REFRESH {
                last_refresh = Instant::now();
                self.refresh_delay(opened);
            }
        }
    }

    fn apply(&mut self, message: Control, opened: &Opened) {
        match message {
            Control::Play { utterance, samples, sample_rate } => {
                let Some(speaker) = &opened.speaker else {
                    tracing::debug!(utterance, "audio: speech for an engine with no speaker");
                    return;
                };
                let mono = to_f32(&samples);
                let ready = match resample::clip(&mono, sample_rate, speaker.rate) {
                    Ok(ready) => ready,
                    Err(e) => {
                        // Whoever is waiting must not wait for audio that will
                        // never play, so it is queued as an empty utterance,
                        // which finishes in its turn.
                        tracing::warn!(error = %e, "audio: could not convert speech for the speaker");
                        Vec::new()
                    }
                };
                lock(&opened.queue).push(utterance, ready);
                // An empty one may have finished at once.
                opened.finished.store(true, Ordering::Release);
            }
            Control::Pause => lock(&opened.queue).pause(),
            Control::Resume => lock(&opened.queue).resume(),
            Control::Clear => {
                lock(&opened.queue).clear();
                opened.finished.store(true, Ordering::Release);
            }
            Control::Working(on) => lock(&opened.queue).set_working(on),
            Control::Chime => lock(&opened.queue).play_chime(),
            Control::Stop | Control::Finish(_) => {}
        }
    }

    /// Collect what the speaker has finished and tell the sink.
    fn report_played(&mut self, opened: &Opened, sink: &EngineSink) {
        lock(&opened.queue).take_finished(&mut self.finished, &mut self.spent);
        // Freed here, where freeing is allowed.
        self.spent.clear();
        for utterance in self.finished.drain(..) {
            sink(EngineEvent::Played(utterance));
        }
    }

    fn hear_reference(&mut self, samples: &[f32]) {
        let (Some(echo), Some(cleaner)) = (self.echo.as_mut(), self.cleaner.as_mut()) else {
            return;
        };
        self.resampled.clear();
        match echo.reference.push(samples, &mut self.resampled) {
            Ok(()) => cleaner.push_reference(&self.resampled),
            Err(e) => tracing::debug!(error = %e, "audio: could not resample the reference"),
        }
    }

    /// Clean a block of microphone and give the sink what is ready.
    fn hear_microphone(&mut self, samples: &[f32], sink: &EngineSink) {
        let (Some(microphone), Some(cleaner)) = (self.microphone.as_mut(), self.cleaner.as_mut())
        else {
            return;
        };
        self.resampled.clear();
        if let Err(e) = microphone.push(samples, &mut self.resampled) {
            tracing::debug!(error = %e, "audio: could not resample the microphone");
            return;
        }
        cleaner.process(&self.resampled, &mut self.batch);
        if self.batch.len() >= BATCH_SAMPLES {
            let audio = std::mem::replace(&mut self.batch, Vec::with_capacity(BATCH_SAMPLES * 2));
            self.deliver(audio, sink);
        }
    }

    fn refresh_delay(&mut self, opened: &Opened) {
        let (Some(echo), Some(microphone), Some(cleaner)) =
            (self.echo.as_mut(), self.microphone.as_ref(), self.cleaner.as_mut())
        else {
            return;
        };
        let delay = stream_delay_ms(
            opened.latencies.output_us.load(Ordering::Relaxed),
            opened.latencies.input_us.load(Ordering::Relaxed),
            microphone.delay_ms(),
            echo.reference.delay_ms(),
        );
        // Not worth disturbing the canceller for a few milliseconds.
        let worth_it = match echo.last_delay {
            Some(last) => (delay - last).abs() >= 5,
            None => true,
        };
        if worth_it {
            echo.last_delay = Some(delay);
            cleaner.set_delay_ms(delay);
        }
    }
}

// ---- the models' thread -------------------------------------------------

/// What the engine thread and the models' thread tell each other.
#[derive(Clone, Default)]
struct Flags {
    /// The engine has stopped: stop now, whatever is left.
    cancel: Arc<AtomicBool>,
    /// The models have been turned off: what comes is as it went in.
    gave_up: Arc<AtomicBool>,
}

/// The models' thread, as the engine thread holds it.
struct Worker {
    /// Batches for it; dropped to tell it the microphone has closed.
    batches: Option<SyncSender<Vec<i16>>>,
    join: Option<std::thread::JoinHandle<()>>,
    flags: Flags,
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Not finished properly ([`Pipeline::end`] takes what it waits for):
        // nothing more it makes is wanted.
        if self.join.is_some() {
            self.flags.cancel.store(true, Ordering::Release);
        }
    }
}

/// The "mic-cleanup" thread: load voice isolation, then clean every batch
/// until the engine lets go of the channel, and then what the models still
/// hold. Without models -- they would not load, or could not keep up --
/// batches go on as they came. Stops as soon as the engine says so.
fn clean(
    noise: Option<NoiseFilter>,
    sample: Option<Vec<i16>>,
    batches: &Receiver<Vec<i16>>,
    sink: &EngineSink,
    flags: &Flags,
) {
    let voices = sample.and_then(|sample| match VoiceIsolation::load(&sample) {
        Ok(voices) => Some(voices),
        Err(e) => {
            cleanup_failed(e.to_string());
            None
        }
    });
    let mut models = MicCleanup::new(noise, voices);
    let mut pace = Pace::default();
    let mut out = Vec::new();
    while let Ok(batch) = batches.recv() {
        if flags.cancel.load(Ordering::Acquire) {
            return;
        }
        let Some(running) = models.as_mut() else {
            sink(EngineEvent::Microphone(batch));
            continue;
        };
        let started = Instant::now();
        running.process(&batch, &mut out);
        if pace.behind(batch.len(), started.elapsed()) {
            cleanup_failed(
                "This computer could not keep up with taking other voices and noise out of the \
                 microphone, so they were turned off until it next opens."
                    .into(),
            );
            models = None;
            flags.gave_up.store(true, Ordering::Release);
        }
        if !out.is_empty() {
            sink(EngineEvent::Microphone(std::mem::take(&mut out)));
        }
    }
    if let Some(running) = models.as_mut()
        && !flags.cancel.load(Ordering::Acquire)
    {
        running.finish(&mut out);
        if !out.is_empty() {
            sink(EngineEvent::Microphone(out));
        }
    }
}

/// Whether the models are keeping up: the time they took over the last
/// [`PACE_WINDOW`] of audio, as a share of that audio's length, past
/// [`PACE_LIMIT`]. Judged only once a whole window has been heard, so the
/// backlog left by loading, worked off as fast as they can go, is not held
/// against them.
#[derive(Default)]
struct Pace {
    /// `(samples, time taken)` for the batches of the window.
    recent: std::collections::VecDeque<(usize, Duration)>,
    samples: usize,
    spent: Duration,
}

impl Pace {
    fn behind(&mut self, samples: usize, spent: Duration) -> bool {
        self.recent.push_back((samples, spent));
        self.samples += samples;
        self.spent += spent;
        let window = (PACE_WINDOW.as_secs_f64() * f64::from(SAMPLE_RATE)) as usize;
        while self.samples > window {
            let Some((old_samples, old_spent)) = self.recent.pop_front() else { break };
            if self.samples - old_samples < window {
                self.recent.push_front((old_samples, old_spent));
                break;
            }
            self.samples -= old_samples;
            self.spent = self.spent.saturating_sub(old_spent);
        }
        let heard = self.samples as f64 / f64::from(SAMPLE_RATE);
        self.samples >= window && self.spent.as_secs_f64() > heard * PACE_LIMIT
    }
}

/// A spent block goes back to the callback it came from, if that device is
/// open, to be used again.
fn give_back(side: &Option<Side>, buffer: Vec<f32>) {
    if let Some(side) = side {
        let _ = side.pool.try_send(buffer);
    }
}

fn to_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|&s| s as f32 / 32768.0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::echo::FRAME;

    /// Batches of `ms` of audio, each taking `took` to clean, for `seconds`:
    /// whether the models were ever judged behind.
    fn paced(ms: u64, took: Duration, seconds: u64) -> bool {
        let mut pace = Pace::default();
        let samples = (SAMPLE_RATE as u64 * ms / 1000) as usize;
        (0..seconds * 1000 / ms).any(|_| pace.behind(samples, took))
    }

    #[test]
    fn models_that_keep_up_are_left_running() {
        assert!(!paced(30, Duration::from_millis(12), 60));
    }

    #[test]
    fn models_slower_than_real_time_are_found_out() {
        assert!(paced(30, Duration::from_millis(29), 30));
        assert!(paced(30, Duration::from_millis(40), 30));
    }

    #[test]
    fn a_slow_start_is_not_held_against_them() {
        // Loading's backlog worked off in a burst, then keeping up: never
        // judged before a whole window has been heard.
        let mut pace = Pace::default();
        let batch = (SAMPLE_RATE as usize * 30) / 1000;
        assert!(!pace.behind(batch, Duration::from_millis(2_000)));
        for _ in 0..1_000 {
            assert!(!pace.behind(batch, Duration::from_millis(10)));
        }
    }

    #[test]
    fn a_device_that_reports_no_latency_gets_the_constant() {
        assert_eq!(stream_delay_ms(0, 0, 10, 10), FALLBACK_DELAY_MS);
    }

    #[test]
    fn the_hint_is_the_two_latencies_adjusted_by_the_resamplers() {
        // 20 ms out, 30 ms in; the microphone's resampler adds 6, the
        // reference's takes 2 back.
        assert_eq!(stream_delay_ms(20_000, 30_000, 6, 2), 54);
    }

    #[test]
    fn the_hint_stays_in_the_range_the_canceller_takes() {
        assert_eq!(stream_delay_ms(400_000, 400_000, 0, 0), 500);
        assert_eq!(stream_delay_ms(1_000, 0, 0, 20), 0);
    }

    #[test]
    fn a_wild_latency_is_capped_before_it_is_stored() {
        assert_eq!(micros(Duration::from_secs(5)), MAX_LATENCY_US as u32);
        assert_eq!(micros(Duration::from_millis(12)), 12_000);
    }

    #[test]
    fn what_was_heard_is_never_handled_before_what_was_played() {
        let mut events = [
            Event::Mic(vec![0.0]),
            Event::Fault {
                device: Device::Speaker,
                kind: cpal::ErrorKind::BackendError,
                message: String::new(),
            },
            Event::Reference(vec![0.0]),
            Event::Mic(vec![0.0]),
            Event::Reference(vec![0.0]),
        ];
        events.sort_by_key(Event::rank);
        let ranks: Vec<u8> = events.iter().map(Event::rank).collect();
        assert_eq!(ranks, vec![0, 0, 1, 1, 2]);
    }

    #[test]
    fn only_a_dead_stream_ends_the_session() {
        assert!(is_fatal(cpal::ErrorKind::DeviceNotAvailable));
        assert!(is_fatal(cpal::ErrorKind::StreamInvalidated));
        assert!(!is_fatal(cpal::ErrorKind::DeviceChanged));
        assert!(!is_fatal(cpal::ErrorKind::Xrun));
    }

    #[test]
    fn a_block_is_written_to_every_channel_and_the_reference_is_what_was_written() {
        let queue = Mutex::new(PlaybackQueue::new());
        let finished = AtomicBool::new(false);
        queue.lock().unwrap().push(1, vec![0.5, -0.5, 0.25]);

        let mut data = [0.0f32; 8]; // four stereo frames
        let mut mono = Vec::new();
        write_block::<f32>(&mut data, 2, &mut mono, &queue, &finished);

        assert_eq!(data, [0.5, 0.5, -0.5, -0.5, 0.25, 0.25, 0.0, 0.0]);
        assert_eq!(mono, vec![0.5, -0.5, 0.25, 0.0]);
        assert!(finished.load(Ordering::Acquire), "utterance 1 ended inside the block");
    }

    #[test]
    fn the_listening_sound_is_in_the_block_and_so_in_the_reference() {
        let queue = Mutex::new(PlaybackQueue::new().with_chime(vec![0.25; 3]));
        let finished = AtomicBool::new(false);
        queue.lock().unwrap().play_chime();

        let mut data = [0.0f32; 8]; // four stereo frames
        let mut mono = Vec::new();
        write_block::<f32>(&mut data, 2, &mut mono, &queue, &finished);

        assert_eq!(data, [0.25, 0.25, 0.25, 0.25, 0.25, 0.25, 0.0, 0.0]);
        assert_eq!(
            mono,
            vec![0.25, 0.25, 0.25, 0.0],
            "what was played is what the canceller hears"
        );
        assert!(
            !finished.load(Ordering::Acquire),
            "it is not an utterance, so nothing is reported"
        );
    }

    #[test]
    fn a_block_converts_to_the_devices_own_sample_format() {
        let queue = Mutex::new(PlaybackQueue::new());
        let finished = AtomicBool::new(false);
        queue.lock().unwrap().push(1, vec![0.0, 1.0]);

        let mut data = [7i16; 2];
        let mut mono = Vec::new();
        write_block::<i16>(&mut data, 1, &mut mono, &queue, &finished);
        assert_eq!(data[0], 0);
        assert!(data[1] > 30_000, "full scale, got {}", data[1]);
    }

    #[test]
    fn a_contended_queue_plays_silence_rather_than_waiting() {
        let queue = Mutex::new(PlaybackQueue::new());
        let finished = AtomicBool::new(false);
        queue.lock().unwrap().push(1, vec![0.5; 4]);

        let _held = queue.lock().unwrap();
        let mut data = [1.0f32; 4];
        let mut mono = Vec::new();
        write_block::<f32>(&mut data, 1, &mut mono, &queue, &finished);
        assert_eq!(data, [0.0; 4]);
        assert_eq!(mono, vec![0.0; 4], "silence is what was played, so it is the reference");
    }

    #[test]
    fn speech_is_made_ready_for_the_resampler_at_full_scale_range() {
        assert_eq!(to_f32(&[0, 16384, -32768]), vec![0.0, 0.5, -1.0]);
    }

    #[test]
    fn echo_is_cancelled_only_with_both_devices_and_only_if_asked() {
        let options = |microphone, speaker, echo_cancellation| EngineOptions {
            microphone,
            speaker,
            echo_cancellation,
        };
        assert!(options(true, true, true).cancels_echo());
        assert!(!options(true, true, false).cancels_echo());
        assert!(!options(true, false, true).cancels_echo(), "nothing is playing to cancel");
        assert!(!options(false, true, true).cancels_echo(), "nothing is listening");
    }

    #[test]
    fn the_canceller_is_built_only_when_both_devices_are_open_and_it_was_asked_for() {
        assert!(Pipeline::new(Some(48_000), Some(44_100), true).unwrap().echo.is_some());
        assert!(Pipeline::new(Some(48_000), Some(44_100), false).unwrap().echo.is_none());
        assert!(Pipeline::new(Some(48_000), None, true).unwrap().echo.is_none());
        assert!(Pipeline::new(None, Some(44_100), true).unwrap().echo.is_none());
        assert!(Pipeline::new(None, Some(44_100), true).unwrap().microphone.is_none());
    }

    #[test]
    fn every_microphone_is_cleaned_and_only_one_with_a_speaker_of_echo() {
        let cleaning = |pipeline: Pipeline| pipeline.cleaner.map(|c| c.cleaning());
        let with = |m, s, e| cleaning(Pipeline::new(m, s, e).unwrap());
        assert_eq!(with(Some(48_000), Some(44_100), true), Some(Cleaning::Echo));
        assert_eq!(with(Some(48_000), Some(44_100), false), Some(Cleaning::Noise));
        assert_eq!(with(Some(48_000), None, true), Some(Cleaning::Noise));
        assert_eq!(with(None, Some(44_100), true), None, "nothing is listening");
    }

    /// A sink that keeps what it is given.
    fn collecting() -> (EngineSink, Arc<Mutex<Vec<EngineEvent>>>) {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let kept = heard.clone();
        let sink: EngineSink = Arc::new(move |event| kept.lock().unwrap().push(event));
        (sink, heard)
    }

    #[test]
    fn a_microphone_alone_is_framed_into_batches_of_whole_frames() {
        let (sink, heard) = collecting();
        let mut pipeline = Pipeline::new(Some(SAMPLE_RATE), None, false).unwrap();

        // Less than a batch is held back.
        pipeline.hear_microphone(&vec![0.25; BATCH_SAMPLES - 1], &sink);
        assert!(heard.lock().unwrap().is_empty());
        // The rest of one is sent, in the cleaner's 10 ms frames; what does
        // not make a frame waits for the next block.
        pipeline.hear_microphone(&[0.25; 11], &sink);
        let heard = heard.lock().unwrap();
        let [EngineEvent::Microphone(batch)] = &heard[..] else {
            panic!("one batch, got {heard:?}");
        };
        assert_eq!(BATCH_SAMPLES % FRAME, 0, "a batch is whole frames");
        assert_eq!(batch.len(), BATCH_SAMPLES);
    }

    #[test]
    fn a_microphone_at_another_rate_comes_out_at_the_speech_rate() {
        let (sink, heard) = collecting();
        let mut pipeline = Pipeline::new(Some(48_000), None, false).unwrap();
        // A second of it, in the odd pieces a device hands over.
        for piece in vec![0.1f32; 48_000].chunks(441) {
            pipeline.hear_microphone(piece, &sink);
        }
        let samples: usize = heard
            .lock()
            .unwrap()
            .iter()
            .map(|event| match event {
                EngineEvent::Microphone(batch) => batch.len(),
                other => panic!("only the microphone was open, got {other:?}"),
            })
            .sum();
        assert!(samples <= SAMPLE_RATE as usize, "{samples}");
        assert!(samples + BATCH_SAMPLES + 1024 >= SAMPLE_RATE as usize, "{samples}");
    }

    #[tokio::test]
    async fn an_engine_asked_for_no_device_is_refused_without_opening_any() {
        let (sink, _heard) = collecting();
        let options = EngineOptions { microphone: false, speaker: false, echo_cancellation: true };
        let refused = AudioEngine::start(options, None, sink).await.err().expect("nothing to open");
        assert!(refused.0.contains("no audio device"), "{refused}");
    }

    /// The real thing, on whatever devices are the default. `#[ignore]`, and
    /// gated again, because it makes sound: run it inside a private PipeWire
    /// with `KARMAYOGA_TEST_VOICE_DEVICES=1`, never on somebody's desktop.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "opens the default microphone and speakers"]
    async fn the_devices_play_and_listen() {
        if std::env::var("KARMAYOGA_TEST_VOICE_DEVICES").as_deref() != Ok("1") {
            return;
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sink: EngineSink = Arc::new(move |event| {
            let _ = tx.send(event);
        });
        let options = EngineOptions { microphone: true, speaker: true, echo_cancellation: true };
        let engine = AudioEngine::start(options, None, sink).await.expect("the devices open");
        let tone = |seconds: f32| -> Vec<i16> {
            (0..(seconds * 24_000.0) as usize)
                .map(|i| {
                    ((i as f32 * 440.0 / 24_000.0 * std::f32::consts::TAU).sin() * 9000.0) as i16
                })
                .collect()
        };
        engine.play(1, tone(0.5), 24_000);
        engine.play(2, tone(5.0), 24_000);
        let began = std::time::Instant::now();
        let (mut samples, mut energy, mut played) = (0usize, 0f64, Vec::new());
        let mut cleared = false;
        while began.elapsed() < Duration::from_secs(3) {
            match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
                Ok(Some(EngineEvent::Microphone(batch))) => {
                    samples += batch.len();
                    energy += batch.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>();
                }
                Ok(Some(EngineEvent::Played(utterance))) => played.push(utterance),
                Ok(Some(EngineEvent::Failed(message))) => panic!("trouble: {message}"),
                Err(_) => {}
                Ok(None) => break,
            }
            if !cleared && played == [1] {
                cleared = true;
                engine.clear();
            }
        }
        engine.stop();
        let seconds = began.elapsed().as_secs_f64();
        let rate = samples as f64 / seconds;
        println!("{rate:.0} samples a second, rms {:.0}", (energy / samples as f64).sqrt());
        assert_eq!(played, [1], "the first played, and the cleared one never reports");
        // At least real time: a null device has no clock and runs ahead of it.
        assert!(rate > 13_000.0, "{rate}");
    }
}
