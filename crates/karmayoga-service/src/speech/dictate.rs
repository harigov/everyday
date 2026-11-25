//! Dictation: talking instead of typing.
//!
//! One session per stretch of dictation: microphone audio in, the words so far
//! out. Built on a [`LiveTranscription`], so each pause the person leaves is the
//! end of an utterance and its words are added to what was heard; nothing here
//! decides what the words are for. Mail's "write by voice" is the first user.
//!
//! A session ends when it is told to stop, when nobody has said anything for
//! [`SILENCE_LIMIT`], or when it has run for [`TIME_LIMIT`] -- and, when its
//! transport asks for it ([`DictationInput::EndAfterPause`]), when somebody
//! who has said something pauses for long enough: voice typing's "pause to
//! finish". All are counted in audio fed, not wall-clock time, so the tests
//! can run them in a moment. The words of an utterance still being transcribed when it stops are
//! waited for, so nothing said is lost.
//!
//! Nothing is kept: the audio goes to the recogniser and is dropped.
//!
//! See `docs/plans/listen.md`.

use crate::error::CommandResult;
use crate::service::Service;
use crate::speech::SAMPLE_RATE;
use crate::speech::live::{LiveEvent, LiveTranscription};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// Quiet this long, after the start or after the last thing said, ends it.
pub const SILENCE_LIMIT: Duration = Duration::from_secs(15);
/// The longest a session runs.
pub const TIME_LIMIT: Duration = Duration::from_secs(180);

/// What a session is doing, for the button on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DictationState {
    /// Waiting for the person to talk.
    Listening,
    /// The person is talking.
    Hearing,
    /// Stopped; the last words are being worked out.
    Finishing,
}

/// Why a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DictationEnd {
    /// Asked to stop: the words are wanted.
    Stopped,
    /// Nobody said anything for [`SILENCE_LIMIT`].
    Silence,
    /// It ran for [`TIME_LIMIT`].
    TimeLimit,
    /// Somebody said something and then paused for as long as
    /// [`DictationInput::EndAfterPause`] said would end it.
    Paused,
    /// Asked to cancel: the words are not wanted.
    Cancelled,
    /// The microphone or the recogniser failed.
    Failed,
}

/// What a session tells the interface. On the wire to the webview.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DictationEvent {
    State {
        state: DictationState,
    },
    /// Everything heard so far, utterances joined with a space.
    Heard {
        text: String,
    },
    /// Something went wrong with one utterance, and the session carries on.
    Trouble {
        message: String,
    },
    /// The session is over. `text` is everything heard (empty when
    /// cancelled); `error` says what failed when `reason` is `Failed`.
    Ended {
        reason: DictationEnd,
        text: String,
        error: Option<String>,
    },
}

/// What a transport hands a session.
#[derive(Debug)]
pub enum DictationInput {
    /// Microphone audio at [`SAMPLE_RATE`](crate::speech::SAMPLE_RATE).
    Audio(Vec<i16>),
    /// Audio heard before the session opened -- kept by a transport that
    /// opened the microphone first, while the speech model loaded. Listened to
    /// like the rest, but not counted towards [`SILENCE_LIMIT`]: it arrives all
    /// at once, far ahead of the detector, which has not yet had the chance to
    /// say anybody was talking in it, and a long load would otherwise end the
    /// session for quiet in the middle of somebody's sentence.
    Backlog(Vec<i16>),
    /// Stop listening, and finish with what was heard.
    Stop,
    /// From now on, once something has been heard, end the session
    /// ([`DictationEnd::Paused`]) when the quiet after the last thing said has
    /// lasted this long; `None` waits to be told to stop again. The quiet is
    /// counted from the end of an utterance, which the detector reports only
    /// after a quiet of its own (the end of turn in
    /// [`Tuning::DICTATION`](crate::speech::detect::Tuning::DICTATION)), so the
    /// person has been quiet for that and this together. Nothing said, or only
    /// sounds with no words in them, never ends it this way: the person is
    /// still finding their first words, and [`SILENCE_LIMIT`] is for that.
    EndAfterPause(Option<Duration>),
    /// Stop listening, and throw away what was heard.
    Cancel,
    /// The microphone failed, in words for the person.
    Failed(String),
}

/// Both ends of a running session, from the transport's side. The session
/// ends with [`DictationEvent::Ended`], after which `events` closes.
pub struct DictationLink {
    pub input: tokio::sync::mpsc::UnboundedSender<DictationInput>,
    pub events: tokio::sync::mpsc::UnboundedReceiver<DictationEvent>,
}

/// How long a finished session waits for the words of what was said last. A
/// recogniser that has not answered by then is not going to, and the person
/// is better off with what there is than with a button that never comes back.
const FINISH_WAIT: Duration = Duration::from_secs(20);

/// Silence fed after stopping in the middle of a sentence, in seconds. Longer
/// than the quiet [`Tuning::DICTATION`] waits for before it ends an utterance,
/// so that what was half said is closed the way it would have been, and its
/// words are transcribed rather than dropped with the stream.
const TRAILING_QUIET_SECONDS: usize = 2;

/// Start a session over `live`, on the current Tokio runtime.
pub fn spawn(live: LiveTranscription) -> DictationLink {
    let (input, input_rx) = tokio::sync::mpsc::unbounded_channel();
    let (events_tx, events) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(run(live, input_rx, events_tx));
    DictationLink { input, events }
}

/// Samples in a stretch of audio, to the millisecond.
fn samples_in(span: Duration) -> u64 {
    (span.as_millis() as u64) * u64::from(SAMPLE_RATE) / 1000
}

/// What a session has heard, and how it is going.
struct Session {
    events: UnboundedSender<DictationEvent>,
    /// Utterances so far, joined with a space.
    text: String,
    /// Somebody is talking: between a `Started` and its `Ended`.
    hearing: bool,
    /// Audio fed in all.
    fed: u64,
    /// Audio fed since the start or the last time anything began or ended.
    quiet: u64,
    /// How much quiet after something said ends the session, in samples:
    /// [`DictationInput::EndAfterPause`].
    pause: Option<u64>,
}

impl Session {
    /// Tell the interface. False when nobody is listening any more.
    fn send(&self, event: DictationEvent) -> bool {
        self.events.send(event).is_ok()
    }

    /// Take in what the recogniser made of an utterance.
    fn transcribed(&mut self, result: CommandResult<String>) -> bool {
        match result {
            Ok(words) => {
                let words = words.trim();
                if words.is_empty() {
                    return true;
                }
                if !self.text.is_empty() {
                    self.text.push(' ');
                }
                self.text.push_str(words);
                self.send(DictationEvent::Heard { text: self.text.clone() })
            }
            Err(e) => self.send(DictationEvent::Trouble { message: e.message }),
        }
    }

    /// Somebody has said something and then been quiet for as long as ends
    /// it.
    fn paused(&self) -> bool {
        self.pause
            .is_some_and(|pause| !self.hearing && !self.text.is_empty() && self.quiet >= pause)
    }

    /// The end of the session, with the words or without.
    fn end(&self, reason: DictationEnd, error: Option<String>) {
        let text =
            if reason == DictationEnd::Cancelled { String::new() } else { self.text.clone() };
        let _ = self.send(DictationEvent::Ended { reason, text, error });
    }
}

/// The session: audio in, words out, until it is over.
async fn run(
    live: LiveTranscription,
    mut input: UnboundedReceiver<DictationInput>,
    events: UnboundedSender<DictationEvent>,
) {
    let LiveTranscription { control, events: mut live_events } = live;
    let mut session =
        Session { events, text: String::new(), hearing: false, fed: 0, quiet: 0, pause: None };
    let silence_limit = samples_in(SILENCE_LIMIT);
    let time_limit = samples_in(TIME_LIMIT);
    if !session.send(DictationEvent::State { state: DictationState::Listening }) {
        return;
    }

    // Why it is finishing, and what went wrong if something did.
    let (reason, error) = loop {
        tokio::select! {
            message = input.recv() => match message {
                // The transport went away: nobody is waiting for the words.
                None | Some(DictationInput::Cancel) => {
                    session.end(DictationEnd::Cancelled, None);
                    return;
                }
                Some(DictationInput::Stop) => break (DictationEnd::Stopped, None),
                Some(DictationInput::EndAfterPause(pause)) => {
                    session.pause = pause.map(samples_in);
                }
                Some(DictationInput::Failed(message)) => {
                    break (DictationEnd::Failed, Some(message));
                }
                Some(DictationInput::Backlog(samples)) => {
                    session.fed += samples.len() as u64;
                    control.feed(samples);
                }
                Some(DictationInput::Audio(samples)) => {
                    let len = samples.len() as u64;
                    control.feed(samples);
                    session.fed += len;
                    if !session.hearing {
                        session.quiet += len;
                    }
                    if session.fed >= time_limit {
                        break (DictationEnd::TimeLimit, None);
                    }
                    if !session.hearing && session.quiet >= silence_limit {
                        break (DictationEnd::Silence, None);
                    }
                    // Checked as the audio comes rather than when the words
                    // do: the quiet is what decides, and words that arrive
                    // after it has run out end it at the next piece of audio.
                    if session.paused() {
                        break (DictationEnd::Paused, None);
                    }
                }
            },
            event = live_events.recv() => match event {
                // The recogniser's side is gone, which it does only when it
                // has nothing more to say; there is nothing left to listen with.
                None => break (
                    DictationEnd::Failed,
                    Some("The speech recogniser stopped unexpectedly.".to_string()),
                ),
                Some(LiveEvent::Started { .. }) => {
                    session.hearing = true;
                    session.quiet = 0;
                    if !session.send(DictationEvent::State { state: DictationState::Hearing }) {
                        return;
                    }
                }
                Some(LiveEvent::Ended { .. }) => {
                    session.hearing = false;
                    session.quiet = 0;
                    if !session.send(DictationEvent::State { state: DictationState::Listening }) {
                        return;
                    }
                }
                Some(LiveEvent::Transcribed { result, .. }) => {
                    if !session.transcribed(result) {
                        return;
                    }
                }
            },
        }
    };

    if !session.send(DictationEvent::State { state: DictationState::Finishing }) {
        return;
    }
    // Somebody mid-sentence is given the quiet that ends it, ahead of the
    // stream closing: a stream that closes does not finish what it is holding.
    // Always, not only while `hearing`: the detector runs behind the audio, so
    // a phrase begun just before Stop has often not been reported as begun yet,
    // and was lost (voice typing let go right after a last short phrase, in
    // `voice_real.rs`). Quiet fed after quiet costs a moment of the detector.
    for _ in 0..TRAILING_QUIET_SECONDS {
        control.feed(vec![0; SAMPLE_RATE as usize]);
    }
    drop(control);

    let waited = tokio::time::timeout(FINISH_WAIT, async {
        let mut input_open = true;
        loop {
            tokio::select! {
                message = input.recv(), if input_open => match message {
                    Some(DictationInput::Cancel) => return false,
                    // Audio and the rest no longer matter; a closed channel is
                    // not a reason to throw away what is nearly done.
                    Some(_) => {}
                    None => input_open = false,
                },
                event = live_events.recv() => match event {
                    None => return true,
                    Some(LiveEvent::Transcribed { result, .. }) => {
                        if !session.transcribed(result) {
                            return false;
                        }
                    }
                    Some(_) => {}
                },
            }
        }
    })
    .await;
    match waited {
        Ok(true) | Err(_) => session.end(reason, error),
        Ok(false) => session.end(DictationEnd::Cancelled, None),
    }
}

impl Service {
    /// Start dictating: the chosen speech model behind a detector tuned for
    /// dictation. Refuses, in a sentence, when listening is not set up.
    pub async fn dictation_open(self: &Arc<Self>) -> CommandResult<DictationLink> {
        let live = self
            .live_transcription(
                crate::speech::detect::Tuning::DICTATION,
                crate::speech::expect::Occasion::Dictation,
                crate::speech::access::HeardFrom::ThisComputer,
            )
            .await?;
        Ok(spawn(live))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{CommandError, codes};
    use crate::speech::detect::{Detected, DetectorMode, SpeechDetector};
    use crate::speech::transcribe::BoxFuture;
    use crate::speech::utterance::UtteranceTranscriber;
    use std::collections::VecDeque;
    use tokio::sync::Notify;

    /// A detector that answers each push with the next thing it was told to
    /// say, whatever the audio.
    struct Scripted {
        steps: VecDeque<Vec<Detected>>,
    }

    impl SpeechDetector for Scripted {
        fn push(&mut self, _samples: &[i16]) -> Vec<Detected> {
            self.steps.pop_front().unwrap_or_default()
        }
        fn set_mode(&mut self, _mode: DetectorMode) {}
        fn reset(&mut self) {}
    }

    /// A detector that starts on the first thing it hears and, like the real
    /// one, ends only once it has heard a second of quiet twice over.
    struct NeedsQuiet {
        started: bool,
        quiet_seconds: usize,
    }

    impl SpeechDetector for NeedsQuiet {
        fn push(&mut self, samples: &[i16]) -> Vec<Detected> {
            if !self.started {
                self.started = true;
                return vec![Detected::Started];
            }
            if samples.len() >= SAMPLE_RATE as usize && samples.iter().all(|s| *s == 0) {
                self.quiet_seconds += 1;
            }
            if self.quiet_seconds >= 2 {
                self.quiet_seconds = 0;
                self.started = false;
                return vec![Detected::Ended { samples: vec![7; 1_600], start_ms: 0 }];
            }
            Vec::new()
        }
        fn set_mode(&mut self, _mode: DetectorMode) {}
        fn reset(&mut self) {}
    }

    /// Says "said N" for an utterance of N, except that 1 waits to be let go
    /// and 98 fails.
    struct Fake {
        gate: Arc<Notify>,
    }

    impl UtteranceTranscriber for Fake {
        fn hear<'a>(&'a self, samples: &'a [i16]) -> BoxFuture<'a, CommandResult<String>> {
            Box::pin(async move {
                let first = samples[0];
                if first == 1 {
                    self.gate.notified().await;
                }
                if first == 98 {
                    return Err(CommandError::new(codes::INTERNAL, "no good"));
                }
                Ok(format!("said {first}"))
            })
        }
    }

    fn ended(first: i16, start_ms: u64) -> Detected {
        Detected::Ended { samples: vec![first; 1_600], start_ms }
    }

    fn open(detector: Box<dyn SpeechDetector>) -> (DictationLink, Arc<Notify>) {
        let gate = Arc::new(Notify::new());
        let live = LiveTranscription::start(detector, Arc::new(Fake { gate: gate.clone() }));
        (spawn(live), gate)
    }

    fn scripted(steps: Vec<Vec<Detected>>) -> Box<dyn SpeechDetector> {
        Box::new(Scripted { steps: steps.into() })
    }

    async fn next(link: &mut DictationLink) -> Option<DictationEvent> {
        tokio::time::timeout(Duration::from_secs(5), link.events.recv()).await.expect("an event")
    }

    /// A little audio, which is all a scripted detector needs to take its
    /// next step.
    fn blip(link: &DictationLink) {
        link.input.send(DictationInput::Audio(vec![0; 160])).unwrap();
    }

    /// Audio of this long, in one piece.
    fn audio(link: &DictationLink, span: Duration) {
        link.input.send(DictationInput::Audio(vec![0; samples_in(span) as usize])).unwrap();
    }

    async fn until_state(link: &mut DictationLink, wanted: DictationState) {
        loop {
            match next(link).await.expect("the session ended first") {
                DictationEvent::State { state } if state == wanted => return,
                DictationEvent::Ended { reason, .. } => panic!("ended ({reason:?}) not {wanted:?}"),
                _ => {}
            }
        }
    }

    async fn until_heard(link: &mut DictationLink, wanted: &str) {
        loop {
            match next(link).await.expect("the session ended first") {
                DictationEvent::Heard { text } if text == wanted => return,
                DictationEvent::Ended { reason, .. } => panic!("ended ({reason:?}) not {wanted:?}"),
                _ => {}
            }
        }
    }

    /// Everything up to and including the end, and then that the channel is
    /// closed.
    async fn finish(link: &mut DictationLink) -> (DictationEnd, String, Option<String>) {
        let ended = loop {
            if let DictationEvent::Ended { reason, text, error } =
                next(link).await.expect("the session closed without an end")
            {
                break (reason, text, error);
            }
        };
        assert!(next(link).await.is_none(), "nothing follows the end");
        ended
    }

    #[tokio::test]
    async fn it_starts_listening_and_hearing_follows_the_voice() {
        let (mut link, _) = open(scripted(vec![vec![Detected::Started], vec![ended(2, 0)]]));
        let first = next(&mut link).await.unwrap();
        assert!(matches!(first, DictationEvent::State { state: DictationState::Listening }));
        blip(&link);
        until_state(&mut link, DictationState::Hearing).await;
        blip(&link);
        until_state(&mut link, DictationState::Listening).await;
    }

    #[tokio::test]
    async fn words_accumulate_across_utterances_and_stop_keeps_them() {
        let steps = vec![
            vec![Detected::Started],
            vec![ended(2, 0)],
            vec![Detected::Started, ended(3, 4_000)],
        ];
        let (mut link, _) = open(scripted(steps));
        for _ in 0..3 {
            blip(&link);
        }
        until_heard(&mut link, "said 2").await;
        until_heard(&mut link, "said 2 said 3").await;
        link.input.send(DictationInput::Stop).unwrap();
        let (reason, text, error) = finish(&mut link).await;
        assert_eq!(reason, DictationEnd::Stopped);
        assert_eq!(text, "said 2 said 3");
        assert_eq!(error, None);
    }

    #[tokio::test]
    async fn an_utterance_with_nothing_in_it_adds_nothing() {
        let steps = vec![vec![Detected::Started, Detected::Ended { samples: vec![], start_ms: 0 }]];
        let (mut link, _) = open(scripted(steps));
        blip(&link);
        until_state(&mut link, DictationState::Hearing).await;
        until_state(&mut link, DictationState::Listening).await;
        link.input.send(DictationInput::Stop).unwrap();
        let (reason, text, _) = finish(&mut link).await;
        assert_eq!((reason, text.as_str()), (DictationEnd::Stopped, ""));
    }

    #[tokio::test]
    async fn quiet_for_the_silence_limit_ends_it_and_keeps_what_was_said() {
        let (mut link, _) = open(scripted(vec![vec![Detected::Started, ended(2, 0)]]));
        blip(&link);
        until_heard(&mut link, "said 2").await;
        audio(&link, SILENCE_LIMIT);
        let (reason, text, error) = finish(&mut link).await;
        assert_eq!(reason, DictationEnd::Silence);
        assert_eq!(text, "said 2");
        assert_eq!(error, None);
    }

    #[tokio::test]
    async fn a_pause_after_words_ends_it_once_asked_to() {
        let (mut link, _) = open(scripted(vec![vec![Detected::Started, ended(2, 0)]]));
        link.input.send(DictationInput::EndAfterPause(Some(Duration::from_millis(800)))).unwrap();
        blip(&link);
        until_heard(&mut link, "said 2").await;
        // Not yet: quiet, but not for long enough.
        audio(&link, Duration::from_millis(500));
        let early = tokio::time::timeout(Duration::from_millis(100), async {
            loop {
                if let Some(DictationEvent::Ended { .. }) = link.events.recv().await {
                    return;
                }
            }
        })
        .await;
        assert!(early.is_err(), "it ended before the pause was long enough");
        audio(&link, Duration::from_millis(400));
        let (reason, text, error) = finish(&mut link).await;
        assert_eq!((reason, text.as_str(), error), (DictationEnd::Paused, "said 2", None));
    }

    #[tokio::test]
    async fn without_being_asked_a_pause_ends_nothing() {
        let (mut link, _) = open(scripted(vec![vec![Detected::Started, ended(2, 0)]]));
        blip(&link);
        until_heard(&mut link, "said 2").await;
        audio(&link, Duration::from_secs(5));
        link.input.send(DictationInput::Stop).unwrap();
        let (reason, _, _) = finish(&mut link).await;
        assert_eq!(reason, DictationEnd::Stopped);
    }

    #[tokio::test]
    async fn quiet_before_any_words_is_not_the_pause_that_ends_it() {
        // Nothing said, then a sound with no words in it.
        let steps =
            vec![vec![], vec![Detected::Started, Detected::Ended { samples: vec![], start_ms: 0 }]];
        let (mut link, _) = open(scripted(steps));
        link.input.send(DictationInput::EndAfterPause(Some(Duration::from_millis(500)))).unwrap();
        audio(&link, Duration::from_secs(3));
        blip(&link);
        until_state(&mut link, DictationState::Hearing).await;
        until_state(&mut link, DictationState::Listening).await;
        audio(&link, Duration::from_secs(3));
        link.input.send(DictationInput::Stop).unwrap();
        let (reason, text, _) = finish(&mut link).await;
        assert_eq!((reason, text.as_str()), (DictationEnd::Stopped, ""));
    }

    #[tokio::test]
    async fn talking_again_before_the_pause_runs_out_carries_on() {
        let steps = vec![
            vec![Detected::Started, ended(2, 0)],
            vec![],
            vec![Detected::Started],
            vec![],
            vec![ended(3, 4_000)],
        ];
        let (mut link, _) = open(scripted(steps));
        link.input.send(DictationInput::EndAfterPause(Some(Duration::from_secs(1)))).unwrap();
        blip(&link);
        until_heard(&mut link, "said 2").await;
        audio(&link, Duration::from_millis(600));
        blip(&link);
        until_state(&mut link, DictationState::Hearing).await;
        // A long sentence: talking is not a pause.
        audio(&link, Duration::from_secs(5));
        blip(&link);
        until_heard(&mut link, "said 2 said 3").await;
        audio(&link, Duration::from_millis(1100));
        let (reason, text, _) = finish(&mut link).await;
        assert_eq!((reason, text.as_str()), (DictationEnd::Paused, "said 2 said 3"));
    }

    #[tokio::test]
    async fn audio_from_before_the_session_opened_is_not_counted_as_quiet() {
        // Twenty seconds kept while the model loaded, then a little more: no
        // fifteen seconds of quiet as far as the session is concerned.
        let (mut link, _) = open(scripted(vec![]));
        let kept = vec![0; samples_in(Duration::from_secs(20)) as usize];
        link.input.send(DictationInput::Backlog(kept)).unwrap();
        audio(&link, Duration::from_secs(1));
        link.input.send(DictationInput::Stop).unwrap();
        let (reason, _, _) = finish(&mut link).await;
        assert_eq!(reason, DictationEnd::Stopped);
    }

    #[tokio::test]
    async fn quiet_from_the_very_start_ends_it_with_nothing() {
        let (mut link, _) = open(scripted(vec![]));
        audio(&link, SILENCE_LIMIT);
        let (reason, text, _) = finish(&mut link).await;
        assert_eq!((reason, text.as_str()), (DictationEnd::Silence, ""));
    }

    #[tokio::test]
    async fn speech_starts_the_quiet_counting_again_and_is_not_itself_counted() {
        // Quiet for ten seconds, a long stretch of talk, then ten seconds more:
        // never fifteen of quiet in a row, though more than fifteen in all.
        let steps = vec![vec![], vec![Detected::Started], vec![], vec![ended(2, 0)]];
        let (mut link, _) = open(scripted(steps));
        audio(&link, Duration::from_secs(10));
        blip(&link);
        until_state(&mut link, DictationState::Hearing).await;
        audio(&link, Duration::from_secs(60));
        blip(&link);
        until_heard(&mut link, "said 2").await;
        audio(&link, Duration::from_secs(10));
        link.input.send(DictationInput::Stop).unwrap();
        let (reason, text, _) = finish(&mut link).await;
        assert_eq!((reason, text.as_str()), (DictationEnd::Stopped, "said 2"));
    }

    #[tokio::test]
    async fn running_for_the_time_limit_ends_it_even_while_somebody_is_talking() {
        let steps = vec![vec![Detected::Started, ended(2, 0), Detected::Started]];
        let (mut link, _) = open(scripted(steps));
        blip(&link);
        // The second start is announced ahead of the first one's words, so by
        // the time they are in, somebody is talking again.
        until_heard(&mut link, "said 2").await;
        audio(&link, TIME_LIMIT);
        let (reason, text, _) = finish(&mut link).await;
        assert_eq!((reason, text.as_str()), (DictationEnd::TimeLimit, "said 2"));
    }

    #[tokio::test]
    async fn stop_waits_for_the_words_of_the_last_utterance() {
        // The recogniser holds utterance 1 until it is let go.
        let (mut link, gate) = open(scripted(vec![vec![Detected::Started, ended(1, 0)]]));
        blip(&link);
        until_state(&mut link, DictationState::Hearing).await;
        until_state(&mut link, DictationState::Listening).await;
        link.input.send(DictationInput::Stop).unwrap();
        until_state(&mut link, DictationState::Finishing).await;
        let early = tokio::time::timeout(Duration::from_millis(150), link.events.recv()).await;
        assert!(early.is_err(), "it ended before the words were in: {early:?}");

        gate.notify_one();
        until_heard(&mut link, "said 1").await;
        let (reason, text, _) = finish(&mut link).await;
        assert_eq!((reason, text.as_str()), (DictationEnd::Stopped, "said 1"));
    }

    #[tokio::test]
    async fn stopping_in_the_middle_of_a_sentence_still_gets_its_words() {
        let (mut link, _) = open(Box::new(NeedsQuiet { started: false, quiet_seconds: 0 }));
        blip(&link);
        until_state(&mut link, DictationState::Hearing).await;
        link.input.send(DictationInput::Stop).unwrap();
        let (reason, text, _) = finish(&mut link).await;
        assert_eq!((reason, text.as_str()), (DictationEnd::Stopped, "said 7"));
    }

    #[tokio::test]
    async fn stopping_before_the_detector_has_said_anything_began_still_gets_the_words() {
        // Stop straight after the audio, without waiting for `Hearing`: the
        // detector may not have reported the start yet when Stop is read.
        let (mut link, _) = open(Box::new(NeedsQuiet { started: false, quiet_seconds: 0 }));
        blip(&link);
        link.input.send(DictationInput::Stop).unwrap();
        let (reason, text, _) = finish(&mut link).await;
        assert_eq!((reason, text.as_str()), (DictationEnd::Stopped, "said 7"));
    }

    #[tokio::test]
    async fn cancel_throws_the_words_away_at_once() {
        let (mut link, _) = open(scripted(vec![vec![Detected::Started, ended(2, 0)]]));
        blip(&link);
        until_heard(&mut link, "said 2").await;
        link.input.send(DictationInput::Cancel).unwrap();
        let (reason, text, error) = finish(&mut link).await;
        assert_eq!((reason, text.as_str(), error), (DictationEnd::Cancelled, "", None));
    }

    #[tokio::test]
    async fn cancel_while_the_last_words_are_awaited_does_not_wait_for_them() {
        let (mut link, _gate) = open(scripted(vec![vec![Detected::Started, ended(1, 0)]]));
        blip(&link);
        until_state(&mut link, DictationState::Listening).await;
        link.input.send(DictationInput::Stop).unwrap();
        until_state(&mut link, DictationState::Finishing).await;
        link.input.send(DictationInput::Cancel).unwrap();
        let (reason, text, _) = finish(&mut link).await;
        assert_eq!((reason, text.as_str()), (DictationEnd::Cancelled, ""));
    }

    #[tokio::test]
    async fn the_transport_going_away_is_a_cancel() {
        let (mut link, _) = open(scripted(vec![vec![Detected::Started, ended(2, 0)]]));
        blip(&link);
        until_heard(&mut link, "said 2").await;
        let DictationLink { input, events } = link;
        drop(input);
        let mut link = DictationLink { input: tokio::sync::mpsc::unbounded_channel().0, events };
        let (reason, text, _) = finish(&mut link).await;
        assert_eq!((reason, text.as_str()), (DictationEnd::Cancelled, ""));
    }

    #[tokio::test]
    async fn a_failed_utterance_is_trouble_and_the_session_carries_on() {
        let steps =
            vec![vec![Detected::Started, ended(98, 0)], vec![Detected::Started, ended(2, 1_000)]];
        let (mut link, _) = open(scripted(steps));
        blip(&link);
        blip(&link);
        let message = loop {
            match next(&mut link).await.expect("an event") {
                DictationEvent::Trouble { message } => break message,
                DictationEvent::Ended { reason, .. } => panic!("ended: {reason:?}"),
                _ => {}
            }
        };
        assert_eq!(message, "no good");
        until_heard(&mut link, "said 2").await;
        link.input.send(DictationInput::Stop).unwrap();
        let (reason, text, _) = finish(&mut link).await;
        assert_eq!((reason, text.as_str()), (DictationEnd::Stopped, "said 2"));
    }

    #[tokio::test]
    async fn a_dead_microphone_ends_it_with_what_was_heard_and_why() {
        let (mut link, _) = open(scripted(vec![vec![Detected::Started, ended(2, 0)]]));
        blip(&link);
        until_heard(&mut link, "said 2").await;
        link.input.send(DictationInput::Failed("The microphone was unplugged.".into())).unwrap();
        let (reason, text, error) = finish(&mut link).await;
        assert_eq!(reason, DictationEnd::Failed);
        assert_eq!(text, "said 2");
        assert_eq!(error.as_deref(), Some("The microphone was unplugged."));
    }
}
