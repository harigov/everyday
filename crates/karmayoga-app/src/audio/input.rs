//! Opening an input stream, for everything that listens.
//!
//! Meeting capture's microphone and call track, and the audio engine's
//! microphone, are the same thing to `cpal`: an input stream in whichever
//! sample format the device runs, which has to become mono `f32` and reach a
//! thread that is allowed to be slow. What each does with a block once it has
//! one, and what an error callback tells its thread, differs, so those are
//! the caller's: it passes a closure for each.
//!
//! # Realtime rules
//!
//! The callback must not allocate and must not block. Downmixing a block still
//! needs somewhere to write its samples, and that `Vec` has to move out of the
//! callback whole -- ownership, not a copy -- to reach another thread through
//! a channel without a second allocation there. The two together mean a fresh
//! buffer is needed every callback unless *something* hands old ones back.
//! That something is the `SyncSender<Vec<f32>>` [`open_input`] returns
//! alongside the stream: the receiving thread sends a buffer back through it
//! once done reading from it, the callback's own `Receiver` half draws from
//! that pool first, and only allocates -- `unwrap_or_default` -- on the rare
//! block where the pool is empty (cold start, or a run of blocks the other
//! thread has not caught up on yet). Steady state, once the pool has warmed
//! up, costs no allocation at all.
//!
//! A caller that has no steady state to speak of (a one-shot clip) may simply
//! never return buffers: running dry falls back to allocating.

use std::sync::mpsc::{Receiver, SyncSender};

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, SupportedStreamConfig};

use super::AudioError;

/// Interleaved samples of any `cpal`-supported format, down to mono `f32` in
/// `-1.0..=1.0`, written into `out` (cleared first, so its capacity is kept
/// rather than freed) instead of being returned as a fresh `Vec` -- see the
/// module's own doc on why the realtime callback must not allocate one every
/// block.
pub fn downmix_into<T>(data: &[T], channels: cpal::ChannelCount, out: &mut Vec<f32>)
where
    T: Sample,
    f32: FromSample<T>,
{
    let channels = channels.max(1) as usize;
    out.clear();
    out.extend(data.chunks(channels).map(|frame| {
        let sum: f32 = frame.iter().map(|s| f32::from_sample(*s)).sum();
        sum / frame.len() as f32
    }));
}

/// A sample as 16-bit PCM, clamped rather than wrapped.
pub fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

/// Build and start an input stream on `device`, downmixing every callback to
/// mono and handing the block, with the callback's own timing, to `deliver`.
/// Shared by every microphone, and by the call track of a meeting (an output
/// device opened as input -- the loopback trick `capture.rs` explains).
///
/// `deliver` runs in the realtime callback, so it must follow the module's
/// rules: a `try_send` and nothing slower. `on_error` is `cpal`'s error
/// callback, which has the same constraint. `what` names the stream for the
/// messages ("microphone"); `pool_capacity` is how many spare buffers the
/// pool holds, which should be at least the most blocks the channel `deliver`
/// sends into can have in flight.
///
/// Returns the running stream, which the caller must keep, and the pool's
/// return half. `cpal::Stream` is not reliably `Send`, so it is also the
/// caller's to drop on the thread that built it.
pub fn open_input<D, E>(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
    what: &str,
    pool_capacity: usize,
    deliver: D,
    on_error: E,
) -> Result<(cpal::Stream, SyncSender<Vec<f32>>), AudioError>
where
    D: FnMut(Vec<f32>, &cpal::InputCallbackInfo) + Send + 'static,
    E: FnMut(cpal::Error) + Send + 'static,
{
    let (free_tx, free_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(pool_capacity);
    let stream = match config.sample_format() {
        SampleFormat::I8 => build::<i8, _, _>(device, config, free_rx, deliver, on_error),
        SampleFormat::I16 => build::<i16, _, _>(device, config, free_rx, deliver, on_error),
        SampleFormat::I32 => build::<i32, _, _>(device, config, free_rx, deliver, on_error),
        SampleFormat::F32 => build::<f32, _, _>(device, config, free_rx, deliver, on_error),
        other => return Err(AudioError(format!("unsupported {what} sample format {other}"))),
    }
    .map_err(|e| AudioError(format!("could not open the {what}: {e}")))?;

    stream.play().map_err(|e| AudioError(format!("could not start the {what}: {e}")))?;
    Ok((stream, free_tx))
}

/// [`open_input`] for one sample format, which is what the callback has to be
/// written against.
fn build<T, D, E>(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
    free_rx: Receiver<Vec<f32>>,
    mut deliver: D,
    on_error: E,
) -> Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + 'static,
    f32: FromSample<T>,
    D: FnMut(Vec<f32>, &cpal::InputCallbackInfo) + Send + 'static,
    E: FnMut(cpal::Error) + Send + 'static,
{
    let channels = config.channels();
    device.build_input_stream(
        config.clone().into(),
        move |data: &[T], info: &cpal::InputCallbackInfo| {
            let mut samples = free_rx.try_recv().unwrap_or_default();
            downmix_into::<T>(data, channels, &mut samples);
            deliver(samples, info);
        },
        on_error,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmixes_stereo_to_mono() {
        // Left full-scale, right silent: mono should land at half.
        let interleaved: Vec<f32> = (0..200).flat_map(|_| [1.0f32, 0.0f32]).collect();
        let mut mono = Vec::new();
        downmix_into::<f32>(&interleaved, 2, &mut mono);
        assert_eq!(mono.len(), 200);
        assert!((mono[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn downmix_into_reuses_the_buffer_it_is_given_rather_than_growing_it_unboundedly() {
        // The whole point of `downmix_into` over the old `downmix` is that
        // a buffer handed back for reuse keeps its capacity -- `clear`, not
        // a fresh `Vec` -- so a steady stream of same-sized blocks settles
        // into zero further allocation. Prove that directly: capacity after
        // a second, same-sized call must not have grown past the first.
        let interleaved: Vec<f32> = (0..200).flat_map(|_| [1.0f32, 0.0f32]).collect();
        let mut buf = Vec::new();
        downmix_into::<f32>(&interleaved, 2, &mut buf);
        let capacity_after_first = buf.capacity();
        downmix_into::<f32>(&interleaved, 2, &mut buf);
        assert_eq!(buf.capacity(), capacity_after_first, "same-sized block must not reallocate");
        assert_eq!(buf.len(), 200);
    }

    #[test]
    fn samples_become_pcm_clamped_not_wrapped() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), i16::MAX);
        assert_eq!(f32_to_i16(7.0), i16::MAX);
        assert_eq!(f32_to_i16(-7.0), -i16::MAX);
    }
}
