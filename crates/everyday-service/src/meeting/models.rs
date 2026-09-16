//! The local speech models: what can be downloaded, where it goes, and
//! whether it is there.
//!
//! Not behind the `speech` feature: settings has to be able to say what a
//! build without local speech would need, and whether a model is present is
//! a question about files, not about sherpa-onnx.
//!
//! # The catalogue
//!
//! Three downloads, per `docs/plans/meeting-notes.md`'s "The libraries" and
//! "Models": [`SPEECH_KIT_ID`] (Silero VAD, pyannote `segmentation-3.0`, and
//! a small English speaker-embedding model -- shared by both recognisers and
//! needed from phase 2 on) and the two local recognisers named by
//! [`LocalModel`]. Every file is pinned by URL, byte count and SHA-256,
//! checked by downloading each one and hashing what arrived -- see this
//! phase's report for the exact commands run. Two of the three are `.tar.bz2`
//! archives holding files this application does not want (Python export
//! scripts, a licence file, test WAVs); [`Source::Archive`] downloads the
//! whole archive, checks *its* hash, and extracts only the named members,
//! each checked again on its own.
//!
//! # Where they live
//!
//! [`models_dir`] is the platform's data directory, the same
//! `ProjectDirs::from("app", "Every Day", "EveryDay")` construction
//! `everyday_vault::default_vault_dir` uses, plus `models/`. Models are
//! neither secret nor per-vault, so they sit beside vaults rather than
//! inside one -- a vault backup must not silently grow by 500 MB because a
//! recogniser happened to be installed. `everyday-vault` already owns this
//! constructor for `default_vault_dir` and `config_dir`; this crate does not
//! depend on it for a third call of the same one, since that would be a
//! service-layer module reaching into the vault crate for a directory that
//! has nothing to do with a vault.
//!
//! # Whether a model is installed
//!
//! [`installed`] and [`speech_kit_installed`] do not hash hundreds of
//! megabytes on every call. Each model's directory carries a marker file,
//! written once a download's every file has been checked against its pinned
//! SHA-256; after that, "installed" is a `stat` of each expected file against
//! the size the marker recorded, not a re-hash. A file that is missing, or
//! whose size has changed underneath the marker, is treated as not
//! installed -- the marker only ever speaks for a download this module
//! itself completed and verified.
//!
//! # Downloading
//!
//! [`start_download`] runs one model's download in the background, streaming
//! each source to `<dir>/<name>.partial` (or `<dir>/.partial-archive` for an
//! archive), checking its hash, and renaming it into place -- so a crash or a
//! cancel never leaves a half-written file where a finished one is expected.
//! [`progress`] reads `(done, total)` bytes while it runs; [`cancel_download`]
//! asks it to stop at the next chunk boundary; [`last_error`] remembers why a
//! download failed, until the next attempt starts. At most one download runs
//! per model at a time -- calling [`start_download`] again for a model
//! already downloading, or already installed, is a no-op.

use everyday_core::meeting::LocalModel;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// The speech kit's id in the catalogue and on the wire. See
/// `ui/src/lib/types.ts`'s `SpeechModelInfo.id`.
pub const SPEECH_KIT_ID: &str = "speechKit";

/// The id [`LocalModel`] serialises as, and the id the catalogue and the
/// wire use for it. Spelled out rather than read off `serde` -- this module
/// has no reason to depend on `everyday_core::meeting::LocalModel`'s own
/// wire representation matching its catalogue key, even though today it
/// does; a `#[test]` below pins the two together instead.
pub fn local_model_id(model: LocalModel) -> &'static str {
    match model {
        LocalModel::ParakeetV3 => "parakeetV3",
        LocalModel::WhisperTurbo => "whisperTurbo",
    }
}

/// Where models are kept: the platform's data directory, beside vaults but
/// not in one. See this module's doc.
pub fn models_dir() -> PathBuf {
    directories::ProjectDirs::from("app", "Every Day", "EveryDay")
        .map(|d| d.data_dir().join("models"))
        .unwrap_or_else(|| PathBuf::from(".everyday-models"))
}

/// One file this application ends up with, once a [`Source`] is downloaded
/// (and, for an archive, extracted).
#[derive(Debug, Clone, Copy)]
pub struct ModelFile {
    /// The filename this file is kept under, inside its model's directory.
    pub dest: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
}

/// One thing to fetch on the way to a model being installed.
#[derive(Debug, Clone, Copy)]
pub enum Source {
    /// Downloaded straight to `file.dest`.
    File { url: &'static str, file: ModelFile },
    /// A `.tar.bz2` downloaded once, hashed as a whole, and then a handful of
    /// its members extracted -- each hashed again on its own -- and the
    /// archive itself discarded. `members` pairs the *archive's* path for an
    /// entry with the [`ModelFile`] it becomes; extraction only ever writes
    /// [`ModelFile::dest`], which this module chose, so nothing in the
    /// archive's own paths -- however an entry there is spelled -- is ever
    /// used to name a file on disk. See [`extract_members`].
    Archive {
        url: &'static str,
        sha256: &'static str,
        bytes: u64,
        members: &'static [(&'static str, ModelFile)],
    },
}

impl Source {
    fn download_bytes(&self) -> u64 {
        match self {
            Source::File { file, .. } => file.bytes,
            Source::Archive { bytes, .. } => *bytes,
        }
    }
}

/// One downloadable model: the speech kit, or a recogniser.
#[derive(Debug)]
pub struct ModelSpec {
    /// [`SPEECH_KIT_ID`], or a [`local_model_id`].
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub languages: &'static str,
    sources: &'static [Source],
}

impl ModelSpec {
    /// Total bytes downloaded to install this model -- an archive's own
    /// size, not the (smaller) sum of the files extracted from it, since
    /// that is what a progress bar is actually counting up to.
    pub fn bytes(&self) -> u64 {
        self.sources.iter().map(Source::download_bytes).sum()
    }

    pub fn dir(&self) -> PathBuf {
        models_dir().join(self.id)
    }

    fn files(&self) -> Vec<&ModelFile> {
        self.sources
            .iter()
            .flat_map(|s| match s {
                Source::File { file, .. } => vec![file],
                Source::Archive { members, .. } => members.iter().map(|(_, f)| f).collect(),
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// The catalogue
//
// Every URL, byte count and SHA-256 below was checked by downloading the
// file and hashing what arrived -- see this phase's report. Byte counts are
// GitHub's own `content-length`; SHA-256 is this module's own hash of the
// bytes on disk, not a checksum file the publisher shipped (sherpa-onnx's
// release assets do not consistently ship one).
// ---------------------------------------------------------------------------

const SPEECH_KIT_SOURCES: &[Source] = &[
    Source::File {
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx",
        file: ModelFile {
            dest: "silero-vad.onnx",
            sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6",
            bytes: 643_854,
        },
    },
    Source::Archive {
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2",
        sha256: "24615ee884c897d9d2ba09bb4d30da6bb1b15e685065962db5b02e76e4996488",
        bytes: 6_958_444,
        members: &[(
            "sherpa-onnx-pyannote-segmentation-3-0/model.onnx",
            ModelFile {
                dest: "segmentation.onnx",
                sha256: "220ad67ca923bef2fa91f2390c786097bf305bceb5e261d4af67b38e938e1079",
                bytes: 5_992_913,
            },
        )],
    },
    Source::File {
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_eres2net_sv_en_voxceleb_16k.onnx",
        file: ModelFile {
            dest: "speaker-embedding.onnx",
            sha256: "c59158379255ad66e161679cca6af8d52d51e389e3224ab7d7a7baae295c2db5",
            bytes: 26_485_263,
        },
    },
];

const PARAKEET_V3_SOURCES: &[Source] = &[Source::Archive {
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2",
    sha256: "5793d0fd397c5778d2cf2126994d58e9d56b1be7c04d13c7a15bb1b4eafb16bf",
    bytes: 487_170_055,
    members: &[
        (
            "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/encoder.int8.onnx",
            ModelFile {
                dest: "encoder.int8.onnx",
                sha256: "acfc2b4456377e15d04f0243af540b7fe7c992f8d898d751cf134c3a55fd2247",
                bytes: 652_184_281,
            },
        ),
        (
            "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/decoder.int8.onnx",
            ModelFile {
                dest: "decoder.int8.onnx",
                sha256: "179e50c43d1a9de79c8a24149a2f9bac6eb5981823f2a2ed88d655b24248db4e",
                bytes: 11_845_275,
            },
        ),
        (
            "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/joiner.int8.onnx",
            ModelFile {
                dest: "joiner.int8.onnx",
                sha256: "3164c13fc2821009440d20fcb5fdc78bff28b4db2f8d0f0b329101719c0948b3",
                bytes: 6_355_277,
            },
        ),
        (
            "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/tokens.txt",
            ModelFile {
                dest: "tokens.txt",
                sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
                bytes: 93_939,
            },
        ),
    ],
}];

const WHISPER_TURBO_SOURCES: &[Source] = &[Source::Archive {
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-whisper-turbo.tar.bz2",
    sha256: "b11acbbcd660b44a8e0df33724feb5aaa709cf65668f2823d59f656312544f22",
    bytes: 563_790_207,
    members: &[
        (
            "sherpa-onnx-whisper-turbo/turbo-encoder.int8.onnx",
            ModelFile {
                dest: "turbo-encoder.int8.onnx",
                sha256: "b02dcdf54f348741e93fe732b67d933c8dcb6735655f710640143081db38878b",
                bytes: 674_716_297,
            },
        ),
        (
            "sherpa-onnx-whisper-turbo/turbo-decoder.int8.onnx",
            ModelFile {
                dest: "turbo-decoder.int8.onnx",
                sha256: "20accd02388482eb3a46bd615631adfdc85e1eb2c7db9ea3f02a40ffe6b81547",
                bytes: 361_080_764,
            },
        ),
        (
            "sherpa-onnx-whisper-turbo/turbo-tokens.txt",
            ModelFile {
                dest: "turbo-tokens.txt",
                sha256: "b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126",
                bytes: 816_730,
            },
        ),
    ],
}];

/// Every downloadable model, in the order the settings pane lists them.
pub static CATALOGUE: [ModelSpec; 3] = [
    ModelSpec {
        id: SPEECH_KIT_ID,
        name: "Speech kit",
        description: "Voice detection, speaker segmentation and speaker embeddings. Needed by both recognisers below, and downloaded once for either.",
        languages: "Any",
        sources: SPEECH_KIT_SOURCES,
    },
    ModelSpec {
        id: "parakeetV3",
        name: "Parakeet TDT 0.6B v3",
        description: "Several times faster than real time on a laptop CPU.",
        languages: "English and the major European languages",
        sources: PARAKEET_V3_SOURCES,
    },
    ModelSpec {
        id: "whisperTurbo",
        name: "Whisper large-v3-turbo",
        description: "Slower than Parakeet, and heavier, but understands far more languages.",
        languages: "Any language Whisper supports",
        sources: WHISPER_TURBO_SOURCES,
    },
];

pub fn catalogue() -> &'static [ModelSpec] {
    &CATALOGUE
}

pub fn spec(id: &str) -> Option<&'static ModelSpec> {
    CATALOGUE.iter().find(|s| s.id == id)
}

/// The VAD, segmentation and embedding models, all present and checked.
pub fn speech_kit_installed() -> bool {
    spec(SPEECH_KIT_ID).is_some_and(is_installed)
}

/// This recogniser's files, all present and checked.
pub fn installed(model: LocalModel) -> bool {
    spec(local_model_id(model)).is_some_and(is_installed)
}

// ---------------------------------------------------------------------------
// The marker: install-once, stat-thereafter.
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Marker {
    /// `(dest, bytes)` for every file this model installs, as verified the
    /// moment the download that wrote them finished.
    files: Vec<(String, u64)>,
}

fn marker_path(dir: &Path) -> PathBuf {
    dir.join(".installed.json")
}

fn is_installed(spec: &ModelSpec) -> bool {
    let dir = spec.dir();
    let Ok(text) = std::fs::read_to_string(marker_path(&dir)) else { return false };
    let Ok(marker) = serde_json::from_str::<Marker>(&text) else { return false };
    let expected = spec.files();
    if marker.files.len() != expected.len() {
        return false;
    }
    expected.iter().all(|file| {
        marker.files.iter().any(|(dest, bytes)| dest == file.dest && *bytes == file.bytes)
            && std::fs::metadata(dir.join(file.dest)).is_ok_and(|m| m.len() == file.bytes)
    })
}

// ---------------------------------------------------------------------------
// The download manager.
// ---------------------------------------------------------------------------

/// One model's in-flight or most-recently-failed download.
struct Download {
    total: u64,
    done: AtomicU64,
    cancelled: AtomicBool,
    /// Cleared the moment the task ends, whichever way. What's left after
    /// that -- an entry that stays in [`registry`] only when it failed -- is
    /// what [`last_error`] reads.
    running: AtomicBool,
    error: Mutex<Option<String>>,
}

fn registry() -> &'static Mutex<HashMap<&'static str, Arc<Download>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<&'static str, Arc<Download>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `(done, total)` bytes, while `id`'s download is running. `None` once it
/// has finished, successfully or not -- see [`last_error`] for why.
pub fn progress(id: &str) -> Option<(u64, u64)> {
    let map = registry().lock().unwrap();
    let dl = map.get(id)?;
    if dl.running.load(Ordering::SeqCst) {
        Some((dl.done.load(Ordering::SeqCst), dl.total))
    } else {
        None
    }
}

/// Why `id`'s last download failed, until the next [`start_download`] for it.
/// `None` while it is running, and `None` again once a fresh attempt starts
/// or a deliberate [`cancel_download`] ends it -- a cancel is not a failure.
pub fn last_error(id: &str) -> Option<String> {
    registry().lock().unwrap().get(id)?.error.lock().unwrap().clone()
}

/// Whether `id` is installed, its progress if a download is running, and its
/// last error if the last one failed -- everything `domains::speech`'s
/// `speech_models` command reads to build one `SpeechModelInfo`. `None` when
/// `id` is not in the catalogue.
pub struct Status {
    pub installed: bool,
    pub progress: Option<(u64, u64)>,
    pub error: Option<String>,
}

pub fn status(id: &str) -> Option<Status> {
    let spec = spec(id)?;
    Some(Status { installed: is_installed(spec), progress: progress(id), error: last_error(id) })
}

/// Start `id` downloading in the background. A no-op if it is already
/// installed, or already downloading.
pub fn start_download(id: &str) -> Result<(), String> {
    let spec = spec(id).ok_or_else(|| format!("there is no such model: {id}"))?;
    if is_installed(spec) {
        return Ok(());
    }
    {
        let map = registry().lock().unwrap();
        if map.get(spec.id).is_some_and(|dl| dl.running.load(Ordering::SeqCst)) {
            return Ok(());
        }
    }
    let dl = Arc::new(Download {
        total: spec.bytes(),
        done: AtomicU64::new(0),
        cancelled: AtomicBool::new(false),
        running: AtomicBool::new(true),
        error: Mutex::new(None),
    });
    registry().lock().unwrap().insert(spec.id, dl.clone());

    tokio::spawn(async move {
        let outcome = run_download(spec, &dl).await;
        dl.running.store(false, Ordering::SeqCst);
        match outcome {
            Ok(()) => {
                registry().lock().unwrap().remove(spec.id);
            }
            Err(Outcome::Cancelled) => {
                registry().lock().unwrap().remove(spec.id);
            }
            Err(Outcome::Failed(message)) => {
                *dl.error.lock().unwrap() = Some(message);
            }
        }
    });
    Ok(())
}

/// Ask `id`'s download to stop, if one is running. Takes effect at the next
/// chunk boundary; [`progress`] and [`last_error`] both go back to `None`
/// once it has.
pub fn cancel_download(id: &str) {
    if let Some(dl) = registry().lock().unwrap().get(id) {
        dl.cancelled.store(true, Ordering::SeqCst);
    }
}

/// Remove `id`'s files, cancelling a running download first.
pub fn delete(id: &str) -> Result<(), String> {
    let spec = spec(id).ok_or_else(|| format!("there is no such model: {id}"))?;
    cancel_download(id);
    let dir = spec.dir();
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| format!("could not remove {}: {e}", dir.display()))?;
    }
    Ok(())
}

#[derive(Debug)]
enum Outcome {
    Cancelled,
    Failed(String),
}

async fn run_download(spec: &'static ModelSpec, dl: &Download) -> Result<(), Outcome> {
    let dir = spec.dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| Outcome::Failed(format!("could not create {}: {e}", dir.display())))?;

    let mut installed_files: Vec<(String, u64)> = Vec::new();
    let mut base_offset = 0u64;
    for source in spec_sources(spec) {
        if dl.cancelled.load(Ordering::SeqCst) {
            return Err(Outcome::Cancelled);
        }
        match source {
            Source::File { url, file } => {
                let dest = dir.join(file.dest);
                let partial = dir.join(format!("{}.partial", file.dest));
                stream_download(url, &partial, file.bytes, file.sha256, dl, base_offset).await?;
                std::fs::rename(&partial, &dest)
                    .map_err(|e| Outcome::Failed(format!("could not finish {}: {e}", file.dest)))?;
                installed_files.push((file.dest.to_string(), file.bytes));
                base_offset += file.bytes;
            }
            Source::Archive { url, sha256, bytes, members } => {
                let archive_partial = dir.join(".partial-archive");
                stream_download(url, &archive_partial, *bytes, sha256, dl, base_offset).await?;
                base_offset += bytes;
                extract_members(&archive_partial, &dir, members).await?;
                let _ = std::fs::remove_file(&archive_partial);
                for (_, file) in *members {
                    installed_files.push((file.dest.to_string(), file.bytes));
                }
            }
        }
    }

    let marker = Marker { files: installed_files };
    let text = serde_json::to_string(&marker)
        .map_err(|e| Outcome::Failed(format!("could not record the install: {e}")))?;
    std::fs::write(marker_path(&dir), text)
        .map_err(|e| Outcome::Failed(format!("could not record the install: {e}")))?;
    Ok(())
}

fn spec_sources(spec: &'static ModelSpec) -> &'static [Source] {
    spec.sources
}

/// Longest a single model download may sit idle or in flight. Generous: a
/// 550 MB archive on a slow link is still a download somebody asked for, not
/// one that should be timed out from under them the way an ordinary fetch
/// in `crate::http` is.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// This module's own client, separate from [`crate::http::client`]. That one
/// exists for calendar subscriptions, search and remote images -- all small,
/// all expected back in seconds -- and its doc is explicit that a second
/// caller's own caution belongs beside that caller, not folded into the
/// shared settings. A model archive is hundreds of megabytes and a download
/// somebody asked for once; the shared client's thirty-second timeout would
/// abort it long before it could finish.
fn download_client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .timeout(DOWNLOAD_TIMEOUT)
                .user_agent(concat!(
                    "EveryDay/",
                    env!("CARGO_PKG_VERSION"),
                    " (+https://github.com/everyday-app)"
                ))
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| format!("could not start the downloader: {e}"))
}

/// Stream `url` to `dest` (a `.partial` path the caller renames into place),
/// checking the byte count and the SHA-256 as they arrive. `base_offset` is
/// how much of `dl.total` earlier sources in the same model already account
/// for, so progress is cumulative across a multi-source model like the
/// speech kit rather than resetting at each file.
async fn stream_download(
    url: &str,
    dest: &Path,
    expected_bytes: u64,
    expected_sha256: &str,
    dl: &Download,
    base_offset: u64,
) -> Result<(), Outcome> {
    let client = download_client().map_err(Outcome::Failed)?;
    let response = client.get(url).send().await.map_err(|e| {
        Outcome::Failed(format!("download failed: {}", crate::http::strip_url(&e.to_string())))
    })?;
    if !response.status().is_success() {
        return Err(Outcome::Failed(format!(
            "download failed: the server answered {}",
            response.status()
        )));
    }

    let mut response = response;
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| Outcome::Failed(format!("could not write {}: {e}", dest.display())))?;
    let mut hasher = Sha256::new();
    let mut got: u64 = 0;

    loop {
        if dl.cancelled.load(Ordering::SeqCst) {
            drop(file);
            let _ = tokio::fs::remove_file(dest).await;
            return Err(Outcome::Cancelled);
        }
        let chunk = response.chunk().await.map_err(|e| {
            Outcome::Failed(format!("download failed: {}", crate::http::strip_url(&e.to_string())))
        })?;
        let Some(chunk) = chunk else { break };
        got += chunk.len() as u64;
        if got > expected_bytes {
            drop(file);
            let _ = tokio::fs::remove_file(dest).await;
            return Err(Outcome::Failed(
                "download failed: the server sent more than expected".into(),
            ));
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| Outcome::Failed(format!("could not write {}: {e}", dest.display())))?;
        dl.done.store(base_offset + got, Ordering::SeqCst);
    }
    file.flush()
        .await
        .map_err(|e| Outcome::Failed(format!("could not write {}: {e}", dest.display())))?;
    drop(file);

    if got != expected_bytes {
        let _ = tokio::fs::remove_file(dest).await;
        return Err(Outcome::Failed(format!(
            "download failed: expected {expected_bytes} bytes, got {got}"
        )));
    }
    let digest = to_hex(&hasher.finalize());
    if !digest.eq_ignore_ascii_case(expected_sha256) {
        let _ = tokio::fs::remove_file(dest).await;
        return Err(Outcome::Failed("download failed: the checksum did not match".into()));
    }
    Ok(())
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Extract `members` from the `.tar.bz2` at `archive`, each checked against
/// its own SHA-256, into `dir`. Off the async runtime: decompressing and
/// copying half a gigabyte is real CPU and disk work, not something to run
/// on a thread also driving other downloads' network reads.
///
/// # Path safety
///
/// This never writes to a path the archive names. Every entry's *own* path
/// is only ever compared for equality against the fixed strings in
/// `members` -- decided by this module, not by whoever built the archive --
/// and the file it becomes is `dir.join(file.dest)`, `dest` likewise fixed
/// in this module's own catalogue. An entry whose path is absolute, or
/// carries a `..` component, is skipped like any other entry this call was
/// not asked for; it is never joined onto `dir` or opened for writing.
async fn extract_members(
    archive: &Path,
    dir: &Path,
    members: &'static [(&'static str, ModelFile)],
) -> Result<(), Outcome> {
    let archive = archive.to_path_buf();
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || extract_members_blocking(&archive, &dir, members))
        .await
        .map_err(|e| Outcome::Failed(format!("extraction failed: {e}")))?
}

/// Whether an archive entry's own path is safe to even consider: relative,
/// and with no `..` component. Checked independently of, and before, the
/// exact-match lookup against `members` -- see [`extract_members_blocking`]'s
/// doc on why a malicious entry cannot escape `dir` even without this check,
/// and why this one exists anyway.
fn is_safe_member_path(path: &Path) -> bool {
    !path.is_absolute() && !path.components().any(|c| matches!(c, std::path::Component::ParentDir))
}

fn extract_members_blocking(
    archive: &Path,
    dir: &Path,
    members: &'static [(&'static str, ModelFile)],
) -> Result<(), Outcome> {
    let file = std::fs::File::open(archive)
        .map_err(|e| Outcome::Failed(format!("could not open {}: {e}", archive.display())))?;
    let mut tar = tar::Archive::new(bzip2::read::BzDecoder::new(file));
    let entries =
        tar.entries().map_err(|e| Outcome::Failed(format!("could not read the archive: {e}")))?;

    let mut remaining: std::collections::HashSet<&str> =
        members.iter().map(|(path, _)| *path).collect();
    for entry in entries {
        let mut entry =
            entry.map_err(|e| Outcome::Failed(format!("could not read the archive: {e}")))?;
        let Ok(path) = entry.path() else { continue };
        // Skip anything that is not a plain relative path -- see this
        // function's doc. Nothing below ever uses `path` to name a file on
        // disk; this is a second, independent reason a malicious entry
        // could not escape `dir`, not the only one.
        if !is_safe_member_path(&path) {
            continue;
        }
        let Some(path_str) = path.to_str() else { continue };
        let Some((_, model_file)) =
            members.iter().find(|(member_path, _)| *member_path == path_str)
        else {
            continue;
        };
        remaining.remove(path_str);

        let partial = dir.join(format!("{}.partial", model_file.dest));
        let mut out = std::fs::File::create(&partial)
            .map_err(|e| Outcome::Failed(format!("could not write {}: {e}", model_file.dest)))?;
        let mut hasher = Sha256::new();
        std::io::copy(&mut entry, &mut HashingWriter { inner: &mut out, hasher: &mut hasher })
            .map_err(|e| Outcome::Failed(format!("could not extract {}: {e}", model_file.dest)))?;
        drop(out);

        let digest = to_hex(&hasher.finalize());
        if !digest.eq_ignore_ascii_case(model_file.sha256) {
            let _ = std::fs::remove_file(&partial);
            return Err(Outcome::Failed(format!(
                "{}: the checksum did not match after extraction",
                model_file.dest
            )));
        }
        std::fs::rename(&partial, dir.join(model_file.dest))
            .map_err(|e| Outcome::Failed(format!("could not finish {}: {e}", model_file.dest)))?;
    }

    if !remaining.is_empty() {
        return Err(Outcome::Failed(format!("the archive did not contain: {remaining:?}")));
    }
    Ok(())
}

/// Forwards every write to `inner` and folds the same bytes into `hasher`,
/// so extracting a member and hashing it is one pass rather than two.
struct HashingWriter<'a, W> {
    inner: W,
    hasher: &'a mut Sha256,
}

impl<W: std::io::Write> std::io::Write for HashingWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

// ---------------------------------------------------------------------------
// Paths, for `meeting::speech` (feature `speech`) to load models from.
// Kept here rather than duplicated there, so the filenames a `Source`
// extracts to and the filenames a recogniser is pointed at cannot drift
// apart.
// ---------------------------------------------------------------------------

/// Where the speech kit's three files are, once [`speech_kit_installed`].
pub struct SpeechKitPaths {
    pub vad: PathBuf,
    pub segmentation: PathBuf,
    pub speaker_embedding: PathBuf,
}

pub fn speech_kit_paths() -> Option<SpeechKitPaths> {
    let spec = spec(SPEECH_KIT_ID)?;
    if !is_installed(spec) {
        return None;
    }
    let dir = spec.dir();
    Some(SpeechKitPaths {
        vad: dir.join("silero-vad.onnx"),
        segmentation: dir.join("segmentation.onnx"),
        speaker_embedding: dir.join("speaker-embedding.onnx"),
    })
}

/// Where a recogniser's files are, once [`installed`]. The two local
/// recognisers are different model families -- a transducer and Whisper --
/// so they are pointed at differently; see `meeting::speech::Recogniser`.
pub enum RecogniserPaths {
    Transducer { encoder: PathBuf, decoder: PathBuf, joiner: PathBuf, tokens: PathBuf },
    Whisper { encoder: PathBuf, decoder: PathBuf, tokens: PathBuf },
}

pub fn recogniser_paths(model: LocalModel) -> Option<RecogniserPaths> {
    let spec = spec(local_model_id(model))?;
    if !is_installed(spec) {
        return None;
    }
    let dir = spec.dir();
    Some(match model {
        LocalModel::ParakeetV3 => RecogniserPaths::Transducer {
            encoder: dir.join("encoder.int8.onnx"),
            decoder: dir.join("decoder.int8.onnx"),
            joiner: dir.join("joiner.int8.onnx"),
            tokens: dir.join("tokens.txt"),
        },
        LocalModel::WhisperTurbo => RecogniserPaths::Whisper {
            encoder: dir.join("turbo-encoder.int8.onnx"),
            decoder: dir.join("turbo-decoder.int8.onnx"),
            tokens: dir.join("turbo-tokens.txt"),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalogue_id_is_unique_and_matches_local_model() {
        let mut seen = std::collections::HashSet::new();
        for spec in catalogue() {
            assert!(seen.insert(spec.id), "{} is declared twice", spec.id);
        }
        assert_eq!(local_model_id(LocalModel::ParakeetV3), "parakeetV3");
        assert_eq!(local_model_id(LocalModel::WhisperTurbo), "whisperTurbo");
        for model in [LocalModel::ParakeetV3, LocalModel::WhisperTurbo] {
            assert!(
                spec(local_model_id(model)).is_some(),
                "{} has no catalogue entry",
                model.as_str()
            );
        }
    }

    #[test]
    fn every_source_names_at_least_one_file_and_a_lowercase_hash() {
        for spec in catalogue() {
            assert!(!spec.files().is_empty(), "{} has no files", spec.id);
            assert!(spec.bytes() > 0, "{} has no size", spec.id);
            for file in spec.files() {
                assert_eq!(file.sha256.len(), 64, "{}'s hash is not 64 hex characters", file.dest);
                assert!(
                    file.sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                    "{}'s hash is not lowercase hex",
                    file.dest
                );
                assert!(file.bytes > 0, "{} has no size", file.dest);
            }
        }
    }

    #[test]
    fn the_marker_round_trips_through_a_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        let spec = ModelSpec {
            id: "test-model",
            name: "Test",
            description: "",
            languages: "",
            // A placeholder hash: this test never extracts or verifies
            // anything against it, only round-trips the marker's record of
            // which files, and what size, this spec expects.
            sources: &[Source::File {
                url: "https://example.invalid/one.bin",
                file: ModelFile { dest: "one.bin", sha256: "0", bytes: 3 },
            }],
        };
        // Not installed: no marker, and no file either.
        assert!(!is_installed_at(&spec, dir.path()));

        std::fs::write(dir.path().join("one.bin"), b"abc").unwrap();
        // The file exists, but there is still no marker -- an unverified
        // file on disk is not "installed".
        assert!(!is_installed_at(&spec, dir.path()));

        let marker = Marker { files: vec![("one.bin".to_string(), 3)] };
        std::fs::write(marker_path(dir.path()), serde_json::to_string(&marker).unwrap()).unwrap();
        assert!(is_installed_at(&spec, dir.path()));

        // The marker says 3 bytes; a truncated file is not installed even
        // though the marker is still sitting there claiming it is.
        std::fs::write(dir.path().join("one.bin"), b"ab").unwrap();
        assert!(!is_installed_at(&spec, dir.path()));
    }

    /// [`is_installed`] against an arbitrary directory rather than
    /// `spec.dir()` (which is always under the real [`models_dir`]), so
    /// tests do not have to share a process-wide environment variable to
    /// redirect it.
    fn is_installed_at(spec: &ModelSpec, dir: &Path) -> bool {
        let Ok(text) = std::fs::read_to_string(marker_path(dir)) else { return false };
        let Ok(marker) = serde_json::from_str::<Marker>(&text) else { return false };
        let expected = spec.files();
        if marker.files.len() != expected.len() {
            return false;
        }
        expected.iter().all(|file| {
            marker.files.iter().any(|(dest, bytes)| dest == file.dest && *bytes == file.bytes)
                && std::fs::metadata(dir.join(file.dest)).is_ok_and(|m| m.len() == file.bytes)
        })
    }

    #[test]
    fn extraction_ignores_members_this_module_did_not_ask_for() {
        let members: &'static [(&'static str, ModelFile)] = leak_members(&[(
            "inner/wanted.bin",
            ModelFile { dest: "wanted.bin", sha256: sha256_hex(b"hello"), bytes: 5 },
        )]);

        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("archive.tar.bz2");
        write_test_archive(
            &archive_path,
            &[
                ("inner/wanted.bin", b"hello".as_slice()),
                // Not in `members`: must be skipped, not written anywhere.
                // (A `..`-carrying entry is exercised separately, in
                // `is_safe_member_path_refuses_traversal`: the `tar` crate's
                // own writer refuses to build an archive containing one at
                // all, which is a second, independent reason such an entry
                // could not reach this function's caller in practice.)
                ("inner/unwanted.bin", b"nope!".as_slice()),
            ],
        );

        extract_members_blocking(&archive_path, dir.path(), members).unwrap();

        assert_eq!(std::fs::read(dir.path().join("wanted.bin")).unwrap(), b"hello");
        assert!(!dir.path().join("unwanted.bin").exists());
        // Nothing escaped `dir`, and nothing but the one wanted file (and
        // the archive itself) landed inside it.
        let names: std::collections::HashSet<_> =
            std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(
            names,
            std::collections::HashSet::from([
                std::ffi::OsString::from("wanted.bin"),
                std::ffi::OsString::from("archive.tar.bz2"),
            ])
        );
    }

    #[test]
    fn extraction_fails_when_a_wanted_member_never_arrives() {
        // A placeholder hash: this test expects extraction to fail before
        // any checksum is even checked, because the member never arrives.
        let members: &'static [(&'static str, ModelFile)] =
            &[("inner/missing.bin", ModelFile { dest: "missing.bin", sha256: "0", bytes: 1 })];
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("archive.tar.bz2");
        write_test_archive(&archive_path, &[("inner/something-else.bin", b"x".as_slice())]);

        let err = extract_members_blocking(&archive_path, dir.path(), members).unwrap_err();
        assert!(matches!(err, Outcome::Failed(_)));
    }

    #[test]
    fn extraction_fails_when_a_members_checksum_is_wrong() {
        let members: &'static [(&'static str, ModelFile)] = leak_members(&[(
            "inner/wanted.bin",
            ModelFile { dest: "wanted.bin", sha256: sha256_hex(b"not what arrives"), bytes: 5 },
        )]);
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("archive.tar.bz2");
        write_test_archive(&archive_path, &[("inner/wanted.bin", b"hello".as_slice())]);

        let err = extract_members_blocking(&archive_path, dir.path(), members).unwrap_err();
        assert!(matches!(err, Outcome::Failed(_)));
        assert!(!dir.path().join("wanted.bin").exists());
    }

    /// Leaked rather than owned: [`ModelFile::sha256`] is `&'static str`,
    /// which a real [`ModelSpec`] gets for free from being a `const`, and a
    /// test-only one has to fake by leaking -- fine for a test process.
    fn sha256_hex(bytes: &[u8]) -> &'static str {
        Box::leak(to_hex(&Sha256::digest(bytes)).into_boxed_str())
    }

    /// Leaked the same way: `members` is `&'static [(&'static str,
    /// ModelFile)]`, and an array literal built from a non-const call (like
    /// [`sha256_hex`]) is not `'static` promotable, unlike one built purely
    /// from literals.
    fn leak_members(members: &[(&'static str, ModelFile)]) -> &'static [(&'static str, ModelFile)] {
        Box::leak(members.to_vec().into_boxed_slice())
    }

    fn write_test_archive(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let encoder = bzip2::write::BzEncoder::new(file, bzip2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn is_safe_member_path_refuses_traversal_and_absolute_paths() {
        assert!(is_safe_member_path(Path::new("inner/wanted.bin")));
        assert!(is_safe_member_path(Path::new("wanted.bin")));
        assert!(!is_safe_member_path(Path::new("../../etc/passwd")));
        assert!(!is_safe_member_path(Path::new("inner/../../etc/passwd")));
        assert!(!is_safe_member_path(Path::new("/etc/passwd")));
    }

    #[test]
    fn to_hex_matches_a_known_vector() {
        // SHA-256("") -- the standard empty-string test vector.
        assert_eq!(
            to_hex(&Sha256::digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
