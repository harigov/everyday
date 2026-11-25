//! Taking other people's voices, and noise the ordinary filter leaves, out of
//! a microphone that listens for the person: two models run as it streams,
//! through sherpa-onnx's speech denoisers.
//!
//! - **Noise** ([`Cleanup::strong_noise`]): DPDFNet, a DeepFilterNet2 with
//!   dual-path RNN blocks, 16 kHz, its "2" size. Noise that comes and goes --
//!   a fan, dishes, a keyboard -- which the ordinary suppressor, which learns
//!   a steady noise floor, leaves in. About a tenth of real time on one
//!   thread. It changes some words of clean speech, so it is a choice, not a
//!   default.
//! - **Other voices** ([`Cleanup::voice_sample`]): VoxLock, a 27M causal
//!   band-split RNN that keeps the voice of one speaker and removes the rest,
//!   given a few seconds of that speaker's voice. The only thing here that
//!   takes out somebody talking *over* the person, which nothing downstream
//!   can: by the time a detector has cut an utterance, two voices in it are
//!   one sound.
//!
//! The app's audio engine runs one [`MicCleanup`] per microphone, on a thread
//! of its own, after echo cancellation; see `karmayoga-app`'s `audio::engine`.
//!
//! # What testing found
//!
//! Measured on Hari's own recording with a podcast mixed under it, through
//! the detector and Parakeet as voice mode hears it
//! (`docs/plans/mic-filtering.md`):
//!
//! - With talk 12 dB under the voice, VoxLock took the word error rate
//!   against the clean transcript from 0.73 to 0.12; 6 dB under, from 0.76
//!   to 0.39. Over clean speech it changed about one word in eight.
//! - Started cold, it muted the first several seconds of the person: its
//!   recurrent state has to learn who is talking. Fed two seconds of their
//!   own voice first ([`PRIME_MS`]) it did not, and more than two bought
//!   nothing over the person's own speech.
//! - What it holds on to is the voice it has lately heard that matches the
//!   sample. After the person talks, somebody else talking alone is cut by
//!   30 dB or more for about 45 seconds, then let back in. Somebody talking
//!   alone before the person has said anything is not cut at all, so a
//!   television on while they are quiet is the owner's voice check's to
//!   leave out ([`super::owner`]), not this. Feeding it the whole sample
//!   first held on longer, but cost five seconds before the microphone
//!   answered.
//! - How it is told the voice matters most of all. It enrols on the loudest
//!   three seconds of what it is given; given just three seconds, cut from a
//!   recording, it kept the podcast and dropped Hari. Given twelve to fifteen
//!   it found him every time. So the whole kept sample goes to it.
//! - Speed: one thread, 30 ms at a time, was slower than real time. Two
//!   threads, 80 ms at a time ([`CHUNK`]), is under half of it.
//!
//! # The voice sample on disk
//!
//! sherpa-onnx 2.0.1 reads a voice sample only from a WAV file, once, while
//! the denoiser is made. The person's voice is sealed in the vault the rest
//! of the time, so it is written to a private folder of this machine's,
//! read, and removed at once -- and anything left there by a process that
//! died between the two is removed the next time.

use super::{SAMPLE_RATE, pcm};
use crate::error::{CommandError, CommandResult, codes};
use crate::local::files;
use std::path::{Path, PathBuf};

/// What to run over a microphone. See the module doc.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cleanup {
    /// DPDFNet over the noise.
    pub strong_noise: bool,
    /// VoxLock, keeping this voice: the owner's kept sample, at
    /// [`SAMPLE_RATE`].
    pub voice_sample: Option<Vec<i16>>,
}

/// How much audio each model is handed at a time: 80 ms. Smaller is slower
/// than real time for VoxLock on one desktop thread; see the module doc.
pub const CHUNK: usize = (SAMPLE_RATE as usize * 80) / 1000;

/// How much of the owner's own voice VoxLock hears before the microphone,
/// so its state is of them talking: 2 s.
pub const PRIME_MS: u64 = 2_000;

/// VoxLock's threads. One is not enough, and more than two buys little for
/// a model that runs as long as a microphone is open.
const VOXLOCK_THREADS: i32 = 2;

/// DPDFNet, loaded. Quick to load (a tenth of a second), so the engine loads
/// it before deciding whether its own noise suppression can stand aside.
pub struct NoiseFilter(sherpa_onnx::OnlineSpeechDenoiser);

impl NoiseFilter {
    /// Fails when it is not downloaded or will not load. Blocking.
    pub fn load() -> CommandResult<NoiseFilter> {
        noise_filter().map(NoiseFilter)
    }
}

/// VoxLock, loaded, enrolled on the person's voice and primed with it.
pub struct VoiceIsolation(sherpa_onnx::OnlineSpeechDenoiser);

impl VoiceIsolation {
    /// Fails when it is not downloaded, will not load, or `sample` is too
    /// short. Blocking: about two seconds.
    pub fn load(sample: &[i16]) -> CommandResult<VoiceIsolation> {
        voice_isolation(sample).map(VoiceIsolation)
    }
}

/// The models there are, in order, ready for audio.
pub struct MicCleanup {
    noise: Option<NoiseFilter>,
    voices: Option<VoiceIsolation>,
    /// Microphone audio waiting to make a [`CHUNK`].
    pending: Vec<f32>,
}

impl MicCleanup {
    /// The noise filter, then voice isolation, whichever are given. `None`
    /// when neither is.
    pub fn new(noise: Option<NoiseFilter>, voices: Option<VoiceIsolation>) -> Option<MicCleanup> {
        (noise.is_some() || voices.is_some()).then(|| MicCleanup {
            noise,
            voices,
            pending: Vec::with_capacity(CHUNK * 2),
        })
    }

    /// Load what `cleanup` asks for. `None` when it asks for nothing. Fails
    /// when a model it asks for is not downloaded or will not load. Blocking:
    /// loading and priming take about two seconds.
    pub fn build(cleanup: &Cleanup) -> CommandResult<Option<MicCleanup>> {
        let noise = if cleanup.strong_noise { Some(NoiseFilter::load()?) } else { None };
        let voices = match &cleanup.voice_sample {
            Some(sample) => Some(VoiceIsolation::load(sample)?),
            None => None,
        };
        Ok(MicCleanup::new(noise, voices))
    }

    fn chain(&self) -> impl Iterator<Item = &sherpa_onnx::OnlineSpeechDenoiser> {
        self.noise.iter().map(|n| &n.0).chain(self.voices.iter().map(|v| &v.0))
    }

    /// Clean `samples`, 16 kHz mono, appending what is ready to `out`. Audio
    /// goes in and comes out at the same rate; a little is held back until a
    /// whole [`CHUNK`] has arrived.
    pub fn process(&mut self, samples: &[i16], out: &mut Vec<i16>) {
        self.pending.extend(pcm::to_f32(samples));
        let whole = self.pending.len() / CHUNK * CHUNK;
        if whole == 0 {
            return;
        }
        let mut audio: Vec<f32> = self.pending.drain(..whole).collect();
        for denoiser in self.chain() {
            audio = denoiser.run(&audio, SAMPLE_RATE as i32).samples;
        }
        out.extend(pcm::to_i16(&audio));
    }

    /// The microphone has closed: everything still held -- the part of a
    /// chunk waiting for the rest, and what each model holds back to look
    /// ahead -- is cleaned and appended to `out`.
    pub fn finish(&mut self, out: &mut Vec<i16>) {
        let mut audio: Vec<f32> = std::mem::take(&mut self.pending);
        for denoiser in self.chain() {
            let mut cleaned = if audio.is_empty() {
                Vec::new()
            } else {
                denoiser.run(&audio, SAMPLE_RATE as i32).samples
            };
            cleaned.extend(denoiser.flush().samples);
            audio = cleaned;
        }
        out.extend(pcm::to_i16(&audio));
    }
}

fn noise_filter() -> CommandResult<sherpa_onnx::OnlineSpeechDenoiser> {
    let model = files::noise_filter_path().ok_or_else(|| missing("The stronger noise filter"))?;
    let config = sherpa_onnx::OnlineSpeechDenoiserConfig {
        model: sherpa_onnx::OfflineSpeechDenoiserModelConfig {
            dpdfnet: sherpa_onnx::OfflineSpeechDenoiserDpdfNetModelConfig {
                model: Some(path_string(&model)),
                attenuation_limit_db: 0.0,
            },
            num_threads: 1,
            ..Default::default()
        },
    };
    sherpa_onnx::OnlineSpeechDenoiser::create(&config)
        .ok_or_else(|| CommandError::new(codes::INTERNAL, "could not load the noise filter"))
}

fn voice_isolation(sample: &[i16]) -> CommandResult<sherpa_onnx::OnlineSpeechDenoiser> {
    let paths = files::voice_isolation_paths().ok_or_else(|| missing("Removing other voices"))?;
    if sample.len() < (SAMPLE_RATE * 3) as usize {
        return Err(CommandError::new(
            codes::INVALID,
            "The recording of your voice is too short to tell it from others. Record it again.",
        ));
    }
    let held = HeldSample::write_in(&HeldSample::dir(), sample)?;
    let config = sherpa_onnx::OnlineSpeechDenoiserConfig {
        model: sherpa_onnx::OfflineSpeechDenoiserModelConfig {
            voxlock: sherpa_onnx::OfflineSpeechDenoiserVoxLockModelConfig {
                enroll_model: Some(path_string(&paths.enroll)),
                chunk_model: Some(path_string(&paths.chunk)),
                voice_sample: Some(path_string(held.path())),
            },
            num_threads: VOXLOCK_THREADS,
            ..Default::default()
        },
    };
    let denoiser = sherpa_onnx::OnlineSpeechDenoiser::create(&config);
    // Read while it was made: not needed on disk a moment longer.
    drop(held);
    let denoiser = denoiser.ok_or_else(|| {
        CommandError::new(codes::INTERNAL, "could not load the model that removes other voices")
    })?;
    // Its own voice first, output thrown away: see the module doc.
    let prime = pcm::to_f32(&sample[..pcm::sample_count(PRIME_MS)]);
    for chunk in prime.chunks(CHUNK) {
        let _ = denoiser.run(chunk, SAMPLE_RATE as i32);
    }
    Ok(denoiser)
}

fn missing(what: &str) -> CommandError {
    CommandError::new(
        codes::INVALID,
        format!("{what} needs its model, which has not been downloaded yet."),
    )
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// How old a voice sample on disk must be to have been left behind by a
/// process that died: reading one takes well under a second.
const LEFT_BEHIND: std::time::Duration = std::time::Duration::from_secs(60);

/// The owner's voice as a WAV file, for as long as this lives. See the module
/// doc.
struct HeldSample {
    path: PathBuf,
}

impl HeldSample {
    fn dir() -> PathBuf {
        karmayoga_vault::data_dir().join("voice-sample")
    }

    fn write_in(dir: &Path, sample: &[i16]) -> CommandResult<HeldSample> {
        let failed = |e: String| {
            CommandError::new(codes::INTERNAL, format!("could not hand over your voice: {e}"))
        };
        karmayoga_core::fsutil::create_private_dir(dir).map_err(|e| failed(e.to_string()))?;
        // Whatever a process that died before removing its own left behind:
        // anything older than another microphone opening at the same moment
        // could still be reading.
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let stale = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|at| at.elapsed().ok())
                    .is_some_and(|age| age > LEFT_BEHIND);
                if stale {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        let path = dir.join(format!("{}.wav", karmayoga_core::fsutil::unique_tag()));
        let bytes = super::wav::encode(sample, SAMPLE_RATE);
        karmayoga_core::fsutil::write_atomic_private(
            &path,
            &bytes,
            &karmayoga_core::fsutil::unique_tag(),
        )
        .map_err(|e| failed(e.to_string()))?;
        Ok(HeldSample { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for HeldSample {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_asked_for_is_nothing_loaded() {
        assert!(MicCleanup::build(&Cleanup::default()).unwrap().is_none());
    }

    #[test]
    fn audio_waits_for_a_whole_chunk() {
        // No models: the chain is empty, so what comes out is what went in,
        // in whole chunks.
        // No models: nothing to make, so built by hand.
        let mut cleanup = MicCleanup { noise: None, voices: None, pending: Vec::new() };
        let mut out = Vec::new();
        cleanup.process(&vec![1000i16; CHUNK - 10], &mut out);
        assert!(out.is_empty());
        cleanup.process(&[1000i16; 20], &mut out);
        assert_eq!(out.len(), CHUNK);
        assert!(out.iter().all(|&s| (s - 1000).abs() <= 1), "{:?}", &out[..4]);
    }

    #[test]
    fn what_is_held_comes_out_when_the_microphone_closes() {
        let mut cleanup = MicCleanup { noise: None, voices: None, pending: Vec::new() };
        let mut out = Vec::new();
        cleanup.process(&[500i16; 100], &mut out);
        assert!(out.is_empty());
        cleanup.finish(&mut out);
        assert_eq!(out, vec![500i16; 100]);
    }

    #[test]
    fn the_voice_sample_does_not_outlive_its_reading() {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("voice-sample");
        std::fs::create_dir_all(&left).unwrap();
        std::fs::write(left.join("old.wav"), b"left by a crash").unwrap();
        let old = std::fs::File::options().write(true).open(left.join("old.wav")).unwrap();
        old.set_modified(std::time::SystemTime::now() - LEFT_BEHIND * 2).unwrap();
        std::fs::write(left.join("another.wav"), b"another microphone's, being read").unwrap();
        let held = HeldSample::write_in(&left, &vec![100i16; SAMPLE_RATE as usize]).unwrap();
        assert!(!left.join("old.wav").exists(), "a leftover is removed");
        assert!(left.join("another.wav").exists(), "one being read now is not");
        let path = held.path().to_path_buf();
        assert!(path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "readable by others: {mode:o}");
        }
        drop(held);
        assert!(!path.exists());
    }
}
