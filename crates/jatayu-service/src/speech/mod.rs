//! Speech, for everything in this application that listens or talks.
//!
//! The assistant's voice mode is one user of this layer and meeting notes are
//! another. Nothing here knows about either, or about where the audio comes
//! from or goes: a caller that has a microphone, a file or a stream of samples
//! can listen, and one that has a speaker or somewhere to put a recording can
//! talk.
//!
//! ```text
//!   listening                                talking
//!
//!   detect      where speech starts and ends   text      writing made fit to hear
//!                                              script    how it is said, and what is shown
//!   utterance   one stretch of speech -> words synth     words -> audio
//!   live        a stream -> words as they come narrate   sentences -> audio, in order
//!   transcribe  chunks of a recording -> words
//!   local       the models on this computer    tempo     audio faster or slower, same pitch
//!   qwen        Qwen3-ASR, which local runs through llama.cpp
//!   expect      the words to expect, from the vault
//!   hotwords    how Parakeet is told them
//!
//!   pcm, wav    sample and file conversions everything shares
//!   resample    one rate to another, for devices and calls
//! ```
//!
//! [`access`] is how the rest of the application gets them: built from what is
//! chosen in Settings → Models, and refused in a sentence when something is
//! missing. See `docs/plans/voice.md`.

pub mod access;
#[cfg(feature = "speech")]
pub mod cleanup;
pub mod decode;
pub mod detect;
pub mod dictate;
pub mod expect;
pub mod hotwords;
pub mod listen;
pub mod live;
#[cfg(feature = "speech")]
pub mod local;
pub mod narrate;
pub mod owner;
pub mod pcm;
pub mod qwen;
pub mod ready;
pub mod render;
pub mod resample;
pub mod script;
#[cfg(feature = "speech")]
pub mod spotter;
pub mod synth;
pub mod tempo;
pub mod text;
pub mod transcribe;
pub mod utterance;
pub mod voicing;
pub mod wake;
pub mod wav;

/// The rate of everything that is listened to: 16 kHz mono, as every speech
/// model here wants it.
pub const SAMPLE_RATE: u32 = jatayu_core::meeting::SAMPLE_RATE;
