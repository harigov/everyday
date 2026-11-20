//! One conversation being talked.
//!
//! A single Tokio task owns all the state, and every loop of it is "wake,
//! handle, tell the interface what changed". Anything that can take time runs
//! elsewhere: listening and transcription are a
//! [`LiveTranscription`](crate::speech::live), whose events the loop reads,
//! synthesis is a [`Narrator`], and the assistant's turn is a task of its own,
//! reporting back on an internal channel. This module is a user of
//! [`crate::speech`], not part of it: what is particular to a conversation --
//! when to pause, what counts as an echo, what a spoken yes means -- lives here.
//! Nothing the loop awaits can keep it from hearing [`VoiceInput::Stop`] --
//! except two short things on purpose, rewriting a stored reply
//! ([`Session::cut_reply`]) and, while stopping, waiting for the cancelled turn
//! to put its last words down. Both are bounded by a timeout.
//!
//! The specification is `docs/plans/voice.md`, "The session"; the comments
//! here say why, not what, and number the items they implement.
//!
//! # What the person heard
//!
//! Interrupting the assistant has to leave its stored reply saying what was
//! actually heard, or the model goes on believing it said things the person
//! never got to. So every sentence is tracked from the moment it is cut out of
//! the stream to the moment the transport reports it played (the [`Narrator`]
//! hands back what played, tagged with its [`Kind`]), and only the playing of
//! one counts. Anything the session says itself -- a tool phrase, a
//! confirmation question, an apology -- is spoken but is not the reply and is
//! left out of what is kept, though it does go into the rolling record of
//! everything said that the echo check reads.

use super::confirm::{self, SpokenAnswer};
use super::phrases as text;
use super::{
    TurnOutcome, VoiceAssistant, VoiceEvent, VoiceInput, VoiceLink, VoiceOutput, VoiceParts,
    VoiceState,
};
use crate::agent::{AgentEvent, ConfirmAnswer, Sink};
use crate::error::{CommandError, CommandResult, codes};
use crate::listen::ReadAloud;
use crate::speech::detect::DetectorMode;
use crate::speech::listen::{Piece, PieceStream};
use crate::speech::live::{LiveControl, LiveEvent, LiveTranscription};
use crate::speech::narrate::{Narrated, Narrator, Sentence, Synthesised};
use crate::speech::text::{SentenceSplitter, sentences};
use futures::StreamExt;
use karmayoga_core::agent::onscreen::OnScreen;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::Instant;

/// How many sentences may be synthesised or queued with the transport at once.
/// One is playing and one is ready behind it, so there is never a gap, and a
/// long reply is not all synthesised before the person has had the chance to
/// say stop.
const MAX_AHEAD: usize = 2;

/// How far back what was said counts as something that might be coming back
/// through the microphone.
const ECHO_WINDOW: Duration = Duration::from_secs(30);

/// How long rewriting a stored reply may take before the session carries on
/// without it.
const CUT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long stopping waits for a cancelled turn to finish writing down what it
/// had said, so the reply is not rewritten underneath it.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// How long after a spoken question has played before what is heard can be
/// its answer. "Played" means handed to the device, so the end of the question
/// may still be in the air, and a question can end in a word chosen by whatever
/// the model has read.
const ANSWER_GRACE: Duration = Duration::from_millis(400);

/// How long after the last thing played what is heard may still be it. Played
/// means handed to the device, so the end of a sentence can still be in the
/// room, and on its way back through the microphone, after that.
const ECHO_TAIL: Duration = Duration::from_millis(1500);

/// Said when a turn fails.
const SORRY: &str = "Sorry, that didn't work.";

/// How many sentences of something being read aloud are known beyond what
/// has played before no more of its text is asked for. Its text is a model's
/// writing as it is read, so somebody who stops after a minute has not paid
/// for the rest of the article -- the reason the Listen player has its own.
const READ_AHEAD: usize = 4;

/// Said when what was being read aloud could not be read to the end.
const COULD_NOT_FINISH: &str = "Sorry, I couldn't read the rest of it.";

/// Said when what was to be read aloud could not be read at all, and why is
/// not a sentence meant for a person.
const COULD_NOT_READ: &str = "Sorry, I couldn't read that.";

/// How long after a goodbye has played before the session ends on it.
/// "Played" means handed to the device, or on a call sent, so the last word
/// can still be in the speaker's buffer or the phone's, and ending at once --
/// closing the device, hanging up -- cut it off.
const GOODBYE_TAIL: Duration = Duration::from_millis(1500);

/// Start a session on the current Tokio runtime.
///
/// Everything it needs is already built: loading models is
/// [`Service::voice_open`](crate::service::Service)'s job, so a session starts
/// listening at once.
pub fn spawn(parts: VoiceParts) -> VoiceLink {
    let (input, input_rx) = unbounded_channel();
    let (out, output) = unbounded_channel();
    let (tx, internal) = unbounded_channel();
    let LiveTranscription { control, events } = parts.live;
    // Each finished synthesis comes back through the loop, so the narrator is
    // only ever touched from it.
    let finished = tx.clone();
    let narrator = Narrator::new(
        parts.synthesiser,
        MAX_AHEAD,
        Arc::new(move |done| {
            let _ = finished.send(Internal::Synthesised(done));
        }),
    );
    let session = Session {
        assistant: parts.assistant,
        needs_a_click: parts.needs_a_click,
        narrator,
        out,
        tx,
        internal,
        control,
        state: VoiceState::Starting,
        muted: false,
        stopping: false,
        screen: None,
        mode: DetectorMode::Open,
        speaking: false,
        talking: None,
        utterances: HashMap::new(),
        understanding: 0,
        paused: false,
        said: VecDeque::new(),
        last_played: None,
        next_turn: 1,
        turn: None,
        pending_prompt: None,
        confirm: None,
        ending: false,
        goodbye_at: None,
        working: false,
        reading: None,
        to_read: VecDeque::new(),
        next_reading: 1,
    };
    tokio::spawn(run(session, input_rx, events));
    VoiceLink { input, output }
}

/// What the tasks the session starts report back.
enum Internal {
    /// One sentence, synthesised, to be handed back to the narrator.
    Synthesised(Synthesised),
    /// Something from a turn, through its [`Sink`].
    Agent { turn: u64, event: AgentEvent },
    /// A turn is over. Always the last thing it sends.
    TurnDone { turn: u64, result: CommandResult<TurnOutcome> },
    /// The text of what a turn asked to have read aloud, ready to be read --
    /// or why it cannot be.
    ReadingOpened { reading: u64, result: CommandResult<PieceStream> },
    /// The next piece of that text, as asked for; `None` when there is no
    /// more.
    ReadingPiece { reading: u64, piece: Option<CommandResult<Piece>> },
}

/// What was true when somebody began talking, which is when it has to be
/// decided: by the time the words are known, everything has moved on.
#[derive(Debug, Clone, Copy, Default)]
struct Utterance {
    /// Which spoken question had finished playing before they started, so that
    /// what they say may be its answer, and only its. One that began earlier
    /// might be the question coming back through the speakers, or somebody
    /// talking over it, and a later question was never put to them.
    answers: Option<u64>,
    /// Something was audible when they started, so it may be an echo.
    over_audio: bool,
}

/// What somebody said that begins a turn.
#[derive(Debug, Clone)]
struct Said {
    text: String,
    /// Nothing of the session's own was audible when they began: these are
    /// their words, not an echo the check missed, and may approve what they
    /// ask for (see [`VoiceAssistant::turn`]).
    clear: bool,
}

/// What a sentence is, which decides whether it is part of the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The model's own words.
    Reply,
    /// The session's: a tool phrase, an apology, a request for a click.
    Own,
    /// A confirmation question, asked aloud.
    Question,
    /// Something being read aloud in full (`read_aloud`): said after the
    /// reply, and not part of it.
    Reading,
}

/// What a turn has done, as far as speech is concerned.
struct TurnState {
    id: u64,
    /// The assistant's stored reply, from [`AgentEvent::Started`].
    message_id: Option<String>,
    splitter: SentenceSplitter,
    /// The splitter has been told the reply is over, so its tail is a
    /// sentence like any other.
    flushed: bool,
    /// The turn's own last word has come back: nothing more will happen.
    done: bool,
    /// Somebody talked over it or stopped it, and its events no longer speak.
    cancelling: bool,
    /// The cancel reached a running turn. A turn only just begun has not
    /// registered yet, and is asked again when it has.
    cancel_landed: bool,
    /// It failed, and the person has been told.
    failed: bool,
    /// A phrase has been said for a tool. Once per turn.
    phrase_said: bool,
    /// Sentences of the reply handed to the transport, and how many of them
    /// have played.
    reply_total: usize,
    reply_played: usize,
    /// What of the reply has played, in order.
    heard: Vec<String>,
    /// The stored reply has been put right, or there was nothing to put
    /// right.
    cut: bool,
    /// Every sentence of the reply has played, and the interface was told.
    heard_whole: bool,
}

impl TurnState {
    fn new(id: u64) -> Self {
        Self {
            id,
            message_id: None,
            splitter: SentenceSplitter::new(),
            flushed: false,
            done: false,
            cancelling: false,
            cancel_landed: false,
            failed: false,
            phrase_said: false,
            reply_total: 0,
            reply_played: 0,
            heard: Vec::new(),
            cut: false,
            heard_whole: false,
        }
    }

    /// Whether some of the reply was not heard. A tail the splitter was still
    /// holding when the turn was stopped is part of it, so it is asked for
    /// here, once.
    fn unheard(&mut self) -> bool {
        if !self.flushed {
            self.flushed = true;
            if let Some(tail) = self.splitter.finish()
                && !tail.trim().is_empty()
            {
                self.reply_total += 1;
            }
        }
        self.reply_total > self.reply_played
    }
}

/// Something a turn asked to have read aloud, being read.
///
/// Its text comes from a task of its own, one piece each time the session
/// asks ([`Session::read_on`]), and the session asks only while fewer than
/// [`READ_AHEAD`] of its sentences are waiting to be played. Dropped, the
/// task goes with it.
struct Reading {
    id: u64,
    /// Lets the task reading the text take one more piece. `None` until the
    /// text is open.
    more: Option<Arc<Semaphore>>,
    /// A piece has been asked for and has not come.
    asked: bool,
    splitter: SentenceSplitter,
    /// The part of the text the splitter is in: a sentence ends where a part
    /// does.
    part: u32,
    /// Its sentences handed to the narrator and not yet played.
    queued: usize,
    /// No more of the text is coming.
    ended: bool,
    /// Some of it has been handed over to be said.
    read_any: bool,
    task: Option<tokio::task::AbortHandle>,
}

impl Reading {
    fn new(id: u64) -> Self {
        Self {
            id,
            more: None,
            asked: false,
            splitter: SentenceSplitter::new(),
            part: 0,
            queued: 0,
            ended: false,
            read_any: false,
            task: None,
        }
    }

    /// All of it has been read.
    fn finished(&self) -> bool {
        self.ended && self.queued == 0
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// A confirmation the turn is waiting on.
struct Confirm {
    call_id: String,
    kind: String,
    subject: String,
    /// May be answered by voice, and has not yet fallen back to the click.
    voice: bool,
    /// The question has been played, so the next thing said may be the
    /// answer.
    asked: bool,
    /// The question being asked now, so its playing is recognised.
    question: Option<u64>,
    /// When it finished playing.
    played_at: Option<Instant>,
    /// How many times what was said was no answer.
    unclear: u8,
}

/// Why the loop woke.
enum Wake {
    Input(Option<VoiceInput>),
    Internal(Option<Internal>),
    Live(Option<LiveEvent>),
    OutputGone,
    /// [`Session::goodbye_at`] came.
    Goodbye,
}

struct Session {
    assistant: Arc<dyn VoiceAssistant>,
    /// What is said when only a click can answer: see [`VoiceParts`].
    needs_a_click: &'static str,
    /// Turns sentences into audio in order, a limited way ahead of playback,
    /// and remembers which of them the transport has yet to play. A sentence's
    /// [`Kind`] is the tag it carries.
    narrator: Narrator<Kind>,
    out: UnboundedSender<VoiceOutput>,
    /// What the tasks it starts report on.
    tx: UnboundedSender<Internal>,
    internal: UnboundedReceiver<Internal>,
    /// Feeds the detector and sets how much it takes. The detector's thread
    /// ends when this and its clones are dropped.
    control: LiveControl,

    state: VoiceState,
    muted: bool,
    /// The end has begun, and nothing new may start.
    stopping: bool,
    screen: Option<OnScreen>,
    mode: DetectorMode,

    /// Somebody is talking: between `Started` and `Ended`.
    speaking: bool,
    /// Who, while they are.
    talking: Option<u64>,
    /// What was true when each utterance began, until its words arrive. An
    /// utterance that is not here when its words do was dropped on purpose
    /// (it was said while muted) and is not listened to.
    utterances: HashMap<u64, Utterance>,
    /// Utterances being transcribed.
    understanding: usize,
    /// Playback is held because they may be talking over it.
    paused: bool,

    /// Everything said lately, for the echo check.
    said: VecDeque<(Instant, String)>,
    /// When something last finished playing.
    last_played: Option<Instant>,

    next_turn: u64,
    turn: Option<TurnState>,
    /// What was said over the reply while its turn was ending: the one
    /// prompt held for when it has. Later words replace it.
    pending_prompt: Option<Said>,
    confirm: Option<Confirm>,
    /// The last turn ended the conversation ([`super::END_TOOL`]): the session
    /// ends once its goodbye has been heard, unless the person says something
    /// first, which begins a turn and takes it back.
    ending: bool,
    /// When the goodbye will have been heard and the session ends: set once
    /// it has played, and taken back if anything happens in the meantime.
    goodbye_at: Option<Instant>,
    /// The transport has been asked to play the sound of working.
    working: bool,
    /// What a turn asked to have read aloud, while it is being read.
    reading: Option<Reading>,
    /// What else the turn asked to have read, after it, in order.
    to_read: VecDeque<ReadAloud>,
    next_reading: u64,
}

async fn run(
    mut s: Session,
    mut input: UnboundedReceiver<VoiceInput>,
    mut live: UnboundedReceiver<LiveEvent>,
) {
    // Kept apart from `s` so that the branch waiting on it borrows nothing
    // the handlers need.
    let gone = s.out.clone();
    s.sync();
    let error = loop {
        let goodbye_at = s.goodbye_at;
        let wake = tokio::select! {
            // Stop is read before anything else, and what was heard last: it
            // is of no use until what is already known is settled.
            biased;
            message = input.recv() => Wake::Input(message),
            message = s.internal.recv() => Wake::Internal(message),
            event = live.recv() => Wake::Live(event),
            () = gone.closed() => Wake::OutputGone,
            () = wait_until(goodbye_at) => Wake::Goodbye,
        };
        match wake {
            // 9. A closed input is a Stop, and so is a transport that has
            // stopped listening to us.
            Wake::Input(None | Some(VoiceInput::Stop))
            | Wake::OutputGone
            | Wake::Internal(None) => {
                break None;
            }
            Wake::Input(Some(message)) => s.on_input(message),
            Wake::Internal(Some(message)) => s.on_internal(message).await,
            Wake::Live(Some(event)) => s.on_live(event).await,
            Wake::Live(None) => {
                break Some("Listening for speech stopped working.".to_string());
            }
            // 10. The assistant said goodbye, and it has been heard.
            Wake::Goodbye => break None,
        }
        s.sync();
        // 10. Once the goodbye has played, the end -- after a moment for its
        // last word to leave the speaker, and not if anything happens first.
        if s.said_goodbye() {
            s.goodbye_at.get_or_insert_with(|| Instant::now() + GOODBYE_TAIL);
        } else {
            s.goodbye_at = None;
        }
    };
    s.stop(error).await;
}

/// Wait until `at`, or for ever when there is nothing to wait for.
async fn wait_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

impl Session {
    // ---- telling the transport ------------------------------------------

    fn send(&self, output: VoiceOutput) {
        // A transport that has gone is noticed by the loop, which is waiting
        // on `closed`.
        let _ = self.out.send(output);
    }

    fn emit(&self, event: VoiceEvent) {
        self.send(VoiceOutput::Event(event));
    }

    fn trouble(&self, message: impl Into<String>) {
        self.emit(VoiceEvent::Trouble { message: message.into() });
    }

    /// Tell the detector how much it takes, and the interface what the
    /// session is doing -- each only when it has changed.
    fn sync(&mut self) {
        let state = self.derive_state();
        // 12. The sound of working: while the assistant is working out what
        // to say and has nothing to play yet -- its words still being made
        // into a voice count -- and never while the person talks, while
        // anything is playing or held, or while it waits on them. Told before
        // the state, so a transport never hears it over the person.
        let working = !self.stopping
            && matches!(
                state,
                VoiceState::Understanding | VoiceState::Thinking | VoiceState::Speaking
            )
            && !self.narrator.playing()
            && !self.paused;
        self.set_working(working);
        // Not guarded for the sound of working: most of it is silence, a
        // speaker may not be playing it at all, and a short "stop" said while
        // a slow tool runs must be heard. A soft tone is not speech to the
        // detector, and echo cancellation hears it as it hears the voice.
        let mode =
            if self.audio_outstanding() { DetectorMode::Guarded } else { DetectorMode::Open };
        if mode != self.mode {
            self.mode = mode;
            self.control.set_mode(mode);
        }
        if state != self.state {
            self.state = state;
            self.emit(VoiceEvent::State { state });
        }
    }

    fn set_working(&mut self, working: bool) {
        if working != self.working {
            self.working = working;
            self.send(VoiceOutput::Working(working));
        }
    }

    fn derive_state(&self) -> VoiceState {
        // Something being read aloud that has nothing ready to play is being
        // worked on, as a turn is.
        let waiting_on_turn = self.turn.as_ref().is_some_and(|t| !t.done)
            || self.reading.is_some()
            || !self.to_read.is_empty();
        if self.speaking {
            VoiceState::Hearing
        } else if self.understanding > 0 {
            VoiceState::Understanding
        } else if self.confirm.as_ref().is_some_and(|c| !c.voice) {
            VoiceState::WaitingForClick
        } else if self.asking() {
            VoiceState::Asking
        } else if self.audio_outstanding() {
            VoiceState::Speaking
        } else if waiting_on_turn {
            VoiceState::Thinking
        } else {
            VoiceState::Listening
        }
    }

    // ---- what the transport tells us -------------------------------------

    fn on_input(&mut self, input: VoiceInput) {
        match input {
            VoiceInput::Audio(samples) => {
                if !self.muted {
                    self.control.feed(samples);
                }
            }
            VoiceInput::Played { utterance } => self.on_played(utterance),
            VoiceInput::Muted(muted) => {
                self.muted = muted;
                if muted {
                    // Whatever was half heard is forgotten, and what is being
                    // said goes on: held playback is let go.
                    self.control.reset();
                    self.speaking = false;
                    if let Some(id) = self.talking.take() {
                        self.utterances.remove(&id);
                    }
                    self.resume_if_paused();
                }
            }
            VoiceInput::Screen(screen) => self.screen = screen,
            VoiceInput::Say(text) => {
                if !self.stopping {
                    for sentence in sentences(&text) {
                        self.say(sentence);
                    }
                }
            }
            // Handled by the loop, which must not leave it to us.
            VoiceInput::Stop => {}
        }
    }

    /// (1) Listening, and what was heard.
    async fn on_live(&mut self, event: LiveEvent) {
        match event {
            LiveEvent::Started { utterance } => {
                if self.muted {
                    return;
                }
                self.speaking = true;
                self.talking = Some(utterance);
                self.utterances.insert(
                    utterance,
                    Utterance {
                        answers: self.answerable_question(),
                        over_audio: self.audio_outstanding() || self.tail_in_the_air(),
                    },
                );
                // 4. They may be talking over it: hold it where it is until
                // the words say whether they are.
                if self.audio_outstanding() && !self.paused {
                    self.paused = true;
                    self.send(VoiceOutput::Pause);
                }
            }
            LiveEvent::Ended { utterance, .. } => {
                if self.muted {
                    self.utterances.remove(&utterance);
                    return;
                }
                self.speaking = false;
                self.talking = None;
                self.understanding += 1;
                // Started while muted, and unmuted since: nothing is known
                // of it.
                self.utterances.entry(utterance).or_default();
            }
            LiveEvent::Transcribed { utterance, result, .. } => {
                // Dropped when it ended, so it is neither counted nor heard.
                let Some(facts) = self.utterances.remove(&utterance) else { return };
                self.understanding = self.understanding.saturating_sub(1);
                if self.stopping {
                    return;
                }
                match result {
                    // 8. Carry on.
                    Err(e) => {
                        self.trouble(e.message);
                        self.resume_if_paused();
                    }
                    Ok(text) => self.on_words(text.trim().to_string(), facts).await,
                }
            }
        }
    }

    // ---- what the tasks report -------------------------------------------

    async fn on_internal(&mut self, message: Internal) {
        match message {
            Internal::Synthesised(done) => self.on_synthesised(done),
            Internal::Agent { turn, event } => self.on_agent(turn, event),
            Internal::TurnDone { turn, result } => self.on_turn_done(turn, result).await,
            Internal::ReadingOpened { reading, result } => self.on_reading_opened(reading, result),
            Internal::ReadingPiece { reading, piece } => self.on_reading_piece(reading, piece),
        }
    }

    // ---- words -------------------------------------------------------------

    /// What to do with what somebody said.
    async fn on_words(&mut self, text: String, utterance: Utterance) {
        if text.is_empty() {
            self.resume_if_paused();
            return;
        }
        // 7. An answer to the question just asked: only if they began after it
        // had finished, so that the question cannot approve itself through
        // the speakers, and only to the question it was said to.
        let current = self.confirm.as_ref().filter(|c| c.voice).and_then(|c| c.question);
        let answers_it = utterance.answers.is_some() && utterance.answers == current;
        if answers_it && self.asking() {
            match confirm::spoken_answer(&text) {
                SpokenAnswer::Yes => return self.answer_confirmation(ConfirmAnswer::Confirm),
                SpokenAnswer::No => return self.answer_confirmation(ConfirmAnswer::Decline),
                SpokenAnswer::Unclear => {
                    if self.is_echo(&text) {
                        self.resume_if_paused();
                        return;
                    }
                    // A stop word is not an unclear answer; it falls through
                    // to being the stop it is.
                    if !text::is_stop_word(&text) {
                        self.resume_if_paused();
                        self.ask_again_or_click();
                        return;
                    }
                }
            }
        }
        // Said over the question, too soon after it, or to another one. It
        // cannot approve anything, but "no" is safe to take at its word, and a
        // "yes" is no reason to throw the question away: it goes on, and is
        // answered when it has finished.
        if !answers_it && current.is_some() {
            match confirm::spoken_answer(&text) {
                SpokenAnswer::No => return self.answer_confirmation(ConfirmAnswer::Decline),
                SpokenAnswer::Yes => return self.resume_if_paused(),
                SpokenAnswer::Unclear => {}
            }
        }
        // 4. Their own voice or ours coming back: carry on as if nothing
        // happened.
        if (utterance.over_audio || self.audio_outstanding()) && self.is_echo(&text) {
            self.resume_if_paused();
            return;
        }
        if text::is_stop_word(&text) {
            self.interrupt(None).await;
        } else {
            self.interrupt(Some(Said { text, clear: !utterance.over_audio })).await;
        }
    }

    fn is_echo(&mut self, heard: &str) -> bool {
        let spoken = self.recently_said();
        text::sounds_like_echo(heard, &spoken)
    }

    /// Everything said in the last [`ECHO_WINDOW`], joined.
    fn recently_said(&mut self) -> String {
        let now = Instant::now();
        while self.said.front().is_some_and(|(at, _)| now.duration_since(*at) > ECHO_WINDOW) {
            self.said.pop_front();
        }
        self.said.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>().join(" ")
    }

    /// (4) Talking over it, or stopping it: throw away what is queued, stop the
    /// turn, and -- once the turn has ended, so that nothing it writes lands
    /// after -- put the stored reply right and begin on `prompt`, if there
    /// is one.
    ///
    /// Words that arrive while that is still going on replace the one held
    /// prompt; a stop word drops it.
    async fn interrupt(&mut self, prompt: Option<Said>) {
        let running = self.turn.as_ref().is_some_and(|t| !t.done);
        let active = running || self.audio_outstanding();
        self.pending_prompt = prompt;
        // Whatever was being read aloud is over: they said stop, or moved on.
        self.reading = None;
        self.to_read.clear();
        if self.audio_outstanding() || self.paused {
            self.clear_audio();
        }
        self.confirm = None;
        if running {
            if let Some(t) = self.turn.as_mut()
                && !t.cancelling
            {
                t.cancelling = true;
                t.cancel_landed = self.assistant.cancel();
            }
            // Carried on by `on_turn_done`.
            return;
        }
        if active {
            self.cut_reply().await;
        }
        if let Some(prompt) = self.pending_prompt.take() {
            self.start_turn(prompt);
        }
    }

    // ---- turns -------------------------------------------------------------

    /// (2) Run a turn on `prompt`. The words are announced only now, as the
    /// turn really begins, so the thread never shows a yes or no, or a stop
    /// word, as a message.
    fn start_turn(&mut self, said: Said) {
        if self.stopping {
            return;
        }
        self.emit(VoiceEvent::Heard { text: said.text.clone() });
        let id = self.next_turn;
        self.next_turn += 1;
        self.turn = Some(TurnState::new(id));
        self.confirm = None;
        self.reading = None;
        self.to_read.clear();
        // 10. They had more to say after all.
        self.ending = false;

        let events = self.tx.clone();
        let sink: Sink = Arc::new(move |event| {
            let _ = events.send(Internal::Agent { turn: id, event });
        });
        let turn = self.assistant.turn(said.text, said.clear, self.screen.clone(), sink);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            // A turn that panics must still be a turn that ended, or the
            // session would wait on it for ever.
            let result = match tokio::spawn(turn).await {
                Ok(result) => result,
                Err(e) => Err(CommandError::new(codes::PANIC, format!("the turn failed: {e}"))),
            };
            let _ = tx.send(Internal::TurnDone { turn: id, result });
        });
    }

    async fn on_turn_done(&mut self, turn: u64, result: CommandResult<TurnOutcome>) {
        let Some(t) = self.turn.as_mut().filter(|t| t.id == turn) else { return };
        t.done = true;
        let cancelling = t.cancelling;
        let failed_already = t.failed;
        self.confirm = None;
        // 10. A turn talked over is not a goodbye that was heard.
        if !cancelling && matches!(&result, Ok(outcome) if outcome.ended_voice) {
            self.ending = true;
        }
        // 11. What it was asked to read aloud is read after its reply -- not
        // for a turn somebody talked over, who has moved on.
        if !cancelling && let Ok(outcome) = &result {
            self.to_read.extend(outcome.read_aloud.iter().cloned());
            self.read_next();
        }
        // A turn that returns an error is a turn that failed, whether or not
        // it said so first.
        if let Err(e) = result
            && !cancelling
            && !failed_already
        {
            // It may have failed before it said anything, and the reply the
            // thread opened for it is waiting to hear how it ended.
            self.emit(VoiceEvent::Agent {
                event: AgentEvent::Failed { message: e.message.clone() },
            });
            self.turn_failed(e.message);
        }
        if cancelling {
            self.cut_reply().await;
            if let Some(prompt) = self.pending_prompt.take() {
                self.start_turn(prompt);
            }
        }
    }

    /// (3) Tell the interface once every sentence of the reply has played:
    /// it was heard whole, whatever is read aloud after it.
    fn heard_whole(&mut self) {
        let Some(t) = self.turn.as_mut() else { return };
        if t.heard_whole || t.cancelling || !t.flushed || t.reply_played < t.reply_total {
            return;
        }
        let Some(message_id) = t.message_id.clone() else { return };
        t.heard_whole = true;
        self.emit(VoiceEvent::ReplyHeard { message_id });
    }

    /// (8) Say so, and carry on.
    fn turn_failed(&mut self, message: String) {
        if let Some(t) = self.turn.as_mut() {
            t.failed = true;
        }
        self.trouble(message);
        self.say(SORRY.to_string());
    }

    /// (5) If some of the reply was not heard, make the stored one read what
    /// was. The model's memory of the conversation then matches the person's.
    /// Not before the turn has ended, which is why it is only ever called
    /// once the turn is done.
    async fn cut_reply(&mut self) {
        let Some(t) = self.turn.as_mut() else { return };
        if t.cut {
            return;
        }
        t.cut = true;
        let Some(message_id) = t.message_id.clone() else { return };
        if !t.unheard() {
            return;
        }
        let heard = t.heard.join(" ");
        let kept = self.assistant.keep_heard(message_id.clone(), heard.clone());
        match tokio::time::timeout(CUT_TIMEOUT, kept).await {
            Ok(Ok(())) => self.emit(VoiceEvent::ReplyCut { message_id, text: heard }),
            Ok(Err(e)) => self.trouble(e.message),
            Err(_) => self.trouble("The reply could not be updated to what was heard."),
        }
    }

    /// (10) The assistant ended the conversation and everything it had to say
    /// has been heard: its turn is over, nothing is playing or waiting to, and
    /// nobody is talking over the goodbye or waiting to be understood -- their
    /// words decide, and any that are not an echo or a stop begin a turn.
    fn said_goodbye(&self) -> bool {
        self.ending
            && self.turn.as_ref().is_none_or(|t| t.done)
            && self.reading.is_none()
            && self.to_read.is_empty()
            && !self.audio_outstanding()
            && !self.speaking
            && self.understanding == 0
            && self.pending_prompt.is_none()
    }

    /// (9) Cancel any turn, clear what is queued, cut the reply, and say so.
    async fn stop(&mut self, error: Option<String>) {
        self.stopping = true;
        self.pending_prompt = None;
        self.confirm = None;
        self.reading = None;
        self.to_read.clear();
        self.set_working(false);
        if let Some(t) = self.turn.as_mut()
            && !t.done
        {
            t.cancelling = true;
            t.cancel_landed = self.assistant.cancel();
        }
        self.clear_audio();
        // The turn writes its reply down as it ends, so the reply is not
        // rewritten until it has. The wait is bounded: a turn that will not
        // end is not worth holding the end of the session for.
        let deadline = Instant::now() + STOP_TIMEOUT;
        while self.turn.as_ref().is_some_and(|t| !t.done) {
            match tokio::time::timeout_at(deadline, self.internal.recv()).await {
                Ok(Some(message @ (Internal::Agent { .. } | Internal::TurnDone { .. }))) => {
                    self.on_internal(message).await;
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        self.cut_reply().await;
        self.emit(VoiceEvent::Ended { error });
    }

    // ---- the turn's events ---------------------------------------------------

    /// 2, 3, 6, 7 and 8: what a turn says, as it says it.
    fn on_agent(&mut self, turn: u64, event: AgentEvent) {
        // The screen sees everything, whether or not it is spoken.
        self.emit(VoiceEvent::Agent { event: event.clone() });
        let Some(t) = self.turn.as_mut().filter(|t| t.id == turn) else { return };
        if let AgentEvent::Started { message_id } = &event {
            t.message_id.get_or_insert_with(|| message_id.clone());
            // A turn stopped before it had registered cannot be cancelled;
            // asked again, now that it has.
            if t.cancelling && !t.cancel_landed {
                t.cancel_landed = self.assistant.cancel();
            }
        }
        if t.cancelling {
            return;
        }
        match event {
            AgentEvent::Delta { text } => {
                for sentence in t.splitter.push(&text) {
                    self.enqueue_reply(sentence);
                }
            }
            // Nothing is said twice: the splitter hands out only what it has
            // not already.
            AgentEvent::Retrying { text } => {
                for sentence in t.splitter.replace(&text) {
                    self.enqueue_reply(sentence);
                }
            }
            AgentEvent::Finished { .. } => {
                let tail = t.splitter.finish();
                t.flushed = true;
                self.confirm = None;
                if let Some(tail) = tail {
                    self.enqueue_reply(tail);
                }
                self.heard_whole();
            }
            AgentEvent::Failed { message } => {
                self.confirm = None;
                self.turn_failed(message);
            }
            // 6. Something is about to take a while, and nothing is being
            // said: say that it is being worked on, once.
            AgentEvent::ToolPreparing { name, .. } => self.tool_phrase(&name),
            AgentEvent::ToolStarted { call_id, name, .. } => {
                self.resolve_confirmation(&call_id);
                self.tool_phrase(&name);
            }
            AgentEvent::ToolFinished { call_id, .. } => self.resolve_confirmation(&call_id),
            AgentEvent::ConfirmationRequired { call_id, name, subject, kind, .. } => {
                self.begin_confirmation(call_id, &name, kind, subject);
            }
            AgentEvent::Started { .. } | AgentEvent::Thinking { .. } => {}
        }
    }

    fn tool_phrase(&mut self, tool: &str) {
        // Ending the conversation takes no time, and "One moment" before a
        // goodbye says the opposite of what is happening.
        if tool == super::END_TOOL {
            return;
        }
        let said = self.turn.as_ref().is_none_or(|t| t.phrase_said);
        if said || self.audio_outstanding() {
            return;
        }
        if let Some(t) = self.turn.as_mut() {
            t.phrase_said = true;
        }
        self.say(text::tool_phrase(tool).to_string());
    }

    // ---- confirmations -------------------------------------------------------

    /// (7) A call is waiting on a person.
    fn begin_confirmation(&mut self, call_id: String, tool: &str, kind: String, subject: String) {
        let voice = confirm::may_answer_aloud(&kind, tool, &subject);
        let question = voice.then(|| confirm::question(&kind, &subject));
        self.confirm = Some(Confirm {
            call_id,
            kind,
            subject,
            voice,
            asked: false,
            question: None,
            played_at: None,
            unclear: 0,
        });
        match question {
            Some(question) => {
                let id = self.enqueue(question, Kind::Question);
                if let Some(c) = self.confirm.as_mut() {
                    c.question = Some(id);
                }
            }
            // Only a click can answer it, and the card is on screen.
            None => self.say(self.needs_a_click.to_string()),
        }
    }

    /// Whether a spoken question is out and its answer is awaited.
    fn asking(&self) -> bool {
        self.confirm.as_ref().is_some_and(|c| c.voice && c.asked)
    }

    /// The question somebody starting to talk now could be answering: one that
    /// has been out for long enough that it is not what is heard.
    fn answerable_question(&self) -> Option<u64> {
        let c = self.confirm.as_ref().filter(|c| c.voice && c.asked)?;
        c.played_at.filter(|at| at.elapsed() >= ANSWER_GRACE)?;
        c.question
    }

    /// Whether the end of what was last played may still be coming back.
    fn tail_in_the_air(&self) -> bool {
        self.last_played.is_some_and(|at| at.elapsed() < ECHO_TAIL)
    }

    fn answer_confirmation(&mut self, answer: ConfirmAnswer) {
        if let Some(c) = self.confirm.take() {
            self.assistant.answer(&c.call_id, answer);
        }
        self.resume_if_paused();
    }

    /// Something that was no answer: ask once more, and then leave it to the
    /// click.
    fn ask_again_or_click(&mut self) {
        let Some(c) = self.confirm.as_mut() else { return };
        c.unclear += 1;
        c.asked = false;
        c.played_at = None;
        if c.unclear == 1 {
            let question = confirm::question(&c.kind, &c.subject);
            let id = self.enqueue(question, Kind::Question);
            if let Some(c) = self.confirm.as_mut() {
                c.question = Some(id);
            }
        } else {
            self.fall_back_to_click();
        }
    }

    fn fall_back_to_click(&mut self) {
        if let Some(c) = self.confirm.as_mut() {
            c.voice = false;
        }
        self.say(self.needs_a_click.to_string());
    }

    /// The card was answered where it is drawn, or the call ran or failed
    /// without it: nothing is being waited on.
    fn resolve_confirmation(&mut self, call_id: &str) {
        if self.confirm.as_ref().is_some_and(|c| c.call_id == call_id) {
            self.confirm = None;
        }
    }

    // ---- speaking --------------------------------------------------------------

    fn audio_outstanding(&self) -> bool {
        self.narrator.outstanding()
    }

    /// Queue a sentence of the reply. The splitter hands out sentences already
    /// made speakable, so they are not put through [`text::speakable`] a
    /// second time. Nothing to say, nothing queued.
    fn enqueue_reply(&mut self, sentence: String) {
        let text = sentence.trim().to_string();
        if text.is_empty() {
            return;
        }
        if let Some(t) = self.turn.as_mut() {
            t.reply_total += 1;
        }
        self.enqueue(text, Kind::Reply);
    }

    /// Say something of the session's own: not part of the reply.
    fn say(&mut self, text: String) {
        self.enqueue(text, Kind::Own);
    }

    /// (3) The narrator starts on it as soon as it may, and holds it back while
    /// the transport already has enough.
    fn enqueue(&mut self, text: String, kind: Kind) -> u64 {
        self.narrator.say(text, kind)
    }

    fn on_synthesised(&mut self, done: Synthesised) {
        match self.narrator.finished(done) {
            Narrated::Ready { sentence, speech } => {
                self.said.push_back((Instant::now(), sentence.text.clone()));
                self.send(VoiceOutput::Play {
                    utterance: sentence.id,
                    samples: speech.samples,
                    sample_rate: speech.sample_rate,
                });
                // Somebody began talking after what was playing had ended:
                // this must not start over them.
                if self.speaking && !self.paused {
                    self.paused = true;
                    self.send(VoiceOutput::Pause);
                }
            }
            // 8. A sentence that cannot be said is skipped.
            Narrated::Failed { sentence, error } => {
                if let Some(e) = error {
                    self.trouble(e.message);
                }
                // Never going to be played, so its not being heard says nothing
                // about whether the person stopped listening: the reply on
                // screen stays whole.
                if sentence.tag == Kind::Reply
                    && let Some(t) = self.turn.as_mut()
                {
                    t.reply_total = t.reply_total.saturating_sub(1);
                    self.heard_whole();
                }
                if sentence.tag == Kind::Reading {
                    self.read_one();
                }
                self.skipped(&sentence);
            }
            // Cleared since: what was asked for is not wanted.
            Narrated::Stale => {}
        }
    }

    fn skipped(&mut self, sentence: &Sentence<Kind>) {
        // A question that cannot be asked is not going to be answered.
        if sentence.tag == Kind::Question
            && self.confirm.as_ref().is_some_and(|c| c.question == Some(sentence.id))
        {
            self.fall_back_to_click();
        }
    }

    /// The transport says `utterance` has played, and so has everything
    /// before it.
    fn on_played(&mut self, utterance: u64) {
        // Cleared since, or never sent: it names nothing, and nothing comes
        // back.
        for sentence in self.narrator.played(utterance) {
            self.last_played = Some(Instant::now());
            match sentence.tag {
                Kind::Reply => {
                    if let Some(t) = self.turn.as_mut() {
                        t.reply_played += 1;
                        t.heard.push(sentence.text);
                    }
                    self.heard_whole();
                }
                Kind::Question => {
                    if let Some(c) = self.confirm.as_mut()
                        && c.question == Some(sentence.id)
                    {
                        c.asked = true;
                        c.played_at = Some(Instant::now());
                    }
                }
                Kind::Reading => self.read_one(),
                Kind::Own => {}
            }
        }
    }

    // ---- reading aloud -----------------------------------------------------

    /// (11) Start on the next thing the turn asked to have read, if nothing
    /// is being read.
    fn read_next(&mut self) {
        if self.reading.is_none()
            && let Some(what) = self.to_read.pop_front()
        {
            self.start_reading(what);
        }
    }

    /// (11) Open what a turn asked to have read aloud. Its text may take a
    /// moment -- an article fetched, a model asked -- so it is opened on a
    /// task of its own, and the reply plays meanwhile.
    fn start_reading(&mut self, what: ReadAloud) {
        if self.stopping {
            return;
        }
        let id = self.next_reading;
        self.next_reading += 1;
        let opening = self.assistant.open_reading(what);
        let tx = self.tx.clone();
        let mut reading = Reading::new(id);
        reading.task = Some(
            tokio::spawn(async move {
                let result = opening.await;
                let _ = tx.send(Internal::ReadingOpened { reading: id, result });
            })
            .abort_handle(),
        );
        self.reading = Some(reading);
    }

    fn on_reading_opened(&mut self, id: u64, result: CommandResult<PieceStream>) {
        let Some(reading) = self.reading.as_mut().filter(|r| r.id == id) else { return };
        let mut pieces = match result {
            Ok(pieces) => pieces,
            Err(e) => {
                // Said, since they asked for it out loud: in its own words
                // when those are meant for a person ("There is nothing in
                // this email to read."), and plainly when they are not.
                self.reading = None;
                self.trouble(e.message.clone());
                let said = if e.code == codes::INVALID { e.message } else { COULD_NOT_READ.into() };
                self.say(said);
                self.read_next();
                return;
            }
        };
        // One piece for every permit: the session hands them out as it wants
        // more, so nothing is written far ahead of what is heard.
        let more = Arc::new(Semaphore::new(0));
        let permits = more.clone();
        let tx = self.tx.clone();
        reading.more = Some(more);
        reading.task = Some(
            tokio::spawn(async move {
                loop {
                    let Ok(permit) = permits.acquire().await else { return };
                    permit.forget();
                    let piece = pieces.next().await;
                    let last = !matches!(piece, Some(Ok(_)));
                    if tx.send(Internal::ReadingPiece { reading: id, piece }).is_err() || last {
                        return;
                    }
                }
            })
            .abort_handle(),
        );
        self.read_on();
    }

    fn on_reading_piece(&mut self, id: u64, piece: Option<CommandResult<Piece>>) {
        let Some(reading) = self.reading.as_mut().filter(|r| r.id == id) else { return };
        reading.asked = false;
        let mut said = Vec::new();
        let mut failed = None;
        match piece {
            Some(Ok(piece)) => {
                if piece.part != reading.part {
                    said.extend(reading.splitter.finish());
                    reading.splitter = SentenceSplitter::new();
                    reading.part = piece.part;
                }
                said.extend(reading.splitter.push(&piece.text));
            }
            Some(Err(e)) => {
                said.extend(reading.splitter.finish());
                reading.ended = true;
                failed = Some(e.message);
            }
            None => {
                said.extend(reading.splitter.finish());
                reading.ended = true;
            }
        }
        for sentence in said {
            let text = sentence.trim().to_string();
            if text.is_empty() {
                continue;
            }
            if let Some(reading) = self.reading.as_mut() {
                reading.queued += 1;
                reading.read_any = true;
            }
            self.enqueue(text, Kind::Reading);
        }
        if let Some(message) = failed {
            let any = self.reading.as_ref().is_some_and(|r| r.read_any);
            self.trouble(message);
            self.say(if any { COULD_NOT_FINISH } else { COULD_NOT_READ }.to_string());
        }
        self.read_on();
    }

    /// One sentence of what is being read has played, or could not be said.
    fn read_one(&mut self) {
        if let Some(reading) = self.reading.as_mut() {
            reading.queued = reading.queued.saturating_sub(1);
        }
        self.read_on();
    }

    /// Ask for more of what is being read, if little enough of it is waiting
    /// to be played; let it go once all of it has been.
    fn read_on(&mut self) {
        let Some(reading) = self.reading.as_mut() else { return };
        if reading.finished() {
            self.reading = None;
            self.read_next();
            return;
        }
        if reading.ended || reading.asked || reading.queued >= READ_AHEAD {
            return;
        }
        if let Some(more) = &reading.more {
            reading.asked = true;
            more.add_permits(1);
        }
    }

    /// Throw away everything queued, being synthesised or held by the
    /// transport.
    fn clear_audio(&mut self) {
        self.narrator.clear();
        self.paused = false;
        self.send(VoiceOutput::Clear);
    }

    /// Let held playback go on, unless somebody is talking again by now: their
    /// next words decide.
    fn resume_if_paused(&mut self) {
        if self.paused && !self.speaking {
            self.paused = false;
            self.send(VoiceOutput::Resume);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speech::detect::{Detected, SpeechDetector};
    use crate::speech::synth::{Speech, Synthesiser};
    use crate::speech::utterance::UtteranceTranscriber;
    use crate::voice::BoxFuture;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::sync::Notify;

    // ---- the fakes ---------------------------------------------------------

    /// Sentinel audio: `[1]` is somebody starting to talk, `[2]` their stopping.
    struct FakeDetector {
        modes: Arc<Mutex<Vec<DetectorMode>>>,
    }

    impl SpeechDetector for FakeDetector {
        fn push(&mut self, samples: &[i16]) -> Vec<Detected> {
            match samples {
                [1] => vec![Detected::Started],
                [2] => vec![Detected::Ended { samples: vec![2], start_ms: 0 }],
                _ => Vec::new(),
            }
        }
        fn set_mode(&mut self, mode: DetectorMode) {
            self.modes.lock().unwrap().push(mode);
        }
        fn reset(&mut self) {}
    }

    /// Hears what it is told to, one utterance at a time.
    struct FakeHearing(Mutex<VecDeque<String>>);

    impl UtteranceTranscriber for FakeHearing {
        fn hear<'a>(&'a self, _samples: &'a [i16]) -> BoxFuture<'a, CommandResult<String>> {
            let next = self.0.lock().unwrap().pop_front().unwrap_or_default();
            Box::pin(async move { Ok(next) })
        }
    }

    /// Says everything, and remembers what it was asked to.
    struct FakeSynthesiser {
        asked: Arc<Mutex<Vec<String>>>,
        /// Every sentence fails, as with a voice whose key is wrong.
        fails: Arc<AtomicBool>,
    }

    impl Synthesiser for FakeSynthesiser {
        fn synthesise<'a>(&'a self, text: &'a str) -> BoxFuture<'a, CommandResult<Speech>> {
            self.asked.lock().unwrap().push(text.to_string());
            let fails = self.fails.load(Ordering::SeqCst);
            Box::pin(async move {
                if fails {
                    return Err(CommandError::new(codes::INVALID, "the voice is unavailable"));
                }
                Ok(Speech { samples: vec![5; 8], sample_rate: 16_000 })
            })
        }
    }

    /// What a scripted turn does: sends its events, optionally waits to be
    /// stopped or answered, then ends.
    struct Script {
        events: Vec<AgentEvent>,
        /// Wait for a cancel or an answer before ending.
        hang: bool,
        /// Send `Finished` as it ends, as a real turn does unless it failed.
        finish: Option<&'static str>,
        /// End in this error.
        error: Option<&'static str>,
    }

    #[derive(Default)]
    struct Record {
        scripts: Mutex<VecDeque<Script>>,
        prompts: Mutex<Vec<String>>,
        cancels: AtomicUsize,
        answers: Mutex<Vec<(String, ConfirmAnswer)>>,
        cuts: Mutex<Vec<(String, String)>>,
        running: AtomicBool,
        cancelled: AtomicBool,
        release: Notify,
        /// Whether each turn's words were heard with nothing of ours audible.
        clear: Mutex<Vec<bool>>,
        /// What the next turn asks to have read aloud.
        read_aloud: Mutex<Vec<ReadAloud>>,
        /// What opening each gives, in turn: its pieces, or why not.
        reading: Mutex<VecDeque<CommandResult<Vec<Piece>>>>,
        /// What was opened to be read, and how many of its pieces were taken.
        opened: Mutex<Vec<ReadAloud>>,
        /// Every piece of what is read fails, as a model gone wrong does.
        fail_reading: Mutex<bool>,
        pulled: Arc<AtomicUsize>,
    }

    struct FakeAssistant(Arc<Record>);

    impl VoiceAssistant for FakeAssistant {
        fn turn(
            &self,
            prompt: String,
            heard_clear: bool,
            _on_screen: Option<OnScreen>,
            sink: Sink,
        ) -> BoxFuture<'static, CommandResult<TurnOutcome>> {
            let record = self.0.clone();
            record.prompts.lock().unwrap().push(prompt);
            record.clear.lock().unwrap().push(heard_clear);
            let script = record.scripts.lock().unwrap().pop_front().unwrap_or(Script {
                events: Vec::new(),
                hang: false,
                finish: Some("unscripted"),
                error: None,
            });
            record.running.store(true, Ordering::SeqCst);
            record.cancelled.store(false, Ordering::SeqCst);
            // As the real turn reads it from the calls that ran.
            let ends = script.events.iter().any(|e| {
                matches!(e, AgentEvent::ToolStarted { name, .. } if name == crate::voice::END_TOOL)
            });
            let read_aloud = std::mem::take(&mut *record.read_aloud.lock().unwrap());
            Box::pin(async move {
                for event in script.events {
                    (sink)(event);
                }
                if script.hang {
                    record.release.notified().await;
                }
                let stopped = record.cancelled.load(Ordering::SeqCst);
                record.running.store(false, Ordering::SeqCst);
                if let Some(id) = script.finish {
                    (sink)(AgentEvent::Finished { message_id: id.to_string() });
                }
                match script.error {
                    Some(message) => Err(CommandError::new(codes::AGENT, message)),
                    None => Ok(TurnOutcome {
                        stopped,
                        ended_voice: ends && !stopped,
                        read_aloud: if stopped { Vec::new() } else { read_aloud },
                    }),
                }
            })
        }

        fn cancel(&self) -> bool {
            if !self.0.running.load(Ordering::SeqCst) {
                return false;
            }
            self.0.cancels.fetch_add(1, Ordering::SeqCst);
            self.0.cancelled.store(true, Ordering::SeqCst);
            self.0.release.notify_one();
            true
        }

        fn answer(&self, call_id: &str, answer: ConfirmAnswer) -> bool {
            self.0.answers.lock().unwrap().push((call_id.to_string(), answer));
            self.0.release.notify_one();
            true
        }

        fn keep_heard(
            &self,
            message_id: String,
            heard: String,
        ) -> BoxFuture<'static, CommandResult<()>> {
            self.0.cuts.lock().unwrap().push((message_id, heard));
            Box::pin(async { Ok(()) })
        }

        fn open_reading(&self, what: ReadAloud) -> BoxFuture<'static, CommandResult<PieceStream>> {
            self.0.opened.lock().unwrap().push(what);
            let reading = self.0.reading.lock().unwrap().pop_front().unwrap_or(Ok(Vec::new()));
            let pulled = self.0.pulled.clone();
            let fails = *self.0.fail_reading.lock().unwrap();
            Box::pin(async move {
                let pieces = reading?;
                if fails {
                    let failure = CommandError::new(codes::QUICK, "the model went away");
                    return Ok(futures::stream::iter([Err(failure)]).boxed());
                }
                let stream = futures::stream::iter(pieces).map(move |piece| {
                    pulled.fetch_add(1, Ordering::SeqCst);
                    Ok(piece)
                });
                Ok(stream.boxed())
            })
        }
    }

    // ---- the rig -----------------------------------------------------------

    const FIRST: &str = "The weather today is sunny and quite warm in town.";
    const SECOND: &str = "Tomorrow it will rain heavily for most of the afternoon.";

    fn started(id: &str) -> AgentEvent {
        AgentEvent::Started { message_id: id.to_string() }
    }

    fn delta(text: &str) -> AgentEvent {
        AgentEvent::Delta { text: text.to_string() }
    }

    /// A reply that has said two whole sentences and begun a third, and is
    /// still going until somebody stops it.
    fn long_reply() -> Script {
        Script {
            events: vec![started("m1"), delta(&format!("{FIRST} {SECOND} And on the"))],
            hang: true,
            finish: Some("m1"),
            error: None,
        }
    }

    fn short_reply(id: &'static str, text: &str) -> Script {
        Script {
            events: vec![started(id), delta(text)],
            hang: false,
            finish: Some(id),
            error: None,
        }
    }

    /// A reply that ends the conversation, as a model calling
    /// [`crate::voice::END_TOOL`] and then saying goodbye does.
    fn goodbye(id: &'static str, text: &str) -> Script {
        Script {
            events: vec![
                started(id),
                AgentEvent::ToolPreparing {
                    call_id: "end".into(),
                    name: crate::voice::END_TOOL.into(),
                },
                AgentEvent::ToolStarted {
                    call_id: "end".into(),
                    name: crate::voice::END_TOOL.into(),
                    arguments: serde_json::json!({}),
                },
                delta(text),
            ],
            hang: false,
            finish: Some(id),
            error: None,
        }
    }

    fn confirmation(kind: &str, subject: &str) -> Script {
        confirmation_for("web_search", kind, subject)
    }

    fn confirmation_for(tool: &str, kind: &str, subject: &str) -> Script {
        Script {
            events: vec![
                started("m1"),
                AgentEvent::ConfirmationRequired {
                    call_id: "c1".to_string(),
                    name: tool.to_string(),
                    subject: subject.to_string(),
                    arguments: serde_json::json!({}),
                    kind: kind.to_string(),
                    can_park: false,
                },
            ],
            hang: true,
            finish: Some("m1"),
            error: None,
        }
    }

    struct Rig {
        link: VoiceLink,
        record: Arc<Record>,
        asked: Arc<Mutex<Vec<String>>>,
        modes: Arc<Mutex<Vec<DetectorMode>>>,
        voice_fails: Arc<AtomicBool>,
        seen: Vec<VoiceOutput>,
    }

    impl Rig {
        /// A session whose assistant plays `scripts` in turn and whose
        /// microphone hears `heard` in turn.
        async fn start(scripts: Vec<Script>, heard: &[&str]) -> Rig {
            let record = Arc::new(Record::default());
            record.scripts.lock().unwrap().extend(scripts);
            let asked = Arc::new(Mutex::new(Vec::new()));
            let modes = Arc::new(Mutex::new(Vec::new()));
            let voice_fails = Arc::new(AtomicBool::new(false));
            let live = LiveTranscription::start(
                Box::new(FakeDetector { modes: modes.clone() }),
                Arc::new(FakeHearing(Mutex::new(heard.iter().map(|h| h.to_string()).collect()))),
            );
            let link = spawn(VoiceParts {
                live,
                synthesiser: Arc::new(FakeSynthesiser {
                    asked: asked.clone(),
                    fails: voice_fails.clone(),
                }),
                assistant: Arc::new(FakeAssistant(record.clone())),
                needs_a_click: confirm::NEEDS_A_CLICK,
            });
            let mut rig = Rig { link, record, asked, modes, voice_fails, seen: Vec::new() };
            rig.until("the first state", |o| is_state(o, VoiceState::Listening)).await;
            rig
        }

        fn send(&self, input: VoiceInput) {
            self.link.input.send(input).expect("the session is listening");
        }

        /// Somebody starts talking.
        fn begin(&self) {
            self.send(VoiceInput::Audio(vec![1]));
        }

        /// ... and stops.
        fn end(&self) {
            self.send(VoiceInput::Audio(vec![2]));
        }

        fn talk(&self) {
            self.begin();
            self.end();
        }

        async fn next(&mut self) -> Option<VoiceOutput> {
            tokio::time::timeout(Duration::from_secs(5), self.link.output.recv())
                .await
                .expect("timed out waiting for the session")
        }

        /// Read outputs, keeping them, until `hit`. Its index in `seen`.
        async fn until(&mut self, what: &str, hit: impl Fn(&VoiceOutput) -> bool) -> usize {
            loop {
                let output = self
                    .next()
                    .await
                    .unwrap_or_else(|| panic!("the session ended waiting for {what}"));
                let found = hit(&output);
                self.seen.push(output);
                if found {
                    return self.seen.len() - 1;
                }
            }
        }

        /// The id of the next sentence played.
        async fn next_play(&mut self) -> u64 {
            let at =
                self.until("a sentence to play", |o| matches!(o, VoiceOutput::Play { .. })).await;
            match &self.seen[at] {
                VoiceOutput::Play { utterance, .. } => *utterance,
                _ => unreachable!(),
            }
        }

        /// Report everything played until the session is listening again.
        async fn play_out(&mut self) {
            loop {
                let output = self.next().await.expect("the session ended");
                let (play, done) = match &output {
                    VoiceOutput::Play { utterance, .. } => (Some(*utterance), false),
                    other => (None, is_state(other, VoiceState::Listening)),
                };
                self.seen.push(output);
                if let Some(utterance) = play {
                    self.send(VoiceInput::Played { utterance });
                }
                if done {
                    return;
                }
            }
        }

        fn states(&self) -> Vec<VoiceState> {
            self.seen
                .iter()
                .filter_map(|o| match o {
                    VoiceOutput::Event(VoiceEvent::State { state }) => Some(*state),
                    _ => None,
                })
                .collect()
        }

        fn events(&self) -> Vec<&VoiceEvent> {
            self.seen
                .iter()
                .filter_map(|o| match o {
                    VoiceOutput::Event(event) => Some(event),
                    _ => None,
                })
                .collect()
        }

        fn heard_texts(&self) -> Vec<String> {
            self.events()
                .into_iter()
                .filter_map(|e| match e {
                    VoiceEvent::Heard { text } => Some(text.clone()),
                    _ => None,
                })
                .collect()
        }

        fn count(&self, what: impl Fn(&VoiceOutput) -> bool) -> usize {
            self.seen.iter().filter(|o| what(o)).count()
        }

        fn said(&self) -> Vec<String> {
            self.asked.lock().unwrap().clone()
        }

        fn answers(&self) -> Vec<(String, ConfirmAnswer)> {
            self.record.answers.lock().unwrap().clone()
        }
    }

    fn is_state(output: &VoiceOutput, want: VoiceState) -> bool {
        matches!(output, VoiceOutput::Event(VoiceEvent::State { state }) if *state == want)
    }

    fn in_order(states: &[VoiceState], want: &[VoiceState]) -> bool {
        let mut wanted = want.iter();
        let mut next = wanted.next();
        for state in states {
            if Some(state) == next {
                next = wanted.next();
            }
        }
        next.is_none()
    }

    async fn eventually(what: &str, check: impl Fn() -> bool) {
        for _ in 0..200 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("never became true: {what}");
    }

    // ---- the tests ---------------------------------------------------------

    #[tokio::test]
    async fn a_plain_exchange() {
        let mut rig = Rig::start(
            vec![short_reply("m1", &format!("{FIRST} {SECOND}"))],
            &["what is the weather"],
        )
        .await;
        rig.talk();
        rig.play_out().await;

        assert!(
            in_order(
                &rig.states(),
                &[
                    VoiceState::Listening,
                    VoiceState::Hearing,
                    VoiceState::Understanding,
                    VoiceState::Thinking,
                    VoiceState::Speaking,
                    VoiceState::Listening,
                ]
            ),
            "{:?}",
            rig.states()
        );
        assert_eq!(rig.heard_texts(), ["what is the weather"]);
        assert_eq!(*rig.record.prompts.lock().unwrap(), ["what is the weather"]);
        assert_eq!(rig.said().join(" "), format!("{FIRST} {SECOND}"));
        // The words came back before the audio, and the turn's own events
        // reached the screen.
        let heard_at =
            rig.events().iter().position(|e| matches!(e, VoiceEvent::Heard { .. })).expect("heard");
        let started_at = rig
            .events()
            .iter()
            .position(|e| matches!(e, VoiceEvent::Agent { event: AgentEvent::Started { .. } }))
            .expect("started");
        assert!(heard_at < started_at);
        // Everything was heard, so the stored reply is left alone.
        assert_eq!(rig.count(|o| matches!(o, VoiceOutput::Event(VoiceEvent::ReplyCut { .. }))), 0);
        assert!(rig.record.cuts.lock().unwrap().is_empty());
        // The detector was guarded while it spoke, and is open again.
        let modes = rig.modes.clone();
        eventually("guarded then open", move || {
            let modes = modes.lock().unwrap();
            modes.contains(&DetectorMode::Guarded) && modes.last() == Some(&DetectorMode::Open)
        })
        .await;
    }

    #[tokio::test]
    async fn a_barge_in_that_was_only_an_echo_resumes() {
        let mut rig = Rig::start(vec![long_reply()], &["what is the weather", FIRST]).await;
        rig.talk();
        rig.next_play().await;
        // Something that is the assistant's own sentence comes back.
        rig.begin();
        rig.until("the pause", |o| matches!(o, VoiceOutput::Pause)).await;
        rig.end();
        rig.until("the resume", |o| matches!(o, VoiceOutput::Resume)).await;

        assert_eq!(rig.record.cancels.load(Ordering::SeqCst), 0, "the turn went on");
        assert_eq!(rig.count(|o| matches!(o, VoiceOutput::Clear)), 0);
        assert_eq!(rig.heard_texts(), ["what is the weather"], "nothing new was announced");
    }

    #[tokio::test]
    async fn a_barge_in_with_real_words_cuts_the_reply_and_starts_a_new_turn() {
        let mut rig = Rig::start(
            vec![long_reply(), short_reply("m2", "Your calendar is free all day tomorrow.")],
            &["what is the weather", "actually tell me about my calendar tomorrow"],
        )
        .await;
        rig.talk();
        // Two sentences are with the transport; only the first is heard.
        let first = rig.next_play().await;
        rig.next_play().await;
        rig.send(VoiceInput::Played { utterance: first });
        rig.begin();
        rig.until("the pause", |o| matches!(o, VoiceOutput::Pause)).await;
        rig.end();
        rig.until("the new turn", |o| {
            matches!(o, VoiceOutput::Event(VoiceEvent::Heard { text })
                if text == "actually tell me about my calendar tomorrow")
        })
        .await;

        let order: Vec<&str> = rig
            .seen
            .iter()
            .filter_map(|o| match o {
                VoiceOutput::Pause => Some("pause"),
                VoiceOutput::Clear => Some("clear"),
                VoiceOutput::Event(VoiceEvent::ReplyCut { .. }) => Some("cut"),
                VoiceOutput::Event(VoiceEvent::Heard { .. }) => Some("heard"),
                _ => None,
            })
            .collect();
        assert_eq!(order, ["heard", "pause", "clear", "cut", "heard"]);
        assert_eq!(rig.record.cancels.load(Ordering::SeqCst), 1);
        // What was kept is exactly what played: the first sentence.
        let first_said = rig.said()[0].clone();
        assert_eq!(*rig.record.cuts.lock().unwrap(), [("m1".to_string(), first_said.clone())]);
        assert!(rig.events().iter().any(|e| matches!(
            e, VoiceEvent::ReplyCut { message_id, text } if message_id == "m1" && *text == first_said
        )));
        assert_eq!(
            *rig.record.prompts.lock().unwrap(),
            ["what is the weather", "actually tell me about my calendar tomorrow"]
        );
        rig.play_out().await;
    }

    #[tokio::test]
    async fn a_stop_word_stops_and_starts_nothing() {
        let mut rig = Rig::start(vec![long_reply()], &["what is the weather", "stop"]).await;
        rig.talk();
        rig.next_play().await;
        rig.talk();
        rig.until("the cut", |o| matches!(o, VoiceOutput::Event(VoiceEvent::ReplyCut { .. })))
            .await;
        rig.until("listening", |o| is_state(o, VoiceState::Listening)).await;

        assert_eq!(rig.record.cancels.load(Ordering::SeqCst), 1);
        assert_eq!(rig.count(|o| matches!(o, VoiceOutput::Clear)), 1);
        // Nothing was heard of it, so nothing is kept.
        assert_eq!(*rig.record.cuts.lock().unwrap(), [("m1".to_string(), String::new())]);
        assert_eq!(rig.heard_texts(), ["what is the weather"], "'stop' is not a message");
        assert_eq!(rig.record.prompts.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_spoken_yes_approves_a_search() {
        let mut rig =
            Rig::start(vec![confirmation("search", "best e-bike")], &["look up an e-bike", "yes"])
                .await;
        rig.talk();
        let question = rig.next_play().await;
        assert_eq!(rig.said(), ["Should I search the web for best e-bike?"]);
        rig.send(VoiceInput::Played { utterance: question });
        rig.until("asking", |o| is_state(o, VoiceState::Asking)).await;
        // They started after the question had played and died away, so it
        // counts.
        tokio::time::sleep(ANSWER_GRACE + Duration::from_millis(50)).await;
        rig.talk();
        eventually("the answer", {
            let record = rig.record.clone();
            move || !record.answers.lock().unwrap().is_empty()
        })
        .await;
        assert_eq!(rig.answers(), [("c1".to_string(), ConfirmAnswer::Confirm)]);
        rig.until("listening", |o| is_state(o, VoiceState::Listening)).await;
        assert_eq!(rig.heard_texts(), ["look up an e-bike"], "'yes' is not a message");
        assert_eq!(rig.record.cancels.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_spoken_no_declines_it() {
        let mut rig = Rig::start(
            vec![confirmation("fetch", "https://news.example.org/a/long/path")],
            &["open the news", "no"],
        )
        .await;
        rig.talk();
        let question = rig.next_play().await;
        assert_eq!(rig.said(), ["Should I open news.example.org slash a slash long slash path?"]);
        rig.send(VoiceInput::Played { utterance: question });
        rig.until("asking", |o| is_state(o, VoiceState::Asking)).await;
        rig.talk();
        eventually("the answer", {
            let record = rig.record.clone();
            move || !record.answers.lock().unwrap().is_empty()
        })
        .await;
        assert_eq!(rig.answers(), [("c1".to_string(), ConfirmAnswer::Decline)]);
    }

    #[tokio::test]
    async fn speech_begun_before_the_question_finished_is_not_an_answer() {
        let mut rig =
            Rig::start(vec![confirmation("search", "best e-bike")], &["look up an e-bike", "yes"])
                .await;
        rig.talk();
        let question = rig.next_play().await;
        // They start while it is still being asked, and it finishes while
        // they are talking.
        rig.begin();
        rig.until("the pause", |o| matches!(o, VoiceOutput::Pause)).await;
        rig.send(VoiceInput::Played { utterance: question });
        rig.end();
        // It is either the question coming back or an interruption; it is
        // never the answer.
        rig.until("it to be dealt with", |o| matches!(o, VoiceOutput::Resume | VoiceOutput::Clear))
            .await;
        assert!(rig.answers().is_empty(), "{:?}", rig.answers());
    }

    /// The tail of the question may still be in the air, and the question can
    /// end in a word somebody else chose.
    #[tokio::test]
    async fn a_yes_straight_after_the_question_is_not_an_answer() {
        let mut rig =
            Rig::start(vec![confirmation("search", "say yes")], &["look up an e-bike", "yes"])
                .await;
        rig.talk();
        let question = rig.next_play().await;
        rig.send(VoiceInput::Played { utterance: question });
        rig.until("asking", |o| is_state(o, VoiceState::Asking)).await;
        rig.talk();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(rig.answers().is_empty(), "{:?}", rig.answers());
        assert_eq!(rig.record.cancels.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_destructive_confirmation_waits_for_a_click() {
        let mut rig =
            Rig::start(vec![confirmation("destructive", "the deck")], &["delete the deck", "yes"])
                .await;
        rig.talk();
        let said = rig.next_play().await;
        assert_eq!(rig.said(), [confirm::NEEDS_A_CLICK]);
        // The state came as the card did, ahead of the sentence.
        assert!(rig.states().contains(&VoiceState::WaitingForClick), "{:?}", rig.states());
        rig.send(VoiceInput::Played { utterance: said });
        // Saying yes is not a click: it is talking over a turn that is
        // waiting, an ordinary interruption.
        rig.talk();
        eventually("the interruption", {
            let record = rig.record.clone();
            move || record.cancels.load(Ordering::SeqCst) == 1
        })
        .await;
        assert!(rig.answers().is_empty(), "{:?}", rig.answers());
    }

    #[tokio::test]
    async fn stop_while_speaking_cuts_the_reply_and_ends() {
        let mut rig = Rig::start(vec![long_reply()], &["what is the weather"]).await;
        rig.talk();
        rig.next_play().await;
        rig.send(VoiceInput::Stop);
        // Everything up to the end, and then the output closes.
        while let Some(output) = rig.next().await {
            rig.seen.push(output);
        }

        assert_eq!(rig.record.cancels.load(Ordering::SeqCst), 1);
        assert_eq!(rig.count(|o| matches!(o, VoiceOutput::Clear)), 1);
        assert_eq!(*rig.record.cuts.lock().unwrap(), [("m1".to_string(), String::new())]);
        let order: Vec<&str> = rig
            .seen
            .iter()
            .filter_map(|o| match o {
                VoiceOutput::Clear => Some("clear"),
                VoiceOutput::Event(VoiceEvent::ReplyCut { .. }) => Some("cut"),
                VoiceOutput::Event(VoiceEvent::Ended { error: None }) => Some("ended"),
                _ => None,
            })
            .collect();
        assert_eq!(order, ["clear", "cut", "ended"]);
    }

    #[tokio::test]
    async fn a_closed_input_is_a_stop() {
        let mut rig = Rig::start(Vec::new(), &[]).await;
        let VoiceLink { input, output } = rig.link;
        drop(input);
        rig.link = VoiceLink { input: tokio::sync::mpsc::unbounded_channel().0, output };
        let at = rig
            .until("the end", |o| matches!(o, VoiceOutput::Event(VoiceEvent::Ended { .. })))
            .await;
        assert!(matches!(&rig.seen[at], VoiceOutput::Event(VoiceEvent::Ended { error: None })));
        assert!(rig.next().await.is_none(), "the output closes after Ended");
    }

    #[tokio::test]
    async fn something_to_say_is_said_without_a_turn() {
        let mut rig = Rig::start(Vec::new(), &[]).await;
        rig.send(VoiceInput::Say(
            "This is your **Morning brief**.\n\nThree things are due today.".into(),
        ));
        rig.play_out().await;
        assert_eq!(rig.said(), ["This is your Morning brief.", "Three things are due today."]);
        assert!(rig.record.prompts.lock().unwrap().is_empty(), "no turn was run for it");
    }

    #[tokio::test]
    async fn a_failed_turn_is_reported_and_apologised_for() {
        let failing = Script {
            events: vec![started("m1"), AgentEvent::Failed { message: "boom".to_string() }],
            hang: false,
            finish: None,
            error: Some("boom"),
        };
        let mut rig = Rig::start(vec![failing], &["do something"]).await;
        rig.talk();
        rig.play_out().await;

        let troubles: Vec<_> = rig
            .events()
            .into_iter()
            .filter_map(|e| match e {
                VoiceEvent::Trouble { message } => Some(message.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(troubles, ["boom"], "once, though it failed twice over");
        assert_eq!(rig.said(), [SORRY]);
    }

    #[tokio::test]
    async fn a_tool_is_announced_once() {
        let script = Script {
            events: vec![
                started("m1"),
                AgentEvent::ToolPreparing { call_id: "c1".into(), name: "web_search".into() },
                AgentEvent::ToolStarted {
                    call_id: "c1".into(),
                    name: "web_search".into(),
                    arguments: serde_json::json!({}),
                },
                AgentEvent::ToolPreparing { call_id: "c2".into(), name: "read_web_page".into() },
            ],
            hang: false,
            finish: Some("m1"),
            error: None,
        };
        let mut rig = Rig::start(vec![script], &["search for it"]).await;
        rig.talk();
        rig.play_out().await;
        assert_eq!(rig.said(), [text::tool_phrase("web_search")]);
        // A phrase of the session's own is not the reply, so it is not kept.
        assert!(rig.record.cuts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_assistant_ends_the_conversation_once_its_goodbye_is_heard() {
        let mut rig = Rig::start(vec![goodbye("m1", "Talk soon.")], &["that's all I have"]).await;
        rig.talk();
        let utterance = rig.next_play().await;
        assert_eq!(rig.said(), ["Talk soon."], "no tool phrase before a goodbye");
        // Still there while it is being said.
        tokio::time::sleep(Duration::from_millis(50)).await;
        while let Ok(output) = rig.link.output.try_recv() {
            let ended = matches!(output, VoiceOutput::Event(VoiceEvent::Ended { .. }));
            assert!(!ended, "it does not end before the goodbye has played");
            rig.seen.push(output);
        }
        rig.send(VoiceInput::Played { utterance });
        let at = rig
            .until("the end", |o| matches!(o, VoiceOutput::Event(VoiceEvent::Ended { .. })))
            .await;
        assert!(matches!(&rig.seen[at], VoiceOutput::Event(VoiceEvent::Ended { error: None })));
        assert!(rig.next().await.is_none(), "the output closes after Ended");
        assert!(rig.record.cuts.lock().unwrap().is_empty(), "all of the goodbye was heard");
    }

    #[tokio::test]
    async fn talking_just_after_the_goodbye_is_still_heard() {
        let mut rig = Rig::start(
            vec![goodbye("m1", "Talk soon."), short_reply("m2", "Sure.")],
            &["that's all", "oh, one more thing"],
        )
        .await;
        rig.talk();
        let utterance = rig.next_play().await;
        rig.send(VoiceInput::Played { utterance });
        // Within the moment the goodbye is given to leave the speaker.
        rig.talk();
        rig.until("the new words", |o| {
            matches!(o, VoiceOutput::Event(VoiceEvent::Heard { text }) if text == "oh, one more thing")
        })
        .await;
        rig.play_out().await;
        let ended = rig.count(|o| matches!(o, VoiceOutput::Event(VoiceEvent::Ended { .. })));
        assert_eq!(ended, 0, "the session goes on");
    }

    #[tokio::test]
    async fn speaking_over_the_goodbye_carries_the_conversation_on() {
        let mut rig = Rig::start(
            vec![goodbye("m1", &format!("{FIRST} {SECOND}")), short_reply("m2", "Sure.")],
            &["that's it", "actually one more thing"],
        )
        .await;
        rig.talk();
        rig.next_play().await;
        // Talked over: the goodbye is cut, and a new turn takes it back.
        rig.talk();
        rig.until("the new words", |o| {
            matches!(o, VoiceOutput::Event(VoiceEvent::Heard { text }) if text == "actually one more thing")
        })
        .await;
        rig.play_out().await;
        assert_eq!(rig.heard_texts(), ["that's it", "actually one more thing"]);
        assert_eq!(
            rig.count(|o| matches!(o, VoiceOutput::Event(VoiceEvent::Ended { .. }))),
            0,
            "the session goes on"
        );
    }

    #[tokio::test]
    async fn muted_audio_is_not_listened_to() {
        let mut rig = Rig::start(Vec::new(), &["hello"]).await;
        rig.send(VoiceInput::Muted(true));
        rig.talk();
        // Nothing was detected, so nothing happens; unmuted, it does.
        rig.send(VoiceInput::Muted(false));
        rig.talk();
        rig.until("the words", |o| matches!(o, VoiceOutput::Event(VoiceEvent::Heard { .. }))).await;
        assert_eq!(rig.heard_texts(), ["hello"]);
        let heard = rig.states().iter().filter(|s| **s == VoiceState::Hearing).count();
        assert_eq!(heard, 1, "only the unmuted speech was heard");
    }

    #[test]
    fn a_reply_with_a_tail_still_held_has_unheard_words() {
        let mut turn = TurnState::new(1);
        assert!(!turn.unheard(), "nothing said, nothing missed");
        turn.reply_total = 2;
        turn.reply_played = 2;
        turn.flushed = true;
        assert!(!turn.unheard(), "all of it played");
        turn.reply_played = 1;
        assert!(turn.unheard());
    }

    /// Played means handed to the device, so the last words of a reply can come
    /// back after it, and must not become the next question.
    #[tokio::test]
    async fn the_tail_of_a_reply_is_not_taken_for_the_person() {
        let mut rig =
            Rig::start(vec![short_reply("m1", FIRST)], &["what is the weather", FIRST]).await;
        rig.talk();
        rig.play_out().await;
        rig.talk();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(*rig.record.prompts.lock().unwrap(), ["what is the weather"]);
    }

    /// What was said to one question cannot answer the next.
    #[tokio::test]
    async fn an_answer_is_only_to_the_question_it_was_said_to() {
        let mut rig = Rig::start(
            vec![confirmation("search", "best e-bike")],
            &["look up an e-bike", "what was that", "yes"],
        )
        .await;
        rig.talk();
        let question = rig.next_play().await;
        rig.send(VoiceInput::Played { utterance: question });
        rig.until("asking", |o| is_state(o, VoiceState::Asking)).await;
        tokio::time::sleep(ANSWER_GRACE + Duration::from_millis(50)).await;
        // The first is no answer, so the question is asked afresh; the second
        // began before that one had been put.
        rig.talk();
        rig.talk();
        rig.next_play().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(rig.answers().is_empty(), "{:?}", rig.answers());
    }

    /// A sentence the voice could not say was not skipped by the person, so the
    /// reply they can read stays as it was.
    #[tokio::test]
    async fn a_voice_that_fails_leaves_the_reply_alone() {
        let mut rig = Rig::start(vec![short_reply("m1", FIRST)], &["what is the weather"]).await;
        rig.voice_fails.store(true, Ordering::SeqCst);
        rig.talk();
        rig.until("the trouble", |o| matches!(o, VoiceOutput::Event(VoiceEvent::Trouble { .. })))
            .await;
        rig.send(VoiceInput::Stop);
        while let Some(output) = rig.next().await {
            rig.seen.push(output);
        }
        assert!(rig.record.cuts.lock().unwrap().is_empty());
    }

    /// The thread opened a reply when the words were heard, and has to be told
    /// how it ended even if the turn never began.
    #[tokio::test]
    async fn a_turn_that_fails_before_it_starts_still_ends_on_screen() {
        let failing =
            Script { events: Vec::new(), hang: false, finish: None, error: Some("no key") };
        let mut rig = Rig::start(vec![failing], &["what is the weather"]).await;
        rig.talk();
        rig.until("the failure", |o| {
            matches!(o, VoiceOutput::Event(VoiceEvent::Agent { event: AgentEvent::Failed { .. } }))
        })
        .await;
    }

    // ---- the sound of working ---------------------------------------------

    /// Whether the working sound was on as each state was announced.
    fn working_at_each_state(seen: &[VoiceOutput]) -> Vec<(VoiceState, bool)> {
        let mut on = false;
        let mut out = Vec::new();
        for output in seen {
            match output {
                VoiceOutput::Working(now) => on = *now,
                VoiceOutput::Event(VoiceEvent::State { state }) => out.push((*state, on)),
                _ => {}
            }
        }
        out
    }

    #[tokio::test]
    async fn the_working_sound_plays_while_it_thinks_and_never_while_listening() {
        let mut rig = Rig::start(
            vec![short_reply("m1", &format!("{FIRST} {SECOND}"))],
            &["what is the weather"],
        )
        .await;
        rig.talk();
        rig.play_out().await;
        let states = working_at_each_state(&rig.seen);
        assert!(
            states.iter().any(|(s, on)| *s == VoiceState::Thinking && *on),
            "it works out loud: {states:?}"
        );
        for (state, on) in &states {
            match state {
                VoiceState::Understanding | VoiceState::Thinking => {
                    assert!(*on, "{state:?} in {states:?}");
                }
                // Until the first of the reply has been made into a voice.
                VoiceState::Speaking => {}
                _ => assert!(!*on, "{state:?} in {states:?}"),
            }
        }
        // Off by the time anything is played, and off at the end.
        let first_play = rig.seen.iter().position(|o| matches!(o, VoiceOutput::Play { .. }));
        let off_after = rig
            .seen
            .iter()
            .skip(first_play.unwrap())
            .any(|o| matches!(o, VoiceOutput::Working(false)));
        assert!(off_after);
        let last = rig.seen.iter().rev().find_map(|o| match o {
            VoiceOutput::Working(on) => Some(*on),
            _ => None,
        });
        assert_eq!(last, Some(false));
    }

    #[tokio::test]
    async fn the_working_sound_stops_when_they_talk_again() {
        let mut rig = Rig::start(vec![long_reply_after_a_tool()], &["what is the weather"]).await;
        rig.talk();
        rig.until("working", |o| matches!(o, VoiceOutput::Working(true))).await;
        // Nothing is said, so the detector stays open: a short "stop" said
        // while a slow tool runs is heard.
        assert!(!rig.modes.lock().unwrap().contains(&DetectorMode::Guarded));
        rig.begin();
        let at = rig.until("hearing", |o| is_state(o, VoiceState::Hearing)).await;
        assert!(
            rig.seen[..at].iter().rev().find_map(|o| match o {
                VoiceOutput::Working(on) => Some(*on),
                _ => None,
            }) == Some(false),
            "off before they are heard"
        );
        rig.send(VoiceInput::Stop);
        while let Some(output) = rig.next().await {
            rig.seen.push(output);
        }
    }

    /// A turn that is still working, with nothing said yet.
    fn long_reply_after_a_tool() -> Script {
        Script { events: vec![started("m1")], hang: true, finish: Some("m1"), error: None }
    }

    // ---- reading aloud -----------------------------------------------------

    fn piece(text: &str, part: u32) -> Piece {
        Piece { text: text.to_string(), part }
    }

    #[tokio::test]
    async fn what_a_turn_asks_to_have_read_is_read_after_its_reply() {
        let mut rig =
            Rig::start(vec![short_reply("m1", "Here is the Morning Brew.")], &["read it to me"])
                .await;
        let story = karmayoga_core::StoryId::new();
        *rig.record.read_aloud.lock().unwrap() = vec![ReadAloud::Story(story)];
        rig.record.reading.lock().unwrap().push_back(Ok(vec![
            piece("Markets rose today. ", 0),
            piece("Oil fell.", 0),
            piece("In other news, it rained.", 1),
        ]));
        rig.talk();
        rig.play_out().await;
        assert_eq!(*rig.record.opened.lock().unwrap(), [ReadAloud::Story(story)]);
        // The reply was heard whole before the reading went on.
        let heard_whole = rig
            .seen
            .iter()
            .position(|o| {
                matches!(
                    o,
                    VoiceOutput::Event(VoiceEvent::ReplyHeard { message_id }) if message_id == "m1"
                )
            })
            .expect("the reply was heard whole");
        let plays: Vec<usize> = rig
            .seen
            .iter()
            .enumerate()
            .filter(|(_, o)| matches!(o, VoiceOutput::Play { .. }))
            .map(|(at, _)| at)
            .collect();
        assert!(heard_whole < plays[2], "before the second sentence of the reading played");
        assert_eq!(
            rig.said(),
            [
                "Here is the Morning Brew.",
                "Markets rose today.",
                "Oil fell.",
                "In other news, it rained."
            ]
        );
        // Nothing of it is the reply, so nothing of the reply is cut.
        assert!(rig.record.cuts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_reading_is_asked_for_only_a_little_ahead_and_stopped_by_talking() {
        let mut rig =
            Rig::start(vec![short_reply("m1", "Here it is.")], &["read it", "stop"]).await;
        *rig.record.read_aloud.lock().unwrap() =
            vec![ReadAloud::Story(karmayoga_core::StoryId::new())];
        let pieces: Vec<Piece> =
            (0..40).map(|i| piece(&format!("Sentence number {i} is here."), i)).collect();
        rig.record.reading.lock().unwrap().push_back(Ok(pieces));
        rig.talk();
        // The reply, then the first of the reading.
        let reply = rig.next_play().await;
        rig.send(VoiceInput::Played { utterance: reply });
        rig.next_play().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let pulled = rig.record.pulled.load(Ordering::SeqCst);
        assert!(pulled <= READ_AHEAD + 1, "only a little ahead: {pulled}");
        // Talking over it with "stop" ends it.
        rig.talk();
        rig.until("cleared", |o| matches!(o, VoiceOutput::Clear)).await;
        rig.until("listening", |o| is_state(o, VoiceState::Listening)).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(rig.record.pulled.load(Ordering::SeqCst), pulled, "and nothing more is read");
        assert!(rig.said().len() < 10, "{:?}", rig.said());
    }

    #[tokio::test]
    async fn a_reading_that_cannot_be_opened_says_why_and_the_next_is_read() {
        let mut rig = Rig::start(vec![short_reply("m1", "Here they are.")], &["read them"]).await;
        *rig.record.read_aloud.lock().unwrap() = vec![
            ReadAloud::Story(karmayoga_core::StoryId::new()),
            ReadAloud::Story(karmayoga_core::StoryId::new()),
            ReadAloud::Story(karmayoga_core::StoryId::new()),
        ];
        {
            let mut reading = rig.record.reading.lock().unwrap();
            reading.push_back(Err(CommandError::new(
                codes::INVALID,
                "There is nothing in this email to read.",
            )));
            reading.push_back(Err(CommandError::new(codes::NOT_FOUND, "story 0192f8b2 not found")));
            reading.push_back(Ok(vec![piece("The second one is here.", 0)]));
        }
        rig.talk();
        rig.play_out().await;
        assert_eq!(rig.record.opened.lock().unwrap().len(), 3, "each in turn");
        assert_eq!(
            rig.said(),
            [
                "Here they are.",
                "There is nothing in this email to read.",
                COULD_NOT_READ,
                "The second one is here."
            ],
            "a sentence meant for a person is said, a vault's error is not"
        );
    }

    #[tokio::test]
    async fn a_reading_that_fails_before_any_of_it_is_read_does_not_say_the_rest() {
        let mut rig = Rig::start(vec![short_reply("m1", "Here it is.")], &["read it"]).await;
        *rig.record.read_aloud.lock().unwrap() =
            vec![ReadAloud::Story(karmayoga_core::StoryId::new())];
        // What the model writes, gone wrong at once.
        *rig.record.fail_reading.lock().unwrap() = true;
        rig.talk();
        rig.play_out().await;
        assert_eq!(rig.said(), ["Here it is.", COULD_NOT_READ]);
    }

    // ---- whether what was said may approve what it asks -----------------------

    #[tokio::test]
    async fn words_begun_over_the_assistant_may_not_approve_what_they_ask() {
        let mut rig = Rig::start(
            vec![long_reply(), short_reply("m2", "Done.")],
            &["what is the weather", "delete the dentist task"],
        )
        .await;
        rig.talk();
        rig.next_play().await;
        // Said over the reply: it could be the reply coming back.
        rig.talk();
        eventually("the second turn", {
            let record = rig.record.clone();
            move || record.clear.lock().unwrap().len() == 2
        })
        .await;
        assert_eq!(*rig.record.clear.lock().unwrap(), [true, false]);
    }

    // ---- a delete asked for by voice ---------------------------------------

    #[tokio::test]
    async fn a_delete_still_asked_about_may_be_answered_aloud() {
        let mut rig = Rig::start(
            vec![confirmation_for("delete_task", "destructive", "Book the dentist")],
            &["what do I have on", "yes"],
        )
        .await;
        rig.talk();
        let question = rig.next_play().await;
        assert_eq!(rig.said(), ["Should I delete Book the dentist?"]);
        rig.send(VoiceInput::Played { utterance: question });
        rig.until("asking", |o| is_state(o, VoiceState::Asking)).await;
        tokio::time::sleep(ANSWER_GRACE + Duration::from_millis(50)).await;
        rig.talk();
        eventually("the answer", {
            let record = rig.record.clone();
            move || !record.answers.lock().unwrap().is_empty()
        })
        .await;
        assert_eq!(rig.answers(), [("c1".to_string(), ConfirmAnswer::Confirm)]);
    }
}
