//! This computer's microphone and speakers, for everything that uses them.
//!
//! Meeting capture, the assistant's voice mode and whatever comes next share
//! what is here rather than each opening devices its own way:
//!
//! - [`input`] opens a microphone (or anything else `cpal` will hand over as
//!   input) with the realtime rules kept, and says what a block of it is.
//! - [`resample`] brings audio from a device's rate to the one speech models
//!   want, and back.
//! - [`engine`] runs a microphone, a speaker or both for something that talks
//!   and listens, with echo cancellation ([`echo`]) when it has both.
//!
//! Meeting capture uses the first two and keeps its own thread, since it opens
//! a second input (the system's audio), reopens streams that die and frames
//! what it hears into chunks; none of that is the engine's business.

use karmayoga_service::error::CommandError;

pub mod echo;
pub mod engine;
pub mod input;
pub mod playback;
pub mod resample;

/// A plain, displayable error from the audio devices. Not
/// [`karmayoga_service::error::CommandError`]: this crosses a `std::thread`
/// boundary and an audio backend's errors have no vault-command code to
/// carry.
///
/// Meeting capture knows it as `capture::CaptureError`.
#[derive(Debug, Clone)]
pub struct AudioError(pub String);

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AudioError {}

impl From<CommandError> for AudioError {
    fn from(e: CommandError) -> Self {
        Self(e.to_string())
    }
}

impl From<AudioError> for CommandError {
    fn from(e: AudioError) -> Self {
        CommandError::new("capture", e.0)
    }
}
