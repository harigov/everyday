//! Templates: the shapes an entry, a note or a meeting's notes start from.
//!
//! Stored beside notes -- see `crate::store::notes`'s module docs -- so a
//! backend that holds notes holds these. What is worth reading here is the
//! read path: [`Vault::templates`] moves the meeting settings' own templates
//! into the table the first time anything asks (see
//! [`Vault::migrate_meeting_templates`]), so every reader -- the Templates
//! list, a new entry, a recording being written up -- sees one list.

use super::Vault;
use super::session::Domain;
use crate::error::{Error, Result};
use crate::id::TemplateId;
use crate::model::{Entry, Journal};
use crate::record::RecordKind;
use crate::store::notes::NoteStore;
use crate::template::{self, Template};
use jiff::civil::Date;

impl Vault {
    /// Does this vault's backend hold templates? Exactly when it holds notes.
    pub fn supports_templates(&self) -> bool {
        self.supports_notes()
    }

    pub(super) fn with_templates<T>(
        &self,
        f: impl FnOnce(&dyn NoteStore) -> Result<T>,
    ) -> Result<T> {
        self.with_domain(Domain::Notes, |s| s.notes().map(f))
    }

    /// Every template, oldest first.
    ///
    /// The meeting settings' templates are moved in first if they have not
    /// been yet. A vault that cannot be written to -- another process holds
    /// it -- cannot move them, and lists them anyway, merged in for the
    /// reading. The built-in starter is brought up to date if it was never
    /// edited, as it always was.
    pub fn templates(&self) -> Result<Vec<Template>> {
        if let Err(e) = self.migrate_meeting_templates() {
            // Not fatal: what is lost is the old list, not this one.
            tracing::warn!(error = %e, "could not move the meeting templates");
        }
        let mut all = self.with_templates(|n| n.list_templates())?;
        if !self.is_writable() {
            for legacy in self.unmoved_meeting_templates()? {
                if !all.iter().any(|t| t.id == legacy.id) {
                    all.push(legacy);
                }
            }
        }
        crate::meeting::template::upgrade_starter(&mut all);
        Ok(all)
    }

    pub fn template(&self, id: TemplateId) -> Result<Template> {
        self.templates()?
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| Error::not_found("template", id))
    }

    pub fn save_template(&self, template: &Template) -> Result<()> {
        self.writable()?;
        template.validate()?;
        // Moved first, so a template saved into a vault that has not yet
        // moved its meeting templates cannot be followed by a move that
        // writes an older copy of the same id over it.
        self.migrate_meeting_templates()?;
        self.with_templates(|n| n.put_template(template))?;
        self.wrote(RecordKind::Template, template.id);
        Ok(())
    }

    /// Delete a template. Nothing that named it is rewritten: a journal's
    /// rule, the meetings' default and a recording's choice all pass over an
    /// id that is gone -- see [`template::JournalTemplates`] and
    /// [`template::for_meeting`].
    pub fn delete_template(&self, id: TemplateId) -> Result<()> {
        self.writable()?;
        self.migrate_meeting_templates()?;
        self.with_templates(|n| n.delete_template(id))?;
        self.wrote(RecordKind::Template, id);
        Ok(())
    }

    /// The template a meeting is written up from. See
    /// [`template::for_meeting`]. Never fails: a templates table or settings
    /// row that cannot be read is logged and passed over, and the built-in
    /// starter answers -- a recording must always have a shape to be written
    /// up in, as it did when templates lived in the settings.
    pub fn meeting_template(&self, asked: Option<TemplateId>) -> Template {
        let default = match self.meeting_settings() {
            Ok(settings) => settings.default_template,
            Err(e) => {
                tracing::warn!(error = %e, "could not read the meetings' default template");
                None
            }
        };
        let all = self.templates().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "could not read the templates; using the starter");
            Vec::new()
        });
        template::for_meeting(&all, asked, default)
    }

    /// The template `journal` starts an entry dated `date` from, if any.
    pub fn journal_template(&self, journal: &Journal, date: Date) -> Result<Option<Template>> {
        if journal.templates.is_empty() || !self.supports_templates() {
            return Ok(None);
        }
        Ok(journal.templates.pick(date, &self.templates()?).cloned())
    }

    /// Fill a new, unsaved entry from its journal's template for its date.
    /// Returns whether there was one. Nothing is written.
    pub fn start_entry_from_template(&self, entry: &mut Entry) -> Result<bool> {
        let journal = self.journal(entry.journal_id)?;
        let Some(template) = self.journal_template(&journal, entry.local_date)? else {
            return Ok(false);
        };
        template::start_entry(entry, &template, &journal);
        Ok(true)
    }
}
