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
use std::io::{BufRead, IsTerminal, Read, Write};
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
        /// Also serve the MCP endpoint, so an agent on this machine can use
        /// this vault.
        ///
        /// Opt-in here rather than read from `mcp.json`'s switch, and the
        /// distinction is deliberate. That switch is thrown by somebody at a
        /// keyboard, for a listener that answers only on loopback; this is a
        /// daemon an operator starts, often on a machine other people can
        /// reach. The two are different decisions, and a configuration
        /// directory that travels -- synced dotfiles, a shared home, an image
        /// baked from somebody's laptop -- must not make the first one into
        /// the second. The port and the token still come from `mcp.json`.
        #[arg(long)]
        mcp: bool,
        /// Answer MCP on this address instead of the one `mcp.json` names.
        ///
        /// Takes `ADDR` or `ADDR:PORT`. There is no TLS on this endpoint --
        /// an MCP client has no way to pin a certificate -- so anything but a
        /// loopback address is plaintext on a network, and is never chosen
        /// for you.
        #[arg(long, value_name = "ADDR")]
        mcp_listen: Option<String>,
        /// Open the vault with the key this machine has in its keychain,
        /// rather than waiting for a client to type a password.
        ///
        /// Only works where the desktop app has been told to keep one --
        /// Settings, Vault, "open this vault without a password". A headless
        /// machine with no keychain says so and carries on locked.
        #[arg(long)]
        keychain: bool,
    },
    /// Let another program's model use this vault, over stdio.
    ///
    /// A pipe, not a second server: every line read from stdin is posted to
    /// the MCP listener a running Every Day already serves on a loopback
    /// port, and every reply comes back as a line on stdout. This command
    /// opens no vault of its own -- the second process to open one gets it
    /// read-only, which would mean every tool that writes failing forever,
    /// silently, from inside whatever agent called it. It reads the port and
    /// the token out of `mcp.json` instead, the same file the settings panel
    /// writes, and forwards to whichever process already holds the vault
    /// open.
    ///
    /// The listener this talks to is a separate switch, off by default --
    /// Settings, Vault, "Let an AI agent use this vault". With it off there
    /// is nothing on the other end of this pipe, which is reported as a
    /// JSON-RPC error on stdout rather than by this process dying with no
    /// explanation the calling agent can show anybody.
    Mcp {
        /// Talk to a listener on this port instead of the one `mcp.json`
        /// names. Useful when this user's configuration directory is not
        /// the default one, or when pointing this at a listener started by
        /// hand for testing.
        #[arg(long)]
        port: Option<u16>,
        /// Authenticate with this token instead of the one `mcp.json`
        /// names, for the same reasons as `--port`.
        #[arg(long)]
        token: Option<String>,
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

    // `mcp` is the other command dispatched before a vault is even looked
    // for, and for a stronger reason than `init`'s: it must never open one.
    // See its own doc, and `docs/plans/mcp.md`'s "Streamable HTTP is the
    // transport; stdio is a pipe to it" -- the second process to open a
    // vault gets it read-only, which is the one failure mode this command
    // exists to make impossible rather than to handle.
    if let Command::Mcp { port, token } = &cli.command {
        return mcp(*port, token.clone());
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
    if let Command::Serve {
        listen,
        port,
        pair,
        no_remote_unlock,
        no_tls,
        mcp,
        mcp_listen,
        keychain,
    } = &cli.command
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
        return serve(
            vault,
            &path,
            ServeOptions {
                listen,
                port: *port,
                pair: *pair,
                no_remote_unlock: *no_remote_unlock,
                no_tls: *no_tls,
                mcp: *mcp,
                mcp_listen: mcp_listen.as_deref(),
            },
        );
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
        Command::Mcp { .. } => unreachable!("handled above"),
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
/// Everything `serve` was asked for, as one argument.
///
/// A struct rather than nine parameters, which is where this arrived once MCP
/// gained a switch and an address of its own. They travel together, they all
/// come from one `Command::Serve`, and a call site listing nine bare values in
/// a row is one transposition away from serving the wrong thing on the wrong
/// port.
pub struct ServeOptions<'a> {
    /// Address for the vault server itself.
    pub listen: &'a str,
    pub port: u16,
    /// Print a pairing link and a QR code before serving.
    pub pair: bool,
    pub no_remote_unlock: bool,
    pub no_tls: bool,
    /// Serve the MCP endpoint too. See the flag's own documentation for why
    /// this is not read from `mcp.json`.
    pub mcp: bool,
    pub mcp_listen: Option<&'a str>,
}

fn serve(vault: Vault, vault_path: &std::path::Path, options: ServeOptions<'_>) -> Result<()> {
    let ServeOptions { listen, port, pair, no_remote_unlock, no_tls, mcp, mcp_listen } = options;
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
        service.set_events(broadcaster.clone());

        let running = everyday_server::start(service.clone(), parts, &config, name.clone())
            .await
            .map_err(command_error)?;

        // MCP, on the same runtime and -- this is the part that matters --
        // the same `Registry`. Two of those over one `devices.json` write
        // the whole list each and silently erase each other's rows; see
        // `Registry`'s own doc.
        let mcp_running = if mcp {
            Some(serve_mcp(&dir, mcp_listen, service.clone(), registry.clone()).await?)
        } else {
            let config = everyday_server::mcp::Config::load(&dir);
            if config.enabled {
                // The switch in the settings panel is on, and this process is
                // deliberately not reading it. Saying so is the whole point:
                // a field that is quietly ignored by one of its two readers
                // is worse than one that is not there.
                eprintln!(
                    "note: mcp.json has the MCP server switched on. This command does not \
                     act on that switch;\n      pass --mcp to serve it here."
                );
            }
            None
        };
        if let Some(running) = &mcp_running {
            // Both sinks, so `lock_state` reaches every paired device *and*
            // every open MCP stream -- which is what tells a client that
            // connected to a locked vault to ask for the catalogue again.
            service.set_events(everyday_server::fanout(running.sink.clone(), broadcaster.clone()));
            println!("MCP endpoint: http://{}/mcp", running.address);
        }

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

// ---- the stdio pipe to a running vault's MCP listener ------------------

/// Pipe JSON-RPC between this process's stdin/stdout and the MCP listener
/// a running copy of Every Day already serves.
///
/// Start the MCP endpoint beside a headless server.
///
/// The address is resolved here rather than in the caller because there are
/// three sources and a precedence between them: `--mcp-listen` if it was
/// given, else whatever `mcp.json` holds, else the loopback default. An
/// address is accepted with or without a port, since somebody who has gone
/// to the trouble of naming one usually means both.
///
/// A machine with no token yet is given one, printed once. That is not a
/// convenience: a headless box has no settings panel to issue one from, so
/// without this `--mcp` would start a listener that answers `401` to
/// everything and offers no way out of it.
async fn serve_mcp(
    dir: &std::path::Path,
    listen: Option<&str>,
    service: std::sync::Arc<everyday_service::Service>,
    registry: std::sync::Arc<everyday_server::Registry>,
) -> Result<everyday_server::mcp::Running> {
    let mut config = everyday_server::mcp::Config::load(dir);

    if let Some(raw) = listen {
        config.listen = match raw.parse::<std::net::SocketAddr>() {
            Ok(address) => address,
            Err(_) => {
                let ip: std::net::IpAddr =
                    raw.parse().map_err(|_| Error::Invalid(format!("{raw} is not an address")))?;
                std::net::SocketAddr::new(ip, config.listen.port())
            }
        };
    }

    if config.token.is_none() {
        let token = everyday_server::mcp::issue_token(
            &registry,
            dir,
            vec![everyday_service::ctx::Scope::All],
        )
        .map_err(command_error)?;
        println!("MCP token:    {token}");
        println!("              Kept in {}; this is the only time it is printed.", dir.display());
        // Taken into the config we are about to serve with, rather than by
        // re-reading the file `issue_token` just wrote. That file holds the
        // *persisted* address, and re-reading it here threw away whatever
        // `--mcp-listen` asked for -- so the endpoint came up on the stored
        // port and announced it, which is a confusing way to be ignored.
        config.token = Some(token);
    }

    if !config.listen.ip().is_loopback() {
        // Said once, plainly, at the moment it becomes true. There is no TLS
        // on this endpoint because an MCP client cannot pin a certificate, so
        // an address other than loopback is somebody's journal in the clear
        // on a network.
        eprintln!(
            "warning: MCP is answering on {}, which is not loopback, and that endpoint \
             has no TLS.\n         Put it on a WireGuard or Tailscale address rather than \
             a shared network.",
            config.listen
        );
    }

    everyday_server::mcp::start(service, registry, &config).await.map_err(command_error)
}

/// This is the whole of `everyday mcp`: it opens no vault, and it does not
/// link against `everyday-mcp` for anything that decides what a JSON-RPC
/// message *means* -- only for `expected_headers`, which reads what the
/// body already implies so this pipe can mirror it into the headers the
/// modern binding requires, without a second implementation of that rule
/// to drift out of step with the first. Every line read here is forwarded
/// byte-for-byte as an HTTP body; the "translation" beyond that -- what a
/// method or a tool call means -- is entirely HTTP framing, done once, in
/// `everyday-server::mcp`, for every client rather than reimplemented per
/// transport. See `docs/plans/mcp.md`'s "Streamable HTTP is the
/// transport; stdio is a pipe to it" for why that is the design and not a
/// shortcut.
///
/// # Nothing but protocol goes to stdout, ever
///
/// A client speaking stdio reads every line on this process's stdout as a
/// JSON-RPC message. Every other subcommand in this file prints freely --
/// a status line, a table, a friendly warning -- and every one of those
/// habits is wrong here: a single stray `println!` corrupts the session in
/// a way the client cannot recover from, because there is no way to tell
/// "that line was a message" from "that line was a banner". So every
/// human-readable word this function has to say, success or failure, goes
/// to `stderr`, and the only things this function ever writes to `stdout`
/// are a reply -- usually read verbatim from the HTTP response body, or
/// synthesised in its place when that body has nothing in it a client
/// could read (see [`mcp_status_error`]) -- and, for a notification,
/// nothing at all. Do not add a startup banner, a progress message, or a
/// debug `dbg!` that writes to stdout -- however harmless it looks, it
/// breaks every message that follows it.
fn mcp(port: Option<u16>, token: Option<String>) -> Result<()> {
    let config = everyday_server::mcp::Config::load(&everyday_vault::config_dir());
    let (url, token) = mcp_endpoint(&config, port, token);
    eprintln!("Forwarding stdio to {url}. Press Ctrl-D to stop.");

    let client = reqwest::blocking::Client::new();
    let stdin = std::io::stdin();
    // `Stdout`, not `stdout.lock()`: `mcp_pipe` shares its writer with an
    // SSE thread (see its doc), and `StdoutLock` is not `Send` -- there is
    // no locking to give up by passing the unlocked handle instead, since
    // `mcp_pipe` puts it behind a `Mutex` of its own and every write to it
    // already goes through `Stdout`'s internal lock besides.
    let stdout = std::io::stdout();
    mcp_pipe(&client, &url, &token, stdin.lock(), stdout)
}

/// Work out which listener to talk to and which token to present, from
/// `mcp.json` and the two overriding flags.
///
/// A free function rather than inlined into [`mcp`], so config/flag
/// precedence -- flags win, `mcp.json` is the fallback, an unissued token
/// becomes an empty one rather than a panic -- is a fact this file can
/// test without opening a socket.
fn mcp_endpoint(
    config: &everyday_server::mcp::Config,
    port: Option<u16>,
    token: Option<String>,
) -> (String, String) {
    let mut listen = config.listen;
    if let Some(port) = port {
        listen.set_port(port);
    }
    // No token issued yet is not this function's problem to solve --
    // `mcp_pipe` sends whatever it is given, the listener answers `401`
    // exactly as it would to anybody else's bad credential, and that
    // answer flows back to the client as an ordinary reply rather than
    // this command inventing a second way to say "not configured".
    let token = token.or_else(|| config.token.clone()).unwrap_or_default();
    (format!("http://{listen}/mcp"), token)
}

/// The JSON-RPC error code this pipe answers with when the endpoint could
/// not be reached at all.
///
/// Not one of `everyday-mcp`'s own codes -- this process does not link
/// against that crate, and those codes are private to it besides (see
/// `everyday-mcp/src/errors.rs`).
///
/// Deliberately outside JSON-RPC's reserved range rather than inside it,
/// which is the opposite of the obvious choice and is what the current
/// specification asks for. MCP partitions the implementation-defined block:
/// `-32000` to `-32019` is *legacy*, and "new implementations SHOULD NOT use
/// codes from this sub-range at all"; `-32020` to `-32099` is reserved to
/// the specification itself, and emitting an undefined code from it is
/// forbidden outright. What is left for a code like this one is the space
/// outside `-32768` to `-32000`, which is where the specification says to
/// put it. See `docs/plans/mcp-protocol-notes.md`.
///
/// It is also not a failure of the protocol but of the pipe underneath it:
/// nothing answered at the other end. No MCP code describes that, because
/// on every other transport it is not a thing that can happen.
const ENDPOINT_UNREACHABLE: i64 = -31000;

/// The JSON-RPC error code this pipe answers with when the listener *did*
/// answer, but with a status whose body is empty by design -- `401`,
/// `403`, `405`, or anything else that carries nothing to just forward.
///
/// Kept apart from [`ENDPOINT_UNREACHABLE`]: that code means no HTTP
/// answer arrived at all; this one means an answer arrived and said no.
/// Same reasoning places it in the same implementation-defined space
/// outside `-32768`..`-32000` rather than MCP's own reserved block -- see
/// `ENDPOINT_UNREACHABLE`'s doc.
const REQUEST_REFUSED: i64 = -31001;

/// The shape every JSON-RPC error this pipe invents shares, so
/// [`mcp_transport_error`] and [`mcp_status_error`] spell "code plus
/// message, echoing the request's id" the same way once rather than each
/// building the envelope by hand and drifting apart on some field name.
fn json_rpc_error(id: &serde_json::Value, code: i64, message: String) -> String {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    });
    body.to_string()
}

/// Build the JSON-RPC error this pipe answers with when a request could
/// not be completed -- the listener not running being the ordinary case,
/// since the switch it depends on is off by default. See [`mcp`]'s doc:
/// a client whose server exits silently reports nothing useful to the
/// person behind it, so this is written to `stdout` as a reply rather
/// than to `stderr` as a warning nobody watching the client will see.
///
/// `id` echoes the request's own, or `Null` when the line that was sent
/// could not even be parsed enough to find one -- the same convention
/// `everyday-mcp` uses for a request it cannot correlate.
fn mcp_transport_error(id: &serde_json::Value, detail: &str) -> String {
    json_rpc_error(
        id,
        ENDPOINT_UNREACHABLE,
        format!(
            "could not reach this vault's MCP listener ({detail}). Turn it \
             on in Settings, under Vault, \"Let an AI agent use this \
             vault\"."
        ),
    )
}

/// Build the JSON-RPC error this pipe answers with when the listener
/// replied with one of the statuses whose body is empty on purpose, so a
/// client waiting on `id` gets a readable reply instead of a blank line --
/// see [`mcp_pipe`]'s doc for why a blank line is not a safe substitute for
/// a reply. Each of `401`, `403` and `405` gets the plain-words reading a
/// person can act on; anything else empty gets a generic one, on the same
/// principle.
fn mcp_status_error(id: &serde_json::Value, status: reqwest::StatusCode) -> String {
    let meaning = match status.as_u16() {
        401 => "the token in `mcp.json` was refused; re-issue it from Settings".to_string(),
        403 => "the request's Origin header was refused".to_string(),
        405 => "this endpoint does not accept that HTTP method".to_string(),
        other => format!("the listener answered with no body (HTTP {other})"),
    };
    json_rpc_error(id, REQUEST_REFUSED, format!("HTTP {}: {meaning}", status.as_u16()))
}

/// Write one reply line to the shared writer, holding the lock across both
/// the write and the flush.
///
/// `output` is shared -- the main loop and, while a `subscriptions/listen`
/// stream is open, a thread of its own both write to it -- so locking only
/// around `writeln!` and flushing separately would let the two interleave
/// a half-written line between them exactly as a stray `println!` would.
/// A poisoned lock (the other side panicked mid-write) is recovered rather
/// than propagated: losing one writer's panic is better than every
/// subsequent reply silently stopping too.
fn mcp_pipe_write<W: Write>(output: &std::sync::Mutex<W>, line: &str) -> std::io::Result<()> {
    let mut output = output.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    writeln!(output, "{line}")?;
    output.flush()
}

/// Drain a `subscriptions/listen` response on a thread of its own, writing
/// each SSE `data:` payload to `output` as it arrives.
///
/// The response is `text/event-stream`: a stream that answers
/// `notifications/tools/list_changed` on vault unlock and otherwise stays
/// open indefinitely. Reading it the way an ordinary reply is read --
/// `.text()`, which drains to EOF -- would block on a stream that never
/// reaches EOF, and with it the whole pipe: no ack, no later request
/// answered, nothing but silence until the client times out and reports
/// the misleading "could not reach this vault's MCP listener". Reading it
/// here, off [`mcp_pipe`]'s main loop, is what lets that loop carry on
/// reading stdin while this stream stays open.
fn mcp_pipe_stream_sse<W: Write>(
    response: reqwest::blocking::Response,
    output: &std::sync::Mutex<W>,
) {
    let mut reader = std::io::BufReader::new(response);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let text = line.trim_end_matches(['\r', '\n']);
        // Keep-alive comments (`: ...`) and the blank lines separating SSE
        // events carry no payload of ours; only a `data:` line does.
        if text.is_empty() || text.starts_with(':') {
            continue;
        }
        let Some(payload) = text.strip_prefix("data:") else { continue };
        if mcp_pipe_write(output, payload.trim_start()).is_err() {
            return;
        }
    }
}

/// Read JSON-RPC lines from `input`, post each to `url`, and write the
/// reply to `output` -- the pipe itself, factored out from [`mcp`] so a
/// test can drive it against an in-process listener instead of a real
/// stdin and a real process's stdout.
///
/// One failed request is not a reason to stop answering the next one: an
/// agent that gets a transport error back from one call is expected to
/// try again, or to tell the person what happened, and either needs this
/// loop still running. Only the end of `input` -- stdin closing -- ends
/// it.
///
/// `output` is taken by value rather than `&mut`, because it is about to
/// be shared: a `subscriptions/listen` reply hands its stream to a thread
/// of its own (see [`mcp_pipe_stream_sse`]), and that thread writes to the
/// same destination as this loop. `std::thread::scope` is what lets those
/// threads borrow `client`, `url` and `token` without demanding `'static`,
/// and guarantees every one of them has finished -- and so has stopped
/// touching `output` -- before this function can return.
fn mcp_pipe<W: Write + Send>(
    client: &reqwest::blocking::Client,
    url: &str,
    token: &str,
    input: impl BufRead,
    output: W,
) -> Result<()> {
    let output = std::sync::Mutex::new(output);

    std::thread::scope(|scope| {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            // Parsed once and reused for both the `id` a reply echoes and
            // the headers the modern binding requires mirrored from the
            // body -- see `everyday_mcp::expected_headers`'s own doc for
            // why deriving those twice, in two crates, is the mistake this
            // avoids.
            let value = serde_json::from_str::<serde_json::Value>(&line).ok();
            let id = value
                .as_ref()
                .and_then(|v| v.get("id").cloned())
                .unwrap_or(serde_json::Value::Null);
            let expected = value.as_ref().map(everyday_mcp::expected_headers).unwrap_or_default();

            let mut request = client
                .post(url)
                .bearer_auth(token)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header(reqwest::header::ACCEPT, "application/json, text/event-stream");
            if let Some(protocol_version) = &expected.protocol_version {
                request = request.header("MCP-Protocol-Version", protocol_version);
            }
            if let Some(method) = &expected.method {
                request = request.header("Mcp-Method", method);
            }
            if let Some(name) = &expected.name {
                request = request.header("Mcp-Name", name);
            }

            let response = match request.body(line).send() {
                Ok(response) => response,
                Err(e) => {
                    mcp_pipe_write(&output, &mcp_transport_error(&id, &e.to_string()))?;
                    continue;
                }
            };

            // A `subscriptions/listen` reply is the one response this pipe
            // must not read to completion on this loop -- see
            // `mcp_pipe_stream_sse`'s doc. Everything else is an ordinary,
            // bounded body.
            let is_sse = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("text/event-stream"));
            if is_sse {
                let output = &output;
                scope.spawn(move || mcp_pipe_stream_sse(response, output));
                continue;
            }

            let status = response.status();
            let body = match response.text() {
                Ok(body) => body,
                Err(e) => {
                    mcp_pipe_write(&output, &mcp_transport_error(&id, &e.to_string()))?;
                    continue;
                }
            };

            if status == reqwest::StatusCode::ACCEPTED {
                // The correct answer to a JSON-RPC *notification*, and a
                // notification must never be replied to -- not even with
                // a blank line. Writing nothing here is the fix, not an
                // oversight.
                continue;
            }
            let reply = if body.trim().is_empty() {
                // `401`, `403`, `405` and any other empty-bodied answer:
                // without this, the line written below would be blank,
                // and a client waiting on `id` would get no reply at all.
                mcp_status_error(&id, status)
            } else {
                body
            };
            mcp_pipe_write(&output, &reply)?;
        }
        Ok(())
    })
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

    /// `--mcp-listen` must reach the listener, including on the path that
    /// mints a token first.
    ///
    /// This is a regression test for a bug that only a real run found: the
    /// token-minting branch re-read `mcp.json` to pick the token back up, and
    /// in doing so threw away the address the flag had asked for. The server
    /// then came up on the stored port and announced it, which looks exactly
    /// like the flag not existing.
    #[test]
    fn an_address_given_on_the_command_line_survives_minting_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = everyday_server::mcp::Config::load(dir.path());
        assert!(config.token.is_none(), "a fresh directory has no token");
        let stored = config.listen;

        // What `serve_mcp` does with `--mcp-listen`, in the same order.
        config.listen = "127.0.0.1:7654".parse().unwrap();
        let registry =
            everyday_server::Registry::open(dir.path().join(everyday_server::DEVICES_FILE))
                .unwrap();
        let token = everyday_server::mcp::issue_token(
            &registry,
            dir.path(),
            vec![everyday_service::ctx::Scope::All],
        )
        .unwrap();
        config.token = Some(token);

        assert_eq!(config.listen.port(), 7654, "the flag must outlive the token");
        assert!(config.token.is_some());
        // And the file keeps the persisted address: a flag is for this run,
        // not a way to rewrite somebody's configuration behind their back.
        assert_eq!(everyday_server::mcp::Config::load(dir.path()).listen, stored);
    }

    #[test]
    fn the_cli_surface_is_well_formed() {
        // Catches conflicting short flags and bad defaults at test time
        // rather than on the user's first run.
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn mcp_flags_win_over_mcp_json_and_mcp_json_wins_over_nothing_at_all() {
        let default_config = everyday_server::mcp::Config::default();
        let (url, token) = mcp_endpoint(&default_config, None, None);
        assert_eq!(url, format!("http://{}/mcp", default_config.listen));
        // Never issued: an empty token, not a panic. See `mcp_endpoint`'s
        // doc for why that is left for the listener to refuse rather than
        // handled specially here.
        assert_eq!(token, "");

        let configured = everyday_server::mcp::Config {
            listen: "127.0.0.1:9999".parse().unwrap(),
            token: Some("from-mcp-json".to_string()),
            ..everyday_server::mcp::Config::default()
        };
        let (url, token) = mcp_endpoint(&configured, None, None);
        assert_eq!(url, "http://127.0.0.1:9999/mcp");
        assert_eq!(token, "from-mcp-json");

        // Flags override both the port and the token `mcp.json` names.
        let (url, token) = mcp_endpoint(&configured, Some(8000), Some("from-a-flag".to_string()));
        assert_eq!(url, "http://127.0.0.1:8000/mcp");
        assert_eq!(token, "from-a-flag");
    }

    #[test]
    fn a_transport_failure_answers_as_a_wellformed_json_dash_rpc_error_carrying_the_request_id() {
        let client = reqwest::blocking::Client::new();
        // Port 0 is never a real listener to connect to: binding picks a
        // fresh port, but nothing has bound *this* address, so the
        // connection itself fails before any HTTP exchange happens -- the
        // "endpoint is not answering" case this function exists for.
        let url = "http://127.0.0.1:0/mcp";
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":42,\"method\":\"tools/list\"}\n";
        let mut output = Vec::new();

        mcp_pipe(&client, url, "irrelevant", &input[..], &mut output).unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{text}");
        let reply: serde_json::Value = serde_json::from_str(lines[0]).expect(&text);
        assert_eq!(reply["jsonrpc"], "2.0");
        assert_eq!(reply["id"], 42);
        assert_eq!(reply["error"]["code"], ENDPOINT_UNREACHABLE);
        assert!(reply["error"]["message"].as_str().unwrap().contains("Let an AI agent"), "{text}");
    }

    #[test]
    fn a_line_with_no_id_answers_with_a_null_id_not_a_missing_one() {
        let client = reqwest::blocking::Client::new();
        let url = "http://127.0.0.1:0/mcp";
        // Not even valid JSON -- the pipe must still answer something a
        // client can parse, rather than propagating a parse error of its
        // own out of this loop.
        let input = b"not json at all\n";
        let mut output = Vec::new();

        mcp_pipe(&client, url, "irrelevant", &input[..], &mut output).unwrap();

        let text = String::from_utf8(output).unwrap();
        let reply: serde_json::Value = serde_json::from_str(text.trim()).expect(&text);
        assert_eq!(reply["id"], serde_json::Value::Null);
    }

    #[test]
    fn a_transport_failure_does_not_end_the_pipe() {
        let client = reqwest::blocking::Client::new();
        let url = "http://127.0.0.1:0/mcp";
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n\
                       {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n";
        let mut output = Vec::new();

        mcp_pipe(&client, url, "irrelevant", &input[..], &mut output).unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        for (line, want_id) in lines.iter().zip([1, 2]) {
            let reply: serde_json::Value = serde_json::from_str(line).expect(&text);
            assert_eq!(reply["id"], want_id);
        }
    }

    /// Stand up a real `everyday_server::mcp` listener against a fresh
    /// vault, and issue it a token, so a test can drive `mcp_pipe` against
    /// an actual HTTP endpoint rather than fake the shape of one.
    ///
    /// Shared by every test below that needs a real listener, so each one
    /// reads as "send this, expect that" rather than repeating the
    /// eight-line ritual of standing one up.
    ///
    /// The two temporary directories are leaked with `.keep()` rather than
    /// dropped at the end of this function: the listener they back runs on
    /// a thread that outlives this call, and a `TempDir` dropped while
    /// still in use would delete the vault out from under it.
    fn start_test_mcp_listener() -> (reqwest::blocking::Client, String, String) {
        let vault_dir = tempfile::tempdir().unwrap();
        let vault = everyday_vault::create(
            vault_dir.path(),
            VaultConfig {
                name: "Test".into(),
                backend: "sqlite".into(),
                settings: Default::default(),
                password: None,
                kdf: Default::default(),
                auto_lock_seconds: 900,
                forget_key_seconds: 0,
            },
        )
        .unwrap();
        vault.save_journal(&Journal::new("Journal")).unwrap();
        let _ = vault_dir.keep();

        let service = std::sync::Arc::new(everyday_service::Service::new());
        service.set(vault);

        let config_dir = tempfile::tempdir().unwrap();
        let registry = std::sync::Arc::new(
            everyday_server::auth::Registry::open(config_dir.path().join("devices.json")).unwrap(),
        );
        let token = everyday_server::mcp::issue_token(
            &registry,
            config_dir.path(),
            vec![everyday_service::Scope::All],
        )
        .unwrap();
        let _ = config_dir.keep();

        // The listener runs on a runtime of its own, on a thread of its
        // own, deliberately: `mcp_pipe` uses a *blocking* client, which
        // panics if it is ever called from inside a Tokio runtime's own
        // worker thread. Keeping the server's runtime on a separate OS
        // thread is what lets a test call the exact function `mcp` calls,
        // rather than a `.await`-flavoured stand-in for it.
        let (address_tx, address_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async move {
                let config = everyday_server::mcp::Config {
                    listen: "127.0.0.1:0".parse().unwrap(),
                    enabled: true,
                    allow_destructive: false,
                    token: None,
                    device_id: None,
                };
                let running =
                    everyday_server::mcp::start(service, registry, &config).await.unwrap();
                address_tx.send(running.address).unwrap();
                // Every test's assertions run on its own thread; this one
                // just has to keep the listener alive until the process
                // exits at the end of the test binary.
                std::future::pending::<()>().await;
            });
        });
        let address = address_rx.recv().unwrap();

        let client = reqwest::blocking::Client::new();
        let url = format!("http://{address}/mcp");
        (client, url, token)
    }

    /// The test that proves the feature: a real `tools/list` call, sent as
    /// a line on stdin, comes back on stdout as the same catalogue
    /// `everyday-server`'s own tests get over the wire directly -- with no
    /// vault open in this process at all.
    #[test]
    fn a_real_tools_list_call_round_trips_through_the_pipe() {
        let (client, url, token) = start_test_mcp_listener();
        let request = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\",\"params\":{}}\n";
        let mut output = Vec::new();
        mcp_pipe(&client, &url, &token, request.as_bytes(), &mut output).unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{text}");
        let reply: serde_json::Value = serde_json::from_str(lines[0]).expect(&text);
        assert_eq!(reply["id"], 1);
        let tools = reply["result"]["tools"].as_array().expect(&text);
        assert!(!tools.is_empty(), "{text}");
    }

    /// Defect 1: `mcp_pipe` used to send only `Authorization`,
    /// `Content-Type` and `Accept` -- never the `MCP-Protocol-Version`,
    /// `Mcp-Method` and `Mcp-Name` headers the modern era's
    /// `check_header_mirroring` requires, so every modern request through
    /// this pipe came back `-32020` no matter how well-formed its body
    /// was. This sends a `tools/list` call carrying modern `_meta` --
    /// `io.modelcontextprotocol/protocolVersion` and
    /// `io.modelcontextprotocol/clientCapabilities` -- and asserts a
    /// `result` comes back, not that error.
    #[test]
    fn a_modern_era_tools_list_call_round_trips_through_the_pipe() {
        let (client, url, token) = start_test_mcp_listener();
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/list",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": everyday_mcp::MODERN,
                    "io.modelcontextprotocol/clientCapabilities": {},
                },
            },
        });
        let mut output = Vec::new();
        mcp_pipe(&client, &url, &token, format!("{request}\n").as_bytes(), &mut output).unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{text}");
        let reply: serde_json::Value = serde_json::from_str(lines[0]).expect(&text);
        assert_eq!(reply["id"], 7);
        assert!(reply.get("error").is_none(), "{text}");
        let tools = reply["result"]["tools"].as_array().expect(&text);
        assert!(!tools.is_empty(), "{text}");
    }

    /// Defect 3, the notification half: `202 Accepted` is the correct
    /// answer to a JSON-RPC notification (a message with no `id`), and a
    /// notification must never be replied to -- not even with a blank
    /// line, which is what the pipe used to write for every empty body it
    /// saw. Sending one and finding stdout still empty is what proves that
    /// distinction is drawn correctly.
    #[test]
    fn a_notification_produces_no_line_on_stdout() {
        let (client, url, token) = start_test_mcp_listener();
        let notification = "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n";
        let mut output = Vec::new();
        mcp_pipe(&client, &url, &token, notification.as_bytes(), &mut output).unwrap();

        assert!(output.is_empty(), "{}", String::from_utf8_lossy(&output));
    }

    /// Defect 3, the error half: `401` also answers with an empty body,
    /// but unlike a notification's `202` it is not a reply this pipe may
    /// skip -- the request it refuses carries an `id` a caller is waiting
    /// on. A bad token must come back as a readable JSON-RPC error, not
    /// the blank line the pipe used to write for every empty-bodied
    /// answer regardless of which one it was.
    #[test]
    fn an_unauthorised_request_answers_with_a_json_dash_rpc_error_not_a_blank_line() {
        let (client, url, _token) = start_test_mcp_listener();
        let request = "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"tools/list\",\"params\":{}}\n";
        let mut output = Vec::new();
        mcp_pipe(&client, &url, "not-the-issued-token", request.as_bytes(), &mut output).unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1, "{text}");
        let reply: serde_json::Value = serde_json::from_str(lines[0]).expect(&text);
        assert_eq!(reply["id"], 9);
        assert_eq!(reply["error"]["code"], REQUEST_REFUSED, "{text}");
        assert!(reply["error"]["message"].as_str().unwrap().contains("token"), "{text}");
    }

    /// A `Write` shared between `mcp_pipe`'s main loop and the SSE thread
    /// it spawns, so a test can poll what has been written so far without
    /// waiting for `mcp_pipe` itself to return -- which, for as long as a
    /// `subscriptions/listen` stream stays open, it never does. Test-only:
    /// the real pipe shares `Stdout` the same way, through the `Mutex`
    /// `mcp_pipe` builds internally, but has no need to peek at partial
    /// output from outside itself.
    #[derive(Clone, Default)]
    struct SharedSink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for SharedSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Defect 2: an SSE answer used to be read with `.text()`, which
    /// drains to EOF -- fine for an ordinary reply, fatal for
    /// `subscriptions/listen`'s response, which is a stream that never
    /// ends. That blocked the whole pipe: no ack, and no answer to
    /// anything sent afterwards, until the client gave up.
    ///
    /// `mcp_pipe` never returns while that stream is still open (its
    /// internal `thread::scope` waits for the reader thread, and nothing
    /// in this test closes the connection), so this drives it from a
    /// thread of its own and polls the shared output for both expected
    /// lines rather than waiting on the call to return. The timeout is a
    /// hang backstop, not the pass condition -- the test succeeds the
    /// moment both lines appear, however soon that is, and only fails if
    /// they never do.
    #[test]
    fn a_subscriptions_listen_ack_arrives_while_a_later_request_still_gets_its_answer() {
        let (client, url, token) = start_test_mcp_listener();
        let listen = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "subscriptions/listen",
            "params": {
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": everyday_mcp::MODERN,
                    "io.modelcontextprotocol/clientCapabilities": {},
                },
            },
        });
        let request = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n";
        let input = format!("{listen}\n{request}");

        let sink = SharedSink::default();
        let probe = sink.clone();
        std::thread::spawn(move || {
            // Abandoned deliberately at the end of this closure: see the
            // doc above for why `mcp_pipe` does not return here, and why
            // that is fine to leave running past this test.
            let _ = mcp_pipe(&client, &url, &token, input.as_bytes(), sink);
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let text = loop {
            let text = String::from_utf8(probe.0.lock().unwrap().clone()).unwrap();
            if text.lines().count() >= 2 {
                break text;
            }
            assert!(std::time::Instant::now() < deadline, "timed out waiting for both replies");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };

        let parsed: Vec<serde_json::Value> = text
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .collect();
        let ack = parsed
            .iter()
            .find(|v| v["method"] == "notifications/subscriptions/acknowledged")
            .unwrap_or_else(|| panic!("no acknowledgement line in {text}"));
        assert!(
            ack["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"].as_str().is_some(),
            "{text}"
        );

        let reply = parsed
            .iter()
            .find(|v| v["id"] == 2)
            .unwrap_or_else(|| panic!("no reply to the second request in {text}"));
        assert!(reply.get("result").is_some(), "{text}");
    }
}
