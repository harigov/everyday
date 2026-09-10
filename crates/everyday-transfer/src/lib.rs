//! Getting a vault's contents out of it, and back in.
//!
//! Everything in Every Day is stored sealed, in a database, under a key one
//! person holds. That is the point of it, and it is also the thing that makes
//! a promise necessary: *your writing is yours, and you can walk away with
//! it.* An application that can only be read by itself is one you are
//! trapped in, however good its encryption.
//!
//! So this crate turns the vault into a folder of files that other programs
//! already read — Markdown with front matter, iCalendar, CSV — and reads such
//! a folder back. There is no Every Day format anywhere in it. The archive is
//! a zip because that is what all three desktops open by double-clicking; the
//! files inside it are what they would be if you had kept your journal in a
//! text editor all along.
//!
//! # An app declares its own
//!
//! ```text
//!   Portable ── spec()    what this is, and what shape it is written in
//!            ── tally()   how many records there are to hand over
//!            ── export()  write them, into a folder that is yours alone
//!            ── import()  read them back
//! ```
//!
//! Each app implements [`Portable`] once, in `parts/`, and adds a line to
//! [`PARTS`]. Nothing else learns that it exists: the chooser in settings is
//! drawn from [`survey`], the archive's manifest and its README are generated
//! from the same specs, and the commands in the service take a list of part
//! ids they never interpret. The fifth app becomes exportable by being
//! written, not by somebody remembering to extend a match arm here — which is
//! the same reasoning that put the command table in
//! [`everyday_service::domains`] and the change vocabulary in one enum.
//!
//! A part owns a folder in the archive and can see nothing outside it. On the
//! way out [`Files`] prefixes every name it is given; on the way back in
//! [`Part`] only offers the files under that prefix. Two apps therefore
//! cannot collide, and an app cannot read another's records by reaching into
//! its folder — which matters most for the app that will one day hold
//! passwords.
//!
//! # What is deliberately not here
//!
//! **Secrets.** The assistant's API key, the device tokens, the wrapped data
//! key. An export is a plaintext file that will end up in a Downloads folder,
//! and a credential in it is a credential leaked. The vault's own
//! `backup` — which copies it *sealed* — is the thing that carries those, and
//! it is a different verb on purpose.
//!
//! **A promise of perfect fidelity in both directions.** Markdown does not
//! have a node type for everything the editor can produce, and CSV has no
//! opinion about a `#rrggbb`. Where a field has nowhere to go in the format,
//! it goes in the front matter beside it; where even that would be silly, it
//! is lost, and the [`Spec`] says so in words the settings dialog shows.

pub mod ics;
mod manifest;
mod parts;
pub mod text;
pub mod zip;

use everyday_core::store::JournalStore;
use everyday_core::{Error, Result, Vault};
use std::collections::BTreeMap;
use std::io::Write;

pub use manifest::{Manifest, PartEntry};

/// What one app hands over, in the terms a chooser needs.
pub struct Spec {
    /// Stable id. Also the archive folder, so it may not change once shipped
    /// without making older archives unreadable by name.
    pub id: &'static str,
    /// What the app is called, as the app bar calls it.
    pub label: &'static str,
    /// What is in here, in one line, for somebody deciding whether to tick it.
    pub summary: &'static str,
    /// The format, named the way its own community names it -- "Markdown with
    /// YAML front matter", "iCalendar (.ics)" -- so a person can tell at a
    /// glance what will open it.
    pub format: &'static str,
    /// Whether ticking "include attachments" changes anything here.
    pub media: bool,
    /// Can this be read back? A part may honestly answer no: the assistant's
    /// transcripts are a record of something that happened, and writing one
    /// into a vault would be minuting a conversation nobody had.
    pub imports: bool,
}

impl Spec {
    /// The folder this part owns, trailing slash included.
    fn folder(&self) -> String {
        format!("{}/", self.id)
    }
}

/// One app's half of an export.
///
/// Implementors live in `parts/` and are registered in [`PARTS`].
pub trait Portable: Send + Sync {
    fn spec(&self) -> &'static Spec;

    /// How many records there are, or `None` when this backend cannot hold
    /// them at all.
    ///
    /// `None` and `Some(0)` are different answers and the difference is shown:
    /// a vault whose backend has no task store should say the todo app is not
    /// stored here, not that there are no tasks.
    fn tally(&self, store: &dyn JournalStore) -> Result<Option<u64>>;

    /// Write this part's files.
    fn export(&self, store: &dyn JournalStore, out: &mut Files<'_>, opts: &Options) -> Result<()>;

    /// Read them back. Never called for a part whose spec says it does not
    /// import.
    fn import(&self, store: &dyn JournalStore, src: &Part<'_>, mode: Mode) -> Result<Report> {
        let _ = (store, src, mode);
        Err(Error::Unsupported("importing this app's records"))
    }
}

/// Every app that can hand its records over, in the order the app bar lists
/// them.
///
/// The whole registry. Adding an app is a module in `parts/` and a line here.
pub static PARTS: &[&dyn Portable] = &[
    &parts::journal::JOURNAL,
    &parts::notes::NOTES,
    &parts::tasks::TASKS,
    &parts::calendar::CALENDAR,
    &parts::library::LIBRARY,
    &parts::trackers::TRACKERS,
    &parts::purpose::PURPOSE,
    &parts::assistant::ASSISTANT,
    &parts::you::YOU,
];

fn find(id: &str) -> Option<&'static dyn Portable> {
    PARTS.iter().copied().find(|p| p.spec().id == id)
}

/// What to include.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Part ids. Empty means every part this vault can offer, which is what
    /// the command line does when nobody said otherwise.
    pub parts: Vec<String>,
    /// Write the photographs, video and covers as well as the words.
    ///
    /// Off is not a smaller export of the same thing -- it is an export
    /// missing the pictures -- so the settings dialog shows what the media
    /// weighs before asking.
    pub media: bool,
}

impl Options {
    fn wants(&self, spec: &Spec) -> bool {
        self.parts.is_empty() || self.parts.iter().any(|p| p == spec.id)
    }
}

/// How an import treats a record the vault already has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Add what is missing; leave what is already here exactly as it is.
    ///
    /// The default, and the right one for reading somebody else's archive or
    /// an old one of your own: nothing you have written can be lost by it.
    #[default]
    Skip,
    /// Add what is missing and overwrite what is here from the archive.
    ///
    /// What "I edited the export in a text editor and want the changes back"
    /// needs. Destructive by design, which is why the interface asks twice
    /// and the dry run says how many records it would replace.
    Replace,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "skip" => Some(Mode::Skip),
            "replace" => Some(Mode::Replace),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Skip => "skip",
            Mode::Replace => "replace",
        }
    }
}

/// What one part's import did.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub part: String,
    pub added: u64,
    pub replaced: u64,
    /// Already here, and left alone.
    pub skipped: u64,
    /// Files that could not be read, named. Not an error: one unreadable note
    /// in four hundred should not cost somebody the other three hundred and
    /// ninety-nine, and a silent skip is how people lose things without
    /// knowing.
    #[serde(default)]
    pub problems: Vec<String>,
}

impl Report {
    pub fn new(part: &str) -> Self {
        Self { part: part.to_string(), ..Default::default() }
    }

    /// Count a record, by what happened to it.
    pub fn count(&mut self, existed: bool, mode: Mode) {
        match (existed, mode) {
            (false, _) => self.added += 1,
            (true, Mode::Replace) => self.replaced += 1,
            (true, Mode::Skip) => self.skipped += 1,
        }
    }

    pub fn problem(&mut self, file: &str, why: impl std::fmt::Display) {
        self.problems.push(format!("{file}: {why}"));
    }

    pub fn touched(&self) -> u64 {
        self.added + self.replaced
    }
}

/// What a vault can hand over right now.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartInfo {
    pub id: String,
    pub label: String,
    pub summary: String,
    pub format: String,
    pub records: u64,
    pub media: bool,
    pub imports: bool,
}

/// Ask every app how much it has. What the settings dialog draws.
///
/// A part its backend cannot hold is left out rather than listed as empty:
/// the question the chooser answers is "what can I take with me", and an app
/// this vault has never been able to store is not an answer to it.
pub fn survey(vault: &Vault) -> Result<Vec<PartInfo>> {
    vault.with_store(|store| {
        let mut out = Vec::with_capacity(PARTS.len());
        for part in PARTS {
            let spec = part.spec();
            let Some(records) = part.tally(store)? else { continue };
            out.push(PartInfo {
                id: spec.id.into(),
                label: spec.label.into(),
                summary: spec.summary.into(),
                format: spec.format.into(),
                records,
                media: spec.media,
                imports: spec.imports,
            });
        }
        Ok(out)
    })
}

// ---- writing ------------------------------------------------------------

/// Somewhere to put a file. Implemented by the archive writer, and by a map
/// in tests.
pub trait Sink {
    fn put(&mut self, name: &str, body: &[u8]) -> Result<()>;
}

impl<W: Write> Sink for zip::Writer<W> {
    fn put(&mut self, name: &str, body: &[u8]) -> Result<()> {
        self.add(name, body)
    }
}

/// One part's view of the archive it is writing into.
///
/// Every name is prefixed with the part's own folder, so a part cannot write
/// outside it however it is implemented.
pub struct Files<'a> {
    sink: &'a mut dyn Sink,
    prefix: String,
    files: u64,
    bytes: u64,
    records: u64,
    /// Attachments already written, and the path each landed at, so an image
    /// used by three entries is one file linked three times. Blobs are
    /// content-addressed, so this is exact rather than a guess at sameness.
    blobs: BTreeMap<everyday_core::BlobId, String>,
}

impl Files<'_> {
    /// Write a text file at `name`, relative to this part's folder.
    pub fn text(&mut self, name: &str, body: impl AsRef<str>) -> Result<()> {
        self.bytes_at(name, body.as_ref().as_bytes())
    }

    /// Write a text file and count it as `records` records.
    pub fn records(&mut self, name: &str, body: impl AsRef<str>, records: u64) -> Result<()> {
        self.records += records;
        self.text(name, body)
    }

    pub fn bytes_at(&mut self, name: &str, body: &[u8]) -> Result<()> {
        self.sink.put(&format!("{}{name}", self.prefix), body)?;
        self.files += 1;
        self.bytes += body.len() as u64;
        Ok(())
    }

    /// Count records written by something other than [`Files::records`].
    pub fn counted(&mut self, records: u64) {
        self.records += records;
    }

    /// Write an attachment, and answer with the path to link to it by.
    ///
    /// `Ok(None)` when attachments were not asked for, or when the blob has
    /// gone -- a link to a file that is not there is worse than prose that
    /// says the picture was not included, and one lost photograph should not
    /// cost somebody the entry it was in.
    ///
    /// A failure to *write* is a different thing and is an error: a disk that
    /// filled up half way through means the export is not what it claims to
    /// be, and an export that reports success with the pictures missing is the
    /// one outcome this whole crate exists to prevent.
    ///
    /// The name carries eight hex digits of the blob's own address, which is
    /// what keeps two photographs both called `IMG_0042.jpg` apart without
    /// inventing a counter that would differ between two exports of the same
    /// vault.
    pub fn media(
        &mut self,
        store: &dyn JournalStore,
        blob: everyday_core::BlobId,
        filename: &str,
        opts: &Options,
    ) -> Result<Option<String>> {
        if !opts.media {
            return Ok(None);
        }
        if let Some(written) = self.blobs.get(&blob) {
            return Ok(Some(written.clone()));
        }
        let body = match store.get_blob(blob) {
            Ok(body) => body,
            Err(e) => {
                tracing::warn!(%blob, error = %e, "an attachment could not be exported");
                return Ok(None);
            }
        };
        // A record does not always know what its own file was called -- a
        // cover fetched from the web has a URL and no name -- so the type is
        // taken from the bytes when the name does not carry one. An image
        // saved as `cover` opens in nothing; `cover.png` opens in everything.
        let stem = text::safe_name(filename);
        let name = match stem.rsplit_once('.') {
            Some((_, ext)) if !ext.is_empty() && ext.len() <= 5 => stem.clone(),
            _ => format!("{stem}{}", extension(&body)),
        };
        let path = format!("media/{}-{name}", &blob.to_hex()[..8]);
        self.bytes_at(&path, &body)?;
        self.blobs.insert(blob, path.clone());
        Ok(Some(path))
    }
}

/// Write an archive of `vault` into `out`.
///
/// The manifest is written last of the metadata and first in reading order,
/// because it is generated from what the parts actually wrote rather than
/// from what they were asked to write.
pub fn export<W: Write>(vault: &Vault, opts: &Options, out: W) -> Result<(W, Manifest)> {
    let at = jiff::Zoned::now();
    let mut writer = zip::Writer::new(out, at.datetime());
    let manifest = write_into(vault, opts, &mut writer)?;
    Ok((writer.finish()?, manifest))
}

/// The same, into anything that can take a named file.
///
/// The command line writes a folder with this rather than a zip, which is the
/// same archive with the container taken off -- and it is why [`Sink`] is a
/// trait at all. Nothing about a part changes between the two.
pub fn write_into(vault: &Vault, opts: &Options, sink: &mut dyn Sink) -> Result<Manifest> {
    let at = jiff::Zoned::now();
    let writer = sink;
    let mut manifest = Manifest::new(&vault.status().name, at.timestamp(), opts.media);

    vault.with_store(|store| {
        for part in PARTS {
            let spec = part.spec();
            if !opts.wants(spec) || part.tally(store)?.is_none() {
                continue;
            }
            let mut files = Files {
                sink: writer,
                prefix: spec.folder(),
                files: 0,
                bytes: 0,
                records: 0,
                blobs: BTreeMap::new(),
            };
            part.export(store, &mut files, opts)?;
            // A part with nothing in it writes nothing and is not listed. An
            // empty folder in an archive tells the reader an app exists and
            // nothing else.
            if files.files == 0 {
                continue;
            }
            manifest.add(spec, files.records, files.files, files.bytes);
        }
        Ok(())
    })?;

    writer.put("everyday.json", manifest.render().as_bytes())?;
    writer.put("README.md", manifest.readme().as_bytes())?;
    Ok(manifest)
}

/// The file extension for these bytes, dot included, or nothing.
///
/// Sniffed rather than taken from the record, on the same principle the media
/// protocol already follows: what a document *says* a file is can be wrong or
/// hostile, and what it starts with cannot.
fn extension(body: &[u8]) -> &'static str {
    match everyday_vault::media::sniff_mime(body) {
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "image/avif" => ".avif",
        "image/heic" => ".heic",
        "image/svg+xml" => ".svg",
        "image/bmp" => ".bmp",
        "video/mp4" => ".mp4",
        "video/quicktime" => ".mov",
        "video/webm" => ".webm",
        "video/x-msvideo" => ".avi",
        "audio/mp4" => ".m4a",
        "audio/mpeg" => ".mp3",
        "audio/wav" => ".wav",
        "audio/ogg" => ".ogg",
        "audio/flac" => ".flac",
        "application/pdf" => ".pdf",
        _ => "",
    }
}

// ---- reading ------------------------------------------------------------

/// One part's view of an archive being read.
///
/// Offers the files under the part's folder and nothing else, named relative
/// to it -- the mirror of [`Files`], and for the same reason.
pub struct Part<'a> {
    archive: &'a zip::Reader,
    prefix: String,
}

impl<'a> Part<'a> {
    /// Every file in this part, as `(relative name, bytes)`, in path order.
    pub fn files(&'a self) -> impl Iterator<Item = (&'a str, &'a [u8])> {
        self.archive.under(&self.prefix)
    }

    /// Every file whose name ends in `suffix`, skipping the media folder --
    /// which is where an attachment called `notes.md` would otherwise be read
    /// as a note.
    pub fn documents(&'a self, suffix: &'a str) -> impl Iterator<Item = (&'a str, &'a str)> {
        self.files().filter_map(move |(name, body)| {
            (name.ends_with(suffix) && !name.starts_with("media/"))
                .then(|| Some((name, std::str::from_utf8(body).ok()?)))
                .flatten()
        })
    }

    /// One file by relative name.
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.archive.get(&format!("{}{name}", self.prefix))
    }

    pub fn text(&self, name: &str) -> Option<&str> {
        std::str::from_utf8(self.get(name)?).ok()
    }

    pub fn is_empty(&self) -> bool {
        self.files().next().is_none()
    }
}

/// What is in an archive, and what importing it would touch.
///
/// The manifest is believed where there is one and the folders are believed
/// where there is not, because the second case is the one that matters most:
/// a folder of Markdown somebody wrote by hand, or dragged out of another
/// program, should import without having to be given a manifest first.
pub fn inspect(archive: &zip::Reader) -> Result<Manifest> {
    if archive.is_empty() {
        return Err(Error::Invalid("this archive is empty".into()));
    }
    let stated = archive
        .get("everyday.json")
        .and_then(|bytes| serde_json::from_slice::<Manifest>(bytes).ok());
    let mut manifest = stated.unwrap_or_else(Manifest::unstated);

    // Whatever the manifest says, what is *there* is what can be imported.
    // An archive edited by hand -- a folder deleted, a folder added -- is the
    // ordinary case rather than a corruption.
    manifest.parts.retain(|entry| find(&entry.id).is_some_and(|p| !part_of(archive, p).is_empty()));
    for part in PARTS {
        let spec = part.spec();
        if manifest.parts.iter().any(|e| e.id == spec.id) {
            continue;
        }
        let files = part_of(archive, *part);
        if !files.is_empty() {
            let count = files.files().count() as u64;
            manifest.add(spec, 0, count, 0);
        }
    }
    if manifest.parts.is_empty() {
        return Err(Error::Invalid(
            "there is nothing in this archive that Every Day knows how to read".into(),
        ));
    }
    Ok(manifest)
}

fn part_of<'a>(archive: &'a zip::Reader, part: &dyn Portable) -> Part<'a> {
    Part { archive, prefix: part.spec().folder() }
}

/// Read `parts` of `archive` into `vault`.
///
/// One report per part, in the order the parts are registered. A part that
/// fails outright is reported as a problem against itself rather than
/// abandoning the ones after it: an archive whose calendar folder is damaged
/// should still deliver the journal.
pub fn import(
    vault: &Vault,
    archive: &zip::Reader,
    parts: &[String],
    mode: Mode,
) -> Result<Vec<Report>> {
    if !vault.is_writable() {
        return Err(Error::Unsupported("importing into a read-only vault"));
    }
    let reports = vault.with_store(|store| {
        let mut reports = Vec::new();
        for part in PARTS {
            let spec = part.spec();
            if !spec.imports || !parts.iter().any(|p| p == spec.id) {
                continue;
            }
            let src = part_of(archive, *part);
            if src.is_empty() {
                continue;
            }
            reports.push(match part.import(store, &src, mode) {
                Ok(report) => report,
                Err(e) => {
                    let mut report = Report::new(spec.id);
                    report.problem(spec.label, e);
                    report
                }
            });
        }
        Ok(reports)
    })?;

    // Entries and notes are searched through an index built at unlock time,
    // and nothing below the vault knows how to add to it. Rebuilding once,
    // after everything has landed, is both cheaper than maintaining it per
    // record and the only thing that gets a bulk import into search at all.
    if reports.iter().any(|r| r.touched() > 0) {
        vault.reindex()?;
    }
    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_part_is_registered_twice() {
        let mut seen = std::collections::BTreeSet::new();
        for part in PARTS {
            assert!(seen.insert(part.spec().id), "{} is registered twice", part.spec().id);
        }
    }

    #[test]
    fn a_part_id_is_a_plain_folder_name() {
        for part in PARTS {
            let id = part.spec().id;
            assert!(
                id.chars().all(|c| c.is_ascii_lowercase() || c == '-') && !id.is_empty(),
                "{id} would not make a tidy folder"
            );
        }
    }

    #[test]
    fn every_part_says_what_shape_it_is_in() {
        for part in PARTS {
            let spec = part.spec();
            assert!(!spec.summary.is_empty(), "{} has no summary", spec.id);
            assert!(!spec.format.is_empty(), "{} does not name its format", spec.id);
        }
    }

    #[test]
    fn a_report_counts_by_what_happened() {
        let mut r = Report::new("journal");
        r.count(false, Mode::Skip);
        r.count(true, Mode::Skip);
        r.count(true, Mode::Replace);
        assert_eq!((r.added, r.skipped, r.replaced), (1, 1, 1));
        assert_eq!(r.touched(), 2);
    }
}
