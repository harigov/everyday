//! Meeting notes: the service half.
//!
//! `jatayu_core::meeting` decides; this owns the models, the spool and
//! the sockets. The desktop shell owns the microphone and hands audio in
//! through [`spool`].
//!
//! ```text
//!   watch        the offer: online calls starting now (and "Always")
//!   spool        sealed chunk files; begin / append / finish / discard; recovery
//!   live         while it is recorded: the person's own notes, questions about it
//!   pipeline     one supervised task per recording:
//!                  vad → transcribe → identify → merge → summarise → note
//! ```
//!
//! What turns audio into words is not here. Transcription backends
//! ([`crate::speech::transcribe`]) and the sherpa-onnx models for voice
//! detection, recognition, diarisation and speaker embeddings
//! ([`crate::speech::local`], feature `speech`) are the platform's speech
//! layer, which meeting notes use along with the assistant's voice mode. What
//! stays here is what is particular to a meeting: chunks and their spool,
//! telling the speakers apart, and the note.
//!
//! The model files themselves -- what can be downloaded, and downloading it
//! -- are [`crate::local::files`], shared with every other model that runs
//! on this computer.

pub mod brief;
pub mod live;
pub mod pipeline;
pub mod spool;
pub mod watch;
