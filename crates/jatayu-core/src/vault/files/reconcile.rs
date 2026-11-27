//! What the background reconciler calls to bring the catalog and the folder together.

use super::Vault;
use super::helpers::{found_entry, from_micros, set_plain_path, stack_new_files};
use crate::error::Result;
use crate::files::disk;
use crate::files::{AppliedScan, FileEntry, FileStatus, IndexRow, ScanChanges, ScanReport};
use crate::id::FileId;
use crate::record::RecordKind;
use jiff::Timestamp;
use std::collections::{HashMap, HashSet};

impl Vault {
    pub fn file_index(&self) -> Result<Vec<IndexRow>> {
        self.with_files(|s| s.file_index())
    }

    /// Write one pass's findings in one go: new files catalogued (origin
    /// [`crate::files::FileOrigin::Found`]), changed ones updated (their
    /// metadata to be read again), moved ones re-pathed, vanished ones
    /// marked missing, returned ones present again. Stacks among the new
    /// files are formed ([`crate::files::reconcile::stacks`]).
    ///
    /// A file arriving next to one the catalog already has -- the RAW file
    /// that a sync tool brings after its JPEG -- is stacked under it too,
    /// found by names alone from the catalog's clear columns. Nothing is
    /// done while the folder is unavailable, whatever the changes say.
    pub fn apply_scan(&self, changes: &ScanChanges) -> Result<AppliedScan> {
        self.writable()?;
        let mut report = ScanReport::default();
        if !self.folder_state()?.is_available() {
            report.unavailable = true;
            return Ok(AppliedScan { report, to_probe: Vec::new() });
        }
        let now = Timestamp::now();
        let wanted: Vec<FileId> = changes
            .changed
            .iter()
            .map(|c| c.id)
            .chain(changes.moved.iter().map(|m| m.id))
            .chain(changes.vanished.iter().copied())
            .chain(changes.returned.iter().copied())
            .collect();
        let mut known: HashMap<FileId, FileEntry> =
            self.with_files(|s| s.get_files(&wanted))?.into_iter().map(|f| (f.id, f)).collect();
        let index = self.file_index()?;
        let taken: HashSet<&str> = index.iter().map(|row| row.path.as_str()).collect();
        let move_targets: HashSet<&str> = changes.moved.iter().map(|m| m.path.as_str()).collect();

        let mut batch: Vec<FileEntry> = Vec::new();
        let mut to_probe: Vec<FileId> = Vec::new();
        for c in &changes.changed {
            let Some(mut f) = known.remove(&c.id).filter(|f| !f.status.is_trashed()) else {
                continue;
            };
            f.hash.clone_from(&c.hash);
            f.size = c.size;
            f.modified_at = from_micros(c.modified_us);
            f.status = FileStatus::Present;
            f.updated_at = now;
            to_probe.push(f.id);
            batch.push(f);
            report.changed += 1;
        }
        for m in &changes.moved {
            let Some(mut f) = known.remove(&m.id).filter(|f| !f.status.is_trashed()) else {
                continue;
            };
            set_plain_path(&mut f, m.path.clone());
            f.size = m.size;
            f.modified_at = from_micros(m.modified_us);
            f.status = FileStatus::Present;
            f.updated_at = now;
            batch.push(f);
            report.moved += 1;
        }
        for id in &changes.vanished {
            let Some(mut f) = known.remove(id).filter(|f| f.status == FileStatus::Present) else {
                continue;
            };
            f.status = FileStatus::Missing { since: now };
            f.updated_at = now;
            batch.push(f);
            report.missing += 1;
        }
        for id in &changes.returned {
            let Some(mut f) =
                known.remove(id).filter(|f| matches!(f.status, FileStatus::Missing { .. }))
            else {
                continue;
            };
            f.status = FileStatus::Present;
            f.updated_at = now;
            batch.push(f);
            report.returned += 1;
        }

        let mut fresh: Vec<FileEntry> = Vec::with_capacity(changes.new.len());
        for n in &changes.new {
            // Somebody got there first: an import, or the pass before.
            if taken.contains(n.path.as_str()) || move_targets.contains(n.path.as_str()) {
                continue;
            }
            let name = disk::split(&n.path).1;
            let mime = if n.mime.is_empty() || n.mime == "application/octet-stream" {
                disk::guess_mime(name, &[])
            } else {
                n.mime.clone()
            };
            fresh.push(found_entry(&n.path, n.size, n.modified_us, n.hash.clone(), mime, now));
        }
        stack_new_files(&mut fresh, &index);
        report.added = fresh.len() as u32;
        to_probe.extend(fresh.iter().map(|f| f.id));
        batch.extend(fresh);

        if !batch.is_empty() {
            self.with_files(|s| s.put_files(&batch))?;
            for f in &batch {
                self.wrote(RecordKind::File, f.id);
            }
        }
        Ok(AppliedScan { report, to_probe })
    }
}
