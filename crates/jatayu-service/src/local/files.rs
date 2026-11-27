//! The files of every model that runs on this computer: what can be
//! downloaded, where it goes, and whether it is there.
//!
//! Not behind the `speech` or `decisions` features: settings has to be able
//! to say what a build without them would need, and whether a model is
//! present is a question about files, not about sherpa-onnx or llama.cpp.
//! Keeping a loaded one in memory is [`super::resident`]'s business.
//!
//! # The catalogue
//!
//! Two kinds of model ([`ModelKind`]), one list.
//!
//! **Speech**, per `docs/plans/meeting-notes.md`'s "The libraries" and
//! "Models": [`SPEECH_KIT_ID`] (Silero VAD, pyannote `segmentation-3.0`, and
//! a small English speaker-embedding model -- shared by every recogniser and
//! needed from phase 2 on) and the local recognisers named by
//! [`LocalModel`]. Parakeet and Whisper are `.tar.bz2` archives holding files
//! this application does not want (Python export scripts, a licence file,
//! test WAVs); [`Source::Archive`] downloads the whole archive, checks *its*
//! hash, and extracts only the named members, each checked again on its own.
//! The two Qwen3-ASR recognisers are two GGUF files each, run by llama.cpp
//! as the decision models are.
//!
//! **Decision**, per `docs/plans/mail-sorting.md`: the three
//! [`LocalDecisionModel`]s, each one GGUF file from llama.cpp's own
//! conversions, fetched from a fixed revision of its repository rather than
//! from whatever `main` says today.
//!
//! Every file is pinned by URL, byte count and SHA-256, checked by
//! downloading each one and hashing what arrived.
//!
//! # Where they live
//!
//! [`models_dir`] is the platform's data directory --
//! `jatayu_vault::machine_data_dir`, the folder the default vault is in -- plus
//! `models/`. Models are neither secret nor per-vault, so they sit beside
//! vaults rather than inside one -- a vault backup must not silently grow by
//! 500 MB because a recogniser happened to be installed. Asked of the vault
//! crate rather than built here, because that is where an installation
//! under a former name (Every Day's, or Karma Yoga's) is carried across
//! before anything answers: a models folder made under the new name first
//! would leave the old one, and the vault in it, behind.
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

use jatayu_core::imagegen::LocalImageModel;
use jatayu_core::llm::LocalDecisionModel;
use jatayu_core::meeting::LocalModel;
use jatayu_core::speech::LocalVoice;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// The speech kit's id in the catalogue and on the wire. See
/// `ui/src/lib/types.ts`'s `LocalModelInfo.id`.
pub const SPEECH_KIT_ID: &str = "speechKit";

/// The microphone's two filters' ids in the catalogue and on the wire: see
/// `speech::cleanup` and the app's `microphone.rs`.
pub const VOICE_ISOLATION_ID: &str = "voiceIsolation";
pub const NOISE_FILTER_ID: &str = "noiseFilter";

/// The id [`LocalModel`] serialises as, and the id the catalogue and the
/// wire use for it. Spelled out rather than read off `serde` -- this module
/// has no reason to depend on `jatayu_core::meeting::LocalModel`'s own
/// wire representation matching its catalogue key, even though today it
/// does; a `#[test]` below pins the two together instead.
pub fn local_model_id(model: LocalModel) -> &'static str {
    match model {
        LocalModel::ParakeetV3 => "parakeetV3",
        LocalModel::WhisperTurbo => "whisperTurbo",
        LocalModel::Qwen3Asr17b => "qwen3Asr17b",
        LocalModel::Qwen3Asr06b => "qwen3Asr06b",
    }
}

/// Where models are kept: the platform's data directory, beside vaults but
/// not in one. See this module's doc.
pub fn models_dir() -> PathBuf {
    jatayu_vault::machine_data_dir().join("models")
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

/// What a model is for -- which settings pane lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    /// Listening: the speech kit and the recognisers, for meeting notes, the
    /// assistant's voice and whatever listens next.
    Speech,
    /// Sorting mail: the [`LocalDecisionModel`]s.
    Decision,
    /// The assistant's voice: the [`LocalVoice`]s.
    Voice,
    /// A picture model, or the stable-diffusion.cpp binary that runs one.
    Image,
}

/// One downloadable model: the speech kit, a recogniser, or a decision
/// model.
#[derive(Debug)]
pub struct ModelSpec {
    /// [`SPEECH_KIT_ID`], a [`local_model_id`], or a
    /// [`LocalDecisionModel::id`].
    pub id: &'static str,
    pub kind: ModelKind,
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
            sha256: "c4947caf150b8502fdd75bbe4089b971ab1ec42e90ee3ba547faca2cde44d9ee",
            bytes: 26_485_264,
        },
    },
];

const PARAKEET_V3_SOURCES: &[Source] = &[Source::Archive {
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2",
    sha256: "1495e91b59d7fc53ddca91f3431758ec99f8fa253fdb0e1e851be0f72b14b0f9",
    bytes: 487_211_443,
    members: &[
        (
            "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/encoder.int8.onnx",
            ModelFile {
                dest: "encoder.int8.onnx",
                sha256: "1afbe69c1ffdabee94755f42e4daf1d93549131a3da11356bda84fcef0f2c479",
                bytes: 652_282_294,
            },
        ),
        (
            "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/decoder.int8.onnx",
            ModelFile {
                dest: "decoder.int8.onnx",
                sha256: "0222a86147299d4dee90edf95b6d13827edb42ad8b7c9cf0b2582f030937c0b5",
                bytes: 11_845_274,
            },
        ),
        (
            "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/joiner.int8.onnx",
            ModelFile {
                dest: "joiner.int8.onnx",
                sha256: "cbd02f3696e9944970111fb439d9c3fd365a80374565f13f2b9c7007d253fc98",
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

/// The decision models are one GGUF each, from llama.cpp's own conversions
/// (`ggml-org/vllm-d3-*-GGUF` on Hugging Face) of vLLM Semantic Router's
/// Decision 3.0 models. The revision in each URL is that repository's
/// commit on the day it was pinned; the SHA-256 is Hugging Face's own LFS
/// object id for the file, checked against a download of it. d3-lite and
/// d3-nano are 8-bit (llama.cpp publishes nothing smaller for them); d3-mini
/// is 4-bit, its smallest, at little cost to its answers.
const D3_LITE_SOURCES: &[Source] = &[Source::File {
    url: "https://huggingface.co/ggml-org/vllm-d3-lite-GGUF/resolve/0d544132bc35075d6346013f56454ea6dc19e640/vllm-d3-lite-Q8_0.gguf",
    file: ModelFile {
        dest: DECISION_MODEL_FILE,
        sha256: "14e1d3ad398bdc245b5afe42e4e2a90e12b17fac699d3ab28c3b7632aaacc827",
        bytes: 1_082_017_408,
    },
}];

const D3_NANO_SOURCES: &[Source] = &[Source::File {
    url: "https://huggingface.co/ggml-org/vllm-d3-nano-GGUF/resolve/f2256dfaff5b8337e8bf67ca064aeef18ee14a79/vllm-d3-nano-Q8_0.gguf",
    file: ModelFile {
        dest: DECISION_MODEL_FILE,
        sha256: "a4d7563d9622a4c34cc7276a3381c3e27a25bbf8d4c0f5ee2f1404fc89ce2ab9",
        bytes: 2_552_358_496,
    },
}];

const D3_MINI_SOURCES: &[Source] = &[Source::File {
    url: "https://huggingface.co/ggml-org/vllm-d3-mini-GGUF/resolve/8b29fd3b87373ea85fa59cc2f26251c1591d771a/vllm-d3-mini-Q4_K_M.gguf",
    file: ModelFile {
        dest: DECISION_MODEL_FILE,
        sha256: "1e5e5a87dd6903b6d8585331eee94a2f01d5069e369b143e1dceb9de51ba2902",
        bytes: 3_707_523_584,
    },
}];

/// Gradium's Phonon 31M, English, at the revision `ptts` 0.7.1 names as its
/// default model, and its English speakers from Gradium's voice catalogue at
/// the revision `ptts` names for that -- see [`LocalVoice`] and
/// `jatayu_core::speech::PHONON_SPEAKERS`. The model's weights and the
/// voices are Apache-2.0. Only what the runtime reads is fetched: the
/// config, the tokenizer, the q8 weights (`ptts` keeps them quantized on a
/// CPU) and one embedding per speaker; the f32 and q4 weights, and the
/// voices that are characters rather than people, are left behind.
const PHONON_SOURCES: &[Source] = &[
    Source::File {
        url: "https://huggingface.co/gradium/phonon-31m-en/resolve/92741c5b07d085ff840e26fea8ce43110b87852f/config.json",
        file: ModelFile {
            dest: "config.json",
            sha256: "b5d0f88353638370d94035b7d73d7d6577c0554d78f8d7ecfd40490aae39ae85",
            bytes: 4_634,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-31m-en/resolve/92741c5b07d085ff840e26fea8ce43110b87852f/tokenizer.json",
        file: ModelFile {
            dest: "tokenizer.json",
            sha256: "691a00068fa7b989f2f3e5ae5daa2d1a5bb319f2d5360deafcfb19a8bb6e7936",
            bytes: 277_967,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-31m-en/resolve/92741c5b07d085ff840e26fea8ce43110b87852f/model.q8.gguf",
        file: ModelFile {
            dest: "model.q8.gguf",
            sha256: "a7d9aad2cd6dc66473f24a08bc6d2939030ef4d751ef82349361fbef3272dbae",
            bytes: 64_512_352,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/4SZHfMpw-p46Ywgs.safetensors",
        file: ModelFile {
            dest: "voice-harper.safetensors",
            sha256: "6b3da6e2d796a20c51af892c482bd6509fb88267a221aff83c870843dee2a123",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/vjFyp2BcCyTlUmYb.safetensors",
        file: ModelFile {
            dest: "voice-everly.safetensors",
            sha256: "958ea94da14f46dd48c7f64adac11ed2ad2aa45f7ccd165dfebab5272664402e",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/bXwrkHhURL1VFJdG.safetensors",
        file: ModelFile {
            dest: "voice-hadley.safetensors",
            sha256: "a879938aa9a2df0a8220bdadef88137929f71e887bd91e7db205bd5fa11661d2",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/nGzMHMPAWGaJFHOo.safetensors",
        file: ModelFile {
            dest: "voice-delia.safetensors",
            sha256: "a0c3dcb91b2247eca56536be9ae8e8becc1c807b1136c5fe101552d6a155b24a",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/NIWIOv5vTi5eRMj8.safetensors",
        file: ModelFile {
            dest: "voice-juniper.safetensors",
            sha256: "444378eb06b7c492b86e8fc7d13e4db1a3c0cf3e68891a960bf6f0a41a13ef35",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/NbpkqMVS3CJeq2j8.safetensors",
        file: ModelFile {
            dest: "voice-zoey.safetensors",
            sha256: "97c7f8c253413574406ab80dd23f49c3ebb2517163b00a9bf86b9c4cf43e7e23",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/d9XCyhjo4JhkHF1d.safetensors",
        file: ModelFile {
            dest: "voice-miles.safetensors",
            sha256: "eab20e4f159b48cfbc01b1885804e24f681f80e7a64f909182763a4e3361bee8",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/qIxlZ7Ei8KvWlyQH.safetensors",
        file: ModelFile {
            dest: "voice-preston.safetensors",
            sha256: "6e04b84d3ae8751b22bca0a4f63936f9cb648f4f2182b2d0788071ea1c0b68a1",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/0zLApsZfVLLNaEGl.safetensors",
        file: ModelFile {
            dest: "voice-trevor.safetensors",
            sha256: "dc1341c8937af5534e6bbcc5c0fc5e7c10312229b59061228074f93683fe2261",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/LMDIYE0Oa377Qh6Z.safetensors",
        file: ModelFile {
            dest: "voice-easton.safetensors",
            sha256: "45c582673cb20edb04ddcca2b5a8865ddfb0a82599efadb269021e87d20fbdd4",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/40QrMVgAxFiUnP9f.safetensors",
        file: ModelFile {
            dest: "voice-micah.safetensors",
            sha256: "0de1098d1051686a138d068e83ff09deb5053328bc74ec9541a1fdb90bfc24da",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/_6Aslh2DxfmnRLmP.safetensors",
        file: ModelFile {
            dest: "voice-russell.safetensors",
            sha256: "beae9e4030efce0a935fcb0f807dc0784d598bcaa87ad4aacb047436b1eaa411",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/6PWnV0Nq4wu7RVBT.safetensors",
        file: ModelFile {
            dest: "voice-maeve.safetensors",
            sha256: "56494ee67d533ad227ff0dfd0b1c13ff5aa91f1ad6e596e66c2986ba3005e844",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/gfP44RlHwr2kEwk7.safetensors",
        file: ModelFile {
            dest: "voice-bryony.safetensors",
            sha256: "bcc207a73b7885e45b49ab633d3377a10158292e3a7cef9cd6764ad107f1cb9b",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/gDT1nz7Ie36ZhL-C.safetensors",
        file: ModelFile {
            dest: "voice-imogen.safetensors",
            sha256: "1980ab3c35fa98a25b5f3b5e643fd852e14ca8a12a4a26b06aaf1abf2a8c68b1",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/uem82D50GRv2Dwma.safetensors",
        file: ModelFile {
            dest: "voice-pippa.safetensors",
            sha256: "d41a8f01d6e33450f1ce062d332914da10beee99e585dab531750706f187c48a",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/m9MxpY6IUGXtWp76.safetensors",
        file: ModelFile {
            dest: "voice-lottie.safetensors",
            sha256: "a906ebf17e43fded12b3d6bd077abb2df33bc86eff51c1beeb82e5d416527db3",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/GgfEkEJtxZR7gnpy.safetensors",
        file: ModelFile {
            dest: "voice-freya.safetensors",
            sha256: "eb3cd47418349e618408c16bea7153988b3ad04b41a6033039dd474a9a4dcd23",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/y9bQqIDnyxwqK01t.safetensors",
        file: ModelFile {
            dest: "voice-elodie-rose.safetensors",
            sha256: "21eade9d83b2e66b9364e1b6cd999a3a6704c000a71f27f34e05ace5cdfd58bb",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/zUCxzrOrugTSis03.safetensors",
        file: ModelFile {
            dest: "voice-edwin.safetensors",
            sha256: "ff1d0735b8b45710462b1d1f846da040f970c6eb62c7c2f36dbfb8e5bde74f1e",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/YyKcxPG7iIgS3jsK.safetensors",
        file: ModelFile {
            dest: "voice-hugh.safetensors",
            sha256: "3169d74b47a54d1eb15e4c243f56393abe9fe90c7e1c308ecf96ee67618b152a",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/JOW0vchyswkQUMDS.safetensors",
        file: ModelFile {
            dest: "voice-spencer.safetensors",
            sha256: "cc9ac1107b0ffce282374a19beb7bfa95847724e6b7afbac991b4ae4277d5e6d",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/dME3IWyZBvmh1n1q.safetensors",
        file: ModelFile {
            dest: "voice-toby.safetensors",
            sha256: "09ab315c7f4b72b4d71e66cb756184cedfb1f14d222d096e115bf95bfe7b29dc",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/Zn7g6NxLqXlJb7Qi.safetensors",
        file: ModelFile {
            dest: "voice-teddy.safetensors",
            sha256: "baea0ea9506f3237a5ebfc61afb7c72dbd189c5603b199ef42fbb00dbda379e0",
            bytes: 256_136,
        },
    },
    Source::File {
        url: "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3/voices/X6NceqF285Gr04gb.safetensors",
        file: ModelFile {
            dest: "voice-alfie.safetensors",
            sha256: "4de75497fff3030168e8aa6dca23ffeb332b1bc8c25d0bf3507ceadac953ae9c",
            bytes: 256_136,
        },
    },
];

/// hexgrad's Kokoro 82M v1.0, as sherpa-onnx packages it for its own runtime:
/// the full-precision model (the int8 one in the same release is less than
/// half as quick on a CPU), the table of speaker embeddings and the phoneme
/// vocabulary. The archive's lexicons, its espeak-ng data and its Chinese
/// dictionaries are left behind: English text is turned into phonemes here,
/// by `loqui-g2p` -- see `speech::synth::kokoro`. Model and voices are
/// Apache-2.0. The archive's hash is GitHub's own digest for the release
/// asset; each member's was taken from the files it unpacked to.
const KOKORO_SOURCES: &[Source] = &[Source::Archive {
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/kokoro-multi-lang-v1_0.tar.bz2",
    sha256: "60703707bc05ccd48d60e1eb723f38b07d6e474795a0b6a588d7220400e0f7a4",
    bytes: 349_886_476,
    members: &[
        (
            "kokoro-multi-lang-v1_0/model.onnx",
            ModelFile {
                dest: "model.onnx",
                sha256: "b40f62b166ac8164b0627ef48a0b358eda0985e272fb03ef5252e7206305da11",
                bytes: 325_560_556,
            },
        ),
        (
            "kokoro-multi-lang-v1_0/voices.bin",
            ModelFile {
                dest: "voices.bin",
                sha256: "1c5a5b983d3d50d8586d437a51f3faa2da7919ce76a013c081e65671a3447c29",
                bytes: 28_200_960,
            },
        ),
        (
            "kokoro-multi-lang-v1_0/tokens.txt",
            ModelFile {
                dest: "tokens.txt",
                sha256: "6ebb6bb288f20f3ae8d004d3c2ca27697da27c037d75e81a60e2a6a663f95425",
                bytes: 687,
            },
        ),
    ],
}];

/// The stable-diffusion.cpp release the picture models are run with. Pinned
/// and hashed, like every other download here: a newer build is a deliberate
/// change, not something a card picks up on its own.
///
/// One release for every system, `master-1043-3be2378`: the newest whose
/// Linux build was made on Ubuntu 24.04. The builds after it are made on
/// 26.04 and need its glibc 2.43 -- they do not start on 24.04 -- and this
/// one needs glibc 2.38, so 24.04 and later. Nothing published for these
/// models starts on 22.04, which the app's own Linux bundle is built on; on
/// a system that old the engine says so when it fails to start (see
/// `images::local`), and `JATAYU_SD_CLI` can name one built there. The
/// macOS build is likewise for macOS 26 and later.
///
/// Chosen by processor as well as by system, since a build for another one
/// does not start at all. The macOS archive is universal -- arm64 and x86_64
/// in one binary, whatever its name says -- so it serves an Intel Mac
/// running the universal app too.
pub const SD_CLI_ID: &str = "sdCli";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SD_CLI_SOURCES: &[Source] = &[Source::File {
    url: "https://github.com/leejet/stable-diffusion.cpp/releases/download/master-1043-3be2378/sd-master-3be2378-bin-Linux-Ubuntu-24.04-x86_64.zip",
    file: ModelFile {
        dest: SD_CLI_ARCHIVE,
        sha256: "01e1c773c1bc2a6276952f6dbd13294e58b7255c659b4a770086934c4e8fad3a",
        bytes: 27_322_680,
    },
}];

#[cfg(target_os = "macos")]
const SD_CLI_SOURCES: &[Source] = &[Source::File {
    url: "https://github.com/leejet/stable-diffusion.cpp/releases/download/master-1043-3be2378/sd-master-3be2378-bin-Darwin-macOS-26.6.2-arm64.zip",
    file: ModelFile {
        dest: SD_CLI_ARCHIVE,
        sha256: "ec40c7b84d86333ea89a9a52745fce25c3081122f6207a4370f35b504df99252",
        bytes: 37_650_853,
    },
}];

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const SD_CLI_SOURCES: &[Source] = &[Source::File {
    url: "https://github.com/leejet/stable-diffusion.cpp/releases/download/master-1043-3be2378/sd-master-3be2378-bin-win-cpu-x64.zip",
    file: ModelFile {
        dest: SD_CLI_ARCHIVE,
        sha256: "10ef877a334c97d014e3e23b4bf6c9077f6b78ff9da3bb157664d2103cb1ae0f",
        bytes: 18_067_084,
    },
}];

/// Nothing is published for any other system or processor -- Linux or
/// Windows on ARM among them -- and [`start_download`] says so rather than
/// fetch a build that would not start.
#[cfg(not(any(
    all(target_os = "linux", target_arch = "x86_64"),
    target_os = "macos",
    all(target_os = "windows", target_arch = "x86_64"),
)))]
const SD_CLI_SOURCES: &[Source] = &[];

/// What the engine's archive is kept as, inside its directory.
const SD_CLI_ARCHIVE: &str = "sd-cli.zip";

/// FLUX.2 [klein] 4B at Q4_0, with the text encoder and VAE it was published
/// beside. The checksums are that repository's SHA256SUMS.
const FLUX2_KLEIN_Q4_SOURCES: &[Source] = &[
    Source::File {
        url: "https://huggingface.co/manishdashsharma/flux2-klein-4b-gguf/resolve/main/flux-2-klein-4b-q4_0.gguf",
        file: ModelFile {
            dest: "flux-2-klein-4b-q4_0.gguf",
            sha256: "9c5939d55e3ac76e3a700029770fcc5e1b873d12c2a6d22c6c3fec217488e718",
            bytes: 2_256_889_280,
        },
    },
    Source::File {
        url: "https://huggingface.co/manishdashsharma/flux2-klein-4b-gguf/resolve/main/Qwen3-4B-Q4_K_S.gguf",
        file: ModelFile {
            dest: "Qwen3-4B-Q4_K_S.gguf",
            sha256: "78cea0ccfd5fd7af5761f7dfdeebd2ec8f04328be0f1e6f70e2213a6f1309bc9",
            bytes: 2_383_310_112,
        },
    },
    Source::File {
        url: "https://huggingface.co/manishdashsharma/flux2-klein-4b-gguf/resolve/main/flux2-vae.safetensors",
        file: ModelFile {
            dest: "flux2-vae.safetensors",
            sha256: "868fe7b343cc8f3a19dbcfcafbc3d5f888802be3f89bd81b65b3621a066ce8f3",
            bytes: 336_211_292,
        },
    },
];

/// Z-Image Turbo at Q4_K, with its text encoder and the FLUX VAE.
const Z_IMAGE_TURBO_Q4_SOURCES: &[Source] = &[
    Source::File {
        url: "https://huggingface.co/leejet/Z-Image-Turbo-GGUF/resolve/main/z_image_turbo-Q4_K.gguf",
        file: ModelFile {
            dest: "z_image_turbo-Q4_K.gguf",
            sha256: "14b375ab4f226bc5378f68f37e899ef3c2242b8541e61e2bc1aff40976086fbd",
            bytes: 3_864_250_304,
        },
    },
    Source::File {
        url: "https://huggingface.co/unsloth/Qwen3-4B-Instruct-2507-GGUF/resolve/main/Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        file: ModelFile {
            dest: "Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
            sha256: "3605803b982cb64aead44f6c1b2ae36e3acdb41d8e46c8a94c6533bc4c67e597",
            bytes: 2_497_281_120,
        },
    },
    Source::File {
        url: "https://huggingface.co/Comfy-Org/Z-Image-Turbo/resolve/main/split_files/vae/ae.safetensors",
        file: ModelFile {
            dest: "ae.safetensors",
            sha256: "afc8e28272cd15db3919bacdb6918ce9c1ed22e96cb12c4d5ed0fba823529e38",
            bytes: 335_304_388,
        },
    },
];

/// What a decision model's one file is called inside its directory.
const DECISION_MODEL_FILE: &str = "model.gguf";

/// What a Qwen3-ASR recogniser's two files are called inside its
/// directory: the language model, and the audio encoder llama.cpp calls a
/// multimodal projector.
const QWEN_MODEL_FILE: &str = "model.gguf";
const QWEN_MMPROJ_FILE: &str = "mmproj.gguf";

/// The Qwen3-ASR recognisers, from llama.cpp's own conversions
/// (`ggml-org/Qwen3-ASR-*-GGUF` on Hugging Face) of Qwen's Apache-2.0
/// weights, at the revision each repository had on the day they were
/// pinned. The SHA-256 is Hugging Face's LFS object id, checked against a
/// download. Both halves are 8-bit, the smallest llama.cpp publishes; the
/// language model is where the size goes.
const QWEN3_ASR_17B_SOURCES: &[Source] = &[
    Source::File {
        url: "https://huggingface.co/ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/36a678687ba7d07a74ca70ccb0e36902e005fb80/Qwen3-ASR-1.7B-Q8_0.gguf",
        file: ModelFile {
            dest: QWEN_MODEL_FILE,
            sha256: "58e22d0532d4eacaf034cfac17a6fed159f37c41390c710186783be439d1fc57",
            bytes: 2_165_034_944,
        },
    },
    Source::File {
        url: "https://huggingface.co/ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/36a678687ba7d07a74ca70ccb0e36902e005fb80/mmproj-Qwen3-ASR-1.7B-Q8_0.gguf",
        file: ModelFile {
            dest: QWEN_MMPROJ_FILE,
            sha256: "46c1d533af3f354ceb37ce855dbceff7da7fa7cf1e6a523df3b13440bd164c0d",
            bytes: 355_709_344,
        },
    },
];

const QWEN3_ASR_06B_SOURCES: &[Source] = &[
    Source::File {
        url: "https://huggingface.co/ggml-org/Qwen3-ASR-0.6B-GGUF/resolve/928ab958557df9aa2ef1c93e0e83c7ad0933fae2/Qwen3-ASR-0.6B-Q8_0.gguf",
        file: ModelFile {
            dest: QWEN_MODEL_FILE,
            sha256: "bca259818b50ca7c4c05e9bdb35a5dc04fa039653a6d6f3f0f331f96f6aa1971",
            bytes: 804_749_248,
        },
    },
    Source::File {
        url: "https://huggingface.co/ggml-org/Qwen3-ASR-0.6B-GGUF/resolve/928ab958557df9aa2ef1c93e0e83c7ad0933fae2/mmproj-Qwen3-ASR-0.6B-Q8_0.gguf",
        file: ModelFile {
            dest: QWEN_MMPROJ_FILE,
            sha256: "41a342b5e4c514e968cb756de6cd1b7be39eff43c44c57a2ef5fc6522e36603d",
            bytes: 214_392_480,
        },
    },
];

/// VoxLock, int8, as sherpa-onnx republishes it from its author's release
/// (github.com/arbazsiddiqui/voxlock, Apache-2.0): the enrolment model, run
/// once on the person's voice, and the model run over every chunk of the
/// microphone. The archive's test WAVs and README are left in it.
const VOICE_ISOLATION_SOURCES: &[Source] = &[Source::Archive {
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speech-enhancement-models/sherpa-onnx-voxlock-int8.tar.bz2",
    sha256: "bcb947f9e8676d3e963f6048b0088505c84f0843bfdc88cb26dde3264d81570b",
    bytes: 72_508_647,
    members: &[
        (
            "sherpa-onnx-voxlock-int8/enroll.onnx",
            ModelFile {
                dest: "enroll.onnx",
                sha256: "5d06048c156bb8cd422d29848e6d38fb26a7b39f6f7fd92eccf2a6ed1ce003c6",
                bytes: 12_476_014,
            },
        ),
        (
            "sherpa-onnx-voxlock-int8/chunk_int8.onnx",
            ModelFile {
                dest: "chunk_int8.onnx",
                sha256: "df61a9d0062b0adbcf1182302b1eb8b815f15ac47ea4923e8e164c1aff64121d",
                bytes: 66_608_910,
            },
        ),
    ],
}];

/// DPDFNet's 16 kHz model with two dual-path blocks (Ceva, Apache-2.0), as
/// sherpa-onnx republishes it. `dpdfnet4` was no better on the one noisy
/// test clip, at twice the cost of a model that runs whenever a microphone
/// is open.
const NOISE_FILTER_SOURCES: &[Source] = &[Source::File {
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speech-enhancement-models/dpdfnet2.onnx",
    file: ModelFile {
        dest: "dpdfnet2.onnx",
        sha256: "ce35d6025fc71df0ef10d1540e1b7916837bbfe5f6896deb744508d2cad487a9",
        bytes: 10_249_356,
    },
}];

/// Every downloadable model, in the order the settings panes list them.
pub static CATALOGUE: [ModelSpec; 15] = [
    ModelSpec {
        id: SPEECH_KIT_ID,
        kind: ModelKind::Speech,
        name: "Speech kit",
        description: "Voice detection, speaker segmentation and speaker embeddings. Needed by every recogniser below, and downloaded once for all of them.",
        languages: "Any",
        sources: SPEECH_KIT_SOURCES,
    },
    ModelSpec {
        id: "parakeetV3",
        kind: ModelKind::Speech,
        name: "Parakeet TDT 0.6B v3",
        description: "Several times faster than real time on a laptop CPU.",
        languages: "English and the major European languages",
        sources: PARAKEET_V3_SOURCES,
    },
    ModelSpec {
        id: "whisperTurbo",
        kind: ModelKind::Speech,
        name: "Whisper large-v3-turbo",
        description: "Slower than Parakeet, and heavier, but understands far more languages.",
        languages: "Any language Whisper supports",
        sources: WHISPER_TURBO_SOURCES,
    },
    ModelSpec {
        id: "qwen3Asr17b",
        kind: ModelKind::Speech,
        name: "Qwen3-ASR 1.7B",
        description: "The most accurate here, best with accents such as Indian English. Quick on a graphics card, slower than Parakeet without one.",
        languages: "English, Chinese, Hindi and 27 other languages",
        sources: QWEN3_ASR_17B_SOURCES,
    },
    ModelSpec {
        id: "qwen3Asr06b",
        kind: ModelKind::Speech,
        name: "Qwen3-ASR 0.6B",
        description: "A smaller Qwen3-ASR: about a third of the size and quicker, and less accurate.",
        languages: "English, Chinese, Hindi and 27 other languages",
        sources: QWEN3_ASR_06B_SOURCES,
    },
    ModelSpec {
        id: VOICE_ISOLATION_ID,
        kind: ModelKind::Speech,
        name: "Voice isolation (VoxLock)",
        description: "Keeps your voice in the microphone and takes out other people talking over you.",
        languages: "Any",
        sources: VOICE_ISOLATION_SOURCES,
    },
    ModelSpec {
        id: NOISE_FILTER_ID,
        kind: ModelKind::Speech,
        name: "Noise filter (DPDFNet)",
        description: "Takes noise that comes and goes out of the microphone: a fan, dishes, a keyboard.",
        languages: "Any",
        sources: NOISE_FILTER_SOURCES,
    },
    ModelSpec {
        id: "d3Lite",
        kind: ModelKind::Decision,
        name: "d3-lite",
        description: "The smallest and quickest, and the least sure: in testing it put about one plain message in ten in the wrong category.",
        languages: "English, best",
        sources: D3_LITE_SOURCES,
    },
    ModelSpec {
        id: "d3Nano",
        kind: ModelKind::Decision,
        name: "d3-nano",
        description: "Recommended. As good as d3-mini in testing, in about two thirds of the time and memory.",
        languages: "English, best",
        sources: D3_NANO_SOURCES,
    },
    ModelSpec {
        id: "d3Mini",
        kind: ModelKind::Decision,
        name: "d3-mini",
        description: "The largest and most accurate, and the slowest: several seconds a message on most computers.",
        languages: "English, best",
        sources: D3_MINI_SOURCES,
    },
    ModelSpec {
        id: "phonon",
        kind: ModelKind::Voice,
        name: "Phonon 31M",
        description: "Twenty-five natural voices that run on this computer, many times quicker than they speak.",
        languages: "English",
        sources: PHONON_SOURCES,
    },
    ModelSpec {
        id: "kokoro",
        kind: ModelKind::Voice,
        name: "Kokoro 82M",
        description: "Twenty-eight voices that run on this computer, a few times quicker than they speak. A larger download than Phonon, and slower.",
        languages: "English",
        sources: KOKORO_SOURCES,
    },
    ModelSpec {
        id: SD_CLI_ID,
        kind: ModelKind::Image,
        name: "Image engine",
        description: "stable-diffusion.cpp, which runs the picture models on this computer.",
        languages: "—",
        sources: SD_CLI_SOURCES,
    },
    ModelSpec {
        id: "flux2KleinQ4",
        kind: ModelKind::Image,
        name: "FLUX.2 Klein",
        description: "Quantized FLUX.2 Klein 4B. Makes a picture in four steps.",
        languages: "Prompts in English, and the languages its text encoder knows",
        sources: FLUX2_KLEIN_Q4_SOURCES,
    },
    ModelSpec {
        id: "zImageTurboQ4",
        kind: ModelKind::Image,
        name: "Z-Image Turbo",
        description: "Quantized Z-Image Turbo. Makes a picture in eight steps.",
        languages: "English and Chinese",
        sources: Z_IMAGE_TURBO_Q4_SOURCES,
    },
];

pub fn catalogue() -> &'static [ModelSpec] {
    &CATALOGUE
}

pub fn spec(id: &str) -> Option<&'static ModelSpec> {
    CATALOGUE.iter().find(|s| s.id == id)
}

/// The models of one kind, in catalogue order.
pub fn catalogue_of(kind: ModelKind) -> impl Iterator<Item = &'static ModelSpec> {
    CATALOGUE.iter().filter(move |s| s.kind == kind)
}

/// This decision model's file, downloaded and checked.
pub fn decision_installed(model: LocalDecisionModel) -> bool {
    spec(model.id()).is_some_and(is_installed)
}

/// Where this decision model's GGUF is, once [`decision_installed`].
pub fn decision_model_path(model: LocalDecisionModel) -> Option<PathBuf> {
    let spec = spec(model.id())?;
    is_installed(spec).then(|| spec.dir().join(DECISION_MODEL_FILE))
}

/// The VAD, segmentation and embedding models, all present and checked.
pub fn speech_kit_installed() -> bool {
    spec(SPEECH_KIT_ID).is_some_and(is_installed)
}

/// VoxLock's two files, present and checked.
pub fn voice_isolation_installed() -> bool {
    spec(VOICE_ISOLATION_ID).is_some_and(is_installed)
}

/// DPDFNet's file, present and checked.
pub fn noise_filter_installed() -> bool {
    spec(NOISE_FILTER_ID).is_some_and(is_installed)
}

/// Where VoxLock's files are, once [`voice_isolation_installed`].
pub struct VoiceIsolationPaths {
    pub enroll: PathBuf,
    pub chunk: PathBuf,
}

pub fn voice_isolation_paths() -> Option<VoiceIsolationPaths> {
    let spec = spec(VOICE_ISOLATION_ID)?;
    is_installed(spec).then(|| {
        let dir = spec.dir();
        VoiceIsolationPaths { enroll: dir.join("enroll.onnx"), chunk: dir.join("chunk_int8.onnx") }
    })
}

/// Where DPDFNet is, once [`noise_filter_installed`].
pub fn noise_filter_path() -> Option<PathBuf> {
    let spec = spec(NOISE_FILTER_ID)?;
    is_installed(spec).then(|| spec.dir().join("dpdfnet2.onnx"))
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

/// Files their publisher has since replaced in place, by `dest` and the
/// size of the copy published before: a copy downloaded and verified then
/// is still a good copy, and is still installed. Without this, sherpa-onnx
/// re-publishing the speaker-embedding model one byte larger (Nov 2026)
/// made every speech kit downloaded before it "not installed" -- and with
/// it meeting notes, voice typing and voice mode, all refused until the kit
/// was fetched again.
const EARLIER_RELEASES: &[(&str, u64)] = &[("speaker-embedding.onnx", 26_485_263)];

/// Whether a file of `bytes` is `file` as published now, or as published
/// before (see [`EARLIER_RELEASES`]).
fn published_size(file: &ModelFile, bytes: u64) -> bool {
    bytes == file.bytes || EARLIER_RELEASES.iter().any(|&(dest, b)| dest == file.dest && b == bytes)
}

fn is_installed(spec: &ModelSpec) -> bool {
    installed_in(spec, &spec.dir())
}

/// [`is_installed`] for the model's files in `dir`: the marker the download
/// left lists every file, each at a size its publisher put out, and each
/// file on disk is the size the marker recorded.
fn installed_in(spec: &ModelSpec, dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(marker_path(dir)) else { return false };
    let Ok(marker) = serde_json::from_str::<Marker>(&text) else { return false };
    let expected = spec.files();
    if marker.files.len() != expected.len() {
        return false;
    }
    expected.iter().all(|file| {
        marker.files.iter().any(|(dest, bytes)| {
            dest == file.dest
                && published_size(file, *bytes)
                && std::fs::metadata(dir.join(file.dest)).is_ok_and(|m| m.len() == *bytes)
        })
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
/// last error if the last one failed -- everything the `speech_models` and
/// `decision_models` commands read to build one `LocalModelInfo`. `None`
/// when `id` is not in the catalogue.
pub struct Status {
    pub installed: bool,
    pub progress: Option<(u64, u64)>,
    pub error: Option<String>,
}

pub fn status(id: &str) -> Option<Status> {
    let spec = spec(id)?;
    Some(Status { installed: is_installed(spec), progress: progress(id), error: last_error(id) })
}

/// Check whether `id` already has a download running and, if not, register
/// a fresh [`Download`] for it and return that -- one lock acquisition, not
/// two, which is what makes this the atomic "start unless already started"
/// [`start_download`] needs. Two lock acquisitions -- check under one,
/// insert under a second -- would let two overlapping callers both see
/// nothing running and both register their own `Download`, so two downloads
/// of the same model run at once, each streaming into the same `.partial`
/// files. `None` means a download for `id` is already running and the
/// caller has nothing more to do.
fn register_download(id: &'static str, total: u64) -> Option<Arc<Download>> {
    let mut map = registry().lock().unwrap();
    if map.get(id).is_some_and(|dl| dl.running.load(Ordering::SeqCst)) {
        return None;
    }
    let dl = Arc::new(Download {
        total,
        done: AtomicU64::new(0),
        cancelled: AtomicBool::new(false),
        running: AtomicBool::new(true),
        error: Mutex::new(None),
    });
    map.insert(id, dl.clone());
    Some(dl)
}

/// Start `id` downloading in the background. A no-op if it is already
/// installed, or already downloading.
pub fn start_download(id: &str) -> Result<(), String> {
    let spec = spec(id).ok_or_else(|| format!("there is no such model: {id}"))?;
    if spec.sources.is_empty() {
        return Err(format!("{} is not published for this kind of computer", spec.name));
    }
    if is_installed(spec) {
        return Ok(());
    }
    let Some(dl) = register_download(spec.id, spec.bytes()) else {
        return Ok(());
    };

    tokio::spawn(async move {
        let outcome = run_download(spec, dl.clone()).await;
        dl.running.store(false, Ordering::SeqCst);
        match outcome {
            Ok(()) => {
                registry().lock().unwrap().remove(spec.id);
                forget_retired(spec.id).await;
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

/// Models this application no longer offers, by their old catalogue id, and
/// the one that replaced each. Nothing lists a retired model any more, so
/// nothing in settings could delete its folder; it goes once its replacement
/// has downloaded, and not before, so that a failed download leaves nobody
/// with neither.
const RETIRED: &[(&str, &str)] = &[("supertonic3", "phonon")];

async fn forget_retired(installed: &str) {
    for (retired, by) in RETIRED {
        if *by != installed {
            continue;
        }
        let dir = models_dir().join(retired);
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => tracing::info!(model = retired, "removed a model no longer offered"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(model = retired, error = %e, "could not remove a retired model")
            }
        }
    }
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
///
/// The model's own files go first and the marker last, so a file that will
/// not go -- one still open on Windows -- leaves the model installed and
/// whole, rather than a marker gone and gigabytes left behind it.
pub fn delete(id: &str) -> Result<(), String> {
    let spec = spec(id).ok_or_else(|| format!("there is no such model: {id}"))?;
    cancel_download(id);
    let dir = spec.dir();
    if !dir.exists() {
        return Ok(());
    }
    for file in spec.files() {
        let path = dir.join(file.dest);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("could not remove {}: {e}", path.display())),
        }
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("could not remove {}: {e}", dir.display()))
}

#[derive(Debug)]
enum Outcome {
    Cancelled,
    Failed(String),
}

async fn run_download(spec: &'static ModelSpec, dl: Arc<Download>) -> Result<(), Outcome> {
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
                stream_download(url, &partial, file.bytes, file.sha256, &dl, base_offset).await?;
                std::fs::rename(&partial, &dest)
                    .map_err(|e| Outcome::Failed(format!("could not finish {}: {e}", file.dest)))?;
                installed_files.push((file.dest.to_string(), file.bytes));
                base_offset += file.bytes;
            }
            Source::Archive { url, sha256, bytes, members } => {
                let archive_partial = dir.join(".partial-archive");
                stream_download(url, &archive_partial, *bytes, sha256, &dl, base_offset).await?;
                base_offset += bytes;
                // Whatever `extract_members` answers, the archive itself is
                // no longer wanted -- including on `Cancelled` or `Failed`,
                // when it would otherwise sit in the model's directory as
                // an orphaned partial output, up to hundreds of megabytes,
                // until the next successful install overwrites it.
                let extracted = extract_members(&archive_partial, &dir, members, dl.clone()).await;
                let _ = std::fs::remove_file(&archive_partial);
                extracted?;
                for (_, file) in *members {
                    installed_files.push((file.dest.to_string(), file.bytes));
                }
            }
        }
    }

    finalize_install(&dir, &dl, installed_files)
}

/// Commit the install: write the marker recording every file this model now
/// has, so future calls skip straight to [`is_installed`]'s cheap `stat`.
/// Checked here, not only at the top of each source's pass through
/// [`run_download`]'s own loop, because a cancel can land after every byte
/// has already arrived and every checksum has already passed -- the last
/// gap left once [`extract_members`] started checking between members too.
/// Without this, that race would silently finish installing a model the
/// person had just told this module to stop fetching.
fn finalize_install(
    dir: &Path,
    dl: &Download,
    installed_files: Vec<(String, u64)>,
) -> Result<(), Outcome> {
    if dl.cancelled.load(Ordering::SeqCst) {
        return Err(Outcome::Cancelled);
    }
    let marker = Marker { files: installed_files };
    let text = serde_json::to_string(&marker)
        .map_err(|e| Outcome::Failed(format!("could not record the install: {e}")))?;
    std::fs::write(marker_path(dir), text)
        .map_err(|e| Outcome::Failed(format!("could not record the install: {e}")))?;
    Ok(())
}

fn spec_sources(spec: &'static ModelSpec) -> &'static [Source] {
    spec.sources
}

/// Longest a model download may go without receiving a byte. Not a limit
/// on the whole download: a decision model is several gigabytes, and on a
/// slow link that is still a download somebody asked for, not one that
/// should be timed out from under them the way an ordinary fetch in
/// `crate::http` is. A server that has stopped sending is caught by this
/// instead.
const STALL_TIMEOUT: Duration = Duration::from_secs(2 * 60);
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
                .read_timeout(STALL_TIMEOUT)
                .user_agent(crate::http::USER_AGENT)
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
    let digest = jatayu_core::hex::encode(&hasher.finalize());
    if !digest.eq_ignore_ascii_case(expected_sha256) {
        let _ = tokio::fs::remove_file(dest).await;
        return Err(Outcome::Failed("download failed: the checksum did not match".into()));
    }
    Ok(())
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
///
/// # Cancellation
///
/// `dl` is checked between every entry the tar reader hands back --
/// including the ones this call skips -- so a cancel lands within one
/// member's worth of latency rather than waiting for the whole archive
/// (the biggest of these, `whisperTurbo`'s encoder, is 674 MB on its own).
/// Because the check runs *before* a member's own `.partial` file is
/// created, a cancel never has to clean one up: extraction has either not
/// started writing the next member yet, or it already finished the one it
/// was on -- there is no half-written file in between for it to leave.
async fn extract_members(
    archive: &Path,
    dir: &Path,
    members: &'static [(&'static str, ModelFile)],
    dl: Arc<Download>,
) -> Result<(), Outcome> {
    let archive = archive.to_path_buf();
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || extract_members_blocking(&archive, &dir, members, &dl))
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
    dl: &Download,
) -> Result<(), Outcome> {
    let file = std::fs::File::open(archive)
        .map_err(|e| Outcome::Failed(format!("could not open {}: {e}", archive.display())))?;
    let mut tar = tar::Archive::new(bzip2::read::BzDecoder::new(file));
    let entries =
        tar.entries().map_err(|e| Outcome::Failed(format!("could not read the archive: {e}")))?;

    let mut remaining: std::collections::HashSet<&str> =
        members.iter().map(|(path, _)| *path).collect();
    for entry in entries {
        if dl.cancelled.load(Ordering::SeqCst) {
            return Err(Outcome::Cancelled);
        }
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

        let digest = jatayu_core::hex::encode(&hasher.finalize());
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
// Paths, for `speech::local` (feature `speech`) to load models from.
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

/// This voice's files, all present and checked.
pub fn voice_installed(voice: LocalVoice) -> bool {
    spec(voice.id()).is_some_and(is_installed)
}

/// Where Phonon's files are, once [`voice_installed`]: what `ptts` is
/// handed to load it, and each speaker's embedding by its
/// [`PHONON_SPEAKERS`](jatayu_core::speech::PHONON_SPEAKERS) name.
pub struct PhononPaths {
    pub config: PathBuf,
    pub tokenizer: PathBuf,
    pub weights: PathBuf,
    /// In speaker order: `(name, file)`.
    pub speakers: Vec<(&'static str, PathBuf)>,
}

pub fn phonon_paths() -> Option<PhononPaths> {
    let dir = installed_voice_dir(LocalVoice::Phonon)?;
    Some(PhononPaths {
        config: dir.join("config.json"),
        tokenizer: dir.join("tokenizer.json"),
        weights: dir.join("model.q8.gguf"),
        speakers: jatayu_core::speech::PHONON_SPEAKERS
            .iter()
            .map(|name| (*name, dir.join(phonon_speaker_file(name))))
            .collect(),
    })
}

/// Where Kokoro's files are, once [`voice_installed`]: what sherpa-onnx is
/// handed to load it.
pub struct KokoroPaths {
    pub model: PathBuf,
    pub voices: PathBuf,
    pub tokens: PathBuf,
}

pub fn kokoro_paths() -> Option<KokoroPaths> {
    let dir = installed_voice_dir(LocalVoice::Kokoro)?;
    Some(KokoroPaths {
        model: dir.join("model.onnx"),
        voices: dir.join("voices.bin"),
        tokens: dir.join("tokens.txt"),
    })
}

fn installed_voice_dir(voice: LocalVoice) -> Option<PathBuf> {
    let spec = spec(voice.id())?;
    is_installed(spec).then(|| spec.dir())
}

/// The file a Phonon speaker's embedding is kept under.
fn phonon_speaker_file(name: &str) -> String {
    format!("voice-{}.safetensors", name.to_lowercase())
}

/// The quantized weights of a picture model, once it is installed.
pub struct ImageWeightPaths {
    pub diffusion: PathBuf,
    pub text_encoder: PathBuf,
    pub vae: PathBuf,
}

pub fn image_paths(model: LocalImageModel) -> Option<ImageWeightPaths> {
    let spec = spec(model.id())?;
    if !is_installed(spec) {
        return None;
    }
    let dir = spec.dir();
    Some(ImageWeightPaths {
        diffusion: dir.join(model.diffusion_file()),
        text_encoder: dir.join(model.text_encoder_file()),
        vae: dir.join(model.vae_file()),
    })
}

/// The downloaded stable-diffusion.cpp archive, once it is installed.
pub struct EngineArchive {
    pub path: PathBuf,
    /// What it was checked against, and so which release it is.
    pub sha256: &'static str,
}

pub fn sd_cli_archive() -> Option<EngineArchive> {
    let spec = spec(SD_CLI_ID)?;
    if !is_installed(spec) {
        return None;
    }
    let file = spec.files().into_iter().find(|f| f.dest == SD_CLI_ARCHIVE)?;
    Some(EngineArchive { path: spec.dir().join(SD_CLI_ARCHIVE), sha256: file.sha256 })
}

/// Where a recogniser's files are, once [`installed`]. The local
/// recognisers are different model families -- a transducer, Whisper, and
/// an audio language model -- so they are pointed at differently; see
/// `speech::local::Recogniser`.
pub enum RecogniserPaths {
    Transducer {
        encoder: PathBuf,
        decoder: PathBuf,
        joiner: PathBuf,
        tokens: PathBuf,
    },
    Whisper {
        encoder: PathBuf,
        decoder: PathBuf,
        tokens: PathBuf,
    },
    /// A GGUF language model and the audio encoder that feeds it, for
    /// llama.cpp.
    Llama {
        model: PathBuf,
        mmproj: PathBuf,
    },
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
        LocalModel::Qwen3Asr17b | LocalModel::Qwen3Asr06b => RecogniserPaths::Llama {
            model: dir.join(QWEN_MODEL_FILE),
            mmproj: dir.join(QWEN_MMPROJ_FILE),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug `register_download` exists to close: `start_download` used to
    /// check "is one already running?" under one lock acquisition and
    /// insert the fresh `Download` under a second, separate one. Two
    /// threads racing that gap could both see nothing running and both
    /// register their own `Download` for the same model id, so two
    /// downloads of it would run at once. Many real threads, lined up on a
    /// `Barrier` so they all call in at effectively the same instant, is
    /// what actually exercises that interleaving -- a sequential "call it
    /// twice" test would pass whether or not the race was closed, since
    /// there would be no genuine concurrency for it to catch.
    ///
    /// Run across several distinct ids, each with its own set of threads,
    /// since one round passing is not proof the race is closed -- only that
    /// this particular scheduling happened not to hit it.
    #[test]
    fn register_download_lets_only_one_caller_through_per_id_under_real_contention() {
        const THREADS: usize = 16;
        for round in 0..20 {
            let id: &'static str =
                Box::leak(format!("__test_race_model_{round}__").into_boxed_str());
            let barrier = Arc::new(std::sync::Barrier::new(THREADS));
            let handles: Vec<_> = (0..THREADS)
                .map(|_| {
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        register_download(id, 100).is_some()
                    })
                })
                .collect();
            let winners = handles.into_iter().map(|h| h.join().unwrap()).filter(|&won| won).count();
            assert_eq!(winners, 1, "round {round}: exactly one caller should have registered {id}");
        }
    }

    #[test]
    fn every_catalogue_id_is_unique_and_matches_local_model() {
        let mut seen = std::collections::HashSet::new();
        for spec in catalogue() {
            assert!(seen.insert(spec.id), "{} is declared twice", spec.id);
        }
        assert_eq!(local_model_id(LocalModel::ParakeetV3), "parakeetV3");
        assert_eq!(local_model_id(LocalModel::WhisperTurbo), "whisperTurbo");
        for model in LocalModel::ALL {
            assert!(
                spec(local_model_id(model)).is_some(),
                "{} has no catalogue entry",
                model.as_str()
            );
        }
    }

    #[test]
    fn every_local_decision_model_has_a_catalogue_entry_of_its_kind() {
        for model in LocalDecisionModel::ALL {
            let spec = spec(model.id()).unwrap_or_else(|| panic!("{} is missing", model.id()));
            assert_eq!(spec.kind, ModelKind::Decision);
            assert_eq!(spec.name, model.name());
        }
        assert_eq!(catalogue_of(ModelKind::Decision).count(), LocalDecisionModel::ALL.len());
        assert!(catalogue_of(ModelKind::Speech).all(|s| spec(s.id).is_some()));
    }

    #[test]
    fn the_local_voice_downloads_a_file_for_every_speaker_it_offers() {
        let spec = spec(LocalVoice::Phonon.id()).expect("the local voice is in the catalogue");
        assert_eq!(spec.kind, ModelKind::Voice);
        let dests: Vec<&str> = spec.files().iter().map(|f| f.dest).collect();
        for name in jatayu_core::speech::PHONON_SPEAKERS {
            let file = phonon_speaker_file(name);
            assert!(dests.contains(&file.as_str()), "{name} has no {file} to download");
        }
        for wanted in ["config.json", "tokenizer.json", "model.q8.gguf"] {
            assert!(dests.contains(&wanted), "the voice is missing {wanted}");
        }
        assert_eq!(dests.len(), 3 + jatayu_core::speech::PHONON_SPEAKERS.len());
    }

    #[test]
    fn every_local_voice_has_a_catalogue_entry_of_its_kind() {
        for voice in [LocalVoice::Phonon, LocalVoice::Kokoro] {
            let spec = spec(voice.id()).unwrap_or_else(|| panic!("{} is missing", voice.id()));
            assert_eq!(spec.kind, ModelKind::Voice);
        }
        assert_eq!(catalogue_of(ModelKind::Voice).count(), 2);
        let spec = spec(LocalVoice::Kokoro.id()).unwrap();
        let dests: Vec<&str> = spec.files().iter().map(|f| f.dest).collect();
        assert_eq!(dests, ["model.onnx", "voices.bin", "tokens.txt"]);
    }

    #[test]
    fn every_local_image_model_downloads_the_files_it_is_run_with() {
        for model in LocalImageModel::ALL {
            let spec = spec(model.id()).unwrap_or_else(|| panic!("{} is missing", model.id()));
            assert_eq!(spec.kind, ModelKind::Image);
            assert_eq!(spec.name, model.name());
            let dests: Vec<&str> = spec.files().iter().map(|f| f.dest).collect();
            for file in [model.diffusion_file(), model.text_encoder_file(), model.vae_file()] {
                assert!(dests.contains(&file), "{} does not download {file}", model.id());
            }
        }
        // The models, and the engine that runs them.
        assert_eq!(catalogue_of(ModelKind::Image).count(), LocalImageModel::ALL.len() + 1);
        assert_eq!(spec(SD_CLI_ID).map(|s| s.kind), Some(ModelKind::Image));
    }

    #[test]
    fn a_retired_model_is_not_offered_and_its_replacement_is() {
        for (retired, by) in RETIRED {
            assert!(spec(retired).is_none(), "{retired} is retired but still offered");
            assert!(spec(by).is_some(), "{retired}'s replacement {by} is not offered");
        }
    }

    #[test]
    fn every_source_names_at_least_one_file_and_a_lowercase_hash() {
        for spec in catalogue() {
            if spec.sources.is_empty() {
                // Only the engine, and only where none is published.
                assert_eq!(spec.id, SD_CLI_ID, "{} has nothing to download", spec.id);
                continue;
            }
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
            kind: ModelKind::Speech,
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
        installed_in(spec, dir)
    }

    #[test]
    fn a_copy_of_a_file_since_republished_is_still_installed() {
        let dir = tempfile::tempdir().unwrap();
        let spec = ModelSpec {
            id: "test-kit",
            kind: ModelKind::Speech,
            name: "Test",
            description: "",
            languages: "",
            sources: &[Source::File {
                url: "https://example.invalid/speaker-embedding.onnx",
                file: ModelFile { dest: "speaker-embedding.onnx", sha256: "0", bytes: 26_485_264 },
            }],
        };
        let old = 26_485_263u64;
        let file = std::fs::File::create(dir.path().join("speaker-embedding.onnx")).unwrap();
        file.set_len(old).unwrap();
        let marker = Marker { files: vec![("speaker-embedding.onnx".to_string(), old)] };
        std::fs::write(marker_path(dir.path()), serde_json::to_string(&marker).unwrap()).unwrap();
        assert!(is_installed_at(&spec, dir.path()), "the copy published before still counts");

        // Any other size is still not installed, whatever the marker says.
        file.set_len(old - 1).unwrap();
        assert!(!is_installed_at(&spec, dir.path()));
        let marker = Marker { files: vec![("speaker-embedding.onnx".to_string(), old - 1)] };
        std::fs::write(marker_path(dir.path()), serde_json::to_string(&marker).unwrap()).unwrap();
        assert!(!is_installed_at(&spec, dir.path()));
    }

    /// A [`Download`] in the shape [`extract_members_blocking`] and
    /// [`finalize_install`] expect: not cancelled, with whatever `total` the
    /// test cares about (usually nothing, since these tests check
    /// cancellation, not progress).
    fn fresh_download() -> Download {
        Download {
            total: 0,
            done: AtomicU64::new(0),
            cancelled: AtomicBool::new(false),
            running: AtomicBool::new(true),
            error: Mutex::new(None),
        }
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

        extract_members_blocking(&archive_path, dir.path(), members, &fresh_download()).unwrap();

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

        let err = extract_members_blocking(&archive_path, dir.path(), members, &fresh_download())
            .unwrap_err();
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

        let err = extract_members_blocking(&archive_path, dir.path(), members, &fresh_download())
            .unwrap_err();
        assert!(matches!(err, Outcome::Failed(_)));
        assert!(!dir.path().join("wanted.bin").exists());
    }

    /// The bug this exists to catch: without a cancellation check inside
    /// `extract_members_blocking`'s own loop, a cancel set before extraction
    /// starts would still be ignored until the *whole* archive had been
    /// pulled apart -- for a real model, up to a further 674 MB of work
    /// after the person asked this to stop. Setting `cancelled` before
    /// calling in proves the check is live, not merely present in
    /// `run_download`'s outer, once-per-source loop.
    #[test]
    fn extraction_stops_between_members_once_cancelled() {
        let members: &'static [(&'static str, ModelFile)] = leak_members(&[
            (
                "inner/first.bin",
                ModelFile { dest: "first.bin", sha256: sha256_hex(b"one"), bytes: 3 },
            ),
            (
                "inner/second.bin",
                ModelFile { dest: "second.bin", sha256: sha256_hex(b"two"), bytes: 3 },
            ),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("archive.tar.bz2");
        write_test_archive(
            &archive_path,
            &[("inner/first.bin", b"one".as_slice()), ("inner/second.bin", b"two".as_slice())],
        );

        let dl = fresh_download();
        dl.cancelled.store(true, Ordering::SeqCst);
        let err = extract_members_blocking(&archive_path, dir.path(), members, &dl).unwrap_err();

        assert!(matches!(err, Outcome::Cancelled));
        assert!(!dir.path().join("first.bin").exists(), "nothing should have been extracted");
        assert!(!dir.path().join("second.bin").exists());
    }

    #[test]
    fn finalize_install_refuses_to_write_the_marker_once_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let dl = fresh_download();
        dl.cancelled.store(true, Ordering::SeqCst);

        let err = finalize_install(dir.path(), &dl, vec![("one.bin".to_string(), 3)]).unwrap_err();

        assert!(matches!(err, Outcome::Cancelled));
        assert!(!marker_path(dir.path()).exists(), "a cancelled install must not look installed");
    }

    #[test]
    fn finalize_install_writes_the_marker_when_not_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let dl = fresh_download();

        finalize_install(dir.path(), &dl, vec![("one.bin".to_string(), 3)]).unwrap();

        assert!(marker_path(dir.path()).exists());
    }

    /// Leaked rather than owned: [`ModelFile::sha256`] is `&'static str`,
    /// which a real [`ModelSpec`] gets for free from being a `const`, and a
    /// test-only one has to fake by leaking -- fine for a test process.
    fn sha256_hex(bytes: &[u8]) -> &'static str {
        Box::leak(jatayu_core::hex::encode(&Sha256::digest(bytes)).into_boxed_str())
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
            jatayu_core::hex::encode(&Sha256::digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
