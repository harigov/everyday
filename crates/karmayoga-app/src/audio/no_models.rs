//! The microphone's models, on a build that has none.
//!
//! The noise filter and voice isolation run in sherpa-onnx, which only the
//! desktop builds (`karmayoga-service`'s `speech` feature). A phone hands the
//! engine no `Cleanup` at all -- see `voice::try_start` -- so nothing here is
//! ever asked to load; it stands in for `karmayoga_service::speech::cleanup`
//! so the engine reads the same on both.

use karmayoga_service::error::{CommandError, CommandResult, codes};

/// What the engine is asked to run over the microphone.
pub struct Cleanup {
    pub strong_noise: bool,
    pub voice_sample: Option<Vec<i16>>,
}

fn none<T>() -> CommandResult<T> {
    Err(CommandError::new(codes::UNSUPPORTED, "this device has no models for the microphone"))
}

pub struct NoiseFilter;

impl NoiseFilter {
    pub fn load() -> CommandResult<NoiseFilter> {
        none()
    }
}

pub struct VoiceIsolation;

impl VoiceIsolation {
    pub fn load(_sample: &[i16]) -> CommandResult<VoiceIsolation> {
        none()
    }
}

pub struct MicCleanup;

impl MicCleanup {
    pub fn new(_noise: Option<NoiseFilter>, _voices: Option<VoiceIsolation>) -> Option<MicCleanup> {
        None
    }

    pub fn process(&mut self, _samples: &[i16], _out: &mut Vec<i16>) {}

    pub fn finish(&mut self, _out: &mut Vec<i16>) {}
}
