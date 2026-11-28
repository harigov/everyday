//! The wake word with a model of its own ([`spotter`]), for a name one was
//! trained for: a [`Spotter`] listens to every 80 ms, and only an utterance
//! it heard the name in early on is written down at all, for what came after
//! the name. See [`super`] and `docs/plans/wake-word.md`.

use super::{
    FRESH_EVERY, GREETINGS, WakeLink, Woken, heard, is_joining_mark, sound_of, sounds_alike,
    words_of,
};
use crate::error::CommandResult;
use crate::service::Service;
use crate::speech::SAMPLE_RATE;
use crate::speech::access::HeardFrom;
use crate::speech::detect::{Detected, Heard, ListenerInput, SpeechDetector, Tuning};
use crate::speech::expect::Occasion;
use crate::speech::pcm;
use crate::speech::spotter::{self, Spotter};
use crate::speech::utterance::UtteranceTranscriber;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// How long after an utterance's end a hit still belongs to it, in samples.
/// The spotter mostly hears the name within a tenth of a second of its end,
/// either side, but in noise up to 0.4 s after; and the detector closes the
/// utterance's audio 0.2 s after the last sound.
const HIT_AFTER: u64 = SAMPLE_RATE as u64 / 2;

/// Shorter than this after the step that heard the name, in milliseconds,
/// and with no syllable begun in it ([`syllable_after`]), nothing was said
/// after the name: what follows is the name's own last sound and the 0.2 s
/// of quiet the detector keeps after speech.
const NOTHING_AFTER_MS: u64 = 400;

/// Heard later than this into an utterance, in milliseconds, the name came
/// after other words -- "I asked Jatayu" -- and the assistant is being
/// talked about, not called. Room for a greeting before it ("hey", "okay")
/// and for a slow name.
const CALLED_WITHIN_MS: u64 = 2_000;

/// How sure of the name the spotter must have grown ([`Spotter::peak`]) to
/// wake when the recogniser wrote nothing like it. Parakeet, which does not
/// know "Jatayu", left it out of a fifth of the recordings that called it
/// ("Take a note.", "Add milk to my shopping list."), so the recogniser
/// cannot be asked to agree; but nor should a spotter's near thing send
/// somebody else's sentence. On 22.6 h of held-out speech, steps this sure
/// woke 0.04 times an hour, against 0.31 at the model's own threshold (0.9).
const SURE: f32 = 0.99;

/// Words said before the name in what the model was trained on, besides the
/// greetings [`heard`] takes: "um, Jatayu", "so Jatayu". Taken here and not
/// by [`heard`] for every name, because here the spotter has also heard the
/// name early on: "so Jimmy said", heard only by a recogniser, is somebody
/// talking about Jimmy.
const FILLERS: &[&str] = &["um", "uh", "so", "hello"];

/// Whether the owner's voice check is on now: see [`spawn_spotted`].
pub type Refusing = Arc<dyn Fn() -> bool + Send + Sync>;

/// Start listening for `names`, which `model` hears, on the current Tokio
/// runtime: `detector` finds the utterances, `model` the name, and
/// `transcriber` hears only those utterances the name was heard in.
///
/// `refusing` says, when asked, whether the owner's voice check is on: then
/// `transcriber` writes nothing for anybody else, and the name alone is
/// written down too, to be sure it was the owner who said it.
pub fn spawn_spotted(
    model: Arc<spotter::Model>,
    detector: Box<dyn SpeechDetector>,
    transcriber: Arc<dyn UtteranceTranscriber>,
    names: Vec<String>,
    refusing: Refusing,
) -> WakeLink {
    let (input, audio) = unbounded_channel();
    let (tell, woken) = unbounded_channel();
    tokio::spawn(run_spotted(model, detector, transcriber, names, refusing, audio, tell));
    WakeLink { input, woken }
}

async fn run_spotted(
    model: Arc<spotter::Model>,
    detector: Box<dyn SpeechDetector>,
    transcriber: Arc<dyn UtteranceTranscriber>,
    names: Vec<String>,
    refusing: Refusing,
    mut audio: UnboundedReceiver<Vec<i16>>,
    woken: UnboundedSender<Woken>,
) {
    let (found, mut heard_rx) = unbounded_channel::<Heard>();
    let detector = crate::speech::detect::spawn_detector(detector, move |h| found.send(h).is_ok());
    let (done, mut finished) = unbounded_channel::<Option<String>>();
    let mut spotter = Spotter::new(model);
    // Samples heard, and how many of them had been when the detector was
    // last made afresh: it counts from then, and the spotter from the start.
    // The spotter has no counter to run out, and made afresh it would hear
    // nothing for its first two seconds.
    let (mut fed, mut detector_from) = (0u64, 0u64);
    let mut talking = false;
    let mut before_reset = false;
    let mut hits: VecDeque<Hit> = VecDeque::new();
    loop {
        tokio::select! {
            samples = audio.recv() => match samples {
                Some(samples) => {
                    for hit in spotter.push(&samples) {
                        // What follows the name is about to be wanted: the
                        // recogniser loads, if it was let go, while the
                        // utterance is ending.
                        transcriber.warm();
                        hits.push_back(Hit { at: hit.end_sample, peak: hit.score });
                    }
                    if let Some(last) = hits.back_mut() {
                        last.peak = last.peak.max(spotter.peak());
                    }
                    fed += samples.len() as u64;
                    let _ = detector.send(ListenerInput::Audio(samples));
                    if fed - detector_from >= FRESH_EVERY && !talking {
                        let _ = detector.send(ListenerInput::Reset);
                        detector_from = fed;
                        before_reset = true;
                    }
                }
                None => return,
            },
            heard = heard_rx.recv() => match heard {
                Some(Heard::Reset) => before_reset = false,
                Some(Heard::Found(Detected::Started)) => talking = true,
                Some(Heard::Found(Detected::Ended { samples, start_ms })) => {
                    talking = false;
                    // From before the reset: on a clock `detector_from` no
                    // longer matches.
                    if before_reset || samples.is_empty() {
                        continue;
                    }
                    let start = detector_from + pcm::sample_count(start_ms) as u64;
                    let end = start + samples.len() as u64;
                    let Some(hit) = take_hit(&mut hits, start, end) else { continue };
                    let at = (hit.at - start) as usize;
                    let sure = hit.peak >= SURE;
                    let (transcriber, names, done) = (transcriber.clone(), names.clone(), done.clone());
                    let refusing = refusing.clone();
                    tokio::spawn(async move {
                        let rest =
                            after_name(&*transcriber, &samples, at, &names, sure, refusing()).await;
                        let _ = done.send(rest);
                    });
                }
                None => return,
            },
            Some(rest) = finished.recv() => {
                if let Some(rest) = rest
                    && woken.send(Woken { rest }).is_err()
                {
                    return;
                }
            }
        }
    }
}

/// The hit that calls the assistant in the utterance from sample `start` to
/// `end`, taken out of `hits` with every other one before `end`: the first
/// at most [`CALLED_WITHIN_MS`] into it, reported up to [`HIT_AFTER`] after
/// it ends. One later than that is somebody talking about the assistant --
/// or, after an utterance cut at its longest and the next begun straight
/// after, the next one's, and is left for it.
fn take_hit(hits: &mut VecDeque<Hit>, start: u64, end: u64) -> Option<Hit> {
    let called_by = start + pcm::sample_count(CALLED_WITHIN_MS) as u64;
    let last = (end + HIT_AFTER).min(called_by);
    let hit = hits.iter().copied().find(|h| h.at >= start && h.at <= last);
    hits.retain(|h| h.at > end && Some(h.at) != hit.map(|hit| hit.at));
    hit
}

/// The name heard: where, in samples since listening began, and how sure
/// of it the spotter grew.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Hit {
    at: u64,
    peak: f32,
}

/// What was said after the name in `samples`, an utterance the spotter heard
/// it in `at` samples from its start; `None` when it was not the assistant
/// being called after all.
///
/// Mostly decided by when the name was heard. Late in the utterance, it was
/// talked about. With nothing after it, it was said alone, and nothing need
/// be written down at all -- unless the owner's voice check is on
/// (`refusing`), when it is, and nothing written is somebody else. With more
/// after it, the whole utterance is heard: the name written at its start --
/// the recogniser is told to expect it -- gives the request by the
/// recogniser's own reckoning, and written later on it is the name talked
/// about. A recogniser that wrote it some other way ("Jet a you") hears what
/// came after the name on its own instead -- and so does one that wrote
/// nothing like it, when the spotter was [`SURE`] of it (`sure`). Otherwise
/// the spotter alone heard it, and whatever was said goes nowhere.
async fn after_name(
    transcriber: &dyn UtteranceTranscriber,
    samples: &[i16],
    at: usize,
    names: &[String],
    sure: bool,
    refusing: bool,
) -> Option<String> {
    if pcm::duration_ms(at) > CALLED_WITHIN_MS {
        return None;
    }
    let after = &samples[at.min(samples.len())..];
    let alone = pcm::duration_ms(after.len()) < NOTHING_AFTER_MS && !syllable_after(samples, at);
    if alone && !refusing {
        return Some(String::new());
    }
    let words = match transcriber.hear(samples).await {
        Ok(words) => words,
        Err(e) => {
            tracing::debug!(error = %e.message, "wake word: an utterance was not transcribed");
            return None;
        }
    };
    // Nothing written: with the owner's voice checked, somebody else; and
    // nothing to go on but a spotter not sure of the name.
    if words.trim().is_empty() && (refusing || !sure) {
        return None;
    }
    if alone {
        return Some(String::new());
    }
    if let Some(rest) = called(&words, names) {
        return Some(rest);
    }
    let opens_like = opens_like_name(&words, names);
    if mentions(&words, names) || !(sure || opens_like) {
        return None;
    }
    // The name left out altogether ("Take a note."): what was written is
    // what was asked. Parakeet wrote nothing at all for some of what came
    // after the hit, heard on its own, that it wrote right heard whole.
    if !opens_like && !words.trim().is_empty() {
        return Some(without_greeting(&words));
    }
    let rest = transcriber.hear(after).await.ok()?;
    Some(without_name_tail(&rest, names))
}

/// `transcript` without a greeting or filler it begins with: "take a note"
/// from "Hey, take a note".
fn without_greeting(transcript: &str) -> String {
    let rest = match words_of(transcript).first() {
        Some(first)
            if GREETINGS.contains(&first.text.as_str())
                || FILLERS.contains(&first.text.as_str()) =>
        {
            &transcript[first.end..]
        }
        _ => transcript,
    };
    rest.trim_start_matches(|c: char| c.is_whitespace() || is_joining_mark(c))
        .trim_end()
        .to_string()
}

/// What came after one of `names` at the start of `transcript`, as [`heard`]
/// finds it, or after one of [`FILLERS`].
fn called(transcript: &str, names: &[String]) -> Option<String> {
    heard(transcript, names).or_else(|| {
        let first = words_of(transcript).into_iter().next()?;
        if !FILLERS.contains(&first.text.as_str()) {
            return None;
        }
        let rest = transcript[first.end..]
            .trim_start_matches(|c: char| c.is_whitespace() || is_joining_mark(c));
        heard(rest, names)
    })
}

/// Whether one of `names` is written anywhere in `transcript`.
fn mentions(transcript: &str, names: &[String]) -> bool {
    let words = words_of(transcript);
    names.iter().any(|name| {
        let name: Vec<String> = words_of(name).into_iter().map(|w| w.text).collect();
        !name.is_empty()
            && words
                .windows(name.len())
                .any(|said| said.iter().zip(&name).all(|(s, n)| sounds_alike(&s.text, n)))
    })
}

/// Whether `transcript` opens with what a recogniser that does not know one
/// of `names` might write for it: near it run together
/// ([`starts_like_name`]), or with a word that sounds like its end among the
/// first few ([`OPENING_WORDS`]) -- the "you" of "Jatayu", which Parakeet
/// kept when it wrote the rest as other words: "Got our, you", "Jot are
/// you?", "Hi Jetter, you". Not "Jacuzzi, set the timer for an hour": what
/// the spotter took for the name was another word.
fn opens_like_name(transcript: &str, names: &[String]) -> bool {
    starts_like_name(transcript, names)
        || words_of(transcript)
            .iter()
            .take(OPENING_WORDS)
            .any(|word| is_name_tail(&word.text, names))
}

/// How many words a name a recogniser wrote as other words opens with: a
/// greeting, and the name in up to three.
const OPENING_WORDS: usize = 4;

/// Whether `word` sounds like the end of one of `names` -- two letters or
/// more of its last word, as it sounds, but not all of it: "you", "yu" or
/// "tayu" for "Jatayu".
fn is_name_tail(word: &str, names: &[String]) -> bool {
    let said = sound_of(word).replace("ou", "u");
    names.iter().any(|name| {
        words_of(name).last().is_some_and(|last| {
            let last = sound_of(&last.text);
            said.len() >= 2 && said.len() < last.len() && last.ends_with(&said)
        })
    })
}

/// Whether `transcript` begins -- after a greeting or a filler at most --
/// with one to three words that, run together, sound within a letter in
/// three of one of `names`: "Jet a you", "Jatai you".
fn starts_like_name(transcript: &str, names: &[String]) -> bool {
    let words = words_of(transcript);
    let starts = match words.first() {
        Some(first)
            if GREETINGS.contains(&first.text.as_str())
                || FILLERS.contains(&first.text.as_str()) =>
        {
            0..=1
        }
        _ => 0..=0,
    };
    names.iter().any(|name| {
        let name = sound_of(&words_of(name).into_iter().map(|w| w.text).collect::<String>());
        let allowed = name.chars().count() / 3;
        starts.clone().any(|from| {
            (1..=3).any(|n| {
                words.get(from..from + n).is_some_and(|said| {
                    let said = sound_of(&said.iter().map(|w| w.text.as_str()).collect::<String>());
                    edits(&said, &name) <= allowed
                })
            })
        })
    })
}

/// How many letters put in, taken out or changed make `a` into `b`.
fn edits(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let next = (diagonal + usize::from(ca != cb)).min(row[j] + 1).min(row[j + 1] + 1);
            diagonal = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// `rest` without a first word that is only the end of a name, cut off where
/// the spotter heard it: "you, what's the time" from "Jatayu, what's the
/// time" heard from the "yu".
fn without_name_tail(rest: &str, names: &[String]) -> String {
    let words = words_of(rest);
    let Some(first) = words.first() else { return rest.trim().to_string() };
    if !is_name_tail(&first.text, names) {
        return rest.trim().to_string();
    }
    rest[first.end..]
        .trim_start_matches(|c: char| c.is_whitespace() || is_joining_mark(c))
        .trim_end()
        .to_string()
}

/// Whether another syllable begins after sample `at` of `samples`: the
/// loudness of its 20 ms stretches falls under a fifth of the utterance's
/// loudest and then rises past a third of it -- a vowel after the name's
/// last. The name's own fading end, after a step that heard it a little
/// early, only falls, and so does the quiet the detector keeps after speech.
/// For a short word said straight after the name -- "Jatayu, stop" -- that
/// the spotter heard late.
fn syllable_after(samples: &[i16], at: usize) -> bool {
    let frame = pcm::sample_count(20).max(1);
    let loudness: Vec<f64> = samples
        .chunks(frame)
        .map(|c| (c.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / c.len() as f64).sqrt())
        .collect();
    let loudest = loudness.iter().copied().fold(0.0, f64::max);
    if loudest <= 0.0 {
        return false;
    }
    let mut fell = false;
    for &level in loudness.iter().skip(at / frame) {
        if level < loudest / 5.0 {
            fell = true;
        } else if fell && level > loudest / 3.0 {
            return true;
        }
    }
    false
}

impl Service {
    /// Listening for `names` with the built-in model, when it hears every one
    /// of them; `None` when it does not, for the recogniser to listen.
    pub(super) async fn wake_spotted(
        self: &Arc<Self>,
        names: &[String],
    ) -> CommandResult<Option<WakeLink>> {
        let Some(model) = spotter::Model::jatayu()
            .ok()
            .filter(|model| !names.is_empty() && names.iter().all(|name| model.hears(name)))
        else {
            return Ok(None);
        };
        let (detector, transcriber) = self
            .listening_parts(Tuning::CONVERSATION, Occasion::Wake, HeardFrom::ThisComputer, |t| t)
            .await?;
        // Loaded now, as the recogniser path loads it, so the first call is
        // answered as quickly as later ones. It is let go when it sits idle,
        // and loaded again as the next call is being said.
        transcriber.warm();
        let refusing = self.owner_check_on().await;
        Ok(Some(spawn_spotted(model, detector, transcriber, names.to_vec(), refusing)))
    }

    /// Whether, from moment to moment, what this computer hears is checked
    /// for the owner's voice: Settings' switch is on and their voice is
    /// recorded to check against (see `access::checked`).
    async fn owner_check_on(self: &Arc<Self>) -> Refusing {
        let recorded = matches!(self.owner_voice().await, Ok(Some(v)) if !v.centroids.is_empty());
        let check = self.voice_check();
        Arc::new(move || recorded && check.lock().unwrap_or_else(|e| e.into_inner()).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CommandResult;
    use crate::speech::transcribe::BoxFuture;

    fn names(of: &[&str]) -> Vec<String> {
        of.iter().map(|n| n.to_string()).collect()
    }

    /// A recogniser that writes `whole` for the utterance it is first given
    /// and `after` for anything shorter, and counts what it is asked.
    struct TwoPart {
        whole: &'static str,
        after: &'static str,
        length: usize,
        asked: std::sync::Mutex<Vec<usize>>,
    }

    impl UtteranceTranscriber for TwoPart {
        fn hear<'a>(&'a self, samples: &'a [i16]) -> BoxFuture<'a, CommandResult<String>> {
            self.asked.lock().unwrap().push(samples.len());
            let text = if samples.len() == self.length { self.whole } else { self.after };
            Box::pin(async move { Ok(text.to_string()) })
        }
    }

    /// What `after_name` makes of a 3 s utterance the spotter heard the name
    /// in `at_ms` into, not [`SURE`] of it, and how many times it asked the
    /// recogniser.
    async fn after(
        whole: &'static str,
        after_it: &'static str,
        at_ms: u64,
        refusing: bool,
    ) -> (Option<String>, usize) {
        after_in(3_000, whole, after_it, at_ms, refusing).await
    }

    /// [`after`] 900 ms in, with the spotter sure of the name.
    async fn after_sure(whole: &'static str, after_it: &'static str) -> (Option<String>, usize) {
        let samples = vec![0; pcm::sample_count(3_000)];
        let length = samples.len();
        let t = TwoPart { whole, after: after_it, length, asked: Default::default() };
        let at = pcm::sample_count(900);
        let rest = after_name(&t, &samples, at, &names(&["Jatayu"]), true, false).await;
        let asked = t.asked.lock().unwrap().len();
        (rest, asked)
    }

    async fn after_in(
        length_ms: u64,
        whole: &'static str,
        after_it: &'static str,
        at_ms: u64,
        refusing: bool,
    ) -> (Option<String>, usize) {
        heard_in(vec![0; pcm::sample_count(length_ms)], whole, after_it, at_ms, refusing).await
    }

    async fn heard_in(
        samples: Vec<i16>,
        whole: &'static str,
        after_it: &'static str,
        at_ms: u64,
        refusing: bool,
    ) -> (Option<String>, usize) {
        let t =
            TwoPart { whole, after: after_it, length: samples.len(), asked: Default::default() };
        let at = pcm::sample_count(at_ms);
        let rest = after_name(&t, &samples, at, &names(&["Jatayu"]), false, refusing).await;
        let asked = t.asked.lock().unwrap().len();
        (rest, asked)
    }

    /// `ms` of a vowel-like tone at `level`, then `gap_ms` of silence.
    fn sound(out: &mut Vec<i16>, ms: u64, level: f64, gap_ms: u64) {
        let n = pcm::sample_count(ms);
        out.extend((0..n).map(|i| (level * (i as f64 * 0.12).sin()) as i16));
        out.extend(std::iter::repeat_n(0, pcm::sample_count(gap_ms)));
    }

    #[tokio::test]
    async fn the_name_written_at_the_start_gives_what_followed_it() {
        let (rest, asked) = after("Hey Jatayu, what's the time?", "unused", 900, false).await;
        assert_eq!(rest.as_deref(), Some("what's the time?"));
        assert_eq!(asked, 1, "heard once");
        // After the words the model was trained to hear before it, too.
        let rest = after("So Jatayu, what's on today?", "unused", 900, false).await.0;
        assert_eq!(rest.as_deref(), Some("what's on today?"));
        let rest = after("Um, Jatayu. Read my mail.", "unused", 900, false).await.0;
        assert_eq!(rest.as_deref(), Some("Read my mail."));
    }

    #[tokio::test]
    async fn the_name_talked_about_wakes_nothing() {
        // Written after other words.
        assert_eq!(
            after("I asked Jatayu about it yesterday", "about it", 1_200, false).await.0,
            None
        );
        // Heard late in what was said: not even written down.
        assert_eq!(after("unused", "unused", 2_500, false).await, (None, 0));
    }

    #[tokio::test]
    async fn a_name_the_recogniser_misspelt_gives_what_came_after_the_spotter_heard_it() {
        let (rest, asked) =
            after("Jet a you, what's the time?", "you, what's the time?", 900, false).await;
        assert_eq!(rest.as_deref(), Some("what's the time?"));
        assert_eq!(asked, 2);
        // A first word that is not the name's end is the request's own.
        let rest = after("Jet a you, what's on today?", "what's on today?", 900, false).await.0;
        assert_eq!(rest.as_deref(), Some("what's on today?"));
        let rest = after("Hey, jet a you, take a note", "take a note", 900, false).await.0;
        assert_eq!(rest.as_deref(), Some("take a note"));
    }

    #[tokio::test]
    async fn what_the_spotter_alone_took_for_the_name_goes_nowhere() {
        // Another word where the name should be: somebody else's sentence.
        assert_eq!(
            after("Jacuzzi, set the timer for an hour", "set the timer for an hour", 900, false)
                .await,
            (None, 1)
        );
        // Nothing written for any of it.
        assert_eq!(after("", "never mind", 900, false).await, (None, 1));
        // The name left out, the request written: the spotter is not sure
        // enough to go on alone.
        assert_eq!(after("Take a note.", "Take a note.", 900, false).await, (None, 1));
    }

    #[tokio::test]
    async fn a_spotter_sure_of_the_name_needs_no_recogniser_to_agree() {
        // The name left out: what was written is what was asked.
        assert_eq!(
            after_sure("Take a note.", "unused").await,
            (Some("Take a note.".to_string()), 1)
        );
        assert_eq!(
            after_sure("Hey, take a note.", "unused").await.0.as_deref(),
            Some("take a note.")
        );
        // Written as nothing at all: what came after the hit, heard alone.
        assert_eq!(after_sure("", "never mind").await, (Some("never mind".to_string()), 2));
        // Written as something like it: what came after it, heard alone.
        let rest = after_sure("Jot are you? What did I miss?", "you, what did I miss?").await.0;
        assert_eq!(rest.as_deref(), Some("what did I miss?"));
        // Still not when the name was written later on: talked about.
        assert_eq!(after_sure("I asked Jatayu about it", "about it").await.0, None);
    }

    #[tokio::test]
    async fn the_name_alone_is_a_call_with_nothing_asked_and_nothing_written() {
        // "Hey Jatayu", and the 0.2 s of quiet the detector keeps.
        assert_eq!(
            after_in(1_300, "unused", "unused", 1_050, false).await,
            (Some(String::new()), 0)
        );
        // Heard a little early: only the name's own end fades after the hit.
        let mut samples = Vec::new();
        sound(&mut samples, 200, 8_000.0, 60);
        sound(&mut samples, 200, 9_000.0, 60);
        sound(&mut samples, 250, 8_000.0, 200);
        let at = pcm::duration_ms(samples.len()) - 300;
        assert_eq!(
            heard_in(samples, "unused", "unused", at, false).await,
            (Some(String::new()), 0)
        );
    }

    #[tokio::test]
    async fn a_short_word_straight_after_a_late_hit_is_written_down() {
        // "Jatayu, stop", the spotter's step a while after the name's end:
        // little audio after it, but a syllable begun in it.
        let mut samples = Vec::new();
        sound(&mut samples, 200, 8_000.0, 60);
        sound(&mut samples, 200, 9_000.0, 60);
        sound(&mut samples, 200, 8_000.0, 60);
        let at = pcm::duration_ms(samples.len()) - 20;
        sound(&mut samples, 120, 7_000.0, 200);
        assert!(pcm::duration_ms(samples.len() - pcm::sample_count(at)) < NOTHING_AFTER_MS);
        let (rest, asked) = heard_in(samples, "Jatayu, stop.", "unused", at, false).await;
        assert_eq!(rest.as_deref(), Some("stop."));
        assert_eq!(asked, 1);
    }

    #[tokio::test]
    async fn with_the_owners_voice_checked_the_name_alone_must_be_written_too() {
        // The check writes nothing for somebody else.
        assert_eq!(after_in(1_300, "", "unused", 1_050, true).await, (None, 1));
        assert_eq!(
            after_in(1_300, "Jatayu.", "unused", 1_050, true).await,
            (Some(String::new()), 1)
        );
        assert_eq!(after("", "what's the time", 900, true).await.0, None);
    }

    #[test]
    fn the_end_of_the_name_is_taken_off_and_nothing_else() {
        let n = names(&["Jatayu"]);
        assert_eq!(without_name_tail("yu, type hello", &n), "type hello");
        assert_eq!(without_name_tail("tayu type hello", &n), "type hello");
        assert_eq!(without_name_tail("you type hello", &n), "type hello");
        assert_eq!(without_name_tail("type hello", &n), "type hello");
        assert_eq!(
            without_name_tail("a type hello", &n),
            "a type hello",
            "one letter is not a tail"
        );
        assert_eq!(without_name_tail("", &n), "");
    }

    #[test]
    fn what_opens_like_the_name_is_near_it_or_ends_as_it_does() {
        let n = names(&["Jatayu"]);
        for said in ["Jet a you, hi", "Chatayu what", "hey jatai you", "um, jataiyu", "Jathayu"] {
            assert!(starts_like_name(said, &n), "{said}");
        }
        for said in ["Jacuzzi, set the timer", "What a view", "data", "I asked Jet a you", ""] {
            assert!(!starts_like_name(said, &n), "{said}");
        }
        // As Parakeet wrote it for voices it did not know.
        for said in [
            "Hey, Jedar, you any deck for some little time.",
            "You. Never mind.",
            "Got our, you, add milk to my shopping list.",
            "Heyja, taught you, stop.",
            "Jot are you? What did I miss?",
            "Hi Jetter, you. How long is my commute?",
        ] {
            assert!(opens_like_name(said, &n), "{said}");
        }
        for said in
            ["Jacuzzi, set the timer for an hour", "Take a note.", "It was then that you left", ""]
        {
            assert!(!opens_like_name(said, &n), "{said}");
        }
        assert_eq!(edits("jetayou", "jatayu"), 2);
        assert_eq!(edits("", "abc"), 3);
        assert_eq!(edits("abc", "abc"), 0);
    }

    #[test]
    fn a_hit_belongs_to_the_utterance_it_calls_from() {
        let second = SAMPLE_RATE as u64;
        // Early in it, or reported a moment after it ended: taken, with
        // everything else up to its end.
        let at = |hits: &[u64]| -> VecDeque<Hit> {
            hits.iter().map(|&at| Hit { at, peak: 0.95 }).collect()
        };
        let taken = |hit: Option<Hit>| hit.map(|h| h.at);
        let mut hits = at(&[second / 2, 2 * second]);
        assert_eq!(taken(take_hit(&mut hits, second, 3 * second)), Some(2 * second));
        assert!(hits.is_empty());
        let mut hits = at(&[3 * second + second / 5]);
        let hit = take_hit(&mut hits, 2 * second, 3 * second);
        assert_eq!(taken(hit), Some(3 * second + second / 5));
        // Late in it: talked about.
        let mut hits = at(&[5 * second]);
        assert_eq!(take_hit(&mut hits, 0, 10 * second), None);
        assert!(hits.is_empty());
        // Just after a 30 s utterance cut at its longest: the next one's.
        let mut hits = at(&[30 * second + second / 4]);
        assert_eq!(take_hit(&mut hits, 0, 30 * second), None);
        let hit = take_hit(&mut hits, 30 * second, 33 * second);
        assert_eq!(taken(hit), Some(30 * second + second / 4));
    }
}
