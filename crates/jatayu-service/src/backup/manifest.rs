//! One copy's manifest: every file in it, and the chunks each is made of.
//!
//! JSON, compressed, then sealed under the copy's own name (see
//! `jatayu_core::backup::BackupKeys::seal_manifest`). JSON because a
//! manifest is read back by a future build of this program and possibly by a
//! person with the key and a few lines of script, and it costs nothing here:
//! a vault of two hundred thousand files makes a manifest of a few megabytes,
//! and most of that is chunk names, which compress to half.

use jatayu_core::backup::BackupKeys;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

/// The manifest format this build writes, and the newest it reads.
pub const FORMAT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format: u32,
    pub created: Timestamp,
    /// The version of Jatayu that made it.
    pub app_version: String,
    /// The computer that made it.
    pub host: String,
    /// The vault's name, as its header has it.
    pub vault_name: String,
    /// The files folder as the vault's settings named it when this copy was
    /// made, when one was set up: where `files/` goes back to unless a
    /// restore is told otherwise. Sealed with the rest of the manifest, so a
    /// path on somebody's computer is as private here as it is there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_folder: Option<String>,
    pub files: Vec<FileEntry>,
}

/// What a file in the person's files folder is named under inside a copy:
/// `files/Photos/a.jpg`. Entries under it are laid back into the files
/// folder, not the vault directory, though they are [`Area::Vault`] ones as
/// far as a build that has never heard of the files folder is concerned --
/// which restores them as a harmless `files/` beside the store.
pub const FILES_PREFIX: &str = "files/";

/// Which folder a file is laid back down in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Area {
    /// The vault directory.
    Vault,
    /// This computer's settings folder.
    Config,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub area: Area,
    /// Relative to the area's folder, `/`-separated.
    pub path: String,
    pub size: u64,
    /// Nanoseconds since the epoch, as the file said when it was read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<i64>,
    /// Where the file was cut: every this many bytes.
    pub chunk_size: u32,
    /// In order; the file is their plaintexts end to end.
    pub chunks: Vec<String>,
}

impl FileEntry {
    /// Whether this is a file from the person's files folder (see
    /// [`FILES_PREFIX`]).
    pub fn in_files_folder(&self) -> bool {
        self.area == Area::Vault && self.path.starts_with(FILES_PREFIX)
    }
}

impl Manifest {
    /// Sealed bytes, ready to store as `snapshot`.
    pub fn encode(&self, keys: &BackupKeys, snapshot: &str) -> Result<Vec<u8>, String> {
        let json = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        let mut deflate =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        deflate.write_all(&json).map_err(|e| e.to_string())?;
        let packed = deflate.finish().map_err(|e| e.to_string())?;
        keys.seal_manifest(snapshot, &packed).map_err(|e| e.to_string())
    }

    /// The manifest stored as `snapshot`. Refuses one from a newer format
    /// rather than reading part of it.
    pub fn decode(sealed: &[u8], keys: &BackupKeys, snapshot: &str) -> Result<Self, String> {
        let packed = keys
            .open_manifest(snapshot, sealed)
            .map_err(|_| format!("the backup {snapshot} does not open with this vault's key"))?;
        let mut json = Vec::new();
        flate2::read::DeflateDecoder::new(&packed[..])
            .read_to_end(&mut json)
            .map_err(|e| format!("the backup {snapshot} is damaged: {e}"))?;
        let manifest: Manifest = serde_json::from_slice(&json)
            .map_err(|e| format!("the backup {snapshot} is damaged: {e}"))?;
        if manifest.format > FORMAT {
            return Err(format!(
                "the backup {snapshot} was made by a newer Jatayu (format {}); update to \
                 restore it",
                manifest.format
            ));
        }
        Ok(manifest)
    }

    /// Every chunk this copy needs, each once per file it is in.
    pub fn chunks(&self) -> impl Iterator<Item = &str> {
        self.files.iter().flat_map(|f| f.chunks.iter().map(String::as_str))
    }

    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}
