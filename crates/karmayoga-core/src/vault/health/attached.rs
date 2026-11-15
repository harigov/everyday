//! Health's files in the files catalog: a document's pages and a meal's
//! photograph.
//!
//! Health keeps no bytes of its own for what somebody hands it. A scan, a PDF
//! or a picture of a plate goes into the files catalog the way every app's
//! files do -- uploaded in pieces, or copied in by path on the vault's own
//! machine -- sealed, with [`FileOrigin::Health`], and the record refers to
//! it by id. What that buys is the Files app's: a viewer and thumbnails,
//! "used in", backups and exports that carry the file, and a trash.
//!
//! The rules here:
//!
//! - **A record refers only to files that are there**: in the catalog and
//!   not in the trash ([`Vault::health_files`]).
//! - **The references table follows every save and delete**, so the Files
//!   app can say which document a page belongs to, and never lists one under
//!   "Unused".
//! - **What Health sealed and nothing refers to any more goes to the Files
//!   trash** when a document or a meal lets go of it ([`Vault::release_health_files`]):
//!   deleting a lab report should not leave its pages behind in the vault,
//!   and the trash gives them back for a while if that was a mistake. A file
//!   somebody has since written out to the folder is theirs, as every plain
//!   file is, and is left where it is.

use crate::error::{Error, Result};
use crate::files::{FileEntry, FileOrigin, FileStatus};
use crate::id::FileId;
use crate::vault::Vault;

impl Vault {
    /// The catalog's entry for each of `ids`, in order and without repeats,
    /// each checked to be a file a record may refer to: in the catalog and
    /// not in the trash.
    pub(super) fn health_files(&self, ids: &[FileId]) -> Result<Vec<FileEntry>> {
        let mut seen = Vec::with_capacity(ids.len());
        let mut entries = Vec::with_capacity(ids.len());
        for id in ids {
            if seen.contains(id) {
                continue;
            }
            seen.push(*id);
            let entry = match self.file(*id) {
                Ok(entry) => entry,
                Err(Error::NotFound { .. }) => {
                    return Err(Error::Invalid(
                        "that file is not in the files catalog; add it again".into(),
                    ));
                }
                Err(Error::Unsupported(_)) => {
                    return Err(Error::Unsupported(
                        "this vault keeps no files catalog, so Health cannot keep files".into(),
                    ));
                }
                Err(e) => return Err(e),
            };
            if entry.status.is_trashed() {
                return Err(Error::Invalid(format!(
                    "{} is in the trash; restore it in Files first",
                    entry.name
                )));
            }
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Files a document or a meal no longer refers to, after its references
    /// have been recorded: each one Health sealed that nothing else refers to
    /// goes to the Files trash. Never fails the save or the delete that let
    /// go of them: a failure is logged, and the file stays in the catalog
    /// where the Files app's "Unused" view finds it.
    pub(super) fn release_health_files(&self, dropped: &[FileId]) {
        if dropped.is_empty() {
            return;
        }
        let unused = self.with_files(|s| {
            let mut unused = Vec::new();
            for entry in s.get_files(dropped)? {
                let ours = entry.origin == FileOrigin::Health && entry.is_sealed();
                if ours
                    && matches!(entry.status, FileStatus::Present)
                    && s.refs_to(entry.id)?.is_empty()
                {
                    unused.push(entry.id);
                }
            }
            Ok(unused)
        });
        let trashed = unused
            .and_then(|ids| if ids.is_empty() { Ok(Vec::new()) } else { self.trash_files(&ids) });
        if let Err(e) = trashed {
            tracing::warn!(error = %e, "could not move Health's unused files to the trash");
        }
    }
}
