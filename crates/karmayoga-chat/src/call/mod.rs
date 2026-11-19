//! A voice call's audio, on any chat service: WebRTC to the caller's device
//! on one side, a voice session on the other.
//!
//! ```text
//!   caller's device ◀─ UDP (ICE, DTLS-SRTP, Opus) ─▶ media ─▶ codec ─▶ VoiceInput::Audio  (16 kHz mono)
//!                                                     ▲
//!                                     outbound ◀── codec ◀── VoiceOutput::Play         (any rate)
//! ```
//!
//! Nothing here knows about Matrix: a connector carries the session
//! descriptions and candidates, and the relay (`crate::calling`) hands the
//! offer to [`answer`] and the voice session to [`Call::run`]. The caller's
//! device cancels its own echo, as a phone does on every call, so the audio
//! that arrives is ready for a voice session as it is.
//!
//! - `media`: the peer -- str0m, sans-IO -- and the one task that drives it:
//!   the UDP socket, the timers, and the voice session's two channels.
//! - `codec`: Opus in and out, at the call's 48 kHz and the session's
//!   16 kHz.
//! - `outbound`: what the session asked to be played, paced out in 20 ms
//!   frames, held, resumed or thrown away as it says, and when each
//!   utterance has been played.
//!
//! See `docs/plans/calls.md`.

mod codec;
mod media;
mod outbound;

pub use media::{Addresses, Answered, Call, CallEnd, CallError, ToCall, answer};
