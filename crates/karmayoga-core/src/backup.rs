//! Backups: where copies of the vault go, how often one is made, how many
//! are kept -- and the key every copy is sealed under.
//!
//! The copying itself is `karmayoga_service::backup`'s. What is here is what
//! the vault stores and what both ends of a backup have to agree on: the
//! settings and the last run's outcome (two sealed halves of one singleton,
//! so a run writing its status can never clobber a settings save), and
//! [`BackupKeys`], which a running vault and a restore working from nothing
//! but a header and a password both derive the same way.
//!
//! # A copy is a snapshot, not an archive
//!
//! A vault is tens of gigabytes once mail is in it, and a fresh copy of all
//! of it every day, kept seven times over, is seven times that in a bucket
//! and the whole of it up a home connection every night. So a destination
//! holds a *repository*: every file cut into chunks, each chunk sealed and
//! named by a keyed hash of what is in it, and one sealed, date-stamped
//! manifest per backup listing which chunks make up which file. Each
//! manifest is a whole copy -- restoring one needs nothing from any other --
//! and two copies share every chunk that did not change between them. That
//! is what makes "keep seven" cost about one vault plus a week of changes.
//!
//! # The key is the vault's
//!
//! Chunks and manifests are sealed under subkeys of the vault's own data key
//! (see [`BackupKeys::from_cipher`]), so a backup opens with the vault's
//! password and there is no second secret to lose. The header -- the salt,
//! the KDF parameters and the data key wrapped under the password -- is kept
//! beside the chunks in the clear, exactly as `vault.json` is kept beside a
//! vault: it is what turns the password into the key, and it is worth
//! nothing without the password. Only the latest header is kept, so a
//! password changed since opens every copy and the old one opens none.
//!
//! An unencrypted vault has no key to derive from, and its backups are
//! sealed under a key derived from nothing secret -- as private as the vault
//! itself, which is to say not at all. Settings says so.
//!
//! Names are keyed, never plain hashes: an unkeyed content hash in a bucket
//! listing would let anybody holding a copy of a well-known file check
//! whether you have it too.

use crate::crypto::{AeadCipher, Cipher, SecretKey};
use crate::error::{Error, Result};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// How often a backup may be asked for, in hours. Settings offers exactly
/// these; anything else is refused rather than rounded.
pub const FREQUENCIES: [u32; 4] = [1, 6, 24, 168];

/// The most copies a destination is asked to keep.
pub const MAX_KEEP: u32 = 100;

/// Owner kind the access keys for an S3 destination are sealed under in the
/// per-record secret store, when they were pasted in rather than read from
/// this computer -- see [`S3Credentials::Stored`]. One per vault.
pub const BACKUP_SECRET_OWNER_KIND: &str = "backup";
pub const BACKUP_SECRET_OWNER_ID: &str = "s3";

/// Everything Settings → Backup decides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct BackupSettings {
    /// Whether backups are made on their own. "Back up now" works either
    /// way, once there is somewhere to put one.
    pub enabled: bool,
    /// Where copies go. `None` until somebody has chosen.
    pub destination: Option<Destination>,
    /// Hours between backups: one of [`FREQUENCIES`].
    pub every_hours: u32,
    /// How many copies the destination keeps, newest first: `1..=MAX_KEEP`.
    pub keep: u32,
    /// Also copy this computer's own settings -- the folder beside the
    /// vault that holds the paired devices, the server's certificate, the
    /// MCP token. On by default: they are what makes a restored machine the
    /// same machine, and they travel sealed like everything else.
    pub include_config: bool,
}

impl Default for BackupSettings {
    fn default() -> Self {
        Self { enabled: false, destination: None, every_hours: 24, keep: 7, include_config: true }
    }
}

impl BackupSettings {
    pub fn validate(&self) -> Result<()> {
        if !FREQUENCIES.contains(&self.every_hours) {
            return Err(Error::Invalid(format!(
                "a backup can be made every {} hours; {} is not one of them",
                FREQUENCIES.map(|h| h.to_string()).join(", "),
                self.every_hours
            )));
        }
        if !(1..=MAX_KEEP).contains(&self.keep) {
            return Err(Error::Invalid(format!("keep between 1 and {MAX_KEEP} copies")));
        }
        if let Some(d) = &self.destination {
            d.validate()?;
        }
        if self.enabled && self.destination.is_none() {
            return Err(Error::Invalid("choose where backups go before turning them on".into()));
        }
        Ok(())
    }
}

/// Where a backup goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Destination {
    /// A folder on this computer: another disk, a mounted share, a synced
    /// folder.
    Folder { path: String },
    /// An S3 bucket -- Amazon's, or anything that speaks its API (R2, B2,
    /// MinIO, Wasabi, and Google Cloud Storage with HMAC keys).
    S3 {
        bucket: String,
        /// Folder inside the bucket, without leading or trailing slashes.
        #[serde(default)]
        prefix: String,
        /// `None` reads the profile's region, then `AWS_REGION`, then
        /// `us-east-1`.
        #[serde(default)]
        region: Option<String>,
        /// For a service that is not Amazon's: its base URL. Addressed
        /// path-style (`https://host/bucket/key`), which every such service
        /// accepts.
        #[serde(default)]
        endpoint: Option<String>,
        /// Which profile in `~/.aws` to read, when credentials come from
        /// this computer. `None` is `AWS_PROFILE`, then `default`.
        #[serde(default)]
        profile: Option<String>,
        #[serde(default)]
        credentials: S3Credentials,
    },
    /// A Google Cloud Storage bucket, signed in with this computer's own
    /// Google credentials.
    Gcs {
        bucket: String,
        #[serde(default)]
        prefix: String,
        /// A service account or user credentials file to use instead of the
        /// application default credentials.
        #[serde(default)]
        credentials_file: Option<String>,
    },
}

/// Where an S3 destination's credentials come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum S3Credentials {
    /// What this computer already has: the `AWS_*` environment, the
    /// profile's keys in `~/.aws/credentials`, or whatever the AWS CLI can
    /// export for it (SSO, an assumed role, a credential process). Nothing
    /// is stored.
    #[default]
    ThisComputer,
    /// An access key pasted into Settings, sealed in the vault -- for
    /// services whose keys are not kept in `~/.aws`.
    Stored,
}

/// An access key for an S3 destination, as [`S3Credentials::Stored`] keeps
/// it. Never sent to the interface.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S3Keys {
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl std::fmt::Debug for S3Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Keys")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<redacted>")
            .finish()
    }
}

impl Destination {
    /// How Settings and the status line name it: a path, `s3://bucket/prefix`
    /// or `gs://bucket/prefix`.
    pub fn label(&self) -> String {
        fn join(scheme: &str, bucket: &str, prefix: &str) -> String {
            if prefix.is_empty() {
                format!("{scheme}://{bucket}")
            } else {
                format!("{scheme}://{bucket}/{prefix}")
            }
        }
        match self {
            Self::Folder { path } => path.clone(),
            Self::S3 { bucket, prefix, endpoint: None, .. } => join("s3", bucket, prefix),
            Self::S3 { bucket, prefix, endpoint: Some(e), .. } => {
                format!("{} ({})", join("s3", bucket, prefix), e.trim_end_matches('/'))
            }
            Self::Gcs { bucket, prefix, .. } => join("gs", bucket, prefix),
        }
    }

    /// The same place, spelled the same way however it was typed: prefixes
    /// without stray slashes, blanks as `None`, the endpoint without a
    /// trailing slash.
    pub fn normalized(&self) -> Self {
        fn prefix(p: &str) -> String {
            p.trim().trim_matches('/').to_string()
        }
        fn opt(o: &Option<String>) -> Option<String> {
            o.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
        }
        match self {
            Self::Folder { path } => Self::Folder { path: path.trim().to_string() },
            Self::S3 { bucket, prefix: p, region, endpoint, profile, credentials } => Self::S3 {
                bucket: bucket.trim().to_string(),
                prefix: prefix(p),
                region: opt(region),
                endpoint: opt(endpoint).map(|e| e.trim_end_matches('/').to_string()),
                profile: opt(profile),
                credentials: *credentials,
            },
            Self::Gcs { bucket, prefix: p, credentials_file } => Self::Gcs {
                bucket: bucket.trim().to_string(),
                prefix: prefix(p),
                credentials_file: opt(credentials_file),
            },
        }
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Folder { path } => {
                let path = path.trim();
                if path.is_empty() {
                    return Err(Error::Invalid("choose a folder for backups".into()));
                }
                if !std::path::Path::new(path).is_absolute() {
                    return Err(Error::Invalid(format!(
                        "{path} is not a full path; choose the folder again"
                    )));
                }
                Ok(())
            }
            Self::S3 { bucket, prefix, endpoint, .. } => {
                check_bucket(bucket)?;
                check_prefix(prefix)?;
                if let Some(e) = endpoint.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
                    if !e.starts_with("https://") && !e.starts_with("http://") {
                        return Err(Error::Invalid(
                            "the endpoint must start with https:// or http://".into(),
                        ));
                    }
                    if e.starts_with("http://") && !crate::llm::is_loopback(e) {
                        return Err(Error::Invalid(
                            "an endpoint off this computer must be https:// -- the requests \
                             carry your access key's signature"
                                .into(),
                        ));
                    }
                    if e.contains(char::is_whitespace) {
                        return Err(Error::Invalid("the endpoint cannot contain spaces".into()));
                    }
                }
                Ok(())
            }
            Self::Gcs { bucket, prefix, .. } => {
                check_bucket(bucket)?;
                check_prefix(prefix)
            }
        }
    }
}

/// Bucket names, as both providers allow them: 3 to 63 characters of
/// lowercase letters, digits, dots, dashes and (Google only) underscores.
fn check_bucket(bucket: &str) -> Result<()> {
    let b = bucket.trim();
    let ok = (3..=222).contains(&b.len())
        && b.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-._".contains(c))
        && b.starts_with(|c: char| c.is_ascii_alphanumeric());
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "{b:?} is not a bucket name: lowercase letters, digits, dots and dashes"
        )))
    }
}

fn check_prefix(prefix: &str) -> Result<()> {
    let p = prefix.trim().trim_matches('/');
    if p.split('/').any(|part| part == ".." || part == ".") || p.contains("//") {
        return Err(Error::Invalid(format!("{p:?} is not a folder name inside a bucket")));
    }
    if p.chars().any(|c| c.is_control()) {
        return Err(Error::Invalid("the folder name cannot contain control characters".into()));
    }
    Ok(())
}

/// What became of the last backup, and of the one before it that worked.
///
/// Written by every run, and only by runs: the other half of the singleton
/// the settings are in, so a save from Settings and a run finishing cannot
/// overwrite one another.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct BackupStatus {
    /// [`Destination::label`] of where this is about. A status about a
    /// destination that is no longer the chosen one is not shown, and a new
    /// destination starts with a backup due at once.
    pub destination: Option<String>,
    pub last_attempt: Option<Timestamp>,
    pub last_success: Option<Timestamp>,
    /// Why the last attempt failed. Cleared by the next one that works.
    pub last_error: Option<String>,
    /// When the failures that are still going on began.
    pub failing_since: Option<Timestamp>,
    pub last_report: Option<BackupReport>,
    /// When somebody last stopped a backup to here. The schedule then waits
    /// a whole interval rather than starting the same backup again at its
    /// next look.
    pub stopped_at: Option<Timestamp>,
    /// Every destination ([`Destination::label`]) a backup has ever been
    /// made to, kept across changes of destination. A scheduled backup to
    /// one of these that finds nothing there has found a disk that is not
    /// plugged in, not a new place -- see `karmayoga_service::backup::repo`.
    pub used: Vec<String>,
}

/// The most destinations [`BackupStatus::used`] remembers.
const USED_REMEMBERED: usize = 32;

impl BackupStatus {
    /// The status as it applies to `destination`: itself if it is about the
    /// same place, or a fresh one if it is about somewhere else -- keeping
    /// the destinations ever used, whichever it is.
    pub fn for_destination(&self, destination: &Destination) -> Self {
        let label = destination.label();
        if self.destination.as_deref() == Some(label.as_str()) {
            self.clone()
        } else {
            Self { destination: Some(label), used: self.used.clone(), ..Self::default() }
        }
    }

    /// Whether a backup has ever been made to `destination`.
    pub fn has_used(&self, destination: &Destination) -> bool {
        let label = destination.label();
        self.used.contains(&label)
            || (self.destination.as_deref() == Some(label.as_str()) && self.last_success.is_some())
    }

    /// Remember that a backup was made to `destination`.
    pub fn mark_used(&mut self, destination: &Destination) {
        let label = destination.label();
        self.used.retain(|l| *l != label);
        self.used.push(label);
        if self.used.len() > USED_REMEMBERED {
            self.used.remove(0);
        }
    }
}

/// What one backup did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", default)]
pub struct BackupReport {
    /// The copy this made, as the destination names it: see
    /// [`snapshot_name`].
    pub snapshot: String,
    pub files: u64,
    /// Everything the copy holds, before chunks are shared with other copies.
    pub bytes: u64,
    /// What actually had to be sent: chunks no earlier copy had.
    pub uploaded_bytes: u64,
    pub uploaded_chunks: u64,
    /// Older copies let go of, to keep [`BackupSettings::keep`].
    pub removed_snapshots: u32,
    pub removed_chunks: u64,
    pub seconds: f64,
    /// Files that went away between being listed and being read -- an
    /// attachment collected, a pack compacted -- named so a gap is never
    /// silent.
    pub skipped: Vec<String>,
    /// The files folder is set up and was not usable -- an encrypted volume
    /// that is locked, a drive that is not mounted -- so this copy carries
    /// the previous copy's `files/` forward unchanged instead of recording
    /// the folder empty. Said in Settings, because it means the files in
    /// this copy are as old as that one's: a few runs during a locked volume
    /// would otherwise push every real copy of them out of "keep N".
    pub files_folder_unavailable: bool,
}

/// One copy at a destination, as Settings lists them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SnapshotInfo {
    pub name: String,
    pub created: Timestamp,
}

/// The name a backup made at `at` is stored under: `2026-11-14T030512Z`. A
/// date stamp, in UTC, with no colons in it -- a folder on a FAT disk or a
/// Windows share refuses them -- and in an order that sorts as it reads.
pub fn snapshot_name(at: Timestamp) -> String {
    at.strftime("%Y-%m-%dT%H%M%SZ").to_string()
}

/// The moment [`snapshot_name`] stamped into `name`, or `None` for a name it
/// did not make.
pub fn parse_snapshot_name(name: &str) -> Option<Timestamp> {
    let stamp = name.strip_suffix('Z')?;
    let dt = jiff::civil::DateTime::strptime("%Y-%m-%dT%H%M%S", stamp).ok()?;
    dt.to_zoned(jiff::tz::TimeZone::UTC).ok().map(|z| z.timestamp())
}

/// The keys one vault's backups are sealed and named under.
///
/// Two subkeys of the vault's data key, by label: one seals, one names. A
/// chunk's name is a keyed hash of its plaintext, so the same bytes in two
/// copies are stored once, and the name says nothing to anybody without
/// the key.
pub struct BackupKeys {
    seal: AeadCipher,
    name: SecretKey,
    /// The vault's own, unchanging identity -- see [`vault_identity`] --
    /// folded into [`BackupKeys::repository_id`].
    identity: String,
}

impl std::fmt::Debug for BackupKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BackupKeys(<redacted>)")
    }
}

/// What a restore says when the header at the backup's destination claims the
/// vault was never encrypted and the person has not said that is expected.
/// See [`BackupKeys::from_header`].
pub const UNENCRYPTED_BACKUP_REFUSAL: &str = "this backup says it is not encrypted, so restoring \
    it would ask for no password. A backup's header is a plain file that anyone who can write to \
    the folder or bucket could have replaced, and an unencrypted backup is sealed under a key \
    everybody knows -- so anyone with that access could have swapped in a vault of their own \
    making, and nothing here could tell. If this vault really never had a password, run the \
    restore again with --unencrypted; otherwise, do not restore from this place.";

const SEAL_LABEL: &str = "karmayoga.backup.seal.v1";
const NAME_LABEL: &str = "karmayoga.backup.name.v1";

/// Hex characters in a chunk's name: 128 bits of a keyed BLAKE3 hash. A
/// collision would need somebody holding the key, and by accident is out of
/// reach for any number of chunks a vault will ever be cut into.
pub const CHUNK_NAME_LEN: usize = 32;

impl BackupKeys {
    /// From the cipher a vault is unlocked with -- see
    /// [`Vault::backup_keys`](crate::Vault::backup_keys).
    ///
    /// `identity` is [`vault_identity`] of the vault's header.
    pub fn from_cipher(cipher: &dyn Cipher, identity: &str) -> Self {
        Self {
            seal: AeadCipher::new(&cipher.derive_subkey(SEAL_LABEL)),
            name: cipher.derive_subkey(NAME_LABEL),
            identity: identity.to_string(),
        }
    }

    /// From a vault header and its password, for a restore that has no vault
    /// open -- see [`crate::vault::VaultHeader`]. `None` for an unencrypted
    /// vault, whose header holds no key.
    ///
    /// A header that says the vault is not encrypted is refused unless
    /// `allow_unencrypted` is set, and that is the whole point of the
    /// argument. The header kept beside a backup is plaintext and
    /// unauthenticated, and an unencrypted vault's keys come from nothing
    /// secret: the same all-zero key for every one. So "none" in a header
    /// somebody else could have written is not a statement to act on -- it
    /// skips the password, and with it everything that makes the copy
    /// anybody's own. Anyone able to write to the bucket could put down a
    /// header saying so, a marker computed from that public key, and a whole
    /// repository of their own sealing, and a restore that believed the
    /// header would lay it down as the person's vault. A header that says
    /// the opposite needs no such care: its keys come out of the password,
    /// and a header somebody else wrote for a password they chose opens
    /// nothing of this vault's -- the manifests are sealed under the real
    /// key, which only the real header and the real password produce.
    pub fn from_header(
        header: &crate::vault::VaultHeader,
        password: Option<&str>,
        allow_unencrypted: bool,
    ) -> Result<Self> {
        if !header.is_encrypted() && !allow_unencrypted {
            return Err(Error::Invalid(UNENCRYPTED_BACKUP_REFUSAL.into()));
        }
        let cipher: Box<dyn Cipher> = match crate::vault::data_key_from(header, password)? {
            Some(dek) => Box::new(AeadCipher::new(&dek)),
            None => Box::new(crate::crypto::NullCipher),
        };
        Ok(Self::from_cipher(cipher.as_ref(), &vault_identity(header)))
    }

    /// The name a chunk holding `bytes` is stored under.
    pub fn chunk_name(&self, bytes: &[u8]) -> String {
        let hash = blake3::keyed_hash(self.name.expose(), bytes);
        hash.to_hex()[..CHUNK_NAME_LEN].to_string()
    }

    /// Seal `bytes`, which [`BackupKeys::chunk_name`] named `name`.
    pub fn seal_chunk(&self, name: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        self.seal.seal(&chunk_aad(name), bytes)
    }

    /// Open the chunk stored as `name`, and check that what it holds is what
    /// that name says -- a chunk copied over another under its name, or one
    /// sealed by a different vault, is refused rather than restored.
    pub fn open_chunk(&self, name: &str, sealed: &[u8]) -> Result<Vec<u8>> {
        let bytes = self.seal.open(&chunk_aad(name), sealed)?;
        if self.chunk_name(&bytes) != name {
            return Err(Error::Decrypt);
        }
        Ok(bytes)
    }

    /// Seal a manifest stored as `snapshot`. The name is bound in, so one
    /// copy's manifest cannot be passed off under another's date.
    pub fn seal_manifest(&self, snapshot: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        self.seal.seal(&manifest_aad(snapshot), bytes)
    }

    pub fn open_manifest(&self, snapshot: &str, sealed: &[u8]) -> Result<Vec<u8>> {
        self.seal.open(&manifest_aad(snapshot), sealed)
    }

    /// Who a repository belongs to: a keyed hash of the vault's identity,
    /// the same for every backup of one vault and different for every other
    /// vault, and nothing anybody without the key can work back from. A
    /// repository records it when it is made, so a second vault pointed at
    /// the same place is refused rather than mixed in.
    ///
    /// The identity is in it, not only the key, for unencrypted vaults: their
    /// key derives from nothing secret and is the same for all of them, so a
    /// keyed constant alone would let any two share one repository -- and
    /// prune each other's copies.
    pub fn repository_id(&self) -> String {
        let mut input = b"karmayoga.backup.repository\0".to_vec();
        input.extend_from_slice(self.identity.as_bytes());
        let hash = blake3::keyed_hash(self.name.expose(), &input);
        hash.to_hex()[..CHUNK_NAME_LEN].to_string()
    }
}

/// What tells one vault from every other for its backups: the moment its
/// header was created, which a password change, a rename or a restore keeps,
/// and which no two vaults share.
pub fn vault_identity(header: &crate::vault::VaultHeader) -> String {
    header.created_at.to_string()
}

fn chunk_aad(name: &str) -> Vec<u8> {
    format!("karmayoga.backup.chunk.v1:{name}").into_bytes()
}

fn manifest_aad(snapshot: &str) -> Vec<u8> {
    format!("karmayoga.backup.snapshot.v1:{snapshot}").into_bytes()
}

/// Associated data for the two sealed halves of the backup singleton.
pub fn settings_aad() -> Vec<u8> {
    b"karmayoga.backup-settings.v1".to_vec()
}

pub fn status_aad() -> Vec<u8> {
    b"karmayoga.backup-status.v1".to_vec()
}

/// One file a backup copies, as the vault hands it over: where it goes in
/// the copy, and where to read it from now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFile {
    /// Where it lives inside the vault directory, `/`-separated:
    /// `store/karmayoga.db`, `store/media/ab/…`, `spool/recordings/…`.
    pub path: String,
    /// Where to read it from: the live file, or for a database a consistent
    /// copy of it made for this backup.
    pub source: std::path::PathBuf,
    pub size: u64,
    /// Last modified, in nanoseconds since the epoch, when the platform
    /// says. A file whose size and time match the previous copy's is not
    /// read again. `None` for a database copy, which is always new.
    pub modified_ns: Option<i64>,
    /// A database, cut into small chunks: one page changing must not cost a
    /// megabyte. See `karmayoga_service::backup`.
    pub database: bool,
}

/// Every regular file under `dir`, as [`StagedFile`]s read in place, named
/// `prefix/…` with `/` between the parts whatever the platform. Symbolic
/// links are passed over -- a vault's are pointers left by a rename, never
/// data -- and so is anything `skip` says yes to, given the path it would be
/// named by. A file that cannot be looked at is passed over with a warning
/// rather than failing the whole listing; one that vanishes before it is
/// read is the backup's to report.
///
/// In name order, so two listings of the same tree come out the same.
pub fn list_files(
    dir: &std::path::Path,
    prefix: &str,
    skip: &dyn Fn(&str) -> bool,
) -> Result<Vec<StagedFile>> {
    let mut out = Vec::new();
    walk(dir, prefix, skip, &mut out)?;
    Ok(out)
}

fn walk(
    dir: &std::path::Path,
    prefix: &str,
    skip: &dyn Fn(&str) -> bool,
    out: &mut Vec<StagedFile>,
) -> Result<()> {
    let listing = match std::fs::read_dir(dir) {
        Ok(listing) => listing,
        // Gone between being seen and being opened -- a recording written
        // up, a shard emptied. Nothing in it to copy.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(Error::io(dir, e)),
    };
    let mut entries: Vec<_> = listing
        .filter_map(|e| match e {
            Ok(e) => Some(e),
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "backup: passing over an entry");
                None
            }
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            tracing::warn!(path = %entry.path().display(), "backup: passing over a name that is not UTF-8");
            continue;
        };
        let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        if skip(&path) {
            continue;
        }
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            // One folder that cannot be read costs that folder, said, not
            // the whole backup.
            if let Err(e) = walk(&entry.path(), &path, skip, out) {
                tracing::warn!(path = %path, error = %e, "backup: passing over a folder");
            }
        } else if kind.is_file() {
            let Ok(meta) = entry.metadata() else { continue };
            out.push(StagedFile {
                path,
                source: entry.path(),
                size: meta.len(),
                modified_ns: modified_ns(&meta),
                database: false,
            });
        }
    }
    Ok(())
}

/// A file's modification time in nanoseconds since the epoch, where the
/// platform says.
pub fn modified_ns(meta: &std::fs::Metadata) -> Option<i64> {
    let since = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(since.as_nanos()).ok()
}

/// Whether a file named `path` could be a database at all: one in a chat
/// link's store, or with a database's extension. Only those are opened to
/// look for SQLite's magic -- a vault's hundreds of thousands of sealed
/// attachments and mail packs never can be one, and opening each to check
/// would cost a seek apiece on every backup.
pub fn might_be_database(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let extension = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    path.split('/').any(|part| part == "chat")
        || matches!(extension.as_deref(), Some("sqlite" | "sqlite3" | "db" | "db3"))
}

/// Whether the file at `path` is a SQLite database: its first sixteen bytes
/// are SQLite's own magic. A database has to be copied through SQLite to be
/// a consistent copy; read like any other file while something writes to it,
/// it can be torn.
pub fn is_sqlite_file(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 16];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok()
        && &magic == b"SQLite format 3\0"
}

/// A store's half of a backup: given an empty staging directory on the same
/// disk as the vault, copy every database into it and list every file the
/// backup should read, with paths relative to the vault directory. See
/// [`crate::store::JournalStore::backup_job`].
pub type BackupJob = Box<dyn FnOnce(&std::path::Path) -> Result<Vec<StagedFile>> + Send>;

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(byte: u8) -> BackupKeys {
        BackupKeys::from_cipher(&AeadCipher::new(&SecretKey::from_bytes([byte; 32])), "vault")
    }

    #[test]
    fn a_chunk_round_trips_and_its_name_is_checked() {
        let k = keys(1);
        let name = k.chunk_name(b"hello");
        assert_eq!(name.len(), CHUNK_NAME_LEN);
        let sealed = k.seal_chunk(&name, b"hello").unwrap();
        assert_eq!(k.open_chunk(&name, &sealed).unwrap(), b"hello");

        // Stored under another chunk's name: the AAD no longer matches.
        let other = k.chunk_name(b"world");
        assert!(k.open_chunk(&other, &sealed).is_err());
    }

    #[test]
    fn names_are_keyed_per_vault() {
        assert_eq!(keys(1).chunk_name(b"x"), keys(1).chunk_name(b"x"));
        assert_ne!(keys(1).chunk_name(b"x"), keys(2).chunk_name(b"x"));
        assert_ne!(keys(1).repository_id(), keys(2).repository_id());
        // Not the plain hash either.
        assert_ne!(keys(1).chunk_name(b"x"), blake3::hash(b"x").to_hex()[..CHUNK_NAME_LEN]);
    }

    #[test]
    fn unencrypted_vaults_do_not_share_a_repository() {
        use crate::crypto::NullCipher;
        let a = BackupKeys::from_cipher(&NullCipher, "2026-01-01T00:00:00Z");
        let b = BackupKeys::from_cipher(&NullCipher, "2026-02-02T00:00:00Z");
        assert_ne!(a.repository_id(), b.repository_id());
    }

    #[test]
    fn a_header_that_says_none_is_not_restored_from_unless_that_was_asked_for() {
        let header: crate::vault::VaultHeader = serde_json::from_str(
            r#"{"format":1,"name":"x","backend":"sqlite","cipher":"none",
                "createdAt":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        // The password, if one is offered, is no way round it.
        let err = BackupKeys::from_header(&header, Some("anything"), false).unwrap_err();
        assert!(err.to_string().contains("--unencrypted"), "{err}");
        assert!(BackupKeys::from_header(&header, None, false).is_err());
        // Asked for, it is the all-known key it always was.
        let keys = BackupKeys::from_header(&header, None, true).unwrap();
        let null = BackupKeys::from_cipher(&crate::crypto::NullCipher, &vault_identity(&header));
        assert_eq!(keys.repository_id(), null.repository_id());
    }

    #[test]
    fn destinations_used_are_remembered_across_a_change_of_destination() {
        let usb = Destination::Folder { path: "/media/usb".into() };
        let bucket =
            Destination::Gcs { bucket: "b".into(), prefix: String::new(), credentials_file: None };
        let mut status = BackupStatus::default().for_destination(&usb);
        assert!(!status.has_used(&usb));
        status.mark_used(&usb);
        let elsewhere = status.for_destination(&bucket);
        assert!(!elsewhere.has_used(&bucket));
        let back = elsewhere.for_destination(&usb);
        assert!(back.has_used(&usb), "switching away and back does not forget it");
        assert_eq!(back.last_success, None);
    }

    #[test]
    fn only_plausible_databases_are_sniffed() {
        assert!(might_be_database("store/chat/link/matrix-sdk-crypto.sqlite3"));
        assert!(might_be_database("store/chat/link/anything"));
        assert!(might_be_database("other.db"));
        assert!(!might_be_database("store/media/ab/abcdef.blob"));
        assert!(!might_be_database("store/mail-packs/acct/0001.pack"));
        assert!(!might_be_database("chatty.json"));
    }

    #[test]
    fn another_vaults_key_opens_nothing() {
        let sealed = keys(1).seal_manifest("2026-11-14T030000Z", b"{}").unwrap();
        assert!(keys(2).open_manifest("2026-11-14T030000Z", &sealed).is_err());
        // Nor under another date.
        assert!(keys(1).open_manifest("2026-11-15T030000Z", &sealed).is_err());
        assert_eq!(keys(1).open_manifest("2026-11-14T030000Z", &sealed).unwrap(), b"{}");
    }

    #[test]
    fn snapshot_names_round_trip_and_sort_by_time() {
        let at: Timestamp = "2026-11-14T03:05:12Z".parse().unwrap();
        let name = snapshot_name(at);
        assert_eq!(name, "2026-11-14T030512Z");
        assert_eq!(parse_snapshot_name(&name), Some(at));
        assert!(parse_snapshot_name("not-a-date").is_none());
        let later = snapshot_name("2026-11-14T13:00:00Z".parse().unwrap());
        assert!(later > name);
    }

    #[test]
    fn settings_validate_frequency_keep_and_destination() {
        let mut s = BackupSettings::default();
        s.validate().unwrap();
        s.every_hours = 5;
        assert!(s.validate().is_err());
        s.every_hours = 6;
        s.keep = 0;
        assert!(s.validate().is_err());
        s.keep = 7;
        s.enabled = true;
        assert!(s.validate().is_err(), "on with nowhere to go");
        s.destination = Some(Destination::Folder { path: "relative/path".into() });
        assert!(s.validate().is_err());
        s.destination = Some(Destination::Folder { path: "/mnt/backup".into() });
        s.validate().unwrap();
        s.destination = Some(Destination::S3 {
            bucket: "My Bucket".into(),
            prefix: String::new(),
            region: None,
            endpoint: None,
            profile: None,
            credentials: S3Credentials::ThisComputer,
        });
        assert!(s.validate().is_err());
    }

    #[test]
    fn an_endpoint_off_this_computer_must_be_https() {
        let d = |e: &str| Destination::S3 {
            bucket: "backups".into(),
            prefix: "karmayoga".into(),
            region: None,
            endpoint: Some(e.into()),
            profile: None,
            credentials: S3Credentials::Stored,
        };
        assert!(d("http://minio.example.com").validate().is_err());
        d("http://127.0.0.1:9000").validate().unwrap();
        d("https://abc.r2.cloudflarestorage.com").validate().unwrap();
    }

    #[test]
    fn labels_and_normalising() {
        let d = Destination::Gcs {
            bucket: " backups ".into(),
            prefix: "/karmayoga/hari/".into(),
            credentials_file: Some("  ".into()),
        }
        .normalized();
        assert_eq!(d.label(), "gs://backups/karmayoga/hari");
        assert_eq!(
            d,
            Destination::Gcs {
                bucket: "backups".into(),
                prefix: "karmayoga/hari".into(),
                credentials_file: None
            }
        );
    }

    #[test]
    fn a_status_for_another_destination_starts_fresh() {
        let folder = Destination::Folder { path: "/mnt/a".into() };
        let status = BackupStatus {
            destination: Some(folder.label()),
            last_success: Some(Timestamp::now()),
            ..BackupStatus::default()
        };
        assert_eq!(status.for_destination(&folder), status);
        let other = Destination::Folder { path: "/mnt/b".into() };
        let fresh = status.for_destination(&other);
        assert_eq!(fresh.last_success, None);
        assert_eq!(fresh.destination.as_deref(), Some("/mnt/b"));
    }
}
