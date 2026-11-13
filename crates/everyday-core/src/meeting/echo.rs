//! Echo: the other side's voice reaching the microphone from the speakers.
//!
//! Without headphones the mic hears the call as well as the owner, so the
//! same words land on both tracks and every line of theirs would be in the
//! transcript twice, once in the owner's name. The system track is the clean
//! copy, so the mic's is the one dropped: [`is_echo`] decides, for
//! [`super::merge::remove_echo`].
//!
//! Whether there is any echo at all is decided from the audio, never from the
//! words. With headphones nothing of theirs reaches the mic, and a short reply
//! that repeats their words -- "yeah, I think so", a date read back -- is the
//! owner's own; judging every recording by its words dropped exactly those.
//! [`Envelopes`] keeps how loud each track was in 10 ms blocks, and
//! [`EchoCheck::analyse`] compares the two over 2 s windows while both carry
//! sound. Their voice from the speakers makes the mic move with the system
//! track, a little late, at the same lag every time; two people talking over
//! each other line up now and then, at random lags. So echo is a rate -- a
//! good share of the windows, at a steady lag -- never a count. Only with that
//! evidence does [`is_echo`] read a mic line's words, wanting more of them the
//! shorter the line, and never taking an acknowledgement. Once the room's lag
//! is known, a line's own stretch of audio has its say as well
//! ([`EchoCheck::over`]): the owner's voice is louder on the mic than the leak
//! ever is.
//!
//! The approach and its numbers follow Yogurt (github.com/FrigadeHQ/yogurt,
//! MIT): `asr/echoDetect.ts` for the audio, `asr/echo.ts` for the words.
//! Yogurt listens live over a ring buffer; here the whole recording is in
//! hand, so its windows are walked once, afterwards, and the lag search
//! reaches a little either side of zero, because the two tracks are two
//! capture streams whose starts need not line up.

use super::words::{shared, tokens};
use super::{SAMPLE_RATE, Track, TrackSegment};
use std::collections::BTreeMap;

/// One envelope value per this many milliseconds.
pub const BLOCK_MS: u64 = 10;

/// Samples in one block at [`SAMPLE_RATE`].
const BLOCK_SAMPLES: usize = (SAMPLE_RATE as u64 * BLOCK_MS / 1_000) as usize;

/// What an `i16` sample is divided by to put it on the -1.0..=1.0 scale.
const FULL_SCALE: f64 = 32_768.0;

/// The two envelopes are compared this many blocks (2 s) at a time: long
/// enough to hold a few words of theirs, short enough that a stretch of the
/// owner talking alone spoils only the window it falls in.
const WINDOW_BLOCKS: usize = 200;

/// [`WINDOW_BLOCKS`] in milliseconds, for finding the window holding a moment.
const WINDOW_MS: u64 = WINDOW_BLOCKS as u64 * BLOCK_MS;

/// The earliest lag searched, in blocks: the mic up to 100 ms ahead of the
/// system track. Sound from the speakers can only reach the mic after it was
/// played, so Yogurt starts at zero; here the two tracks come from separate
/// capture streams whose starts can be tens of milliseconds apart either way.
const MIN_LAG_BLOCKS: i64 = -10;

/// The latest lag searched, in blocks: the mic up to 300 ms behind, which
/// covers the speakers' output buffer, the air, and the mic's input buffer.
const MAX_LAG_BLOCKS: i64 = 30;

/// The system track is somebody talking from this RMS up, both for a
/// window's mean and for a single block (one of their "speaking" blocks).
const SYSTEM_LOUD: f32 = 0.004;

/// The mic holds something from this mean RMS up. Below it the window is a
/// quiet mic -- headphones, or a muted one -- and says nothing either way.
const MIC_SOME: f32 = 0.0006;

/// A window is compared only with this many speaking blocks of theirs in it:
/// a correlation over fewer is chance.
const MIN_SPEAKING: usize = 20;

/// The mic followed the system track at this Pearson correlation or above.
const CORRELATED: f64 = 0.6;

/// No verdict before this many windows where both tracks carried sound: a
/// few correlated windows early on prove nothing.
const MIN_ACTIVE: usize = 10;

/// Echo needs at least this share of the active windows correlated. People
/// talking over each other correlate now and then, never this often.
const MIN_RATE: f64 = 0.4;

/// From this share of the active windows correlated, echo is present at any
/// lags: a room has more than one path from the speakers to the mic, so the
/// lag can split in two.
const STRONG_RATE: f64 = 0.55;

/// Below [`STRONG_RATE`], echo needs this share of the correlated windows at
/// one steady lag -- chance correlations land at random lags.
const MIN_STEADY: f64 = 0.7;

/// A correlated window's lag counts as steady within this many blocks of the
/// most common one.
const STEADY_SLACK: i64 = 1;

/// A line is judged on its own audio only with this many speaking blocks of
/// theirs inside it: with their voice hardly there, there is nothing to leak.
const LINE_MIN_SPEAKING: usize = 20;

/// A lag behind this share of the correlated windows is one the room has
/// (a second path from the speakers), and a line is searched around it.
const KNOWN_LAG_SHARE: f64 = 0.2;

/// A line is searched this many blocks either side of each lag the room has:
/// one line is short, and its best lag wanders a little.
const LAG_SLACK: i64 = 2;

/// The mic over a whole line may be this many times as loud, next to their
/// track, as the leak usually is and still be judged by its words. Louder,
/// and the owner was talking: their own words, even if the other side's track
/// carries a copy of them.
const LOUD_RATIO: f64 = 2.0;

/// A line with nothing on the mic but their voice from the speakers follows
/// their track at least this closely.
const PURE_CORR: f64 = 0.8;

/// Such a line is also no louder, next to their track, than this many times
/// the leak's usual level: anything of the owner's on top would raise it.
const PURE_RATIO: f64 = 1.5;

/// And their voice is in at least this share of it, so the closeness is
/// measured over enough of the line to mean something.
const PURE_COVER: f64 = 0.4;

/// A mic line is compared with their lines that come within this much of it:
/// two recognisers cut a sentence at different pauses, and a chunk boundary
/// can slip.
const NEAR_MS: u64 = 1_500;

/// The speakers' tail: a mic line that starts later than this after their
/// line ended is the owner's own -- a read-back -- not their voice still
/// coming out of the speakers.
const TAIL_MS: u64 = 500;

/// A line of fewer words is too short to compare by its share of words; only
/// filler or a scrap of their line said at the same moment goes.
const MIN_TOKENS: usize = 3;

/// Lines of [`MIN_TOKENS`] up to this many words must match nearly whole
/// ([`SHORT_THRESHOLD`]): "I can do that" shares words with theirs too easily.
const MID_TOKENS: usize = 6;

/// Lines from [`MID_TOKENS`] up to this many words need [`MID_THRESHOLD`];
/// from here on, [`THRESHOLD`].
const LONG_TOKENS: usize = 8;

/// A long line is an echo with this share of its words in one line of theirs.
const THRESHOLD: f32 = 0.6;

/// A six or seven word line needs this share: a few shared words come easily
/// ("Sounds good, I can send it" against "yes I can send it so you have it").
const MID_THRESHOLD: f32 = 0.8;

/// A line under six words needs nearly all of them.
const SHORT_THRESHOLD: f32 = 0.95;

/// A long line judged against several of their lines at once needs this
/// share: common words add up over a longer stretch.
const JOINED_THRESHOLD: f32 = 0.75;

/// A line its own audio calls pure still needs this many of its words in
/// their line at that moment -- never dropped on the audio alone, since a
/// person who speaks as quietly as the leak sounds much the same.
const PURE_SHARED: usize = 3;

/// A scrap of one or two words goes only when their line overlapped it, give
/// or take this much.
const SCRAP_MS: u64 = 300;

/// The owner's own short replies: a line of fewer than [`MIN_TOKENS`] words
/// made only of these is never an echo, however well it fits their words.
const ACK: &[&str] = &[
    "yeah",
    "yes",
    "yep",
    "yup",
    "no",
    "nope",
    "right",
    "okay",
    "ok",
    "sure",
    "exactly",
    "true",
    "agreed",
    "fine",
    "great",
    "good",
    "thanks",
    "thank",
    "you",
    "please",
    "sorry",
    "absolutely",
    "definitely",
    "indeed",
    "correct",
    "cool",
    "perfect",
    "hi",
    "hello",
    "bye",
    "alright",
    "well",
    "so",
    "and",
    "but",
    "i",
    "me",
    "too",
    "also",
];

/// Loudness envelopes of both tracks across the whole recording: one RMS
/// value per [`BLOCK_MS`] block, on the -1.0..=1.0 scale (an `i16` sample
/// divided by 32768). A block nothing was recorded for is 0.0 -- silence.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Envelopes {
    pub mic: Vec<f32>,
    pub system: Vec<f32>,
}

impl Envelopes {
    /// Fold in one chunk of `track`, whose first sample is `start_ms` from
    /// the recording's start, at [`super::SAMPLE_RATE`]. Chunks can come in
    /// any order; a gap between them stays silence, and a short last block
    /// is the RMS of what it has.
    pub fn push(&mut self, track: Track, start_ms: u64, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        let envelope = match track {
            Track::Mic => &mut self.mic,
            Track::System => &mut self.system,
        };
        let first = (start_ms / BLOCK_MS) as usize;
        let end = first + samples.len().div_ceil(BLOCK_SAMPLES);
        if envelope.len() < end {
            envelope.resize(end, 0.0);
        }
        for (slot, block) in envelope[first..end].iter_mut().zip(samples.chunks(BLOCK_SAMPLES)) {
            *slot = rms(block);
        }
    }
}

/// The RMS of a non-empty block of samples, on the -1.0..=1.0 scale.
fn rms(block: &[i16]) -> f32 {
    let sum: f64 = block
        .iter()
        .map(|&s| {
            let x = f64::from(s) / FULL_SCALE;
            x * x
        })
        .sum();
    (sum / block.len() as f64).sqrt() as f32
}

/// What one mic line's own audio says -- see [`EchoCheck::over`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Moment {
    /// The mic followed the system track over the line, at the room's lag.
    pub correlated: bool,
    /// The mic was no louder over the line than the leak usually is.
    pub loud: bool,
    /// Nothing on the mic but their voice from the speakers.
    pub pure: bool,
}

/// One 2 s window in which the mic followed the system track.
#[derive(Debug, Clone, Copy)]
struct Hit {
    /// Which window: its start over [`WINDOW_MS`].
    window: usize,
    /// The lag it followed at, in blocks (mic later than system is positive).
    lag: i64,
    /// The mic's mean level over their speaking blocks, as a share of theirs:
    /// how loud the leak is in this room.
    ratio: f64,
}

/// The verdict on one recording: whether echo is present at all, and the
/// evidence a single line is judged against.
#[derive(Debug, Clone, Default)]
pub struct EchoCheck {
    /// Kept for judging single lines ([`EchoCheck::over`]).
    envelopes: Envelopes,
    /// Windows where both tracks carried sound.
    active: usize,
    /// The active windows where the mic followed the system track, in order.
    hits: Vec<Hit>,
    /// Made by [`EchoCheck::assume_present`]: no audio, echo taken as given.
    assumed: bool,
    /// What [`EchoCheck::settle`] works out once from `hits`, rather than
    /// again for every mic line judged: the verdict, the lags the room has,
    /// and the leak's usual level.
    settled: Settled,
}

/// See [`EchoCheck::settled`].
#[derive(Debug, Clone, Default)]
struct Settled {
    present: bool,
    known_lags: Vec<i64>,
    gain: f64,
}

impl EchoCheck {
    /// No evidence: headphones, or no audio to look at. Nothing is ever
    /// judged an echo against this.
    pub fn none() -> Self {
        Self::default()
    }

    /// Echo taken as present, with no audio to show for it.
    ///
    /// Every moment counts as one where the mic followed the system track,
    /// and no line has a verdict of its own audio, so [`is_echo`] judges by
    /// the words alone. For tests, and for a caller that knows the mic was
    /// open to the speakers but has no audio to look at.
    pub fn assume_present() -> Self {
        Self { assumed: true, ..Self::default() }.settle()
    }

    /// Look at the whole recording's envelopes: every whole 2 s window of the
    /// system track, against the mic at lags from -100 ms to +300 ms.
    pub fn analyse(envelopes: Envelopes) -> Self {
        let windows = envelopes.system.len() / WINDOW_BLOCKS;
        let mut active = 0;
        let mut hits = Vec::new();
        for window in 0..windows {
            match look(&envelopes, window) {
                Look::Quiet => {}
                Look::Active => active += 1,
                Look::Followed { lag, ratio } => {
                    active += 1;
                    hits.push(Hit { window, lag, ratio });
                }
            }
        }
        Self { envelopes, active, hits, assumed: false, settled: Settled::default() }.settle()
    }

    /// Work out, once, what every line is judged against.
    fn settle(mut self) -> Self {
        self.settled = Settled {
            present: self.assumed || self.heard(),
            known_lags: self.known_lags(),
            gain: self.gain(),
        };
        self
    }

    /// Echo was heard in this recording: enough active windows, a good share
    /// of them correlated, and -- unless most of them are -- at a steady lag.
    pub fn present(&self) -> bool {
        self.settled.present
    }

    /// See [`EchoCheck::present`].
    fn heard(&self) -> bool {
        if self.active < MIN_ACTIVE {
            return false;
        }
        let followed = self.hits.len();
        let rate = followed as f64 / self.active as f64;
        if rate < MIN_RATE {
            return false;
        }
        if rate >= STRONG_RATE {
            return true;
        }
        let Some(mode) = self.modal_lag() else {
            return false;
        };
        let steady = self.hits.iter().filter(|h| (h.lag - mode).abs() <= STEADY_SLACK).count();
        steady as f64 / followed as f64 >= MIN_STEADY
    }

    /// The mic followed the system track in the window holding `ms`.
    pub fn correlated_at(&self, ms: u64) -> bool {
        let window = (ms / WINDOW_MS) as usize;
        self.assumed || self.hits.binary_search_by_key(&window, |h| h.window).is_ok()
    }

    /// A mic line judged on its own audio, or `None` before the room's lag
    /// is known (or for a line past the end of either track).
    ///
    /// Over the line: does the mic follow their track at a lag the room has
    /// (`correlated`); is the mic, over the whole line, no louder than the
    /// leak usually is (`loud`); and is it so close a copy, at the leak's
    /// level, over enough of the line, that it sounds like nothing but their
    /// voice (`pure`)? With their voice hardly inside the line, none of
    /// these. The whole span is taken, pauses included: the owner's own words
    /// in the pauses pull the correlation down and the level up, as they
    /// should.
    pub fn over(&self, start_ms: u64, end_ms: u64) -> Option<Moment> {
        if self.assumed {
            return None;
        }
        let known = &self.settled.known_lags;
        let first = *known.first()?;
        let a = (start_ms / BLOCK_MS) as i64;
        let b = end_ms.div_ceil(BLOCK_MS) as i64;
        let inside = recorded(&self.envelopes.system, a) && recorded(&self.envelopes.mic, a);
        if !inside {
            return None;
        }
        let theirs: Vec<f64> = (a..b).map(|i| level(&self.envelopes.system, i)).collect();
        let speaking = theirs.iter().filter(|&&v| v >= f64::from(SYSTEM_LOUD)).count();
        if speaking < LINE_MIN_SPEAKING {
            return Some(Moment::default());
        }
        let mic_at = |lag: i64| -> Vec<f64> {
            (a..b).map(|i| level(&self.envelopes.mic, i + lag)).collect()
        };
        let (mut best, mut best_lag) = (0.0, first);
        for &room in known {
            let lags =
                (room - LAG_SLACK).max(MIN_LAG_BLOCKS)..=(room + LAG_SLACK).min(MAX_LAG_BLOCKS);
            for lag in lags {
                let c = pearson(&theirs, &mic_at(lag));
                if c > best {
                    best = c;
                    best_lag = lag;
                }
            }
        }
        let ratio = mean(&mic_at(best_lag)) / mean(&theirs);
        let gain = self.settled.gain;
        let cover = speaking as f64 / theirs.len() as f64;
        Some(Moment {
            correlated: best >= CORRELATED,
            loud: ratio <= LOUD_RATIO * gain,
            pure: best >= PURE_CORR && ratio <= PURE_RATIO * gain && cover >= PURE_COVER,
        })
    }

    /// How many correlated windows each lag has.
    fn lag_counts(&self) -> BTreeMap<i64, usize> {
        let mut counts: BTreeMap<i64, usize> = BTreeMap::new();
        for hit in &self.hits {
            *counts.entry(hit.lag).or_default() += 1;
        }
        counts
    }

    /// The most common lag of the correlated windows; between equals, the
    /// one seen first.
    fn modal_lag(&self) -> Option<i64> {
        let counts = self.lag_counts();
        let most = counts.values().copied().max()?;
        self.hits.iter().map(|h| h.lag).find(|lag| counts[lag] == most)
    }

    /// The lags the room has shown: each behind [`KNOWN_LAG_SHARE`] of the
    /// correlated windows.
    fn known_lags(&self) -> Vec<i64> {
        let followed = self.hits.len() as f64;
        self.lag_counts()
            .into_iter()
            .filter(|&(_, n)| n as f64 / followed >= KNOWN_LAG_SHARE)
            .map(|(lag, _)| lag)
            .collect()
    }

    /// The leak's usual level on the mic, as a share of their track's: the
    /// middle of the correlated windows' ratios.
    fn gain(&self) -> f64 {
        let mut ratios: Vec<f64> = self.hits.iter().map(|h| h.ratio).collect();
        ratios.sort_by(f64::total_cmp);
        ratios.get(ratios.len() / 2).copied().unwrap_or(0.0)
    }
}

/// What one 2 s window shows.
enum Look {
    /// One track or the other was quiet: no evidence either way.
    Quiet,
    /// Both carried sound, and the mic did not follow the system track.
    Active,
    /// The mic followed the system track at `lag` blocks, at `ratio` of its
    /// level.
    Followed { lag: i64, ratio: f64 },
}

/// One window of their track against the mic's, at every lag searched. Only
/// the blocks they are speaking in count: the owner's own words in the same
/// window would drown a faint leak.
fn look(envelopes: &Envelopes, window: usize) -> Look {
    let from = (window * WINDOW_BLOCKS) as i64;
    let to = from + WINDOW_BLOCKS as i64;
    let system: Vec<f64> = (from..to).map(|i| level(&envelopes.system, i)).collect();
    let mic: Vec<f64> = (from..to).map(|i| level(&envelopes.mic, i)).collect();
    if mean(&system) < f64::from(SYSTEM_LOUD) || mean(&mic) < f64::from(MIC_SOME) {
        return Look::Quiet;
    }
    let speaking: Vec<i64> = (from..)
        .zip(&system)
        .filter(|&(_, &v)| v >= f64::from(SYSTEM_LOUD))
        .map(|(i, _)| i)
        .collect();
    if speaking.len() < MIN_SPEAKING {
        return Look::Active;
    }
    let theirs: Vec<f64> = speaking.iter().map(|&i| level(&envelopes.system, i)).collect();
    let mut mine = Vec::with_capacity(speaking.len());
    let (mut best, mut best_lag) = (0.0, 0);
    for lag in MIN_LAG_BLOCKS..=MAX_LAG_BLOCKS {
        mine.clear();
        mine.extend(speaking.iter().map(|&i| level(&envelopes.mic, i + lag)));
        let c = pearson(&theirs, &mine);
        if c > best {
            best = c;
            best_lag = lag;
        }
    }
    if best < CORRELATED {
        return Look::Active;
    }
    mine.clear();
    mine.extend(speaking.iter().map(|&i| level(&envelopes.mic, i + best_lag)));
    Look::Followed { lag: best_lag, ratio: mean(&mine) / mean(&theirs) }
}

/// Block `index` of `envelope`; silence outside it.
fn level(envelope: &[f32], index: i64) -> f64 {
    usize::try_from(index).ok().and_then(|i| envelope.get(i)).map_or(0.0, |&v| f64::from(v))
}

/// Block `index` is inside `envelope`: something was recorded that far.
fn recorded(envelope: &[f32], index: i64) -> bool {
    usize::try_from(index).is_ok_and(|i| i < envelope.len())
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() { 0.0 } else { values.iter().sum::<f64>() / values.len() as f64 }
}

/// Pearson correlation of two equally long series; 0.0 when either is flat.
fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let (ma, mb) = (mean(a), mean(b));
    let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        let (dx, dy) = (x - ma, y - mb);
        cov += dx * dy;
        va += dx * dx;
        vb += dy * dy;
    }
    if va > 0.0 && vb > 0.0 { cov / (va * vb).sqrt() } else { 0.0 }
}

/// Is `mic` (a mic-track segment) a copy of what the system track said
/// around it? `system` is every system-track segment of the recording (any
/// other segment in it is ignored). Always `false` unless `check` says echo
/// is present, and for a segment that is not the mic's.
pub fn is_echo(mic: &TrackSegment, system: &[&TrackSegment], check: &EchoCheck) -> bool {
    if mic.track != Track::Mic || !check.present() {
        return false;
    }
    let theirs: Vec<&TrackSegment> =
        system.iter().copied().filter(|s| s.track == Track::System).collect();
    echo_match(mic, &theirs, |ms| check.correlated_at(ms), check.over(mic.start_ms, mic.end_ms))
}

/// The word rules, given echo is present.
///
/// `at` says whether the mic followed the system track in the window holding
/// a moment, `span` what the line's own audio says (see [`EchoCheck::over`]).
/// An echo is their voice on the mic while the line was said. The line's own
/// audio decides where it can: the mic followed their track over the line,
/// or in a window holding it (while the owner talked over them); and never
/// when the mic was far louder than the leak. Then the words: a long line at
/// 60% of its words in one of theirs, a six or seven word line at 80%, a
/// shorter one only nearly whole. A long line is also judged against their
/// nearby lines together, since two recognisers cut one sentence at different
/// pauses and its echo can straddle two of their lines.
fn echo_match(
    m: &TrackSegment,
    theirs: &[&TrackSegment],
    at: impl Fn(u64) -> bool,
    span: Option<Moment>,
) -> bool {
    let in_window = at(m.start_ms) || at(m.start_ms.midpoint(m.end_ms)) || at(m.end_ms);
    let heard = match span {
        Some(s) => (s.correlated || in_window) && s.loud,
        None => in_window,
    };
    if !heard {
        return false;
    }
    let mine = tokens(&m.text);
    let all_ack = mine.iter().all(|w| is_ack(w.as_str()));
    // Nothing on the mic but their voice, and their line at that moment has
    // most of its words: an echo at a lower bar than the word rules below.
    if let Some(s) = span
        && s.pure
        && mine.len() >= MIN_TOKENS
        && !all_ack
        && theirs.iter().any(|o| {
            o.start_ms < m.end_ms
                && o.end_ms > m.start_ms
                && shared(&m.text, &o.text) >= PURE_SHARED
                && share_of(&mine, &o.text) >= THRESHOLD
        })
    {
        return true;
    }
    // Their lines this one could be a copy of: close in time, and begun
    // before their line (and the speakers' tail) ended.
    let near: Vec<&TrackSegment> = theirs
        .iter()
        .copied()
        .filter(|o| {
            m.start_ms <= o.end_ms + NEAR_MS
                && m.end_ms + NEAR_MS >= o.start_ms
                && m.start_ms < o.end_ms + TAIL_MS
        })
        .collect();
    if mine.len() < MIN_TOKENS {
        // Too short to compare by share: only filler while they talk, or a
        // scrap of a line of theirs said at the same moment ("role is" while
        // they said "my role is to design"). A short reply of the owner's own
        // ("yeah", "right") stays.
        if !mine.is_empty() && mine.iter().all(|w| is_filler(w.as_str())) {
            return !near.is_empty();
        }
        if mine.is_empty() || all_ack {
            return false;
        }
        return theirs
            .iter()
            .filter(|o| m.start_ms < o.end_ms + SCRAP_MS && m.end_ms + SCRAP_MS > o.start_ms)
            .any(|o| {
                let words = tokens(&o.text);
                mine.iter().all(|w| words.contains(w))
            });
    }
    if mine.len() < MID_TOKENS {
        return near.iter().any(|o| {
            tokens(&o.text).len() >= MIN_TOKENS && share_of(&mine, &o.text) >= SHORT_THRESHOLD
        });
    }
    let needed = if mine.len() < LONG_TOKENS { MID_THRESHOLD } else { THRESHOLD };
    let best = near
        .iter()
        .filter(|o| tokens(&o.text).len() >= MIN_TOKENS)
        .map(|o| share_of(&mine, &o.text))
        .fold(0.0, f32::max);
    if best >= needed {
        return true;
    }
    let joined = near.iter().map(|o| o.text.as_str()).collect::<Vec<_>>().join(" ");
    mine.len() >= LONG_TOKENS && near.len() > 1 && share_of(&mine, &joined) >= JOINED_THRESHOLD
}

/// The share of the mic line's own words (`mine`) that `theirs` has too,
/// each counted as often as it appears in both. The mic line's, never the
/// shorter line's: a long reply of the owner's that happens to hold a short
/// line of theirs ("yes we can ship it on Friday if design signs off"
/// against "so can we ship it") is mostly words of its own, not a copy.
fn share_of(mine: &[String], theirs: &str) -> f32 {
    if mine.is_empty() {
        return 0.0;
    }
    let mut left: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for w in tokens(theirs) {
        *left.entry(w).or_default() += 1;
    }
    let mut n = 0usize;
    for w in mine {
        if let Some(c) = left.get_mut(w)
            && *c > 0
        {
            *c -= 1;
            n += 1;
        }
    }
    n as f32 / mine.len() as f32
}

/// One of the owner's own short replies ([`ACK`]).
fn is_ack(word: &str) -> bool {
    ACK.contains(&word)
}

/// A sound that is not a word: "um", "uh", "hmm", "mm", "mhm", "erm", "ah",
/// "oh", "eh", "huh", drawn out or not. "Mm-hmm" and "uh-huh" arrive as two
/// such words, since the hyphen splits them.
fn is_filler(word: &str) -> bool {
    let drawn = |head: char, tail: char| {
        word.strip_prefix(head)
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c == tail))
    };
    drawn('u', 'm')
        || drawn('u', 'h')
        || drawn('h', 'm')
        || drawn('m', 'm')
        || matches!(word, "mhm" | "erm" | "ah" | "oh" | "eh" | "huh")
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: usize = SAMPLE_RATE as usize;

    /// A small linear congruential generator: the same noise on every run.
    struct Lcg(u64);

    impl Lcg {
        /// Uniform in -1.0..1.0.
        fn uniform(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(1_103_515_245).wrapping_add(12_345) & 0x7fff_ffff;
            self.0 as f64 / 1_073_741_824.0 - 1.0
        }
    }

    fn to_sample(x: f64) -> i16 {
        (x * 32_767.0).round() as i16
    }

    /// Bursts of noise, `secs` long: `on_ms` at `amp` in every `period_ms`,
    /// a hundredth of that in between.
    fn bursts(secs: usize, on_ms: usize, period_ms: usize, amp: f64, seed: u64) -> Vec<i16> {
        let mut noise = Lcg(seed);
        (0..secs * RATE)
            .map(|i| {
                let ms = i * 1_000 / RATE;
                let level = if ms % period_ms < on_ms { amp } else { amp / 100.0 };
                to_sample(level * noise.uniform())
            })
            .collect()
    }

    /// Speech-like: 300 ms on, 200 ms off.
    fn speech(secs: usize, seed: u64) -> Vec<i16> {
        bursts(secs, 300, 500, 0.3, seed)
    }

    /// `system` through the speakers into the mic: `gain` as loud and
    /// `delay_ms` late (early, when negative).
    fn leak(system: &[i16], gain: f64, delay_ms: i64) -> Vec<i16> {
        let shift = delay_ms * (RATE as i64 / 1_000);
        (0..system.len() as i64)
            .map(|i| {
                usize::try_from(i - shift)
                    .ok()
                    .and_then(|j| system.get(j))
                    .map_or(0, |&s| (gain * f64::from(s)).round() as i16)
            })
            .collect()
    }

    /// Both tracks in 100 ms chunks, the mic's last chunk first.
    fn envelopes(mic: &[i16], system: &[i16]) -> Envelopes {
        let mut envelopes = Envelopes::default();
        for (n, chunk) in system.chunks(1_600).enumerate() {
            envelopes.push(Track::System, n as u64 * 100, chunk);
        }
        for (n, chunk) in mic.chunks(1_600).enumerate().rev() {
            envelopes.push(Track::Mic, n as u64 * 100, chunk);
        }
        envelopes
    }

    fn line(track: Track, text: &str, start_ms: u64, end_ms: u64) -> TrackSegment {
        TrackSegment {
            track,
            start_ms,
            end_ms,
            text: text.to_string(),
            speaker_hint: None,
            hint_scope: 0,
        }
    }

    fn mine(text: &str, start_ms: u64, end_ms: u64) -> TrackSegment {
        line(Track::Mic, text, start_ms, end_ms)
    }

    fn theirs(text: &str, start_ms: u64, end_ms: u64) -> TrackSegment {
        line(Track::System, text, start_ms, end_ms)
    }

    /// Is `m` dropped as an echo of `them` under `check`?
    fn dropped(m: &TrackSegment, them: &[TrackSegment], check: &EchoCheck) -> bool {
        let them: Vec<&TrackSegment> = them.iter().collect();
        is_echo(m, &them, check)
    }

    /// The word rules on their own: `at` for the windows, `span` for the
    /// line's own audio.
    fn matched(
        m: &TrackSegment,
        them: &[TrackSegment],
        at: impl Fn(u64) -> bool,
        span: Option<Moment>,
    ) -> bool {
        let them: Vec<&TrackSegment> = them.iter().collect();
        echo_match(m, &them, at, span)
    }

    /// A check with `active` windows and one correlated window per lag.
    fn heard(active: usize, lags: &[i64]) -> EchoCheck {
        let hits =
            lags.iter().enumerate().map(|(window, &lag)| Hit { window, lag, ratio: 0.3 }).collect();
        EchoCheck { active, hits, ..EchoCheck::default() }.settle()
    }

    #[test]
    fn push_places_a_chunk_at_its_block_and_fills_gaps_with_silence() {
        let mut envelopes = Envelopes::default();
        envelopes.push(Track::System, 50, &[16_384; 160]);
        assert_eq!(envelopes.system, [0.0, 0.0, 0.0, 0.0, 0.0, 0.5]);
        assert!(envelopes.mic.is_empty());
        // An earlier chunk, arriving later, with a short last block.
        envelopes.push(Track::System, 0, &[-8_192; 240]);
        assert_eq!(envelopes.system, [0.25, 0.25, 0.0, 0.0, 0.0, 0.5]);
        envelopes.push(Track::Mic, 20, &[16_384; 320]);
        assert_eq!(envelopes.mic, [0.0, 0.0, 0.5, 0.5]);
        envelopes.push(Track::Mic, 1_000, &[]);
        assert_eq!(envelopes.mic.len(), 4);
    }

    #[test]
    fn a_leak_from_the_speakers_is_heard() {
        let system = speech(30, 1);
        let check = EchoCheck::analyse(envelopes(&leak(&system, 0.3, 80), &system));
        assert!(check.present(), "{:?}", check.hits);
        assert_eq!(check.modal_lag(), Some(8));
        assert!(check.correlated_at(10_100));
        assert!(!check.correlated_at(60_000));
    }

    #[test]
    fn a_mic_stream_that_starts_a_little_early_still_shows_the_leak() {
        let system = speech(30, 3);
        let check = EchoCheck::analyse(envelopes(&leak(&system, 0.3, -50), &system));
        assert!(check.present(), "{:?}", check.hits);
        assert_eq!(check.modal_lag(), Some(-5));
    }

    #[test]
    fn headphones_leave_the_mic_quiet() {
        let system = speech(30, 1);
        let mic = bursts(30, 1_000, 1_000, 0.000_3, 5);
        let check = EchoCheck::analyse(envelopes(&mic, &system));
        assert!(!check.present());
        assert!(!check.correlated_at(10_100));
        assert_eq!(check.over(10_000, 12_000), None);
    }

    #[test]
    fn the_owner_talking_in_their_own_rhythm_is_not_echo() {
        let system = speech(60, 1);
        let mic = bursts(60, 350, 700, 0.2, 7);
        let check = EchoCheck::analyse(envelopes(&mic, &system));
        assert!(check.active >= MIN_ACTIVE, "{}", check.active);
        assert!(!check.present(), "{:?}", check.hits);
    }

    #[test]
    fn echo_is_a_rate_at_a_steady_lag() {
        // Too few windows to say anything.
        assert!(!heard(9, &[8; 9]).present());
        assert!(heard(10, &[8; 10]).present());
        // Under the rate.
        assert!(!heard(10, &[8, 8, 8]).present());
        // Between the rates: only at a steady lag.
        assert!(heard(10, &[8, 8, 9, 7]).present());
        assert!(!heard(10, &[8, 20, -4, 14]).present());
        // Over the strong rate, at any lags: a room can have two paths.
        assert!(heard(10, &[8, 20, -4, 14, 3, 25]).present());
    }

    #[test]
    fn assume_present_and_none_say_what_they_mean() {
        let assumed = EchoCheck::assume_present();
        assert!(assumed.present());
        assert!(assumed.correlated_at(0) && assumed.correlated_at(3_600_000));
        assert_eq!(assumed.over(0, 2_000), None);
        let none = EchoCheck::none();
        assert!(!none.present());
        assert!(!none.correlated_at(0));
        assert_eq!(none.over(0, 2_000), None);
        assert!(!EchoCheck::analyse(Envelopes::default()).present());
    }

    #[test]
    fn a_leaked_line_is_pure_on_its_own_audio() {
        let system = speech(30, 1);
        let check = EchoCheck::analyse(envelopes(&leak(&system, 0.3, 80), &system));
        assert_eq!(
            check.over(10_000, 12_000),
            Some(Moment { correlated: true, loud: true, pure: true })
        );
    }

    #[test]
    fn a_line_said_while_they_were_silent_is_not_their_voice() {
        let mut system = speech(24, 1);
        let mut mic = leak(&system, 0.3, 80);
        system.resize(system.len() + 4 * RATE, 0);
        mic.extend(bursts(4, 300, 500, 0.3, 7));
        let check = EchoCheck::analyse(envelopes(&mic, &system));
        assert!(check.present());
        assert_eq!(check.over(25_000, 27_000), Some(Moment::default()));
    }

    #[test]
    fn the_owners_own_loud_line_is_not_like_the_leak() {
        // Their voice leaks in for 24 s; then the owner talks alone while
        // their track carries a faint copy of it (far-end echo).
        let own = bursts(6, 300, 500, 0.3, 7);
        let mut system = speech(24, 1);
        let mut mic = leak(&system, 0.3, 80);
        system.extend(own.iter().map(|&s| (f64::from(s) * 0.2).round() as i16));
        mic.extend_from_slice(&own);
        let check = EchoCheck::analyse(envelopes(&mic, &system));
        let moment = check.over(26_000, 29_000).expect("the room's lag is known");
        assert!(!moment.loud, "{moment:?}");
        assert!(!moment.pure, "{moment:?}");
    }

    #[test]
    fn the_audio_decides_whether_a_copy_is_dropped() {
        let system = speech(30, 1);
        let leaking = EchoCheck::analyse(envelopes(&leak(&system, 0.3, 80), &system));
        let silent = vec![0; system.len()];
        let headphones = EchoCheck::analyse(envelopes(&silent, &system));
        let them = [theirs("we should move the launch", 10_000, 12_000)];
        let m = mine("we should move the launch", 10_200, 12_200);
        assert!(dropped(&m, &them, &leaking));
        assert!(!dropped(&m, &them, &headphones));
    }

    #[test]
    fn an_exact_copy_is_an_echo_only_when_echo_was_heard() {
        let them = [theirs("we should move the launch", 10_000, 12_000)];
        let m = mine("we should move the launch", 10_200, 12_200);
        assert!(dropped(&m, &them, &EchoCheck::assume_present()));
        assert!(!dropped(&m, &them, &EchoCheck::none()));
    }

    #[test]
    fn different_words_or_words_said_far_away_are_not_an_echo() {
        let present = EchoCheck::assume_present();
        let them = [theirs("we should move the launch", 10_000, 12_000)];
        assert!(!dropped(&mine("I can send the notes tonight", 10_200, 12_200), &them, &present));
        assert!(!dropped(&mine("we should move the launch", 60_000, 62_000), &them, &present));
        // Case, punctuation and a few missing words do not matter.
        let them = [theirs("Okay, we should move the launch to Friday.", 9_500, 11_500)];
        assert!(dropped(&mine("We should move... the launch!", 10_000, 12_000), &them, &present));
        // Nor does a script written without spaces.
        let them = [theirs("明日の会議は十時からです", 4_800, 6_800)];
        assert!(dropped(&mine("明日の会議は十時からです", 5_000, 7_000), &them, &present));
    }

    #[test]
    fn only_system_lines_are_compared_and_only_mic_lines_judged() {
        let present = EchoCheck::assume_present();
        let other = mine("we should move the launch", 10_000, 12_000);
        let m = mine("we should move the launch", 10_200, 12_200);
        assert!(!is_echo(&m, &[&other], &present));
        let sys = theirs("we should move the launch", 10_000, 12_000);
        assert!(!is_echo(&sys, &[&sys], &present));
    }

    #[test]
    fn short_acknowledgements_are_never_echoes() {
        let present = EchoCheck::assume_present();
        let them = [theirs("yeah right okay so I think we can ship it", 10_000, 13_000)];
        for reply in ["Yeah.", "Right, okay.", "Okay.", "Yes, thanks, great."] {
            assert!(!dropped(&mine(reply, 11_000, 11_600), &them, &present), "{reply}");
        }
        // Not even when they said exactly the same.
        assert!(!dropped(
            &mine("okay", 10_000, 12_000),
            &[theirs("okay", 10_000, 12_000)],
            &present
        ));
    }

    #[test]
    fn a_six_or_seven_word_line_needs_eighty_percent() {
        let present = EchoCheck::assume_present();
        let them = [theirs("and yes I can send it so you have it", 11_200, 14_000)];
        // Four of six words: the owner's own reply.
        assert!(!dropped(&mine("Sounds good, I can send it.", 11_000, 12_500), &them, &present));
        // Five of six: their line on the mic.
        assert!(dropped(&mine("yes I can send it now", 11_300, 13_000), &them, &present));
    }

    #[test]
    fn a_long_line_needs_sixty_percent() {
        let present = EchoCheck::assume_present();
        let them = [theirs("we should move the launch to friday morning", 10_000, 12_000)];
        let five_of_eight = mine("we should move the launch beyond monday evening", 10_100, 12_100);
        assert!(dropped(&five_of_eight, &them, &present));
        let one_of_eight = mine("we could push it beyond monday evening then", 10_100, 12_100);
        assert!(!dropped(&one_of_eight, &them, &present));
    }

    #[test]
    fn a_read_back_after_their_line_ended_is_the_owners_own() {
        let present = EchoCheck::assume_present();
        let them = [theirs("we should move the launch", 10_000, 12_000)];
        // Inside the speakers' tail: their voice still on the mic.
        assert!(dropped(&mine("we should move the launch", 12_400, 14_000), &them, &present));
        // Later than that: said back.
        assert!(!dropped(&mine("we should move the launch", 12_600, 14_000), &them, &present));
    }

    #[test]
    fn only_the_window_holding_the_line_counts() {
        let windows = [5, 6, 10, 11];
        let at = |ms: u64| windows.contains(&(ms / 2_000));
        let them = [
            theirs("Can you send me the report by Friday?", 10_000, 12_500),
            theirs("so the deadline is next Friday the 14th", 20_000, 22_500),
        ];
        for m in [
            mine("Send you the report by Friday? Sure.", 13_000, 15_000),
            mine("Send you the report by Friday? Sure.", 15_000, 17_000),
            mine("The deadline is next Friday the 14th, okay.", 23_000, 25_000),
        ] {
            assert!(!matched(&m, &them, at, None), "{} @{}", m.text, m.start_ms);
        }
        assert!(matched(
            &mine("can you send me the report by Friday", 10_100, 12_400),
            &them,
            at,
            None
        ));
        // Just after their line ended, within the speakers' tail.
        assert!(matched(&mine("send me the report by Friday", 12_800, 14_000), &them, at, None));
    }

    #[test]
    fn a_long_line_straddling_two_of_theirs_is_caught_whole() {
        let present = EchoCheck::assume_present();
        let them = [
            theirs("okay so I think we will probably move the", 9_000, 11_000),
            theirs("launch to next friday if that works for everyone", 11_000, 13_500),
        ];
        let straddling = mine("I think we should move the launch to next friday", 10_100, 12_600);
        assert!(dropped(&straddling, &them, &present));
        let own = mine("I think we should wait for more data first", 10_100, 12_600);
        assert!(!dropped(&own, &them, &present));
    }

    #[test]
    fn a_scrap_of_their_line_said_at_that_moment_is_an_echo() {
        let present = EchoCheck::assume_present();
        let them = [theirs(
            "the main responsibility is user interface and my role is to design a remote",
            28_000,
            32_000,
        )];
        assert!(dropped(&mine("role is", 29_000, 29_600), &them, &present));
        assert!(dropped(&mine("design a", 30_400, 30_900), &them, &present));
        // The same words at another moment, or a reply of the owner's own, stay.
        assert!(!dropped(&mine("role is", 40_000, 40_600), &them, &present));
        assert!(!dropped(&mine("okay sure", 29_000, 29_600), &them, &present));
    }

    #[test]
    fn filler_while_they_talk_is_nothing() {
        let present = EchoCheck::assume_present();
        let them = [theirs("so that is kind of our brief", 9_000, 12_000)];
        assert!(dropped(&mine("Um...", 10_000, 10_500), &them, &present));
        assert!(dropped(&mine("Mm-hmm.", 10_000, 10_500), &them, &present));
        assert!(!dropped(&mine("Um...", 50_000, 50_500), &them, &present));
    }

    #[test]
    fn without_evidence_nothing_is_an_echo() {
        let them =
            [theirs("yeah I think we can ship it on Friday if design is right", 10_000, 16_000)];
        for m in
            ["Yeah.", "Friday?", "Mm-hmm.", "Ship it.", "yeah I think we can ship it on Friday"]
        {
            assert!(!dropped(&mine(m, 12_000, 12_500), &them, &EchoCheck::none()), "{m}");
        }
        // With echo heard, the rules apply: scraps and filler go, replies stay.
        let present = EchoCheck::assume_present();
        assert!(dropped(&mine("Friday?", 12_000, 12_500), &them, &present));
        assert!(dropped(&mine("Mm-hmm.", 12_000, 12_500), &them, &present));
        assert!(!dropped(&mine("Okay.", 12_000, 12_500), &them, &present));
    }

    #[test]
    fn a_line_that_is_only_their_voice_goes_at_a_lower_word_bar() {
        let never = |_: u64| false;
        let pure = Some(Moment { correlated: true, loud: true, pure: true });
        // Three of four words: under the short-line bar, enough with the audio.
        let them = [theirs("My name is Aileen.", 41_200, 41_900)];
        assert!(matched(&mine("My name is Eileen.", 40_940, 42_220), &them, never, pure));
        let them =
            [theirs("so first of all just to kind of make sure that we all know", 20_400, 23_940)];
        assert!(matched(&mine("that we all know each other.", 23_580, 24_920), &them, never, pure));
        // Never on the audio alone: a quiet owner whose words differ stays.
        let designing =
            [theirs("so we are designing a new remote control and I have to", 41_300, 49_000)];
        assert!(!matched(&mine("- I agree it, okay.", 41_400, 44_100), &designing, never, pure));
        let song = [theirs("la la la the night is young", 8_000, 18_000)];
        let budget = mine("Let me check the budget numbers first", 10_000, 13_000);
        assert!(!matched(&budget, &song, never, pure));
        // Their matching line must overlap the mic line, not merely be near it.
        let before = [theirs("My name is Aileen.", 39_500, 40_900)];
        assert!(!matched(&mine("My name is Eileen.", 40_940, 42_220), &before, never, pure));
        // And never the owner's short replies.
        for reply in ["Okay.", "Yeah, right.", "Yes, thanks, great.", "with the"] {
            assert!(!matched(&mine(reply, 42_000, 42_600), &designing, never, pure), "{reply}");
        }
    }

    #[test]
    fn the_lines_own_level_rules_out_the_owners_own_words() {
        let always = |_: u64| true;
        let never = |_: u64| false;
        let moment = |correlated, loud| Some(Moment { correlated, loud, pure: false });
        // A copy of the owner's words on their track (far-end echo): the mic
        // is far louder than the leak.
        let words = "so why don't we look at the presentation";
        let copy = [theirs(words, 65_300, 68_900)];
        assert!(!matched(&mine(words, 65_200, 68_900), &copy, always, moment(true, false)));
        // Talking over them: the line's own correlation drops, but the
        // window's verdict still counts, with the level check.
        let them = [theirs(
            "and I am a marketing person I want to figure out how to sell them",
            35_400,
            39_800,
        )];
        let m = mine("I want to figure out how to sell them.", 37_460, 40_040);
        assert!(matched(&m, &them, always, moment(false, true)));
        assert!(!matched(&m, &them, never, moment(false, true)));
        assert!(!matched(&m, &them, always, moment(false, false)));
        // With no verdict for the span, the windows decide.
        let them = [theirs("we should move the launch", 10_000, 12_000)];
        let m = mine("we should move the launch", 10_200, 12_000);
        assert!(matched(&m, &them, always, None));
        assert!(!matched(&m, &them, never, None));
    }

    #[test]
    fn a_long_reply_holding_a_short_line_of_theirs_is_the_owners_own() {
        let check = EchoCheck::assume_present();
        let them = [theirs("so can we ship it", 9_500, 11_000)];
        let reply = mine("yes we can ship it on Friday if design signs off", 10_600, 13_000);
        assert!(!dropped(&reply, &them, &check));
        // Their line copied, a word or two short, still is.
        let copy = mine("so can we ship it", 9_600, 11_100);
        assert!(dropped(&copy, &them, &check));
    }
}
