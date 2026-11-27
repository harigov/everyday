//! Albums: named, ordered sets of files.

use super::MAX_ALBUM_NAME;
use super::Vault;
use super::helpers::{check_batch, unique};
use crate::error::{Error, Result};
use crate::files::{Album, AlbumView, MAX_CAPTION_LEN};
use crate::id::{AlbumId, FileId};
use crate::record::RecordKind;
use jiff::Timestamp;

impl Vault {
    /// Every album, by name, with its count and cover.
    pub fn albums(&self) -> Result<Vec<AlbumView>> {
        self.with_files(|s| {
            let counts = s.album_counts()?;
            let mut views = Vec::new();
            for album in s.list_albums()? {
                let count = counts.get(&album.id).copied().unwrap_or(0);
                let mut cover = None;
                if count > 0 {
                    let members = s.album_files(album.id)?;
                    // The cover somebody chose, if it is still in the album
                    // and not in the trash; else the first file that is.
                    let chosen = album.cover.filter(|c| members.contains(c));
                    for id in chosen.into_iter().chain(members.iter().copied()) {
                        match s.get_file(id) {
                            Ok(f) if !f.status.is_trashed() => {
                                cover = Some(f);
                                break;
                            }
                            Ok(_) | Err(Error::NotFound { .. }) => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
                views.push(AlbumView { album, count, cover });
            }
            views.sort_by_key(|v| v.album.name.to_lowercase());
            Ok(views)
        })
    }

    /// Create or rename. A name is 1 to 200 characters.
    pub fn save_album(&self, album: &Album) -> Result<Album> {
        self.writable()?;
        let mut album = album.clone();
        album.name = album.name.trim().to_string();
        let length = album.name.chars().count();
        if length == 0 || length > MAX_ALBUM_NAME {
            return Err(Error::Invalid(format!(
                "an album's name is 1 to {MAX_ALBUM_NAME} characters"
            )));
        }
        if album.description.chars().count() > MAX_CAPTION_LEN {
            return Err(Error::Invalid(format!(
                "an album's description is at most {MAX_CAPTION_LEN} characters"
            )));
        }
        let now = Timestamp::now();
        let id = album.id;
        let stored = self.with_files(|s| {
            // An album that exists keeps when it was made, whatever the
            // caller says.
            match s.get_album(id) {
                Ok(existing) => album.created_at = existing.created_at,
                Err(Error::NotFound { .. }) => album.created_at = now,
                Err(e) => return Err(e),
            }
            album.updated_at = now;
            s.put_album(&album)?;
            Ok(album.clone())
        })?;
        self.wrote(RecordKind::Album, id);
        Ok(stored)
    }

    pub fn delete_album(&self, id: AlbumId) -> Result<()> {
        self.writable()?;
        self.with_files(|s| s.delete_album(id))?;
        self.wrote(RecordKind::Album, id);
        Ok(())
    }

    pub fn add_to_album(&self, id: AlbumId, files: &[FileId]) -> Result<()> {
        self.writable()?;
        check_batch(files.len())?;
        let files = unique(files);
        self.with_files(|s| {
            s.get_album(id)?;
            let found = s.get_files(&files)?;
            if let Some(missing) = files.iter().find(|f| !found.iter().any(|x| x.id == **f)) {
                return Err(Error::not_found("file", missing));
            }
            s.add_to_album(id, &files)
        })?;
        self.wrote(RecordKind::Album, id);
        Ok(())
    }

    pub fn remove_from_album(&self, id: AlbumId, files: &[FileId]) -> Result<()> {
        self.writable()?;
        check_batch(files.len())?;
        self.with_files(|s| s.remove_from_album(id, files))?;
        self.wrote(RecordKind::Album, id);
        Ok(())
    }
}
