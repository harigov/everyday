//! Spike: does cpal 0.18 actually give us two tracks -- the microphone and
//! whatever the computer is playing -- on this machine, the way
//! `docs/plans/meeting-notes.md`'s "Decisions already made" says it will
//! everywhere?
//!
//! Opens the default input device and, on the same call, the default
//! *output* device as an input stream -- the loopback trick the plan
//! describes: WASAPI enables its own loopback mode transparently, CoreAudio
//! opens a process tap, and PipeWire's host exposes a sink node as a duplex
//! device whose input stream is that sink's monitor (see `cpal`'s
//! `host/pipewire/device.rs`, around `STREAM_CAPTURE_SINK`). Records both to
//! 16 kHz mono WAV files and prints one RMS line per second per track, so a
//! silent system track is obvious without opening an audio editor.
//!
//! Run with a directory to write into and, optionally, how long to record:
//!
//! ```text
//! cargo run -p everyday-app --example capture -- /path/to/dir 5
//! ```
//!
//! While it runs, play something (`pw-play`, `paplay`, or
//! `speaker-test -t sine -f 440 -l 1`, kept short and quiet) and confirm the
//! system track is not silent.

use std::path::PathBuf;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SupportedStreamConfig};
use rubato::{Fft, FixedSync, Indexing, Resampler};

const TARGET_RATE: u32 = 16_000;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let out_dir: PathBuf = args.next().expect("usage: capture <out-dir> [seconds]").into();
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);
    std::fs::create_dir_all(&out_dir)?;

    let host = cpal::default_host();
    println!("Host: {} ({:?})", host.id().name(), host.id());

    let mic = open_track("mic", TrackKind::Input, &host, seconds);
    let system = open_track("system", TrackKind::Loopback, &host, seconds);

    println!("Recording {seconds}s from both tracks...");
    std::thread::sleep(Duration::from_secs(seconds) + Duration::from_millis(300));

    for (name, track) in [("mic", mic), ("system", system)] {
        match track {
            Ok(track) => {
                drop(track.stream);
                let raw = track.samples.lock().unwrap();
                println!(
                    "{name}: device={:?} channels={} source_rate={} samples={}",
                    track.device_name,
                    track.config.channels(),
                    track.config.sample_rate(),
                    raw.len()
                );
                print_rms_per_second(name, &raw, track.config.sample_rate());
                let resampled = resample_all(&raw, track.config.sample_rate(), TARGET_RATE)?;
                write_wav(&out_dir.join(format!("{name}.wav")), &resampled)?;
                println!(
                    "{name}: wrote {} resampled samples to {}",
                    resampled.len(),
                    out_dir.join(format!("{name}.wav")).display()
                );
            }
            Err(e) => println!("{name}: could not open -- {e}"),
        }
    }

    Ok(())
}

enum TrackKind {
    /// The default input device, opened the ordinary way.
    Input,
    /// The default *output* device, opened as an input -- the loopback
    /// trick. `supports_input()` is false for a plain output device on most
    /// backends, so the config comes from the output side instead; cpal's
    /// WASAPI and CoreAudio backends detect that the device is output-only
    /// and enable loopback capture themselves. On Linux's PipeWire host the
    /// device is exposed as duplex to begin with.
    Loopback,
}

struct OpenTrack {
    device_name: String,
    config: SupportedStreamConfig,
    samples: std::sync::Arc<std::sync::Mutex<Vec<f32>>>,
    stream: cpal::Stream,
}

fn open_track(
    label: &str,
    kind: TrackKind,
    host: &cpal::Host,
    _seconds: u64,
) -> anyhow::Result<OpenTrack> {
    let device = match kind {
        TrackKind::Input => host.default_input_device(),
        TrackKind::Loopback => host.default_output_device(),
    }
    .ok_or_else(|| anyhow::anyhow!("no default device for {label}"))?;
    let device_name = device.to_string();

    let config = match kind {
        TrackKind::Input => device.default_input_config()?,
        TrackKind::Loopback => {
            if device.supports_input() {
                device.default_input_config()?
            } else {
                device.default_output_config()?
            }
        }
    };
    println!("{label}: opening {device_name} ({config:?})");

    let channels = config.channels();
    let samples = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let samples_cb = samples.clone();
    let err_label = label.to_string();
    let err_fn = move |e: cpal::Error| eprintln!("{err_label}: stream error: {e}");

    macro_rules! build {
        ($t:ty) => {
            device.build_input_stream(
                config.clone().into(),
                move |data: &[$t], _: &cpal::InputCallbackInfo| {
                    let mono = downmix::<$t>(data, channels);
                    samples_cb.lock().unwrap().extend_from_slice(&mono);
                },
                err_fn,
                None,
            )?
        };
    }

    let stream = match config.sample_format() {
        SampleFormat::I8 => build!(i8),
        SampleFormat::I16 => build!(i16),
        SampleFormat::I32 => build!(i32),
        SampleFormat::F32 => build!(f32),
        other => anyhow::bail!("unsupported sample format {other} for {label}"),
    };
    stream.play()?;

    Ok(OpenTrack { device_name, config, samples, stream })
}

/// Interleaved samples of any cpal-supported format, down to mono `f32` in
/// `-1.0..=1.0`. Cheap and allocates once per callback, which is what a spike
/// tool can afford; the shipped `capture.rs` worker is stricter about it.
fn downmix<T>(data: &[T], channels: cpal::ChannelCount) -> Vec<f32>
where
    T: Sample,
    f32: FromSample<T>,
{
    let channels = channels.max(1) as usize;
    data.chunks(channels)
        .map(|frame| {
            let sum: f32 = frame.iter().map(|s| f32::from_sample(*s)).sum();
            sum / frame.len() as f32
        })
        .collect()
}

fn print_rms_per_second(label: &str, samples: &[f32], rate: cpal::SampleRate) {
    let window = rate.max(1) as usize;
    for (i, chunk) in samples.chunks(window).enumerate() {
        let rms = rms(chunk);
        println!(
            "{label}: t={i}s rms={rms:.5}{}",
            if rms < 0.001 { "  (near silence)" } else { "" }
        );
    }
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

/// Resample a whole in-memory mono buffer from `source_rate` to `target_rate`
/// with rubato's fixed-ratio FFT resampler. The shipped module does this
/// incrementally as audio arrives; a spike tool can afford to do it in one
/// pass over what was recorded.
fn resample_all(input: &[f32], source_rate: u32, target_rate: u32) -> anyhow::Result<Vec<i16>> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let chunk_size = 1024usize;
    let mut resampler = Fft::<f32>::new(
        source_rate as usize,
        target_rate as usize,
        chunk_size,
        1,
        FixedSync::Input,
    )
    .map_err(|e| anyhow::anyhow!("could not build resampler: {e}"))?;
    let out_len = resampler.output_frames_max();
    let mut out_scratch = vec![0.0f32; out_len];
    let mut out = Vec::new();

    let mut pos = 0usize;
    while pos + chunk_size <= input.len() {
        let in_adapter = audioadapter_buffers::direct::InterleavedSlice::new(
            &input[pos..pos + chunk_size],
            1,
            chunk_size,
        )?;
        let mut out_adapter =
            audioadapter_buffers::direct::InterleavedSlice::new_mut(&mut out_scratch, 1, out_len)?;
        let (nbr_in, nbr_out) =
            resampler.process_into_buffer(&in_adapter, &mut out_adapter, None)?;
        out.extend(out_scratch[..nbr_out].iter().map(|s| f32_to_i16(*s)));
        pos += nbr_in;
    }

    let remaining = input.len() - pos;
    if remaining > 0 {
        let mut tail = vec![0.0f32; chunk_size];
        tail[..remaining].copy_from_slice(&input[pos..]);
        let in_adapter = audioadapter_buffers::direct::InterleavedSlice::new(&tail, 1, chunk_size)?;
        let mut out_adapter =
            audioadapter_buffers::direct::InterleavedSlice::new_mut(&mut out_scratch, 1, out_len)?;
        let indexing = Indexing::new().partial_len(remaining);
        let (_nbr_in, nbr_out) =
            resampler.process_into_buffer(&in_adapter, &mut out_adapter, Some(&indexing))?;
        out.extend(out_scratch[..nbr_out].iter().map(|s| f32_to_i16(*s)));
    }

    Ok(out)
}

fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

fn write_wav(path: &std::path::Path, samples: &[i16]) -> anyhow::Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        writer.write_sample(s)?;
    }
    writer.finalize()?;
    Ok(())
}
