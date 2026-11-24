//! Listening to something read aloud: an email, an article.
//!
//! One session per thing being listened to: text in, audio and events out.
//! Like [`crate::voice`] it knows nothing of where the audio goes. The desktop
//! app's speakers are the first transport (`karmayoga-app`'s `listen` module);
//! anything else that can play 16-bit mono audio and say when it has finished
//! is another.
//!
//! ```text
//!   Piece stream ─▶ SentenceSplitter ─▶ speakable ─▶ Narrator ─▶ ListenOutput::Play
//!   (model deltas,                                                   │
//!    or whole text)                     ListenInput::Played ◀────────┘
//! ```
//!
//! The text arrives as a stream of [`Piece`]s -- a model's reply as it is
//! written, or a whole article at once -- so the first sentence can be said
//! while the rest is still being written. Every sentence is kept, in order,
//! so the listener can go back and forward a sentence at a time and change
//! the speed without anything being written again.
//!
//! # The session
//!
//! A single Tokio task owns all the state, as in [`crate::voice`]'s. Whatever
//! can take time runs elsewhere and reports back on a channel: synthesis is a
//! [`Narrator`], and building a voice at a new speed is a task of its own.
//! Nothing the loop awaits can keep it from hearing [`ListenInput::Stop`].
//!
//! - **The text is asked for only as needed.** It is not read while twelve
//!   sentences are known beyond the one being listened to, because a model's
//!   reply is written as the stream is read: somebody who stops after a minute
//!   must not have paid for the whole of an article. Skipping forward past
//!   what is known reads on until it is.
//! - **A sentence ends at the end of its part**, so a paragraph's last words
//!   are never run into the next paragraph's first.
//! - **A skip plays**, whether or not it was paused: asking to go somewhere is
//!   asking to hear it. Back stops at the first sentence and forward at the
//!   last one, which is then said again.
//! - **A change of speed starts the sentence playing again**, in a new voice,
//!   once that is built; until then the old one carries on. Held playback stays
//!   held. If the voice cannot be built, the listener is told and nothing
//!   changes.
//! - **A sentence the voice cannot say is skipped**; three in a row end the
//!   session, since the voice is not working. A failure of the text is told at
//!   once, and ends the session when what had been written has been heard.
//! - **`Pause` and `Resume` are absolute**, not toggles. After a `Clear` that
//!   is not a skip (a change of speed) a `Pause` is sent again if the listener
//!   is still paused, so a transport is free to let a `Clear` let go of what it
//!   held.
//!
//! See `docs/plans/listen.md`.

use crate::error::CommandResult;
use crate::speech::narrate::{Narrated, Narrator, Synthesised};
use crate::speech::script;
use crate::speech::synth::Synthesiser;
use crate::speech::text::SentenceSplitter;
use crate::speech::transcribe::BoxFuture;
use futures::stream::{BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinSet;

/// A piece of the text to be read, in order. A model's reply arrives as many
/// pieces of one `part`; an article as one or more pieces per paragraph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub text: String,
    /// Which part of the source this belongs to: the paragraph of an
    /// article, counted from 0. Everything in one email briefing is part 0.
    /// A change of part ends the sentence before it.
    pub part: u32,
}

/// The text to be read, as it becomes available. An `Err` ends it: what was
/// already said stands, and the session reports the error and ends once the
/// rest has been played.
pub type PieceStream = BoxStream<'static, CommandResult<Piece>>;

/// What a session is doing, for the player on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ListenState {
    /// Nothing is ready to play yet: the text is being written, or the first
    /// sentence is being made into audio.
    Preparing,
    Playing,
    Paused,
}

/// What a session tells the interface. On the wire to the webview.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ListenEvent {
    State {
        state: ListenState,
    },
    /// Sentence `index` (counted from 0 over the whole text) has begun
    /// playing. `text` is what is being said; `part` is where it came from.
    Saying {
        index: u32,
        part: u32,
        text: String,
    },
    /// Something went wrong, and the session carries on: a sentence that
    /// could not be said is skipped.
    Trouble {
        message: String,
    },
    /// The session is over: everything was played, it was stopped, or it
    /// failed. `error` says why when it did not end on request or at the end.
    Ended {
        error: Option<String>,
    },
}

/// What a transport hands a session.
#[derive(Debug)]
pub enum ListenInput {
    /// Everything of [`ListenOutput::Play`]'s `utterance` has been played.
    Played {
        utterance: u64,
    },
    Pause,
    Resume,
    /// Go back (negative) or forward by this many sentences from the one
    /// playing, and carry on from there.
    Skip(i32),
    /// Speak at this multiple of the voice's own speed (`1.0` is as set in
    /// Settings), starting again from the sentence playing.
    Speed(f32),
    /// End the session.
    Stop,
}

/// What a session asks of a transport.
#[derive(Debug)]
pub enum ListenOutput {
    /// Queue this to be played after whatever is queued already.
    Play { utterance: u64, samples: Vec<i16>, sample_rate: u32 },
    /// Hold playback where it is.
    Pause,
    /// Carry on from where [`ListenOutput::Pause`] held it.
    Resume,
    /// Throw away everything queued, and anything held.
    Clear,
    /// Tell the interface.
    Event(ListenEvent),
}

/// Both ends of a running session, from the transport's side. The session
/// ends with [`ListenEvent::Ended`], after which `output` closes.
pub struct ListenLink {
    pub input: tokio::sync::mpsc::UnboundedSender<ListenInput>,
    pub output: tokio::sync::mpsc::UnboundedReceiver<ListenOutput>,
}

/// A voice at a given speed: how a session gets a new synthesiser when the
/// listener changes the speed. The argument is the multiple of the voice's
/// configured speed.
pub type VoiceAtSpeed =
    Arc<dyn Fn(f32) -> BoxFuture<'static, CommandResult<Arc<dyn Synthesiser>>> + Send + Sync>;

/// What a session is built from.
pub struct ListenParts {
    pub pieces: PieceStream,
    /// The voice at `speed`, already built, for the first sentence.
    pub synthesiser: Arc<dyn Synthesiser>,
    /// The speed `synthesiser` speaks at.
    pub speed: f32,
    /// For a change of speed.
    pub voice: VoiceAtSpeed,
}

/// Slowest and fastest a listener may ask for.
pub const MIN_SPEED: f32 = 0.5;
pub const MAX_SPEED: f32 = 2.5;

/// How many sentences may be made into audio and waiting to be played. One is
/// playing and one is ready behind it, so there is never a gap, and nothing
/// long is made before the listener has had the chance to stop it.
const MAX_AHEAD: usize = 2;

/// How many sentences are known beyond the one playing before the text stops
/// being asked for. A model's reply is written as the stream is read, so this
/// is what decides how much of a long article somebody who stops after a minute
/// has had written for them.
const LOOKAHEAD: usize = 12;

/// How many sentences in a row the voice may fail on before the session gives
/// up: one bad sentence is skipped, a voice that is not working is not.
const MAX_FAILURES: u32 = 3;

/// Said when the text ended without a sentence in it.
const NOTHING_TO_READ: &str = "There was nothing to read.";

/// Start a session on the current Tokio runtime.
///
/// The voice is already built, so the first sentence is being made as soon as
/// the text has one.
pub fn spawn(parts: ListenParts) -> ListenLink {
    let (input, input_rx) = unbounded_channel();
    let (out, output) = unbounded_channel();
    let (tx, internal) = unbounded_channel();
    let ListenParts { pieces, synthesiser, speed, voice } = parts;
    let session = Session {
        narrator: narrator_for(synthesiser, 0, &tx),
        out,
        tx,
        voice,
        epoch: 0,
        id_base: 0,
        says: 0,
        splitter: SentenceSplitter::new(),
        open_part: None,
        sentences: Vec::new(),
        stream_done: false,
        stream_error: None,
        current: 0,
        next_to_say: 0,
        pending_skip: None,
        playing: VecDeque::new(),
        paused: false,
        state: None,
        failures: 0,
        last_failure: String::new(),
        speed,
        wanted_speed: speed,
        speed_change: 0,
        working: JoinSet::new(),
    };
    tokio::spawn(run(session, input_rx, internal, Some(pieces)));
    ListenLink { input, output }
}

/// A narrator whose finished sentences come back through the session's loop,
/// marked with the `epoch` it belongs to. Each sentence's tag is its index in
/// the whole text.
///
/// Sentences are made one at a time, never said together as voice mode's
/// are ([`Narrator::joining`]): the reader shows the sentence being said and
/// skips by sentences, and in a clip of several it could only show the first
/// until the clip was over.
fn narrator_for(
    synthesiser: Arc<dyn Synthesiser>,
    epoch: u64,
    tx: &UnboundedSender<Internal>,
) -> Narrator<usize> {
    let finished = tx.clone();
    Narrator::new(
        synthesiser,
        MAX_AHEAD,
        Arc::new(move |done| {
            let _ = finished.send(Internal::Synthesised { epoch, done });
        }),
    )
}

/// What the tasks the session starts report back.
enum Internal {
    /// One sentence made into audio, for the narrator of `epoch`.
    Synthesised { epoch: u64, done: Synthesised },
    /// The voice at a new speed is built, or could not be. `change` says which
    /// request it answers.
    Voice { change: u64, speed: f32, result: CommandResult<Arc<dyn Synthesiser>> },
}

/// How the session came to its end.
enum End {
    /// Everything there was has been said.
    Done,
    /// It was stopped.
    Stopped,
    /// The voice could not say anything.
    Failed(String),
}

/// Why the loop woke.
enum Wake {
    Input(Option<ListenInput>),
    Internal(Option<Internal>),
    Piece(Option<CommandResult<Piece>>),
    OutputGone,
}

struct Session {
    /// Makes sentences into audio in order, a limited way ahead of playback,
    /// and remembers which of them the transport has yet to play.
    narrator: Narrator<usize>,
    out: UnboundedSender<ListenOutput>,
    /// What the tasks it starts report on.
    tx: UnboundedSender<Internal>,
    voice: VoiceAtSpeed,
    /// Which narrator results are for. A new narrator -- the voice changed
    /// speed -- numbers its sentences from nothing again, so a result of the
    /// old one must not be taken for one of the new.
    epoch: u64,
    /// Narrator ids are made unique across narrators by adding this, which is
    /// the number of sentences earlier narrators were given. The transport
    /// is told the sum, so a `Played` that was in flight when the voice
    /// changed cannot name a sentence of the new one.
    id_base: u64,
    /// Sentences the current narrator has been given.
    says: u64,

    splitter: SentenceSplitter,
    /// The part of the text the splitter is holding the rest of.
    open_part: Option<u32>,
    /// Every sentence there has been, in order, and the part of the text it
    /// came from. Kept for the session so going back and changing the speed
    /// never ask the text to be written again.
    sentences: Vec<(String, u32)>,
    /// The text has ended, one way or another.
    stream_done: bool,
    /// How it ended, if badly: said again when the rest has been played.
    stream_error: Option<String>,

    /// The sentence being listened to: the one playing, or when none is, the
    /// next to be.
    current: usize,
    /// The next sentence to hand to the narrator.
    next_to_say: usize,
    /// A skip forward to a sentence that is not known yet, waiting for it to be
    /// written.
    pending_skip: Option<usize>,
    /// Sentences handed to the transport and not yet played, with the id it
    /// was told and the sentence's index. The front is what is playing.
    playing: VecDeque<(u64, usize)>,
    paused: bool,
    /// What the interface was last told, so it is only told what changes.
    state: Option<ListenState>,
    /// Sentences in a row the voice has failed on, and the last reason.
    failures: u32,
    last_failure: String,

    /// The speed the narrator's voice speaks at.
    speed: f32,
    /// The speed last asked for, which may still be being built.
    wanted_speed: f32,
    /// Counts requests for a speed, so the answer to one that has been
    /// overtaken is known.
    speed_change: u64,
    /// Voices being built. Aborted with the session.
    working: JoinSet<()>,
}

async fn run(
    mut s: Session,
    mut input: UnboundedReceiver<ListenInput>,
    mut internal: UnboundedReceiver<Internal>,
    mut pieces: Option<PieceStream>,
) {
    // Kept apart from `s` so that the branch waiting on it borrows nothing
    // the handlers need.
    let gone = s.out.clone();
    s.sync();
    let end = loop {
        if let Some(end) = s.check_end() {
            break end;
        }
        // Only ask the text for more while there is not much of it ahead. A
        // stream that is not polled does no work, which is how somebody who
        // stops does not pay for the rest of the article.
        let want_more = pieces.is_some() && s.wants_more();
        let wake = tokio::select! {
            // Stop is read before anything else: nothing awaited here may keep
            // the loop from hearing it.
            biased;
            message = input.recv() => Wake::Input(message),
            message = internal.recv() => Wake::Internal(message),
            () = gone.closed() => Wake::OutputGone,
            piece = next_piece(&mut pieces), if want_more => Wake::Piece(piece),
        };
        match wake {
            Wake::Input(None | Some(ListenInput::Stop)) => break End::Stopped,
            // Nobody is listening any longer: nothing more to say to them.
            Wake::OutputGone => return,
            Wake::Internal(None) => break End::Stopped,
            Wake::Input(Some(message)) => s.on_input(message),
            Wake::Internal(Some(message)) => s.on_internal(message),
            Wake::Piece(Some(Ok(piece))) => s.on_piece(piece),
            Wake::Piece(Some(Err(e))) => {
                pieces = None;
                s.on_stream_error(e.message);
            }
            Wake::Piece(None) => {
                pieces = None;
                s.on_stream_end();
            }
        }
        s.sync();
    };
    s.finish(end);
}

async fn next_piece(pieces: &mut Option<PieceStream>) -> Option<CommandResult<Piece>> {
    match pieces {
        Some(pieces) => pieces.next().await,
        None => std::future::pending().await,
    }
}

impl Session {
    // ---- telling the transport ----------------------------------------------

    fn send(&self, output: ListenOutput) {
        // A transport that has gone is noticed by the loop, which is waiting
        // on `closed`.
        let _ = self.out.send(output);
    }

    fn emit(&self, event: ListenEvent) {
        self.send(ListenOutput::Event(event));
    }

    fn trouble(&self, message: impl Into<String>) {
        self.emit(ListenEvent::Trouble { message: message.into() });
    }

    /// Tell the interface what the session is doing, when that has changed.
    fn sync(&mut self) {
        let state = if self.paused {
            ListenState::Paused
        } else if self.playing.is_empty() {
            ListenState::Preparing
        } else {
            ListenState::Playing
        };
        if self.state != Some(state) {
            self.state = Some(state);
            self.emit(ListenEvent::State { state });
        }
    }

    /// Sentence `index` has begun playing.
    fn announce(&mut self, index: usize) {
        self.current = index;
        let (text, part) = self.sentences[index].clone();
        // Shown without the line's end the splitter marked for the voice.
        let text = script::shown(&text);
        self.emit(ListenEvent::Saying { index: index as u32, part, text });
    }

    // ---- the end -------------------------------------------------------------

    /// Whether there is nothing left to wait for.
    fn check_end(&mut self) -> Option<End> {
        if self.failures >= MAX_FAILURES {
            let why = std::mem::take(&mut self.last_failure);
            return Some(End::Failed(format!(
                "The voice failed on three sentences in a row: {why}"
            )));
        }
        let all_said = self.stream_done
            && self.pending_skip.is_none()
            && self.next_to_say >= self.sentences.len()
            && self.playing.is_empty()
            && !self.narrator.outstanding();
        all_said.then_some(End::Done)
    }

    fn finish(&mut self, end: End) {
        let error = match end {
            End::Done if self.sentences.is_empty() => {
                Some(self.stream_error.take().unwrap_or_else(|| NOTHING_TO_READ.to_string()))
            }
            End::Done => self.stream_error.take(),
            End::Stopped => {
                self.silence();
                None
            }
            End::Failed(message) => {
                self.silence();
                Some(message)
            }
        };
        self.emit(ListenEvent::Ended { error });
    }

    /// Throw away everything queued, being made, or held by the transport.
    fn silence(&mut self) {
        self.narrator.clear();
        self.playing.clear();
        self.send(ListenOutput::Clear);
        self.working.abort_all();
    }

    // ---- the text --------------------------------------------------------------

    /// Whether the text should be asked for more now: not while there are
    /// plenty of sentences ahead of the one being listened to, unless a skip is
    /// waiting on one that has not been written.
    fn wants_more(&self) -> bool {
        !self.stream_done
            && (self.pending_skip.is_some() || self.sentences.len() < self.current + 1 + LOOKAHEAD)
    }

    fn on_piece(&mut self, piece: Piece) {
        // A new part ends the sentence before it, so a paragraph's last words
        // are not run into the next one's first.
        if self.open_part.is_some_and(|part| part != piece.part) {
            self.flush();
        }
        self.open_part = Some(piece.part);
        // The splitter's sentences are already made speakable. Said as
        // written: what is read aloud is somebody else's writing, or a model's
        // account of it, and has no say in how a word sounds (see `script`).
        for sentence in self.splitter.push(&script::inert(&piece.text)) {
            self.add(sentence, piece.part);
        }
        self.know_more();
    }

    fn on_stream_error(&mut self, message: String) {
        self.trouble(message.clone());
        self.stream_error = Some(message);
        self.on_stream_end();
    }

    fn on_stream_end(&mut self) {
        self.flush();
        self.stream_done = true;
        self.know_more();
    }

    /// The sentence the splitter is holding the end of is complete.
    ///
    /// The splitter keeps all it is given and looks at it from the start on
    /// every push, so each part gets a fresh one: an article costs one
    /// paragraph's worth at a time, not the whole article again per delta.
    fn flush(&mut self) {
        if let Some(part) = self.open_part
            && let Some(tail) = self.splitter.finish()
        {
            self.add(tail, part);
        }
        self.splitter = SentenceSplitter::new();
    }

    fn add(&mut self, sentence: String, part: u32) {
        let text = sentence.trim();
        if !text.is_empty() {
            self.sentences.push((text.to_string(), part));
        }
    }

    /// There are more sentences than there were, or no more are coming.
    fn know_more(&mut self) {
        self.resolve_skip();
        self.feed();
    }

    /// Hand the narrator every sentence it has not had, in order. It makes them
    /// only as far ahead as it is allowed to.
    fn feed(&mut self) {
        if self.pending_skip.is_some() {
            return;
        }
        while self.next_to_say < self.sentences.len() {
            let index = self.next_to_say;
            self.narrator.say(self.sentences[index].0.clone(), index);
            self.says += 1;
            self.next_to_say += 1;
        }
    }

    // ---- what the transport tells us ---------------------------------------------

    fn on_input(&mut self, input: ListenInput) {
        match input {
            ListenInput::Played { utterance } => self.on_played(utterance),
            ListenInput::Pause => {
                if !self.paused {
                    self.paused = true;
                    self.send(ListenOutput::Pause);
                }
            }
            ListenInput::Resume => {
                if self.paused {
                    self.paused = false;
                    self.send(ListenOutput::Resume);
                }
            }
            ListenInput::Skip(by) => self.skip(by),
            ListenInput::Speed(speed) => self.change_speed(speed),
            // Handled by the loop, which must not leave it to us.
            ListenInput::Stop => {}
        }
    }

    /// The transport says `utterance` has played, and so has everything before
    /// it.
    fn on_played(&mut self, utterance: u64) {
        // Made for a narrator since replaced: it names nothing.
        let Some(id) = utterance.checked_sub(self.id_base) else { return };
        let played = self.narrator.played(id);
        // Cleared since, or already reported: nothing has changed.
        if played.is_empty() {
            return;
        }
        for sentence in played {
            self.playing.pop_front();
            self.current = sentence.tag + 1;
        }
        if let Some(&(_, index)) = self.playing.front() {
            self.announce(index);
        }
    }

    fn on_internal(&mut self, message: Internal) {
        match message {
            Internal::Synthesised { epoch, done } => self.on_synthesised(epoch, done),
            Internal::Voice { change, speed, result } => self.on_voice(change, speed, result),
        }
    }

    // ---- sentences made into audio -------------------------------------------------

    fn on_synthesised(&mut self, epoch: u64, done: Synthesised) {
        // For a narrator since replaced.
        if epoch != self.epoch {
            return;
        }
        match self.narrator.finished(done) {
            Narrated::Ready { sentence, speech } => {
                self.failures = 0;
                let utterance = self.id_base + sentence.id;
                self.playing.push_back((utterance, sentence.tag));
                self.send(ListenOutput::Play {
                    utterance,
                    samples: speech.samples,
                    sample_rate: speech.sample_rate,
                });
                // Nothing else was outstanding, so this is what is said now.
                if self.playing.len() == 1 {
                    self.sync();
                    self.announce(sentence.tag);
                }
            }
            // A sentence that cannot be said is skipped.
            Narrated::Failed { sentence, error } => {
                self.failures += 1;
                self.last_failure = match error {
                    Some(e) => e.message,
                    None => "the voice made no sound".to_string(),
                };
                self.trouble(format!("A sentence could not be said: {}", self.last_failure));
                // Never going to be played: the listener is on the next.
                if self.playing.is_empty() && self.current == sentence.tag {
                    self.current += 1;
                }
            }
            Narrated::Stale => {}
        }
    }

    // ---- going back and forward -------------------------------------------------------

    /// Move `by` sentences from the one being listened to and carry on from
    /// there. A skip while paused plays: asking to go somewhere is asking to
    /// hear it.
    ///
    /// Back stops at the first sentence. Forward stops at the last there is,
    /// which, when it is not known yet, is waited for: the text is asked for
    /// more until it has that many or ends.
    fn skip(&mut self, by: i32) {
        let from = self.pending_skip.unwrap_or(self.current) as i64;
        self.pending_skip = Some((from + i64::from(by)).max(0) as usize);
        self.stop_playing();
        if self.paused {
            self.paused = false;
            self.send(ListenOutput::Resume);
        }
        self.know_more();
    }

    /// Settle a skip as far as the sentences known allow.
    fn resolve_skip(&mut self) {
        let Some(target) = self.pending_skip else { return };
        let at = if target < self.sentences.len() {
            target
        } else if self.stream_done {
            // Nothing beyond the last sentence: it is said again.
            match self.sentences.len().checked_sub(1) {
                Some(last) => last,
                None => {
                    self.pending_skip = None;
                    return;
                }
            }
        } else {
            return;
        };
        self.pending_skip = None;
        self.current = at;
        self.next_to_say = at;
    }

    /// Throw away everything queued, being made, or held, without ending
    /// anything: the sentences are made again from where the listener is.
    fn stop_playing(&mut self) {
        self.narrator.clear();
        self.playing.clear();
        self.send(ListenOutput::Clear);
    }

    // ---- speed ---------------------------------------------------------------------------

    /// Ask for the voice at `speed`. Building it can take seconds, so it is
    /// done elsewhere and heard about when it is ready: what is playing goes
    /// on at the old speed until then.
    fn change_speed(&mut self, speed: f32) {
        if !speed.is_finite() {
            return;
        }
        let speed = speed.clamp(MIN_SPEED, MAX_SPEED);
        if speed == self.wanted_speed {
            return;
        }
        self.wanted_speed = speed;
        self.speed_change += 1;
        // Back to the speed it is at: whatever was being built is no longer
        // wanted, and nothing needs building.
        if speed == self.speed {
            return;
        }
        let (change, voice, tx) = (self.speed_change, self.voice.clone(), self.tx.clone());
        self.working.spawn(async move {
            let result = voice(speed).await;
            let _ = tx.send(Internal::Voice { change, speed, result });
        });
    }

    fn on_voice(&mut self, change: u64, speed: f32, result: CommandResult<Arc<dyn Synthesiser>>) {
        // Overtaken by a later request, which has its own answer coming.
        if change != self.speed_change {
            return;
        }
        match result {
            Ok(synthesiser) => {
                self.speed = speed;
                self.restart_with(synthesiser);
            }
            Err(e) => {
                self.wanted_speed = self.speed;
                self.trouble(e.message);
            }
        }
    }

    /// Say the sentence being listened to again, and the ones after it, in
    /// `synthesiser`'s voice. Held playback stays held.
    fn restart_with(&mut self, synthesiser: Arc<dyn Synthesiser>) {
        // The old narrator's sentences in flight are aborted by the clear, and
        // any that had finished are for an epoch that is no longer this one.
        self.narrator.clear();
        self.epoch += 1;
        self.id_base += self.says;
        self.says = 0;
        self.narrator = narrator_for(synthesiser, self.epoch, &self.tx);
        self.playing.clear();
        self.send(ListenOutput::Clear);
        // Clear may let go of held playback, and the listener has not resumed.
        if self.paused {
            self.send(ListenOutput::Pause);
        }
        self.failures = 0;
        if self.pending_skip.is_none() {
            self.next_to_say = self.current;
        }
        self.feed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{CommandError, codes};
    use crate::speech::synth::Speech;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Notify;

    // ---- the fakes ---------------------------------------------------------

    /// A voice at a speed. A sentence with `FAIL` in it cannot be said, and
    /// any other is two samples: the speed in tenths, and the sentence's length.
    struct Voice {
        speed: f32,
    }

    impl Synthesiser for Voice {
        fn synthesise<'a>(&'a self, text: &'a str) -> BoxFuture<'a, CommandResult<Speech>> {
            Box::pin(async move {
                if text.contains("FAIL") {
                    return Err(CommandError::new(codes::NETWORK, "no voice today"));
                }
                Ok(Speech {
                    samples: vec![(self.speed * 10.0).round() as i16, text.len() as i16],
                    sample_rate: 16_000,
                })
            })
        }
    }

    /// How the voice is asked for at a speed: every speed asked for is
    /// recorded, and the answer is up to the test.
    /// A speed held back from being built, and what releases it.
    type Held = Arc<Mutex<Vec<(f32, Arc<Notify>)>>>;

    #[derive(Clone, Default)]
    struct Voices {
        asked: Arc<Mutex<Vec<f32>>>,
        /// Speeds that cannot be built.
        broken: Arc<Mutex<Vec<f32>>>,
        /// Speeds that are not built until released.
        held: Held,
    }

    impl Voices {
        fn at_speed(&self) -> VoiceAtSpeed {
            let me = self.clone();
            Arc::new(move |speed| {
                let me = me.clone();
                Box::pin(async move {
                    me.asked.lock().unwrap().push(speed);
                    let gate = me.held.lock().unwrap().iter().find(|(s, _)| *s == speed).cloned();
                    if let Some((_, gate)) = gate {
                        gate.notified().await;
                    }
                    if me.broken.lock().unwrap().contains(&speed) {
                        return Err(CommandError::new(codes::NETWORK, "the voice would not load"));
                    }
                    Ok(Arc::new(Voice { speed }) as Arc<dyn Synthesiser>)
                })
            })
        }

        fn asked(&self) -> Vec<f32> {
            self.asked.lock().unwrap().clone()
        }
    }

    fn sentence(i: usize) -> Piece {
        Piece { text: format!("Sentence number {i} is read here. "), part: 0 }
    }

    fn sentences(n: usize) -> Vec<CommandResult<Piece>> {
        (0..n).map(|i| Ok(sentence(i))).collect()
    }

    /// What the session shows a transport and the interface, as it arrives.
    #[derive(Debug)]
    enum Seen {
        Play { utterance: u64, samples: Vec<i16> },
        Pause,
        Resume,
        Clear,
        Event(ListenEvent),
    }

    struct Rig {
        link: ListenLink,
        voices: Voices,
        closed: bool,
    }

    fn rig_with(pieces: PieceStream) -> Rig {
        let voices = Voices::default();
        let link = spawn(ListenParts {
            pieces,
            synthesiser: Arc::new(Voice { speed: 1.0 }),
            speed: 1.0,
            voice: voices.at_speed(),
        });
        Rig { link, voices, closed: false }
    }

    fn rig(pieces: Vec<CommandResult<Piece>>) -> Rig {
        rig_with(futures::stream::iter(pieces).boxed())
    }

    /// The text goes on being written for ever, and nothing ends it.
    fn rig_endless(pieces: Vec<CommandResult<Piece>>) -> Rig {
        rig_with(futures::stream::iter(pieces).chain(futures::stream::pending()).boxed())
    }

    impl Rig {
        /// Long enough for everything that was going to happen to have; what
        /// arrived is kept, and what has arrived since the last call is
        /// returned.
        async fn settle(&mut self) -> Vec<Seen> {
            let mut got = Vec::new();
            loop {
                match tokio::time::timeout(Duration::from_millis(60), self.link.output.recv()).await
                {
                    Ok(Some(output)) => got.push(match output {
                        ListenOutput::Play { utterance, samples, .. } => {
                            Seen::Play { utterance, samples }
                        }
                        ListenOutput::Pause => Seen::Pause,
                        ListenOutput::Resume => Seen::Resume,
                        ListenOutput::Clear => Seen::Clear,
                        ListenOutput::Event(event) => Seen::Event(event),
                    }),
                    Ok(None) => {
                        self.closed = true;
                        break;
                    }
                    Err(_) => break,
                }
            }
            got
        }

        fn send(&self, input: ListenInput) {
            let _ = self.link.input.send(input);
        }

        fn played(&self, utterance: u64) {
            self.send(ListenInput::Played { utterance });
        }
    }

    fn plays(seen: &[Seen]) -> Vec<u64> {
        seen.iter()
            .filter_map(|s| match s {
                Seen::Play { utterance, .. } => Some(*utterance),
                _ => None,
            })
            .collect()
    }

    fn sayings(seen: &[Seen]) -> Vec<usize> {
        seen.iter()
            .filter_map(|s| match s {
                Seen::Event(ListenEvent::Saying { index, .. }) => Some(*index as usize),
                _ => None,
            })
            .collect()
    }

    fn states(seen: &[Seen]) -> Vec<ListenState> {
        seen.iter()
            .filter_map(|s| match s {
                Seen::Event(ListenEvent::State { state }) => Some(*state),
                _ => None,
            })
            .collect()
    }

    fn troubles(seen: &[Seen]) -> Vec<String> {
        seen.iter()
            .filter_map(|s| match s {
                Seen::Event(ListenEvent::Trouble { message }) => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    fn ended(seen: &[Seen]) -> Option<Option<String>> {
        seen.iter().find_map(|s| match s {
            Seen::Event(ListenEvent::Ended { error }) => Some(error.clone()),
            _ => None,
        })
    }

    fn cleared(seen: &[Seen]) -> bool {
        seen.iter().any(|s| matches!(s, Seen::Clear))
    }

    // ---- playing ---------------------------------------------------------------

    #[tokio::test]
    async fn sentences_play_in_order_and_each_is_announced_as_it_begins() {
        let mut rig = rig(sentences(3));
        let seen = rig.settle().await;
        // Two are made ahead of the one that is playing, and no more; only
        // the first is being said.
        assert_eq!(plays(&seen), vec![0, 1]);
        assert_eq!(sayings(&seen), vec![0]);
        assert_eq!(states(&seen), vec![ListenState::Preparing, ListenState::Playing]);
        assert!(matches!(
            seen.iter().find(|s| matches!(s, Seen::Event(ListenEvent::Saying { .. }))),
            Some(Seen::Event(ListenEvent::Saying { index: 0, part: 0, text }))
                if text == "Sentence number 0 is read here."
        ));

        rig.played(0);
        let seen = rig.settle().await;
        assert_eq!(sayings(&seen), vec![1]);
        assert_eq!(plays(&seen), vec![2], "room for the third");

        rig.played(1);
        assert_eq!(sayings(&rig.settle().await), vec![2]);

        rig.played(2);
        let seen = rig.settle().await;
        assert_eq!(ended(&seen), Some(None));
        assert!(rig.closed, "the output closes after Ended");
    }

    #[tokio::test]
    async fn the_text_is_announced_as_played_when_a_later_id_says_the_earlier_ones_are_too() {
        let mut rig = rig(sentences(4));
        rig.settle().await;
        // The transport reports the second, and so the first.
        rig.played(1);
        let seen = rig.settle().await;
        assert_eq!(sayings(&seen), vec![2]);
    }

    #[tokio::test]
    async fn a_change_of_part_ends_the_sentence_before_it_and_each_sentence_keeps_its_part() {
        let mut rig = rig(vec![
            Ok(Piece { text: "A heading".into(), part: 0 }),
            Ok(Piece { text: "The first paragraph says this. And that".into(), part: 1 }),
            Ok(Piece { text: " as well.".into(), part: 1 }),
            Ok(Piece { text: "A last short one".into(), part: 2 }),
        ]);
        let seen = rig.settle().await;
        assert_eq!(plays(&seen), vec![0, 1]);
        let mut said = Vec::new();
        let mut all = seen;
        for id in 0..4 {
            rig.played(id);
            all.extend(rig.settle().await);
        }
        for s in &all {
            if let Seen::Event(ListenEvent::Saying { index, part, text }) = s {
                said.push((*index, *part, text.clone()));
            }
        }
        assert_eq!(
            said,
            vec![
                (0, 0, "A heading".to_string()),
                (1, 1, "The first paragraph says this.".to_string()),
                (2, 1, "And that as well.".to_string()),
                (3, 2, "A last short one".to_string()),
            ]
        );
        assert_eq!(ended(&all), Some(None));
    }

    #[tokio::test]
    async fn the_text_is_not_asked_for_more_than_is_needed_ahead_of_the_listener() {
        let pulled = Arc::new(AtomicUsize::new(0));
        let counter = pulled.clone();
        let text = futures::stream::iter(0..200).map(move |i| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(sentence(i))
        });
        let mut rig = rig_with(text.boxed());
        rig.settle().await;
        // The one being listened to, and twelve beyond it.
        assert_eq!(pulled.load(Ordering::SeqCst), 1 + LOOKAHEAD);

        // Listening on lets it ask for as much as the listener has used.
        rig.played(0);
        rig.settle().await;
        assert_eq!(pulled.load(Ordering::SeqCst), 2 + LOOKAHEAD);
        rig.played(1);
        rig.played(2);
        rig.settle().await;
        assert_eq!(pulled.load(Ordering::SeqCst), 4 + LOOKAHEAD);

        rig.send(ListenInput::Stop);
        rig.settle().await;
        assert_eq!(pulled.load(Ordering::SeqCst), 4 + LOOKAHEAD, "nothing after stopping");
    }

    #[tokio::test]
    async fn the_first_sentence_plays_while_the_rest_is_still_being_written() {
        let mut rig = rig_endless(vec![Ok(sentence(0))]);
        let seen = rig.settle().await;
        assert_eq!(plays(&seen), vec![0]);
        assert_eq!(sayings(&seen), vec![0]);
        // Not over: more may come.
        rig.played(0);
        let seen = rig.settle().await;
        assert_eq!(ended(&seen), None);
        assert_eq!(states(&seen), vec![ListenState::Preparing], "waiting for the next");
    }

    // ---- pause and resume ---------------------------------------------------------

    #[tokio::test]
    async fn pausing_and_resuming_hold_and_release_playback_and_say_so() {
        let mut rig = rig(sentences(3));
        rig.settle().await;
        rig.send(ListenInput::Pause);
        rig.send(ListenInput::Pause);
        let seen = rig.settle().await;
        assert!(
            matches!(
                seen.as_slice(),
                [Seen::Pause, Seen::Event(ListenEvent::State { state: ListenState::Paused })]
            ),
            "{seen:?}"
        );

        // Making sentences ahead may go on while paused.
        rig.played(0);
        let seen = rig.settle().await;
        assert_eq!(plays(&seen), vec![2]);

        rig.send(ListenInput::Resume);
        rig.send(ListenInput::Resume);
        let seen = rig.settle().await;
        assert!(
            matches!(
                seen.as_slice(),
                [Seen::Resume, Seen::Event(ListenEvent::State { state: ListenState::Playing })]
            ),
            "{seen:?}"
        );
    }

    // ---- going back and forward ------------------------------------------------------

    #[tokio::test]
    async fn skipping_forward_clears_and_plays_from_the_sentence_that_far_ahead() {
        let mut rig = rig(sentences(8));
        rig.settle().await;
        rig.played(0);
        rig.settle().await; // listening to 1
        rig.send(ListenInput::Skip(3));
        let seen = rig.settle().await;
        assert!(cleared(&seen));
        assert_eq!(sayings(&seen), vec![4]);
        assert_eq!(plays(&seen).len(), 2, "four, and one made ahead");
        // The ids carry on: nothing from before can be taken for these.
        assert!(plays(&seen).iter().all(|id| *id >= 3), "{seen:?}");

        // The old audio being reported as played changes nothing.
        rig.played(1);
        let seen = rig.settle().await;
        assert!(sayings(&seen).is_empty(), "{seen:?}");
    }

    #[tokio::test]
    async fn skipping_back_goes_over_sentences_already_said_without_writing_them_again() {
        let mut rig = rig(sentences(6));
        rig.settle().await;
        for id in 0..3 {
            rig.played(id);
            rig.settle().await;
        }
        // Listening to 3.
        rig.send(ListenInput::Skip(-2));
        let seen = rig.settle().await;
        assert!(cleared(&seen));
        assert_eq!(sayings(&seen), vec![1]);
        let first_play = plays(&seen)[0];
        rig.played(first_play);
        assert_eq!(sayings(&rig.settle().await), vec![2]);
    }

    #[tokio::test]
    async fn skipping_back_stops_at_the_first_sentence_and_forward_at_the_last() {
        let mut rig = rig(sentences(3));
        rig.settle().await;
        rig.send(ListenInput::Skip(-5));
        assert_eq!(sayings(&rig.settle().await), vec![0]);
        rig.send(ListenInput::Skip(50));
        assert_eq!(sayings(&rig.settle().await), vec![2]);
    }

    #[tokio::test]
    async fn skipping_forward_past_what_is_known_asks_the_text_for_more() {
        let pulled = Arc::new(AtomicUsize::new(0));
        let counter = pulled.clone();
        let text = futures::stream::iter(0..200).map(move |i| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(sentence(i))
        });
        let mut rig = rig_with(text.boxed());
        rig.settle().await;
        assert_eq!(pulled.load(Ordering::SeqCst), 1 + LOOKAHEAD);

        rig.send(ListenInput::Skip(40));
        let seen = rig.settle().await;
        assert_eq!(sayings(&seen), vec![40]);
        // Far enough to know it, and the usual way beyond: not all of it.
        assert_eq!(pulled.load(Ordering::SeqCst), 41 + LOOKAHEAD);
    }

    #[tokio::test]
    async fn a_skip_while_paused_plays() {
        let mut rig = rig(sentences(5));
        rig.settle().await;
        rig.send(ListenInput::Pause);
        rig.settle().await;
        rig.send(ListenInput::Skip(2));
        let seen = rig.settle().await;
        assert!(matches!(&seen[..2], [Seen::Clear, Seen::Resume]), "{seen:?}");
        assert_eq!(sayings(&seen), vec![2]);
        assert_eq!(states(&seen).last(), Some(&ListenState::Playing));
    }

    // ---- speed ---------------------------------------------------------------------------

    #[tokio::test]
    async fn a_change_of_speed_starts_the_sentence_playing_again_in_the_new_voice() {
        let mut rig = rig(sentences(5));
        let seen = rig.settle().await;
        assert!(
            matches!(&seen.iter().find(|s| matches!(s, Seen::Play { .. })), Some(Seen::Play { samples, .. }) if samples[0] == 10)
        );
        rig.played(0);
        rig.settle().await; // listening to 1
        rig.send(ListenInput::Speed(1.5));
        let seen = rig.settle().await;
        assert_eq!(rig.voices.asked(), vec![1.5]);
        assert!(cleared(&seen));
        assert_eq!(sayings(&seen), vec![1], "from the sentence that was playing");
        for s in &seen {
            if let Seen::Play { samples, .. } = s {
                assert_eq!(samples[0], 15, "made at the new speed");
            }
        }
        assert_eq!(plays(&seen).len(), 2);

        // And it carries on from there.
        rig.played(plays(&seen)[0]);
        assert_eq!(sayings(&rig.settle().await), vec![2]);
    }

    #[tokio::test]
    async fn a_speed_is_kept_to_what_is_allowed() {
        let mut rig = rig(sentences(3));
        rig.settle().await;
        rig.send(ListenInput::Speed(9.0));
        rig.settle().await;
        rig.send(ListenInput::Speed(0.01));
        rig.settle().await;
        rig.send(ListenInput::Speed(f32::NAN));
        rig.settle().await;
        assert_eq!(rig.voices.asked(), vec![MAX_SPEED, MIN_SPEED]);
    }

    #[tokio::test]
    async fn a_voice_that_cannot_be_built_is_reported_and_the_old_one_carries_on() {
        let mut rig = rig(sentences(4));
        rig.voices.broken.lock().unwrap().push(2.0);
        rig.settle().await;
        rig.send(ListenInput::Speed(2.0));
        let seen = rig.settle().await;
        assert_eq!(troubles(&seen), vec!["the voice would not load".to_string()]);
        assert!(!cleared(&seen), "nothing was interrupted");
        assert!(plays(&seen).is_empty());

        // Still at the old speed, and still going.
        rig.played(0);
        let seen = rig.settle().await;
        assert_eq!(sayings(&seen), vec![1]);
        assert!(
            matches!(seen.iter().find(|s| matches!(s, Seen::Play { .. })), Some(Seen::Play { samples, .. }) if samples[0] == 10)
        );

        // Asking for it again is not taken as already asked.
        rig.send(ListenInput::Speed(2.0));
        rig.settle().await;
        assert_eq!(rig.voices.asked(), vec![2.0, 2.0]);
    }

    #[tokio::test]
    async fn an_answer_to_a_speed_asked_for_before_another_is_ignored() {
        let mut rig = rig(sentences(4));
        let gate = Arc::new(Notify::new());
        rig.voices.held.lock().unwrap().push((2.0, gate.clone()));
        rig.settle().await;
        rig.send(ListenInput::Speed(2.0));
        rig.send(ListenInput::Speed(1.25));
        let seen = rig.settle().await;
        // The second was built first, and is the one in use.
        assert_eq!(rig.voices.asked(), vec![2.0, 1.25]);
        assert!(cleared(&seen));
        for s in &seen {
            if let Seen::Play { samples, .. } = s {
                assert_eq!(samples[0], 13); // 12.5 rounds to 13
            }
        }
        // The first finishes late, and is of no interest.
        gate.notify_one();
        let seen = rig.settle().await;
        assert!(seen.is_empty(), "{seen:?}");
    }

    #[tokio::test]
    async fn asking_for_the_speed_it_is_at_again_is_not_a_restart() {
        let mut rig = rig(sentences(3));
        rig.settle().await;
        rig.send(ListenInput::Speed(1.0));
        let seen = rig.settle().await;
        assert!(seen.is_empty());
        assert!(rig.voices.asked().is_empty());
    }

    #[tokio::test]
    async fn changing_speed_while_paused_stays_paused() {
        let mut rig = rig(sentences(3));
        rig.settle().await;
        rig.send(ListenInput::Pause);
        rig.settle().await;
        rig.send(ListenInput::Speed(1.5));
        let seen = rig.settle().await;
        assert!(matches!(&seen[..2], [Seen::Clear, Seen::Pause]), "{seen:?}");
        assert_eq!(states(&seen).last(), None, "still paused, nothing to tell");
    }

    #[tokio::test]
    async fn audio_reported_played_from_before_a_change_of_speed_names_nothing_afterwards() {
        let mut rig = rig(sentences(4));
        rig.settle().await;
        rig.send(ListenInput::Speed(2.0));
        let seen = rig.settle().await;
        // The first sentence's id at the new speed is not 0 again.
        assert!(plays(&seen).iter().all(|id| *id >= 3), "{seen:?}");
        rig.played(0);
        rig.played(1);
        assert!(rig.settle().await.is_empty());
    }

    // ---- going wrong ----------------------------------------------------------------------------

    #[tokio::test]
    async fn a_failed_sentence_is_skipped_and_the_next_one_plays() {
        let mut rig = rig(vec![
            Ok(Piece { text: "Sentence one is fine to say. ".into(), part: 0 }),
            Ok(Piece { text: "Sentence two will FAIL to say. ".into(), part: 0 }),
            Ok(Piece { text: "Sentence three is fine to say. ".into(), part: 0 }),
        ]);
        let seen = rig.settle().await;
        assert_eq!(troubles(&seen).len(), 1);
        assert!(troubles(&seen)[0].contains("no voice today"));
        assert_eq!(plays(&seen), vec![0, 2]);
        assert_eq!(sayings(&seen), vec![0]);

        rig.played(0);
        assert_eq!(sayings(&rig.settle().await), vec![2], "the sentence after the failed one");
        rig.played(2);
        assert_eq!(ended(&rig.settle().await), Some(None));
    }

    #[tokio::test]
    async fn a_failed_sentence_at_the_start_leaves_the_listener_on_the_next() {
        let mut rig = rig(vec![
            Ok(Piece { text: "Sentence one will FAIL to say. ".into(), part: 0 }),
            Ok(Piece { text: "Sentence two is fine to say. ".into(), part: 0 }),
        ]);
        let seen = rig.settle().await;
        assert_eq!(sayings(&seen), vec![1]);
        rig.send(ListenInput::Skip(-1));
        // Back from the second is the first, which cannot be said again: it
        // fails once more, and the second is what plays.
        let seen = rig.settle().await;
        assert_eq!(sayings(&seen), vec![1]);
    }

    #[tokio::test]
    async fn three_failures_in_a_row_end_the_session_with_an_error() {
        let fail =
            |i: usize| Ok(Piece { text: format!("Sentence {i} will FAIL to say. "), part: 0 });
        let mut rig = rig(vec![fail(1), fail(2), fail(3), Ok(sentence(4))]);
        let seen = rig.settle().await;
        assert_eq!(troubles(&seen).len(), 3);
        assert!(cleared(&seen));
        let error = ended(&seen).expect("it ended").expect("with an error");
        assert!(error.contains("three sentences in a row"), "{error}");
        assert!(error.contains("no voice today"), "{error}");
        assert!(plays(&seen).is_empty());
        assert!(rig.closed);
    }

    #[tokio::test]
    async fn a_success_between_failures_starts_the_count_again() {
        let fail =
            |i: usize| Ok(Piece { text: format!("Sentence {i} will FAIL to say. "), part: 0 });
        let mut rig =
            rig(vec![fail(1), fail(2), Ok(sentence(3)), fail(4), fail(5), Ok(sentence(6))]);
        // Four failures, but never three in a row, so it goes on.
        let seen = rig.settle().await;
        assert_eq!(troubles(&seen).len(), 4, "{seen:?}");
        assert_eq!(plays(&seen), vec![2, 5]);
        assert_eq!(ended(&seen), None);
        rig.played(2);
        rig.played(5);
        assert_eq!(ended(&rig.settle().await), Some(None));
    }

    #[tokio::test]
    async fn a_failure_of_the_text_is_reported_and_what_is_left_is_played_before_the_end() {
        let mut rig = rig(vec![
            Ok(sentence(0)),
            Ok(Piece { text: "A half-written sentence that is left".into(), part: 0 }),
            Err(CommandError::new(codes::QUICK, "the model went away")),
        ]);
        let seen = rig.settle().await;
        assert_eq!(troubles(&seen), vec!["the model went away".to_string()]);
        assert_eq!(ended(&seen), None, "there is more to say first");
        assert_eq!(plays(&seen), vec![0, 1], "even the unfinished sentence is said");
        rig.played(0);
        rig.settle().await;
        rig.played(1);
        let seen = rig.settle().await;
        assert_eq!(ended(&seen), Some(Some("the model went away".to_string())));
        assert!(rig.closed);
    }

    #[tokio::test]
    async fn text_with_nothing_in_it_is_an_error_and_not_silence() {
        let mut rig = rig(Vec::new());
        let seen = rig.settle().await;
        assert_eq!(ended(&seen), Some(Some("There was nothing to read.".to_string())));
        assert!(plays(&seen).is_empty());
        assert!(rig.closed);

        let mut rig = self::rig(vec![Ok(Piece { text: "  \n ".into(), part: 0 })]);
        let seen = rig.settle().await;
        assert_eq!(ended(&seen), Some(Some("There was nothing to read.".to_string())));
    }

    #[tokio::test]
    async fn text_that_fails_before_saying_anything_ends_with_that_failure() {
        let mut rig = rig(vec![Err(CommandError::new(codes::QUICK, "no key"))]);
        let seen = rig.settle().await;
        assert_eq!(troubles(&seen), vec!["no key".to_string()]);
        assert_eq!(ended(&seen), Some(Some("no key".to_string())));
    }

    // ---- stopping --------------------------------------------------------------------------------

    #[tokio::test]
    async fn stop_clears_what_is_queued_and_ends_without_an_error() {
        let mut rig = rig_endless(sentences(3));
        rig.settle().await;
        rig.send(ListenInput::Stop);
        let seen = rig.settle().await;
        assert!(
            matches!(
                seen.as_slice(),
                [Seen::Clear, Seen::Event(ListenEvent::Ended { error: None })]
            ),
            "{seen:?}"
        );
        assert!(rig.closed);
    }

    #[tokio::test]
    async fn stop_is_heard_while_the_text_is_not_coming() {
        // Nothing at all ever arrives from the text.
        let mut rig = rig_with(futures::stream::pending().boxed());
        rig.settle().await;
        rig.send(ListenInput::Stop);
        let seen = rig.settle().await;
        assert_eq!(ended(&seen), Some(None));
        assert!(rig.closed);
    }

    #[tokio::test]
    async fn stop_is_heard_while_a_voice_is_being_built() {
        let mut rig = rig(sentences(3));
        let gate = Arc::new(Notify::new());
        rig.voices.held.lock().unwrap().push((2.0, gate));
        rig.settle().await;
        rig.send(ListenInput::Speed(2.0));
        rig.send(ListenInput::Stop);
        let seen = rig.settle().await;
        assert_eq!(ended(&seen), Some(None));
        assert!(rig.closed);
    }

    #[tokio::test]
    async fn a_closed_input_is_a_stop() {
        let mut rig = rig_endless(sentences(2));
        rig.settle().await;
        let ListenLink { input, output } = rig.link;
        drop(input);
        rig.link = ListenLink { input: tokio::sync::mpsc::unbounded_channel().0, output };
        let seen = rig.settle().await;
        assert!(cleared(&seen));
        assert_eq!(ended(&seen), Some(None));
        assert!(rig.closed);
    }

    #[tokio::test]
    async fn nothing_more_is_asked_of_the_text_after_the_session_ends() {
        let pulled = Arc::new(AtomicUsize::new(0));
        let counter = pulled.clone();
        let text = futures::stream::iter(0..50).map(move |i| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(sentence(i))
        });
        let mut rig = rig_with(text.boxed());
        rig.settle().await;
        let before = pulled.load(Ordering::SeqCst);
        // The listener goes away without a word.
        drop(rig.link.output);
        tokio::time::sleep(Duration::from_millis(60)).await;
        let _ = rig.link.input.send(ListenInput::Played { utterance: 0 });
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(pulled.load(Ordering::SeqCst), before);
    }
}
