//! Sample-rate conversion, shared with everything else that handles audio:
//! see `karmayoga_service::speech::resample`. Here only so the device code can
//! go on naming it as one of its own.

pub use karmayoga_service::speech::resample::*;

impl From<ResampleError> for super::AudioError {
    fn from(e: ResampleError) -> Self {
        Self(e.0)
    }
}
