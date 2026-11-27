//! Which records use which files, kept in step with the records that hold them.

use super::Vault;
use crate::error::{Error, Result};
use crate::files::{RefKind, RefOwner};
use crate::id::FileId;
use crate::store::agent::ConversationQuery;
use crate::store::library::ItemQuery;
use jiff::Timestamp;
use std::collections::{BTreeSet, HashSet};

impl Vault {
    /// Record that `owner` refers to exactly `files`. Quiet when the
    /// backend keeps no catalog, so a save of an entry never fails for it.
    ///
    /// Only files the catalog holds are recorded: a body can still name a
    /// file that was deleted, and a reference to nothing says nothing.
    pub fn set_file_refs(&self, owner: &RefOwner, files: &[FileId]) -> Result<()> {
        self.writable()?;
        let recorded = self.with_files(|s| {
            let held: Vec<FileId> = if files.is_empty() {
                Vec::new()
            } else {
                s.get_files(files)?.into_iter().map(|f| f.id).collect()
            };
            s.set_refs(owner, &held)
        });
        match recorded {
            Err(Error::Unsupported(_)) => Ok(()),
            other => other,
        }
    }

    /// [`Vault::set_file_refs`] for the save of a record, which must never
    /// fail for it: a failure is logged and the next save tries again, and
    /// [`Vault::rebuild_file_refs`] is there for a table that has drifted.
    pub(in crate::vault) fn track_file_refs(
        &self,
        owner: RefOwner,
        files: impl IntoIterator<Item = FileId>,
    ) {
        let files: Vec<FileId> = files.into_iter().collect();
        if let Err(e) = self.set_file_refs(&owner, &files) {
            tracing::warn!(
                error = %e,
                kind = owner.kind.as_str(),
                id = %owner.id,
                "could not record which files a record refers to"
            );
        }
    }

    /// A record is gone, and with it everything it referred to.
    pub(in crate::vault) fn drop_file_refs(&self, owner: RefOwner) {
        self.track_file_refs(owner, std::iter::empty());
    }

    /// Recompute the whole references table from every entry, note,
    /// assistant message, health document, meal and library item. Answers
    /// how many references there are.
    pub fn rebuild_file_refs(&self) -> Result<u64> {
        self.writable()?;
        // The exclusive lock, not the shared one: a save landing between
        // reading the records and writing the table would have its own
        // references overwritten with the older ones.
        self.write(|u| {
            let files = match u.files() {
                Ok(files) => files,
                Err(Error::Unsupported(_)) => return Ok(0),
                Err(e) => return Err(e),
            };
            let mut refs: Vec<(RefOwner, FileId)> = Vec::new();
            for entry in u.store.all_entries()? {
                for file in entry.body.file_refs() {
                    refs.push((RefOwner::new(RefKind::Entry, entry.id), file));
                }
            }
            if let Some(notes) = u.store.notes() {
                for note in notes.all_notes()? {
                    for file in note.body.file_refs() {
                        refs.push((RefOwner::new(RefKind::Note, note.id), file));
                    }
                }
            }
            if let Some(agent) = u.store.agent() {
                for conversation in agent.list_conversations(&ConversationQuery::default())? {
                    for message in agent.list_messages(conversation.id)? {
                        for file in &message.files {
                            refs.push((RefOwner::new(RefKind::Message, message.id), *file));
                        }
                    }
                }
            }
            if let Some(health) = u.store.health() {
                for document in health.list_documents()? {
                    for page in &document.files {
                        refs.push((RefOwner::new(RefKind::HealthDocument, document.id), page.file));
                    }
                }
                for meal in health.meals(Timestamp::MIN, Timestamp::MAX)? {
                    if let Some(photo) = meal.photo {
                        refs.push((RefOwner::new(RefKind::Meal, meal.id), photo));
                    }
                }
            }
            if let Some(library) = u.store.library() {
                for item in library.list_items(&ItemQuery::default())? {
                    for file in &item.files {
                        refs.push((RefOwner::new(RefKind::Item, item.id), *file));
                    }
                }
            }
            if let Some(news) = u.store.news() {
                match news.stories_with_files() {
                    Ok(stories) => {
                        for story in stories {
                            for file in story.files.all() {
                                refs.push((RefOwner::new(RefKind::Story, story.id), file));
                            }
                        }
                    }
                    Err(Error::Unsupported(_)) => {}
                    Err(e) => return Err(e),
                }
            }
            // Only files the catalog holds, as `set_file_refs` keeps.
            let distinct: Vec<FileId> =
                refs.iter().map(|(_, f)| *f).collect::<BTreeSet<_>>().into_iter().collect();
            let mut held: HashSet<FileId> = HashSet::with_capacity(distinct.len());
            for chunk in distinct.chunks(200) {
                held.extend(files.get_files(chunk)?.into_iter().map(|f| f.id));
            }
            refs.retain(|(_, f)| held.contains(f));
            files.replace_all_refs(&refs)?;
            Ok(refs.len() as u64)
        })
    }
}
