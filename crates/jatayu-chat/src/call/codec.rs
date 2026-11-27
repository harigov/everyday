//! Opus in and out. See `super`.
//!
//! The two directions are deliberately at different rates. What the caller
//! says is decoded straight to the voice session's 16 kHz: libopus resamples
//! inside the decoder, so nothing here needs a resampler on the way in. What
//! the assistant says goes out at 48 kHz, the rate Opus runs at on a call,
//! because the encoder at that rate is what every client's decoder expects
//! and what keeps the high end of a synthetic voice.
//!
//! Both ends are pure: bytes and samples, no network and no clock, so they can
//! be tested without a call.

use jatayu_service::voice::SAMPLE_RATE;
use opus::{Application, Bandwidth, Bitrate, Channels, Signal};

/// The rate speech is encoded at, which is the rate Opus is clocked at in RTP
/// whatever it carries.
pub const CALL_RATE: u32 = 48_000;

/// Samples in one 20 ms frame at [`CALL_RATE`]: how often a call's audio is
/// sent, and how far the RTP clock moves each time.
pub const FRAME: usize = 960;

/// What an encoded packet can be at most. Speech at the bitrate used here is
/// a tenth of this; it is the size libopus documents as always enough.
const MAX_PACKET: usize = 1275;

/// The longest packet Opus allows, 120 ms, at the session's rate.
const MAX_DECODED: usize = SAMPLE_RATE as usize * 120 / 1000;

/// How many of the session's samples one RTP clock tick at [`CALL_RATE`] is
/// worth, as a divisor: three ticks to a sample.
const TICKS_PER_SAMPLE: u64 = (CALL_RATE / SAMPLE_RATE) as u64;

/// Up to this much missing audio, in the session's samples, is concealed by
/// the decoder from what came before it, which sounds like a short dropout
/// instead of a click. 60 ms: past that concealment turns into a drone.
const CONCEAL_UP_TO: usize = SAMPLE_RATE as usize * 60 / 1000;

/// libopus conceals in whole multiples of 2.5 ms.
const CONCEAL_STEP: usize = SAMPLE_RATE as usize * 25 / 10_000;

/// The longest silence put in for a gap in the caller's packets. A phone that
/// stops sending while the caller is quiet (Opus's discontinuous
/// transmission) leaves a gap, and the session's idea of how long the caller
/// has been quiet is counted in the samples it is given, so the gap has to
/// be given back as silence. Longer than this is a phone that went away, and
/// more silence than the session can use.
const SILENCE_UP_TO: usize = SAMPLE_RATE as usize * 5;

/// The bitrate speech is encoded at. Wideband speech is clean from 24 kbit/s;
/// this leaves some room for a synthetic voice's sibilants.
const BITRATE: i32 = 32_000;

/// Why Opus refused something, in a sentence for a log.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CodecError(pub String);

impl From<opus::Error> for CodecError {
    fn from(e: opus::Error) -> Self {
        CodecError(format!("Opus: {e}"))
    }
}

/// Turns 20 ms frames of the assistant's voice into Opus packets.
pub struct Encoder {
    inner: opus::Encoder,
    packet: Vec<u8>,
}

impl Encoder {
    /// Mono, at [`CALL_RATE`], tuned for speech: the VoIP application and a
    /// voice signal hint, which is what makes a phone call's codec behave
    /// like one.
    pub fn new() -> Result<Self, CodecError> {
        let mut inner = opus::Encoder::new(CALL_RATE, Channels::Mono, Application::Voip)?;
        inner.set_bitrate(Bitrate::Bits(BITRATE))?;
        inner.set_signal(Signal::Voice)?;
        inner.set_bandwidth(Bandwidth::Fullband)?;
        Ok(Self { inner, packet: vec![0; MAX_PACKET] })
    }

    /// One frame, [`FRAME`] samples from -1 to 1, as one packet.
    pub fn encode(&mut self, frame: &[f32; FRAME]) -> Result<Vec<u8>, CodecError> {
        let len = self.inner.encode_float(frame, &mut self.packet)?;
        Ok(self.packet[..len].to_vec())
    }
}

/// Turns the caller's Opus packets into the session's 16 kHz mono samples.
pub struct Decoder {
    inner: opus::Decoder,
    scratch: Vec<i16>,
    /// Where the next packet should start on the RTP clock, if packets have
    /// come. What a packet starts after that is a gap.
    expected: Option<u64>,
}

impl Decoder {
    pub fn new() -> Result<Self, CodecError> {
        Ok(Self {
            inner: opus::Decoder::new(SAMPLE_RATE, Channels::Mono)?,
            scratch: vec![0; MAX_DECODED],
            expected: None,
        })
    }

    /// One packet, whose first sample is at `rtp_time` on the 48 kHz RTP
    /// clock, as samples at [`SAMPLE_RATE`].
    ///
    /// A gap between this packet and the one before it is filled in front of
    /// it: concealed when it is short (a packet lost), silent when it is long
    /// (the caller's phone stopped sending). That keeps the samples handed on
    /// as long as the call has been.
    pub fn decode(&mut self, rtp_time: u64, packet: &[u8]) -> Result<Vec<i16>, CodecError> {
        let mut out = Vec::new();
        if let Some(expected) = self.expected
            && rtp_time > expected
        {
            let missing = ((rtp_time - expected) / TICKS_PER_SAMPLE) as usize;
            self.fill(missing, &mut out)?;
        }
        let len = self.inner.decode(packet, &mut self.scratch, false)?;
        out.extend_from_slice(&self.scratch[..len]);
        self.expected = Some(rtp_time + len as u64 * TICKS_PER_SAMPLE);
        Ok(out)
    }

    fn fill(&mut self, missing: usize, out: &mut Vec<i16>) -> Result<(), CodecError> {
        if missing > CONCEAL_UP_TO {
            out.resize(out.len() + missing.min(SILENCE_UP_TO), 0);
            return Ok(());
        }
        let concealed = missing / CONCEAL_STEP * CONCEAL_STEP;
        if concealed > 0 {
            // An empty packet is how libopus is told one was lost.
            let len = self.inner.decode(&[], &mut self.scratch[..concealed], false)?;
            out.extend_from_slice(&self.scratch[..len]);
        }
        out.resize(out.len() + (missing - concealed), 0);
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// `frames` frames of a sine at `hz`, half scale, at [`CALL_RATE`], the
    /// phase carried across frames so they join.
    pub fn tone(hz: f32, frames: usize) -> Vec<[f32; FRAME]> {
        (0..frames)
            .map(|f| {
                let mut frame = [0.0; FRAME];
                for (i, sample) in frame.iter_mut().enumerate() {
                    let t = (f * FRAME + i) as f32 / CALL_RATE as f32;
                    *sample = 0.5 * (std::f32::consts::TAU * hz * t).sin();
                }
                frame
            })
            .collect()
    }

    /// The root mean square of `samples`.
    pub fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
    }

    /// How much of the energy in `samples` (at `rate`) is a pure tone at
    /// `hz`: near 1 for a clean sine, near 0 for noise or silence.
    pub fn tone_share(samples: &[f32], rate: u32, hz: f32) -> f32 {
        let (mut re, mut im, mut energy) = (0.0f64, 0.0f64, 0.0f64);
        for (i, &s) in samples.iter().enumerate() {
            let angle = std::f64::consts::TAU * f64::from(hz) * i as f64 / f64::from(rate);
            re += f64::from(s) * angle.cos();
            im += f64::from(s) * angle.sin();
            energy += f64::from(s) * f64::from(s);
        }
        if energy == 0.0 {
            return 0.0;
        }
        let n = samples.len() as f64;
        (2.0 * (re * re + im * im) / (n * energy)) as f32
    }

    fn as_f32(samples: &[i16]) -> Vec<f32> {
        samples.iter().map(|&s| f32::from(s) / 32768.0).collect()
    }

    #[test]
    fn a_tone_survives_encoding_at_48k_and_decoding_at_16k() {
        let mut encoder = Encoder::new().expect("an encoder");
        let mut decoder = Decoder::new().expect("a decoder");
        let mut decoded = Vec::new();
        for (i, frame) in tone(440.0, 25).iter().enumerate() {
            let packet = encoder.encode(frame).expect("a packet");
            assert!(!packet.is_empty() && packet.len() < MAX_PACKET);
            let samples = decoder.decode(i as u64 * FRAME as u64, &packet).expect("samples");
            assert_eq!(samples.len(), 320, "20 ms at 16 kHz is 320 samples");
            decoded.extend(as_f32(&samples));
        }
        // The encoder and decoder both take a few frames to settle.
        let settled = &decoded[320 * 5..];
        let sent = tone(440.0, 25).concat();
        assert!(
            rms(settled) > 0.6 * rms(&sent),
            "most of the tone's energy arrives: {} of {}",
            rms(settled),
            rms(&sent)
        );
        assert!(tone_share(settled, SAMPLE_RATE, 440.0) > 0.9, "and it is still a 440 Hz tone");
    }

    #[test]
    fn silence_decodes_to_silence() {
        let mut encoder = Encoder::new().expect("an encoder");
        let mut decoder = Decoder::new().expect("a decoder");
        let packet = encoder.encode(&[0.0; FRAME]).expect("a packet");
        let samples = decoder.decode(0, &packet).expect("samples");
        assert_eq!(samples.len(), 320);
        assert!(samples.iter().all(|&s| s.abs() < 8));
    }

    #[test]
    fn a_short_gap_is_concealed_and_the_samples_stay_as_long_as_the_call() {
        let mut encoder = Encoder::new().expect("an encoder");
        let mut decoder = Decoder::new().expect("a decoder");
        let frames = tone(440.0, 4);
        let first = encoder.encode(&frames[0]).expect("a packet");
        let third = encoder.encode(&frames[2]).expect("a packet");
        assert_eq!(decoder.decode(0, &first).expect("samples").len(), 320);
        // The second frame never arrives: the third starts at 2 * 960.
        let samples = decoder.decode(2 * FRAME as u64, &third).expect("samples");
        assert_eq!(samples.len(), 640, "the lost frame is made up, then the one that came");
    }

    #[test]
    fn a_long_gap_is_silence() {
        let mut encoder = Encoder::new().expect("an encoder");
        let mut decoder = Decoder::new().expect("a decoder");
        let packet = encoder.encode(&tone(440.0, 1)[0]).expect("a packet");
        decoder.decode(0, &packet).expect("samples");
        // A second of 48 kHz ticks later.
        let samples = decoder.decode(CALL_RATE as u64 + FRAME as u64, &packet).expect("samples");
        assert_eq!(
            samples.len(),
            SAMPLE_RATE as usize + 320,
            "a second of silence, then the packet"
        );
        assert!(samples[..SAMPLE_RATE as usize].iter().all(|&s| s == 0));
    }

    #[test]
    fn a_silence_longer_than_the_cap_is_cut_to_it() {
        let mut encoder = Encoder::new().expect("an encoder");
        let mut decoder = Decoder::new().expect("a decoder");
        let packet = encoder.encode(&tone(440.0, 1)[0]).expect("a packet");
        decoder.decode(0, &packet).expect("samples");
        let samples = decoder.decode(CALL_RATE as u64 * 60, &packet).expect("samples");
        assert_eq!(samples.len(), SILENCE_UP_TO + 320);
    }
}
