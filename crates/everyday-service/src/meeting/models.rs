//! The local speech models: what can be downloaded, where it goes, and
//! whether it is there.
//!
//! Not behind the `speech` feature: settings has to be able to say what a
//! build without local speech would need, and whether a model is present is
//! a question about files, not about sherpa-onnx.

use everyday_core::meeting::LocalModel;
use std::path::PathBuf;

/// Where models are kept: the platform's data directory, beside vaults but
/// not in one. Models are neither secret nor per-vault.
pub fn models_dir() -> PathBuf {
    todo!("speech agent")
}

/// The VAD, segmentation and embedding models, all present and checked.
pub fn speech_kit_installed() -> bool {
    todo!("speech agent")
}

/// This recogniser's files, all present and checked.
pub fn installed(model: LocalModel) -> bool {
    let _ = model;
    todo!("speech agent")
}
