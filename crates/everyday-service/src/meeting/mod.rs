//! Meeting notes: the service half.
//!
//! `everyday_core::meeting` decides; this owns the models, the spool and
//! the sockets. The desktop shell owns the microphone and hands audio in
//! through [`spool`].
//!
//! ```text
//!   watch        the offer: online calls starting now (and "Always")
//!   spool        sealed chunk files; begin / append / finish / discard; recovery
//!   pipeline     one supervised task per recording:
//!                  vad → transcribe → identify → merge → summarise → note
//!   transcribe   one trait, three remote backends and a local one
//!   speech       sherpa-onnx: vad, recognise, diarise, embed (feature `speech`)
//!   models       what can be downloaded, and downloading it
//! ```

pub mod models;
pub mod pipeline;
#[cfg(feature = "speech")]
pub mod speech;
pub mod spool;
pub mod transcribe;
pub mod watch;
