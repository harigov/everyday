//! Templates, one Markdown page each.
//!
//! ```text
//!   templates/
//!     Weekly-review-9f2a1c0d.md
//! ```
//!
//! The page below the front matter is the template's own text, exactly as it
//! is kept -- placeholders and all -- not a rendering of it, so a template
//! edited in a text editor and imported back is the template that was
//! edited. Where it is offered, its title and its tags are in the front
//! matter.

use crate::text::{FrontMatter, safe_name, split_front_matter};
use crate::{Files, Mode, Options, Part, Portable, Report, Spec, land};
use karmayoga_core::store::JournalStore;
use karmayoga_core::template::{Template, TemplateUse};
use karmayoga_core::{Result, TemplateId};

pub struct TemplatesPart;
pub static TEMPLATES: TemplatesPart = TemplatesPart;

static SPEC: Spec = Spec {
    id: "templates",
    label: "Templates",
    summary: "The shapes your journal entries, notes and meeting notes start from, with \
              where each is offered. One Markdown page per template.",
    format: "Markdown with YAML front matter, one file per template",
    media: false,
    imports: true,
};

fn use_word(u: TemplateUse) -> &'static str {
    match u {
        TemplateUse::Journal => "journal",
        TemplateUse::Notes => "notes",
        TemplateUse::Meetings => "meetings",
    }
}

fn parse_use(word: &str) -> Option<TemplateUse> {
    TemplateUse::ALL.into_iter().find(|u| use_word(*u).eq_ignore_ascii_case(word.trim()))
}

impl Portable for TemplatesPart {
    fn spec(&self) -> &'static Spec {
        &SPEC
    }

    fn tally(&self, store: &dyn JournalStore) -> Result<Option<u64>> {
        let Some(notes) = store.notes() else { return Ok(None) };
        Ok(Some(notes.list_templates()?.len() as u64))
    }

    fn export(&self, store: &dyn JournalStore, out: &mut Files<'_>, _opts: &Options) -> Result<()> {
        let Some(notes) = store.notes() else { return Ok(()) };
        for template in notes.list_templates()? {
            let uses: Vec<String> =
                template.uses.iter().map(|u| use_word(*u).to_string()).collect();
            let mut front = FrontMatter::new();
            front
                .always("template", &template.name)
                .always("id", template.id.to_string())
                .list("for", &uses)
                .set("title", &template.title)
                .list("tags", &template.tags)
                .set("created", super::doc::stamp(template.created_at))
                .set("updated", super::doc::stamp(template.updated_at));
            let body = format!("{}{}\n", front.render(), template.body.trim_end());
            let name = format!("{}-{}.md", safe_name(&template.name), template.id.short());
            out.records(&name, body, 1)?;
        }
        Ok(())
    }

    fn import(&self, store: &dyn JournalStore, src: &Part<'_>, mode: Mode) -> Result<Report> {
        let mut report = Report::new(SPEC.id);
        let Some(notes) = store.notes() else {
            report.problem(SPEC.label, "this vault's backend does not store templates");
            return Ok(report);
        };
        let existing = notes.list_templates()?;

        for (name, source) in src.documents(".md") {
            let (fields, body) = split_front_matter(source);
            let title = fields.text("template");
            if title.trim().is_empty() {
                report.problem(name, "no template name in the front matter");
                continue;
            }
            let id = fields.parse::<TemplateId>("id").unwrap_or_else(TemplateId::new);
            let current = existing.iter().find(|t| t.id == id).cloned();
            let mut landing = land(current, || Template::new(&title), mode);
            let Some(template) = landing.as_mut() else {
                report.landed(&landing);
                continue;
            };
            template.id = id;
            template.name = title;
            template.uses = fields.list("for").iter().filter_map(|w| parse_use(w)).collect();
            template.title = fields.text("title");
            template.tags = fields.list("tags");
            template.body = body.trim_start_matches('\n').to_string();
            if let Some(created) = fields.parse("created") {
                template.created_at = created;
            }
            template.updated_at = jiff::Timestamp::now();
            if let Err(e) = template.validate() {
                report.problem(name, e);
                continue;
            }
            notes.put_template(template)?;
            report.landed(&landing);
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_use_has_a_word_that_reads_back() {
        for u in TemplateUse::ALL {
            assert_eq!(parse_use(use_word(u)), Some(u));
        }
        assert_eq!(parse_use(" Meetings "), Some(TemplateUse::Meetings));
        assert_eq!(parse_use("diary"), None);
    }
}
