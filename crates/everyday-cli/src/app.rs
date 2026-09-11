//! Turn a parsed [`Cli`] into an open vault and a verb run against it.
//!
//! Deciding whether a vault exists, opening it, and asking for a password
//! belong here; what each subcommand actually does lives in `verbs.rs`,
//! `serve.rs` and `mcp_pipe.rs`. Three commands -- `init`, `backend` and
//! `serve` -- are dispatched before the ordinary open-and-unlock below,
//! each for its own reason spelled out where it is checked.

use crate::args::{Cli, Command};
use crate::serve::{self, ServeOptions};
use crate::verbs;
use crate::{mcp_pipe, verbs::prompt_password};
use everyday_core::{Error, Result};

pub fn run(cli: Cli) -> Result<()> {
    let path = cli.vault.clone().unwrap_or_else(everyday_vault::default_vault_dir);

    // `init` is the one command that must not require an existing vault.
    if let Command::Init { backend, settings, name, no_encryption } = &cli.command {
        return verbs::init(
            &path,
            backend,
            settings,
            name,
            *no_encryption,
            cli.password.as_deref(),
        );
    }

    // `mcp` is the other command dispatched before a vault is even looked
    // for, and for a stronger reason than `init`'s: it must never open one.
    // See its own doc, and `docs/plans/mcp.md`'s "Streamable HTTP is the
    // transport; stdio is a pipe to it" -- the second process to open a
    // vault gets it read-only, which is the one failure mode this command
    // exists to make impossible rather than to handle.
    if let Command::Mcp { port, token } = &cli.command {
        return mcp_pipe::mcp(*port, token.clone());
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
        return verbs::backend(&vault, settings, cli.password.as_deref());
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
        return serve::serve(
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
        Command::Status => verbs::status(&vault),
        Command::Journal(cmd) => verbs::journal(&vault, cmd),
        Command::New { journal, title, tags, date, star } => {
            verbs::new_entry(&vault, journal, title, tags, date, star)
        }
        Command::List { journal, limit, starred, json } => {
            verbs::list(&vault, journal, limit, starred, json)
        }
        Command::Show { id, json } => verbs::show(&vault, &id, json),
        Command::Search { query, limit } => verbs::search(&vault, &query.join(" "), limit),
        Command::Attach { id, file } => verbs::attach(&vault, &id, &file),
        Command::Delete { id } => verbs::delete(&vault, &id),
        Command::Export { path, parts, no_media, list } => {
            verbs::export(&vault, path.as_deref(), parts, !no_media, list)
        }
        Command::Import { path, parts, replace, yes } => {
            verbs::import(&vault, &path, parts, replace, yes)
        }
        Command::Passwd => verbs::passwd(&vault, cli.password.as_deref()),
        Command::Backup { dir } => verbs::backup(&vault, &dir),
        Command::Backend { .. } => unreachable!("handled above"),
        Command::Check => verbs::check(&vault),
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
            verbs::run_tool(vault, name, &arguments, list, confirm_destructive)
        }
        Command::Demo => verbs::demo(&vault),
    }
}
