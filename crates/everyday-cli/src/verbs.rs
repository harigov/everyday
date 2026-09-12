//! The verbs: what each subcommand actually does to an open vault.
//!
//! Every function here takes an already-open `Vault` -- deciding whether
//! one exists, opening it and asking for a password is `app::run`'s job,
//! not this file's, so a verb here can be read as "given a vault, do this"
//! without also carrying the setup that gets it one.

use crate::args::JournalCmd;
use crate::format::{human_bytes, truncate};
use everyday_core::crypto::KdfParams;
use everyday_core::model::{Attachment, MediaKind};
use everyday_core::search::{Found, SearchScope};
use everyday_core::store::SortOrder;
use everyday_core::{
    Entry, EntryQuery, Error, Journal, JournalId, Result, RichDoc, Vault, VaultConfig,
};
use jiff::civil::Date;
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

fn parse_settings(pairs: &[String]) -> Result<everyday_core::BackendSettings> {
    let mut settings = everyday_core::BackendSettings::new();
    for pair in pairs {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| Error::Invalid(format!("--set expects key=value, got {pair:?}")))?;
        settings.set(key.trim(), value);
    }
    Ok(settings)
}

pub(crate) fn init(
    path: &std::path::Path,
    backend: &str,
    settings: &[String],
    name: &str,
    no_encryption: bool,
    supplied: Option<&str>,
) -> Result<()> {
    let settings = parse_settings(settings)?;
    // Checked before a password is asked for, so a typo in the backend name
    // or a missing connection URL is reported now rather than after two
    // prompts and a KDF.
    everyday_vault::validate_settings(backend, &settings)?;

    let password = if no_encryption {
        None
    } else {
        let p = match supplied {
            Some(p) => p.to_string(),
            None => {
                let a = prompt_password("Choose a password: ")?;
                let b = prompt_password("Repeat it: ")?;
                if a != b {
                    return Err(Error::Invalid("the two passwords do not match".into()));
                }
                a
            }
        };
        everyday_vault::validate_password(&p)?;
        Some(p)
    };

    let encrypted = password.is_some();
    let vault = everyday_vault::create(
        path,
        VaultConfig {
            name: name.to_string(),
            backend: backend.to_string(),
            settings,
            password,
            kdf: KdfParams::default(),
            auto_lock_seconds: 15 * 60,
            // Never, by default. The machine holding a vault serves it -- to its
            // own window, to a phone, to the assistant -- and a key that went
            // away because one keyboard was idle would take all of that with it.
            forget_key_seconds: 0,
        },
    )?;

    // A vault with no journal is a dead end; give it one.
    vault.save_journal(&Journal::new("Journal"))?;

    println!("created {} vault at {}", backend, path.display());
    if encrypted {
        println!(
            "\nThere is no way to recover this vault without the password.\n\
             It is not stored anywhere, and it cannot be reset."
        );
    } else {
        println!(
            "\nWarning: this vault is NOT encrypted. Anything written to it is stored in the clear."
        );
    }
    Ok(())
}

/// Show or amend the backend's settings.
///
/// Secrets are never printed back. A connection URL has a password in it, and
/// the whole reason it is sealed in the header is that it should not be
/// sitting somewhere it can be read -- which includes a terminal someone else
/// can scroll back through, and a CI log.
pub(crate) fn backend(vault: &Vault, pairs: &[String], supplied: Option<&str>) -> Result<()> {
    let name = vault.status().backend;
    let specs = everyday_vault::backend_settings(&name)?;

    // The settings are sealed under the vault key, so reading them at all
    // needs the password -- and only the password, which is what makes this
    // reachable on a vault that cannot be opened.
    let password = match (vault.status().encrypted, supplied) {
        (false, _) => None,
        (true, Some(p)) => Some(p.to_string()),
        (true, None) => Some(prompt_password("Password: ")?),
    };
    let mut settings = vault.backend_settings(password.as_deref())?;

    if pairs.is_empty() {
        println!("backend    {name}");
        if specs.is_empty() {
            println!("this backend needs no configuration");
            return Ok(());
        }
        for spec in &specs {
            let shown = match (settings.get(spec.key), spec.secret) {
                (Some(_), true) => "(set, not shown)".to_string(),
                (Some(v), false) => v.to_string(),
                (None, _) if spec.required => "(not set)".to_string(),
                (None, _) => "(default)".to_string(),
            };
            println!("{:<10} {shown}", spec.key);
        }
        return Ok(());
    }

    // Merged, not replaced: `--set schema=archive` on a Postgres vault must
    // not take the connection URL with it.
    for (key, value) in parse_settings(pairs)?.into_pairs() {
        settings.set(key, value);
    }
    everyday_vault::validate_settings(&name, &settings)?;
    vault.set_backend_settings(password.as_deref(), settings)?;
    println!("updated; it takes effect the next time this vault is opened");
    Ok(())
}

pub(crate) fn status(vault: &Vault) -> Result<()> {
    let s = vault.status();
    let stats = s.stats.unwrap_or_default();
    println!("name       {}", s.name);
    println!("path       {}", s.path.display());
    println!("backend    {}", s.backend);
    println!("encrypted  {}", if s.encrypted { "yes" } else { "NO" });
    println!(
        "auto-lock  {}",
        if s.auto_lock_seconds == 0 {
            "off".to_string()
        } else {
            format!("{} minutes", s.auto_lock_seconds / 60)
        }
    );
    println!("journals   {}", stats.journals);
    println!("entries    {}", stats.entries);
    println!("media      {} ({})", stats.blobs, human_bytes(stats.blob_bytes));
    Ok(())
}

pub(crate) fn journal(vault: &Vault, cmd: JournalCmd) -> Result<()> {
    match cmd {
        JournalCmd::List => {
            for j in vault.journals()? {
                let n = vault.entries(&EntryQuery::in_journal(j.id))?.len();
                println!("{} {:<24} {:>5} entries  {}", j.icon, j.name, n, j.id);
            }
            Ok(())
        }
        JournalCmd::New { name, color, icon } => {
            let mut j = Journal::new(&name);
            if let Some(c) = color {
                j.color = c;
            }
            if let Some(i) = icon {
                j.icon = i;
            }
            vault.save_journal(&j)?;
            println!("created journal {} ({})", j.name, j.id);
            Ok(())
        }
        JournalCmd::Delete { name } => {
            let j = find_journal(vault, &name)?;
            let n = vault.entries(&EntryQuery::in_journal(j.id))?.len();
            vault.delete_journal(j.id)?;
            println!("deleted journal {} and its {n} entries", j.name);
            Ok(())
        }
    }
}

pub(crate) fn new_entry(
    vault: &Vault,
    journal: Option<String>,
    title: Option<String>,
    tags: Vec<String>,
    date: Option<String>,
    star: bool,
) -> Result<()> {
    let journal = match journal {
        Some(name) => find_journal(vault, &name)?,
        None => vault
            .journals()?
            .into_iter()
            .next()
            .ok_or_else(|| Error::Invalid("this vault has no journals yet".into()))?,
    };

    let mut body = String::new();
    if std::io::stdin().is_terminal() {
        eprintln!("Writing to {}. Finish with Ctrl-D.", journal.name);
    }
    std::io::stdin().read_to_string(&mut body)?;
    if body.trim().is_empty() {
        return Err(Error::Invalid("refusing to save an empty entry".into()));
    }

    let mut entry = Entry::new(journal.id, &everyday_core::model::system_tz());
    entry.body = RichDoc::from_plain_text(body.trim_end());
    entry.title = title.unwrap_or_default();
    entry.tags = tags;
    entry.starred = star;
    if let Some(d) = date {
        entry.local_date =
            d.parse::<Date>().map_err(|e| Error::Invalid(format!("bad --date {d:?}: {e}")))?;
    }
    vault.save_entry(&entry, None)?;
    println!("{}", entry.id);
    Ok(())
}

pub(crate) fn list(
    vault: &Vault,
    journal: Option<String>,
    limit: u32,
    starred: bool,
    json: bool,
) -> Result<()> {
    let query = EntryQuery {
        journal_id: journal.map(|n| find_journal(vault, &n)).transpose()?.map(|j| j.id),
        starred: starred.then_some(true),
        sort: SortOrder::DateDesc,
        limit: Some(limit),
        ..Default::default()
    };
    let rows = vault.entries(&query)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    for r in rows {
        println!(
            "{}  {}{}  {}",
            r.local_date,
            if r.starred { "* " } else { "  " },
            truncate(&r.title, 48),
            r.id.short()
        );
    }
    Ok(())
}

pub(crate) fn show(vault: &Vault, id: &str, json: bool) -> Result<()> {
    let entry = vault.entry(resolve_entry(vault, id)?)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&entry)?);
        return Ok(());
    }
    println!("# {}\n", entry.display_title());
    println!("{}  {}", entry.local_date, entry.tz);
    if !entry.tags.is_empty() {
        println!("tags: {}", entry.tags.join(", "));
    }
    println!("\n{}", entry.body.to_markdown());
    if !entry.attachments.is_empty() {
        println!("\n---\nattachments:");
        for a in &entry.attachments {
            println!("  {} {} ({})", a.filename, a.mime, human_bytes(a.byte_len));
        }
    }
    Ok(())
}

pub(crate) fn search(vault: &Vault, query: &str, limit: usize) -> Result<()> {
    // Entries only. This command is part of a tool that covers journals and
    // nothing else, and quietly returning notes from it would be a surprise
    // in a script somebody wrote against last year's output.
    let hits = vault.search(query, SearchScope::Entries(None), limit)?;
    if hits.is_empty() {
        eprintln!("no matches for {query:?}");
        return Ok(());
    }
    for h in hits {
        let Found::Entry { id, local_date, .. } = h.found else { continue };
        println!("{}  {}  [{}]", local_date, truncate(&h.title, 48), id.short());
        if !h.snippet.is_empty() {
            println!("    {}", h.snippet.replace('\n', " "));
        }
    }
    Ok(())
}

pub(crate) fn attach(vault: &Vault, id: &str, file: &std::path::Path) -> Result<()> {
    let bytes = std::fs::read(file).map_err(|e| Error::io(file, e))?;
    let mut entry = vault.entry(resolve_entry(vault, id)?)?;
    // The version being replaced, captured before the stamp below overwrites
    // it. Attaching is a read-modify-write, so it is exactly the shape that
    // loses somebody else's edit if it does not check.
    let loaded = entry.updated_at;

    let mime = mime_guess::from_path(file).first_or_octet_stream().to_string();
    let blob = vault.put_blob(&bytes)?;
    entry.attachments.push(Attachment {
        blob,
        kind: MediaKind::from_mime(&mime),
        mime,
        filename: file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attachment".into()),
        byte_len: bytes.len() as u64,
        width: None,
        height: None,
        duration_ms: None,
        caption: String::new(),
    });
    entry.updated_at = jiff::Timestamp::now();
    vault.save_entry(&entry, Some(loaded))?;
    println!("attached {} ({})", file.display(), human_bytes(bytes.len() as u64));
    Ok(())
}

pub(crate) fn delete(vault: &Vault, id: &str) -> Result<()> {
    let id = resolve_entry(vault, id)?;
    vault.delete_entry(id)?;
    println!("deleted {id}");
    Ok(())
}

pub(crate) fn backup(vault: &Vault, dir: &std::path::Path) -> Result<()> {
    vault.backup(dir)?;
    println!("backed up to {}", dir.display());
    println!("it opens with the same password: everyday --vault {} list", dir.display());
    Ok(())
}

pub(crate) fn check(vault: &Vault) -> Result<()> {
    let problems = vault.check_integrity()?;
    if problems.is_empty() {
        println!("no damage found");
        return Ok(());
    }
    eprintln!("{} problem(s) found:", problems.len());
    for p in &problems {
        eprintln!("  {p}");
    }
    eprintln!("\nrestore from a backup; `everyday export` may still salvage readable entries");
    // A non-zero exit so this is usable from a cron line.
    std::process::exit(1);
}

/// Write the archive, as a zip or as the folder it would unzip to.
///
/// The folder form is not a lesser one: it is the same files, and it is what
/// somebody piping an export into `git` or `rsync` actually wants. Which one
/// you get is decided by the extension, because that is the thing a person
/// has already said by typing the path.
pub(crate) fn export(
    vault: &Vault,
    path: Option<&std::path::Path>,
    parts: Vec<String>,
    media: bool,
    list: bool,
) -> Result<()> {
    if list {
        for part in everyday_transfer::survey(vault)? {
            println!("{:<10} {:>8}  {}", part.id, part.records, part.summary);
        }
        return Ok(());
    }
    let Some(path) = path else {
        return Err(Error::Invalid("say where to write it, or use --list".into()));
    };
    for id in &parts {
        if !everyday_transfer::PARTS.iter().any(|p| p.spec().id == id) {
            return Err(Error::Invalid(format!(
                "no such part: {id} -- `everyday export --list` says which there are"
            )));
        }
    }
    let opts = everyday_transfer::Options { parts, media };

    let manifest = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        // Straight to the file rather than through memory: this is the path
        // that has no ceiling, and the reason the settings dialog can point
        // at it for a vault too big to hold in one.
        let file = std::fs::File::create(path).map_err(|e| Error::io(path, e))?;
        let (file, manifest) =
            everyday_transfer::export(vault, &opts, std::io::BufWriter::new(file))?;
        file.into_inner().map_err(|e| Error::io(path, e.into_error()))?;
        manifest
    } else {
        let mut sink = Folder { root: path.to_path_buf() };
        everyday_transfer::write_into(vault, &opts, &mut sink)?
    };

    println!(
        "exported {} record(s) in {} file(s) to {}",
        manifest.records(),
        manifest.files(),
        path.display()
    );
    for part in &manifest.parts {
        println!("  {:<10} {:>8}  {}", part.id, part.records, part.format);
    }
    if !media {
        println!("\nwithout attachments: the words are here, the pictures are not");
    }
    Ok(())
}

/// A [`Sink`](everyday_transfer::Sink) that is a directory on disk.
struct Folder {
    root: PathBuf,
}

impl everyday_transfer::Sink for Folder {
    fn put(&mut self, name: &str, body: &[u8]) -> everyday_core::Result<()> {
        // Names come from the parts, which build them out of `safe_name`, so
        // this is a second lock on a door that is already shut. It is here
        // because this is the one place where a name becomes a real path, and
        // that is exactly where the check belongs rather than where it is
        // convenient.
        if name.contains("..") || name.starts_with('/') || name.contains('\\') {
            return Err(Error::Invalid(format!("{name} is not a path an export may write")));
        }
        let path = name.split('/').fold(self.root.clone(), |at, part| at.join(part));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        std::fs::write(&path, body).map_err(|e| Error::io(&path, e))
    }
}

/// Read an archive, or a folder, back into the vault.
pub(crate) fn import(
    vault: &Vault,
    path: &std::path::Path,
    parts: Vec<String>,
    replace: bool,
    yes: bool,
) -> Result<()> {
    let archive = if path.is_dir() {
        // A folder is read as though it had been zipped. Somebody who
        // unpacked an export, edited it and never rezipped it should not have
        // to, and a folder of Markdown from another program is the same shape.
        let mut files = std::collections::BTreeMap::new();
        collect(path, path, &mut files)?;
        everyday_transfer::zip::Reader::from_files(files)
    } else {
        let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
        everyday_transfer::zip::Reader::open(&bytes)?
    };

    let manifest = everyday_transfer::inspect(&archive)?;
    println!("{}", path.display());
    if !manifest.vault.is_empty() {
        println!("  from the vault \"{}\"", manifest.vault);
    }
    for part in &manifest.parts {
        let note = if part.imports { "" } else { "  (a reading copy; not read back)" };
        println!("  {:<10} {:>8} file(s){note}", part.id, part.files);
    }

    let chosen: Vec<String> = if parts.is_empty() {
        manifest.parts.iter().filter(|p| p.imports).map(|p| p.id.clone()).collect()
    } else {
        parts
    };
    let mode =
        if replace { everyday_transfer::Mode::Replace } else { everyday_transfer::Mode::Skip };

    if !yes {
        println!(
            "\nnothing has been read in. `--yes` does it{}.",
            if replace { ", replacing what this vault already has" } else { "" }
        );
        return Ok(());
    }

    let reports = everyday_transfer::import(vault, &archive, &chosen, mode)?;
    let mut problems = 0;
    for report in &reports {
        println!(
            "  {:<10} {} added, {} replaced, {} left alone",
            report.part, report.added, report.replaced, report.skipped
        );
        for problem in &report.problems {
            eprintln!("    {problem}");
            problems += 1;
        }
    }
    if problems > 0 {
        // Said out loud rather than buried above it: an import that quietly
        // dropped four files is how somebody discovers a gap in a year.
        eprintln!("\n{problems} file(s) could not be read; everything else was");
    }
    Ok(())
}

/// Every file under `dir`, keyed by its path relative to `root`.
fn collect(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut std::collections::BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir).map_err(|e| Error::io(dir, e))? {
        let path = entry.map_err(|e| Error::io(dir, e))?.path();
        if path.is_dir() {
            collect(root, &path, out)?;
        } else {
            let name = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.insert(name, std::fs::read(&path).map_err(|e| Error::io(&path, e))?);
        }
    }
    Ok(())
}

pub(crate) fn passwd(vault: &Vault, supplied: Option<&str>) -> Result<()> {
    let current = match supplied {
        Some(p) => p.to_string(),
        None => prompt_password("Current password: ")?,
    };
    let a = prompt_password("New password: ")?;
    let b = prompt_password("Repeat it: ")?;
    if a != b {
        return Err(Error::Invalid("the two passwords do not match".into()));
    }
    everyday_vault::validate_password(&a)?;
    vault.change_password(Some(&current), Some(&a))?;
    println!("password changed");
    Ok(())
}

pub(crate) fn demo(vault: &Vault) -> Result<()> {
    const SAMPLES: &[(&str, &str, &str, &[&str])] = &[
        (
            "Daily",
            "First frost",
            "The grass was stiff underfoot this morning and the light came in \
             low and orange across the field.\n\nI stood at the gate longer \
             than I meant to.",
            &["winter", "walking"],
        ),
        (
            "Daily",
            "A slow Sunday",
            "Bread, coffee, and most of a book. Nothing happened, which was \
             the entire point of it.",
            &["rest"],
        ),
        (
            "Travel",
            "Arriving in Lisbon",
            "The taxi driver took the long way along the river so I could see \
             the bridge lit up. Worth every extra euro.",
            &["portugal", "travel"],
        ),
        (
            "Travel",
            "The train to Porto",
            "Three hours of eucalyptus and red roofs. I read one page and \
             looked out of the window for the rest.",
            &["portugal", "trains"],
        ),
    ];

    let mut created = 0;
    for (journal_name, title, body, tags) in SAMPLES {
        let journal = match find_journal(vault, journal_name) {
            Ok(j) => j,
            Err(_) => {
                let j = Journal::new(*journal_name);
                vault.save_journal(&j)?;
                j
            }
        };
        let mut entry = Entry::new(journal.id, &everyday_core::model::system_tz());
        entry.title = (*title).into();
        entry.body = RichDoc::from_plain_text(body);
        entry.tags = tags.iter().map(|t| t.to_string()).collect();
        entry.local_date =
            everyday_core::model::today_local().saturating_sub(jiff::Span::new().days(created));
        vault.save_entry(&entry, None)?;
        created += 1;
    }
    println!("added {created} sample entries");
    Ok(())
}

// ---- helpers ------------------------------------------------------------

pub(crate) fn prompt_password(prompt: &str) -> Result<String> {
    // Never echo, and never fall back to a visible read: a silent downgrade
    // would put the password in the user's scrollback.
    if !std::io::stdin().is_terminal() {
        return Err(Error::Invalid(
            "no terminal to read a password from; set EVERYDAY_PASSWORD".into(),
        ));
    }
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    rpassword::read_password().map_err(|e| Error::Invalid(format!("could not read password: {e}")))
}

/// Resolve a journal by exact name, case-insensitive name, or id.
fn find_journal(vault: &Vault, needle: &str) -> Result<Journal> {
    let journals = vault.journals()?;
    if let Ok(id) = needle.parse::<JournalId>()
        && let Some(j) = journals.iter().find(|j| j.id == id)
    {
        return Ok(j.clone());
    }
    journals
        .iter()
        .find(|j| j.name == needle)
        .or_else(|| journals.iter().find(|j| j.name.eq_ignore_ascii_case(needle)))
        .cloned()
        .ok_or_else(|| Error::not_found("journal", needle))
}

/// Accept a full entry id or any unambiguous prefix of one.
fn resolve_entry(vault: &Vault, needle: &str) -> Result<everyday_core::EntryId> {
    if let Ok(id) = needle.parse::<everyday_core::EntryId>() {
        return Ok(id);
    }
    // Accept either end: a prefix (what you get from copying an id) or the
    // short form shown in listings, which is the trailing digits.
    let needle = needle.to_lowercase().replace('-', "");
    let matches: Vec<_> = vault
        .entries(&EntryQuery::default())?
        .into_iter()
        .filter(|e| {
            let hex = e.id.as_uuid().simple().to_string();
            hex.starts_with(&needle) || hex.ends_with(&needle)
        })
        .collect();
    match matches.len() {
        1 => Ok(matches[0].id),
        0 => Err(Error::not_found("entry", needle)),
        n => Err(Error::Invalid(format!("{needle:?} matches {n} entries; use more characters"))),
    }
}

/// Run one of the assistant's tools.
///
/// Through [`everyday_service::Service::call`] rather than
/// `everyday_core::agent::tools::dispatch` directly, which is what this used
/// to do: `dispatch` alone knows nothing about whether a tool is even on
/// offer or about to delete something without asking, so this function used
/// to carry its own copy of exactly the checks
/// `everyday_service::domains::meta::run_tool` already makes -- is the name
/// known, is the tool available on this vault, does a destructive one have
/// its confirmation -- with its own wording for each refusal. The two had
/// no way to agree once one of them changed; going through the service
/// leaves one copy of that logic; the CLI's part is building the same
/// `{ name, arguments, confirmDestructive }` any other caller of
/// `run_tool` sends.
///
/// A caller of this function reads a *service* error message now rather
/// than one written for this binary specifically -- `unknown_tool`,
/// `unsupported` and `confirm_required` read a little differently from the
/// wording this used to print, though they say the same thing.
///
/// Builds a runtime of its own, the same way `serve` does: this is
/// otherwise a synchronous binary, `run_tool` is the one other place that
/// needs an async call answered, and a single tool call has no concurrent
/// I/O to justify `serve`'s multi-threaded one.
pub(crate) fn run_tool(
    vault: Vault,
    name: Option<String>,
    arguments: &str,
    list: bool,
    confirm_destructive: bool,
) -> Result<()> {
    use everyday_core::agent::tools;

    if list {
        for tool in tools::available(&vault) {
            println!("{:<28} {}", tool.name, crate::format::first_sentence(tool.description));
        }
        return Ok(());
    }

    let name = name.expect("clap requires a name unless --list");
    let arguments: serde_json::Value = serde_json::from_str(arguments)
        .map_err(|e| Error::Invalid(format!("the arguments are not JSON: {e}")))?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Invalid(format!("could not start a runtime: {e}")))?;

    runtime.block_on(async move {
        let service = std::sync::Arc::new(everyday_service::Service::new());
        service.set(vault);

        let args = serde_json::json!({
            "name": name,
            "arguments": arguments,
            "confirmDestructive": confirm_destructive,
        });
        let value = service
            .call(everyday_service::Ctx::local(), "run_tool", args)
            .await
            .map_err(|e| refusal(&name, e))?;

        println!("{}", serde_json::to_string_pretty(&value)?);
        Ok(())
    })
}

/// A refusal from the tool table, said the way this program says things.
///
/// The three checks a tool run can fail -- no such tool, not on this vault,
/// destructive and unconfirmed -- live in the service now, so that the
/// palette, MCP and this binary cannot drift about what is allowed. Their
/// wording, though, is written for whoever is calling: the service tells an
/// assistant to "call it again with confirmDestructive", which is the name of
/// a JSON field and not something anybody can type here. `clap` would reject
/// it. So the codes come back across the boundary and the sentence is written
/// again on this side, in flags, with the `--list` hint that a person at a
/// terminal can act on and a model cannot.
fn refusal(name: &str, e: everyday_service::error::CommandError) -> Error {
    use everyday_service::error::codes;
    match e.code.as_str() {
        codes::UNKNOWN_TOOL => {
            Error::Invalid(format!("there is no tool called {name:?}; try --list"))
        }
        codes::UNSUPPORTED => Error::Invalid(format!("{name} is not available on this vault")),
        codes::CONFIRM_REQUIRED => Error::Invalid(format!(
            "{name} deletes something and there is no undo; pass --confirm-destructive"
        )),
        // Anything else is the tool itself failing rather than being refused,
        // and this is the one place that knows it is not a complaint about the
        // arguments. Reporting it as `Invalid` put "invalid data:" in front of
        // a disk that was full.
        _ => Error::Backend(Box::new(e)),
    }
}
