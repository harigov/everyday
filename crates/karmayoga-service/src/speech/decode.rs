//! A recording's sound as 16 kHz mono samples, read a stretch at a time: what
//! a podcast episode, a voice memo or a film is turned into to be
//! transcribed. See `docs/plans/podcasts.md` (phase 2).
//!
//! Through `symphonia`, in pure Rust: MP3, AAC in MP4 and M4A (a video's
//! sound track included), Ogg Vorbis, FLAC and WAV. Opus is not among them;
//! nothing the Feed transcribes is published as Opus, and what it renders as
//! Opus it wrote from text in the first place.
//!
//! Streamed, never whole: two hours of 16 kHz samples is a quarter of a
//! gigabyte, and the transcriber only ever wants a stretch at a time.
//!
//! # How the sound is made fit to be listened to
//!
//! Every channel is averaged into one, which is all a recogniser wants and
//! what [`crate::speech::wav::decode`] does for a WAV from a server. The
//! rate is brought to [`crate::speech::SAMPLE_RATE`] by the converter the
//! rest of speech uses ([`StreamResampler`]), with the delay it puts in front
//! of the sound taken off again, so that a word said at 1:12:03 is at
//! 1:12:03 in what is handed over: a transcript is timed by where in this
//! stream a word is.
//!
//! A frame the decoder cannot make sense of is stepped over -- an MP3 that was
//! cut from a stream begins mid-frame, and a downloaded one may have a bad
//! patch -- and only a recording that is wrecked from end to end is refused.

use super::SAMPLE_RATE;
use super::pcm;
use super::resample::StreamResampler;
use crate::error::{CommandError, CommandResult, codes};
use std::ops::ControlFlow;
use std::path::Path;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, Decoder, DecoderOptions};
use symphonia::core::errors::Error as DecodeFault;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// How many packets in a row may fail to decode before the recording is given
/// up on. A few bad frames are a bad patch; this many is not an audio file.
const MAX_BAD_PACKETS: u32 = 200;

/// What reading a recording found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decoded {
    /// How much sound was read, in milliseconds.
    pub duration_ms: u64,
}

/// Read `path`'s first audio track, mixed to mono and resampled to
/// [`crate::speech::SAMPLE_RATE`], handing `each` about `chunk_ms` of samples
/// at a time, in order. `each` answers `Break` to stop early. Refuses, in a
/// sentence, what cannot be read.
///
/// Every chunk but the last is exactly `chunk_ms` long. When `each` stops it
/// the duration is of what had been read by then, which is what had been
/// handed over and the chunk it stopped at.
///
/// Blocking, and CPU-bound: a two-hour episode is a minute of it. Call it on
/// the blocking pool.
pub fn decode_file(
    path: &Path,
    chunk_ms: u32,
    each: &mut dyn FnMut(&[i16]) -> ControlFlow<()>,
) -> CommandResult<Decoded> {
    let mut opened = open(path)?;
    let chunk = pcm::sample_count(u64::from(chunk_ms.max(1))).max(1);
    let mut sound = Sound::new(chunk);
    let mut bad_in_a_row = 0u32;
    let mut frames: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match opened.format.next_packet() {
            Ok(packet) => packet,
            // The end of the file is how symphonia says the file has ended.
            Err(DecodeFault::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            // A stream that begins again with new parameters (a chained Ogg).
            // What came before is what was asked for.
            Err(DecodeFault::ResetRequired) => break,
            Err(e) => return Err(unreadable(&e)),
        };
        if packet.track_id() != opened.track {
            continue;
        }
        let decoded = match opened.decoder.decode(&packet) {
            Ok(decoded) => {
                bad_in_a_row = 0;
                decoded
            }
            Err(DecodeFault::DecodeError(_) | DecodeFault::IoError(_)) => {
                bad_in_a_row += 1;
                if bad_in_a_row > MAX_BAD_PACKETS {
                    return Err(CommandError::new(
                        codes::UNREADABLE,
                        "This recording could not be read: it is too damaged to make out.",
                    ));
                }
                continue;
            }
            Err(e) => return Err(unreadable(&e)),
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1);
        let buffer = match frames.take() {
            Some(buffer) if buffer.capacity() >= decoded.capacity() * channels => buffer,
            _ => SampleBuffer::<f32>::new(decoded.capacity() as u64, spec),
        };
        let buffer = frames.insert(buffer);
        buffer.copy_interleaved_ref(decoded);
        let mono = mix_to_mono(buffer.samples(), channels);
        if sound.push(&mono, spec.rate, each)?.is_break() {
            return Ok(Decoded { duration_ms: sound.duration_ms() });
        }
    }
    sound.finish(each)?;
    Ok(Decoded { duration_ms: sound.duration_ms() })
}

/// How long `path`'s first audio track is, in milliseconds, when the file says
/// so without being read through. `None` for one that does not -- an MP3
/// with no length in its header -- or cannot be opened at all.
///
/// What a transcription's progress is measured against.
pub fn duration_of(path: &Path) -> Option<u64> {
    let opened = open(path).ok()?;
    let params = opened.format.tracks().iter().find(|t| t.id == opened.track)?.codec_params.clone();
    let frames = params.n_frames?;
    let time = params.time_base?.calc_time(frames);
    Some(time.seconds * 1000 + (time.frac * 1000.0) as u64)
}

/// A recording opened and ready to be read from.
struct Opened {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    /// The track being read.
    track: u32,
}

fn open(path: &Path) -> CommandResult<Opened> {
    let file = std::fs::File::open(path).map_err(|e| {
        let (code, why) = match e.kind() {
            std::io::ErrorKind::NotFound => (codes::NOT_FOUND, "it is not there".to_string()),
            _ => (codes::IO, e.to_string()),
        };
        CommandError::new(code, format!("This recording could not be opened: {why}."))
    })?;
    let source = MediaSourceStream::new(Box::new(file), Default::default());
    // The extension is only a hint, and a wrong one costs a slower probe.
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, source, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| unreadable(&e))?;
    let format = probed.format;
    // A film has a picture track too, which has no codec symphonia knows.
    let track = format
        .tracks()
        .iter()
        .find(|t| {
            t.codec_params.codec != CODEC_TYPE_NULL
                && (t.codec_params.sample_rate.is_some() || t.codec_params.channels.is_some())
        })
        .ok_or_else(|| {
            CommandError::new(
                codes::UNREADABLE,
                "This recording could not be read: it has no sound.",
            )
        })?;
    let id = track.id;
    let decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| unreadable(&e))?;
    Ok(Opened { format, decoder, track: id })
}

/// What symphonia said, as the sentence a person is told.
fn unreadable(fault: &DecodeFault) -> CommandError {
    let why = match fault {
        DecodeFault::Unsupported(_) => "its format is not one this app can read. It reads MP3, \
             M4A and MP4, Ogg Vorbis, FLAC and WAV"
            .to_string(),
        DecodeFault::IoError(e) => e.to_string(),
        DecodeFault::DecodeError(why) => format!("it is damaged ({why})"),
        DecodeFault::SeekError(_) | DecodeFault::ResetRequired => "it could not be read".into(),
        DecodeFault::LimitError(why) => format!("it is too large or too deeply nested ({why})"),
    };
    CommandError::new(codes::UNREADABLE, format!("This recording could not be read: {why}."))
}

/// Interleaved frames of `channels` as one sample each, the channels averaged.
fn mix_to_mono(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels == 1 {
        return interleaved.to_vec();
    }
    interleaved.chunks_exact(channels).map(|f| f.iter().sum::<f32>() / channels as f32).collect()
}

/// The samples read so far, on their way to being handed over in whole chunks
/// at the rate speech wants.
struct Sound {
    /// Samples in a chunk.
    chunk: usize,
    /// Converted, and not yet a whole chunk.
    waiting: Vec<i16>,
    /// The converter, and the rate it converts from.
    resampler: Option<(StreamResampler, u32)>,
    /// What the converter still has to throw away of its start-up delay.
    skip: usize,
    /// Everything handed over or waiting.
    total: usize,
}

impl Sound {
    fn new(chunk: usize) -> Self {
        Self { chunk, waiting: Vec::new(), resampler: None, skip: 0, total: 0 }
    }

    fn duration_ms(&self) -> u64 {
        pcm::duration_ms(self.total)
    }

    /// Take more mono samples at `rate`, and hand over every chunk they
    /// complete.
    fn push(
        &mut self,
        mono: &[f32],
        rate: u32,
        each: &mut dyn FnMut(&[i16]) -> ControlFlow<()>,
    ) -> CommandResult<ControlFlow<()>> {
        if self.resampler.as_ref().is_none_or(|(_, from)| *from != rate) {
            // Only a recording that changes its rate half way through gets
            // here with one already. What the old converter still held is the
            // end of what came before, and is handed over first: dropped, it
            // would be up to a converter's chunk of the recording gone, and
            // every time after it would be that much early.
            let held = self.flush_resampler()?;
            if self.take(&held, each).is_break() {
                return Ok(ControlFlow::Break(()));
            }
            let resampler = StreamResampler::new(rate, SAMPLE_RATE).map_err(|e| {
                CommandError::new(
                    codes::UNREADABLE,
                    format!(
                        "This recording could not be read: its sound is at {rate} Hz, and {e}."
                    ),
                )
            })?;
            self.skip = pcm::sample_count(u64::from(resampler.delay_ms()));
            self.resampler = Some((resampler, rate));
        }
        let mut converted = Vec::new();
        let Some((resampler, _)) = self.resampler.as_mut() else {
            return Ok(ControlFlow::Continue(()));
        };
        resampler.push(mono, &mut converted).map_err(resample_failed)?;
        Ok(self.take(&converted, each))
    }

    /// The end: the converter's last partial chunk, and the last chunk, however
    /// short.
    fn finish(&mut self, each: &mut dyn FnMut(&[i16]) -> ControlFlow<()>) -> CommandResult<()> {
        let flushed = self.flush_resampler()?;
        if self.take(&flushed, each).is_break() {
            return Ok(());
        }
        if !self.waiting.is_empty() {
            let last = std::mem::take(&mut self.waiting);
            let _ = each(&last);
        }
        Ok(())
    }

    fn flush_resampler(&mut self) -> CommandResult<Vec<f32>> {
        let mut out = Vec::new();
        if let Some((resampler, _)) = self.resampler.as_mut() {
            resampler.flush(&mut out).map_err(resample_failed)?;
        }
        self.resampler = None;
        Ok(out)
    }

    /// Add converted samples, dropping what is still the converter's delay,
    /// and hand over every whole chunk.
    fn take(
        &mut self,
        converted: &[f32],
        each: &mut dyn FnMut(&[i16]) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        let skipped = self.skip.min(converted.len());
        self.skip -= skipped;
        let samples = pcm::to_i16(&converted[skipped..]);
        self.total += samples.len();
        self.waiting.extend_from_slice(&samples);
        while self.waiting.len() >= self.chunk {
            let rest = self.waiting.split_off(self.chunk);
            let chunk = std::mem::replace(&mut self.waiting, rest);
            if each(&chunk).is_break() {
                return ControlFlow::Break(());
            }
        }
        ControlFlow::Continue(())
    }
}

fn resample_failed(e: super::resample::ResampleError) -> CommandError {
    CommandError::new(codes::UNREADABLE, format!("This recording could not be read: {e}."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// A recording of `channels` channels at `rate`, 16 bits, `seconds` long,
    /// each channel's samples from `wave(channel, time_in_seconds)`.
    fn wav(rate: u32, channels: u16, seconds: f32, wave: impl Fn(u16, f32) -> f32) -> Vec<u8> {
        let frames = (rate as f32 * seconds) as usize;
        let mut data = Vec::with_capacity(frames * usize::from(channels) * 2);
        for frame in 0..frames {
            for channel in 0..channels {
                let sample = (wave(channel, frame as f32 / rate as f32) * 32767.0) as i16;
                data.extend_from_slice(&sample.to_le_bytes());
            }
        }
        let block = channels * 2;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
        out.extend_from_slice(&block.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    fn tone(hertz: f32) -> impl Fn(u16, f32) -> f32 {
        move |_, t| (2.0 * PI * hertz * t).sin() * 0.5
    }

    fn file(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// All of a file as one run of samples, and how it was cut.
    fn read(path: &Path, chunk_ms: u32) -> (Vec<i16>, Vec<usize>, Decoded) {
        let (mut all, mut sizes) = (Vec::new(), Vec::new());
        let decoded = decode_file(path, chunk_ms, &mut |chunk| {
            all.extend_from_slice(chunk);
            sizes.push(chunk.len());
            ControlFlow::Continue(())
        })
        .unwrap();
        (all, sizes, decoded)
    }

    /// How many times a tone crosses zero going up, which is its pitch in
    /// hertz over a second.
    fn rising_crossings(samples: &[i16]) -> usize {
        samples.windows(2).filter(|w| w[0] < 0 && w[1] >= 0).count()
    }

    fn peak(samples: &[i16]) -> i32 {
        samples.iter().map(|&s| i32::from(s).abs()).max().unwrap_or(0)
    }

    #[test]
    fn a_recording_at_16_khz_comes_through_sample_for_sample() {
        let dir = tempfile::tempdir().unwrap();
        let samples: Vec<i16> = (0..24_000).map(|i| ((i * 37) % 20_000) as i16 - 10_000).collect();
        let path = file(&dir, "memo.wav", &super::super::wav::encode(&samples, SAMPLE_RATE));
        let (all, _, decoded) = read(&path, 1_000);
        assert_eq!(all, samples);
        assert_eq!(decoded.duration_ms, 1_500);
    }

    #[test]
    fn a_recording_at_44_khz_is_brought_to_16_khz_in_pitch_length_and_time() {
        let dir = tempfile::tempdir().unwrap();
        // Silence, then a 440 Hz tone: where the tone starts must stay put.
        let wave = |_: u16, t: f32| if t < 1.0 { 0.0 } else { (2.0 * PI * 440.0 * t).sin() * 0.5 };
        let path = file(&dir, "show.wav", &wav(44_100, 1, 3.0, wave));
        let (all, _, decoded) = read(&path, 1_000);

        assert!(decoded.duration_ms.abs_diff(3_000) <= 30, "{} ms", decoded.duration_ms);
        assert!(all.len().abs_diff(48_000) <= 500, "{} samples", all.len());
        let quiet = &all[..15_000];
        assert!(peak(quiet) < 600, "silence stayed silent: {}", peak(quiet));
        let tone = &all[17_000..];
        let seconds = tone.len() as f32 / SAMPLE_RATE as f32;
        let hertz = rising_crossings(tone) as f32 / seconds;
        assert!((hertz - 440.0).abs() < 8.0, "{hertz} Hz");
        assert!((peak(tone) - 16_384).abs() < 2_000, "peak {}", peak(tone));
        // The tone began at one second, and the first loud sample is there.
        let began = all.iter().position(|s| s.abs() > 3_000).unwrap();
        assert!(began.abs_diff(16_000) < 160, "began at sample {began}");
    }

    #[test]
    fn two_channels_are_averaged_into_one() {
        let dir = tempfile::tempdir().unwrap();
        // The tone in the left ear only: half as loud once mixed.
        let left = |channel: u16, t: f32| {
            if channel == 0 { (2.0 * PI * 300.0 * t).sin() * 0.8 } else { 0.0 }
        };
        let path = file(&dir, "stereo.wav", &wav(44_100, 2, 2.0, left));
        let (all, _, _) = read(&path, 1_000);
        assert!((peak(&all[4_000..]) - 13_100).abs() < 1_500, "peak {}", peak(&all[4_000..]));

        // The same tone in both: as loud as it was.
        let both = |_: u16, t: f32| (2.0 * PI * 300.0 * t).sin() * 0.8;
        let path = file(&dir, "both.wav", &wav(48_000, 2, 2.0, both));
        let (all, _, _) = read(&path, 1_000);
        assert!((peak(&all[4_000..]) - 26_200).abs() < 2_500, "peak {}", peak(&all[4_000..]));
    }

    #[test]
    fn every_chunk_but_the_last_is_the_length_asked_for_and_none_is_lost() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "a.wav", &wav(22_050, 1, 2.5, tone(200.0)));
        let (all, sizes, decoded) = read(&path, 600);
        let (last, whole) = sizes.split_last().unwrap();
        assert!(whole.iter().all(|&n| n == 9_600), "{sizes:?}");
        assert!(*last <= 9_600 && *last > 0, "{sizes:?}");
        assert_eq!(all.len(), sizes.iter().sum::<usize>());
        assert_eq!(decoded.duration_ms, pcm::duration_ms(all.len()));
        assert!(decoded.duration_ms.abs_diff(2_500) <= 30, "{} ms", decoded.duration_ms);
    }

    #[test]
    fn a_chunk_asked_for_of_no_length_is_a_millisecond_and_not_a_loop() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "a.wav", &wav(16_000, 1, 0.05, tone(200.0)));
        let (_, sizes, _) = read(&path, 0);
        assert_eq!(sizes.iter().sum::<usize>(), 800);
        assert!(sizes.iter().all(|&n| n == 16));
    }

    #[test]
    fn stopping_early_stops_reading_and_says_how_far_it_got() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "long.wav", &wav(16_000, 1, 10.0, tone(200.0)));
        let mut calls = 0;
        let decoded = decode_file(&path, 1_000, &mut |_| {
            calls += 1;
            if calls == 3 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        })
        .unwrap();
        assert_eq!(calls, 3);
        assert!(decoded.duration_ms >= 3_000 && decoded.duration_ms < 4_000, "{decoded:?}");
    }

    #[test]
    fn the_length_is_known_from_the_header_without_reading_it_through() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "a.wav", &wav(44_100, 2, 2.0, tone(200.0)));
        assert_eq!(duration_of(&path), Some(2_000));
        assert_eq!(duration_of(&dir.path().join("missing.wav")), None);
        assert_eq!(duration_of(&file(&dir, "words.txt", b"not sound")), None);
    }

    #[test]
    fn a_wrong_or_missing_extension_still_finds_the_format() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = wav(16_000, 1, 0.5, tone(200.0));
        for name in ["episode.mp3", "episode", "episode.bin"] {
            let (all, _, _) = read(&file(&dir, name, &bytes), 1_000);
            assert_eq!(all.len(), 8_000, "{name}");
        }
    }

    #[test]
    fn what_is_not_sound_is_refused_in_a_sentence() {
        let dir = tempfile::tempdir().unwrap();
        let mut never = |_: &[i16]| ControlFlow::Continue(());
        for (name, bytes) in [
            ("page.mp3", &b"<html><body>Sorry, no episode here.</body></html>"[..]),
            ("empty.wav", &b""[..]),
            ("riff.wav", &b"RIFF\x00\x00\x00\x00WAVE"[..]),
        ] {
            let e = decode_file(&file(&dir, name, bytes), 1_000, &mut never).unwrap_err();
            assert_eq!(e.code, codes::UNREADABLE, "{name}");
            assert!(e.message.starts_with("This recording could not be read: "), "{}", e.message);
            assert!(e.message.ends_with('.'), "{}", e.message);
        }
    }

    #[test]
    fn a_recording_that_is_not_there_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let e =
            decode_file(&dir.path().join("gone.mp3"), 1_000, &mut |_| ControlFlow::Continue(()))
                .unwrap_err();
        assert_eq!(e.code, codes::NOT_FOUND);
        assert_eq!(e.message, "This recording could not be opened: it is not there.");
    }

    #[test]
    fn a_recording_cut_short_gives_what_it_has() {
        let dir = tempfile::tempdir().unwrap();
        // A download that stopped half way: the header says two seconds and
        // one is there.
        let mut bytes = wav(16_000, 1, 2.0, tone(200.0));
        bytes.truncate(bytes.len() / 2);
        let (all, _, decoded) = read(&file(&dir, "cut.wav", &bytes), 1_000);
        assert!(all.len() > 10_000 && all.len() < 17_000, "{} samples", all.len());
        assert_eq!(decoded.duration_ms, pcm::duration_ms(all.len()));
    }

    #[test]
    fn what_the_converter_held_when_the_rate_changes_is_not_lost() {
        let tone = |rate: u32, seconds: f32| -> Vec<f32> {
            (0..(rate as f32 * seconds) as usize)
                .map(|i| (2.0 * PI * 300.0 * i as f32 / rate as f32).sin() * 0.5)
                .collect()
        };
        let mut handed = 0usize;
        let mut sound = Sound::new(pcm::sample_count(1_000));
        let mut each = |chunk: &[i16]| {
            handed += chunk.len();
            ControlFlow::Continue(())
        };
        // A second at one rate, in pieces that are not a whole converter chunk
        // each, then a second at another.
        for piece in tone(44_100, 1.0).chunks(1_500) {
            assert!(sound.push(piece, 44_100, &mut each).unwrap().is_continue());
        }
        for piece in tone(48_000, 1.0).chunks(1_500) {
            assert!(sound.push(piece, 48_000, &mut each).unwrap().is_continue());
        }
        sound.finish(&mut each).unwrap();
        let total = sound.total;
        assert_eq!(handed, total, "everything counted was handed over");
        // Two seconds. Nothing of the first is dropped when the rate changes;
        // the converter pads what it held out to a whole chunk, so there may be
        // a few hundredths of a second more, and never less.
        assert!(total >= 32_000 - 60, "{total} samples: some of it was lost");
        assert!(total <= 32_000 + 400, "{total} samples");
    }
}
