//! Settings that belong to this computer rather than to the vault -- the talk
//! key, voice typing, writing help, the wake word -- each a JSON file of its
//! own in the config directory.
//!
//! Written private to the owner from the moment the file exists, not by a
//! chmod after it has been readable under its final name: writing help's
//! holds a restore token, a standing grant to type into other windows, and
//! the others are kept alike since nothing in them needs anybody else to read
//! them.

use serde::Serialize;
use serde::de::DeserializeOwned;

/// `file`'s settings, or the defaults when it is missing or no longer parses.
pub fn load<T: DeserializeOwned + Default>(file: &str) -> T {
    std::fs::read(karmayoga_vault::config_dir().join(file))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Write `config` to `file`, creating the config directory if need be.
pub fn save<T: Serialize>(file: &str, config: &T) -> Result<(), String> {
    let path = karmayoga_vault::config_dir().join(file);
    if let Some(dir) = path.parent() {
        karmayoga_core::fsutil::create_private_dir(dir).map_err(|e| e.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    karmayoga_core::fsutil::write_atomic_private(
        &path,
        &bytes,
        &karmayoga_core::fsutil::unique_tag(),
    )
    .map_err(|e| e.to_string())
}
