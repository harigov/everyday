//! Words from a stream of audio, as they are said.
//!
//! A [`LiveTranscription`] is a detector and a transcriber joined up. Audio goes
//! in through a [`LiveControl`], in whatever pieces the device hands it over;
//! what comes out is a stream of [`LiveEvent`]s, one utterance after another,
//! each with where in the stream it was said. Nothing here knows where the audio
//! comes from or what is done with the words, so one stream serves a voice
//! conversation and a transcript alike, and a caller with two sources (a
//! microphone and what comes out of a call, say) starts one for each.
//!
//! # What a caller sees
//!
//! Utterances are numbered from 1, in the order they began. For each:
//!
//! 1. [`LiveEvent::Started`] at once, while the person is still talking, so a
//!    caller can react without waiting for any words.
//! 2. [`LiveEvent::Ended`] at once when they stop, with where it began and
//!    ended.
//! 3. [`LiveEvent::Transcribed`] when the words are ready.
//!
//! Transcription is one utterance at a time, in the order they ended, so
//! `Transcribed` events arrive in utterance order however long each took: a
//! short "yes" after a long answer waits for the answer. An utterance with
//! nothing in it is `Transcribed` with `Ok("")`, in its place in that order,
//! without asking the recogniser. A recogniser that panics is an `Err` for that
//! utterance and the next one carries on.
//!
//! # Times
//!
//! `start_ms` and `end_ms` are milliseconds of audio fed to the stream, from the
//! first sample or from the last [`LiveControl::reset`], not wall-clock time.
//! A caller that keeps one transcript across resets keeps its own offset.
//!
//! # Reset
//!
//! [`LiveControl::reset`] forgets what is half heard: the detector's state, and
//! the audio it was holding for an utterance not yet over. What has already
//! ended is not forgotten. Its `Ended` has been announced, so a caller expects
//! its words, and they still arrive.
//!
//! An utterance that had started and not ended when the reset happened is
//! closed by it, so nobody is left waiting for an end that will not come: it is
//! given an `Ended` with no length, then a `Transcribed` of `Ok("")`, in order
//! with the rest. Its times are those of the last thing heard before the reset
//! and mean nothing else.
//!
//! # Stopping
//!
//! Drop every [`LiveControl`] and the stream is over: what was heard is still
//! transcribed and delivered, then `events` yields `None`. Drop `events` and the
//! work stops at once, apart from the detector's thread, which notices at the
//! next thing it finds or the next [`LiveControl::feed`] after that.

use super::detect::{self, Detected, DetectorMode, Heard, ListenerInput, SpeechDetector};
use super::pcm;
use super::utterance::UtteranceTranscriber;
use crate::error::{CommandError, CommandResult, codes};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinHandle};

/// What a [`LiveTranscription`] reports, in the order things happened. See the
/// module doc for what a caller can rely on.
#[derive(Debug)]
pub enum LiveEvent {
    /// Somebody has begun talking.
    Started { utterance: u64 },
    /// They have stopped; their words are being worked out.
    Ended { utterance: u64, start_ms: u64, end_ms: u64 },
    /// What they said. Empty when it turned out to be nothing.
    Transcribed { utterance: u64, start_ms: u64, end_ms: u64, result: CommandResult<String> },
}

/// The sending side, which can be cloned and handed to whatever has the audio.
///
/// Every method returns at once and never fails: if the stream has ended, what
/// is sent is dropped, which the caller finds out from `events` closing.
#[derive(Clone)]
pub struct LiveControl {
    input: mpsc::UnboundedSender<ListenerInput>,
}

impl LiveControl {
    /// More audio at [`SAMPLE_RATE`](super::SAMPLE_RATE).
    pub fn feed(&self, samples: Vec<i16>) {
        let _ = self.input.send(ListenerInput::Audio(samples));
    }

    /// How much it takes to count as somebody talking, from here on.
    pub fn set_mode(&self, mode: DetectorMode) {
        let _ = self.input.send(ListenerInput::Mode(mode));
    }

    /// Forget anything half heard. See the module doc for what that leaves.
    pub fn reset(&self) {
        let _ = self.input.send(ListenerInput::Reset);
    }
}

/// A stream of audio being turned into words.
pub struct LiveTranscription {
    pub control: LiveControl,
    pub events: mpsc::UnboundedReceiver<LiveEvent>,
}

impl LiveTranscription {
    /// Start, on the current Tokio runtime: the detector on a thread of its own,
    /// and one task that numbers what it finds and has it transcribed.
    pub fn start(
        detector: Box<dyn SpeechDetector>,
        transcriber: Arc<dyn UtteranceTranscriber>,
    ) -> Self {
        let (events_tx, events) = mpsc::unbounded_channel();
        let (heard_tx, heard) = mpsc::unbounded_channel();
        let input = detect::spawn_detector(detector, move |found| heard_tx.send(found).is_ok());
        tokio::spawn(work(heard, events_tx, transcriber));
        Self { control: LiveControl { input }, events }
    }
}

/// An utterance waiting its turn to be transcribed.
struct Job {
    utterance: u64,
    start_ms: u64,
    end_ms: u64,
    samples: Vec<i16>,
}

/// The utterance being transcribed now, on a task of its own so that a panic in
/// the recogniser is a result here rather than the end of everything.
struct Running {
    job: Job,
    task: JoinHandle<CommandResult<String>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        // Nobody is left to read the answer.
        self.task.abort();
    }
}

/// Wait for the running job, or for ever if there is none.
async fn running_result(running: &mut Option<Running>) -> Result<CommandResult<String>, JoinError> {
    match running {
        Some(running) => (&mut running.task).await,
        None => std::future::pending().await,
    }
}

/// What a recogniser that panicked, or was cut off, leaves for the utterance.
fn failed(e: JoinError) -> CommandError {
    tracing::warn!(error = %e, "speech recognition failed");
    CommandError::new(codes::INTERNAL, "The speech recogniser stopped unexpectedly.")
}

/// Number what the detector finds, tell the caller at once, and have each
/// utterance transcribed in turn.
async fn work(
    mut heard: mpsc::UnboundedReceiver<Heard>,
    events: mpsc::UnboundedSender<LiveEvent>,
    transcriber: Arc<dyn UtteranceTranscriber>,
) {
    let mut next_id = 1_u64;
    // Started, and not yet ended.
    let mut open: Option<u64> = None;
    // How far into the stream the last thing heard reached.
    let mut clock_ms = 0_u64;
    let mut waiting: VecDeque<Job> = VecDeque::new();
    let mut running: Option<Running> = None;
    let mut audio_over = false;

    loop {
        // The next job, if the last has finished. One that is empty is answered
        // here and now, in its place in the order, and the one after it looked
        // at straight away.
        while running.is_none() {
            let Some(job) = waiting.pop_front() else { break };
            if job.samples.is_empty() {
                if !send_transcribed(&events, &job, Ok(String::new())) {
                    return;
                }
                continue;
            }
            let transcriber = transcriber.clone();
            let mut job = job;
            let samples = std::mem::take(&mut job.samples);
            let task = tokio::spawn(async move { transcriber.hear(&samples).await });
            running = Some(Running { job, task });
        }
        if audio_over && running.is_none() {
            return;
        }

        tokio::select! {
            _ = events.closed() => return,
            found = heard.recv(), if !audio_over => match found {
                None => audio_over = true,
                Some(Heard::Found(Detected::Started)) => {
                    if open.is_none() {
                        let utterance = next_id;
                        next_id += 1;
                        open = Some(utterance);
                        if events.send(LiveEvent::Started { utterance }).is_err() {
                            return;
                        }
                    }
                }
                Some(Heard::Found(Detected::Ended { samples, start_ms })) => {
                    // A detector that ends what it never started has still
                    // heard somebody: say so, so that every end has a start.
                    let utterance = match open.take() {
                        Some(utterance) => utterance,
                        None => {
                            let utterance = next_id;
                            next_id += 1;
                            if events.send(LiveEvent::Started { utterance }).is_err() {
                                return;
                            }
                            utterance
                        }
                    };
                    let end_ms = start_ms + pcm::duration_ms(samples.len());
                    clock_ms = clock_ms.max(end_ms);
                    let ended = LiveEvent::Ended { utterance, start_ms, end_ms };
                    if events.send(ended).is_err() {
                        return;
                    }
                    waiting.push_back(Job { utterance, start_ms, end_ms, samples });
                }
                Some(Heard::Reset) => {
                    if let Some(utterance) = open.take() {
                        let ended =
                            LiveEvent::Ended { utterance, start_ms: clock_ms, end_ms: clock_ms };
                        if events.send(ended).is_err() {
                            return;
                        }
                        waiting.push_back(Job {
                            utterance,
                            start_ms: clock_ms,
                            end_ms: clock_ms,
                            samples: Vec::new(),
                        });
                    }
                    // The detector counts from zero again.
                    clock_ms = 0;
                }
            },
            done = running_result(&mut running) => {
                if let Some(finished) = running.take() {
                    let result = match done {
                        Ok(result) => result,
                        Err(e) => Err(failed(e)),
                    };
                    if !send_transcribed(&events, &finished.job, result) {
                        return;
                    }
                }
            }
        }
    }
}

/// Report what an utterance said. False when nobody is listening.
fn send_transcribed(
    events: &mpsc::UnboundedSender<LiveEvent>,
    job: &Job,
    result: CommandResult<String>,
) -> bool {
    let event = LiveEvent::Transcribed {
        utterance: job.utterance,
        start_ms: job.start_ms,
        end_ms: job.end_ms,
        result,
    };
    events.send(event).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speech::transcribe::BoxFuture;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Notify;

    /// A detector that answers each push with the next thing it was told to
    /// say, whatever the audio, and records what it was asked.
    struct Scripted {
        steps: VecDeque<Vec<Detected>>,
        resets: Arc<AtomicUsize>,
        modes: Arc<Mutex<Vec<DetectorMode>>>,
        dropped: Arc<AtomicBool>,
    }

    struct Watch {
        resets: Arc<AtomicUsize>,
        modes: Arc<Mutex<Vec<DetectorMode>>>,
        dropped: Arc<AtomicBool>,
    }

    fn detector(steps: Vec<Vec<Detected>>) -> (Box<dyn SpeechDetector>, Watch) {
        let watch =
            Watch { resets: Arc::default(), modes: Arc::default(), dropped: Arc::default() };
        let detector = Scripted {
            steps: steps.into(),
            resets: watch.resets.clone(),
            modes: watch.modes.clone(),
            dropped: watch.dropped.clone(),
        };
        (Box::new(detector), watch)
    }

    impl SpeechDetector for Scripted {
        fn push(&mut self, _samples: &[i16]) -> Vec<Detected> {
            self.steps.pop_front().unwrap_or_default()
        }

        fn set_mode(&mut self, mode: DetectorMode) {
            self.modes.lock().unwrap().push(mode);
        }

        fn reset(&mut self) {
            self.resets.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Drop for Scripted {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    fn ended(first: i16, len: usize, start_ms: u64) -> Detected {
        Detected::Ended { samples: vec![first; len], start_ms }
    }

    /// Says "said N" for an utterance of N, except that 1 waits to be let go,
    /// 98 fails and 99 panics.
    struct Fake {
        gate: Arc<Notify>,
        calls: AtomicUsize,
        running: AtomicUsize,
        peak: AtomicUsize,
    }

    impl Fake {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                gate: Arc::new(Notify::new()),
                calls: AtomicUsize::new(0),
                running: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
            })
        }
    }

    impl UtteranceTranscriber for Fake {
        fn hear<'a>(&'a self, samples: &'a [i16]) -> BoxFuture<'a, CommandResult<String>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak.fetch_max(now, Ordering::SeqCst);
                let first = samples[0];
                if first == 1 {
                    self.gate.notified().await;
                }
                if first == 99 {
                    panic!("the recogniser blew up");
                }
                self.running.fetch_sub(1, Ordering::SeqCst);
                if first == 98 {
                    return Err(CommandError::new(codes::INTERNAL, "no good"));
                }
                Ok(format!("said {first}"))
            })
        }
    }

    async fn next(events: &mut mpsc::UnboundedReceiver<LiveEvent>) -> Option<LiveEvent> {
        tokio::time::timeout(Duration::from_secs(5), events.recv()).await.expect("an event")
    }

    /// The events as short strings: "S1" started, "E1 500-1500" ended, "T1
    /// 500-1500 said 1" transcribed.
    fn show(event: &LiveEvent) -> String {
        match event {
            LiveEvent::Started { utterance } => format!("S{utterance}"),
            LiveEvent::Ended { utterance, start_ms, end_ms } => {
                format!("E{utterance} {start_ms}-{end_ms}")
            }
            LiveEvent::Transcribed { utterance, start_ms, end_ms, result } => {
                let said = match result {
                    Ok(text) => format!("said {text:?}"),
                    Err(e) => format!("failed {}", e.message),
                };
                format!("T{utterance} {start_ms}-{end_ms} {said}")
            }
        }
    }

    async fn take(events: &mut mpsc::UnboundedReceiver<LiveEvent>, n: usize) -> Vec<String> {
        let mut shown = Vec::new();
        for _ in 0..n {
            shown.push(show(&next(events).await.expect("an event")));
        }
        shown
    }

    fn feed_n(control: &LiveControl, n: usize) {
        for _ in 0..n {
            control.feed(vec![0]);
        }
    }

    #[tokio::test]
    async fn starts_and_ends_go_out_at_once_and_words_follow_in_order() {
        // 16 000 samples is a second; 1 600 a tenth.
        let steps = vec![
            vec![Detected::Started],
            vec![ended(1, 16_000, 500)],
            vec![Detected::Started, ended(2, 1_600, 3_000)],
        ];
        let (detector, _) = detector(steps);
        let fake = Fake::new();
        let live = LiveTranscription::start(detector, fake.clone());
        let LiveTranscription { control, mut events } = live;
        feed_n(&control, 3);

        // The first is held up in the recogniser; the second, short, is not
        // transcribed ahead of it, but everything else about it is announced.
        let before = take(&mut events, 4).await;
        assert_eq!(before, ["S1", "E1 500-1500", "S2", "E2 3000-3100"]);

        fake.gate.notify_one();
        let after = take(&mut events, 2).await;
        assert_eq!(after, [r#"T1 500-1500 said "said 1""#, r#"T2 3000-3100 said "said 2""#]);
        assert_eq!(fake.peak.load(Ordering::SeqCst), 1, "never two at once");
    }

    #[tokio::test]
    async fn an_empty_utterance_is_nothing_and_asks_nobody() {
        let steps = vec![vec![Detected::Started], vec![ended(0, 0, 700)]];
        let (detector, _) = detector(steps);
        let fake = Fake::new();
        let live = LiveTranscription::start(detector, fake.clone());
        let LiveTranscription { control, mut events } = live;
        feed_n(&control, 2);
        let shown = take(&mut events, 3).await;
        assert_eq!(shown, ["S1", "E1 700-700", r#"T1 700-700 said """#]);
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_empty_utterance_waits_for_the_ones_before_it() {
        let steps = vec![
            vec![Detected::Started, ended(1, 1_600, 0)],
            vec![Detected::Started, ended(0, 0, 900)],
        ];
        let (detector, _) = detector(steps);
        let fake = Fake::new();
        let live = LiveTranscription::start(detector, fake.clone());
        let LiveTranscription { control, mut events } = live;
        feed_n(&control, 2);
        assert_eq!(take(&mut events, 4).await, ["S1", "E1 0-100", "S2", "E2 900-900"]);
        fake.gate.notify_one();
        let after = take(&mut events, 2).await;
        assert_eq!(after, [r#"T1 0-100 said "said 1""#, r#"T2 900-900 said """#]);
    }

    #[tokio::test]
    async fn a_failure_and_a_panic_are_results_and_the_next_one_still_comes() {
        let steps = vec![
            vec![Detected::Started, ended(98, 1_600, 0)],
            vec![Detected::Started, ended(99, 1_600, 1_000)],
            vec![Detected::Started, ended(3, 1_600, 2_000)],
        ];
        let (detector, _) = detector(steps);
        let live = LiveTranscription::start(detector, Fake::new());
        let LiveTranscription { control, mut events } = live;
        feed_n(&control, 3);
        let shown = take(&mut events, 9).await;
        let transcribed: Vec<&String> = shown.iter().filter(|s| s.starts_with('T')).collect();
        assert_eq!(transcribed[0], "T1 0-100 failed no good");
        assert!(transcribed[1].starts_with("T2 1000-1100 failed "), "{}", transcribed[1]);
        assert_eq!(transcribed[2], r#"T3 2000-2100 said "said 3""#);
    }

    #[tokio::test]
    async fn a_reset_closes_an_utterance_that_has_not_ended() {
        let steps = vec![vec![Detected::Started], vec![Detected::Started, ended(4, 1_600, 250)]];
        let (detector, watch) = detector(steps);
        let live = LiveTranscription::start(detector, Fake::new());
        let LiveTranscription { control, mut events } = live;
        control.set_mode(DetectorMode::Guarded);
        control.feed(vec![0]);
        control.reset();
        control.feed(vec![0]);
        let shown = take(&mut events, 6).await;
        // Closed with no length and no words, then numbering carries on.
        assert_eq!(
            shown,
            [
                "S1",
                "E1 0-0",
                r#"T1 0-0 said """#,
                "S2",
                "E2 250-350",
                r#"T2 250-350 said "said 4""#,
            ]
        );
        assert_eq!(watch.resets.load(Ordering::SeqCst), 1);
        assert_eq!(*watch.modes.lock().unwrap(), vec![DetectorMode::Guarded]);
    }

    #[tokio::test]
    async fn a_reset_does_not_forget_what_has_already_ended() {
        let steps = vec![vec![Detected::Started, ended(1, 1_600, 400)]];
        let (detector, _) = detector(steps);
        let fake = Fake::new();
        let live = LiveTranscription::start(detector, fake.clone());
        let LiveTranscription { control, mut events } = live;
        control.feed(vec![0]);
        assert_eq!(take(&mut events, 2).await, ["S1", "E1 400-500"]);
        control.reset();
        fake.gate.notify_one();
        assert_eq!(take(&mut events, 1).await, [r#"T1 400-500 said "said 1""#]);
    }

    #[tokio::test]
    async fn dropping_every_control_ends_the_stream_after_what_was_heard() {
        let steps = vec![vec![Detected::Started, ended(5, 1_600, 0)]];
        let (detector, _) = detector(steps);
        let live = LiveTranscription::start(detector, Fake::new());
        let LiveTranscription { control, mut events } = live;
        let second = control.clone();
        control.feed(vec![0]);
        drop(control);
        drop(second);
        assert_eq!(take(&mut events, 3).await, ["S1", "E1 0-100", r#"T1 0-100 said "said 5""#]);
        assert!(next(&mut events).await.is_none());
    }

    #[tokio::test]
    async fn dropping_the_events_stops_the_detectors_thread() {
        let steps = vec![vec![Detected::Started], vec![Detected::Started]];
        let (detector, watch) = detector(steps);
        let live = LiveTranscription::start(detector, Fake::new());
        let LiveTranscription { control, events } = live;
        drop(events);
        // The thread learns nobody is listening when it next has something to
        // say.
        for _ in 0..500 {
            control.feed(vec![0]);
            if watch.dropped.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(watch.dropped.load(Ordering::SeqCst));
    }
}
