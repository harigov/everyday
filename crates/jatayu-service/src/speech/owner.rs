//! Whether what this computer's microphone heard was said by the person it
//! listens for, and only their part of it.
//!
//! Switched on in Settings (the app's `microphone.rs`), every utterance a
//! detector cuts from the microphone is compared with the owner's voiceprint
//! before a recogniser hears it, by the same speaker-embedding model meeting
//! notes tell voices apart with ([`SpeechKit::embed`]). Somebody else's --
//! the television, a podcast, a colleague across the room -- is never
//! transcribed, and so never typed, never answered and never wakes the
//! assistant.
//!
//! # How it decides
//!
//! With a television on, the detector hears no gap between its talk and the
//! owner's and cuts one long utterance of both, so a whole utterance is not
//! the whole story. Three cases, by how like the owner the whole of it is:
//!
//! - **Plainly theirs** ([`Strictness::sure`]): heard whole.
//! - **Theirs, with something else** (over [`Strictness::utterance`]): a
//!   long one is looked at again a slice at a time ([`WINDOW_MS`] every
//!   [`HOP_MS`]), and what comes before the first stretch that sounds like
//!   the owner and after the last is cut -- the television they talked over
//!   the end of, or that carried on after them. Never anything between, so
//!   none of their own words is cut out of the middle.
//! - **Not theirs as a whole**: a long one is sliced the same way and only
//!   the stretches that sound like the owner are heard, joined by a short
//!   silence. None, and nothing is heard.
//!
//! An utterance too short to embed -- a lone "yes", the assistant's name on
//! its own -- is heard: the model cannot say whose it is, and a word the
//! person said going missing is worse than a stranger's "yes".
//!
//! # The numbers
//!
//! Measured on Hari's own recording against four podcasts
//! (`docs/plans/mic-filtering.md`): his utterances came out 0.32 to 0.57
//! from his voiceprint, the podcasts' -0.07 to 0.24. Slices of 1.5 s: 90%
//! of his at least 0.22, 95% of the podcasts' at most 0.21 -- close, so a
//! slice is heard only with a neighbour that agrees ([`MIN_RUN`]), over a
//! bar a little higher than that, and clear of the utterance's own score
//! ([`ABOVE_WHOLE`]). The
//! [`Strictness`] chosen in Settings moves both bars together, since a
//! microphone and a room unlike the one a voiceprint was recorded in score
//! lower.
//!
//! # The voice sample
//!
//! Taking other voices out of the microphone ([`super::cleanup`]) needs the
//! owner's voice itself, not an embedding of it, so recording a voiceprint
//! keeps its most voiced [`MAX_VOICE_SAMPLE_SECONDS`] as well
//! ([`best_stretch`], [`encode_sample`]).

use super::pcm;
#[cfg(feature = "speech")]
use crate::error::CommandResult;
use crate::service::Service;
#[cfg(feature = "speech")]
use crate::service::blocking;
use jatayu_core::meeting::{MAX_VOICE_SAMPLE_SECONDS, SAMPLE_RATE, Voiceprint};
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::sync::Arc;

/// How alike an utterance must be to the owner's voice to be heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Strictness {
    /// For a microphone or a room unlike the one the voice was recorded in:
    /// fewer of the owner's words lost, more of other people's heard.
    Lenient,
    #[default]
    Balanced,
    /// Where other people talk a lot and the owner's voice was recorded on
    /// this microphone.
    Strict,
}

impl Strictness {
    /// The similarity past which a whole utterance is plainly the owner's and
    /// is heard whole without a closer look. Hari's own long utterances came
    /// out 0.48 to 0.57; one with a podcast running through it, 0.33.
    fn sure(self) -> f32 {
        match self {
            Strictness::Lenient => 0.35,
            Strictness::Balanced => 0.45,
            Strictness::Strict => 0.50,
        }
    }

    /// The least similarity, to the nearest of the owner's centroids, at
    /// which a whole utterance holds them at all.
    fn utterance(self) -> f32 {
        match self {
            Strictness::Lenient => 0.20,
            Strictness::Balanced => 0.28,
            Strictness::Strict => 0.36,
        }
    }

    /// The same, for a slice of [`WINDOW_MS`]: lower, as a shorter stretch
    /// of anybody's voice scores lower. A slice must also stand
    /// [`ABOVE_WHOLE`] over the utterance it is in.
    fn window(self) -> f32 {
        match self {
            Strictness::Lenient => 0.17,
            Strictness::Balanced => 0.24,
            Strictness::Strict => 0.30,
        }
    }
}

/// Shorter than this, an utterance is not judged: see the module doc.
pub const MIN_JUDGED_MS: u64 = 1_000;

/// An utterance this long that does not sound like the owner as a whole is
/// looked at a slice at a time.
pub const SLICED_FROM_MS: u64 = 3_000;

/// One slice.
pub const WINDOW_MS: u64 = 1_500;

/// How far apart slices start.
pub const HOP_MS: u64 = 500;

/// The least of the owner, in total, a sliced utterance must hold to be
/// heard at all.
pub const MIN_KEPT_MS: u64 = 1_000;

/// How far a slice must score over the whole utterance it is in. Somebody
/// else's talk wanders around its own average, slice to slice, and now and
/// then over the bar; the owner's voice inside it stands well clear of the
/// average the other voice pulls down.
pub const ABOVE_WHOLE: f32 = 0.06;

/// How many slices in a row must sound like the owner for their stretch to
/// be heard. One in twenty of a podcast's slices scored over the bar on its
/// own, so a long utterance of somebody else's would let a fragment through
/// now and then; two neighbours agreeing, a second's worth of new audio
/// between them, it would not.
pub const MIN_RUN: usize = 2;

/// The silence put between two stretches heard of one utterance.
const JOIN_MS: u64 = 250;

/// The owner's voice as the microphone's listeners need it.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnerVoice {
    /// Embeddings of it, by the speech kit's embedding model.
    pub centroids: Vec<Vec<f32>>,
    /// Their voice itself, at [`SAMPLE_RATE`], for taking other voices out.
    pub sample: Option<Vec<i16>>,
}

impl OwnerVoice {
    pub fn of(voiceprint: &Voiceprint) -> Self {
        Self {
            centroids: voiceprint.centroids.clone(),
            sample: voiceprint.voice_sample.as_deref().and_then(decode_sample),
        }
    }

    /// How alike `embedding` is to the nearest of the owner's centroids.
    pub fn similarity(&self, embedding: &[f32]) -> f32 {
        self.centroids
            .iter()
            .map(|c| jatayu_core::meeting::identify::cosine(c, embedding))
            .fold(f32::MIN, f32::max)
    }
}

/// `samples` as [`Voiceprint::voice_sample`] keeps them.
pub fn encode_sample(samples: &[i16]) -> String {
    use base64::Engine;
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// What [`encode_sample`] made, or `None` for anything else.
pub fn decode_sample(text: &str) -> Option<Vec<i16>> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(text.trim()).ok()?;
    if bytes.len() % 2 != 0 || bytes.is_empty() {
        return None;
    }
    Some(bytes.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect())
}

/// The most voiced [`MAX_VOICE_SAMPLE_SECONDS`] of a recording of the owner:
/// the stretch with the most windows that have a voice's pitch in them
/// ([`super::voicing`]), the loudest of those breaking a tie. All of it when
/// it is no longer than that.
pub fn best_stretch(clip: &[i16]) -> &[i16] {
    let length = (MAX_VOICE_SAMPLE_SECONDS * SAMPLE_RATE) as usize;
    if clip.len() <= length {
        return clip;
    }
    let window = super::voicing::WINDOW;
    let scaled = pcm::to_f32(clip);
    // Per voicing window: whether it is voiced, and its energy.
    let marks: Vec<(bool, f64)> = scaled
        .chunks_exact(window)
        .map(|w| {
            let energy = w.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>();
            (super::voicing::is_voiced(w), energy)
        })
        .collect();
    let per_stretch = length / window;
    let mut best = (0usize, 0.0f64, 0usize);
    let (mut voiced, mut energy) = (0usize, 0.0f64);
    for (i, (v, e)) in marks.iter().enumerate() {
        voiced += usize::from(*v);
        energy += e;
        if i >= per_stretch {
            let (old_v, old_e) = marks[i - per_stretch];
            voiced -= usize::from(old_v);
            energy -= old_e;
        }
        if i + 1 >= per_stretch && (voiced, energy) > (best.0, best.1) {
            best = (voiced, energy, i + 1 - per_stretch);
        }
    }
    let start = best.2 * window;
    &clip[start..start + length]
}

/// What of an utterance to hear.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// All of it: it sounds like the owner, or is too short to tell.
    All,
    /// Only these stretches of it, in samples, in order and apart.
    Parts(Vec<Range<usize>>),
    /// None of it: somebody else's.
    Nothing,
}

/// The decision, from the similarities alone, so that it can be tested
/// without a model: `whole` is the whole utterance's (`None` when it was too
/// short to embed), `slices` each slice's range and similarity, over an
/// utterance of `len` samples. See the module doc.
pub fn judge(
    len: usize,
    whole: Option<f32>,
    slices: &[(Range<usize>, Option<f32>)],
    strictness: Strictness,
) -> Verdict {
    let Some(whole) = whole else { return Verdict::All };
    if len < pcm::sample_count(MIN_JUDGED_MS) || whole >= strictness.sure() {
        return Verdict::All;
    }
    if whole >= strictness.utterance() {
        // It holds the owner: trim what comes before the first stretch of
        // them and after the last, by the plain slice bar, so that their own
        // words, which a slice of them clears, are never trimmed as an edge.
        // With none found the slices could not tell, and the whole is heard.
        let parts = stretches(slices, strictness.window());
        return match (parts.first(), parts.last()) {
            (Some(first), Some(last)) if first.start > 0 || last.end < len => {
                Verdict::Parts(std::iter::once(first.start..last.end).collect())
            }
            _ => Verdict::All,
        };
    }
    let parts = stretches(slices, strictness.window().max(whole + ABOVE_WHOLE));
    let kept: usize = parts.iter().map(|r| r.len()).sum();
    if kept < pcm::sample_count(MIN_KEPT_MS) {
        return Verdict::Nothing;
    }
    if parts.len() == 1 && parts[0] == (0..len) {
        return Verdict::All;
    }
    Verdict::Parts(parts)
}

/// The stretches of an utterance that sound like the owner: runs of at least
/// [`MIN_RUN`] neighbouring slices at or over `bar`, overlapping ones joined.
fn stretches(slices: &[(Range<usize>, Option<f32>)], bar: f32) -> Vec<Range<usize>> {
    let mut runs: Vec<(Range<usize>, usize)> = Vec::new();
    let mut open = false;
    for (range, similarity) in slices {
        if !similarity.is_some_and(|s| s >= bar) {
            open = false;
            continue;
        }
        match runs.last_mut() {
            Some((last, count)) if open => {
                last.end = last.end.max(range.end);
                *count += 1;
            }
            _ => runs.push((range.clone(), 1)),
        }
        open = true;
    }
    let mut parts: Vec<Range<usize>> = Vec::new();
    for (range, _) in runs.into_iter().filter(|(_, count)| *count >= MIN_RUN) {
        match parts.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => parts.push(range),
        }
    }
    parts
}

/// Where the slices of an utterance of `len` samples fall: every
/// [`HOP_MS`], [`WINDOW_MS`] long, the last one ending at the end. None for
/// an utterance shorter than [`SLICED_FROM_MS`].
pub fn slices(len: usize) -> Vec<Range<usize>> {
    let (window, hop) = (pcm::sample_count(WINDOW_MS), pcm::sample_count(HOP_MS));
    if len < pcm::sample_count(SLICED_FROM_MS) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut start = 0;
    while start + window < len {
        out.push(start..start + window);
        start += hop;
    }
    out.push(len - window..len);
    out
}

/// `samples` as `verdict` says to hear them: the stretches joined by
/// [`JOIN_MS`] of silence. Empty for [`Verdict::Nothing`].
pub fn heard_part(samples: &[i16], verdict: &Verdict) -> Vec<i16> {
    match verdict {
        Verdict::All => samples.to_vec(),
        Verdict::Nothing => Vec::new(),
        Verdict::Parts(parts) => {
            let mut out = Vec::new();
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    out.extend(std::iter::repeat_n(0i16, pcm::sample_count(JOIN_MS)));
                }
                out.extend_from_slice(
                    &samples[part.start.min(samples.len())..part.end.min(samples.len())],
                );
            }
            out
        }
    }
}

impl Service {
    /// Check the voice of what this computer's microphone hears from now on,
    /// as strictly as `strictness` says, or stop checking it (`None`). The
    /// app's setting. Streams already listening read it for every utterance,
    /// so it applies to the next thing heard, without their microphone
    /// opening again.
    pub fn set_voice_check(&self, strictness: Option<Strictness>) {
        *self.voice_check.lock().unwrap_or_else(|e| e.into_inner()) = strictness;
    }

    /// The setting, as the streams share it.
    #[cfg(feature = "speech")]
    pub(crate) fn voice_check(&self) -> VoiceCheck {
        self.voice_check.clone()
    }

    /// The owner's voice, from their voiceprint by this build's embedding
    /// model, or `None` when they have not recorded one. Refuses while the
    /// vault is locked.
    #[cfg(feature = "speech")]
    pub async fn owner_voice(self: &Arc<Self>) -> CommandResult<Option<OwnerVoice>> {
        let vault = self.require_unlocked()?;
        blocking(move || {
            Ok(vault
                .voiceprints()?
                .iter()
                .find(|v| v.is_owner && v.model == super::local::EMBEDDING_MODEL_ID)
                .map(OwnerVoice::of))
        })
        .await
    }
}

/// The owner's voice check as it stands, shared between the service and every
/// stream that checks: see [`Service::set_voice_check`].
pub type VoiceCheck = Arc<std::sync::Mutex<Option<Strictness>>>;

#[cfg(feature = "speech")]
pub use checked::OwnerOnly;

#[cfg(feature = "speech")]
mod checked {
    use super::*;
    use crate::speech::transcribe::BoxFuture;
    use crate::speech::utterance::UtteranceTranscriber;

    /// A transcriber that hears only the owner: [`judge`] before `inner`,
    /// as strictly as `check` says at the time, or not at all while it says
    /// nothing.
    pub struct OwnerOnly {
        pub inner: Arc<dyn UtteranceTranscriber>,
        pub voice: Arc<OwnerVoice>,
        pub check: VoiceCheck,
    }

    /// What of `samples` to hear. Blocking: it runs the embedding model, once
    /// for the whole utterance and, if that is not enough, once a slice.
    fn verdict(
        kit: &crate::speech::local::SpeechKit,
        voice: &OwnerVoice,
        strictness: Strictness,
        samples: &[i16],
    ) -> Verdict {
        if samples.len() < pcm::sample_count(MIN_JUDGED_MS) {
            return Verdict::All;
        }
        let whole = kit.embed(samples).map(|e| voice.similarity(&e));
        if whole.is_none_or(|s| s >= strictness.sure()) {
            return Verdict::All;
        }
        let slices: Vec<(Range<usize>, Option<f32>)> = slices(samples.len())
            .into_iter()
            .map(|r| {
                let similarity = kit.embed(&samples[r.clone()]).map(|e| voice.similarity(&e));
                (r, similarity)
            })
            .collect();
        judge(samples.len(), whole, &slices, strictness)
    }

    impl UtteranceTranscriber for OwnerOnly {
        fn hear<'a>(&'a self, samples: &'a [i16]) -> BoxFuture<'a, CommandResult<String>> {
            Box::pin(async move {
                let strictness = *self.check.lock().unwrap_or_else(|e| e.into_inner());
                let Some(strictness) = strictness else {
                    return self.inner.hear(samples).await;
                };
                let kit = crate::speech::local::speech_kit_async().await?;
                let voice = self.voice.clone();
                let owned = samples.to_vec();
                let judged = blocking(move || Ok(verdict(&kit, &voice, strictness, &owned)));
                // The words are worked out while the voice is: most of what
                // is heard is the owner's, and the check would otherwise add
                // its time to every answer. Thrown away when it is not.
                let (verdict, words) = tokio::join!(judged, self.inner.hear(samples));
                match verdict? {
                    Verdict::All => words,
                    Verdict::Nothing => {
                        tracing::debug!("speech: not the owner's voice");
                        Ok(String::new())
                    }
                    parts => {
                        tracing::debug!("speech: only part of it in the owner's voice");
                        self.inner.hear(&heard_part(samples, &parts)).await
                    }
                }
            })
        }

        fn warm(&self) {
            self.inner.warm();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> usize {
        pcm::sample_count(n)
    }

    #[test]
    fn an_utterance_that_sounds_like_the_owner_is_heard_whole() {
        assert_eq!(judge(ms(4_000), Some(0.4), &[], Strictness::Balanced), Verdict::All);
    }

    #[test]
    fn one_too_short_to_tell_is_heard_whole() {
        assert_eq!(judge(ms(800), Some(-0.1), &[], Strictness::Balanced), Verdict::All);
        assert_eq!(judge(ms(4_000), None, &[], Strictness::Balanced), Verdict::All);
    }

    #[test]
    fn somebody_else_is_not_heard() {
        let slices: Vec<_> = slices(ms(6_000)).into_iter().map(|r| (r, Some(0.05))).collect();
        assert_eq!(judge(ms(6_000), Some(0.02), &slices, Strictness::Balanced), Verdict::Nothing);
        // Too short to slice and not the owner: nothing.
        assert_eq!(judge(ms(2_000), Some(0.02), &[], Strictness::Balanced), Verdict::Nothing);
    }

    #[test]
    fn the_owner_inside_somebody_elses_talk_is_kept_alone() {
        let len = ms(8_000);
        // Slices whose start is in the last three seconds are the owner's.
        let slices: Vec<_> = slices(len)
            .into_iter()
            .map(|r| {
                let theirs = r.start >= ms(5_000);
                (r, Some(if theirs { 0.4 } else { 0.0 }))
            })
            .collect();
        let verdict = judge(len, Some(0.15), &slices, Strictness::Balanced);
        assert_eq!(verdict, Verdict::Parts(std::iter::once(ms(5_000)..len).collect()));
        let heard = heard_part(&vec![1i16; len], &verdict);
        assert_eq!(heard.len(), ms(3_000));
    }

    #[test]
    fn plainly_the_owner_is_not_looked_at_again() {
        let slices: Vec<_> = slices(ms(6_000)).into_iter().map(|r| (r, Some(0.0))).collect();
        assert_eq!(judge(ms(6_000), Some(0.5), &slices, Strictness::Balanced), Verdict::All);
    }

    #[test]
    fn the_television_before_and_after_the_owner_is_trimmed_but_not_their_pauses() {
        let len = ms(12_000);
        // The owner from 3 to 9 s, with a pause at 5-6 s that scores low.
        let slices: Vec<_> = slices(len)
            .into_iter()
            .map(|r| {
                let theirs = r.start >= ms(3_000) && r.end <= ms(9_000);
                let pause = r.start >= ms(4_500) && r.start < ms(6_000);
                (r, Some(if theirs && !pause { 0.45 } else { 0.1 }))
            })
            .collect();
        match judge(len, Some(0.32), &slices, Strictness::Balanced) {
            Verdict::Parts(parts) => {
                assert_eq!(parts.len(), 1, "one stretch, its pause kept: {parts:?}");
                assert_eq!(parts[0], ms(3_000)..ms(9_000));
            }
            other => panic!("expected the middle kept, got {other:?}"),
        }
    }

    #[test]
    fn one_slice_alone_is_not_enough() {
        let len = ms(10_000);
        // Every fourth slice of somebody else's talk happens to score high.
        let slices: Vec<_> = slices(len)
            .into_iter()
            .enumerate()
            .map(|(i, r)| (r, Some(if i % 4 == 0 { 0.3 } else { 0.05 })))
            .collect();
        assert_eq!(judge(len, Some(0.1), &slices, Strictness::Balanced), Verdict::Nothing);
    }

    #[test]
    fn two_stretches_of_the_owner_are_joined_by_a_short_silence() {
        let samples = vec![7i16; ms(10_000)];
        let verdict = Verdict::Parts(vec![0..ms(1_000), ms(5_000)..ms(7_000)]);
        let heard = heard_part(&samples, &verdict);
        assert_eq!(heard.len(), ms(1_000) + ms(JOIN_MS) + ms(2_000));
        assert!(heard[ms(1_000)..ms(1_000) + ms(JOIN_MS)].iter().all(|&s| s == 0));
    }

    #[test]
    fn strictness_moves_the_bar() {
        let len = ms(2_000);
        assert_eq!(judge(len, Some(0.25), &[], Strictness::Lenient), Verdict::All);
        assert_eq!(judge(len, Some(0.25), &[], Strictness::Balanced), Verdict::Nothing);
        assert_eq!(judge(len, Some(0.32), &[], Strictness::Balanced), Verdict::All);
        assert_eq!(judge(len, Some(0.32), &[], Strictness::Strict), Verdict::Nothing);
    }

    #[test]
    fn slices_cover_the_utterance_to_its_end() {
        assert!(slices(ms(2_000)).is_empty());
        let s = slices(ms(4_200));
        assert_eq!(s.first(), Some(&(0..ms(WINDOW_MS))));
        assert_eq!(s.last().map(|r| r.end), Some(ms(4_200)));
        assert!(s.iter().all(|r| r.len() == ms(WINDOW_MS)));
    }

    #[test]
    fn a_voice_sample_survives_being_kept() {
        let samples: Vec<i16> = (0..1000).map(|i| (i * 37 % 6000) as i16 - 3000).collect();
        assert_eq!(decode_sample(&encode_sample(&samples)), Some(samples));
        assert_eq!(decode_sample("not base64!"), None);
        assert_eq!(decode_sample(""), None);
    }

    /// A voiced stretch: harmonics of a wavering pitch.
    fn voiced(seconds: usize) -> Vec<i16> {
        let mut phase = 0.0f32;
        (0..seconds * SAMPLE_RATE as usize)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let pitch = 140.0 + 15.0 * (t * 1.3 * std::f32::consts::TAU).sin();
                phase += pitch / SAMPLE_RATE as f32 * std::f32::consts::TAU;
                let tone: f32 = (1..=10).map(|k| (phase * k as f32).sin() / k as f32).sum();
                (tone * 4000.0) as i16
            })
            .collect()
    }

    #[test]
    fn the_sample_kept_is_the_voiced_stretch_of_a_long_recording() {
        let mut clip = vec![0i16; 8 * SAMPLE_RATE as usize];
        clip.extend(voiced(MAX_VOICE_SAMPLE_SECONDS as usize));
        clip.extend(vec![0i16; 5 * SAMPLE_RATE as usize]);
        let best = best_stretch(&clip);
        assert_eq!(best.len(), (MAX_VOICE_SAMPLE_SECONDS * SAMPLE_RATE) as usize);
        let silent = best.iter().filter(|&&s| s == 0).count();
        assert!(silent < SAMPLE_RATE as usize / 2, "{silent} silent samples kept");
        let short = voiced(3);
        assert_eq!(best_stretch(&short).len(), short.len());
    }
}
