//! Finding speech in a stream of audio.
//!
//! Silero, the voice detector the meeting notes use, wrapped so that whatever
//! is listening hears two things from it -- that somebody has started
//! ([`Detected::Started`], soon enough to react to) and that they have finished
//! ([`Detected::Ended`], with everything they said and when they began). How
//! long a pause ends a turn, and how long a turn may run before it is cut, is
//! a [`Tuning`] chosen by the caller: conversation wants the pause people leave
//! when they are done, a dictation or a long reading may want something else.
//!
//! # Two layers
//!
//! `Turns` decides what is a start and what is an end. It is pure: it takes a
//! `RawVad`, four things any voice detector can say, and so is tested here
//! with a scripted one and no model. `SileroVad` is the real `RawVad`, behind
//! feature `speech` like everything else that loads a native model.
//!
//! # Where an utterance began
//!
//! `Detected::Ended`'s `start_ms` counts from when the detector was made or last
//! [reset](SpeechDetector::reset), in milliseconds of audio it was given, not
//! of the clock. It is the segment's own start sample, which Silero places a
//! little before the first loud window to keep the onset of the first word, so
//! a transcript built from it is timed as the audio was heard. A stream that is
//! reset has a new zero, so a caller that needs times across a reset must keep
//! its own offset.
//!
//! # How soon a start counts
//!
//! A start needs speech to have gone on for a little while, and how little
//! depends on [`DetectorMode`]. With nothing playing ([`DetectorMode::Open`])
//! 120 ms will do. While this application's own voice is audible
//! ([`DetectorMode::Guarded`]) it takes 350 ms, and a segment that ends before
//! reaching that is thrown away: a click, a cough or what echo cancellation
//! left of the application's own voice is usually shorter than a word, and
//! being wrong about it means cutting the reply off for nothing.
//!
//! # Whether it is a voice at all
//!
//! Silero is sure of speech now and then over the sound of typing, a knock or
//! a chair, and a start that pauses the assistant's reply and a segment that
//! a recogniser then writes words for are too much to give to a keyboard. So
//! `Turns` asks [`voicing`] as well, window by window: does this sound have a
//! pitch, the way every vowel does? A start also needs some voiced sound in
//! the speech it has heard -- 64 ms of it when open, 128 ms when guarded, since
//! what is left of the assistant's own voice is the likelier thing to be
//! wrong about, and each counted afresh for every utterance -- and a segment
//! that ends with less than the tuning's
//! [`Tuning::min_voiced_seconds`] of it is dropped. If its start had been
//! reported it is closed with an empty [`Detected::Ended`], as below, so no
//! listener is left waiting. Voiced sound is counted the way [`voicing`]
//! counts it: a voiced window beside another voiced window, not one alone. A
//! `min_voiced_seconds` of zero turns all of this off, and nothing is measured
//! at all; so does `Turns::new`, which the tests of the rest use.
//!
//! # Where the detector is tuned for a turn rather than a word
//!
//! Silero itself keeps calling it speech through the quiet that ends a turn, so
//! `RawVad::speaking` is true for that tail too. A sound shorter than the
//! detector's own minimum is therefore a `Started` with no segment after it. It
//! is closed with an [`Detected::Ended`] holding no samples rather than left
//! open, since a listener that heard `Started` and then nothing would wait for
//! ever; hearing no words in it, it carries on as if nothing had happened. Its
//! `start_ms` is where the sound began.

use crate::error::CommandResult;
#[cfg(not(feature = "speech"))]
use crate::error::{CommandError, codes};
use crate::speech::{SAMPLE_RATE, pcm, voicing};
use tokio::sync::mpsc;

/// What a [`SpeechDetector`] noticed.
#[derive(Debug, Clone, PartialEq)]
pub enum Detected {
    /// Somebody has begun talking.
    Started,
    /// They have stopped, and this is everything they said.
    Ended {
        samples: Vec<i16>,
        /// Where it began, in milliseconds since the detector started or was
        /// last reset. See the module doc.
        start_ms: u64,
    },
}

/// How much it takes to count as somebody talking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectorMode {
    /// Nothing is playing.
    Open,
    /// Something this application is playing is audible, so a short sound is
    /// more likely to be what echo cancellation left behind.
    Guarded,
}

/// How a stream is cut into utterances: the three numbers that differ between
/// one use of the detector and another.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tuning {
    /// How long the quiet must last before a turn is over, in seconds. Longer
    /// than the pause somebody leaves between two clauses, or a sentence is
    /// cut in half; shorter, and the answer is slower than it need be.
    pub end_of_turn_seconds: f32,
    /// The shortest speech the detector will report at all, in seconds. Below
    /// this it is a click, whatever the mode.
    pub min_speech_seconds: f32,
    /// A turn this long is cut and a new one begun, in seconds. Somebody
    /// reading aloud does not pause, and a recogniser is better handed thirty
    /// seconds now than five minutes later.
    pub max_turn_seconds: f32,
    /// The least voiced sound -- the pitch of a vowel, see
    /// [`crate::speech::voicing`] -- an utterance must hold to be reported,
    /// in seconds, and what decides that a start is somebody talking rather
    /// than a keyboard or a knock. `0.0` reports anything Silero calls speech.
    pub min_voiced_seconds: f32,
}

impl Tuning {
    /// Talking with somebody: 1.2 s of quiet before the turn is over. People
    /// leave about 0.7 s when they are done, but they leave that much to
    /// think mid-sentence too, and 0.7 s answered them before they had
    /// finished. Half a second more on every answer is the lesser cost.
    ///
    /// One run of two voiced windows (64 ms) is enough voice: a bare "yes"
    /// may hold no more, and 0.12 s dropped most of a recorded one.
    pub const CONVERSATION: Tuning = Tuning {
        end_of_turn_seconds: 1.2,
        min_speech_seconds: 0.15,
        max_turn_seconds: 30.0,
        min_voiced_seconds: 0.064,
    };

    /// Dictating: people pause to think mid-sentence here too, though less
    /// rides on it -- an utterance only decides where the recogniser is
    /// handed audio, not where the text breaks. The same pause as
    /// [`Tuning::CONVERSATION`]; only the shortest sound that counts differs.
    pub const DICTATION: Tuning = Tuning {
        end_of_turn_seconds: 1.2,
        min_speech_seconds: 0.2,
        max_turn_seconds: 30.0,
        min_voiced_seconds: 0.064,
    };
}

/// Finds where speech begins and ends in a stream of audio.
pub trait SpeechDetector: Send {
    fn push(&mut self, samples: &[i16]) -> Vec<Detected>;
    fn set_mode(&mut self, mode: DetectorMode);
    /// Forget anything half heard, and start counting
    /// `Detected::Ended`'s `start_ms` from zero again.
    fn reset(&mut self);
}

/// What a detector's thread is told.
#[derive(Debug)]
pub enum ListenerInput {
    Audio(Vec<i16>),
    Mode(DetectorMode),
    Reset,
}

/// What a detector's thread reports, in the order it happened.
#[derive(Debug, Clone, PartialEq)]
pub enum Heard {
    /// The detector found something.
    Found(Detected),
    /// A [`ListenerInput::Reset`] has been carried out. Everything before it
    /// came from before the reset, and everything after from after, which a
    /// receiver on the far side of a channel cannot otherwise tell.
    Reset,
}

/// A detector on a thread of its own: inference never runs on the runtime.
/// Dropping `input` ends the thread.
pub struct Listener {
    pub input: mpsc::UnboundedSender<ListenerInput>,
    pub detected: mpsc::UnboundedReceiver<Detected>,
}

/// Run `detector` on a thread of its own, named so a profile shows it, and say
/// what it finds through `emit`.
///
/// `emit` is called on that thread, in order, and says whether anybody wants
/// more: when it returns `false` the thread ends. The thread also ends when the
/// returned sender and every clone of it are dropped. If the thread cannot be
/// made the sender still comes back, and sends to it fail, which a caller reads
/// as the end of its audio.
pub fn spawn_detector(
    mut detector: Box<dyn SpeechDetector>,
    mut emit: impl FnMut(Heard) -> bool + Send + 'static,
) -> mpsc::UnboundedSender<ListenerInput> {
    let (input, mut rx) = mpsc::unbounded_channel::<ListenerInput>();
    let spawned = std::thread::Builder::new().name("speech-detector".into()).spawn(move || {
        while let Some(message) = rx.blocking_recv() {
            match message {
                ListenerInput::Audio(samples) => {
                    for found in detector.push(&samples) {
                        if !emit(Heard::Found(found)) {
                            return;
                        }
                    }
                }
                ListenerInput::Mode(mode) => detector.set_mode(mode),
                ListenerInput::Reset => {
                    detector.reset();
                    if !emit(Heard::Reset) {
                        return;
                    }
                }
            }
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "could not start the speech detector's thread");
    }
    input
}

/// [`spawn_detector`] for a caller that wants only what was found, on a channel.
///
/// The thread also ends when nobody is listening to `detected` any more. If
/// the thread cannot be made the channels still come back: sends to `input`
/// fail and `detected` is closed.
pub fn spawn_listener(detector: Box<dyn SpeechDetector>) -> Listener {
    let (tx, detected) = mpsc::unbounded_channel();
    let input = spawn_detector(detector, move |heard| match heard {
        Heard::Found(found) => tx.send(found).is_ok(),
        Heard::Reset => true,
    });
    Listener { input, detected }
}

/// Silero, from the speech kit. Needs the kit downloaded and feature `speech`.
#[cfg(feature = "speech")]
pub fn build_detector(tuning: Tuning) -> CommandResult<Box<dyn SpeechDetector>> {
    use crate::error::{CommandError, codes};
    let paths = crate::local::files::speech_kit_paths().ok_or_else(|| {
        CommandError::new(
            codes::NOT_FOUND,
            "The speech kit is not on this computer yet. Download it in Settings → Models.",
        )
    })?;
    let config = super::local::vad_config(
        &paths.vad.to_string_lossy(),
        // One window at a time, as it arrives: more threads would buy nothing.
        1,
        tuning.end_of_turn_seconds,
        tuning.min_speech_seconds,
        tuning.max_turn_seconds,
    );
    let buffer_seconds = buffer_seconds(tuning);
    let vad = SileroVad::new(config, buffer_seconds)
        .ok_or_else(|| CommandError::new(codes::INTERNAL, "could not load the voice detector"))?;
    let voiced = voiced_samples(tuning.min_voiced_seconds);
    Ok(Box::new(Turns::with_voicing(vad, voiced, voicing::is_voiced)))
}

/// Without feature `speech` there is nothing to detect with.
#[cfg(not(feature = "speech"))]
pub fn build_detector(_tuning: Tuning) -> CommandResult<Box<dyn SpeechDetector>> {
    Err(CommandError::new(codes::UNSUPPORTED, "this build does not include local speech"))
}

/// What the detector may hold unread, in seconds: two maximum turns, since a
/// segment is held until it is popped and a turn may be cut while another is
/// still being heard.
#[cfg(feature = "speech")]
fn buffer_seconds(tuning: Tuning) -> f32 {
    2.0 * tuning.max_turn_seconds
}

/// The samples Silero is asked about at a time, 32 ms at 16 kHz. It accepts
/// no other size.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
const WINDOW: usize = 512;

/// How long speech must go on before a start is reported, in samples at
/// 16 kHz, with nothing playing: 120 ms.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
const OPEN_START_SAMPLES: usize = 1_920;

/// The same while the assistant is audible: 350 ms.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
const GUARDED_START_SAMPLES: usize = 5_600;

/// How much voiced sound the speech so far must hold before a start is
/// reported, with nothing playing: two windows, 64 ms. Enough that one
/// transient that looked like a pitch cannot do it, since sound that
/// counts as voiced comes in pairs of windows at the least.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
const OPEN_VOICED_SAMPLES: usize = 2 * WINDOW;

/// The same while the assistant is audible: four windows, 128 ms. What is
/// left of its own voice after echo cancellation is the likeliest thing to be
/// taken for a person's, and wrongly is the costly mistake.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
const GUARDED_VOICED_SAMPLES: usize = 4 * WINDOW;

/// [`Tuning::min_voiced_seconds`] as samples at the speech rate.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
fn voiced_samples(seconds: f32) -> usize {
    (seconds * SAMPLE_RATE as f32).round().max(0.0) as usize
}

/// One stretch of speech a [`RawVad`] has finished.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
struct Segment {
    /// Where it began, in samples of audio the detector has been given since it
    /// was made or last reset.
    start: u64,
    /// Scaled to -1.0..=1.0.
    samples: Vec<f32>,
}

/// What [`Turns`] needs of a voice detector: Silero, or a script in a test.
///
/// Fed one window at a time; what it has finished is taken with [`Self::pop`]
/// after each.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
trait RawVad: Send {
    /// Take one window of audio. [`WINDOW`] samples, scaled to -1.0..=1.0.
    fn accept(&mut self, window: &[f32]);
    /// Whether the detector is, as of the last window, inside speech.
    fn speaking(&self) -> bool;
    /// The oldest speech it has finished, if any.
    fn pop(&mut self) -> Option<Segment>;
    /// Forget everything, including what it has finished and not been asked
    /// for, and count [`Segment::start`] from zero again.
    fn reset(&mut self);
}

/// How much of the audio after a segment Silero ended is kept on it: 200 ms.
///
/// Silero ends a segment where it stops being sure of speech, which can be
/// before the last sound of the last word has died away -- a soft final
/// syllable, a name said alone. Kokoro saying "Siri." came back as "Si"
/// without it. A segment is only finished after the quiet that ends a turn,
/// so what follows it is that quiet, and there is always this much of it.
#[cfg(feature = "speech")]
const TAIL_SAMPLES: usize = 3_200;

/// How much of what was heard is kept for [`TAIL_SAMPLES`] to be taken from:
/// more than a turn's ending quiet ever is.
#[cfg(feature = "speech")]
const HEARD_SAMPLES: usize = 16_000 * 4;

/// Silero through sherpa-onnx.
#[cfg(feature = "speech")]
struct SileroVad {
    detector: sherpa_onnx::VoiceActivityDetector,
    /// What `detector` was made from, to make it again.
    config: sherpa_onnx::VadModelConfig,
    buffer_seconds: f32,
    /// The last [`HEARD_SAMPLES`] of what was accepted, for a segment's tail.
    heard: std::collections::VecDeque<f32>,
    /// Samples accepted since the detector was made or reset: where the end
    /// of `heard` is, on the clock a segment's start is read against.
    fed: u64,
}

#[cfg(feature = "speech")]
impl SileroVad {
    fn new(config: sherpa_onnx::VadModelConfig, buffer_seconds: f32) -> Option<Self> {
        let detector = sherpa_onnx::VoiceActivityDetector::create(&config, buffer_seconds)?;
        Some(Self {
            detector,
            config,
            buffer_seconds,
            heard: std::collections::VecDeque::with_capacity(HEARD_SAMPLES),
            fed: 0,
        })
    }

    /// Up to [`TAIL_SAMPLES`] of what was heard from sample `from` on.
    fn after(&self, from: u64) -> impl Iterator<Item = f32> + '_ {
        let first = self.fed - self.heard.len() as u64;
        let skip = from.saturating_sub(first);
        let take = if from < first { 0 } else { TAIL_SAMPLES };
        self.heard.iter().copied().skip(skip as usize).take(take)
    }
}

#[cfg(feature = "speech")]
impl RawVad for SileroVad {
    fn accept(&mut self, window: &[f32]) {
        self.detector.accept_waveform(window);
        self.heard.extend(window.iter().copied());
        let over = self.heard.len().saturating_sub(HEARD_SAMPLES);
        self.heard.drain(..over);
        self.fed += window.len() as u64;
    }

    fn speaking(&self) -> bool {
        self.detector.detected()
    }

    fn pop(&mut self) -> Option<Segment> {
        let front = self.detector.front()?;
        // Sherpa counts a segment's start in samples since the detector was
        // made, and the C++ it comes from does not say that a reset begins
        // the count again. A new detector does, for certain, and is a small
        // model, so a reset is a new one.
        let start = u64::try_from(front.start()).unwrap_or(0);
        let mut samples = front.samples().to_vec();
        samples.extend(self.after(start + samples.len() as u64));
        self.detector.pop();
        Some(Segment { start, samples })
    }

    fn reset(&mut self) {
        self.heard.clear();
        self.fed = 0;
        match sherpa_onnx::VoiceActivityDetector::create(&self.config, self.buffer_seconds) {
            Some(fresh) => self.detector = fresh,
            None => {
                // Forgetting is what matters; the times after it may be off.
                tracing::warn!("could not make the voice detector again; clearing it instead");
                self.detector.reset();
                self.detector.clear();
            }
        }
    }
}

/// Starts and ends of speech, from a [`RawVad`]. See the module doc.
#[cfg_attr(not(feature = "speech"), allow(dead_code))]
struct Turns<V: RawVad> {
    vad: V,
    mode: DetectorMode,
    /// Audio not yet a whole window.
    pending: Vec<f32>,
    /// How much speech, in samples, there has been without a break.
    run: usize,
    /// Whether this turn's `Started` has been sent.
    started: bool,
    /// Samples handed to the detector since it was made or last reset: the
    /// clock [`Segment::start`] is read against.
    fed: u64,
    /// Where on that clock the speech now going on began.
    began_at: u64,
    /// Whether a window of audio is voiced: [`voicing::is_voiced`], or in a
    /// test a script.
    voiced: fn(&[f32]) -> bool,
    /// The least voiced sound, in samples, a segment must hold to be
    /// reported. `0` reports anything the detector calls speech.
    min_voiced: usize,
    /// How much of the speech now going on, up to its start, has been voiced.
    /// Counted afresh whenever a run of speech begins and whenever a segment
    /// is finished, never carried from one utterance to the next.
    sustained: voicing::Sustained,
}

/// Every window is voiced: for a [`Turns`] with no gate.
#[cfg(test)]
fn every_window(_: &[f32]) -> bool {
    true
}

#[cfg_attr(not(feature = "speech"), allow(dead_code))]
impl<V: RawVad> Turns<V> {
    /// Starts and ends as `vad` finds them, with nothing asked of how the
    /// sound is voiced. For the tests of everything else; [`build_detector`]
    /// always has a gate, if only one of nothing.
    #[cfg(test)]
    fn new(vad: V) -> Self {
        Self::with_voicing(vad, 0, every_window)
    }

    /// As [`Self::new`], but a segment must hold `min_voiced_samples` of
    /// sound `voiced` says is voiced to be reported, and a start must hold
    /// some of it (see the module doc).
    fn with_voicing(vad: V, min_voiced_samples: usize, voiced: fn(&[f32]) -> bool) -> Self {
        Self {
            vad,
            mode: DetectorMode::Open,
            pending: Vec::with_capacity(WINDOW),
            run: 0,
            started: false,
            fed: 0,
            began_at: 0,
            voiced,
            min_voiced: min_voiced_samples,
            sustained: voicing::Sustained::default(),
        }
    }

    fn start_after(&self) -> usize {
        match self.mode {
            DetectorMode::Open => OPEN_START_SAMPLES,
            DetectorMode::Guarded => GUARDED_START_SAMPLES,
        }
    }

    /// The voiced sound the speech so far must hold for a start: what the mode
    /// asks, or nothing when the tuning asks nothing of a segment.
    fn start_voiced(&self) -> usize {
        if self.min_voiced == 0 {
            return 0;
        }
        match self.mode {
            DetectorMode::Open => OPEN_VOICED_SAMPLES,
            DetectorMode::Guarded => GUARDED_VOICED_SAMPLES,
        }
    }

    /// Feed one whole window and say what happened.
    fn window(&mut self, window: &[f32], out: &mut Vec<Detected>) {
        self.vad.accept(window);
        let speaking = self.vad.speaking();
        if speaking {
            if self.run == 0 {
                self.began_at = self.fed;
                self.sustained = voicing::Sustained::default();
            }
            self.run += window.len();
            // Once the start is out, how voiced the rest is no longer
            // matters here: the segment is measured when it is popped. And
            // with nothing asked of it there is nothing to measure.
            if !self.started && self.min_voiced > 0 {
                self.sustained.push((self.voiced)(window));
            }
        } else {
            self.run = 0;
        }
        if speaking
            && !self.started
            && self.run >= self.start_after()
            && self.sustained.windows() * WINDOW >= self.start_voiced()
        {
            self.started = true;
            out.push(Detected::Started);
        }

        let mut finished = false;
        while let Some(segment) = self.vad.pop() {
            finished = true;
            // Whatever followed is a new utterance, and has to show a voice
            // of its own: a keyboard going on after one that was heard does
            // not inherit its start.
            self.sustained = voicing::Sustained::default();
            if !self.started && self.mode == DetectorMode::Guarded {
                // Never reached the threshold: a click or an echo, not a
                // person.
                continue;
            }
            if !voicing::voiced_at_least(&segment.samples, self.min_voiced, self.voiced) {
                // Sound with no voice in it: typing, a knock, a chair. If a
                // start went out for it, take it back the one way there is.
                if self.started {
                    out.push(Detected::Ended {
                        samples: Vec::new(),
                        start_ms: pcm::duration_ms(segment.start as usize),
                    });
                    self.started = false;
                }
                continue;
            }
            if !self.started {
                // A short "yes" is over before 120 ms of it has been heard
                // as it happens, and is still an answer.
                out.push(Detected::Started);
            }
            out.push(Detected::Ended {
                samples: pcm::to_i16(&segment.samples),
                start_ms: pcm::duration_ms(segment.start as usize),
            });
            self.started = false;
        }

        // Started, and the detector let go of it without a segment: it was
        // under the detector's own minimum. Close it, empty.
        if !finished && self.started && !speaking {
            self.started = false;
            self.sustained = voicing::Sustained::default();
            out.push(Detected::Ended {
                samples: Vec::new(),
                start_ms: pcm::duration_ms(self.began_at as usize),
            });
        }
        self.fed += window.len() as u64;
    }
}

impl<V: RawVad> SpeechDetector for Turns<V> {
    fn push(&mut self, samples: &[i16]) -> Vec<Detected> {
        let mut out = Vec::new();
        self.pending.extend(pcm::to_f32(samples));
        let whole = self.pending.len() / WINDOW * WINDOW;
        if whole == 0 {
            return out;
        }
        let rest = self.pending.split_off(whole);
        let windows = std::mem::replace(&mut self.pending, rest);
        for window in windows.as_chunks::<WINDOW>().0 {
            self.window(window, &mut out);
        }
        out
    }

    fn set_mode(&mut self, mode: DetectorMode) {
        self.mode = mode;
    }

    fn reset(&mut self) {
        self.vad.reset();
        self.pending.clear();
        self.run = 0;
        self.started = false;
        self.fed = 0;
        self.began_at = 0;
        self.sustained = voicing::Sustained::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// One window's worth of what a scripted detector says.
    #[derive(Clone)]
    struct Step {
        speaking: bool,
        /// A segment that finishes with this window, and where it began.
        finishes: Option<(u64, Vec<f32>)>,
    }

    fn speech() -> Step {
        Step { speaking: true, finishes: None }
    }

    fn quiet() -> Step {
        Step { speaking: false, finishes: None }
    }

    fn ends_with(samples: Vec<f32>) -> Step {
        ends_at(0, samples)
    }

    fn ends_at(start: u64, samples: Vec<f32>) -> Step {
        Step { speaking: false, finishes: Some((start, samples)) }
    }

    /// Says what the script says, one step per window, and records resets.
    struct Scripted {
        script: VecDeque<Step>,
        speaking: bool,
        done: VecDeque<Segment>,
        resets: Arc<Mutex<usize>>,
    }

    impl Scripted {
        fn new(script: Vec<Step>) -> (Self, Arc<Mutex<usize>>) {
            let resets = Arc::new(Mutex::new(0));
            let vad = Self {
                script: script.into(),
                speaking: false,
                done: VecDeque::new(),
                resets: resets.clone(),
            };
            (vad, resets)
        }
    }

    impl RawVad for Scripted {
        fn accept(&mut self, window: &[f32]) {
            assert_eq!(window.len(), WINDOW);
            let step = self.script.pop_front().unwrap_or_else(quiet);
            self.speaking = step.speaking;
            if let Some((start, samples)) = step.finishes {
                self.done.push_back(Segment { start, samples });
            }
        }

        fn speaking(&self) -> bool {
            self.speaking
        }

        fn pop(&mut self) -> Option<Segment> {
            self.done.pop_front()
        }

        fn reset(&mut self) {
            self.done.clear();
            self.script.clear();
            self.speaking = false;
            *self.resets.lock().unwrap() += 1;
        }
    }

    fn windows(n: usize) -> Vec<i16> {
        vec![100; n * WINDOW]
    }

    fn repeat(step: Step, n: usize) -> Vec<Step> {
        vec![step; n]
    }

    fn segment() -> Vec<f32> {
        vec![0.5, -0.5, 0.25]
    }

    fn run(script: Vec<Step>, mode: DetectorMode, windows_in: usize) -> Vec<Detected> {
        let (vad, _) = Scripted::new(script);
        let mut turns = Turns::new(vad);
        turns.set_mode(mode);
        turns.push(&windows(windows_in))
    }

    #[test]
    fn speech_that_goes_on_starts_after_120_ms_when_open() {
        // 4 windows is 128 ms: the third (96 ms) is not enough, the fourth is.
        let mut script = repeat(speech(), 4);
        script.push(ends_with(segment()));
        let found = run(script, DetectorMode::Open, 5);
        assert_eq!(found[0], Detected::Started);
        assert_eq!(found.iter().filter(|d| **d == Detected::Started).count(), 1);
        assert_eq!(found.len(), 2);
        assert!(matches!(found[1], Detected::Ended { .. }));
    }

    #[test]
    fn nothing_starts_before_120_ms() {
        let found = run(repeat(speech(), 3), DetectorMode::Open, 3);
        assert!(found.is_empty());
    }

    #[test]
    fn guarded_waits_for_350_ms() {
        // 350 ms is 5600 samples, which is 11 windows (352 ms).
        let found = run(repeat(speech(), 10), DetectorMode::Guarded, 10);
        assert!(found.is_empty());
        let found = run(repeat(speech(), 11), DetectorMode::Guarded, 11);
        assert_eq!(found, vec![Detected::Started]);
    }

    #[test]
    fn started_is_sent_once_per_utterance() {
        let found = run(repeat(speech(), 40), DetectorMode::Open, 40);
        assert_eq!(found, vec![Detected::Started]);
    }

    #[test]
    fn a_finished_segment_is_ended_with_its_samples() {
        let mut script = repeat(speech(), 6);
        script.push(ends_with(vec![0.5, -1.0, 0.0]));
        let found = run(script, DetectorMode::Open, 7);
        let ended = Detected::Ended { samples: vec![16384, -32768, 0], start_ms: 0 };
        assert_eq!(found, vec![Detected::Started, ended]);
    }

    #[test]
    fn a_very_short_segment_still_starts_when_open() {
        // Over before 120 ms: "yes".
        let script = vec![speech(), ends_with(segment())];
        let found = run(script, DetectorMode::Open, 2);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0], Detected::Started);
        assert!(matches!(found[1], Detected::Ended { .. }));
    }

    #[test]
    fn a_very_short_segment_is_dropped_when_guarded() {
        let script = vec![speech(), speech(), ends_with(segment())];
        let found = run(script, DetectorMode::Guarded, 3);
        assert!(found.is_empty());
    }

    #[test]
    fn a_segment_after_the_guarded_threshold_is_kept() {
        let mut script = repeat(speech(), 12);
        script.push(ends_with(segment()));
        let found = run(script, DetectorMode::Guarded, 13);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0], Detected::Started);
        assert!(matches!(found[1], Detected::Ended { .. }));
    }

    #[test]
    fn switching_to_open_mid_speech_starts_at_the_open_threshold() {
        let (vad, _) = Scripted::new(repeat(speech(), 10));
        let mut turns = Turns::new(vad);
        turns.set_mode(DetectorMode::Guarded);
        assert!(turns.push(&windows(5)).is_empty());
        turns.set_mode(DetectorMode::Open);
        assert_eq!(turns.push(&windows(1)), vec![Detected::Started]);
    }

    #[test]
    fn a_second_utterance_starts_again() {
        let mut script = repeat(speech(), 5);
        script.push(ends_with(segment()));
        script.extend(repeat(quiet(), 3));
        script.extend(repeat(speech(), 5));
        script.push(ends_with(segment()));
        let found = run(script, DetectorMode::Open, 15);
        let starts = found.iter().filter(|d| **d == Detected::Started).count();
        let ends = found.iter().filter(|d| matches!(d, Detected::Ended { .. })).count();
        assert_eq!((starts, ends), (2, 2));
    }

    #[test]
    fn a_start_the_detector_lets_go_of_is_closed_empty() {
        let mut script = repeat(speech(), 5);
        script.push(quiet());
        let found = run(script, DetectorMode::Open, 6);
        // Speech from the first window: it began with the stream.
        assert_eq!(
            found,
            vec![Detected::Started, Detected::Ended { samples: Vec::new(), start_ms: 0 }]
        );
    }

    #[test]
    fn a_segment_is_timed_from_its_own_start_sample() {
        // One second in, at 16 kHz.
        let mut script = repeat(speech(), 6);
        script.push(ends_at(16_000, vec![0.5]));
        let found = run(script, DetectorMode::Open, 7);
        let ended = Detected::Ended { samples: vec![16384], start_ms: 1_000 };
        assert_eq!(found, vec![Detected::Started, ended]);
    }

    #[test]
    fn an_empty_end_is_timed_from_where_the_sound_began() {
        // Four quiet windows, then five of sound the detector let go of.
        let mut script = repeat(quiet(), 4);
        script.extend(repeat(speech(), 5));
        script.push(quiet());
        let found = run(script, DetectorMode::Open, 10);
        // 4 * 512 samples is 128 ms.
        let ended = Detected::Ended { samples: Vec::new(), start_ms: 128 };
        assert_eq!(found, vec![Detected::Started, ended]);
    }

    #[test]
    fn after_a_reset_times_count_from_zero_again() {
        let (vad, _) = Scripted::new(repeat(quiet(), 4));
        let mut turns = Turns::new(vad);
        assert!(turns.push(&windows(4)).is_empty());
        turns.reset();
        let mut script = repeat(quiet(), 2);
        script.extend(repeat(speech(), 5));
        script.push(quiet());
        turns.vad.script = script.into();
        let found = turns.push(&windows(8));
        // Two windows after the reset, not six: 64 ms.
        let ended = Detected::Ended { samples: Vec::new(), start_ms: 64 };
        assert_eq!(found, vec![Detected::Started, ended]);
    }

    #[test]
    fn conversation_waits_out_a_pause_to_think() {
        let t = Tuning::CONVERSATION;
        assert_eq!(
            (t.end_of_turn_seconds, t.min_speech_seconds, t.max_turn_seconds),
            (1.2, 0.15, 30.0)
        );
    }

    #[test]
    fn a_whole_window_is_not_needed_per_push() {
        let (vad, _) = Scripted::new(repeat(speech(), 10));
        let mut turns = Turns::new(vad);
        // 100 samples at a time: nothing is fed until 512 have gathered.
        let mut found = Vec::new();
        for _ in 0..(5 * WINDOW / 100 + 1) {
            found.extend(turns.push(&[100; 100]));
        }
        assert_eq!(found, vec![Detected::Started]);
    }

    #[test]
    fn leftover_samples_carry_into_the_next_push() {
        let (vad, _) = Scripted::new(repeat(speech(), 4));
        let mut turns = Turns::new(vad);
        assert!(turns.push(&[0; WINDOW - 1]).is_empty());
        assert_eq!(turns.pending.len(), WINDOW - 1);
        turns.push(&[0; 1]);
        assert!(turns.pending.is_empty());
    }

    #[test]
    fn reset_forgets_partial_windows_and_the_utterance() {
        let (vad, resets) = Scripted::new(repeat(speech(), 20));
        let mut turns = Turns::new(vad);
        assert_eq!(turns.push(&windows(5)), vec![Detected::Started]);
        turns.push(&[0; 200]);
        turns.reset();
        assert_eq!(*resets.lock().unwrap(), 1);
        assert!(turns.pending.is_empty());
        assert_eq!(turns.run, 0);
        assert!(!turns.started);
    }

    #[test]
    fn after_a_reset_a_new_utterance_starts_again() {
        let (vad, _) = Scripted::new(Vec::new());
        let mut turns = Turns::new(vad);
        turns.started = true;
        turns.run = 9_999;
        turns.reset();
        turns.vad.script = repeat(speech(), 5).into();
        assert_eq!(turns.push(&windows(5)), vec![Detected::Started]);
    }

    /// A detector that answers each push with a scripted number of starts.
    struct Counting {
        pushed: Arc<Mutex<Vec<usize>>>,
        modes: Arc<Mutex<Vec<DetectorMode>>>,
        resets: Arc<Mutex<usize>>,
    }

    impl SpeechDetector for Counting {
        fn push(&mut self, samples: &[i16]) -> Vec<Detected> {
            self.pushed.lock().unwrap().push(samples.len());
            vec![Detected::Started, Detected::Ended { samples: samples.to_vec(), start_ms: 0 }]
        }

        fn set_mode(&mut self, mode: DetectorMode) {
            self.modes.lock().unwrap().push(mode);
        }

        fn reset(&mut self) {
            *self.resets.lock().unwrap() += 1;
        }
    }

    #[tokio::test]
    async fn the_listener_passes_audio_modes_and_resets_to_its_thread() {
        let pushed = Arc::new(Mutex::new(Vec::new()));
        let modes = Arc::new(Mutex::new(Vec::new()));
        let resets = Arc::new(Mutex::new(0));
        let detector =
            Counting { pushed: pushed.clone(), modes: modes.clone(), resets: resets.clone() };
        let mut listener = spawn_listener(Box::new(detector));

        listener.input.send(ListenerInput::Mode(DetectorMode::Guarded)).unwrap();
        listener.input.send(ListenerInput::Audio(vec![1, 2, 3])).unwrap();
        listener.input.send(ListenerInput::Reset).unwrap();
        listener.input.send(ListenerInput::Audio(vec![4])).unwrap();

        assert_eq!(listener.detected.recv().await, Some(Detected::Started));
        let first = Detected::Ended { samples: vec![1, 2, 3], start_ms: 0 };
        assert_eq!(listener.detected.recv().await, Some(first));
        assert_eq!(listener.detected.recv().await, Some(Detected::Started));
        let second = Detected::Ended { samples: vec![4], start_ms: 0 };
        assert_eq!(listener.detected.recv().await, Some(second));
        assert_eq!(*pushed.lock().unwrap(), vec![3, 1]);
        assert_eq!(*modes.lock().unwrap(), vec![DetectorMode::Guarded]);
        assert_eq!(*resets.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn dropping_the_input_ends_the_thread_and_closes_detected() {
        let detector =
            Counting { pushed: Arc::default(), modes: Arc::default(), resets: Arc::default() };
        let Listener { input, mut detected } = spawn_listener(Box::new(detector));
        drop(input);
        assert_eq!(detected.recv().await, None);
    }

    #[tokio::test]
    async fn a_reset_is_reported_in_its_place_among_what_was_found() {
        let detector =
            Counting { pushed: Arc::default(), modes: Arc::default(), resets: Arc::default() };
        let (tx, mut rx) = mpsc::unbounded_channel();
        let input = spawn_detector(Box::new(detector), move |heard| tx.send(heard).is_ok());
        input.send(ListenerInput::Audio(vec![1])).unwrap();
        input.send(ListenerInput::Reset).unwrap();
        input.send(ListenerInput::Audio(vec![2])).unwrap();
        drop(input);
        let mut heard = Vec::new();
        while let Some(h) = rx.recv().await {
            heard.push(h);
        }
        let ended = |samples| Heard::Found(Detected::Ended { samples, start_ms: 0 });
        assert_eq!(
            heard,
            vec![
                Heard::Found(Detected::Started),
                ended(vec![1]),
                Heard::Reset,
                Heard::Found(Detected::Started),
                ended(vec![2]),
            ]
        );
    }

    #[tokio::test]
    async fn the_thread_ends_when_emit_says_nobody_is_listening() {
        let resets = Arc::new(Mutex::new(0));
        let detector =
            Counting { pushed: Arc::default(), modes: Arc::default(), resets: resets.clone() };
        let input = spawn_detector(Box::new(detector), |_| false);
        input.send(ListenerInput::Audio(vec![1])).unwrap();
        // The thread returns after the first thing it found; its receiver is
        // dropped with it, so a send that arrives later fails.
        for _ in 0..200 {
            if input.is_closed() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(input.is_closed());
    }

    // What the voicing gate does, with the real `voicing::is_voiced` over
    // synthetic sound, and a scripted Silero that calls all of it speech.

    use crate::speech::voicing::signals::{noise, typing, voice};

    /// The conversation tuning's gate, in samples: what these tests are for
    /// is what the assistant really does, so it is read from the tuning.
    fn gate() -> usize {
        voiced_samples(Tuning::CONVERSATION.min_voiced_seconds)
    }

    /// A gate asking more of a segment than any mode asks of a start, to reach
    /// what happens to a start that is then not kept.
    const STRICT: usize = 4 * WINDOW;

    /// The first `n` windows of `signal`.
    fn first(signal: Vec<f32>, n: usize) -> Vec<f32> {
        assert!(signal.len() >= n * WINDOW);
        signal[..n * WINDOW].to_vec()
    }

    /// `n` windows of a voice.
    fn talking(n: usize) -> Vec<f32> {
        first(voice(150.0, Some(600.0), 2.0, 0.1, 21), n)
    }

    /// `n` windows of a keyboard.
    fn tapping(n: usize) -> Vec<f32> {
        first(typing(3.0, (2_000.0, 4_000.0), 0.3, 22), n)
    }

    /// `n` windows of room noise.
    fn hissing(n: usize) -> Vec<f32> {
        first(noise(1.0, 0.05, 23), n)
    }

    /// Feed `audio` through a gated `Turns`, over a Silero that says `script`.
    fn gated(
        script: Vec<Step>,
        audio: &[f32],
        mode: DetectorMode,
        min_voiced: usize,
    ) -> Vec<Detected> {
        let (vad, _) = Scripted::new(script);
        let mut turns = Turns::with_voicing(vad, min_voiced, voicing::is_voiced);
        turns.set_mode(mode);
        turns.push(&pcm::to_i16(audio))
    }

    /// `n` windows of speech, then a segment of `audio`, as Silero finishes
    /// one.
    fn over(audio: &[f32]) -> Vec<Step> {
        let n = audio.len() / WINDOW;
        let mut script = repeat(speech(), n - 1);
        script.push(ends_with(audio.to_vec()));
        script
    }

    fn ended_with_samples(found: &[Detected]) -> bool {
        found.iter().any(|d| matches!(d, Detected::Ended { samples, .. } if !samples.is_empty()))
    }

    #[test]
    fn typing_silero_calls_speech_never_starts_or_ends() {
        for mode in [DetectorMode::Open, DetectorMode::Guarded] {
            let audio = tapping(60);
            let found = gated(over(&audio), &audio, mode, gate());
            assert!(found.is_empty(), "{mode:?}: {found:?}");
        }
    }

    #[test]
    fn room_noise_silero_calls_speech_never_starts_or_ends() {
        let audio = hissing(30);
        assert!(gated(over(&audio), &audio, DetectorMode::Open, gate()).is_empty());
    }

    #[test]
    fn a_voice_starts_and_ends_with_its_samples() {
        for mode in [DetectorMode::Open, DetectorMode::Guarded] {
            let audio = talking(31);
            let found = gated(over(&audio), &audio, mode, gate());
            assert_eq!(found.len(), 2, "{mode:?}: {found:?}");
            assert_eq!(found[0], Detected::Started);
            assert!(
                matches!(&found[1], Detected::Ended { samples, .. } if samples.len() == audio.len())
            );
        }
    }

    #[test]
    fn a_voice_with_typing_over_it_is_still_heard() {
        let clicks = first(typing(3.0, (2_000.0, 4_000.0), 0.3, 24), 47);
        let audio: Vec<f32> = talking(47).iter().zip(&clicks).map(|(a, b)| a + b).collect();
        let found = gated(over(&audio), &audio, DetectorMode::Open, gate());
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(ended_with_samples(&found));
    }

    #[test]
    fn a_start_with_too_little_voiced_sound_is_closed_empty() {
        // Two windows of voice, then six of noise: enough to start on, not to
        // keep (this gate is stricter than the real one). The segment began one
        // second in.
        let mut audio = talking(2);
        audio.extend(hissing(6));
        let mut script = repeat(speech(), 7);
        script.push(ends_at(16_000, audio.clone()));
        let found = gated(script, &audio, DetectorMode::Open, STRICT);
        let closed = Detected::Ended { samples: Vec::new(), start_ms: 1_000 };
        assert_eq!(found, vec![Detected::Started, closed]);
    }

    #[test]
    fn a_closed_start_lets_the_next_utterance_start_again() {
        let mut audio = talking(2);
        audio.extend(hissing(6));
        let mut script = repeat(speech(), 7);
        script.push(ends_with(audio.clone()));
        script.extend(repeat(quiet(), 2));
        let spoken = talking(20);
        script.extend(over(&spoken));
        audio.extend(hissing(2));
        audio.extend(spoken.clone());
        let found = gated(script, &audio, DetectorMode::Open, STRICT);
        let starts = found.iter().filter(|d| **d == Detected::Started).count();
        assert_eq!(starts, 2, "{found:?}");
        assert!(ended_with_samples(&found));
    }

    #[test]
    fn a_short_answer_with_a_voice_in_it_starts_and_ends() {
        // "yes": over before 120 ms has been heard as it happens, but three
        // windows of it are voiced.
        let audio = talking(3);
        let script = vec![speech(), ends_with(audio.clone())];
        let found = gated(script, &audio, DetectorMode::Open, gate());
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0], Detected::Started);
        assert!(ended_with_samples(&found));
    }

    #[test]
    fn a_short_sound_with_no_voice_in_it_is_nothing_at_all() {
        let audio = tapping(4);
        let script = vec![speech(), ends_with(audio.clone())];
        assert!(gated(script, &audio, DetectorMode::Open, gate()).is_empty());
    }

    #[test]
    fn guarded_asks_for_more_voiced_sound_than_open() {
        // Twenty windows of speech: long enough for either mode. Two of them
        // are voiced, which is all open asks and half what guarded does.
        let mut two = talking(2);
        two.extend(hissing(18));
        let steps = || repeat(speech(), 20);
        assert_eq!(gated(steps(), &two, DetectorMode::Open, gate()), vec![Detected::Started]);
        assert!(gated(steps(), &two, DetectorMode::Guarded, gate()).is_empty());

        let mut four = talking(4);
        four.extend(hissing(16));
        assert_eq!(gated(steps(), &four, DetectorMode::Guarded, gate()), vec![Detected::Started]);
    }

    #[test]
    fn a_lone_voiced_window_does_not_start_anything() {
        // A voiced window with noise either side is one transient, not a
        // vowel.
        let mut audio = hissing(2);
        audio.extend(talking(1));
        audio.extend(hissing(17));
        assert!(gated(repeat(speech(), 20), &audio, DetectorMode::Open, gate()).is_empty());
    }

    #[test]
    fn a_gate_of_nothing_asks_nothing_of_a_start_and_measures_nothing() {
        fn not_looked_at(_: &[f32]) -> bool {
            panic!("a gate of nothing should not look at the sound");
        }
        for (mode, windows) in [(DetectorMode::Open, 4), (DetectorMode::Guarded, 11)] {
            let audio = hissing(20);
            let (vad, _) = Scripted::new(over(&audio));
            let mut turns = Turns::with_voicing(vad, 0, not_looked_at);
            turns.set_mode(mode);
            let found = turns.push(&pcm::to_i16(&audio));
            assert_eq!(found.len(), 2, "{mode:?}: {found:?}");
            assert_eq!(found[0], Detected::Started);
            assert!(ended_with_samples(&found));
            // The start came as soon as the run was long enough, as ever.
            let (vad, _) = Scripted::new(repeat(speech(), windows));
            let mut turns = Turns::with_voicing(vad, 0, not_looked_at);
            turns.set_mode(mode);
            assert_eq!(
                turns.push(&pcm::to_i16(&audio[..windows * WINDOW])),
                vec![Detected::Started]
            );
        }
    }

    #[test]
    fn a_keyboard_after_an_utterance_that_was_heard_does_not_inherit_its_start() {
        // Silero cuts a long turn and goes on hearing speech: the next
        // utterance is typing, and has to earn its own start.
        let spoken = talking(8);
        let mut audio = spoken.clone();
        audio.extend(tapping(8));
        let mut script = repeat(speech(), 7);
        script.push(Step { speaking: true, finishes: Some((0, spoken)) });
        script.extend(repeat(speech(), 8));
        let found = gated(script, &audio, DetectorMode::Open, gate());
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0], Detected::Started);
        assert!(ended_with_samples(&found));
    }

    #[test]
    fn a_gate_of_nothing_hears_typing_as_before() {
        let audio = tapping(40);
        let found = gated(over(&audio), &audio, DetectorMode::Open, 0);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0], Detected::Started);
        assert!(ended_with_samples(&found));
    }

    #[test]
    fn a_detector_made_without_a_gate_hears_typing_too() {
        let audio = tapping(40);
        let (vad, _) = Scripted::new(over(&audio));
        let mut turns = Turns::new(vad);
        let found = turns.push(&pcm::to_i16(&audio));
        assert_eq!(found.len(), 2, "{found:?}");
    }

    #[test]
    fn a_reset_forgets_the_voiced_sound_heard_so_far() {
        let audio = talking(3);
        let (vad, _) = Scripted::new(repeat(speech(), 3));
        let mut turns = Turns::with_voicing(vad, gate(), voicing::is_voiced);
        turns.push(&pcm::to_i16(&audio));
        assert!(turns.sustained.windows() > 0);
        turns.reset();
        assert_eq!(turns.sustained.windows(), 0);
    }

    #[test]
    fn the_tunings_gates_are_in_samples_at_the_speech_rate() {
        // One run of two 512-sample windows: the least a "yes" may hold.
        assert_eq!(voiced_samples(Tuning::CONVERSATION.min_voiced_seconds), 1_024);
        assert_eq!(voiced_samples(Tuning::DICTATION.min_voiced_seconds), 1_024);
        assert_eq!(voiced_samples(0.0), 0);
        assert_eq!(voiced_samples(-1.0), 0);
    }

    #[cfg(feature = "speech")]
    #[test]
    fn the_sherpa_window_is_the_one_turns_feeds() {
        let config = crate::speech::local::vad_config("model.onnx", 1, 0.7, 0.15, 30.0);
        assert_eq!(config.silero_vad.window_size, WINDOW as i32);
    }

    #[cfg(not(feature = "speech"))]
    #[test]
    fn without_the_speech_feature_there_is_no_detector() {
        let err = build_detector(Tuning::CONVERSATION).err().expect("an error");
        assert_eq!(err.code, codes::UNSUPPORTED);
    }
}
