//! Command definitions and their implementations.

use everyday_core::crypto::KdfParams;
use everyday_core::model::{Attachment, MediaKind};
use everyday_core::store::SortOrder;
use everyday_core::{
    Entry, EntryQuery, Error, Journal, JournalId, Result, RichDoc, Vault, VaultConfig,
};
use jiff::civil::Date;
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "everyday",
    about = "A private journal",
    version,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Vault directory. Defaults to the platform data directory.
    #[arg(long, short = 'C', global = true, env = "EVERYDAY_VAULT")]
    vault: Option<PathBuf>,

    /// Vault password. Prefer the prompt or `EVERYDAY_PASSWORD`; a password
    /// passed as an argument is visible to every process on the machine.
    #[arg(long, global = true, env = "EVERYDAY_PASSWORD", hide_env_values = true)]
    password: Option<String>,

    #[command(subcommand)]
    command: Command,
}

use clap::{Parser, Subcommand};

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create a new vault.
    Init {
        /// Storage backend.
        #[arg(long, default_value = everyday_vault::DEFAULT_BACKEND)]
        backend: String,
        /// Name shown in the app.
        #[arg(long, default_value = "My Journal")]
        name: String,
        /// Create the vault *without* encryption. Everything is stored in
        /// the clear; only do this if the vault sits on encrypted storage.
        #[arg(long)]
        no_encryption: bool,
    },
    /// Show vault status.
    Status,
    /// Work with journals.
    #[command(subcommand)]
    Journal(JournalCmd),
    /// Write a new entry. The body is read from stdin.
    New {
        /// Journal name or id. Defaults to the first journal.
        #[arg(long, short)]
        journal: Option<String>,
        #[arg(long, short)]
        title: Option<String>,
        /// Repeatable.
        #[arg(long = "tag", short = 'g')]
        tags: Vec<String>,
        /// Date to file the entry under, as `YYYY-MM-DD`. Defaults to today.
        #[arg(long)]
        date: Option<String>,
        #[arg(long)]
        star: bool,
    },
    /// List entries.
    List {
        #[arg(long, short)]
        journal: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[arg(long)]
        starred: bool,
        /// Emit JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Print one entry as Markdown.
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Full-text search.
    Search {
        query: Vec<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Attach a file to an entry.
    Attach { id: String, file: PathBuf },
    /// Delete an entry.
    Delete { id: String },
    /// Export every entry as Markdown into a directory.
    Export { dir: PathBuf },
    /// Change the vault password.
    Passwd,
    /// Delete attachments no entry references.
    Gc,
    /// Fill the vault with sample content, for trying the app out.
    Demo,
}

#[derive(Subcommand, Debug)]
pub enum JournalCmd {
    List,
    New {
        name: String,
        #[arg(long)]
        color: Option<String>,
        #[arg(long)]
        icon: Option<String>,
    },
    Delete {
        name: String,
    },
}

pub fn run(cli: Cli) -> Result<()> {
    let path = cli.vault.clone().unwrap_or_else(everyday_vault::default_vault_dir);

    // `init` is the one command that must not require an existing vault.
    if let Command::Init { backend, name, no_encryption } = &cli.command {
        return init(&path, backend, name, *no_encryption, cli.password.as_deref());
    }

    if !everyday_vault::exists(&path) {
        return Err(Error::NoVault(path));
    }
    let vault = everyday_vault::open(&path)?;
    if !vault.is_unlocked() {
        let password = match cli.password.clone() {
            Some(p) => p,
            None => prompt_password("Password: ")?,
        };
        vault.unlock(Some(&password))?;
    }

    match cli.command {
        Command::Init { .. } => unreachable!("handled above"),
        Command::Status => status(&vault),
        Command::Journal(cmd) => journal(&vault, cmd),
        Command::New { journal, title, tags, date, star } => {
            new_entry(&vault, journal, title, tags, date, star)
        }
        Command::List { journal, limit, starred, json } => {
            list(&vault, journal, limit, starred, json)
        }
        Command::Show { id, json } => show(&vault, &id, json),
        Command::Search { query, limit } => search(&vault, &query.join(" "), limit),
        Command::Attach { id, file } => attach(&vault, &id, &file),
        Command::Delete { id } => delete(&vault, &id),
        Command::Export { dir } => export(&vault, &dir),
        Command::Passwd => passwd(&vault, cli.password.as_deref()),
        Command::Gc => {
            let n = vault.collect_garbage()?;
            println!("reclaimed {n} unreferenced attachment(s)");
            Ok(())
        }
        Command::Demo => demo(&vault),
    }
}

// ---- commands -----------------------------------------------------------

fn init(
    path: &std::path::Path,
    backend: &str,
    name: &str,
    no_encryption: bool,
    supplied: Option<&str>,
) -> Result<()> {
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
            password,
            kdf: KdfParams::default(),
            auto_lock_seconds: 15 * 60,
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
        println!("\nWarning: this vault is NOT encrypted. Anything written to it is stored in the clear.");
    }
    Ok(())
}

fn status(vault: &Vault) -> Result<()> {
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

fn journal(vault: &Vault, cmd: JournalCmd) -> Result<()> {
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

fn new_entry(
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

    let mut entry = Entry::new(journal.id, &local_timezone());
    entry.body = RichDoc::from_plain_text(body.trim_end());
    entry.title = title.unwrap_or_default();
    entry.tags = tags;
    entry.starred = star;
    if let Some(d) = date {
        entry.local_date =
            d.parse::<Date>().map_err(|e| Error::Invalid(format!("bad --date {d:?}: {e}")))?;
    }
    vault.save_entry(&entry)?;
    println!("{}", entry.id);
    Ok(())
}

fn list(
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

fn show(vault: &Vault, id: &str, json: bool) -> Result<()> {
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

fn search(vault: &Vault, query: &str, limit: usize) -> Result<()> {
    let hits = vault.search(query, None, limit)?;
    if hits.is_empty() {
        eprintln!("no matches for {query:?}");
        return Ok(());
    }
    for h in hits {
        println!("{}  {}  [{}]", h.local_date, truncate(&h.title, 48), h.id.short());
        if !h.snippet.is_empty() {
            println!("    {}", h.snippet.replace('\n', " "));
        }
    }
    Ok(())
}

fn attach(vault: &Vault, id: &str, file: &std::path::Path) -> Result<()> {
    let bytes = std::fs::read(file).map_err(|e| Error::io(file, e))?;
    let mut entry = vault.entry(resolve_entry(vault, id)?)?;

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
    vault.save_entry(&entry)?;
    println!("attached {} ({})", file.display(), human_bytes(bytes.len() as u64));
    Ok(())
}

fn delete(vault: &Vault, id: &str) -> Result<()> {
    let id = resolve_entry(vault, id)?;
    vault.delete_entry(id)?;
    println!("deleted {id}");
    Ok(())
}

fn export(vault: &Vault, dir: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    let journals: std::collections::HashMap<JournalId, String> =
        vault.journals()?.into_iter().map(|j| (j.id, j.name)).collect();

    let mut count = 0;
    for entry in vault.all_entries()? {
        let journal = journals.get(&entry.journal_id).cloned().unwrap_or_else(|| "Unfiled".into());
        let sub = dir.join(sanitize(&journal));
        std::fs::create_dir_all(&sub).map_err(|e| Error::io(&sub, e))?;

        let name = format!(
            "{}-{}-{}.md",
            entry.local_date,
            sanitize(&entry.display_title()),
            entry.id.short()
        );
        let mut text = format!("# {}\n\n_{}_\n\n", entry.display_title(), entry.local_date);
        if !entry.tags.is_empty() {
            text.push_str(&format!("Tags: {}\n\n", entry.tags.join(", ")));
        }
        text.push_str(&entry.body.to_markdown());
        text.push('\n');
        std::fs::write(sub.join(&name), text).map_err(|e| Error::io(sub.join(&name), e))?;

        // Attachments are written alongside so the export is self-contained.
        for a in &entry.attachments {
            let media = sub.join("media");
            std::fs::create_dir_all(&media).map_err(|e| Error::io(&media, e))?;
            let out = media.join(format!("{}-{}", &a.blob.to_hex()[..8], sanitize(&a.filename)));
            if let Ok(bytes) = vault.blob(a.blob) {
                std::fs::write(&out, bytes).map_err(|e| Error::io(&out, e))?;
            }
        }
        count += 1;
    }
    println!("exported {count} entries to {}", dir.display());
    Ok(())
}

fn passwd(vault: &Vault, supplied: Option<&str>) -> Result<()> {
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

fn demo(vault: &Vault) -> Result<()> {
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
        let mut entry = Entry::new(journal.id, &local_timezone());
        entry.title = (*title).into();
        entry.body = RichDoc::from_plain_text(body);
        entry.tags = tags.iter().map(|t| t.to_string()).collect();
        entry.local_date = today_local().saturating_sub(jiff::Span::new().days(created));
        vault.save_entry(&entry)?;
        created += 1;
    }
    println!("added {created} sample entries");
    Ok(())
}

// ---- helpers ------------------------------------------------------------

fn prompt_password(prompt: &str) -> Result<String> {
    // Never echo, and never fall back to a visible read: a silent downgrade
    // would put the password in the user's scrollback.
    if !std::io::stdin().is_terminal() {
        return Err(Error::Invalid(
            "no terminal to read a password from; set EVERYDAY_PASSWORD".into(),
        ));
    }
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    rpassword::read_password()
        .map_err(|e| Error::Invalid(format!("could not read password: {e}")))
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

/// The system time zone, falling back to UTC. Entries record this so their
/// local date survives the author moving countries.
fn local_timezone() -> String {
    jiff::tz::TimeZone::system().iana_name().unwrap_or("UTC").to_string()
}

fn today_local() -> Date {
    everyday_core::model::local_date_in(jiff::Timestamp::now(), &local_timezone())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).chain(['\u{2026}']).collect()
}

/// Make a string safe to use as a single path component.
fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    let out: String = trimmed.chars().take(60).collect();
    if out.is_empty() { "untitled".into() } else { out }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = n as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{n} B") } else { format!("{size:.1} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_produces_a_safe_single_path_component() {
        assert_eq!(sanitize("A Long Walk"), "A-Long-Walk");
        assert_eq!(sanitize("../../etc/passwd"), "etc-passwd");
        assert_eq!(sanitize(""), "untitled");
        assert_eq!(sanitize("///"), "untitled");
        assert!(!sanitize("a/b\\c").contains(['/', '\\']));
        assert!(sanitize(&"x".repeat(200)).chars().count() <= 60);
    }

    #[test]
    fn human_bytes_reads_naturally() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn truncate_respects_character_boundaries() {
        assert_eq!(truncate("short", 10), "short");
        let out = truncate(&"\u{1f600}".repeat(50), 5);
        assert_eq!(out.chars().count(), 5);
    }

    #[test]
    fn the_cli_surface_is_well_formed() {
        // Catches conflicting short flags and bad defaults at test time
        // rather than on the user's first run.
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
