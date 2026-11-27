//! A repository in a folder: another disk, a mounted share, a synced folder.
//!
//! Every object is a file under the root, at its own name. Written to a
//! temporary name beside where it goes, flushed to the disk and renamed into
//! place, so a backup interrupted by a pulled cable leaves whole files or no
//! files -- never a chunk that is half there under its real name.

use bytes::Bytes;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::target::{TargetError, TargetErrorKind, TargetResult, object_limit};

/// What a file being written is called until it is whole. Never listed.
const PARTIAL: &str = ".partial-";

pub struct FolderTarget {
    root: PathBuf,
    /// Folders a chunk has been renamed into since the last object that
    /// says a backup is whole. Their entries are made durable before that
    /// object is written -- see [`FolderTarget::put`].
    unsynced: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<PathBuf>>>,
}

impl FolderTarget {
    pub fn new(root: PathBuf) -> Self {
        Self { root, unsynced: Default::default() }
    }

    /// The file `name` is kept in, refusing a name that would reach outside
    /// the root. Every name comes from the backup itself, so this is a
    /// guard against a bug rather than against anybody.
    fn path(&self, name: &str) -> TargetResult<PathBuf> {
        if name.is_empty()
            || name.starts_with('/')
            || name.split('/').any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(TargetError::other(format!("{name:?} is not a name a backup writes")));
        }
        Ok(name.split('/').fold(self.root.clone(), |path, part| path.join(part)))
    }

    pub async fn put(&self, name: &str, body: Bytes) -> TargetResult<()> {
        let path = self.path(name)?;
        let root = self.root.clone();
        let is_chunk = name.starts_with("chunks/");
        let unsynced = self.unsynced.clone();
        blocking(move || {
            // The root itself is never made here: a disk that is not plugged
            // in leaves its mount point behind, and a backup written into it
            // would fill the computer's own disk while looking like it
            // worked. See [`FolderTarget::create_root`].
            if !root.is_dir() {
                return Err(missing(&root));
            }
            let dir = path.parent().unwrap_or(&root);
            std::fs::create_dir_all(dir).map_err(|e| io_error(&root, dir, e))?;
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("object");
            let partial = dir.join(format!("{PARTIAL}{file_name}-{:08x}", rand::random::<u32>()));
            let written = (|| {
                let mut file = std::fs::File::create(&partial)?;
                file.write_all(&body)?;
                file.sync_all()?;
                std::fs::rename(&partial, &path)
            })();
            if let Err(e) = written {
                let _ = std::fs::remove_file(&partial);
                return Err(io_error(&root, &path, e));
            }
            // A chunk's bytes are flushed, but its folder's entry for it is
            // left for later rather than paying a directory sync each: the
            // folder is remembered, with `chunks/` itself for a shard just
            // made. A manifest, or anything else at the top of the
            // repository, is what says a backup is whole, so before it is
            // written every remembered folder is synced -- on a disk that
            // does not keep metadata in order (exFAT, a network share), a
            // manifest must never outlive the chunks it names.
            let mut pending = unsynced.lock().unwrap_or_else(|e| e.into_inner());
            if is_chunk {
                pending.insert(dir.to_path_buf());
                if let Some(chunks) = dir.parent() {
                    pending.insert(chunks.to_path_buf());
                }
            } else {
                for folder in pending.drain() {
                    jatayu_core::fsutil::sync_dir(&folder);
                }
                jatayu_core::fsutil::sync_dir(dir);
            }
            Ok(())
        })
        .await
    }

    /// Make the folder itself, for a destination being used for the first
    /// time. Only then: see [`FolderTarget::put`].
    pub async fn create_root(&self) -> TargetResult<()> {
        let root = self.root.clone();
        blocking(move || std::fs::create_dir_all(&root).map_err(|e| io_error(&root, &root, e)))
            .await
    }

    /// The object called `name`, or `None` if there is no such file.
    ///
    /// A folder is somebody's disk or a share other computers write to, and
    /// whatever is in it under a backup's name is read whole into memory
    /// before anything has checked it. So a file larger than an object of
    /// that kind can be -- [`object_limit`] -- is refused unread, and one
    /// that grows past it between being measured and being read is cut off
    /// there rather than followed.
    pub async fn get(&self, name: &str) -> TargetResult<Option<Bytes>> {
        let path = self.path(name)?;
        let root = self.root.clone();
        let limit = object_limit(name);
        blocking(move || {
            let read = (|| -> std::io::Result<Option<Vec<u8>>> {
                use std::io::Read;
                let file = std::fs::File::open(&path)?;
                let size = file.metadata()?.len();
                if size > limit {
                    return Ok(None);
                }
                let mut bytes = Vec::with_capacity(size as usize);
                // One past the limit, to tell a file that is exactly as large
                // as may be from one that goes on.
                file.take(limit + 1).read_to_end(&mut bytes)?;
                Ok((bytes.len() as u64 <= limit).then_some(bytes))
            })();
            match read {
                Ok(Some(bytes)) => Ok(Some(Bytes::from(bytes))),
                Ok(None) => Err(too_large(&path, limit)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if root.is_dir() {
                        Ok(None)
                    } else {
                        Err(missing(&root))
                    }
                }
                Err(e) => Err(io_error(&root, &path, e)),
            }
        })
        .await
    }

    pub async fn list(&self, prefix: &str) -> TargetResult<Vec<String>> {
        let root = self.root.clone();
        let prefix = prefix.to_string();
        blocking(move || {
            if !root.is_dir() {
                return Err(missing(&root));
            }
            // Walk only the folder the prefix names, not the whole root.
            let (dir_part, _) = prefix.rsplit_once('/').unwrap_or(("", ""));
            let start = dir_part
                .split('/')
                .filter(|p| !p.is_empty())
                .fold(root.clone(), |path, part| path.join(part));
            let mut out = Vec::new();
            if start.is_dir() {
                walk(&start, dir_part, &mut out).map_err(|e| io_error(&root, &start, e))?;
            }
            out.retain(|name| name.starts_with(&prefix));
            Ok(out)
        })
        .await
    }

    pub async fn delete(&self, name: &str) -> TargetResult<()> {
        let path = self.path(name)?;
        let root = self.root.clone();
        blocking(move || match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_error(&root, &path, e)),
        })
        .await
    }
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
        if name.starts_with(PARTIAL) {
            continue;
        }
        let full = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(&entry.path(), &full, out)?;
        } else if kind.is_file() {
            out.push(full);
        }
    }
    Ok(())
}

fn missing(root: &Path) -> TargetError {
    TargetError::new(
        TargetErrorKind::Missing,
        format!("{} is not there -- if it is on a disk, is the disk plugged in?", root.display()),
    )
}

/// A file under a backup's name that is bigger than any object a backup
/// writes.
fn too_large(path: &Path, limit: u64) -> TargetError {
    TargetError::other(format!(
        "{} is larger than any object a backup stores ({} MB), so it was not read; something \
         other than Jatayu put it there",
        path.display(),
        limit / (1024 * 1024)
    ))
}

fn io_error(root: &Path, path: &Path, e: std::io::Error) -> TargetError {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound if !root.is_dir() => missing(root),
        PermissionDenied | ReadOnlyFilesystem => TargetError::new(
            TargetErrorKind::Denied,
            format!("{} cannot be written to: {e}", root.display()),
        ),
        StorageFull => {
            TargetError::new(TargetErrorKind::Other, format!("{} is full: {e}", root.display()))
        }
        _ => TargetError::other(format!("{}: {e}", path.display())),
    }
}

/// Run file work on the blocking pool, where a slow disk holds up only
/// itself.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> TargetResult<T> + Send + 'static,
) -> TargetResult<T> {
    // Same erasure as `service::blocking`: the closure and its value are
    // boxed, so every caller shares one `spawn_blocking` instantiation.
    let f: Box<dyn FnOnce() -> TargetResult<Box<dyn std::any::Any + Send>> + Send> =
        Box::new(move || match f() {
            Ok(value) => Ok(Box::new(value)),
            Err(error) => Err(error),
        });
    match spawn_erased(f)
        .await
        .map_err(|e| TargetError::other(format!("the file task failed: {e}")))?
    {
        Ok(value) => match value.downcast::<T>() {
            Ok(value) => Ok(*value),
            Err(_) => {
                Err(TargetError::other("the file task returned an unexpected type".to_string()))
            }
        },
        Err(error) => Err(error),
    }
}

#[inline(never)]
fn spawn_erased(
    f: Box<dyn FnOnce() -> TargetResult<Box<dyn std::any::Any + Send>> + Send>,
) -> tokio::task::JoinHandle<TargetResult<Box<dyn std::any::Any + Send>>> {
    tokio::task::spawn_blocking(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    #[test]
    fn objects_round_trip_list_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let t = FolderTarget::new(dir.path().to_path_buf());
        rt().block_on(async {
            t.put("chunks/ab/ab12", Bytes::from_static(b"one")).await.unwrap();
            t.put("chunks/cd/cd34", Bytes::from_static(b"two")).await.unwrap();
            t.put("snapshots/2026-11-14T030000Z.snapshot", Bytes::from_static(b"m")).await.unwrap();
            assert_eq!(t.get("chunks/ab/ab12").await.unwrap().unwrap(), &b"one"[..]);
            assert!(t.get("chunks/ab/nope").await.unwrap().is_none());

            let mut chunks = t.list("chunks/").await.unwrap();
            chunks.sort();
            assert_eq!(chunks, ["chunks/ab/ab12", "chunks/cd/cd34"]);
            assert_eq!(t.list("").await.unwrap().len(), 3);
            assert_eq!(t.list("snapshots/").await.unwrap().len(), 1);

            t.delete("chunks/ab/ab12").await.unwrap();
            t.delete("chunks/ab/ab12").await.unwrap();
            assert_eq!(t.list("chunks/").await.unwrap(), ["chunks/cd/cd34"]);
        });
    }

    #[test]
    fn a_missing_folder_says_so_and_names_cannot_escape() {
        let dir = tempfile::tempdir().unwrap();
        let gone = FolderTarget::new(dir.path().join("unplugged"));
        rt().block_on(async {
            let err = gone.list("").await.unwrap_err();
            assert_eq!(err.kind, TargetErrorKind::Missing);
            let err = gone.get("jatayu-backup.json").await.unwrap_err();
            assert_eq!(err.kind, TargetErrorKind::Missing);
            let t = FolderTarget::new(dir.path().to_path_buf());
            assert!(t.put("../outside", Bytes::new()).await.is_err());
            assert!(t.put("/abs", Bytes::new()).await.is_err());
        });
    }
}
