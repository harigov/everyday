//! Models that run on this computer.
//!
//! ```text
//!   files      what can be downloaded, where it goes, whether it is there
//!   resident   a loaded model kept in memory while it is used, let go after
//!   decide     a decision model: its prompt, and running it (feature `decisions`)
//!   llama      the parts of llama.cpp `decide` calls (feature `decisions`)
//! ```
//!
//! Each kind of model brings its own runtime -- sherpa-onnx for speech
//! (`crate::meeting::speech`, feature `speech`), llama.cpp for decisions
//! ([`decide`], feature `decisions`) -- and shares the rest: one catalogue of
//! pinned downloads beside the vaults, and one rule for memory. A model is
//! loaded the first time it is asked for and let go once nobody has used it
//! for [`resident::IDLE_AFTER`], so a speech recogniser, a decision model and
//! whatever comes next do not all sit in memory at once because each was
//! used once since the app started.

pub mod decide;
pub mod files;
#[cfg(feature = "decisions")]
pub mod llama;
pub mod resident;
