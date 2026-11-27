//! Sentences into audio, in order, a limited way ahead of playback.
//!
//! A voice makes one sentence at a time, and takes longer over some than the
//! time another takes to say. Making all of a long text before saying any of it
//! would leave the person waiting for the end of it, and making it all while
//! the first is playing would spend work on what may never be heard. A
//! [`Narrator`] is the queue between the two: sentences go in, one at a time is
//! made, in the order they went in, and no more than `max_ahead` finished ones
//! wait to be played.
//!
//! It is only the queue. It does not play anything, hear anything or know what
//! the sentences are for: the owner says what was played with
//! [`Narrator::played`], and gets back the sentences, with whatever it tagged
//! them with, so it can keep its own account of what was heard. That is what
//! lets a conversation's reply and a long article use the same type.
//!
//! # Results arrive on another task
//!
//! The narrator is not a task of its own, so it cannot wait for a synthesis. It
//! starts one on the Tokio runtime and its `done` callback hands the result
//! back, from that task, for the owner to pass to [`Narrator::finished`] from
//! wherever it keeps the narrator -- usually by sending it down a channel to
//! the loop that does. The callback must not wait on anything, as it runs on
//! the task that made the sentence.
//!
//! # Clearing
//!
//! [`Narrator::clear`] forgets everything and starts a new generation. A
//! synthesis that was under way is aborted, but one that had already finished
//! may be sitting in the owner's channel, so every result carries the
//! generation it was made for and [`Narrator::finished`] answers
//! [`Narrated::Stale`] to one from before. Ids carry on counting through a
//! clear: a stale result can never be taken for a new sentence.
//!
//! # Sentences said together
//!
//! A voice handed one sentence at a time starts every one afresh, which
//! sounds like a list being read out ([`text::joins`]). An owner that asks for
//! it ([`Narrator::joining`]) has the sentences waiting made together, as one
//! clip, as many as there is time to make: every voice says what it is handed
//! in one go ([`Synthesiser`]), so the longer the clips, the fewer the
//! fresh starts.
//!
//! How long a clip may be is the same rule for every voice, from how fast
//! the voice has been making speech on this computer, which every voice
//! keeps for itself ([`Synthesiser::rate`]).
//!
//! - **Behind a clip that is playing**, the next is made while it is said, so
//!   it may be as long as can be made in [`SAFETY`] of the time the audio
//!   ahead has left to play, and in no more than [`MAX_MAKING`]. A fast voice
//!   reaches [`text::MAX_CLIP`] by the second clip; a slow one, or a busy
//!   computer, keeps them short enough that the speech never stops for one.
//!   Playback the owner holds ([`Narrator::pause`]) is not running down.
//! - **Into silence** -- the start of a reply -- the person is waiting. An
//!   owner may let the first clip wait ([`Narrator::waiting`]) for a second
//!   sentence to say with the first, for as long as the audio would start no
//!   more than that much later than the first sentence alone would have:
//!   the wait for the second sentence's words and the time to make them both
//!   count. It waits only while a sentence of the usual length
//!   ([`LIKELY_NEXT`]) could still be made in that time, so a voice that
//!   could not make one -- every voice on a computer's processor, at half a
//!   second -- does not wait for nothing. It stops waiting as soon as a
//!   second sentence comes, whether it fits or not, or the owner says there
//!   is nothing more for now ([`Narrator::flush`]). The owner looks again
//!   when told ([`Narrator::wake_at`]).
//!
//! A clip comes back as one [`Sentence`]: the last one's id, their words
//! joined, and the first one's tag; and is played as that. When that id is
//! [played](Narrator::played), each sentence in it comes back on its own, as
//! it would have one at a time, so the owner's account of what was heard is
//! the same, only coarser: a clip stopped half way was not heard. A clip
//! that cannot be made is made again a sentence at a time, so that a sentence
//! the voice refuses does not take its neighbours with it.

use super::synth::{Speech, Synthesiser};
use super::text::{self, MAX_CLIP};
use crate::error::{CommandError, CommandResult, codes};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// How much of the time the audio ahead has left a clip behind it is sized to
/// take to make. Half: a voice slows down when the computer is busy, and a
/// clip that is late is a gap in the middle of what is being said.
const SAFETY: f64 = 0.5;

/// How fast a voice is taken to make speech, in characters a second, until it
/// has been timed: four times as fast as it is said, which is Kokoro, the
/// slowest voice here, on this computer. Behind a clip, with [`SAFETY`], that
/// is a clip twice as long as the one ahead of it.
const ASSUMED_RATE: f64 = 60.0;

/// The longest a clip made behind another may take to make. Speech talked
/// over is let go of, but a voice already making a clip of it goes on until
/// it is done, holding the voice: the next reply should not wait long behind
/// words nobody will hear.
const MAX_MAKING: Duration = Duration::from_secs(2);

/// About how long the next sentence of a spoken reply is, in characters:
/// what the first sentence waits for room for. A reply's sentences run from
/// about 30 characters to about 90.
const LIKELY_NEXT: usize = 60;

/// One sentence's synthesis finishing: handed back to [`Narrator::finished`].
#[derive(Debug)]
pub struct Synthesised {
    pub generation: u64,
    pub id: u64,
    pub result: CommandResult<Speech>,
}

/// Making speech at `0` characters a second.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Pace(f64);

impl Pace {
    /// How many characters can be made in `time`, to the nearest.
    fn chars_in(self, time: Duration) -> usize {
        (time.as_secs_f64() * self.0).round() as usize
    }

    /// How long `chars` characters take to make.
    fn time_for(self, chars: usize) -> Duration {
        Duration::from_secs_f64(chars as f64 / self.0)
    }
}

/// A sentence, and whatever its owner wants to remember about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sentence<T> {
    pub id: u64,
    pub text: String,
    pub tag: T,
}

/// What became of a finished synthesis.
#[derive(Debug)]
pub enum Narrated<T> {
    /// Ready to play. It now counts as waiting to be played, until
    /// [`Narrator::played`] says it has been.
    Ready { sentence: Sentence<T>, speech: Speech },
    /// It could not be said, and has been skipped. `error` is `None` when the
    /// voice answered with nothing to play.
    Failed { sentence: Sentence<T>, error: Option<CommandError> },
    /// Nothing for the owner to do: it was for something cleared since, or
    /// sentences said together that could not be, which are being made again
    /// one at a time.
    Stale,
}

/// Aborts the task it holds when dropped, which is how aborting a task that
/// has spawned another reaches the one doing the work.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Whether two sentences in a row, by their tags, may be said together.
type Joins<T> = Box<dyn Fn(&T, &T) -> bool + Send + Sync>;

/// Turns queued sentences into audio one at a time, a limited way ahead of
/// what has been played.
pub struct Narrator<T> {
    synthesiser: Arc<dyn Synthesiser>,
    /// How many clips may wait to be played.
    max_ahead: usize,
    done: Arc<dyn Fn(Synthesised) + Send + Sync>,
    /// `None` says every sentence alone.
    joins: Option<Joins<T>>,
    /// How much later than its first sentence alone would have a clip said
    /// into silence may start, to take a second with it. Nothing waits.
    first_wait: Duration,
    /// When what has been handed over to be played will have been, as near as
    /// can be told from here: played from as soon as it is ready, and moved
    /// on by however long playback was paused. Playback held up any other
    /// way only makes it later than this, which makes clips shorter.
    audio_until: Option<Instant>,
    /// Playback has been paused since then.
    paused_since: Option<Instant>,
    /// Sentences up to this id are all there is for now: see
    /// [`Narrator::flush`].
    flushed_through: Option<u64>,
    /// A clip is waiting for more to be said with it, until this.
    held_until: Option<Instant>,
    next_id: u64,
    /// Which queue results belong to.
    generation: u64,
    /// Waiting to be made, each with when it was queued.
    queued: VecDeque<(Instant, Sentence<T>)>,
    /// Being made: one clip at a time, in order -- one sentence, or several
    /// said together. The task is aborted if the queue is cleared.
    making: Option<(Vec<Sentence<T>>, JoinHandle<()>)>,
    /// Made, and not yet played: clips, each one or more sentences.
    ready: VecDeque<Vec<Sentence<T>>>,
    /// Sentences up to this id are made one at a time: they were in a clip
    /// that could not be made.
    alone_through: Option<u64>,
}

impl<T: Clone + Send + 'static> Narrator<T> {
    /// `done` is called, from another task, as each synthesis finishes. A
    /// `max_ahead` of nothing would never make anything, so it is at least one.
    pub fn new(
        synthesiser: Arc<dyn Synthesiser>,
        max_ahead: usize,
        done: Arc<dyn Fn(Synthesised) + Send + Sync>,
    ) -> Self {
        Self {
            synthesiser,
            max_ahead: max_ahead.max(1),
            done,
            joins: None,
            first_wait: Duration::ZERO,
            audio_until: None,
            paused_since: None,
            flushed_through: None,
            held_until: None,
            next_id: 0,
            generation: 0,
            queued: VecDeque::new(),
            making: None,
            ready: VecDeque::new(),
            alone_through: None,
        }
    }

    /// Have sentences waiting made together, as many as there is time for,
    /// two in a row only when `joins` says their tags may be: see the module
    /// docs.
    pub fn joining(mut self, joins: impl Fn(&T, &T) -> bool + Send + Sync + 'static) -> Self {
        self.joins = Some(Box::new(joins));
        self
    }

    /// Let a clip said into silence start up to `wait` later than its first
    /// sentence alone would have, to say a second with it. Only with
    /// [`Narrator::joining`], and only for tags that join their own kind.
    pub fn waiting(mut self, wait: Duration) -> Self {
        self.first_wait = wait;
        self
    }

    /// Queue a sentence, and answer its id. Starts making it at once if
    /// nothing else is being made, not too much is waiting to be played and
    /// it is not waiting for a sentence to say with it.
    ///
    /// Must be called on a Tokio runtime, which is where the work is started.
    pub fn say(&mut self, text: String, tag: T) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.queued.push_back((Instant::now(), Sentence { id, text, tag }));
        self.pump();
        id
    }

    /// Everything queued is all there is for now: none of it waits for more
    /// to be said with it. For an owner whose text has ended, or stopped for
    /// a while, as a reply does for a tool. What is said after is waited for
    /// again.
    pub fn flush(&mut self) {
        self.flushed_through = self.next_id.checked_sub(1);
        self.pump();
    }

    /// Playback has stopped where it was, until [`Narrator::resume`]: what is
    /// left to play is not running down.
    pub fn pause(&mut self) {
        self.paused_since.get_or_insert_with(Instant::now);
    }

    /// Playback goes on from where it stopped.
    pub fn resume(&mut self) {
        if let Some(since) = self.paused_since.take()
            && let Some(until) = self.audio_until.as_mut()
        {
            *until += since.elapsed();
        }
    }

    /// When a clip waiting for more to be said with it stops waiting, if one
    /// is: the owner calls [`Narrator::wake`] then.
    pub fn wake_at(&self) -> Option<Instant> {
        self.held_until
    }

    /// Start a clip that was waiting for more to be said with it, if it has
    /// waited long enough.
    pub fn wake(&mut self) {
        self.pump();
    }

    /// A synthesis has finished: what became of it. Starts the next one,
    /// except after a [`Narrated::Stale`] result, which changes nothing.
    pub fn finished(&mut self, done: Synthesised) -> Narrated<T> {
        // Cleared since: what was asked for is not wanted.
        if done.generation != self.generation {
            return Narrated::Stale;
        }
        let Some((mut clip, _)) =
            self.making.take_if(|(clip, _)| clip.last().is_some_and(|s| s.id == done.id))
        else {
            return Narrated::Stale;
        };
        let narrated = match done.result {
            Ok(speech) if !speech.samples.is_empty() => {
                // Played after what is ahead of it, or now if nothing is.
                let now = self.playback_now();
                let from = match self.ready.is_empty() {
                    true => now,
                    false => self.audio_until.filter(|&until| until > now).unwrap_or(now),
                };
                self.audio_until = Some(from + length_of(&speech));
                let sentence = together(&clip);
                self.ready.push_back(clip);
                Narrated::Ready { sentence, speech }
            }
            // Several that could not be said together: each again, alone.
            result if clip.len() > 1 => {
                if let Err(e) = result {
                    tracing::debug!(error = %e, "sentences said together failed; saying them one at a time");
                }
                self.alone_through = clip.last().map(|s| s.id);
                let now = Instant::now();
                for sentence in clip.into_iter().rev() {
                    self.queued.push_front((now, sentence));
                }
                Narrated::Stale
            }
            result => Narrated::Failed { sentence: clip.remove(0), error: result.err() },
        };
        self.pump();
        narrated
    }

    /// `id` has been played, and so has everything before it: answers those
    /// sentences, in order, each on its own, and starts the next synthesis if
    /// there is room for it now. An id that is not waiting to be played --
    /// cleared since, or never made -- names nothing, and nothing is answered.
    pub fn played(&mut self, id: u64) -> Vec<Sentence<T>> {
        let Some(at) = self.ready.iter().position(|clip| clip.iter().any(|s| s.id == id)) else {
            return Vec::new();
        };
        let played: Vec<Sentence<T>> = self.ready.drain(..=at).flatten().collect();
        if self.ready.is_empty() {
            self.audio_until = None;
        }
        self.pump();
        played
    }

    /// Forget everything queued, being made or waiting to be played.
    pub fn clear(&mut self) {
        self.generation += 1;
        self.queued.clear();
        if let Some((_, task)) = self.making.take() {
            task.abort();
        }
        self.ready.clear();
        self.alone_through = None;
        self.audio_until = None;
        self.paused_since = None;
        self.flushed_through = None;
        self.held_until = None;
    }

    /// Whether anything made has been handed over to be played and has not
    /// been played yet: whether the transport has something to play.
    pub fn playing(&self) -> bool {
        !self.ready.is_empty()
    }

    /// Whether anything is queued, being made or still to be played.
    pub fn outstanding(&self) -> bool {
        !self.queued.is_empty() || self.making.is_some() || !self.ready.is_empty()
    }

    /// Now, by a clock that stops while playback is paused: what
    /// [`Self::audio_until`] is counted in.
    fn playback_now(&self) -> Instant {
        self.paused_since.unwrap_or_else(Instant::now)
    }

    /// How fast the voice makes speech: as it says, once it has been timed.
    fn pace(&self) -> Pace {
        Pace(
            self.synthesiser
                .rate()
                .filter(|rate| rate.is_finite() && *rate > 0.0)
                .unwrap_or(ASSUMED_RATE),
        )
    }

    /// Start the next clip's synthesis, if one is not under way, not too many
    /// are waiting to be played and it is not waiting for more: the next
    /// sentence, and those after it that may be said with it.
    fn pump(&mut self) {
        self.held_until = None;
        if self.making.is_some() || self.ready.len() >= self.max_ahead {
            return;
        }
        let Some(&(queued_at, ref first)) = self.queued.front() else { return };
        let alone = self.alone_through.is_some_and(|through| first.id <= through);
        let mut take = 1;
        if let Some(joins) = self.joins.as_ref().filter(|_| !alone) {
            let now = Instant::now();
            let pace = self.pace();
            let first_len = first.text.chars().count();
            let silence = self.ready.is_empty();
            // How much there is time to make: see the module docs.
            let room = match silence {
                true => {
                    let left = (queued_at + self.first_wait).saturating_duration_since(now);
                    first_len + pace.chars_in(left)
                }
                false => {
                    let playing = self.playback_now();
                    let left = self.audio_until.map_or(Duration::ZERO, |until| {
                        until.saturating_duration_since(playing).mul_f64(SAFETY)
                    });
                    pace.chars_in(left.min(MAX_MAKING))
                }
            }
            .min(MAX_CLIP);
            let mut len = first_len;
            while let Some((_, next)) = self.queued.get(take) {
                let last = &self.queued[take - 1].1;
                if !joins(&last.tag, &next.tag) || !text::joins(&last.text, len, &next.text, room) {
                    break;
                }
                len += 1 + next.text.chars().count();
                take += 1;
            }
            // Into silence with nothing to say with it yet: wait for a
            // second sentence while one of the usual length could still fit.
            let likely = 1 + LIKELY_NEXT;
            if silence
                && self.queued.len() == 1
                && joins(&first.tag, &first.tag)
                && !text::ends_line(&first.text)
                && first_len + likely <= MAX_CLIP
                && self.flushed_through.is_none_or(|through| first.id > through)
                && let Some(wait) = self.first_wait.checked_sub(pace.time_for(likely))
                && now < queued_at + wait
            {
                self.held_until = Some(queued_at + wait);
                return;
            }
        }
        let clip: Vec<Sentence<T>> = self.queued.drain(..take).map(|(_, s)| s).collect();
        let generation = self.generation;
        let Sentence { id, text, .. } = together(&clip);
        let synthesiser = self.synthesiser.clone();
        let done = self.done.clone();
        let task = tokio::spawn(async move {
            // The work is its own task so that a panic in it is an error
            // here and not a sentence that never comes back; and it is
            // aborted with this one when the queue is cleared.
            let mut work =
                AbortOnDrop(tokio::spawn(async move { synthesiser.synthesise(&text).await }));
            let result = match (&mut work.0).await {
                Ok(result) => result,
                Err(e) => {
                    Err(CommandError::new(codes::PANIC, format!("speech synthesis failed: {e}")))
                }
            };
            done(Synthesised { generation, id, result });
        });
        self.making = Some((clip, task));
    }
}

/// A clip as the one sentence it is made and played as: the last one's id,
/// which is the one to say has been played, their words joined, and the first
/// one's tag.
fn together<T: Clone>(clip: &[Sentence<T>]) -> Sentence<T> {
    let text = clip.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ");
    Sentence { id: clip[clip.len() - 1].id, text, tag: clip[0].tag.clone() }
}

/// How long `speech` takes to play.
fn length_of(speech: &Speech) -> Duration {
    match speech.sample_rate {
        0 => Duration::ZERO,
        rate => Duration::from_secs_f64(speech.samples.len() as f64 / f64::from(rate)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speech::transcribe::BoxFuture;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    /// Sets a flag when it is dropped, which a future is when its task is
    /// aborted.
    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// A voice that says what its words ask of it: `fail`, `empty`, `panic`
    /// and `hang` go wrong in the ways their names say, the word `refuse`
    /// fails whatever is said with it, and anything else is as long as a
    /// person would take to say it, made at once. It says it makes speech at
    /// `rate`.
    #[derive(Default)]
    struct Fake {
        started: Mutex<Vec<String>>,
        abandoned: Arc<AtomicBool>,
        rate: Option<f64>,
    }

    /// How fast [`Fake`] speaks: characters a second, at a thousand samples a
    /// second.
    const SPOKEN: usize = 15;

    fn spoken(text: &str) -> Speech {
        Speech { samples: vec![1; text.chars().count() * 1000 / SPOKEN], sample_rate: 1000 }
    }

    impl Synthesiser for Fake {
        fn synthesise<'a>(&'a self, text: &'a str) -> BoxFuture<'a, CommandResult<Speech>> {
            self.started.lock().unwrap().push(text.to_string());
            let abandoned = self.abandoned.clone();
            Box::pin(async move {
                match text {
                    "panic" => panic!("the voice fell over"),
                    "fail" => Err(CommandError::new(codes::NETWORK, "no voice today")),
                    "empty" => Ok(Speech { samples: Vec::new(), sample_rate: 16_000 }),
                    "hang" => {
                        let _flag = DropFlag(abandoned);
                        std::future::pending::<()>().await;
                        unreachable!()
                    }
                    other if other.split(' ').any(|word| word == "refuse") => {
                        Err(CommandError::new(codes::NETWORK, "not that"))
                    }
                    other => Ok(spoken(other)),
                }
            })
        }

        fn rate(&self) -> Option<f64> {
            self.rate
        }
    }

    struct Rig {
        narrator: Narrator<&'static str>,
        results: UnboundedReceiver<Synthesised>,
        voice: Arc<Fake>,
    }

    fn rig(max_ahead: usize) -> Rig {
        rig_with(max_ahead, Fake::default())
    }

    fn rig_with(max_ahead: usize, voice: Fake) -> Rig {
        let voice = Arc::new(voice);
        let (tx, results) = unbounded_channel();
        let narrator = Narrator::new(
            voice.clone(),
            max_ahead,
            Arc::new(move |done| {
                let _ = tx.send(done);
            }),
        );
        Rig { narrator, results, voice }
    }

    /// A rig whose narrator says sentences with the same tag together.
    fn joining_rig(max_ahead: usize) -> Rig {
        joining_rig_with(max_ahead, Fake::default())
    }

    fn joining_rig_with(max_ahead: usize, voice: Fake) -> Rig {
        let Rig { narrator, results, voice } = rig_with(max_ahead, voice);
        Rig { narrator: narrator.joining(|a, b| a == b), results, voice }
    }

    /// A joining rig whose first sentence into silence may wait half a second
    /// for a second, with a voice fast enough to make one in the time.
    fn waiting_rig() -> Rig {
        waiting_rig_with(Fake { rate: Some(1000.0), ..Fake::default() })
    }

    fn waiting_rig_with(voice: Fake) -> Rig {
        let Rig { narrator, results, voice } = joining_rig_with(2, voice);
        Rig { narrator: narrator.waiting(Duration::from_millis(500)), results, voice }
    }

    const OPENING: &str = "Okay, here is what I found for you.";

    impl Rig {
        async fn next(&mut self) -> Synthesised {
            tokio::time::timeout(Duration::from_secs(5), self.results.recv())
                .await
                .expect("a result within five seconds")
                .expect("the narrator is still there")
        }

        /// The next result, handed to the narrator.
        async fn finish(&mut self) -> Narrated<&'static str> {
            let done = self.next().await;
            self.narrator.finished(done)
        }

        fn started(&self) -> Vec<String> {
            self.voice.started.lock().unwrap().clone()
        }

        /// Long enough for anything that was going to happen to have.
        async fn settle(&self) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn ready_id(narrated: &Narrated<&'static str>) -> u64 {
        match narrated {
            Narrated::Ready { sentence, .. } => sentence.id,
            other => panic!("expected a sentence ready to play, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sentences_are_made_one_at_a_time_in_the_order_they_were_said() {
        let mut rig = rig(10);
        let a = rig.narrator.say("a".into(), "first");
        let b = rig.narrator.say("bb".into(), "second");
        let c = rig.narrator.say("ccc".into(), "third");
        assert_eq!((a, b, c), (0, 1, 2));

        // Nothing starts the second while the first is being made.
        rig.settle().await;
        assert_eq!(rig.started(), vec!["a"]);

        let first = rig.finish().await;
        match &first {
            Narrated::Ready { sentence, speech } => {
                assert_eq!(sentence, &Sentence { id: 0, text: "a".into(), tag: "first" });
                assert_eq!(speech, &spoken("a"));
            }
            other => panic!("{other:?}"),
        }
        rig.settle().await;
        assert_eq!(rig.started(), vec!["a", "bb"]);
        assert_eq!(ready_id(&rig.finish().await), 1);
        assert_eq!(ready_id(&rig.finish().await), 2);
        assert_eq!(rig.started(), vec!["a", "bb", "ccc"]);
    }

    #[tokio::test]
    async fn no_more_than_max_ahead_wait_to_be_played() {
        let mut rig = rig(2);
        for text in ["a", "b", "c"] {
            rig.narrator.say(text.into(), "");
        }
        assert_eq!(ready_id(&rig.finish().await), 0);
        assert_eq!(ready_id(&rig.finish().await), 1);
        // Two are waiting to be played: the third is held back.
        rig.settle().await;
        assert_eq!(rig.started(), vec!["a", "b"]);
        assert!(rig.results.try_recv().is_err());

        // One of them is played, which makes room.
        assert_eq!(rig.narrator.played(0).len(), 1);
        assert_eq!(ready_id(&rig.finish().await), 2);
        assert_eq!(rig.started(), vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn a_max_ahead_of_nothing_still_makes_something() {
        let mut rig = rig(0);
        rig.narrator.say("a".into(), "");
        assert_eq!(ready_id(&rig.finish().await), 0);
    }

    #[tokio::test]
    async fn playing_drains_through_the_id_in_order_and_ignores_ids_it_does_not_know() {
        let mut rig = rig(10);
        for text in ["a", "b", "c"] {
            rig.narrator.say(text.into(), text);
        }
        for _ in 0..3 {
            rig.finish().await;
        }
        assert!(rig.narrator.played(99).is_empty());
        assert!(rig.narrator.outstanding());

        let played = rig.narrator.played(1);
        let texts: Vec<&str> = played.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["a", "b"]);
        assert_eq!(played.iter().map(|s| s.tag).collect::<Vec<_>>(), vec!["a", "b"]);

        // Already played: it names nothing now.
        assert!(rig.narrator.played(0).is_empty());
        assert!(rig.narrator.outstanding());
        assert_eq!(rig.narrator.played(2).len(), 1);
        assert!(!rig.narrator.outstanding());
    }

    #[tokio::test]
    async fn a_sentence_is_outstanding_from_being_said_until_it_is_played() {
        let mut rig = rig(2);
        assert!(!rig.narrator.outstanding());
        let id = rig.narrator.say("a".into(), "");
        assert!(rig.narrator.outstanding(), "being made");
        rig.finish().await;
        assert!(rig.narrator.outstanding(), "waiting to be played");
        rig.narrator.played(id);
        assert!(!rig.narrator.outstanding());
    }

    #[tokio::test]
    async fn a_sentence_that_cannot_be_said_is_skipped_and_the_next_goes_on() {
        let mut rig = rig(2);
        rig.narrator.say("fail".into(), "x");
        rig.narrator.say("empty".into(), "y");
        rig.narrator.say("fine".into(), "z");

        match rig.finish().await {
            Narrated::Failed { sentence, error: Some(error) } => {
                assert_eq!(sentence.text, "fail");
                assert_eq!(error.code, codes::NETWORK);
            }
            other => panic!("{other:?}"),
        }
        match rig.finish().await {
            Narrated::Failed { sentence, error: None } => assert_eq!(sentence.tag, "y"),
            other => panic!("{other:?}"),
        }
        assert_eq!(ready_id(&rig.finish().await), 2);
        // Only what was ready waits to be played: failures took no room.
        assert_eq!(rig.narrator.played(2).len(), 1);
        assert!(!rig.narrator.outstanding());
    }

    #[tokio::test]
    async fn a_panic_in_synthesis_is_a_failure_not_a_sentence_that_never_comes_back() {
        let mut rig = rig(2);
        rig.narrator.say("panic".into(), "");
        rig.narrator.say("fine".into(), "");
        match rig.finish().await {
            Narrated::Failed { error: Some(error), .. } => {
                assert_eq!(error.code, codes::PANIC);
                assert!(error.message.contains("speech synthesis failed"), "{}", error.message);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(ready_id(&rig.finish().await), 1);
    }

    #[tokio::test]
    async fn clearing_makes_what_was_finished_stale_and_ids_carry_on() {
        let mut rig = rig(2);
        let first = rig.narrator.say("a".into(), "");
        let done = rig.next().await;
        // It finished, and then the owner cleared before it got to look.
        rig.narrator.clear();
        assert!(!rig.narrator.outstanding());
        assert!(matches!(rig.narrator.finished(done), Narrated::Stale));
        assert!(!rig.narrator.outstanding());

        let second = rig.narrator.say("b".into(), "");
        assert!(second > first, "{second} after {first}");
        assert_eq!(ready_id(&rig.finish().await), second);
    }

    #[tokio::test]
    async fn clearing_stops_the_sentence_being_made_and_forgets_those_waiting() {
        let mut rig = rig(2);
        rig.narrator.say("hang".into(), "");
        rig.narrator.say("never".into(), "");
        rig.settle().await;
        assert_eq!(rig.started(), vec!["hang"]);
        assert!(!rig.voice.abandoned.load(Ordering::SeqCst));

        rig.narrator.clear();
        rig.settle().await;
        assert!(rig.voice.abandoned.load(Ordering::SeqCst), "the synthesis was not aborted");
        assert!(rig.results.try_recv().is_err(), "an aborted synthesis reported");
        assert_eq!(rig.started(), vec!["hang"], "a forgotten sentence was made");
        assert!(!rig.narrator.outstanding());
    }

    #[tokio::test]
    async fn what_was_waiting_to_be_played_is_forgotten_by_a_clear() {
        let mut rig = rig(2);
        let id = rig.narrator.say("a".into(), "");
        rig.finish().await;
        rig.narrator.clear();
        assert!(rig.narrator.played(id).is_empty());
        assert!(!rig.narrator.outstanding());
    }

    #[tokio::test]
    async fn the_first_sentence_is_made_alone_and_those_waiting_behind_it_together() {
        let mut rig = joining_rig(2);
        let first = "Okay, here is what I found for you.";
        rig.narrator.say(first.into(), "reply");
        rig.narrator.say("The dentist is at ten.".into(), "reply");
        rig.narrator.say("Lunch is at one.".into(), "reply");
        rig.settle().await;
        assert_eq!(rig.started(), vec![first], "said into silence, so alone");

        assert_eq!(ready_id(&rig.finish().await), 0);
        match rig.finish().await {
            Narrated::Ready { sentence, .. } => assert_eq!(
                sentence,
                Sentence {
                    id: 2,
                    text: "The dentist is at ten. Lunch is at one.".into(),
                    tag: "reply"
                }
            ),
            other => panic!("{other:?}"),
        }
        assert_eq!(rig.started()[1], "The dentist is at ten. Lunch is at one.");

        // Played as the last one's id, each comes back on its own.
        assert_eq!(rig.narrator.played(0).len(), 1);
        let played = rig.narrator.played(2);
        assert_eq!(
            played.iter().map(|s| (s.id, s.text.as_str())).collect::<Vec<_>>(),
            vec![(1, "The dentist is at ten."), (2, "Lunch is at one.")]
        );
        assert!(!rig.narrator.outstanding());
    }

    #[tokio::test]
    async fn max_ahead_counts_clips_not_the_sentences_in_them() {
        let mut rig = joining_rig(2);
        rig.narrator.say("Okay, here is what I found for you.".into(), "reply");
        rig.narrator.say("The dentist is at ten.".into(), "reply");
        rig.narrator.say("Lunch is at one.".into(), "reply");
        rig.finish().await;
        rig.finish().await;
        rig.narrator.played(0);
        // One clip of two sentences waits: there is room for another.
        rig.narrator.say("And that's all.".into(), "reply");
        assert_eq!(ready_id(&rig.finish().await), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_clip_behind_another_is_as_long_as_can_be_made_while_that_one_plays() {
        // A hundred characters a second, and the first is thirty characters
        // said in two seconds.
        let mut rig = joining_rig_with(2, Fake { rate: Some(100.0), ..Fake::default() });
        let forty = "b".repeat(40);
        rig.narrator.say("a".repeat(30), "reply");
        for _ in 0..3 {
            rig.narrator.say(forty.clone(), "reply");
        }
        rig.finish().await;
        // Two seconds left to play, and half of them to make the next in: a
        // hundred characters, which two forties fit and three do not.
        match rig.finish().await {
            Narrated::Ready { sentence, .. } => {
                assert_eq!(sentence.text, format!("{forty} {forty}"))
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn however_fast_the_voice_a_clip_is_never_past_the_cap() {
        let mut rig = joining_rig_with(2, Fake { rate: Some(1e9), ..Fake::default() });
        let hundred = "b".repeat(100);
        rig.narrator.say("a".repeat(200), "reply");
        for _ in 0..3 {
            rig.narrator.say(hundred.clone(), "reply");
        }
        rig.finish().await;
        // Thirteen seconds to play: two hundreds fit in the cap, not three.
        match rig.finish().await {
            Narrated::Ready { sentence, .. } => {
                assert_eq!(sentence.text, format!("{hundred} {hundred}"));
                assert!(sentence.text.chars().count() <= MAX_CLIP);
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_clip_made_behind_another_takes_at_most_two_seconds_to_make() {
        // Sixty characters a second, behind thirteen seconds of audio: half
        // of it would be four hundred characters, two seconds a hundred and
        // twenty.
        let mut rig = joining_rig_with(2, Fake { rate: Some(60.0), ..Fake::default() });
        let fifty = "b".repeat(50);
        rig.narrator.say("a".repeat(200), "reply");
        for _ in 0..3 {
            rig.narrator.say(fifty.clone(), "reply");
        }
        rig.finish().await;
        match rig.finish().await {
            Narrated::Ready { sentence, .. } => {
                assert_eq!(sentence.text, format!("{fifty} {fifty}"))
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn what_is_left_to_play_does_not_run_down_while_playback_is_paused() {
        // A hundred characters a second. Two seconds and two thirds of audio
        // ahead when the third clip is started, a second and a half of it
        // paused: half of all of it is room for two forties, half of what
        // would be left without the pause is not.
        let forty = "b".repeat(40);
        for resumed in [false, true] {
            let mut rig = joining_rig_with(3, Fake { rate: Some(100.0), ..Fake::default() });
            rig.narrator.say("a".repeat(30), "reply");
            rig.finish().await;
            rig.narrator.say("x".repeat(10), "reply");
            rig.narrator.say(forty.clone(), "reply");
            rig.narrator.say(forty.clone(), "reply");
            rig.narrator.pause();
            tokio::time::sleep(Duration::from_millis(1500)).await;
            if resumed {
                rig.narrator.resume();
            }
            rig.finish().await;
            match rig.finish().await {
                Narrated::Ready { sentence, .. } => {
                    assert_eq!(sentence.text, format!("{forty} {forty}"), "resumed: {resumed}")
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_voice_that_has_not_been_timed_is_taken_to_be_as_slow_as_kokoro() {
        // Four times as fast as it is said: behind a clip, twice its length.
        let mut rig = joining_rig(2);
        let forty = "b".repeat(40);
        rig.narrator.say("a".repeat(30), "reply");
        rig.narrator.say(forty.clone(), "reply");
        rig.narrator.say("c".repeat(20), "reply");
        rig.finish().await;
        match rig.finish().await {
            Narrated::Ready { sentence, .. } => assert_eq!(sentence.text, forty),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_first_sentence_into_silence_waits_for_a_second_to_be_made_with_it() {
        let mut rig = waiting_rig();
        rig.narrator.say(OPENING.into(), "reply");
        rig.settle().await;
        assert!(rig.started().is_empty(), "waiting");
        assert!(rig.narrator.wake_at().is_some());
        assert!(rig.narrator.outstanding());
        assert!(!rig.narrator.playing());

        rig.narrator.say("Lunch is at one.".into(), "reply");
        assert_eq!(rig.narrator.wake_at(), None);
        rig.settle().await;
        assert_eq!(rig.started(), vec![format!("{OPENING} Lunch is at one.")]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_wait_ends_when_a_usual_sentence_would_start_it_too_late() {
        let mut rig = waiting_rig();
        let start = Instant::now();
        rig.narrator.say(OPENING.into(), "reply");
        let until = rig.narrator.wake_at().expect("waiting");
        // Half a second, less what a sentence of the usual length takes to
        // make.
        let likely = Duration::from_secs_f64((1 + LIKELY_NEXT) as f64 / 1000.0);
        assert_eq!(until - start, Duration::from_millis(500) - likely);

        rig.narrator.wake();
        rig.settle().await;
        assert!(rig.started().is_empty(), "woken early, it waits on");
        tokio::time::sleep_until(until).await;
        rig.narrator.wake();
        rig.settle().await;
        assert_eq!(rig.started(), vec![OPENING]);
        assert_eq!(rig.narrator.wake_at(), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_sentence_that_cannot_be_said_with_the_first_ends_the_wait() {
        // Too long for a clip.
        let mut rig = waiting_rig();
        rig.narrator.say(OPENING.into(), "reply");
        rig.narrator.say("b".repeat(220), "reply");
        rig.settle().await;
        assert_eq!(rig.started(), vec![OPENING]);
        assert_eq!(rig.narrator.wake_at(), None);

        // Not of a kind to be said with it.
        let mut rig = waiting_rig();
        rig.narrator.say(OPENING.into(), "reply");
        rig.narrator.say("Let me see.".into(), "own");
        rig.settle().await;
        assert_eq!(rig.started(), vec![OPENING]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_flush_ends_the_wait_and_what_is_said_after_it_waits_again() {
        let mut rig = waiting_rig();
        let id = rig.narrator.say(OPENING.into(), "reply");
        rig.narrator.flush();
        rig.settle().await;
        assert_eq!(rig.started(), vec![OPENING]);
        rig.finish().await;
        rig.narrator.played(id);

        rig.narrator.say("Anything else?".into(), "reply");
        assert!(rig.narrator.wake_at().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_voice_too_slow_to_make_a_usual_sentence_in_the_time_does_not_wait() {
        // Sixty characters take six tenths of a second, and an untimed voice
        // is slower still.
        let mut rig = waiting_rig_with(Fake { rate: Some(100.0), ..Fake::default() });
        rig.narrator.say(OPENING.into(), "reply");
        assert_eq!(rig.narrator.wake_at(), None);
        let mut rig = waiting_rig_with(Fake::default());
        rig.narrator.say(OPENING.into(), "reply");
        assert_eq!(rig.narrator.wake_at(), None);
        rig.settle().await;
        assert_eq!(rig.started(), vec![OPENING]);
    }

    #[tokio::test(start_paused = true)]
    async fn neither_a_kind_that_does_not_join_nor_a_line_s_end_waits() {
        let Rig { narrator, results, voice } =
            rig_with(2, Fake { rate: Some(1000.0), ..Fake::default() });
        let narrator =
            narrator.joining(|a, b| a == b && *a == "reply").waiting(Duration::from_millis(500));
        let mut rig = Rig { narrator, results, voice };
        let own = rig.narrator.say("Let me see.".into(), "own");
        rig.settle().await;
        assert_eq!(rig.started(), vec!["Let me see."]);
        rig.finish().await;
        rig.narrator.played(own);

        rig.narrator.say("First, the dentist.¶".into(), "reply");
        rig.settle().await;
        assert_eq!(rig.started()[1], "First, the dentist.¶");
    }

    #[tokio::test(start_paused = true)]
    async fn a_clear_forgets_a_wait() {
        let mut rig = waiting_rig();
        rig.narrator.say(OPENING.into(), "reply");
        assert!(rig.narrator.wake_at().is_some());
        rig.narrator.clear();
        assert_eq!(rig.narrator.wake_at(), None);
        assert!(!rig.narrator.outstanding());
    }

    #[tokio::test]
    async fn a_line_s_end_and_a_different_tag_end_a_clip() {
        // Room for three clips, so each is made behind another, with room to
        // join what comes after it.
        let mut rig = joining_rig(3);
        rig.narrator.say("Okay, here is what I found for you today.".into(), "reply");
        rig.narrator.say("First, the dentist.¶".into(), "reply");
        rig.narrator.say("Then lunch.".into(), "reply");
        rig.narrator.say("Let me check.".into(), "own");
        rig.finish().await;
        assert_eq!(ready_id(&rig.finish().await), 1, "an item of a list keeps its own pause");
        assert_eq!(ready_id(&rig.finish().await), 2, "the session's own words are not the reply");
        rig.narrator.played(2);
        assert_eq!(ready_id(&rig.finish().await), 3);
    }

    #[tokio::test]
    async fn sentences_that_cannot_be_said_together_are_said_one_at_a_time() {
        let mut rig = joining_rig(4);
        rig.narrator.say("Okay, here is what I found for you.".into(), "reply");
        rig.narrator.say("One.".into(), "reply");
        rig.narrator.say("refuse".into(), "reply");
        rig.narrator.say("Three.".into(), "reply");
        rig.finish().await;
        assert!(matches!(rig.finish().await, Narrated::Stale));
        assert_eq!(rig.started()[1], "One. refuse Three.");

        // Each again, alone: only the one the voice refuses is lost.
        assert_eq!(ready_id(&rig.finish().await), 1);
        match rig.finish().await {
            Narrated::Failed { sentence, error: Some(_) } => assert_eq!(sentence.id, 2),
            other => panic!("{other:?}"),
        }
        assert_eq!(ready_id(&rig.finish().await), 3);
        assert_eq!(rig.started()[2..].to_vec(), vec!["One.", "refuse", "Three."]);
    }

    #[tokio::test]
    async fn a_result_for_a_sentence_not_being_made_is_stale() {
        let mut rig = rig(2);
        rig.narrator.say("a".into(), "");
        let mut done = rig.next().await;
        done.id = 77;
        assert!(matches!(rig.narrator.finished(done), Narrated::Stale));
    }
}
