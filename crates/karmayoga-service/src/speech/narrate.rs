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
//! it ([`Narrator::joining`]) has the sentences waiting behind a clip that is
//! already playing made together, as one clip, while anything said into
//! silence -- the first sentence of a reply -- is still made alone, so that it
//! starts as soon as it can. A clip may be at most [`GROWTH`] times as long as
//! the one ahead of it, since it is made while that one is said, and never
//! more than [`text::MAX_CLIP`].
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
use tokio::task::JoinHandle;

/// How many times as long as the clip ahead of it the next may be. The next
/// is made while that one is said, so it must be made in the time that one
/// takes to say: every voice here makes speech at least twice as fast as it is
/// said (Kokoro, the slowest on this computer, four times), so one twice as
/// long is ready in time.
const GROWTH: usize = 2;

/// One sentence's synthesis finishing: handed back to [`Narrator::finished`].
#[derive(Debug)]
pub struct Synthesised {
    pub generation: u64,
    pub id: u64,
    pub result: CommandResult<Speech>,
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
    next_id: u64,
    /// Which queue results belong to.
    generation: u64,
    /// Waiting to be made.
    queued: VecDeque<Sentence<T>>,
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
            next_id: 0,
            generation: 0,
            queued: VecDeque::new(),
            making: None,
            ready: VecDeque::new(),
            alone_through: None,
        }
    }

    /// Have sentences waiting behind a clip that is playing made together,
    /// two in a row only when `joins` says their tags may be: see the module
    /// docs.
    pub fn joining(mut self, joins: impl Fn(&T, &T) -> bool + Send + Sync + 'static) -> Self {
        self.joins = Some(Box::new(joins));
        self
    }

    /// Queue a sentence, and answer its id. Starts making it at once if
    /// nothing else is being made and not too much is waiting to be played.
    ///
    /// Must be called on a Tokio runtime, which is where the work is started.
    pub fn say(&mut self, text: String, tag: T) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.queued.push_back(Sentence { id, text, tag });
        self.pump();
        id
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
                for sentence in clip.into_iter().rev() {
                    self.queued.push_front(sentence);
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

    /// Start the next clip's synthesis, if one is not under way and not too
    /// many are waiting to be played: the next sentence, and those after it
    /// that may be said with it.
    fn pump(&mut self) {
        if self.making.is_some() || self.ready.len() >= self.max_ahead {
            return;
        }
        let Some(first) = self.queued.pop_front() else { return };
        let alone = self.alone_through.is_some_and(|through| first.id <= through);
        let mut clip = vec![first];
        if let Some(joins) = self.joins.as_ref().filter(|_| !alone) {
            // Into silence, as soon as it can be; behind a clip, as much as
            // can be made while that one is said.
            let room = self.ready.back().map_or(0, |ahead| length(ahead) * GROWTH).min(MAX_CLIP);
            let mut len = length(&clip);
            while let Some(next) = self.queued.front() {
                let last = &clip[clip.len() - 1];
                if !joins(&last.tag, &next.tag) || !text::joins(&last.text, len, &next.text, room) {
                    break;
                }
                len += 1 + next.text.chars().count();
                clip.extend(self.queued.pop_front());
            }
        }
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

/// How many characters a clip's words come to, joined.
fn length<T>(clip: &[Sentence<T>]) -> usize {
    clip.iter().map(|s| s.text.chars().count()).sum::<usize>() + clip.len().saturating_sub(1)
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
    /// fails whatever is said with it, and anything else is one sample the
    /// length of the sentence.
    #[derive(Default)]
    struct Fake {
        started: Mutex<Vec<String>>,
        abandoned: Arc<AtomicBool>,
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
                    other => Ok(Speech { samples: vec![other.len() as i16], sample_rate: 16_000 }),
                }
            })
        }
    }

    struct Rig {
        narrator: Narrator<&'static str>,
        results: UnboundedReceiver<Synthesised>,
        voice: Arc<Fake>,
    }

    fn rig(max_ahead: usize) -> Rig {
        let voice = Arc::new(Fake::default());
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
        let Rig { narrator, results, voice } = rig(max_ahead);
        Rig { narrator: narrator.joining(|a, b| a == b), results, voice }
    }

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
                assert_eq!(speech.samples, vec![1]);
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

    #[tokio::test]
    async fn a_clip_is_at_most_twice_the_one_ahead_of_it_and_never_past_the_cap() {
        let mut rig = joining_rig(2);
        rig.narrator.say("Okay.".into(), "reply");
        rig.narrator.say("Sure thing.".into(), "reply");
        rig.narrator.say("Next one.".into(), "reply");
        rig.finish().await;
        // Ten characters of room behind "Okay.": the first waiting is made
        // whatever its length, but nothing joins it.
        assert_eq!(ready_id(&rig.finish().await), 1);
        rig.narrator.played(1);
        assert_eq!(ready_id(&rig.finish().await), 2);
        rig.narrator.played(2);

        let long = "a".repeat(200);
        let hundred = "b".repeat(100);
        rig.narrator.say(long.clone(), "reply");
        for _ in 0..3 {
            rig.narrator.say(hundred.clone(), "reply");
        }
        rig.finish().await;
        // Twice 200 is more than the cap: two hundreds fit in it, not three.
        match rig.finish().await {
            Narrated::Ready { sentence, .. } => {
                assert_eq!(sentence.text, format!("{hundred} {hundred}"));
                assert!(sentence.text.chars().count() <= MAX_CLIP);
            }
            other => panic!("{other:?}"),
        }
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
