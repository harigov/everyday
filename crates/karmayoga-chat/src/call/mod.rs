//! A voice call's audio, on any chat service whose calls go through a
//! LiveKit media server (MatrixRTC): the room on one side, a voice session on
//! the other.
//!
//! ```text
//!               ┌─ WebSocket (signal) ─┐
//!   media server ◀─ UDP: publisher ◀── frames(seal) ◀── codec ◀── outbound ◀── VoiceOutput::Play
//!                ── UDP: subscriber ──▶ frames(open) ──▶ codec ──▶ VoiceInput::Audio (16 kHz mono)
//! ```
//!
//! Nothing here knows about Matrix: a connector joins the call on its
//! service, gets this side a token for the room and the caller's keys, and
//! the relay (`crate::calling`) hands those to [`connect`] and the voice
//! session to [`Call::run`]. The caller's device cancels its own echo, as a
//! phone does on every call, so the audio that arrives is ready for a voice
//! session as it is.
//!
//! - `media`: the room -- two str0m connections to the server, sans-IO --
//!   and the one task that drives them, the WebSocket, the timers, and the
//!   voice session's two channels.
//! - `sfu`, `signal`: the WebSocket, and the protobuf messages said on it.
//! - `frames`: each frame's end-to-end encryption, with keys the server
//!   never sees.
//! - `codec`: Opus in and out, at the call's 48 kHz and the session's
//!   16 kHz.
//! - `outbound`: what the session asked to be played, paced out in 20 ms
//!   frames, held, resumed or thrown away as it says, and when each
//!   utterance has been played.
//!
//! See `docs/plans/calls.md`.

mod codec;
mod frames;
mod media;
mod outbound;
mod sfu;
mod signal;

pub use media::{Addresses, Call, CallEnd, CallError, ToCall, connect};
