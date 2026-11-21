//! The background pass that keeps the documents' search index in step with
//! the files folder. See the module docs of [`super`].
//!
//! # What a pass does
//!
//! It lists the catalog's documents, keeps those the settings say to index
//! ([`FilesSettings::indexes`]: plain, present, under the chosen folders and
//! not under the skipped ones), and compares them with what the index holds
//! and at which version -- the cache's name for the file's contents. A
//! document that is new or changed is read ([`DocumentsState::text`], so what
//! the indexer parses the reader finds already cached) and put into the index
//! as passages; one the index holds and the settings no longer want is taken
//! out. A pass never leaves the index holding a stale version of a file it
//! could not read: that file is taken out and listed as failed.
//!
//! One document at a time, each on the blocking pool, with a short breath
//! between them and a check that the vault has not locked -- a pass over a
//! thousand books is minutes of parsing that must not hold up the lock, nor
//! the window the person is reading in. The index is committed every few
//! documents, so what is found is searchable as the pass goes and a pass cut
//! short keeps what it did.
//!
//! # When
//!
//! Shortly after unlock, then every few minutes, and soon after something
//! says there is work: a reindex asked for ([`wake`]), or a change in the
//! catalog or the settings. The catalog's changes are noticed by looking: the
//! files reconciler announces them to the windows, not to the service, so
//! every [`LOOK_EVERY`] the catalog's cheap columns (which every file's row
//! carries in the clear) are hashed and compared with what the last pass saw.
//!
//! # What is not tried again
//!
//! A document that cannot be parsed -- too large, or not what its name says
//! -- is recorded with its version, and is not tried again until its contents
//! change or a reindex is asked for: parsing a broken file again every five
//! minutes would be the same wasted work for ever. A file that could not be
//! *reached* (the folder is away, the disk is busy) is not recorded; it is
//! tried again at the next pass.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use karmayoga_core::Vault;
use karmayoga_core::documents::{DocFormat, DocText};
use karmayoga_core::error::{Error, Result as CoreResult};
use karmayoga_core::files::{
    FileEntry, FileFilter, FileKind, FileOrigin, FileQuery, FileSort, FilesSettings, StatusFilter,
};
use karmayoga_core::id::FileId;
use karmayoga_docindex::Passage;
use tokio::sync::{Notify, watch};
use tokio::time::Instant;

use super::{DocumentsState, LoadError};
use crate::service::{Service, blocking};
use crate::supervisor::{Outcome, TaskResult};

/// The supervisor's key for this task.
pub const TASK_KEY: &str = "documents";

/// How long after the vault unlocks the first pass starts: after the files
/// reconciler's own first (ten seconds), so it indexes what that has found.
const FIRST: Duration = Duration::from_secs(20);

/// How often a pass runs when nothing prompts one.
const EVERY: Duration = Duration::from_secs(5 * 60);

/// How often the catalog is looked at for a change, between passes.
const LOOK_EVERY: Duration = Duration::from_secs(30);

/// How long after a change is seen the pass it brings forward starts: long
/// enough for a burst of files arriving to be one pass.
const QUIET: Duration = Duration::from_secs(5);

/// How long after a [`wake`] the pass starts.
const SOON: Duration = Duration::from_secs(2);

/// The breath between one document and the next.
const PAUSE: Duration = Duration::from_millis(25);

/// How many documents are put in before the index is committed.
const COMMIT_EVERY: usize = 10;

/// How often the cache is swept of what no document maps to any more.
const SWEEP_EVERY: Duration = Duration::from_secs(60 * 60);

/// How many catalog rows are read at a time.
const CATALOG_PAGE: u32 = 500;

/// About how much text a passage holds. A passage is the unit a search finds
/// and a snippet is cut from, so it is a few sentences, not a chapter.
pub const PASSAGE_CHARS: usize = 1_200;

/// A block longer than this is cut into passages of its own, all at the same
/// block: a text file that is one long paragraph must be searchable all the
/// way down, and a block is the address a hit opens at.
const OVERSIZE_CHARS: usize = 3_000;

/// One for the process: the task that waits on it belongs to the vault open
/// now, and a ring left over from one just closed costs the next a pass
/// early, nothing more.
fn wake_signal() -> &'static Arc<Notify> {
    static WAKE: OnceLock<Arc<Notify>> = OnceLock::new();
    WAKE.get_or_init(|| Arc::new(Notify::new()))
}

/// Ask for a pass soon: after a reindex, or a write that bypassed the
/// catalog.
pub fn wake() {
    wake_signal().notify_one();
}

/// Start the indexer if it is not already running. Idempotent, like every
/// supervised task's `ensure_task`.
///
/// Only a process that holds the vault's write claim runs it, and only for a
/// vault that keeps a catalog: a second window that merely reads must not
/// write the index the first is writing, and has no folder to index without
/// a catalog.
pub fn ensure_task(svc: &Arc<Service>, vault: &Arc<Vault>) {
    if !vault.is_writable() || !vault.supports_files() {
        return;
    }
    // Looks the vault up each time it starts rather than keeping one: see
    // `calendar_refresh::ensure_task` for why a vault held by a factory the
    // supervisor keeps for good would be wrong.
    let svc_for_task = svc.clone();
    svc.supervisor().ensure(TASK_KEY, move |stop| {
        let svc = svc_for_task.clone();
        Box::pin(async move {
            match svc.get() {
                Some(vault) if vault.is_writable() && vault.supports_files() => {
                    run(svc, vault, stop).await
                }
                _ => Ok(Outcome::Done),
            }
        })
    });
}

/// The supervised loop: a pass [`FIRST`] after it starts, then every
/// [`EVERY`], and sooner after a wake or a change in the catalog. Returns when
/// `stop` turns `true`.
async fn run(svc: Arc<Service>, vault: Arc<Vault>, mut stop: watch::Receiver<bool>) -> TaskResult {
    let wake = wake_signal().clone();
    let start = Instant::now();
    let mut pass_at = start + FIRST;
    let mut look_at = start + LOOK_EVERY;
    // What the catalog looked like to the last pass; `None` before the first.
    let mut seen: Option<String> = None;
    let mut swept: Option<Instant> = None;
    loop {
        if *stop.borrow() {
            return Ok(Outcome::Done);
        }
        tokio::select! {
            stopping = stop.changed() => {
                if stopping.is_err() {
                    return Ok(Outcome::Done);
                }
            }
            () = wake.notified() => {
                pass_at = pass_at.min(Instant::now() + SOON);
            }
            () = tokio::time::sleep_until(look_at) => {
                look_at = Instant::now() + LOOK_EVERY;
                if seen.is_none() || !vault.is_unlocked() {
                    continue;
                }
                let looking = vault.clone();
                let now = blocking(move || Ok(fingerprint_of(&looking)?)).await;
                if let Ok(now) = now
                    && seen.as_ref() != Some(&now)
                {
                    pass_at = pass_at.min(Instant::now() + QUIET);
                }
            }
            () = tokio::time::sleep_until(pass_at) => {
                if !vault.is_unlocked() {
                    pass_at = Instant::now() + EVERY;
                    continue;
                }
                let sweep = swept.is_none_or(|at| at.elapsed() >= SWEEP_EVERY);
                let ended = pass(&svc, &vault, &stop, &mut seen, sweep).await;
                // The interval is counted from when a pass ends, not from when
                // it began: one that took twenty minutes is not owed another
                // at once.
                pass_at = Instant::now() + EVERY;
                match ended {
                    End::Done { swept: true } => swept = Some(Instant::now()),
                    End::Done { swept: false } | End::Stopped => {}
                    // What the pass found out was cleared from under it: begin
                    // again rather than wait out the interval.
                    End::Again => pass_at = Instant::now() + SOON,
                }
            }
        }
    }
}

/// How a pass ended.
#[derive(Debug, PartialEq, Eq)]
enum End {
    /// All of it done; `swept` says whether the cache was swept too.
    Done { swept: bool },
    /// The vault locked, or the task was told to stop.
    Stopped,
    /// The index was cleared while the pass ran.
    Again,
}

/// What a pass is going to do, worked out before it starts.
#[derive(Default)]
struct Plan {
    /// What the catalog looked like when this was worked out: see
    /// [`fingerprint_of`].
    fingerprint: String,
    /// Documents to read into the index: new, or changed since they were.
    work: Vec<FileEntry>,
    /// Indexed files the settings no longer want.
    remove: Vec<FileId>,
    /// The cache's names for every document the catalog holds, whatever its
    /// state: what a sweep spares.
    names: HashSet<String>,
    /// The folder is not there: nothing is compared with it.
    unavailable: bool,
    /// Indexing is switched off.
    off: bool,
}

/// Marks a pass as running for as long as it lives, and says so no longer
/// however it ends.
struct Running<'a>(&'a DocumentsState);

impl<'a> Running<'a> {
    fn begin(state: &'a DocumentsState) -> Self {
        state.progress().running = true;
        Running(state)
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let mut progress = self.0.progress();
        progress.running = false;
        // Whatever this pass had left is worked out afresh by the next.
        progress.pending = 0;
    }
}

/// What came of one document.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    Indexed,
    /// Could not be read, and will not be tried again as it is.
    Failed,
    /// Left for the next pass: unreachable, or not wanted after all.
    Skipped,
    /// The vault locked.
    Locked,
}

/// One pass: see the module docs. Returns how it ended; `seen` is set to what
/// the catalog looked like when it began.
async fn pass(
    svc: &Arc<Service>,
    vault: &Arc<Vault>,
    stop: &watch::Receiver<bool>,
    seen: &mut Option<String>,
    sweep: bool,
) -> End {
    let Ok(state) = svc.documents() else { return End::Stopped };
    // An index this process cannot write -- another has it open, or this
    // vault's last session is still finishing with it -- is left alone.
    if state.index().is_none_or(|index| !index.can_write()) {
        return End::Done { swept: false };
    }
    let _running = Running::begin(&state);
    let epoch = state.epoch();

    let planning = (state.clone(), vault.clone());
    let plan = match blocking(move || Ok(plan(&planning.0, &planning.1)?)).await {
        Ok(plan) => plan,
        Err(e) => {
            tracing::debug!(error = %e, "documents: could not plan an indexing pass");
            return End::Done { swept: false };
        }
    };
    *seen = Some(plan.fingerprint.clone());
    if plan.off || plan.unavailable {
        return End::Done { swept: false };
    }
    state.progress().pending = (plan.work.len() + plan.remove.len()) as u64;

    let stopped = || *stop.borrow() || !crate::files::still_open(svc, vault);

    if !plan.remove.is_empty() {
        let (state, remove) = (state.clone(), plan.remove);
        let _ = blocking(move || {
            if let Some(index) = state.index() {
                for id in &remove {
                    if let Err(e) = index.remove(*id) {
                        tracing::debug!(error = %e, "documents: could not unindex a file");
                    }
                }
                if let Err(e) = index.commit() {
                    tracing::warn!(error = %e, "documents: could not commit the index");
                }
            }
            let mut progress = state.progress();
            progress.pending = progress.pending.saturating_sub(remove.len() as u64);
            Ok(())
        })
        .await;
    }

    for (n, file) in plan.work.into_iter().enumerate() {
        if stopped() {
            return End::Stopped;
        }
        if state.epoch() != epoch {
            return End::Again;
        }
        let (state, vault, commit) = (state.clone(), vault.clone(), (n + 1) % COMMIT_EVERY == 0);
        match blocking(move || Ok(index_one(&state, &vault, &file, commit))).await {
            Ok(Step::Locked) | Err(_) => return End::Stopped,
            Ok(_) => {}
        }
        // A breath, and a chance for the lock to be heard.
        tokio::time::sleep(PAUSE).await;
    }
    if stopped() {
        return End::Stopped;
    }
    if state.epoch() != epoch {
        return End::Again;
    }

    let (finishing, names) = (state.clone(), plan.names);
    let done = blocking(move || {
        if let Some(index) = finishing.index() {
            if let Err(e) = index.commit() {
                tracing::warn!(error = %e, "documents: could not commit the index");
            }
            // Counted afresh, so the status is what the index says and not a
            // sum of what this pass thought it did.
            if let Ok(all) = index.indexed() {
                finishing.progress().indexed = Some(all.len() as u64);
            }
        }
        if sweep {
            let removed = finishing.cache().sweep(&names);
            if removed > 0 {
                tracing::debug!(removed, "documents: swept the cache of documents nothing maps to");
            }
        }
        Ok(())
    })
    .await;
    if done.is_ok() {
        state.progress().last_pass = Some(svc.now());
    }
    End::Done { swept: sweep && done.is_ok() }
}

/// Read one document and put it into the index; commit after it when
/// `commit`. Blocking.
fn index_one(state: &DocumentsState, vault: &Vault, file: &FileEntry, commit: bool) -> Step {
    let Some(index) = state.index() else { return Step::Skipped };
    let version = state.version_of(file);
    // Not kept in memory: a pass over a library would push out the book that
    // is open in the reader.
    let step = match state.load_as(vault, file, false) {
        Ok(text) => {
            let passages = passages_of(&text);
            match index.put(file.id, &version, &title_of(&text, file), &passages) {
                Ok(()) => {
                    state.progress().failed.remove(&file.id);
                    Step::Indexed
                }
                Err(e) => {
                    tracing::warn!(error = %e, "documents: could not index a document");
                    Step::Skipped
                }
            }
        }
        Err(LoadError::Unparsable(reason)) => {
            // Nothing of an older version is left to be found.
            if let Err(e) = index.remove(file.id) {
                tracing::debug!(error = %e, "documents: could not take a file out of the index");
            }
            state.progress().failed.insert(file.id, (version, reason));
            Step::Failed
        }
        Err(LoadError::Unreadable(Error::Locked)) => Step::Locked,
        Err(LoadError::Unreadable(e)) => {
            tracing::debug!(error = %e, file = %file.name, "documents: could not reach a document");
            Step::Skipped
        }
        // The catalog changed under the pass: the next one will not want it.
        Err(LoadError::Refused(_)) => Step::Skipped,
    };
    {
        let mut progress = state.progress();
        progress.pending = progress.pending.saturating_sub(1);
    }
    if commit && let Err(e) = index.commit() {
        tracing::warn!(error = %e, "documents: could not commit the index");
    }
    step
}

/// Work out what a pass has to do. Blocking.
fn plan(state: &DocumentsState, vault: &Vault) -> CoreResult<Plan> {
    let settings = vault.files_settings()?;
    let fingerprint = fingerprint(vault, &settings)?;
    let Some(index) = state.index() else { return Ok(Plan { fingerprint, ..Plan::default() }) };

    if !settings.index_documents {
        // Switched off: what is in the index goes, once, and the status says
        // there is nothing.
        if !index.indexed()?.is_empty() {
            index.clear()?;
        }
        let mut progress = state.progress();
        progress.indexed = Some(0);
        progress.pending = 0;
        progress.failed.clear();
        return Ok(Plan { fingerprint, off: true, ..Plan::default() });
    }
    if !vault.folder_state()?.is_available() {
        // A drive that is not mounted is not a folder with nothing in it.
        return Ok(Plan { fingerprint, unavailable: true, ..Plan::default() });
    }

    let indexed = index.indexed()?;
    let mut plan = Plan { fingerprint, ..Plan::default() };
    let mut wanted: HashMap<FileId, String> = HashMap::new();
    let failed = state.progress().failed.clone();
    each_document(vault, |file| {
        let version = state.version_of(&file);
        plan.names.insert(version.clone());
        if !settings.indexes(&file) {
            return;
        }
        let current = indexed.get(&file.id) == Some(&version);
        let given_up = failed.get(&file.id).is_some_and(|(was, _)| *was == version);
        wanted.insert(file.id, version);
        if !current && !given_up {
            plan.work.push(file);
        }
    })?;
    plan.remove = indexed.keys().filter(|id| !wanted.contains_key(*id)).copied().collect();

    let mut progress = state.progress();
    // A document that has changed or is no longer wanted is not given up on.
    progress.failed.retain(|id, failure| wanted.get(id) == Some(&failure.0));
    progress.indexed = Some((indexed.len() - plan.remove.len()) as u64);
    Ok(plan)
}

/// Every document of the catalog -- any state, any storage, Health's left
/// out -- one at a time.
fn each_document(vault: &Vault, mut each: impl FnMut(FileEntry)) -> CoreResult<()> {
    let mut offset = 0;
    loop {
        let page = vault.files(&FileQuery {
            filter: FileFilter {
                kinds: vec![FileKind::Document],
                status: StatusFilter::All,
                exclude_origins: vec![FileOrigin::Health],
                include_hidden: true,
                sort: FileSort::Added,
                limit: Some(CATALOG_PAGE),
                offset,
                ..FileFilter::default()
            },
            ..FileQuery::default()
        })?;
        let got = page.files.len();
        let documents = page.files.into_iter().filter(|file| DocFormat::of_file(file).is_some());
        documents.for_each(&mut each);
        if got < CATALOG_PAGE as usize {
            return Ok(());
        }
        offset += CATALOG_PAGE;
    }
}

/// A hash of everything a pass would have to look at: the settings that
/// choose what is indexed, and every document of the catalog's id, path,
/// contents hash, size and time. Read from the catalog's clear columns, so
/// it opens none of the files' records. Two passes that see the same hash
/// have the same work to do.
fn fingerprint(vault: &Vault, settings: &FilesSettings) -> CoreResult<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[u8::from(settings.index_documents)]);
    for folders in [&settings.index_folders, &settings.skip_folders] {
        for folder in folders {
            hasher.update(folder.as_bytes());
            hasher.update(&[0]);
        }
        hasher.update(&[1]);
    }
    let mut rows = vault.file_index()?;
    rows.retain(|row| !row.missing && DocFormat::of_name(&row.path).is_some());
    rows.sort_by_key(|row| row.id);
    for row in rows {
        hasher.update(row.id.as_uuid().as_bytes());
        hasher.update(row.path.as_bytes());
        hasher.update(&[0]);
        hasher.update(row.hash.as_bytes());
        hasher.update(&row.size.to_le_bytes());
        hasher.update(&row.modified_us.to_le_bytes());
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// [`fingerprint`] with the settings read for it.
fn fingerprint_of(vault: &Vault) -> CoreResult<String> {
    fingerprint(vault, &vault.files_settings()?)
}

/// The words a title search finds a document by: its own title and authors,
/// and the name of its file when that says something else -- a PDF's own
/// title is often "Microsoft Word - draft3", and its file's name is what
/// somebody remembers it by.
pub(crate) fn title_of(text: &DocText, file: &FileEntry) -> String {
    let stem = file.name.rsplit_once('.').map_or(file.name.as_str(), |(stem, _)| stem).trim();
    let own = text.meta.title.trim();
    let authors =
        text.meta.authors.iter().map(|a| a.trim()).filter(|a| !a.is_empty()).collect::<Vec<_>>();
    let mut parts: Vec<String> = Vec::new();
    parts.push(if own.is_empty() { stem.to_string() } else { own.to_string() });
    if !authors.is_empty() {
        parts.push(authors.join(", "));
    }
    if !own.is_empty() && !stem.is_empty() && !own.eq_ignore_ascii_case(stem) {
        parts.push(stem.to_string());
    }
    parts.join(" \u{2014} ")
}

/// A passage being made up.
struct Open {
    block: u32,
    page: u32,
    section: String,
    text: String,
    chars: usize,
}

/// A document as the passages the index holds: consecutive blocks of the
/// same section up to about [`PASSAGE_CHARS`], never across a section's start,
/// each carrying its first block's index and page and the innermost section's
/// title.
///
/// A block with no words (a picture with no description, a break between
/// scenes) is skipped. One longer than [`OVERSIZE_CHARS`] is cut into
/// passages of its own, which all name that block. A heading starts a
/// passage, so a search for a chapter's name lands on the chapter's start.
pub(crate) fn passages_of(doc: &DocText) -> Vec<Passage> {
    let starts: HashSet<usize> = doc.sections.iter().map(|s| s.block as usize).collect();
    let title_of_section = |section: Option<u32>| -> String {
        section
            .and_then(|at| doc.sections.get(at as usize))
            .map(|s| s.title.trim().to_string())
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    let mut open: Option<Open> = None;
    let close = |open: &mut Option<Open>, out: &mut Vec<Passage>| {
        if let Some(done) = open.take() {
            out.push(Passage {
                block: done.block,
                page: done.page,
                section: done.section,
                text: done.text,
            });
        }
    };
    let mut current_section: Option<Option<u32>> = None;
    for (at, block) in doc.blocks.iter().enumerate() {
        let words = block.text.trim();
        if words.is_empty() {
            continue;
        }
        let first = u32::try_from(at).unwrap_or(u32::MAX);
        // A section's start closes the passage before it; so does the
        // innermost section changing, for a document whose parser left the
        // starts out.
        if starts.contains(&at) || current_section.is_some_and(|had| had != block.section) {
            close(&mut open, &mut out);
        }
        current_section = Some(block.section);
        let section = title_of_section(block.section);
        let chars = words.chars().count();
        if chars > OVERSIZE_CHARS {
            close(&mut open, &mut out);
            for piece in pieces_of(words, PASSAGE_CHARS) {
                out.push(Passage {
                    block: first,
                    page: block.page,
                    section: section.clone(),
                    text: piece,
                });
            }
            continue;
        }
        match open.as_mut() {
            Some(passage) if passage.chars + 1 + chars <= PASSAGE_CHARS => {
                passage.text.push('\n');
                passage.text.push_str(words);
                passage.chars += 1 + chars;
            }
            _ => {
                close(&mut open, &mut out);
                open = Some(Open {
                    block: first,
                    page: block.page,
                    section,
                    text: words.to_string(),
                    chars,
                });
            }
        }
    }
    close(&mut open, &mut out);
    out
}

/// `text` cut into pieces of at most `max` characters, between words where
/// there is a space to cut at and anywhere where there is not (a long run of
/// characters with no space in it).
fn pieces_of(text: &str, max: usize) -> Vec<String> {
    /// Finish the piece being made, if it has anything in it.
    fn finish(pieces: &mut Vec<String>, piece: &mut String, chars: &mut usize) {
        if !piece.is_empty() {
            pieces.push(std::mem::take(piece));
            *chars = 0;
        }
    }
    let mut pieces = Vec::new();
    let (mut piece, mut chars) = (String::new(), 0);
    for word in text.split_whitespace() {
        let (mut word, mut length) = (word, word.chars().count());
        while length > max {
            // One word longer than a passage: cut it where it must be.
            finish(&mut pieces, &mut piece, &mut chars);
            let cut = word.char_indices().nth(max).map_or(word.len(), |(at, _)| at);
            pieces.push(word[..cut].to_string());
            word = &word[cut..];
            length -= max;
        }
        if chars > 0 && chars + 1 + length > max {
            finish(&mut pieces, &mut piece, &mut chars);
        }
        if chars > 0 {
            piece.push(' ');
            chars += 1;
        }
        piece.push_str(word);
        chars += length;
    }
    finish(&mut pieces, &mut piece, &mut chars);
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;
    use karmayoga_core::VaultConfig;
    use karmayoga_core::documents::{DocBlock, DocBlockKind, DocSection};
    use karmayoga_core::files::ImportOptions;

    fn block(at_page: u32, section: Option<u32>, text: &str) -> DocBlock {
        DocBlock { page: at_page, section, text: text.into(), ..DocBlock::default() }
    }

    fn heading(page: u32, section: Option<u32>, text: &str) -> DocBlock {
        DocBlock { kind: DocBlockKind::Heading, level: 1, ..block(page, section, text) }
    }

    fn section(title: &str, at_block: u32) -> DocSection {
        DocSection { title: title.into(), level: 1, page: 1, block: at_block, parent: None }
    }

    // ---- passages ------------------------------------------------------------

    #[test]
    fn short_blocks_of_a_section_share_a_passage_and_a_section_start_opens_a_new_one() {
        let mut doc = DocText::new(DocFormat::Epub);
        doc.sections = vec![section("Arrival", 0), section("The Orchard", 3)];
        doc.blocks = vec![
            heading(1, Some(0), "Arrival"),
            block(1, Some(0), "Mortimer came down the hill."),
            block(2, Some(0), "The inn was shut."),
            heading(2, Some(1), "The Orchard"),
            block(3, Some(1), "In the orchard he found it."),
        ];
        let passages = passages_of(&doc);
        assert_eq!(passages.len(), 2, "{passages:?}");
        assert_eq!(
            (passages[0].block, passages[0].page, passages[0].section.as_str()),
            (0, 1, "Arrival")
        );
        assert_eq!(passages[0].text, "Arrival\nMortimer came down the hill.\nThe inn was shut.");
        assert_eq!(
            (passages[1].block, passages[1].page, passages[1].section.as_str()),
            (3, 2, "The Orchard")
        );
        assert_eq!(passages[1].text, "The Orchard\nIn the orchard he found it.");
    }

    #[test]
    fn a_section_start_closes_a_passage_even_where_the_blocks_do_not_name_their_sections() {
        let mut doc = DocText::new(DocFormat::Pdf);
        doc.sections = vec![section("One", 0), section("Two", 2)];
        doc.blocks =
            vec![block(1, None, "first"), block(1, None, "second"), block(2, None, "third")];
        let passages = passages_of(&doc);
        assert_eq!(passages.iter().map(|p| p.block).collect::<Vec<_>>(), vec![0, 2]);
        assert_eq!(passages[0].text, "first\nsecond");
        // Not told which section the blocks are in: no title, but still apart.
        assert!(passages.iter().all(|p| p.section.is_empty()));
    }

    #[test]
    fn a_passage_is_closed_when_the_next_block_would_make_it_too_long() {
        let mut doc = DocText::new(DocFormat::Text);
        let paragraph = "word ".repeat(100); // 499 characters once trimmed
        doc.blocks = vec![
            block(1, None, &paragraph),
            block(1, None, &paragraph),
            block(1, None, &paragraph),
        ];
        let passages = passages_of(&doc);
        assert_eq!(passages.len(), 2, "two fit in one, the third starts another");
        assert_eq!((passages[0].block, passages[1].block), (0, 2));
        assert!(passages.iter().all(|p| p.text.chars().count() <= PASSAGE_CHARS));
    }

    #[test]
    fn blocks_with_no_words_are_skipped_and_a_huge_block_is_cut_at_the_same_address() {
        let mut doc = DocText::new(DocFormat::Text);
        doc.blocks = vec![
            DocBlock { kind: DocBlockKind::Image, src: "a.png".into(), ..block(1, None, "") },
            DocBlock { kind: DocBlockKind::Rule, ..block(1, None, "  ") },
            block(1, None, "before"),
            block(2, None, &"long ".repeat(2_000)),
            block(3, None, "after"),
        ];
        let passages = passages_of(&doc);
        // "before", then the long block alone in several, then "after".
        assert_eq!(passages[0].text, "before");
        assert_eq!(passages[0].block, 2, "the address is the block's, empty ones counted");
        let long: Vec<_> = passages.iter().filter(|p| p.block == 3).collect();
        assert!(long.len() >= 8, "{}", long.len());
        assert!(long.iter().all(|p| p.page == 2 && p.text.chars().count() <= PASSAGE_CHARS));
        assert_eq!(long.iter().map(|p| p.text.split(' ').count()).sum::<usize>(), 2_000);
        let last = passages.last().unwrap();
        assert_eq!((last.block, last.text.as_str()), (4, "after"));
        assert!(passages.iter().all(|p| !p.text.trim().is_empty()));
    }

    #[test]
    fn a_word_longer_than_a_passage_is_cut_where_it_must_be() {
        let pieces = pieces_of(&"x".repeat(2_500), 1_000);
        let lengths =
            |pieces: &[String]| pieces.iter().map(|p| p.chars().count()).collect::<Vec<_>>();
        assert_eq!(lengths(&pieces), [1_000, 1_000, 500]);
        // Between words where it can be.
        let pieces = pieces_of("aaa bbb ccc ddd", 7);
        assert_eq!(pieces, ["aaa bbb", "ccc ddd"]);
        assert!(pieces_of("   ", 10).is_empty());
        // Cut on characters, not bytes.
        let pieces = pieces_of(&"\u{e9}".repeat(25), 10);
        assert_eq!(lengths(&pieces), [10, 10, 5]);
    }

    #[test]
    fn a_document_with_no_text_has_no_passages() {
        assert!(passages_of(&DocText::new(DocFormat::Pdf)).is_empty());
    }

    #[test]
    fn a_title_is_the_documents_own_with_its_authors_and_the_name_it_is_filed_under() {
        let (_home, _folder, _svc, vault) = with_folder();
        let file = plain(&vault, "dune_herbert_2011.epub", b"x");
        let mut text = DocText::new(DocFormat::Epub);
        // Nothing said: the file's name without its extension.
        assert_eq!(title_of(&text, &file), "dune_herbert_2011");
        text.meta.title = "Dune".into();
        text.meta.authors = vec!["Frank Herbert".into(), " ".into()];
        let full = "Dune \u{2014} Frank Herbert \u{2014} dune_herbert_2011";
        assert_eq!(title_of(&text, &file), full);
        // The name is not said twice.
        text.meta.title = "DUNE_HERBERT_2011".into();
        text.meta.authors.clear();
        assert_eq!(title_of(&text, &file), "DUNE_HERBERT_2011");
    }

    // ---- planning and passes ---------------------------------------------------

    fn open(home: &std::path::Path) -> (Arc<Service>, Arc<Vault>) {
        let config = VaultConfig { password: None, ..Default::default() };
        let vault = karmayoga_vault::create(home, config).unwrap();
        let svc = Arc::new(Service::new());
        let vault = svc.set(vault);
        (svc, vault)
    }

    fn with_folder() -> (tempfile::TempDir, tempfile::TempDir, Arc<Service>, Arc<Vault>) {
        let (home, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (svc, vault) = open(home.path());
        vault.set_up_files_folder(Some(folder.path())).unwrap();
        (home, folder, svc, vault)
    }

    fn plain(vault: &Vault, name: &str, bytes: &[u8]) -> FileEntry {
        vault.import_bytes(bytes, name, &ImportOptions::new(FileOrigin::Files)).unwrap()
    }

    /// A pass over `vault`, as the task would run one, to its end.
    async fn run_pass(svc: &Arc<Service>, vault: &Arc<Vault>, seen: &mut Option<String>) -> End {
        let (_keep, stop) = watch::channel(false);
        pass(svc, vault, &stop, seen, true).await
    }

    fn hits(state: &DocumentsState, vault: &Vault, query: &str) -> Vec<FileId> {
        let search = karmayoga_core::documents::DocSearch {
            text: query.into(),
            limit: 20,
            ..Default::default()
        };
        let mut files: Vec<FileId> = Vec::new();
        for hit in state.search(vault, &search).unwrap().hits {
            // A title and a passage can both match: one file, once.
            if !files.contains(&hit.file) {
                files.push(hit.file);
            }
        }
        files
    }

    #[tokio::test]
    async fn a_pass_indexes_what_is_new_and_changed_and_forgets_what_is_gone() {
        let (_home, folder, svc, vault) = with_folder();
        let state = svc.documents().unwrap();
        let (dune, kite) = (
            plain(&vault, "dune.txt", b"The spice must flow on Arrakis."),
            plain(&vault, "kite.md", b"# Kites\n\nThe kite rose over the hills."),
        );
        plain(&vault, "photo.png", b"\x89PNG\r\n\x1a\n not a document");

        let mut seen = None;
        assert_eq!(run_pass(&svc, &vault, &mut seen).await, End::Done { swept: true });
        assert!(seen.is_some());
        let indexed = state.index().unwrap().indexed().unwrap();
        assert_eq!(indexed.len(), 2, "the picture is not a document: {indexed:?}");
        assert_eq!(indexed.get(&dune.id), Some(&state.version_of(&dune)));
        assert_eq!(hits(&state, &vault, "arrakis"), vec![dune.id]);
        assert_eq!(hits(&state, &vault, "kite"), vec![kite.id]);

        let status = state.status(&vault);
        assert_eq!((status.enabled, status.indexed, status.pending), (true, 2, 0));
        assert!(!status.running && status.last_pass.is_some() && status.failed.is_empty());

        // Nothing changed: nothing is read again.
        let before = state.cache().len();
        assert_eq!(run_pass(&svc, &vault, &mut seen).await, End::Done { swept: true });
        assert_eq!(state.cache().len(), before);

        // Changed on disk and found by the reconciler: its new words, not its old.
        std::fs::write(folder.path().join("dune.txt"), b"Sandworms under Arrakeen sand.").unwrap();
        crate::files::task::scan_now(&svc, &vault);
        assert_eq!(run_pass(&svc, &vault, &mut seen).await, End::Done { swept: true });
        assert!(hits(&state, &vault, "arrakis").is_empty());
        assert_eq!(hits(&state, &vault, "sandworms"), vec![dune.id]);

        // Trashed: out of the index at the next pass.
        vault.trash_files(&[kite.id]).unwrap();
        assert_eq!(run_pass(&svc, &vault, &mut seen).await, End::Done { swept: true });
        assert!(hits(&state, &vault, "kite").is_empty());
        let indexed = state.index().unwrap().indexed().unwrap();
        assert_eq!(indexed.keys().copied().collect::<Vec<_>>(), vec![dune.id]);
    }

    #[tokio::test]
    async fn only_the_chosen_folders_are_indexed_and_choosing_again_changes_the_index() {
        let (_home, folder, svc, vault) = with_folder();
        let state = svc.documents().unwrap();
        for dir in ["Books", "Receipts"] {
            std::fs::create_dir(folder.path().join(dir)).unwrap();
        }
        let write = |path: &str, words: &[u8]| std::fs::write(folder.path().join(path), words);
        write("Books/novel.txt", b"A novel about a lighthouse.").unwrap();
        write("Receipts/till.txt", b"A till receipt for a lighthouse.").unwrap();
        crate::files::task::scan_now(&svc, &vault);

        let mut settings = vault.files_settings().unwrap();
        settings.index_folders = vec!["Books".into()];
        vault.save_files_settings(&settings).unwrap();
        let mut seen = None;
        run_pass(&svc, &vault, &mut seen).await;
        assert_eq!(hits(&state, &vault, "lighthouse").len(), 1);

        // Both chosen, one skipped.
        settings.index_folders = vec!["Books".into(), "Receipts".into()];
        settings.skip_folders = vec!["Books".into()];
        vault.save_files_settings(&settings).unwrap();
        let before = seen.clone();
        run_pass(&svc, &vault, &mut seen).await;
        assert_ne!(seen, before, "the settings are part of what a pass sees");
        let found = hits(&state, &vault, "lighthouse");
        assert_eq!(found.len(), 1);
        let till = vault.file(found[0]).unwrap();
        assert_eq!(till.path(), Some("Receipts/till.txt"));
    }

    #[tokio::test]
    async fn switched_off_the_index_is_emptied_once_and_left_empty() {
        let (_home, _folder, svc, vault) = with_folder();
        let state = svc.documents().unwrap();
        plain(&vault, "notes.txt", b"some words to find");
        let mut seen = None;
        run_pass(&svc, &vault, &mut seen).await;
        assert_eq!(state.index().unwrap().indexed().unwrap().len(), 1);

        let mut settings = vault.files_settings().unwrap();
        settings.index_documents = false;
        vault.save_files_settings(&settings).unwrap();
        run_pass(&svc, &vault, &mut seen).await;
        assert!(state.index().unwrap().indexed().unwrap().is_empty());
        let status = state.status(&vault);
        assert_eq!((status.enabled, status.indexed), (false, 0));

        // On again: filled again.
        settings.index_documents = true;
        vault.save_files_settings(&settings).unwrap();
        run_pass(&svc, &vault, &mut seen).await;
        assert_eq!(state.index().unwrap().indexed().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_file_that_cannot_be_read_is_listed_once_and_not_tried_again_until_it_changes() {
        let (_home, folder, svc, vault) = with_folder();
        let state = svc.documents().unwrap();
        // A "PDF" that is not one.
        let bad = plain(&vault, "broken.pdf", b"this is not a pdf at all");
        let good = plain(&vault, "fine.txt", b"readable words");

        let mut seen = None;
        run_pass(&svc, &vault, &mut seen).await;
        let status = state.status(&vault);
        assert_eq!(status.failed.len(), 1, "{:?}", status.failed);
        assert_eq!(status.failed[0].file, bad.id);
        assert!(status.failed[0].reason.contains("broken.pdf"), "{}", status.failed[0].reason);
        let indexed = state.index().unwrap().indexed().unwrap();
        assert_eq!(indexed.keys().copied().collect::<Vec<_>>(), vec![good.id]);

        // The same version again: the plan has no work for it.
        let planned = plan(&state, &vault).unwrap();
        assert!(planned.work.iter().all(|f| f.id != bad.id));
        assert!(planned.work.is_empty());

        // New contents: tried again (and fails again, for a new reason to give).
        let changed = b"still not a pdf, but different";
        std::fs::write(folder.path().join("broken.pdf"), changed).unwrap();
        crate::files::task::scan_now(&svc, &vault);
        let planned = plan(&state, &vault).unwrap();
        assert_eq!(planned.work.iter().map(|f| f.id).collect::<Vec<_>>(), vec![bad.id]);

        // A reindex forgets what failed, and asks for everything.
        state.reindex();
        assert!(state.status(&vault).failed.is_empty());
        let planned = plan(&state, &vault).unwrap();
        assert_eq!(planned.work.len(), 2);
    }

    #[tokio::test]
    async fn a_pass_that_is_told_to_stop_stops_between_documents_and_keeps_what_it_did() {
        let (_home, _folder, svc, vault) = with_folder();
        let state = svc.documents().unwrap();
        plain(&vault, "a.txt", b"alpha words");
        plain(&vault, "b.txt", b"beta words");
        let (tx, stop) = watch::channel(true);
        let mut seen = None;
        assert_eq!(pass(&svc, &vault, &stop, &mut seen, false).await, End::Stopped);
        assert!(!state.status(&vault).running, "no longer running once it has stopped");
        drop(tx);
    }

    #[test]
    fn the_catalogs_fingerprint_follows_the_documents_and_the_settings_and_nothing_else() {
        let (_home, folder, svc, vault) = with_folder();
        plain(&vault, "a.txt", b"alpha");
        let first = fingerprint_of(&vault).unwrap();
        assert_eq!(fingerprint_of(&vault).unwrap(), first);

        // A file that is not a document does not move it.
        plain(&vault, "photo.png", b"\x89PNG\r\n\x1a\n");
        assert_eq!(fingerprint_of(&vault).unwrap(), first);

        // A document arriving, or changing, does.
        plain(&vault, "b.md", b"beta");
        let second = fingerprint_of(&vault).unwrap();
        assert_ne!(second, first);
        std::fs::write(folder.path().join("b.md"), b"beta, changed").unwrap();
        crate::files::task::scan_now(&svc, &vault);
        let third = fingerprint_of(&vault).unwrap();
        assert_ne!(third, second);

        // And so do the settings that choose what is indexed.
        let mut settings = vault.files_settings().unwrap();
        settings.skip_folders = vec!["Private".into()];
        vault.save_files_settings(&settings).unwrap();
        assert_ne!(fingerprint_of(&vault).unwrap(), third);
    }
}
