//! What the archive says about itself.
//!
//! Two files at the root: `everyday.json`, which this application reads on the
//! way back in, and `README.md`, which a person reads. Both are generated from
//! the same [`Spec`]s the settings dialog is drawn from, so an app cannot
//! describe itself one way in the chooser and another way in the archive.
//!
//! The manifest is a convenience and never a requirement. [`inspect`] believes
//! the folders that are actually present over anything the manifest claims, so
//! an archive somebody has unzipped, edited and rezipped -- or a folder of
//! Markdown that was never an export at all -- imports on the strength of its
//! layout. A format that only reads its own output is a format you are still
//! trapped in.
//!
//! [`inspect`]: crate::inspect
//! [`Spec`]: crate::Spec

use crate::{PARTS, Spec};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

/// The format this crate writes. Bumped when an existing part's layout
/// changes in a way an older build would misread -- not when a part is added,
/// which older builds already handle by not recognising the folder.
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// So that a person who finds this file in five years knows what made it.
    pub application: String,
    pub format_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exported_at: Option<Timestamp>,
    /// The vault's name, which is a label rather than an identity: importing
    /// does not check it, because moving a journal into a differently named
    /// vault is the ordinary reason to do this.
    #[serde(default)]
    pub vault: String,
    #[serde(default)]
    pub media: bool,
    #[serde(default)]
    pub parts: Vec<PartEntry>,
}

/// One app's contribution, as the archive found it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartEntry {
    pub id: String,
    pub label: String,
    pub format: String,
    /// Records written. Zero when the archive did not say and the folder was
    /// found by looking, which is why the interface shows files as well.
    #[serde(default)]
    pub records: u64,
    #[serde(default)]
    pub files: u64,
    #[serde(default)]
    pub bytes: u64,
    /// Whether this build can read this part back. Filled in on inspection
    /// rather than stored, since it is a fact about the program and not about
    /// the archive.
    #[serde(default)]
    pub imports: bool,
}

impl Manifest {
    pub fn new(vault: &str, at: Timestamp, media: bool) -> Self {
        Self {
            application: "Every Day".into(),
            format_version: FORMAT_VERSION,
            exported_at: Some(at),
            vault: vault.to_string(),
            media,
            parts: Vec::new(),
        }
    }

    /// A manifest for an archive that has none: everything unknown, so that
    /// what is found by looking is all there is.
    pub fn unstated() -> Self {
        Self {
            application: String::new(),
            format_version: FORMAT_VERSION,
            exported_at: None,
            vault: String::new(),
            media: false,
            parts: Vec::new(),
        }
    }

    pub fn add(&mut self, spec: &Spec, records: u64, files: u64, bytes: u64) {
        self.parts.push(PartEntry {
            id: spec.id.into(),
            label: spec.label.into(),
            format: spec.format.into(),
            records,
            files,
            bytes,
            imports: spec.imports,
        });
    }

    pub fn records(&self) -> u64 {
        self.parts.iter().map(|p| p.records).sum()
    }

    pub fn files(&self) -> u64 {
        self.parts.iter().map(|p| p.files).sum()
    }

    pub fn render(&self) -> String {
        // Pretty, with a trailing newline: this file is meant to be opened.
        format!("{}\n", serde_json::to_string_pretty(self).unwrap_or_default())
    }

    /// A file name for the archive: the vault, the date, and nothing else.
    pub fn filename(&self) -> String {
        let day = self
            .exported_at
            .map(|at| at.to_zoned(jiff::tz::TimeZone::system()).date().to_string())
            .unwrap_or_default();
        let vault = crate::text::safe_name(&self.vault);
        let stem = [vault.as_str(), day.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        if stem.is_empty() { "Every Day export.zip".into() } else { format!("{stem}.zip") }
    }

    /// The note to whoever opens this in five years.
    ///
    /// Written in the archive rather than only in this repository's README,
    /// because the person who needs it is holding a zip file and may well not
    /// have the application any more -- which is the situation an export
    /// exists for.
    pub fn readme(&self) -> String {
        let mut out = String::from("# Your Every Day data\n\n");
        if let Some(at) = self.exported_at {
            let _ = writeln!(
                out,
                "Exported from **{}** on {}.\n",
                if self.vault.is_empty() { "a vault" } else { &self.vault },
                at.to_zoned(jiff::tz::TimeZone::system()).strftime("%-d %B %Y")
            );
        }
        out.push_str(
            "Everything here is a plain file in a format other programs read. Nothing \
             needs Every Day to open it, and nothing is encrypted: keep this archive \
             somewhere you would keep the diary itself.\n\n",
        );

        out.push_str("## What is in here\n\n");
        for part in &self.parts {
            let _ = writeln!(out, "### `{}/` — {}\n", part.id, part.label);
            if let Some(spec) = PARTS.iter().map(|p| p.spec()).find(|s| s.id == part.id) {
                let _ = writeln!(out, "{}\n", spec.summary);
            }
            let _ = writeln!(out, "*{}*", part.format);
            if part.records > 0 {
                let _ = writeln!(
                    out,
                    " — {} record{} in {} file{}.",
                    part.records,
                    plural(part.records),
                    part.files,
                    plural(part.files)
                );
            }
            out.push('\n');
        }

        out.push_str(
            "## Getting it back\n\n\
             Open Every Day, go to **Settings → Data**, and choose **Import**. \
             Pick this file, tick what you want, and it will tell you what it is \
             about to do before it does any of it.\n\n\
             You can edit the files first. Every record keeps its `id` in the front \
             matter of its file, and that id is what an import matches on: leave it \
             alone and your edit lands on the record it came from, delete it and the \
             record arrives as a new one.\n\n",
        );
        if !self.media {
            out.push_str(
                "> This export was made **without attachments**, so photographs, \
                 video and cover art are not here. The words are.\n",
            );
        }
        out
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_round_trips() {
        let mut m = Manifest::new("Personal", Timestamp::now(), true);
        m.add(crate::PARTS[0].spec(), 12, 14, 900);
        let back: Manifest = serde_json::from_str(&m.render()).unwrap();
        assert_eq!(back.parts.len(), 1);
        assert_eq!(back.records(), 12);
        assert_eq!(back.vault, "Personal");
    }

    #[test]
    fn the_readme_names_every_folder_it_shipped() {
        let mut m = Manifest::new("Personal", Timestamp::now(), false);
        for part in PARTS {
            m.add(part.spec(), 1, 1, 1);
        }
        let readme = m.readme();
        for part in PARTS {
            assert!(readme.contains(&format!("`{}/`", part.spec().id)), "{}", part.spec().id);
        }
        assert!(readme.contains("without attachments"));
    }

    #[test]
    fn the_file_is_named_after_the_vault_and_the_day() {
        let m = Manifest::new("Work / Home", "2026-09-10T12:00:00Z".parse().unwrap(), true);
        let name = m.filename();
        assert!(name.starts_with("Work-Home "), "{name}");
        assert!(name.ends_with(".zip"), "{name}");
    }
}
