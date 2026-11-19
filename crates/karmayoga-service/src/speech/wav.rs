//! WAV files, read and written.
//!
//! What a voice server sends back, what a webview plays, and what a recogniser
//! on the network is uploaded are all WAV, so both directions are here and
//! belong to nobody in particular.
//!
//! # A WAV from elsewhere is not trusted
//!
//! [`decode`] is written for what servers really send. A streaming server
//! cannot know how long the audio will be before it has made it, so it writes
//! `0` or `0xFFFFFFFF` into the `data` chunk's size, and the RIFF size with it;
//! the audio is then everything to the end of the body. A size that claims
//! more than arrived is treated the same way, since the body is the fact and
//! the header is the claim. 16-bit PCM and 32-bit float are read, any number of
//! channels is averaged into one, and everything else is refused in a sentence
//! rather than played as noise.
//!
//! [`encode`] writes the one shape that every player and recogniser reads:
//! 16-bit PCM, mono.

use super::pcm::sample_to_i16;
use super::synth::Speech;

const WAVE_PCM: u16 = 1;
const WAVE_FLOAT: u16 = 3;
/// What a WAV with more than two channels, or more than 16 bits, says it is
/// before saying what it really is in its sub-format.
const WAVE_EXTENSIBLE: u16 = 0xFFFE;

/// The `fmt ` chunk, as far as it matters here.
struct WavFormat {
    tag: u16,
    channels: usize,
    rate: u32,
    bits: u16,
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    let b = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A WAV file as mono 16-bit speech. The error is a predicate for the server
/// -- "did not send a WAV file" -- for the caller to put a name in front of.
///
/// See the module docs for what it forgives.
pub fn decode(bytes: &[u8]) -> Result<Speech, String> {
    if bytes.len() < 12
        || !(bytes.starts_with(b"RIFF") || bytes.starts_with(b"RF64"))
        || &bytes[8..12] != b"WAVE"
    {
        return Err("did not send a WAV file".into());
    }

    let mut format: Option<WavFormat> = None;
    let mut data: Option<&[u8]> = None;
    let mut pos = 12usize;
    // Each chunk is an id, a size and that many bytes, padded to an even
    // length. Anything but `fmt ` and `data` (LIST, fact, a streaming
    // server's own notes) is stepped over.
    while let Some(header) = pos.checked_add(8).and_then(|end| bytes.get(pos..end)) {
        let id = [header[0], header[1], header[2], header[3]];
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let start = pos + 8;
        let available = bytes.len().saturating_sub(start);
        match &id {
            b"fmt " => {
                let damaged = || "sent a WAV file whose format header is damaged".to_string();
                if size < 16 || available < 16 {
                    return Err(damaged());
                }
                let mut tag = u16_at(bytes, start).ok_or_else(damaged)?;
                if tag == WAVE_EXTENSIBLE && size >= 40 && available >= 26 {
                    // The real format is the first two bytes of the GUID.
                    tag = u16_at(bytes, start + 24).ok_or_else(damaged)?;
                }
                format = Some(WavFormat {
                    tag,
                    channels: usize::from(u16_at(bytes, start + 2).ok_or_else(damaged)?),
                    rate: u32_at(bytes, start + 4).ok_or_else(damaged)?,
                    bits: u16_at(bytes, start + 14).ok_or_else(damaged)?,
                });
            }
            b"data" => {
                let len = if size == 0 || size == u32::MAX as usize || size > available {
                    available
                } else {
                    size
                };
                data = Some(&bytes[start..start + len]);
                break;
            }
            _ => {}
        }
        pos = match start.checked_add(size).and_then(|p| p.checked_add(size & 1)) {
            Some(next) => next,
            None => break,
        };
    }

    let Some(WavFormat { tag, channels, rate, bits }) = format else {
        return Err("sent a WAV file with no format header".into());
    };
    let Some(data) = data else {
        return Err("sent back no audio".into());
    };
    if channels == 0 || channels > 64 || !(1_000..=384_000).contains(&rate) {
        return Err("sent a WAV file with a format this app cannot make sense of".into());
    }
    let sample_bytes = match (tag, bits) {
        (WAVE_PCM, 16) => 2,
        (WAVE_FLOAT, 32) => 4,
        _ => {
            return Err(format!(
                "sent audio this app cannot play (format {tag}, {bits}-bit); it can play 16-bit \
                 and 32-bit float WAV"
            ));
        }
    };
    let frame = sample_bytes * channels;
    let samples: Vec<i16> = data
        .chunks_exact(frame)
        .map(|f| {
            if tag == WAVE_PCM {
                let sum: i32 =
                    f.chunks_exact(2).map(|s| i32::from(i16::from_le_bytes([s[0], s[1]]))).sum();
                (sum / channels as i32) as i16
            } else {
                let sum: f32 =
                    f.chunks_exact(4).map(|s| f32::from_le_bytes([s[0], s[1], s[2], s[3]])).sum();
                sample_to_i16(sum / channels as f32)
            }
        })
        .collect();
    if samples.is_empty() {
        return Err("sent back no audio".into());
    }
    Ok(Speech { samples, sample_rate: rate })
}

/// `samples` as a 16-bit mono WAV file: for a webview to play, or to send to
/// something that wants a file rather than a stream.
pub fn encode(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + samples.len() * 2);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&WAVE_PCM.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&sample_rate.saturating_mul(2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAV file by hand, with the `data` size as the test says it is.
    fn wav(
        tag: u16,
        channels: u16,
        rate: u32,
        bits: u16,
        data: &[u8],
        size: Option<u32>,
    ) -> Vec<u8> {
        let size = size.unwrap_or(data.len() as u32);
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&36u32.wrapping_add(size).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * u32::from(channels) * u32::from(bits) / 8).to_le_bytes());
        out.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    fn pcm(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    fn floats(samples: &[f32]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    // ---- reading a WAV ----------------------------------------------------

    #[test]
    fn plain_16_bit_mono_comes_back_as_it_is() {
        let file = wav(1, 1, 24_000, 16, &pcm(&[0, 100, -100, i16::MAX]), None);
        let speech = decode(&file).unwrap();
        assert_eq!(speech, Speech { samples: vec![0, 100, -100, i16::MAX], sample_rate: 24_000 });
    }

    #[test]
    fn a_streaming_servers_unknown_length_means_everything_to_the_end() {
        let data = pcm(&[1, 2, 3, 4]);
        for size in [0, u32::MAX, 1_000_000] {
            let file = wav(1, 1, 22_050, 16, &data, Some(size));
            let speech = decode(&file).unwrap();
            assert_eq!(speech.samples, vec![1, 2, 3, 4], "data size {size}");
            assert_eq!(speech.sample_rate, 22_050);
        }
    }

    #[test]
    fn a_size_that_stops_short_leaves_what_follows_alone() {
        let file = wav(1, 1, 16_000, 16, &pcm(&[5, 6, 7, 8]), Some(4));
        assert_eq!(decode(&file).unwrap().samples, vec![5, 6]);
    }

    #[test]
    fn stereo_is_averaged_into_one() {
        let file = wav(1, 2, 44_100, 16, &pcm(&[100, 300, -200, -400]), None);
        assert_eq!(decode(&file).unwrap().samples, vec![200, -300]);
    }

    #[test]
    fn float_is_converted_and_clamped() {
        let file = wav(3, 1, 24_000, 32, &floats(&[0.0, 0.5, -1.0, 2.0, -3.0, f32::NAN]), None);
        let samples = decode(&file).unwrap().samples;
        assert_eq!(samples, vec![0, 16_384, -32_767, 32_767, -32_767, 0]);
    }

    #[test]
    fn float_stereo_is_averaged_before_it_is_converted() {
        let file = wav(3, 2, 24_000, 32, &floats(&[1.0, 0.0, 1.0, 1.0]), None);
        assert_eq!(decode(&file).unwrap().samples, vec![16_384, 32_767]);
    }

    #[test]
    fn a_half_frame_at_the_end_is_dropped() {
        let mut data = pcm(&[10, 20]);
        data.push(0x7f);
        let file = wav(1, 1, 24_000, 16, &data, Some(0));
        assert_eq!(decode(&file).unwrap().samples, vec![10, 20]);
    }

    #[test]
    fn chunks_it_does_not_know_are_stepped_over() {
        let plain = wav(1, 1, 24_000, 16, &pcm(&[9, 8]), None);
        // A LIST chunk with an odd size (and so a pad byte) before `data`.
        let mut file = plain[..36].to_vec();
        file.extend_from_slice(b"LIST");
        file.extend_from_slice(&3u32.to_le_bytes());
        file.extend_from_slice(&[1, 2, 3, 0]);
        file.extend_from_slice(&plain[36..]);
        assert_eq!(decode(&file).unwrap().samples, vec![9, 8]);
    }

    #[test]
    fn an_extensible_header_says_what_it_really_is_in_its_sub_format() {
        let data = floats(&[0.5]);
        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        file.extend_from_slice(b"WAVEfmt ");
        file.extend_from_slice(&40u32.to_le_bytes());
        file.extend_from_slice(&WAVE_EXTENSIBLE.to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&24_000u32.to_le_bytes());
        file.extend_from_slice(&96_000u32.to_le_bytes());
        file.extend_from_slice(&4u16.to_le_bytes());
        file.extend_from_slice(&32u16.to_le_bytes());
        file.extend_from_slice(&22u16.to_le_bytes());
        file.extend_from_slice(&32u16.to_le_bytes());
        file.extend_from_slice(&4u32.to_le_bytes());
        file.extend_from_slice(&WAVE_FLOAT.to_le_bytes());
        file.extend_from_slice(&[0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71]);
        file.extend_from_slice(b"data");
        file.extend_from_slice(&u32::MAX.to_le_bytes());
        file.extend_from_slice(&data);
        assert_eq!(decode(&file).unwrap().samples, vec![16_384]);
    }

    #[test]
    fn what_cannot_be_played_is_refused_in_a_sentence() {
        let not_a_wav = decode(b"{\"error\": \"nope\"}").unwrap_err();
        assert_eq!(not_a_wav, "did not send a WAV file");
        assert!(decode(b"").is_err());

        let eight_bit = decode(&wav(1, 1, 24_000, 8, &[1, 2, 3], None)).unwrap_err();
        assert!(eight_bit.contains("cannot play") && eight_bit.contains("8-bit"), "{eight_bit}");
        let adpcm = decode(&wav(2, 1, 24_000, 4, &[1, 2, 3], None)).unwrap_err();
        assert!(adpcm.contains("cannot play"), "{adpcm}");
        let silly = decode(&wav(1, 0, 24_000, 16, &[1, 2], None)).unwrap_err();
        assert!(silly.contains("cannot make sense"), "{silly}");
        let slow = decode(&wav(1, 1, 5, 16, &[1, 2], None)).unwrap_err();
        assert!(slow.contains("cannot make sense"), "{slow}");
    }

    #[test]
    fn no_audio_is_an_error_not_silence() {
        let empty = decode(&wav(1, 1, 24_000, 16, &[], None)).unwrap_err();
        assert!(empty.contains("no audio"), "{empty}");
        let truncated = decode(&wav(1, 1, 24_000, 16, &[], None)[..40]).unwrap_err();
        assert!(!truncated.is_empty());
        let header_only = decode(&wav(1, 1, 24_000, 16, &[], None)[..36]).unwrap_err();
        assert!(header_only.contains("no audio"), "{header_only}");
    }

    #[test]
    fn what_is_written_can_be_read_back() {
        let samples = vec![0, 1, -1, 12_345, i16::MIN, i16::MAX];
        let file = encode(&samples, 24_000);
        assert_eq!(file.len(), 44 + 12);
        let back = decode(&file).unwrap();
        assert_eq!(back, Speech { samples, sample_rate: 24_000 });
    }

    #[test]
    fn what_is_written_says_mono_16_bit_at_its_rate() {
        let file = encode(&[1, 2, 3], 16_000);
        assert_eq!(&file[..4], b"RIFF");
        assert_eq!(u32_at(&file, 4), Some(36 + 6));
        assert_eq!(&file[8..16], b"WAVEfmt ");
        assert_eq!(u16_at(&file, 20), Some(WAVE_PCM));
        assert_eq!(u16_at(&file, 22), Some(1));
        assert_eq!(u32_at(&file, 24), Some(16_000));
        assert_eq!(u32_at(&file, 28), Some(32_000));
        assert_eq!(u16_at(&file, 32), Some(2));
        assert_eq!(u16_at(&file, 34), Some(16));
        assert_eq!(&file[36..40], b"data");
        assert_eq!(u32_at(&file, 40), Some(6));
    }

    #[test]
    fn nothing_written_is_a_header_with_no_audio() {
        let file = encode(&[], 16_000);
        assert_eq!(file.len(), 44);
        assert!(decode(&file).unwrap_err().contains("no audio"));
    }
}
