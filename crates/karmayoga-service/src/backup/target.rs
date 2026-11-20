//! Where a repository lives, as the backup sees it: a flat set of named
//! objects under one root -- a folder, or a prefix in a bucket.
//!
//! Four verbs and nothing else: put a whole object, get a whole object, list
//! the names under a prefix, delete one. Every object a backup writes is
//! small -- a chunk is a megabyte at most, a manifest a few -- so there is no
//! multipart upload, no ranged read and no streaming here, and every
//! provider's simplest request is enough. That is most of why S3 and Google
//! Cloud Storage are a few hundred lines each rather than an SDK.
//!
//! Names are relative to the root and `/`-separated -- `chunks/3f/3f9a…`,
//! `snapshots/2026-11-14T030512Z.snapshot` -- whatever the platform. A target
//! that keeps them under a prefix of its own adds it on the way out and takes
//! it off on the way back in; the backup never sees it.
//!
//! # What a destination may hand back
//!
//! A destination is somebody else's disk or bucket, and what is in it is only
//! trusted once it has been opened with the vault's key -- which is after it has
//! been read, whole, into memory. So nothing is read past the size an object of
//! its kind can be: [`MAX_CHUNK_BYTES`], [`MAX_OBJECT_BYTES`], [`MAX_REPLY_BYTES`]
//! and, for a listing, [`MAX_LIST_PAGES`]. A destination that answers with more is
//! refused with an error that says so; it is never partly read and carried on
//! with, and never inflated -- see [`http_client`].

use bytes::Bytes;
use karmayoga_core::backup::{Destination, S3Credentials, S3Keys};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::folder::FolderTarget;
use super::gcs::{GcsConfig, GcsTarget};
use super::s3::{S3Config, S3Target};

/// The most a sealed chunk can be. A backup cuts a file every
/// `run::FILE_CHUNK` bytes -- a megabyte -- and the
/// seal adds a few dozen; sixteen times that is headroom for a build that
/// cuts larger (each file's `chunk_size` is in its manifest), and small enough
/// that the two dozen chunks a restore reads at once cannot come to a gigabyte.
pub const MAX_CHUNK_BYTES: u64 = 16 * 1024 * 1024;

/// The most any other object can be: a manifest, which is JSON compressed and
/// sealed and comes to a few megabytes for a vault of two hundred thousand
/// files, the marker, the header, the lock. Sixty-four megabytes is more than
/// a million files, and one is read at a time.
pub const MAX_OBJECT_BYTES: u64 = 64 * 1024 * 1024;

/// The most a reply that is not an object can be: a page of a listing, a
/// sign-in service's token, a refusal's explanation. A thousand keys is a few
/// hundred kilobytes of XML or JSON.
pub const MAX_REPLY_BYTES: u64 = 8 * 1024 * 1024;

/// How many pages of a listing are followed. A page is a thousand objects, so
/// this is five million: five terabytes of one-megabyte chunks, far past a
/// vault, and a service that has not stopped by then is not going to.
pub const MAX_LIST_PAGES: usize = 5_000;

/// The most the object called `name` can be.
pub fn object_limit(name: &str) -> u64 {
    if name.starts_with("chunks/") { MAX_CHUNK_BYTES } else { MAX_OBJECT_BYTES }
}

/// Why a request to a destination failed, in the four ways that call for
/// different words to the person reading them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetErrorKind {
    /// Nothing answered: no network, a timeout, a name that does not
    /// resolve, a folder on a disk that is not plugged in.
    Unreachable,
    /// It answered and refused: credentials it does not accept, or a bucket
    /// they are not allowed into.
    Denied,
    /// The bucket, or the folder itself, is not there.
    Missing,
    /// There were no credentials to send -- nothing in the environment, the
    /// profile has no keys, the CLI that would have exported them is not
    /// installed.
    Credentials,
    Other,
}

#[derive(Debug, Clone)]
pub struct TargetError {
    pub kind: TargetErrorKind,
    /// Said to a person: what happened, and where it can be, what to do.
    pub message: String,
}

impl TargetError {
    pub fn new(kind: TargetErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::new(TargetErrorKind::Other, message)
    }
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TargetError {}

pub type TargetResult<T> = Result<T, TargetError>;

/// One destination, ready to be written to.
pub enum Target {
    Folder(FolderTarget),
    S3(S3Target),
    Gcs(GcsTarget),
}

impl Target {
    /// The destination Settings chose. `s3_keys` is the pasted access key,
    /// for an S3 destination whose credentials are
    /// [`S3Credentials::Stored`]; `None` there is refused here rather than
    /// at the first request.
    pub fn open(destination: &Destination, s3_keys: Option<S3Keys>) -> TargetResult<Self> {
        match destination {
            Destination::Folder { path } => {
                Ok(Self::Folder(FolderTarget::new(PathBuf::from(path))))
            }
            Destination::S3 { bucket, prefix, region, endpoint, profile, credentials } => {
                let keys = match credentials {
                    S3Credentials::ThisComputer => None,
                    S3Credentials::Stored => {
                        let keys = s3_keys.ok_or_else(|| {
                            TargetError::new(
                                TargetErrorKind::Credentials,
                                "no access key is stored for this bucket; add one in \
                                 Settings \u{2192} Backup",
                            )
                        })?;
                        Some((keys.access_key_id, keys.secret_access_key))
                    }
                };
                Ok(Self::S3(S3Target::new(
                    http_client()?,
                    S3Config {
                        bucket: bucket.clone(),
                        prefix: prefix.clone(),
                        region: region.clone(),
                        endpoint: endpoint.clone(),
                        profile: profile.clone(),
                        keys,
                    },
                )))
            }
            Destination::Gcs { bucket, prefix, credentials_file } => Ok(Self::Gcs(GcsTarget::new(
                http_client()?,
                GcsConfig {
                    bucket: bucket.clone(),
                    prefix: prefix.clone(),
                    credentials_file: credentials_file.clone(),
                },
            ))),
        }
    }

    /// A folder, for the CLI's `restore` and for tests.
    pub fn folder(path: &Path) -> Self {
        Self::Folder(FolderTarget::new(path.to_path_buf()))
    }

    /// Store `body` as `name`, replacing anything there.
    pub async fn put(&self, name: &str, body: Bytes) -> TargetResult<()> {
        match self {
            Self::Folder(t) => t.put(name, body).await,
            Self::S3(t) => t.put(name, body).await,
            Self::Gcs(t) => t.put(name, body).await,
        }
    }

    /// The object stored as `name`, or `None` if there is none.
    pub async fn get(&self, name: &str) -> TargetResult<Option<Bytes>> {
        match self {
            Self::Folder(t) => t.get(name).await,
            Self::S3(t) => t.get(name).await,
            Self::Gcs(t) => t.get(name).await,
        }
    }

    /// Every name that starts with `prefix` -- `""` for all of them, or a
    /// folder such as `"chunks/"` -- in no particular order.
    pub async fn list(&self, prefix: &str) -> TargetResult<Vec<String>> {
        match self {
            Self::Folder(t) => t.list(prefix).await,
            Self::S3(t) => t.list(prefix).await,
            Self::Gcs(t) => t.list(prefix).await,
        }
    }

    /// Delete `name`. One that is already gone is not an error.
    pub async fn delete(&self, name: &str) -> TargetResult<()> {
        match self {
            Self::Folder(t) => t.delete(name).await,
            Self::S3(t) => t.delete(name).await,
            Self::Gcs(t) => t.delete(name).await,
        }
    }

    /// Make the place itself, for a destination used for the first time: a
    /// folder is created; a bucket has to exist already, and is not.
    pub async fn create_root(&self) -> TargetResult<()> {
        match self {
            Self::Folder(t) => t.create_root().await,
            Self::S3(_) | Self::Gcs(_) => Ok(()),
        }
    }

    /// How many requests to have in flight at once. A folder is a disk, and
    /// more than a few writers only queue at it; a bucket is a service, and
    /// most of a request's time is the round trip.
    pub fn parallelism(&self) -> usize {
        match self {
            Self::Folder(_) => 8,
            Self::S3(_) | Self::Gcs(_) => 24,
        }
    }
}

/// One HTTP client for a backup's requests. Not the one the assistant's
/// web tools share: that one refuses private addresses, and a backup to a
/// MinIO on this computer or the local network is exactly a private address.
/// Generous timeouts, because a megabyte up a slow connection is not quick,
/// and a pool deep enough for [`Target::parallelism`] requests at once.
///
/// Nothing is decompressed. reqwest, built with its `gzip` feature, asks for
/// compressed answers and inflates them as they are read -- before anything has
/// been checked, and to any size. An object in the bucket that is a few
/// kilobytes of gzip and a hundred gigabytes inflated would be filled into
/// memory until the application was killed, on every backup, for as long as it
/// stayed there. Objects are sealed, and sealing does not compress, so
/// asking for none is free; `no_gzip` and its three siblings exist whether or
/// not their feature is on, so a dependency turning one on cannot turn this
/// back. The size caps in `creds::send` hold regardless.
///
/// Redirects are not followed. A signed request sent on to another host
/// arrives without its signature -- reqwest drops `Authorization` across
/// hosts -- and comes back as a refusal that names the wrong cause; S3's 307
/// to a bucket's own region is answered in `s3.rs` by switching region and
/// signing again, which needs to see the 307 rather than its aftermath.
fn http_client() -> TargetResult<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_gzip()
        .no_brotli()
        .no_zstd()
        .no_deflate()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(300))
        .pool_max_idle_per_host(32)
        .user_agent(concat!("KarmaYoga/", env!("CARGO_PKG_VERSION"), " backup"))
        .build()
        .map_err(|e| TargetError::other(format!("could not set up the connection: {e}")))
}
