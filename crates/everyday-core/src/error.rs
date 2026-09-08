use std::path::PathBuf;

/// Every fallible operation in the Every Day core returns this error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("vault is locked")]
    Locked,

    #[error("incorrect password")]
    BadPassword,

    #[error("vault at {0} is already initialised")]
    AlreadyInitialised(PathBuf),

    #[error("no vault found at {0}")]
    NoVault(PathBuf),

    #[error("unsupported vault format version {found} (this build understands up to {supported})")]
    UnsupportedVaultVersion { found: u32, supported: u32 },

    #[error("unknown storage backend {0:?}")]
    UnknownBackend(String),

    #[error("unknown cipher suite {0:?}")]
    UnknownCipher(String),

    #[error("{kind} {id} not found")]
    NotFound { kind: &'static str, id: String },

    #[error("decryption failed: the data is corrupt or the wrong key was used")]
    Decrypt,

    #[error("this backend does not support {0}")]
    Unsupported(&'static str),

    #[error("this vault is open for writing by {holder}; this copy is read-only")]
    VaultInUse { holder: String },

    #[error("{kind} was changed elsewhere since you loaded it")]
    Conflict { kind: &'static str },

    #[error("invalid data: {0}")]
    Invalid(String),

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    RawIo(#[from] std::io::Error),

    #[error("serialisation error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("storage backend error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl Error {
    /// Attach a path to an [`std::io::Error`] for a much friendlier message.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io { path: path.into(), source }
    }

    pub fn not_found(kind: &'static str, id: impl std::fmt::Display) -> Self {
        Error::NotFound { kind, id: id.to_string() }
    }

    pub fn backend<E>(e: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Error::Backend(Box::new(e))
    }

    /// Stable machine-readable code, surfaced to the UI so it can react to
    /// e.g. `locked` without string-matching human prose.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Locked => "locked",
            Error::BadPassword => "bad_password",
            Error::AlreadyInitialised(_) => "already_initialised",
            Error::NoVault(_) => "no_vault",
            Error::UnsupportedVaultVersion { .. } => "unsupported_version",
            Error::UnknownBackend(_) => "unknown_backend",
            Error::UnknownCipher(_) => "unknown_cipher",
            Error::NotFound { .. } => "not_found",
            Error::Decrypt => "decrypt_failed",
            Error::Unsupported(_) => "unsupported",
            Error::VaultInUse { .. } => "vault_in_use",
            Error::Conflict { .. } => "conflict",
            Error::Invalid(_) => "invalid",
            Error::Io { .. } | Error::RawIo(_) => "io",
            Error::Serde(_) => "serde",
            Error::Backend(_) => "backend",
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
