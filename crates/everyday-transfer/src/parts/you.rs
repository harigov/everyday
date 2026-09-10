//! Who the vault belongs to.
//!
//! ```text
//!   you/
//!     profile.md
//! ```
//!
//! One short page, and it is here for a reason that is nearly all of the
//! argument for the feature: a person should be able to see, in plain text,
//! everything this application holds about *them* as opposed to what they
//! wrote in it. That is one file, it is six fields long, and being able to
//! read it is the difference between a private application and one that
//! merely says it is.
//!
//! What is emphatically not here: the key the assistant talks to a model
//! with, the tokens paired devices hold, and the vault's own wrapped data
//! key. An export is a plaintext file bound for a Downloads folder, and a
//! credential in one is a credential leaked. See the crate docs.

use crate::text::{FrontMatter, split_front_matter};
use crate::{Files, Mode, Options, Part, Portable, Report, Spec};
use everyday_core::Result;
use everyday_core::profile::Profile;
use everyday_core::store::JournalStore;

pub struct YouPart;
pub static YOU: YouPart = YouPart;

static SPEC: Spec = Spec {
    id: "you",
    label: "You",
    summary: "The handful of facts the assistant knows about you by being told: your name, \
              where you live, how you describe yourself. No passwords and no keys.",
    format: "One Markdown page",
    media: false,
    imports: true,
};

const PAGE: &str = "profile.md";

impl Portable for YouPart {
    fn spec(&self) -> &'static Spec {
        &SPEC
    }

    fn tally(&self, store: &dyn JournalStore) -> Result<Option<u64>> {
        let profile = store.profile()?;
        // An untouched profile is nothing to export. `Some(0)` rather than
        // `None`: this vault *could* hold one, it just does not yet, and the
        // chooser should say so rather than pretend the tab does not exist.
        Ok(Some(u64::from(!is_blank(&profile))))
    }

    fn export(&self, store: &dyn JournalStore, out: &mut Files<'_>, _opts: &Options) -> Result<()> {
        let profile = store.profile()?;
        if is_blank(&profile) {
            return Ok(());
        }
        let mut front = FrontMatter::new();
        front
            .set("first_name", &profile.first_name)
            .set("last_name", &profile.last_name)
            .set_opt("born", profile.born)
            .set("gender", &profile.gender)
            .set("location", &profile.location);
        if let Some(updated) = profile.updated_at {
            front.set("updated", super::doc::stamp(updated));
        }
        let name = format!("{} {}", profile.first_name, profile.last_name);
        let body = format!("{}# {}\n\n{}\n", front.render(), name.trim(), profile.about.trim());
        out.records(PAGE, body, 1)?;
        Ok(())
    }

    fn import(&self, store: &dyn JournalStore, src: &Part<'_>, mode: Mode) -> Result<Report> {
        let mut report = Report::new(SPEC.id);
        let Some(source) = src.text(PAGE) else { return Ok(report) };

        let existing = store.profile()?;
        let existed = !is_blank(&existing);
        if existed && mode == Mode::Skip {
            report.skipped += 1;
            return Ok(report);
        }

        let (fields, body) = split_front_matter(source);
        let about = body
            .lines()
            .skip_while(|l| l.trim().is_empty() || l.starts_with("# "))
            .collect::<Vec<_>>()
            .join("\n");
        store.put_profile(&Profile {
            first_name: fields.text("first_name"),
            last_name: fields.text("last_name"),
            born: fields.parse("born"),
            gender: fields.text("gender"),
            location: fields.text("location"),
            about: about.trim().to_string(),
            updated_at: Some(jiff::Timestamp::now()),
        })?;
        report.count(existed, mode);
        Ok(report)
    }
}

/// Has anybody filled any of this in?
fn is_blank(profile: &Profile) -> bool {
    profile.first_name.trim().is_empty()
        && profile.last_name.trim().is_empty()
        && profile.gender.trim().is_empty()
        && profile.location.trim().is_empty()
        && profile.about.trim().is_empty()
        && profile.born.is_none()
}
