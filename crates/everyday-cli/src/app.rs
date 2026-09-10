//! Command definitions and their implementations.

use clap::{Parser, Subcommand};
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

#[derive(Parser, Debug)]
#[command(name = "everyday", about = "A private journal", version, disable_help_subcommand = true)]
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

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create a new vault.
    Init {
        /// Storage backend: `sqlite` for a database in the vault folder,
        /// `postgres` for one on a server.
        #[arg(long, default_value = everyday_vault::DEFAULT_BACKEND)]
        backend: String,
        /// Backend setting, as `key=value`. Repeatable.
        ///
        /// What a backend needs depends on the backend: `sqlite` needs
        /// nothing, `postgres` needs
        /// `--set url=postgresql://user:password@host:5432/database` and
        /// optionally `--set schema=everyday`. Settings are sealed under the
        /// vault password, so a URL given here does not end up readable in
        /// the vault header -- but it does end up in this shell's history,
        /// which is worth a thought before pasting a production credential.
        #[arg(long = "set", value_name = "KEY=VALUE")]
        settings: Vec<String>,
        /// Name shown in the app.
        #[arg(long, default_value = "My Journal")]
        name: String,
        /// Create the vault *without* encryption. Everything is stored in
        /// the clear; only do this if the vault sits on encrypted storage.
        #[arg(long)]
        no_encryption: bool,
    },
    /// Serve this vault to other copies of Every Day.
    ///
    /// The headless half of "one vault, many windows": a machine under a desk
    /// or in a container holds the vault, and the app on a laptop or a phone
    /// connects to it. What that machine keeps is the key, the search index,
    /// the calendar feeds and the assistant; a client keeps a device token and
    /// nothing about the data at all.
    ///
    /// It starts *locked* unless a password is given, so an unattended server
    /// does not have to hold one in an environment file: the first client to
    /// connect unlocks it.
    Serve {
        /// Address to answer on. `0.0.0.0` means every network on this
        /// machine, which is what a private network wants.
        #[arg(long, default_value_t = String::from("0.0.0.0"))]
        listen: String,
        /// Port to answer on.
        #[arg(long, default_value_t = everyday_server::DEFAULT_PORT)]
        port: u16,
        /// Print a pairing link and a QR code, then keep serving.
        #[arg(long)]
        pair: bool,
        /// Refuse to unlock over the network; the password must be given here.
        #[arg(long)]
        no_remote_unlock: bool,
        /// Terminate TLS somewhere else -- a reverse proxy holding a real
        /// certificate. Clients then pin nothing and must reach it over https.
        #[arg(long)]
        no_tls: bool,
        /// Open the vault with the key this machine has in its keychain,
        /// rather than waiting for a client to type a password.
        ///
        /// Only works where the desktop app has been told to keep one --
        /// Settings, Vault, "open this vault without a password". A headless
        /// machine with no keychain says so and carries on locked.
        #[arg(long)]
        keychain: bool,
    },
    /// Run one of the assistant's tools, with no model in the loop.
    ///
    /// The same verbs the assistant has, for a script or a keyboard shortcut.
    /// `everyday do list_tools` is not a thing -- use `--list`.
    Do {
        /// Tool name, as `--list` prints it.
        #[arg(required_unless_present = "list")]
        name: Option<String>,
        /// Arguments, as one JSON object.
        #[arg(default_value = "{}")]
        arguments: String,
        /// Print every tool this vault offers and stop.
        #[arg(long)]
        list: bool,
        /// Allow a tool that deletes something. There is no undo.
        #[arg(long)]
        confirm_destructive: bool,
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
    /// Write your data out in formats other programs read.
    ///
    /// A `.zip` path gets an archive; anything else gets the same files in a
    /// folder. Either way it is Markdown, iCalendar and CSV, with no Every
    /// Day format anywhere in it -- see `everyday export --list`.
    ///
    /// This is not a backup. It is your writing with the encryption taken
    /// off, and it leaves out the assistant's key and the devices you have
    /// paired. `everyday backup` is the one that copies the vault sealed.
    Export {
        /// Where to write it. Not needed with `--list`.
        path: Option<PathBuf>,
        /// Which apps, by id. Repeatable. All of them by default.
        #[arg(long = "part", value_name = "ID")]
        parts: Vec<String>,
        /// Leave out photographs, video and cover art.
        #[arg(long)]
        no_media: bool,
        /// Say what could be exported, and how much of it there is, then stop.
        #[arg(long)]
        list: bool,
    },
    /// Read an export back in.
    ///
    /// Takes an archive or a folder -- including a folder of Markdown that
    /// was never an export at all. Says what it found and stops unless
    /// `--yes` is given, because reading one in changes records.
    Import {
        path: PathBuf,
        /// Which apps, by id. Repeatable. Everything readable by default.
        #[arg(long = "part", value_name = "ID")]
        parts: Vec<String>,
        /// Overwrite records this vault already has, rather than leaving them.
        #[arg(long)]
        replace: bool,
        /// Do it. Without this, nothing is written and what would happen is
        /// printed.
        #[arg(long)]
        yes: bool,
    },
    /// Change the vault password.
    Passwd,
    /// Copy the whole vault, sealed as it is, into a directory.
    ///
    /// The copy opens with the same password and needs no restore step.
    Backup { dir: PathBuf },
    /// Show or change what this vault's storage backend is configured with.
    ///
    /// The place to fix a database password that has been rotated, or a
    /// server that has moved, without recreating the vault. Takes effect the
    /// next time the vault is opened.
    Backend {
        /// Backend setting, as `key=value`. Repeatable, and merged with what
        /// is already there. With none, the current configuration is listed.
        #[arg(long = "set", value_name = "KEY=VALUE")]
        settings: Vec<String>,
    },
    /// Check the vault for storage-level damage.
    Check,
    /// Delete attachments no entry references.
    Gc {
        /// Also collect attachments written in the last day.
        ///
        /// Off by default: an attachment is unreferenced from the moment it
        /// is stored until the entry embedding it is saved, so a young
        /// orphan may simply be a draft open in another window.
        #[arg(long)]
        include_recent: bool,
    },
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
    if let Command::Init { backend, settings, name, no_encryption } = &cli.command {
        return init(&path, backend, settings, name, *no_encryption, cli.password.as_deref());
    }

    if !everyday_vault::exists(&path) {
        return Err(Error::NoVault(path));
    }

    // `backend` is dispatched before the open below, and that is the whole
    // reason it works. Opening a vault opens its storage backend, so a vault
    // whose database has moved -- or whose database password has been
    // rotated -- cannot be opened at all, which is precisely the vault whose
    // backend settings need changing. It reads and rewrites the header and
    // touches nothing else. See `Vault::open_dormant`.
    if let Command::Backend { settings } = &cli.command {
        let vault = everyday_vault::open_dormant(&path)?;
        return backend(&vault, settings, cli.password.as_deref());
    }

    // `serve` is the one command that may run against a *locked* vault, and
    // deliberately: an unattended server that had to be given a password would
    // be a server keeping one in an environment file. The first client to
    // connect unlocks it instead.
    if let Command::Serve { listen, port, pair, no_remote_unlock, no_tls, keychain } = &cli.command
    {
        let vault = everyday_vault::open(&path)?;
        if let Some(password) = cli.password.as_deref()
            && !vault.is_unlocked()
        {
            vault.unlock(Some(password))?;
        }
        // A machine under a desk that reboots overnight. Best effort and
        // never fatal: a headless box with no keychain, or a key that no
        // longer fits, leaves the vault locked -- which is where it would
        // have been anyway, and the first client to connect can still open it.
        if *keychain && !vault.is_unlocked() {
            match everyday_vault::autounlock::recall(&path)
                .map(|k| vault.unlock_with_key(k.as_str()))
            {
                Some(Ok(())) => println!("Opened with the key from this machine's keychain."),
                Some(Err(e)) => eprintln!("warning: the key in the keychain did not fit ({e})"),
                None => {
                    eprintln!("warning: this machine has no key for that vault in its keychain")
                }
            }
        }
        return serve(vault, &path, listen, *port, *pair, *no_remote_unlock, *no_tls);
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
        Command::Export { path, parts, no_media, list } => {
            export(&vault, path.as_deref(), parts, !no_media, list)
        }
        Command::Import { path, parts, replace, yes } => import(&vault, &path, parts, replace, yes),
        Command::Passwd => passwd(&vault, cli.password.as_deref()),
        Command::Backup { dir } => backup(&vault, &dir),
        Command::Backend { .. } => unreachable!("handled above"),
        Command::Check => check(&vault),
        Command::Gc { include_recent } => {
            let grace = if include_recent {
                std::time::Duration::ZERO
            } else {
                everyday_core::store::GC_GRACE
            };
            let n = vault.collect_garbage(grace)?;
            println!("reclaimed {n} unreferenced attachment(s)");
            Ok(())
        }
        Command::Serve { .. } => unreachable!("handled above"),
        Command::Do { name, arguments, list, confirm_destructive } => {
            run_tool(vault, name, &arguments, list, confirm_destructive)
        }
        Command::Demo => demo(&vault),
    }
}

// ---- commands -----------------------------------------------------------

/// Parse the repeatable `--set key=value` into backend settings.
///
/// Split on the *first* `=` only: a Postgres URL is full of them, and
/// `--set url=postgresql://u:p@h/db?options=-csearch_path%3Dx` must survive
/// intact.
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

fn init(
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
fn backend(vault: &Vault, pairs: &[String], supplied: Option<&str>) -> Result<()> {
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

fn attach(vault: &Vault, id: &str, file: &std::path::Path) -> Result<()> {
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

fn delete(vault: &Vault, id: &str) -> Result<()> {
    let id = resolve_entry(vault, id)?;
    vault.delete_entry(id)?;
    println!("deleted {id}");
    Ok(())
}

fn backup(vault: &Vault, dir: &std::path::Path) -> Result<()> {
    vault.backup(dir)?;
    println!("backed up to {}", dir.display());
    println!("it opens with the same password: everyday --vault {} list", dir.display());
    Ok(())
}

fn check(vault: &Vault) -> Result<()> {
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
fn export(
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
fn import(
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).chain(['\u{2026}']).collect()
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

// ---- serving, and running a tool --------------------------------------

/// Serve `vault` until the process is stopped.
///
/// Builds a runtime here rather than making `main` async: everything else this
/// binary does is synchronous, and a runtime started for every `everyday list`
/// would be a cost paid by the common case for the sake of the rare one.
fn serve(
    vault: Vault,
    vault_path: &std::path::Path,
    listen: &str,
    port: u16,
    pair: bool,
    no_remote_unlock: bool,
    no_tls: bool,
) -> Result<()> {
    let ip: std::net::IpAddr =
        listen.parse().map_err(|_| Error::Invalid(format!("{listen} is not an address")))?;
    let config = everyday_server::Config {
        listen: std::net::SocketAddr::new(ip, port),
        enabled: true,
        allow_remote_unlock: !no_remote_unlock,
        no_tls,
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Invalid(format!("could not start a runtime: {e}")))?;

    runtime.block_on(async move {
        everyday_server::install_crypto_provider();
        let dir = everyday_vault::config_dir();
        let parts = everyday_server::prepare(&dir, &config).map_err(command_error)?;
        let registry = parts.registry.clone();
        let broadcaster = parts.broadcaster.clone();

        let name = vault.status().name;
        let locked = !vault.is_unlocked();
        let service = std::sync::Arc::new(everyday_service::Service::new());
        service.set(vault);
        service.set_events(broadcaster);

        let running = everyday_server::start(service.clone(), parts, &config, name.clone())
            .await
            .map_err(command_error)?;

        // The assistant's routines, on this runtime rather than one of their
        // own. This is the shape the feature was built for: a machine under a
        // desk, no window anywhere, and a seven o'clock brief that happens
        // anyway. It does nothing until the first client unlocks the vault.
        let (stop_scheduler, listen) = tokio::sync::watch::channel(false);
        let scheduling = tokio::spawn(everyday_service::scheduler::run(service.clone(), listen));

        // The local socket as well, always. It is how `everyday new` writes
        // through a running server instead of coming up read-only beside it,
        // and how a browser-extension host will reach a vault that is not
        // shared on any network at all.
        #[cfg(unix)]
        let _socket = {
            // Named after the vault, not after the working directory. Two
            // `serve` processes on different vaults would otherwise collide on
            // one socket -- and any client looking the path up by the vault it
            // wants would find nothing there.
            let path = everyday_server::socket_path(vault_path);
            match everyday_server::serve_socket(running.server.clone(), path.clone()).await {
                Ok(stop) => {
                    println!("Local socket:  {}", path.display());
                    Some(stop)
                }
                Err(e) => {
                    eprintln!("warning: no local socket ({e})");
                    None
                }
            }
        };

        println!("Serving {name} on {}", running.address);
        if locked {
            println!("The vault is locked. The first client to connect can unlock it.");
        }

        if pair {
            let code = registry.new_pairing_code();
            let host = if running.address.ip().is_unspecified() {
                match everyday_server::tls::interface_addresses().first() {
                    Some(ip) => format!("{ip}:{}", running.address.port()),
                    None => format!("127.0.0.1:{}", running.address.port()),
                }
            } else {
                running.address.to_string()
            };
            let invitation =
                everyday_server::pairing::invitation(&host, running.fingerprint(), &code, &name);
            println!();
            println!("{}", terminal_qr(&invitation.url));
            println!("{}", invitation.url);
            println!();
            println!("Good once, for five minutes.");
        }

        println!("Press Ctrl-C to stop.");
        tokio::signal::ctrl_c().await.ok();
        println!();
        println!("Stopping.");
        // The scheduler first, and genuinely waited on. A routine mid-run is
        // spending money and holding the vault's writer, and locking
        // underneath it would fail its next tool call and strand its row
        // saying `Running`. The loop checks the flag between ticks, so this
        // returns as soon as the current tick does and immediately if none is
        // in flight.
        //
        // Not capped. A run has its own fifteen-minute timeout, which bounds
        // this, and a second Ctrl-C is how somebody says they meant it.
        let _ = stop_scheduler.send(true);
        if !scheduling.is_finished() {
            println!("Waiting for the assistant to finish what it was doing…");
        }
        tokio::select! {
            _ = scheduling => {}
            _ = tokio::signal::ctrl_c() => {
                println!("Stopping anyway. A run in flight will not be written down.");
            }
        }
        running.stop();
        // Give the vault its checkpoint before the process goes.
        if let Some(v) = service.get() {
            let _ = v.with_store(|s| s.flush());
            v.lock();
        }
        Ok(())
    })
}

/// A QR code drawn with half-block characters.
///
/// Two rows of the code per line, because a terminal cell is about twice as
/// tall as it is wide and a code drawn one row per line comes out stretched
/// enough that some scanners refuse it. Light on dark, with a quiet zone.
fn terminal_qr(url: &str) -> String {
    use qrcode::{Color, EcLevel, QrCode};
    let Ok(code) = QrCode::with_error_correction_level(url.as_bytes(), EcLevel::M) else {
        return String::new();
    };
    let width = code.width();
    let quiet = 2;
    let side = width + quiet * 2;
    let dark = |x: usize, y: usize| -> bool {
        if x < quiet || y < quiet || x >= width + quiet || y >= width + quiet {
            return false;
        }
        code[(x - quiet, y - quiet)] == Color::Dark
    };

    let mut out = String::new();
    for row in (0..side).step_by(2) {
        for x in 0..side {
            let top = dark(x, row);
            let bottom = row + 1 < side && dark(x, row + 1);
            // A dark module is drawn light: a terminal is usually dark, and a
            // scanner wants the *quiet zone* to be the lighter of the two.
            out.push(match (top, bottom) {
                (true, true) => ' ',
                (true, false) => '\u{2584}',
                (false, true) => '\u{2580}',
                (false, false) => '\u{2588}',
            });
        }
        out.push('\n');
    }
    out
}

/// Run one of the assistant's tools.
fn run_tool(
    vault: Vault,
    name: Option<String>,
    arguments: &str,
    list: bool,
    confirm_destructive: bool,
) -> Result<()> {
    use everyday_core::agent::tools;

    if list {
        for tool in tools::available(&vault) {
            println!("{:<28} {}", tool.name, first_sentence(tool.description));
        }
        return Ok(());
    }

    let name = name.expect("clap requires a name unless --list");
    let arguments: serde_json::Value = serde_json::from_str(arguments)
        .map_err(|e| Error::Invalid(format!("the arguments are not JSON: {e}")))?;

    let Some(tool) = tools::find(&name) else {
        return Err(Error::Invalid(format!("there is no tool called {name:?}; try --list")));
    };
    if !tools::available(&vault).iter().any(|t| t.name == name) {
        return Err(Error::Invalid(format!("{name} is not available on this vault")));
    }
    if matches!(tool.effect, tools::Effect::Destructive) && !confirm_destructive {
        return Err(Error::Invalid(format!(
            "{name} deletes something and there is no undo; pass --confirm-destructive"
        )));
    }

    let tz = everyday_core::model::system_tz();
    let ctx = tools::ToolContext {
        vault: &vault,
        today: everyday_core::model::today_local(),
        tz: &tz,
        conversation: None,
        // Somebody typed `everyday do`. Not unattended in the sense that
        // matters: a person is reading the output.
        unattended: false,
    };
    let value = tools::dispatch(&ctx, &name, &arguments)?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn first_sentence(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match text.find(". ") {
        Some(at) => text[..=at].to_string(),
        None => text,
    }
}

/// A service error, in the shape the rest of this binary reports.
fn command_error(e: everyday_service::error::CommandError) -> Error {
    Error::Invalid(format!("{}: {}", e.code, e.message))
}

#[cfg(test)]
mod tests {
    use super::*;

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
