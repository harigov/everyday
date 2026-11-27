//! Command line surface: the argument types `clap` derives from.
//!
//! Nothing here does anything -- `app::run` is where a parsed `Cli` turns
//! into a vault being opened and a verb being run. Kept apart because a
//! flag's help text and its default value are a different kind of change
//! from what the verb behind it does, and the two used to live in one
//! file large enough that finding either meant scrolling past the other.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "jatayu", about = "A private journal", version, disable_help_subcommand = true)]
pub struct Cli {
    /// Vault directory. Defaults to the platform data directory.
    #[arg(long, short = 'C', global = true, env = "JATAYU_VAULT")]
    pub(crate) vault: Option<PathBuf>,

    /// Vault password. Prefer the prompt or `JATAYU_PASSWORD`; a password
    /// passed as an argument is visible to every process on the machine.
    #[arg(long, global = true, env = "JATAYU_PASSWORD", hide_env_values = true)]
    pub(crate) password: Option<String>,

    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create a new vault.
    Init {
        /// Storage backend: `sqlite` for a database in the vault folder,
        /// `postgres` for one on a server.
        #[arg(long, default_value = jatayu_vault::DEFAULT_BACKEND)]
        backend: String,
        /// Backend setting, as `key=value`. Repeatable.
        ///
        /// What a backend needs depends on the backend: `sqlite` needs
        /// nothing, `postgres` needs
        /// `--set url=postgresql://user:password@host:5432/database` and
        /// optionally `--set schema=jatayu`. Settings are sealed under the
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
    /// Serve this vault to other copies of Jatayu.
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
        #[arg(long, default_value_t = jatayu_server::DEFAULT_PORT)]
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
        /// Also open the interface to a browser on this machine's tailnet.
        ///
        /// Opt-in here for the same reason as `--mcp`: the switch in the
        /// desktop app's settings is somebody at a keyboard deciding, and a
        /// configuration directory that travels must not decide it for a
        /// daemon. Everything else -- the port, which tailnet devices may
        /// connect -- still comes from `web.json`. Answers only on this
        /// machine's Tailscale addresses, with Tailscale's certificate; see
        /// `docs/plans/web-access.md`.
        #[arg(long)]
        web: bool,
        /// Where the built interface is, for `--web` (`npm run build` in
        /// `ui/` writes it to `ui/dist`).
        #[arg(long, value_name = "DIR", env = "JATAYU_WEB_UI")]
        web_ui: Option<std::path::PathBuf>,
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
    /// the MCP listener a running Jatayu already serves on a loopback
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
    /// Carry an installation made under an earlier name -- Karma Yoga, or
    /// Every Day before it -- across: its settings and data folders, moved
    /// to their new names, with links left at the old ones.
    ///
    /// Any command, and the app, does this by itself before it reads a
    /// folder; this says what happened, or with `--dry-run` what would,
    /// without doing any of it.
    Migrate {
        /// Report each step without taking it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Run one of the assistant's tools, with no model in the loop.
    ///
    /// The same verbs the assistant has, for a script or a keyboard shortcut.
    /// `jatayu do list_tools` is not a thing -- use `--list`.
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
    /// Day format anywhere in it -- see `jatayu export --list`.
    ///
    /// This is not a backup. It is your writing with the encryption taken
    /// off, and it leaves out the assistant's key and the devices you have
    /// paired. `jatayu backup` is the one that copies the vault sealed.
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
    /// For copies made on a schedule, kept to a number, in a folder or a
    /// bucket, see `jatayu backups`.
    Backup { dir: PathBuf },
    /// Sealed, date-stamped backups in a folder or a bucket, as Settings →
    /// Backup makes them.
    #[command(subcommand)]
    Backups(BackupsCmd),
    /// Lay a backup down as a vault that opens with the same password.
    ///
    /// Needs no vault of its own -- this is for the day the vault is gone.
    /// The folder must be empty or not exist yet.
    Restore {
        /// Where the backups are: a folder, `s3://bucket/prefix` or
        /// `gs://bucket/prefix`.
        from: String,
        /// The folder to restore into.
        into: PathBuf,
        /// Which copy, by name (`jatayu backups list`). The newest by
        /// default.
        #[arg(long)]
        snapshot: Option<String>,
        /// Also put this computer's settings back -- paired devices, the
        /// server's certificate, the MCP token -- into the settings folder,
        /// never over a file that is already there.
        #[arg(long)]
        config: bool,
        /// Put the files folder's copy here rather than where the restored
        /// vault's settings say the folder is -- for a different machine, or
        /// a different user, than the one that made the backup.
        #[arg(long = "files-to", value_name = "DIR")]
        files_to: Option<PathBuf>,
        /// Restore a backup whose header says the vault had no password.
        /// Refused without this: that header is a plain file anybody who can
        /// write to the backup's place could have replaced, and an
        /// unencrypted backup is sealed under a key everybody knows. Say it
        /// only for a vault that never had a password, from a place only you
        /// can write to.
        #[arg(long)]
        unencrypted: bool,
        #[command(flatten)]
        location: LocationArgs,
    },
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

/// How to reach a bucket named on the command line.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct LocationArgs {
    /// S3: the bucket's region, when the profile does not say.
    #[arg(long)]
    pub region: Option<String>,
    /// S3: the base URL of an S3-compatible service (R2, B2, MinIO…).
    #[arg(long)]
    pub endpoint: Option<String>,
    /// S3: which profile in `~/.aws` to use.
    #[arg(long)]
    pub profile: Option<String>,
    /// Google Cloud Storage: a key file to use instead of the application
    /// default credentials.
    #[arg(long)]
    pub credentials_file: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum BackupsCmd {
    /// Where backups go, how often, and how the last one went.
    Status,
    /// Make a backup now: to where Settings → Backup says, or to `--to`.
    Run {
        /// A folder, `s3://bucket/prefix` or `gs://bucket/prefix`, instead
        /// of the destination Settings chose.
        #[arg(long)]
        to: Option<String>,
        /// How many copies to keep there, this one included. Settings' own
        /// number by default.
        #[arg(long)]
        keep: Option<u32>,
        /// Leave this computer's settings out of the copy.
        #[arg(long)]
        no_config: bool,
        #[command(flatten)]
        location: LocationArgs,
    },
    /// List the copies kept.
    List {
        /// Somewhere other than the destination Settings chose.
        from: Option<String>,
        #[command(flatten)]
        location: LocationArgs,
    },
    /// Check that every copy is whole.
    Verify {
        from: Option<String>,
        /// Also download and open every piece of the newest copy -- the only
        /// check that no byte has changed, and as large as the vault.
        #[arg(long)]
        read: bool,
        #[command(flatten)]
        location: LocationArgs,
    },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_surface_is_well_formed() {
        // Catches conflicting short flags and bad defaults at test time
        // rather than on the user's first run.
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
