//! Reading the catalog: lists, searches, facets, usage, and the bytes of a file.

use super::HEAD_LEN;
use super::Vault;
use super::helpers::{
    Tally, camera_of, is_texty, is_unused, meal_title, not_there, same, stack_counts, wanted,
    words_match,
};
use crate::error::{Error, Result};
use crate::files::disk;
use crate::files::{
    Album, Facet, FileEntry, FileFilter, FilePage, FileQuery, FileStorage, FileUse, FilesFacets,
    FilesUsage, FolderInfo, FolderListing, RefKind, RefOwner, StatusFilter,
};
use crate::id::{EntryId, FileId, HealthDocumentId, ItemId, MealId, NoteId, StoryId};
use crate::store::agent::ConversationQuery;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

impl Vault {
    pub fn file(&self, id: FileId) -> Result<FileEntry> {
        self.with_files(|s| s.get_file(id))
    }

    /// A page of files. Store-side filtering where the query allows it; the
    /// sealed fields (`text`, `tag`, `person`, `place`) and `unused` are
    /// applied here, after opening, with paging after that.
    ///
    /// `tag`, `person` and `place` are exact names, compared without regard
    /// to case -- what a facet in the sidebar hands back. `text` is the
    /// loose one: every word must be somewhere in the file.
    pub fn files(&self, query: &FileQuery) -> Result<FilePage> {
        let words: Vec<String> = query
            .text
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let tag = wanted(query.tag.as_deref());
        let person = wanted(query.person.as_deref());
        let place = wanted(query.place.as_deref());
        if words.is_empty() && tag.is_none() && person.is_none() && place.is_none() && !query.unused
        {
            return self.with_files(|s| {
                let files = s.query_files(&query.filter)?;
                Ok(FilePage {
                    stack_counts: stack_counts(s, &files)?,
                    total: s.count_files(&query.filter)?,
                    files,
                })
            });
        }

        // Everything the store cannot narrow is decided after opening, so
        // the store is asked for every candidate and paged here.
        let mut filter = query.filter.clone();
        let (limit, offset) = (filter.limit.take(), filter.offset as usize);
        filter.offset = 0;
        let (rows, referenced) = self.with_files(|s| {
            let rows = s.query_files(&filter)?;
            let referenced = if query.unused { s.referenced_files()? } else { BTreeSet::new() };
            Ok((rows, referenced))
        })?;
        let mut kept: Vec<FileEntry> = rows
            .into_iter()
            .filter(|f| {
                words_match(f, &words)
                    && tag.as_deref().is_none_or(|t| f.tags.iter().any(|x| same(x, t)))
                    && person.as_deref().is_none_or(|p| f.people.iter().any(|x| same(x, p)))
                    && place
                        .as_deref()
                        .is_none_or(|p| f.place.as_deref().is_some_and(|x| same(x, p)))
                    && (!query.unused || is_unused(f, &referenced))
            })
            .collect();
        let total = kept.len() as u64;
        kept.drain(..offset.min(kept.len()));
        if let Some(limit) = limit {
            kept.truncate(limit as usize);
        }
        let stack_counts = self.with_files(|s| stack_counts(s, &kept))?;
        Ok(FilePage { files: kept, total, stack_counts })
    }

    /// One folder: its subfolders from the disk, its files from the catalog.
    ///
    /// Files marked hidden are listed: this is the folder as it is, and a
    /// file that is in it should not be missing from it. When the folder is
    /// not available the listing says so in `state` and is empty.
    pub fn list_folder(&self, dir: &str) -> Result<FolderListing> {
        let dir = disk::validate_rel(dir)?;
        let state = self.folder_state()?;
        if !state.is_available() {
            return Ok(FolderListing { dir, state, folders: Vec::new(), files: Vec::new() });
        }
        let folder = self.available_folder()?;
        let folders = folder
            .list_dir(&dir)?
            .into_iter()
            .filter(|item| item.is_dir)
            .map(|item| FolderInfo {
                name: item.name,
                path: item.path,
                items: item.items,
                modified_at: Timestamp::from_microsecond(item.modified_us).ok(),
            })
            .collect();
        let filter = FileFilter {
            dir: Some(dir.clone()),
            status: StatusFilter::Live,
            include_hidden: true,
            sort: crate::files::FileSort::Name,
            ..Default::default()
        };
        let files = self.with_files(|s| s.query_files(&filter))?;
        Ok(FolderListing { dir, state, folders, files })
    }

    /// The records that refer to a file, each named for the person. Newest
    /// first. A record that no longer exists is left out: the references
    /// table is kept up by saves, and a record removed some other way leaves
    /// a row behind until the next rebuild.
    ///
    /// A message is named by its conversation, found by looking through the
    /// conversations newest first until every message is placed -- the
    /// store has no way to ask which conversation a message is in.
    pub fn file_uses(&self, id: FileId) -> Result<Vec<FileUse>> {
        self.read(|u| {
            let files = u.files()?;
            let mut uses = Vec::new();
            let mut messages: BTreeMap<String, RefOwner> = BTreeMap::new();
            for owner in files.refs_to(id)? {
                match owner.kind {
                    RefKind::Entry => {
                        let Ok(entry_id) = EntryId::parse(&owner.id) else { continue };
                        match u.store.get_entry(entry_id) {
                            Ok(e) => uses.push(FileUse {
                                owner,
                                title: e.display_title(),
                                when: Some(e.created_at),
                                parent: None,
                            }),
                            Err(Error::NotFound { .. }) => {}
                            Err(e) => return Err(e),
                        }
                    }
                    RefKind::Note => {
                        let (Ok(note_id), Some(notes)) =
                            (NoteId::parse(&owner.id), u.store.notes())
                        else {
                            continue;
                        };
                        match notes.get_note(note_id) {
                            Ok(n) => uses.push(FileUse {
                                owner,
                                title: n.display_title(),
                                when: Some(n.created_at),
                                parent: None,
                            }),
                            Err(Error::NotFound { .. }) => {}
                            Err(e) => return Err(e),
                        }
                    }
                    RefKind::Message => {
                        messages.insert(owner.id.clone(), owner);
                    }
                    RefKind::HealthDocument => {
                        let (Ok(document_id), Some(health)) =
                            (HealthDocumentId::parse(&owner.id), u.store.health())
                        else {
                            continue;
                        };
                        match health.get_document(document_id) {
                            Ok(d) => uses.push(FileUse {
                                owner,
                                title: match d.title.trim() {
                                    "" => "Untitled document".to_string(),
                                    t => t.to_string(),
                                },
                                when: Some(d.created_at),
                                parent: None,
                            }),
                            Err(Error::NotFound { .. }) => {}
                            Err(e) => return Err(e),
                        }
                    }
                    RefKind::Meal => {
                        let (Ok(meal_id), Some(health)) =
                            (MealId::parse(&owner.id), u.store.health())
                        else {
                            continue;
                        };
                        match health.get_meal(meal_id) {
                            Ok(m) => uses.push(FileUse {
                                owner,
                                title: meal_title(&m),
                                when: Some(m.at),
                                parent: None,
                            }),
                            Err(Error::NotFound { .. }) => {}
                            Err(e) => return Err(e),
                        }
                    }
                    RefKind::Story => {
                        let (Ok(story_id), Some(news)) =
                            (StoryId::parse(&owner.id), u.store.news())
                        else {
                            continue;
                        };
                        match news.get_story(story_id) {
                            // Its source's id rides along so the interface
                            // can open the story where it is in the Feed.
                            Ok(st) => uses.push(FileUse {
                                owner,
                                title: match st.title.trim() {
                                    "" => "Untitled story".to_string(),
                                    t => t.to_string(),
                                },
                                when: Some(st.published_at),
                                parent: Some(st.source_id.to_string()),
                            }),
                            Err(Error::NotFound { .. }) => {}
                            Err(e) => return Err(e),
                        }
                    }
                    RefKind::Item => {
                        let (Ok(item_id), Some(library)) =
                            (ItemId::parse(&owner.id), u.store.library())
                        else {
                            continue;
                        };
                        match library.get_item(item_id) {
                            // Its shelf's id rides along so the interface can
                            // open the shelf the item is on.
                            Ok(i) => uses.push(FileUse {
                                owner,
                                title: match i.title.trim() {
                                    "" => "Untitled item".to_string(),
                                    t => t.to_string(),
                                },
                                when: Some(i.created_at),
                                parent: Some(i.kind_id.to_string()),
                            }),
                            Err(Error::NotFound { .. }) => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
            }
            if !messages.is_empty()
                && let Some(agent) = u.store.agent()
            {
                for conversation in agent.list_conversations(&ConversationQuery::default())? {
                    if messages.is_empty() {
                        break;
                    }
                    for m in agent.list_messages(conversation.id)? {
                        if let Some(owner) = messages.remove(&m.id.to_string()) {
                            let title = match conversation.title.trim() {
                                "" => "Untitled conversation".to_string(),
                                t => t.to_string(),
                            };
                            uses.push(FileUse {
                                owner,
                                title,
                                when: Some(m.created_at),
                                parent: Some(conversation.id.to_string()),
                            });
                        }
                    }
                }
            }
            uses.sort_by_key(|u| std::cmp::Reverse(u.when));
            Ok(uses)
        })
    }

    /// The albums a file is in, by name.
    pub fn file_albums(&self, id: FileId) -> Result<Vec<Album>> {
        self.with_files(|s| {
            let mut albums = Vec::new();
            for album in s.albums_of(id)? {
                match s.get_album(album) {
                    Ok(a) => albums.push(a),
                    Err(Error::NotFound { .. }) => {}
                    Err(e) => return Err(e),
                }
            }
            albums.sort_by_key(|a| a.name.to_lowercase());
            Ok(albums)
        })
    }

    pub fn files_usage(&self) -> Result<FilesUsage> {
        self.with_files(|s| s.files_usage())
    }

    /// Tags, people, places, years and cameras over every live file that is
    /// not hidden, most used first (years newest first).
    ///
    /// Names that differ only in case are one facet, spelled as the first
    /// file that carried it spelled it: a file's own tags are deduplicated
    /// that way already, and the sidebar should not list "Travel" and
    /// "travel" apart.
    pub fn files_facets(&self) -> Result<FilesFacets> {
        let filter = FileFilter { status: StatusFilter::Live, ..Default::default() };
        let files = self.with_files(|s| s.query_files(&filter))?;
        let zone = TimeZone::system();
        let mut tags = Tally::default();
        let mut people = Tally::default();
        let mut places = Tally::default();
        let mut cameras = Tally::default();
        let mut years: BTreeMap<i16, u64> = BTreeMap::new();
        for f in &files {
            f.tags.iter().for_each(|t| tags.add(t));
            f.people.iter().for_each(|p| people.add(p));
            if let Some(place) = &f.place {
                places.add(place);
            }
            if let Some(camera) = camera_of(&f.media) {
                cameras.add(&camera);
            }
            *years.entry(f.when().to_zoned(zone.clone()).year()).or_default() += 1;
        }
        Ok(FilesFacets {
            tags: tags.into_facets(),
            people: people.into_facets(),
            places: places.into_facets(),
            years: years
                .into_iter()
                .rev()
                .map(|(year, count)| Facet { name: year.to_string(), count })
                .collect(),
            cameras: cameras.into_facets(),
        })
    }

    /// A file's length in bytes, from the disk for a plain file.
    pub fn file_len(&self, id: FileId) -> Result<u64> {
        let entry = self.file(id)?;
        match &entry.storage {
            FileStorage::Sealed { blob } => self.read(|u| u.store.blob_len(*blob)),
            FileStorage::Plain { path } => {
                let folder = self.available_folder()?;
                match folder.stat(path)? {
                    Some(on_disk) => Ok(on_disk.size),
                    None => Err(not_there(&entry)),
                }
            }
        }
    }

    /// Up to `len` bytes from `start`, plain or sealed alike.
    pub fn read_file_range(&self, id: FileId, start: u64, len: u64) -> Result<Vec<u8>> {
        let entry = self.file(id)?;
        match &entry.storage {
            FileStorage::Sealed { blob } => {
                self.read(|u| u.store.get_blob_range(*blob, start, len))
            }
            FileStorage::Plain { path } => self.available_folder()?.read_range(path, start, len),
        }
    }

    /// A text file's contents, decoded as UTF-8 (lossily), at most
    /// `max_bytes` of it. Refuses what is not text (by MIME type and by a
    /// NUL in the first 8 KiB).
    pub fn read_file_text(&self, id: FileId, max_bytes: usize) -> Result<String> {
        let entry = self.file(id)?;
        if !is_texty(&entry.mime, &entry.name) {
            return Err(Error::Invalid(format!("{} is not a text file", entry.name)));
        }
        let bytes = self.read_file_range(id, 0, max_bytes as u64)?;
        if bytes[..bytes.len().min(HEAD_LEN as usize)].contains(&0) {
            return Err(Error::Invalid(format!("{} is not a text file", entry.name)));
        }
        // A cut in the middle of a character is the cut's doing, not the
        // file's: stop before it rather than ending in a replacement mark.
        let end = match std::str::from_utf8(&bytes) {
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            _ => bytes.len(),
        };
        Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
    }

    /// A plain file's absolute path, checked to be inside the folder. For
    /// reading metadata, opening it in another program, showing it in a
    /// file manager. Refuses a sealed file.
    pub fn plain_path(&self, id: FileId) -> Result<PathBuf> {
        let entry = self.file(id)?;
        match &entry.storage {
            FileStorage::Sealed { .. } => Err(Error::Invalid(format!(
                "{} is sealed in the vault and has no place in the folder",
                entry.name
            ))),
            FileStorage::Plain { path } => self.available_folder()?.resolve(path),
        }
    }

    /// Whether `version` -- what the interface versions a file's address by:
    /// its hash, or its modified time while it has none -- is this file as it
    /// is now. True only when the catalog says that version *and*, for a
    /// plain file, the disk still has the size and the time the catalog
    /// recorded, so nothing has changed the bytes since the catalog last
    /// looked. What lets a webview keep the bytes behind an address for good:
    /// a file edited on disk before the reconciler's next pass must not be
    /// cached under the version it had.
    pub fn file_version_is_current(&self, id: FileId, version: &str) -> Result<bool> {
        let entry = self.file(id)?;
        let named = if entry.hash.is_empty() {
            version == entry.modified_at.to_string()
        } else {
            version == entry.hash
        };
        if !named {
            return Ok(false);
        }
        let Some(path) = entry.path() else { return Ok(true) };
        let Some(folder) = self.files_folder()? else { return Ok(false) };
        Ok(folder.stat(path)?.is_some_and(|disk| {
            disk.size == entry.size && disk.modified_us == entry.modified_at.as_microsecond()
        }))
    }

    /// The thumbnail's JPEG, if one has been drawn.
    pub fn file_thumbnail(&self, id: FileId) -> Result<Option<Vec<u8>>> {
        match self.file(id)?.thumbnail {
            Some(blob) => self.read(|u| u.store.get_blob(blob)).map(Some),
            None => Ok(None),
        }
    }
}
