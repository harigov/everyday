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
//!       ▲  (what it just wrote)            ├─▶ resample ─▶ echo canceller ─▶ EngineEvent::Microphone
//!       │                                  │
//!   PlaybackQueue ◀── Control::Play ◀──────┘ (resampled to the device's rate here)
//! ```
//!
//! The reference and the canceller exist only when both devices are open and
//! echo cancellation was asked for. Without them the microphone is still
//! resampled to [`SAMPLE_RATE`] and framed into the same batches, as it
//! came.
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
//! for the microphone. See `echo.rs`.
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
use karmayoga_service::voice::cue::Bed;

use super::AudioError;
use super::echo::Canceller;
use super::input::{self, f32_to_i16};
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
    /// About 30 ms of the microphone, mono 16-bit at [`SAMPLE_RATE`], with the
    /// speaker's echo taken out if it was asked to.
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
    Play { utterance: u64, samples: Vec<i16>, sample_rate: u32 },
    Pause,
    Resume,
    Clear,
    Working(bool),
    Stop,
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
    pub async fn start(
        options: EngineOptions,
        sink: EngineSink,
    ) -> Result<AudioEngine, AudioError> {
        if !options.microphone && !options.speaker {
            return Err(AudioError("no audio device was asked for".into()));
        }
        let (control, control_rx) = std::sync::mpsc::channel();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let join = std::thread::Builder::new()
            .name("audio-engine".into())
            .spawn(move || run(options, sink, ready_tx, control_rx))
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

    /// Close the devices. Does not wait for it; safe to say twice.
    pub fn stop(&self) {
        let _ = self.control.send(Control::Stop);
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
    // A speaker can play the sound of working, made now for its rate rather
    // than when it is first wanted, under the lock its callback tries for.
    let queue = Arc::new(Mutex::new(match &speaker {
        Some((_, config)) => {
            PlaybackQueue::with_working_sound(Bed::new(config.sample_rate().into()))
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
    if ready.send(Ok(())).is_err() {
        // Nobody is waiting any more; the start was abandoned.
        return;
    }
    let failure = pipeline.run(&opened, &sink, &control);
    if let Some(message) = failure {
        tracing::warn!(%message, "audio: stopped");
        sink(EngineEvent::Failed(message));
    }
    // `opened` drops here, on this thread, closing the streams.
}

/// What the canceller needs besides itself: the speaker's output brought to
/// its rate.
struct Echo {
    reference: StreamResampler,
    canceller: Canceller,
    /// The delay last given to the canceller.
    last_delay: Option<i32>,
}

/// The engine thread's working state: everything that is not a device.
struct Pipeline {
    /// Present when there is a microphone.
    microphone: Option<StreamResampler>,
    /// Present when the canceller runs, which is when both devices are open
    /// and it was asked for.
    echo: Option<Echo>,
    /// Microphone audio waiting to make a batch.
    batch: Vec<i16>,
    // Scratch, reused so the loop allocates nothing in steady state.
    resampled: Vec<f32>,
    finished: Vec<u64>,
    spent: Vec<Vec<f32>>,
}

impl Pipeline {
    /// For devices running at these rates, `None` for one that is not open.
    /// With `echo` and both rates, the microphone goes through a canceller.
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
                canceller: Canceller::new(),
                last_delay: None,
            }),
            _ => None,
        };
        Ok(Self {
            microphone,
            echo,
            batch: Vec::with_capacity(BATCH_SAMPLES * 2),
            resampled: Vec::new(),
            finished: Vec::new(),
            spent: Vec::new(),
        })
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
            Control::Stop => {}
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
        let Some(echo) = self.echo.as_mut() else { return };
        self.resampled.clear();
        match echo.reference.push(samples, &mut self.resampled) {
            Ok(()) => echo.canceller.push_reference(&self.resampled),
            Err(e) => tracing::debug!(error = %e, "audio: could not resample the reference"),
        }
    }

    /// Clean a block of microphone, if there is a canceller, and give the sink
    /// what is ready.
    fn hear_microphone(&mut self, samples: &[f32], sink: &EngineSink) {
        let Some(microphone) = self.microphone.as_mut() else { return };
        self.resampled.clear();
        if let Err(e) = microphone.push(samples, &mut self.resampled) {
            tracing::debug!(error = %e, "audio: could not resample the microphone");
            return;
        }
        match self.echo.as_mut() {
            Some(echo) => echo.canceller.process(&self.resampled, &mut self.batch),
            None => self.batch.extend(self.resampled.iter().map(|&s| f32_to_i16(s))),
        }
        if self.batch.len() >= BATCH_SAMPLES {
            let audio = std::mem::replace(&mut self.batch, Vec::with_capacity(BATCH_SAMPLES * 2));
            sink(EngineEvent::Microphone(audio));
        }
    }

    fn refresh_delay(&mut self, opened: &Opened) {
        let (Some(echo), Some(microphone)) = (self.echo.as_mut(), self.microphone.as_ref()) else {
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
            echo.canceller.set_delay_ms(delay);
        }
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

    /// A sink that keeps what it is given.
    fn collecting() -> (EngineSink, Arc<Mutex<Vec<EngineEvent>>>) {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let kept = heard.clone();
        let sink: EngineSink = Arc::new(move |event| kept.lock().unwrap().push(event));
        (sink, heard)
    }

    #[test]
    fn a_microphone_with_no_canceller_is_framed_into_batches_as_it_came() {
        let (sink, heard) = collecting();
        let mut pipeline = Pipeline::new(Some(SAMPLE_RATE), None, false).unwrap();

        // Less than a batch is held back.
        pipeline.hear_microphone(&vec![0.25; BATCH_SAMPLES - 1], &sink);
        assert!(heard.lock().unwrap().is_empty());
        // The rest of one is sent, and the samples are not touched.
        pipeline.hear_microphone(&[0.25; 11], &sink);
        let heard = heard.lock().unwrap();
        let [EngineEvent::Microphone(batch)] = &heard[..] else {
            panic!("one batch, got {heard:?}");
        };
        assert_eq!(batch.len(), BATCH_SAMPLES + 10);
        assert!(batch.iter().all(|&s| s == f32_to_i16(0.25)));
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
        let refused = AudioEngine::start(options, sink).await.err().expect("nothing to open");
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
        let engine = AudioEngine::start(options, sink).await.expect("the devices open");
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
