//! Hearing one word: a small network that listens to every 80 ms of the
//! microphone for the assistant's name, cheaply enough to run all day.
//!
//! The wake word ([`super::wake`]) otherwise writes down everything the
//! microphone hears and looks for the name in the words, which spends a speech
//! recogniser on a television. A [`Spotter`] spends a few milliseconds a
//! second instead, and only what follows the name is ever written down.
//!
//! # The network
//!
//! openWakeWord's front end, frozen: a mel spectrogram, then Google's speech
//! embedding (Apache-2.0, trained on a great deal of speech), then a small
//! head trained for one name on synthetic voices (`scripts/wakeword`). The
//! whole of it is in one `.jww` file ([`Model::parse`]): the name's is built
//! in ([`Model::jatayu`]).
//!
//! # Exactly as it was trained
//!
//! Training computed every feature the way this module does, step for step
//! (`scripts/wakeword/features.py` defines it): each step takes 1280 new
//! samples with the 480 before them, makes eight mel frames, and embeds the
//! newest 76 frames; the mel history starts with 76 rows of ones, and the
//! head reads the newest 16 embeddings. The embedding network is
//! convolutional in time with three pools of two, so the window moving by
//! eight frames moves its every layer by whole rows: each layer here keeps
//! the few rows it still needs and works out only the new ones, a ninth of
//! the work of embedding all 76 frames afresh, with the same result.
//!
//! Pure Rust, with `matrixmultiply` for the products, so it runs the same on
//! every platform this crate builds for.

use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};

use serde::Deserialize;

use crate::error::{CommandError, CommandResult, codes};

/// New samples a step takes: 80 ms at 16 kHz.
pub const STEP: usize = 1280;
/// The samples before them that each step's spectrogram also reads.
const CONTEXT: usize = 480;
/// Mel frames each embedding reads.
const MEL_FRAMES: usize = 76;
/// Mel frames each step makes.
const FRAMES_PER_STEP: usize = 8;
const BANDS: usize = 32;
const EMBEDDING: usize = 96;
/// Steps at the start whose embeddings still read the ones the mel history
/// starts with: never judged, as they were never trained on. So a spotter
/// hears nothing for its first two seconds, made or [`Spotter::reset`].
const WARMUP: u64 = (MEL_FRAMES / FRAMES_PER_STEP + 16) as u64;
/// After a hit, how long nothing else counts: one name, one hit.
const REFRACTORY_STEPS: u64 = 20;

/// The name the built-in model hears.
pub const BUILT_IN_NAME: &str = "Jatayu";

static JATAYU: &[u8] = include_bytes!("../../assets/wakeword/jatayu.jww");

/// One `.jww` file, parsed: the front end and, but for a test's file, a head.
pub struct Model {
    name: String,
    threshold: f32,
    mel: Mel,
    layers: Vec<Layer>,
    head: Option<Head>,
}

struct Mel {
    /// The short-time Fourier transform as two matrices, 257 x 512, the
    /// window already in them.
    real: Vec<f32>,
    imag: Vec<f32>,
    /// 257 x 32.
    filters: Vec<f32>,
    low: f32,
    high: f32,
    top: f32,
    top_db: f32,
    window: usize,
    hop: usize,
}

enum Layer {
    Conv {
        cin: usize,
        cout: usize,
        kt: usize,
        kf: usize,
        pad_f: usize,
        /// Frequencies in and out.
        f_in: usize,
        f_out: usize,
        /// cout x (cin, kt, kf), as the file has it.
        weight: Vec<f32>,
        bias: Option<Vec<f32>>,
        /// Leaky slope and floor, when it has an activation.
        act: Option<(f32, f32)>,
    },
    Pool {
        pt: usize,
        pf: usize,
        channels: usize,
        f_in: usize,
        f_out: usize,
    },
}

struct Head {
    window: usize,
    layers: Vec<HeadLayer>,
}

enum HeadLayer {
    Dense { inputs: usize, outputs: usize, weight: Vec<f32>, bias: Vec<f32> },
    LayerNorm { weight: Vec<f32>, bias: Vec<f32>, eps: f32 },
    Relu,
    Sigmoid,
}

// ---- the file -----------------------------------------------------------------

#[derive(Deserialize)]
struct Header {
    format: u32,
    name: String,
    #[serde(default)]
    threshold: Option<f32>,
    mel: MelHeader,
    embedding: Vec<LayerHeader>,
    #[serde(default)]
    head: Option<HeadHeader>,
}

#[derive(Deserialize)]
struct TensorRef {
    offset: usize,
    shape: Vec<usize>,
}

#[derive(Deserialize)]
struct MelHeader {
    real: TensorRef,
    imag: TensorRef,
    filters: TensorRef,
    /// The clip bounds; `null` for none, which JSON cannot write as infinity.
    low: Option<f32>,
    high: Option<f32>,
    top: Option<f32>,
    top_db: f32,
    window: usize,
    hop: usize,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum LayerHeader {
    Conv {
        cin: usize,
        cout: usize,
        kt: usize,
        kf: usize,
        pad_f: usize,
        weight: TensorRef,
        #[serde(default)]
        bias: Option<TensorRef>,
        act: bool,
        #[serde(default)]
        slope: Option<f32>,
        #[serde(default)]
        floor: Option<f32>,
    },
    Pool {
        pt: usize,
        pf: usize,
    },
}

#[derive(Deserialize)]
struct HeadHeader {
    window: usize,
    layers: Vec<HeadLayerHeader>,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum HeadLayerHeader {
    Dense { weight: TensorRef, bias: TensorRef },
    Layernorm { weight: TensorRef, bias: TensorRef, eps: f32 },
    Relu,
    Sigmoid,
}

fn bad(why: impl std::fmt::Display) -> CommandError {
    CommandError::new(codes::INVALID, format!("not a wake-word model: {why}"))
}

impl Model {
    /// The model built into this build, for [`BUILT_IN_NAME`].
    pub fn jatayu() -> CommandResult<Arc<Model>> {
        static MODEL: OnceLock<Result<Arc<Model>, String>> = OnceLock::new();
        MODEL
            .get_or_init(|| Model::parse(JATAYU).map(Arc::new).map_err(|e| e.message))
            .clone()
            .map_err(|message| CommandError::new(codes::INTERNAL, message))
    }

    /// Read a `.jww` file: see `scripts/wakeword/export.py` for its shape.
    pub fn parse(bytes: &[u8]) -> CommandResult<Model> {
        if bytes.len() < 8 || &bytes[..4] != b"JWW1" {
            return Err(bad("the file does not start JWW1"));
        }
        let length = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let text = bytes.get(8..8 + length).ok_or_else(|| bad("the header is cut short"))?;
        let header: Header = serde_json::from_slice(text).map_err(bad)?;
        if header.format != 1 {
            return Err(bad(format!("format {}", header.format)));
        }
        let data = &bytes[8 + length..];
        if !data.len().is_multiple_of(4) {
            return Err(bad("the tensors are not whole floats"));
        }
        let floats: Vec<f32> =
            data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        let take = |t: &TensorRef, want: &[usize]| -> CommandResult<Vec<f32>> {
            if !want.is_empty() && t.shape != want {
                return Err(bad(format!("a tensor is {:?}, not {want:?}", t.shape)));
            }
            let size: usize = t.shape.iter().product();
            floats
                .get(t.offset..t.offset + size)
                .map(<[f32]>::to_vec)
                .ok_or_else(|| bad("a tensor runs past the end"))
        };

        let m = &header.mel;
        let bins = m.window / 2 + 1;
        if m.window != 512 || m.hop != 160 {
            return Err(bad("a spectrogram this module was not written for"));
        }
        let mel = Mel {
            real: take(&m.real, &[bins, m.window])?,
            imag: take(&m.imag, &[bins, m.window])?,
            filters: take(&m.filters, &[bins, BANDS])?,
            low: m.low.unwrap_or(f32::NEG_INFINITY),
            high: m.high.unwrap_or(f32::INFINITY),
            top: m.top.unwrap_or(f32::INFINITY),
            top_db: m.top_db,
            window: m.window,
            hop: m.hop,
        };

        let (mut channels, mut freq) = (1usize, BANDS);
        let mut layers = Vec::new();
        for layer in &header.embedding {
            match layer {
                LayerHeader::Conv { cin, cout, kt, kf, pad_f, weight, bias, act, slope, floor } => {
                    if *cin != channels {
                        return Err(bad("a layer's channels do not follow on"));
                    }
                    let f_out = (freq + 2 * pad_f + 1)
                        .checked_sub(*kf)
                        .ok_or_else(|| bad("a kernel wider than its input"))?;
                    layers.push(Layer::Conv {
                        cin: *cin,
                        cout: *cout,
                        kt: *kt,
                        kf: *kf,
                        pad_f: *pad_f,
                        f_in: freq,
                        f_out,
                        weight: take(weight, &[*cout, *cin, *kt, *kf])?,
                        bias: bias.as_ref().map(|b| take(b, &[*cout])).transpose()?,
                        act: act
                            .then(|| (slope.unwrap_or(0.0), floor.unwrap_or(f32::NEG_INFINITY))),
                    });
                    channels = *cout;
                    freq = f_out;
                }
                LayerHeader::Pool { pt, pf } => {
                    if !(1..=2).contains(pt) || *pf == 0 {
                        return Err(bad("a pool this module was not written for"));
                    }
                    let f_out = freq / pf;
                    layers.push(Layer::Pool { pt: *pt, pf: *pf, channels, f_in: freq, f_out });
                    freq = f_out;
                }
            }
        }
        if channels != EMBEDDING || freq != 1 {
            return Err(bad("the network does not end in one embedding"));
        }
        let stride: usize =
            layers.iter().map(|l| if let Layer::Pool { pt, .. } = l { *pt } else { 1 }).product();
        if stride != FRAMES_PER_STEP {
            return Err(bad("the network does not move a step at a time"));
        }

        let head = match &header.head {
            None => None,
            Some(h) => {
                let mut width = h.window * EMBEDDING;
                let mut out = Vec::new();
                for layer in &h.layers {
                    out.push(match layer {
                        HeadLayerHeader::Dense { weight, bias } => {
                            let [outputs, inputs] = weight.shape[..] else {
                                return Err(bad("a dense layer is not a matrix"));
                            };
                            if inputs != width {
                                return Err(bad("the head's layers do not follow on"));
                            }
                            width = outputs;
                            HeadLayer::Dense {
                                inputs,
                                outputs,
                                weight: take(weight, &[])?,
                                bias: take(bias, &[outputs])?,
                            }
                        }
                        HeadLayerHeader::Layernorm { weight, bias, eps } => HeadLayer::LayerNorm {
                            weight: take(weight, &[width])?,
                            bias: take(bias, &[width])?,
                            eps: *eps,
                        },
                        HeadLayerHeader::Relu => HeadLayer::Relu,
                        HeadLayerHeader::Sigmoid => HeadLayer::Sigmoid,
                    });
                }
                if width != 1 {
                    return Err(bad("the head does not end in one score"));
                }
                Some(Head { window: h.window, layers: out })
            }
        };
        if head.is_some() && header.threshold.is_none() {
            return Err(bad("a head with no threshold"));
        }
        Ok(Model {
            name: header.name,
            threshold: header.threshold.unwrap_or(1.0),
            mel,
            layers,
            head,
        })
    }

    /// The name it hears.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The score a hit needs, as trained and tested.
    pub fn threshold(&self) -> f32 {
        self.threshold
    }

    /// Whether this model hears `name`: the same word, whatever its case.
    pub fn hears(&self, name: &str) -> bool {
        self.head.is_some() && name.trim().eq_ignore_ascii_case(&self.name)
    }
}

// ---- the arithmetic -------------------------------------------------------------

/// `c = a * b`, all row-major: `a` is m x k, `b` k x n, `c` m x n.
fn matmul(a: &[f32], b: &[f32], c: &mut [f32], m: usize, k: usize, n: usize) {
    assert!(a.len() >= m * k && b.len() >= k * n && c.len() >= m * n);
    // SAFETY: the assert above bounds every index sgemm reads or writes for
    // these dimensions and strides.
    unsafe {
        matrixmultiply::sgemm(
            m,
            k,
            n,
            1.0,
            a.as_ptr(),
            k as isize,
            1,
            b.as_ptr(),
            n as isize,
            1,
            0.0,
            c.as_mut_ptr(),
            n as isize,
            1,
        );
    }
}

impl Mel {
    /// Eight frames of 32 bands from one step's 1760 samples, as the head
    /// was trained to read them (`x / 10 + 2`), onto the end of `out`.
    fn frames(&self, window: &[f32], out: &mut Vec<f32>, b: &mut MelBuffers) {
        let bins = self.window / 2 + 1;
        let frames = (window.len() - self.window) / self.hop + 1;
        debug_assert_eq!(frames, FRAMES_PER_STEP);
        b.size(self.window, bins, frames);
        // The signal as a window x frames matrix, each column one frame.
        for n in 0..self.window {
            for f in 0..frames {
                b.framed[n * frames + f] = window[f * self.hop + n];
            }
        }
        matmul(&self.real, &b.framed, &mut b.re, bins, self.window, frames);
        matmul(&self.imag, &b.framed, &mut b.im, bins, self.window, frames);
        // Power, as frames x bins for the filter bank.
        for bin in 0..bins {
            for f in 0..frames {
                let (r, i) = (b.re[bin * frames + f], b.im[bin * frames + f]);
                b.power[f * bins + bin] = r * r + i * i;
            }
        }
        matmul(&b.power, &self.filters, &mut b.mel, frames, bins, BANDS);
        let mut top = f32::NEG_INFINITY;
        for v in &mut b.mel {
            *v = 10.0 * v.clamp(self.low, self.high).ln() / std::f32::consts::LN_10;
            top = top.max(*v);
        }
        let floor = top - self.top_db;
        out.extend(b.mel.iter().map(|v| v.max(floor).min(self.top) / 10.0 + 2.0));
    }
}

/// The spectrogram's working space, kept from step to step.
#[derive(Default)]
struct MelBuffers {
    framed: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    power: Vec<f32>,
    mel: Vec<f32>,
}

impl MelBuffers {
    fn size(&mut self, window: usize, bins: usize, frames: usize) {
        self.framed.resize(window * frames, 0.0);
        self.re.resize(bins * frames, 0.0);
        self.im.resize(bins * frames, 0.0);
        self.power.resize(frames * bins, 0.0);
        self.mel.resize(frames * BANDS, 0.0);
    }
}

/// What one layer of the embedding network still needs of its input.
#[derive(Default)]
struct Rows {
    rows: VecDeque<Vec<f32>>,
}

impl Layer {
    /// Take one input row (channels x frequencies); give the output row it
    /// completes, if it completes one.
    fn push(&self, state: &mut Rows, row: Vec<f32>, patch: &mut Vec<f32>) -> Option<Vec<f32>> {
        match self {
            Layer::Conv { cin, cout, kt, kf, pad_f, f_in, f_out, weight, bias, act } => {
                state.rows.push_back(row);
                if state.rows.len() < *kt {
                    return None;
                }
                let k = cin * kt * kf;
                patch.clear();
                patch.resize(k * f_out, 0.0);
                for ci in 0..*cin {
                    for (dt, row) in state.rows.iter().enumerate().take(*kt) {
                        let src = &row[ci * f_in..(ci + 1) * f_in];
                        for df in 0..*kf {
                            let at = ((ci * kt + dt) * kf + df) * f_out;
                            for f in 0..*f_out {
                                let s = f + df;
                                if s >= *pad_f && s - pad_f < *f_in {
                                    patch[at + f] = src[s - pad_f];
                                }
                            }
                        }
                    }
                }
                state.rows.pop_front();
                let mut out = vec![0.0f32; cout * f_out];
                matmul(weight, patch, &mut out, *cout, k, *f_out);
                if let Some(bias) = bias {
                    for (c, b) in bias.iter().enumerate() {
                        out[c * f_out..(c + 1) * f_out].iter_mut().for_each(|v| *v += b);
                    }
                }
                if let Some((slope, floor)) = act {
                    for v in &mut out {
                        let leaky = if *v < 0.0 { *v * slope } else { *v };
                        *v = leaky.max(*floor);
                    }
                }
                Some(out)
            }
            Layer::Pool { pt, pf, channels, f_in, f_out } => {
                state.rows.push_back(row);
                if state.rows.len() < *pt {
                    return None;
                }
                let mut out = vec![f32::NEG_INFINITY; channels * f_out];
                for row in state.rows.drain(..) {
                    for c in 0..*channels {
                        for f in 0..*f_out {
                            for d in 0..*pf {
                                let v = row[c * f_in + f * pf + d];
                                let o = &mut out[c * f_out + f];
                                *o = o.max(v);
                            }
                        }
                    }
                }
                Some(out)
            }
        }
    }
}

impl Head {
    fn score(&self, input: &[f32]) -> f32 {
        let mut x = input.to_vec();
        for layer in &self.layers {
            match layer {
                HeadLayer::Dense { inputs, outputs, weight, bias } => {
                    let mut y = bias.clone();
                    for (o, out) in y.iter_mut().enumerate() {
                        let w = &weight[o * inputs..(o + 1) * inputs];
                        *out += w.iter().zip(&x).map(|(a, b)| a * b).sum::<f32>();
                    }
                    debug_assert_eq!(y.len(), *outputs);
                    x = y;
                }
                HeadLayer::LayerNorm { weight, bias, eps } => {
                    let n = x.len() as f32;
                    let mean = x.iter().sum::<f32>() / n;
                    let var = x.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n;
                    let scale = 1.0 / (var + eps).sqrt();
                    for (i, v) in x.iter_mut().enumerate() {
                        *v = (*v - mean) * scale * weight[i] + bias[i];
                    }
                }
                HeadLayer::Relu => x.iter_mut().for_each(|v| *v = v.max(0.0)),
                HeadLayer::Sigmoid => x.iter_mut().for_each(|v| *v = 1.0 / (1.0 + (-*v).exp())),
            }
        }
        x[0]
    }
}

// ---- listening ------------------------------------------------------------------

/// The name heard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spotted {
    /// Samples since the spotter was made or reset, up to the end of the step
    /// that heard it: the name ended a little before here, never after.
    pub end_sample: u64,
    pub score: f32,
}

/// A [`Model`] listening to one stream of 16 kHz audio.
pub struct Spotter {
    model: Arc<Model>,
    /// The last [`CONTEXT`] samples of the step before, then this step's.
    window: Vec<f32>,
    pending: Vec<f32>,
    layers: Vec<Rows>,
    embeddings: VecDeque<[f32; EMBEDDING]>,
    steps: u64,
    quiet_until: u64,
    /// The highest score since the last hit: see [`Spotter::peak`].
    peak: f32,
    mel: Vec<f32>,
    patch: Vec<f32>,
    buffers: MelBuffers,
}

impl Spotter {
    pub fn new(model: Arc<Model>) -> Spotter {
        let mut spotter = Spotter {
            layers: model.layers.iter().map(|_| Rows::default()).collect(),
            model,
            window: Vec::with_capacity(STEP + CONTEXT),
            pending: Vec::with_capacity(STEP),
            embeddings: VecDeque::new(),
            steps: 0,
            quiet_until: 0,
            peak: 0.0,
            mel: Vec::with_capacity(FRAMES_PER_STEP * BANDS),
            patch: Vec::new(),
            buffers: MelBuffers::default(),
        };
        spotter.reset();
        spotter
    }

    /// Forget everything heard: as if just made.
    pub fn reset(&mut self) {
        self.window.clear();
        self.window.resize(CONTEXT, 0.0);
        self.pending.clear();
        self.layers.iter_mut().for_each(|l| l.rows.clear());
        self.embeddings.clear();
        self.steps = 0;
        self.quiet_until = 0;
        self.peak = 0.0;
        // The mel history begins with 76 rows of ones, as in training. The
        // first embedding they complete is the window of ones alone, which
        // no step is judged by.
        for _ in 0..MEL_FRAMES {
            let _ = self.feed_row(vec![1.0; BANDS]);
        }
    }

    /// One mel row through every layer; the embedding it completes, if any.
    fn feed_row(&mut self, row: Vec<f32>) -> Option<Vec<f32>> {
        let mut row = row;
        for (layer, state) in self.model.layers.iter().zip(self.layers.iter_mut()) {
            row = layer.push(state, row, &mut self.patch)?;
        }
        Some(row)
    }

    /// Hear more audio (16 kHz mono); what in it was the name.
    pub fn push(&mut self, samples: &[i16]) -> Vec<Spotted> {
        let mut out = Vec::new();
        for &s in samples {
            self.pending.push(f32::from(s));
            if self.pending.len() == STEP
                && let Some(hit) = self.step()
            {
                out.push(hit);
            }
        }
        out
    }

    /// The step's embedding, for the tests that compare it with training's.
    fn embed_step(&mut self) -> [f32; EMBEDDING] {
        self.window.extend_from_slice(&self.pending);
        self.pending.clear();
        self.mel.clear();
        let mut mel = std::mem::take(&mut self.mel);
        self.model.mel.frames(&self.window, &mut mel, &mut self.buffers);
        self.window.drain(..STEP);
        let mut embedding = None;
        for f in 0..FRAMES_PER_STEP {
            let row = mel[f * BANDS..(f + 1) * BANDS].to_vec();
            if let Some(e) = self.feed_row(row) {
                embedding = Some(e);
            }
        }
        self.mel = mel;
        self.steps += 1;
        let e = embedding.expect("eight frames complete one embedding");
        let mut fixed = [0.0; EMBEDDING];
        fixed.copy_from_slice(&e);
        fixed
    }

    fn step(&mut self) -> Option<Spotted> {
        let embedding = self.embed_step();
        let head = self.model.head.as_ref()?;
        self.embeddings.push_back(embedding);
        while self.embeddings.len() > head.window {
            self.embeddings.pop_front();
        }
        if self.embeddings.len() < head.window || self.steps <= WARMUP {
            return None;
        }
        let input: Vec<f32> = self.embeddings.iter().flatten().copied().collect();
        let score = head.score(&input);
        if self.steps < self.quiet_until {
            // Still the last hit's name: how sure of it the spotter grows.
            self.peak = self.peak.max(score);
            return None;
        }
        // A score that is not a number is no hit.
        if score.is_nan() || score < self.model.threshold {
            return None;
        }
        self.quiet_until = self.steps + REFRACTORY_STEPS;
        self.peak = score;
        Some(Spotted { end_sample: self.steps * STEP as u64, score })
    }

    /// The highest score since the last hit, through the steps after it in
    /// which nothing else counts: how sure of the name the spotter grew. A
    /// hit is reported the moment a step is sure enough, which is often
    /// before the name is over.
    pub fn peak(&self) -> f32 {
        self.peak
    }

    /// The score of the latest step, whatever it is, for tests and tuning.
    #[cfg(test)]
    fn score_now(&self) -> Option<f32> {
        let head = self.model.head.as_ref()?;
        (self.embeddings.len() == head.window).then(|| {
            let input: Vec<f32> = self.embeddings.iter().flatten().copied().collect();
            head.score(&input)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clip `scripts/wakeword/features.py reference` makes, made again
    /// in whole numbers so that both make the same samples: two square
    /// waves and a little noise, a hundredth as loud for half a second.
    fn reference_clip() -> Vec<i16> {
        let mut seed = 12_345u32;
        (0..48_000i32)
            .map(|n| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = ((seed >> 16) % 2001) as i32 - 1000;
                let v = ((n / 18) % 2 * 2 - 1) * 3000 + ((n / 6) % 2 * 2 - 1) * 1500 + noise;
                (if (16_000..24_000).contains(&n) { v / 100 } else { v }) as i16
            })
            .collect()
    }

    fn floats(bytes: &[u8]) -> Vec<f32> {
        bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
    }

    #[test]
    fn the_built_in_model_reads() {
        let model = Model::jatayu().unwrap();
        assert!(model.hears("Jatayu") && model.hears(" jatayu "));
        assert!(!model.hears("Jimmy"));
        assert!(model.threshold() > 0.0 && model.threshold() < 1.0);
    }

    #[test]
    fn streaming_embeddings_are_the_ones_training_computed() {
        let want = floats(include_bytes!("../../tests/fixtures/wakeword/embeddings.f32"));
        let mut spotter = Spotter::new(Model::jatayu().unwrap());
        let clip = reference_clip();
        let mut worst = 0f32;
        for (k, chunk) in clip.as_chunks::<STEP>().0.iter().enumerate() {
            spotter.pending.extend(chunk.iter().map(|&s| f32::from(s)));
            let got = spotter.embed_step();
            for (a, b) in got.iter().zip(&want[k * EMBEDDING..(k + 1) * EMBEDDING]) {
                worst = worst.max((a - b).abs());
            }
        }
        assert_eq!(want.len(), clip.len() / STEP * EMBEDDING);
        assert!(worst < 2e-3, "embeddings differ by {worst}");
    }

    #[test]
    fn the_head_scores_as_training_scored() {
        let want = floats(include_bytes!("../../tests/fixtures/wakeword/scores.f32"));
        let mut spotter = Spotter::new(Model::jatayu().unwrap());
        let mut got = Vec::new();
        for chunk in reference_clip().as_chunks::<STEP>().0 {
            spotter.push(chunk);
            if let Some(score) = spotter.score_now() {
                got.push(score);
            }
        }
        assert_eq!(got.len(), want.len());
        for (a, b) in got.iter().zip(&want) {
            assert!((a - b).abs() < 1e-3, "{a} against {b}");
        }
    }

    #[test]
    fn silence_wakes_nothing_and_a_reset_starts_over() {
        let mut spotter = Spotter::new(Model::jatayu().unwrap());
        assert!(spotter.push(&vec![0i16; 16_000 * 10]).is_empty());
        assert_eq!(spotter.steps, 125);
        spotter.reset();
        assert_eq!(spotter.steps, 0);
        assert!(spotter.embeddings.is_empty());
    }

    /// How long a step takes: `cargo test --release ... -- --ignored
    /// --nocapture what_a_step_costs`. A step is 80 ms of audio.
    #[test]
    #[ignore = "a timing, for a release build"]
    fn what_a_step_costs() {
        let mut spotter = Spotter::new(Model::jatayu().unwrap());
        let clip = reference_clip();
        spotter.push(&clip);
        let began = std::time::Instant::now();
        for _ in 0..20 {
            spotter.push(&clip);
        }
        let steps = 20 * clip.len() / STEP;
        let ms = began.elapsed().as_secs_f64() * 1000.0 / steps as f64;
        println!("{ms:.3} ms a step, {:.1}% of one core", ms / 80.0 * 100.0);
    }

    /// The built-in model on recordings it never trained on, against what
    /// training's own code made of them: `scripts/wakeword/eval_wavs.py`
    /// writes the recordings and `manifest.jsonl`, and
    /// `JATAYU_TEST_WAKEWORD_DIR=<there> cargo test --release ... -- --ignored
    /// the_name_is_heard_in_recordings --nocapture` hears them as the app
    /// would, 30 ms at a time.
    #[test]
    #[ignore = "needs recordings made by scripts/wakeword/eval_wavs.py"]
    fn the_name_is_heard_in_recordings() {
        let Ok(dir) = std::env::var("JATAYU_TEST_WAKEWORD_DIR") else { return };
        let dir = std::path::PathBuf::from(dir);
        let manifest = std::fs::read_to_string(dir.join("manifest.jsonl")).unwrap();
        let model = Model::jatayu().unwrap();
        let (mut caught, mut names, mut woke, mut others, mut agree, mut total) =
            (0, 0, 0, 0, 0, 0);
        let mut worst = 0f32;
        for line in manifest.lines() {
            let entry: serde_json::Value = serde_json::from_str(line).unwrap();
            let bytes = std::fs::read(dir.join(entry["file"].as_str().unwrap())).unwrap();
            let clip = crate::speech::wav::decode(&bytes).unwrap();
            assert_eq!(clip.sample_rate, 16_000);
            let mut spotter = Spotter::new(model.clone());
            let mut found = Vec::new();
            let mut peak = 0f32;
            for block in clip.samples.chunks(480) {
                found.extend(spotter.push(block));
                if let Some(score) = spotter.score_now()
                    && spotter.steps > WARMUP
                {
                    peak = peak.max(score);
                }
            }
            let python = entry["peak"].as_f64().unwrap() as f32;
            worst = worst.max((peak - python).abs());
            let fired_python = entry["fired"].as_bool().unwrap();
            total += 1;
            agree += usize::from(fired_python == !found.is_empty());
            match entry["name_end"].as_u64() {
                Some(end) => {
                    names += 1;
                    let late = end + 20 * STEP as u64;
                    let early = end.saturating_sub(4 * STEP as u64);
                    caught +=
                        usize::from(found.iter().any(|h| (early..=late).contains(&h.end_sample)));
                }
                None => {
                    others += 1;
                    woke += usize::from(!found.is_empty());
                }
            }
        }
        println!(
            "caught {caught}/{names}, woke for {woke}/{others} others, agreed with training on \
             {agree}/{total}, peak scores within {worst:.4}"
        );
        assert!(worst < 0.02, "the app's scores are not training's: {worst}");
        assert!(
            agree * 100 >= total * 99,
            "hits differ from training's on {} recordings",
            total - agree
        );
    }

    #[test]
    fn a_file_that_is_not_a_model_is_refused() {
        assert!(Model::parse(b"").is_err());
        assert!(Model::parse(b"JWW1\x02\x00\x00\x00{}").is_err());
        let mut cut = JATAYU.to_vec();
        cut.truncate(cut.len() - 4);
        assert!(Model::parse(&cut).is_err());
    }
}
