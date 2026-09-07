//! The assembly point.
//!
//! [`everyday_core`] deliberately knows nothing about which storage backends
//! exist, and the backends know nothing about each other. This crate is
//! where they are introduced, so that both front ends -- the desktop shell
//! and the CLI -- get an identical set of backends, an identical default
//! vault location, and identical open/create semantics.
//!
//! Adding a storage backend means implementing
//! [`everyday_core::StoreFactory`] and adding one line to [`registry`].

pub mod media;

use everyday_core::store::BackendRegistry;
use everyday_core::{Error, Result, Vault, VaultConfig};
use everyday_store_markdown::MarkdownFactory;
use everyday_store_sqlite::SqliteFactory;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The backend the UI offers first.
pub const DEFAULT_BACKEND: &str = everyday_store_sqlite::BACKEND_ID;

/// Every storage backend this build knows how to open.
pub fn registry() -> Arc<BackendRegistry> {
    let mut reg = BackendRegistry::new();
    reg.register(SqliteFactory).register(MarkdownFactory);
    Arc::new(reg)
}

/// `(id, human description)` for each backend, for the vault-creation UI.
pub fn available_backends() -> Vec<(&'static str, &'static str)> {
    registry().describe_all()
}

/// Where a vault lives when the user has not chosen somewhere else.
///
/// Uses the platform's data directory rather than the document directory:
/// the vault is an opaque store with its own internal layout, not a file the
/// user is expected to open by hand. (A Markdown vault *is* meant to be
/// browsed, which is exactly why it is worth pointing somewhere memorable at
/// creation time.)
pub fn default_vault_dir() -> PathBuf {
    directories::ProjectDirs::from("app", "Every Day", "EveryDay")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".everyday"))
}

/// Where the shell records the vault it last had open.
///
/// A vault need not live in [`default_vault_dir`] -- someone may keep theirs
/// on an external disk or in a synced folder -- and being sent back to the
/// default location on every launch would make that unusable. This file is a
/// pointer and nothing else: it holds a path, never a key, a password or any
/// entry content. The default location is public knowledge anyway.
fn last_vault_pointer() -> Option<PathBuf> {
    directories::ProjectDirs::from("app", "Every Day", "EveryDay")
        .map(|d| d.config_dir().join("last-vault"))
}

/// The vault the previous session left open, if one was recorded.
///
/// The path comes back exactly as it was recorded. Whether a vault is still
/// there is the caller's question to ask -- an unplugged drive is not an
/// error here, it just means the default location should be used instead.
pub fn last_vault() -> Option<PathBuf> {
    read_pointer(&last_vault_pointer()?)
}

/// Record `path` as the vault to reopen on the next launch.
pub fn remember_vault(path: &Path) -> Result<()> {
    let file = last_vault_pointer().ok_or_else(|| {
        Error::Invalid("this platform has no config directory to record the vault in".into())
    })?;
    write_pointer(&file, path)
}

fn read_pointer(file: &Path) -> Option<PathBuf> {
    let recorded = std::fs::read_to_string(file).ok()?;
    let recorded = recorded.trim();
    if recorded.is_empty() {
        return None;
    }
    Some(PathBuf::from(recorded))
}

fn write_pointer(file: &Path, path: &Path) -> Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    std::fs::write(file, path.to_string_lossy().as_bytes()).map_err(|e| Error::io(file, e))
}

/// Open the vault at `path`, or report that there is nothing there.
///
/// An encrypted vault comes back **locked**; call
/// [`everyday_core::Vault::unlock`] with the password.
pub fn open(path: &Path) -> Result<Vault> {
    Vault::open(path, registry())
}

/// Create a new vault. Fails if one already exists at `path`.
pub fn create(path: &Path, config: VaultConfig) -> Result<Vault> {
    Vault::create(path, config, registry())
}

/// Open the vault at `path`, creating it from `config` if absent.
///
/// Returns the vault and whether it was created, so the caller can show a
/// welcome screen rather than a password prompt on first run.
pub fn open_or_create(path: &Path, config: VaultConfig) -> Result<(Vault, bool)> {
    if Vault::exists(path) { Ok((open(path)?, false)) } else { Ok((create(path, config)?, true)) }
}

/// Does a vault exist at `path`?
pub fn exists(path: &Path) -> bool {
    Vault::exists(path)
}

/// Reject a password the user would regret.
///
/// Deliberately minimal: length is the only property that reliably predicts
/// resistance to offline attack, and composition rules mostly push people
/// toward `Password1!`. There is no password recovery in Every Day -- the
/// data key is wrapped by this password and nothing else -- so the UI must
/// say so loudly rather than lean on a strength meter.
pub fn validate_password(password: &str) -> Result<()> {
    let chars = password.chars().count();
    if chars < 8 {
        return Err(Error::Invalid(format!(
            "password must be at least 8 characters (got {chars})"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_shipped_backends_are_registered() {
        let ids = registry().ids();
        assert!(ids.contains(&"sqlite"), "sqlite backend missing: {ids:?}");
        assert!(ids.contains(&"markdown"), "markdown backend missing: {ids:?}");
    }

    #[test]
    fn every_backend_has_a_description_for_the_picker() {
        for (id, desc) in available_backends() {
            assert!(!desc.is_empty(), "backend {id} has no description");
        }
    }

    #[test]
    fn the_default_backend_is_actually_registered() {
        assert!(registry().ids().contains(&DEFAULT_BACKEND));
    }

    #[test]
    fn a_recorded_vault_path_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("last-vault");
        let vault = dir.path().join("somewhere else/my vault");

        write_pointer(&file, &vault).unwrap();
        assert_eq!(read_pointer(&file), Some(vault));
    }

    #[test]
    fn recording_creates_the_config_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("never/made/yet/last-vault");

        write_pointer(&file, Path::new("/tmp/vault")).unwrap();
        assert!(file.exists(), "parent directories should be created");
    }

    #[test]
    fn nothing_recorded_means_no_last_vault() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_pointer(&dir.path().join("absent")), None);

        // A truncated or hand-emptied pointer is "nothing recorded", not a
        // vault at the empty path.
        let blank = dir.path().join("blank");
        std::fs::write(&blank, "  \n").unwrap();
        assert_eq!(read_pointer(&blank), None);
    }

    #[test]
    fn open_or_create_creates_then_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault");
        let cfg = VaultConfig {
            password: Some("correct horse battery".into()),
            kdf: everyday_core::crypto::KdfParams::insecure_fast(),
            ..Default::default()
        };

        let (vault, created) = open_or_create(&path, cfg.clone()).unwrap();
        assert!(created);
        assert!(vault.is_unlocked(), "a freshly created vault is already unlocked");
        drop(vault);

        let (vault, created) = open_or_create(&path, cfg).unwrap();
        assert!(!created, "the second call must open, not create");
        assert!(!vault.is_unlocked(), "an existing encrypted vault opens locked");
    }

    #[test]
    fn a_vault_can_be_created_on_either_backend() {
        for backend in ["sqlite", "markdown"] {
            let dir = tempfile::tempdir().unwrap();
            let cfg = VaultConfig {
                backend: backend.into(),
                password: Some("correct horse battery".into()),
                kdf: everyday_core::crypto::KdfParams::insecure_fast(),
                ..Default::default()
            };
            let vault = create(dir.path(), cfg).unwrap();
            assert_eq!(vault.status().backend, backend);
        }
    }

    #[test]
    fn the_todo_app_works_end_to_end_on_the_default_backend() {
        // The whole stack the desktop shell talks to: a real vault, a real
        // SQLite file, a real cipher. The unit tests below the vault prove
        // each layer; this proves they are wired to each other.
        use everyday_core::store::tasks::{BlockQuery, TaskQuery};
        use everyday_core::task::{
            BlockKind, BlockSubject, Priority, Project, Task, TaskStatus, TimeBlock,
        };

        let dir = tempfile::tempdir().unwrap();
        let cfg = VaultConfig {
            password: Some("correct horse battery".into()),
            kdf: everyday_core::crypto::KdfParams::insecure_fast(),
            ..Default::default()
        };
        let vault = create(dir.path(), cfg.clone()).unwrap();
        assert!(vault.supports_tasks(), "the default backend must carry the task domain");

        let mut project = Project::new("Repaint the hall");
        project.due_date = Some(jiff::civil::date(2026, 5, 1));
        project.tags = vec!["home".into()];
        vault.save_project(&project).unwrap();

        let mut task = Task::new("Buy the paint").in_project(project.id);
        task.priority = Priority::High;
        task.estimate_minutes = Some(45);
        task.tags = vec!["errand".into(), "home".into()];
        vault.save_task(&task).unwrap();

        let subtask = Task::new("Match the old colour").in_project(project.id).under(task.id);
        vault.save_task(&subtask).unwrap();

        let start = "2026-04-20T09:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let logged = TimeBlock::new(BlockSubject::Task { id: task.id }, start, 30, "UTC")
            .of_kind(BlockKind::Actual);
        vault.save_block(&logged).unwrap();

        // Tags come from all three record kinds, most used first.
        assert_eq!(
            vault.task_tags().unwrap(),
            vec![("home".to_string(), 2), ("errand".to_string(), 1)],
        );

        let today = jiff::civil::date(2026, 4, 20);
        let stats = vault.task_stats(today).unwrap();
        assert_eq!((stats.projects, stats.tasks, stats.open_tasks), (1, 2, 2));
        assert_eq!(stats.logged_minutes, 30);

        // Lock, reopen, unlock: the tasks must still be there and still
        // decryptable, which is the part that would break if the task
        // records were sealed against the wrong associated data.
        drop(vault);
        let vault = open(dir.path()).unwrap();
        assert_eq!(
            vault.tasks(&TaskQuery::default()).unwrap_err().code(),
            "locked",
            "a reopened encrypted vault must not hand out tasks"
        );
        vault.unlock(Some("correct horse battery")).unwrap();

        let round = vault.task(task.id).unwrap();
        assert_eq!(round, task, "a task must survive a lock/unlock cycle unchanged");
        assert_eq!(
            vault.tasks(&TaskQuery::children_of(task.id)).unwrap(),
            std::slice::from_ref(&subtask)
        );
        assert_eq!(vault.blocks(&BlockQuery::for_task(task.id)).unwrap().len(), 1);

        // And the cascade holds through the vault, not just the store.
        vault.delete_project(project.id).unwrap();
        assert!(vault.tasks(&TaskQuery::default()).unwrap().is_empty());
        assert!(vault.blocks(&BlockQuery::default()).unwrap().is_empty());
        assert_eq!(vault.task_stats(today).unwrap().tasks, 0);

        let _ = TaskStatus::Todo; // the status set is part of the contract
    }

    #[test]
    fn the_vault_rejects_tasks_that_would_be_unusable() {
        use everyday_core::task::{BlockSubject, Task, TimeBlock};

        let dir = tempfile::tempdir().unwrap();
        let cfg = VaultConfig {
            password: Some("correct horse battery".into()),
            kdf: everyday_core::crypto::KdfParams::insecure_fast(),
            ..Default::default()
        };
        let vault = create(dir.path(), cfg).unwrap();

        // An untitled task is a row nothing can render and nobody can find.
        let blank = Task::new("   ");
        assert_eq!(vault.save_task(&blank).unwrap_err().code(), "invalid");

        // Its own parent: a cycle the subtree walk would have to defend
        // against for ever after.
        let mut loop_task = Task::new("ouroboros");
        loop_task.parent_id = Some(loop_task.id);
        assert_eq!(vault.save_task(&loop_task).unwrap_err().code(), "invalid");

        // A block that ends before it starts would subtract from a total.
        let start = "2026-04-20T09:00:00Z".parse::<jiff::Timestamp>().unwrap();
        let mut backwards = TimeBlock::new(BlockSubject::Adhoc, start, 30, "UTC");
        backwards.end = start - jiff::SignedDuration::from_mins(10);
        assert_eq!(vault.save_block(&backwards).unwrap_err().code(), "invalid");

        // A rejected write must leave nothing behind.
        assert!(vault.tasks(&Default::default()).unwrap().is_empty());
        assert!(vault.blocks(&Default::default()).unwrap().is_empty());
    }

    #[test]
    fn subscribing_to_a_calendar_works_end_to_end_on_the_default_backend() {
        // The whole stack the shell talks to, for the third domain: a real
        // vault, a real SQLite file, a real cipher, and a real feed -- the
        // shape Google, Outlook and Apple all publish, folded lines and all.
        use everyday_core::calendar::Calendar;
        use everyday_core::store::calendars::EventQuery;

        let dir = tempfile::tempdir().unwrap();
        let cfg = VaultConfig {
            password: Some("correct horse battery".into()),
            kdf: everyday_core::crypto::KdfParams::insecure_fast(),
            ..Default::default()
        };
        let vault = create(dir.path(), cfg).unwrap();
        assert!(vault.supports_calendars(), "the default backend must carry the calendar domain");

        let calendar =
            Calendar::subscribed("Work", "webcal://example.com/work.ics").with_color("#0f766e");
        vault.save_calendar(&calendar).unwrap();
        assert_eq!(
            vault.calendar(calendar.id).unwrap().fetch_url().unwrap(),
            "https://example.com/work.ics",
            "webcal must be normalised on the way to the fetcher",
        );

        let feed = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Priya — Work\r\n\
             X-WR-TIMEZONE:Europe/Berlin\r\n\
             BEGIN:VEVENT\r\nUID:standup@example.com\r\nSUMMARY:Morning stand-\r\n up\r\n\
             DTSTART;TZID=Europe/Berlin:20260907T093000\r\n\
             DTEND;TZID=Europe/Berlin:20260907T094500\r\n\
             RRULE:FREQ=WEEKLY;BYDAY=MO,WE;COUNT=4\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:trip@example.com\r\nSUMMARY:Lisbon\r\n\
             DTSTART;VALUE=DATE:20260910\r\nDTEND;VALUE=DATE:20260921\r\nEND:VEVENT\r\n\
             END:VCALENDAR\r\n";

        let window = (jiff::civil::date(2026, 1, 1), jiff::civil::date(2026, 12, 31));
        let report = vault.sync_calendar_from_ics(calendar.id, feed, window, "UTC").unwrap();
        assert_eq!(report.events, 5, "four stand-ups and one trip");
        assert_eq!(report.feed_name.as_deref(), Some("Priya — Work"));

        // The week query is an overlap test, so it finds the trip from the
        // middle of it as well as the stand-ups that start inside it.
        let week = vault
            .events(&EventQuery::between(
                jiff::civil::date(2026, 9, 14),
                jiff::civil::date(2026, 9, 20),
            ))
            .unwrap();
        assert!(
            week.iter().any(|e| e.title == "Lisbon"),
            "a trip must show in its own second week",
        );
        assert!(week.iter().any(|e| e.title == "Morning stand-up"), "folded titles are rejoined",);

        // Syncing again replaces rather than duplicates.
        let report = vault.sync_calendar_from_ics(calendar.id, feed, window, "UTC").unwrap();
        assert_eq!(report.events, 5);
        assert_eq!(
            vault.event_count(calendar.id).unwrap(),
            5,
            "a sync replaces, it does not append",
        );
        assert!(vault.calendar(calendar.id).unwrap().last_synced_at.is_some());

        // Lock, reopen, unlock: the events must still decrypt, which is what
        // would break if they were sealed against the wrong associated data.
        drop(vault);
        let vault = open(dir.path()).unwrap();
        assert_eq!(vault.calendars().unwrap_err().code(), "locked");
        vault.unlock(Some("correct horse battery")).unwrap();
        assert_eq!(vault.event_count(calendar.id).unwrap(), 5);

        // A reply that is not a calendar keeps what is already there.
        let err = vault
            .sync_calendar_from_ics(calendar.id, "<html>Sign in to the wifi</html>", window, "UTC")
            .unwrap_err();
        assert_eq!(err.code(), "invalid", "got {err}");
        assert_eq!(vault.event_count(calendar.id).unwrap(), 5, "a bad reply must not empty a feed",);
        vault.mark_calendar_failed(calendar.id, "that address did not return a calendar").unwrap();
        let after = vault.calendar(calendar.id).unwrap();
        assert!(after.last_error.is_some());
        assert!(after.last_synced_at.is_some(), "a failure must not erase when it last worked");

        // And unsubscribing takes the events with it.
        vault.delete_calendar(calendar.id).unwrap();
        assert!(vault.calendars().unwrap().is_empty());
        assert!(vault.events(&EventQuery::default()).unwrap().is_empty());
    }

    #[test]
    fn a_markdown_vault_offers_journals_but_not_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = VaultConfig {
            backend: "markdown".into(),
            password: Some("correct horse battery".into()),
            kdf: everyday_core::crypto::KdfParams::insecure_fast(),
            ..Default::default()
        };
        let vault = create(dir.path(), cfg).unwrap();
        assert!(!vault.supports_tasks());
        assert!(!vault.supports_calendars());
        let caps = vault.status().capabilities.unwrap();
        assert!(!caps.tasks && !caps.calendars);
        assert_eq!(vault.projects().unwrap_err().code(), "unsupported");
        assert_eq!(vault.calendars().unwrap_err().code(), "unsupported");
    }

    #[test]
    fn the_default_vault_directory_is_absolute_and_named() {
        let dir = default_vault_dir();
        assert!(dir.is_absolute(), "got a relative path: {dir:?}");
        // Each platform spells it differently -- `~/.local/share/everyday`,
        // `~/Library/Application Support/app.Every-Day.EveryDay`,
        // `%APPDATA%\\Every Day\\EveryDay` -- so match case-insensitively.
        let s = dir.to_string_lossy().to_lowercase().replace([' ', '-'], "");
        assert!(s.contains("everyday"), "got {dir:?}");
    }

    #[test]
    fn short_passwords_are_rejected() {
        assert!(validate_password("short").is_err());
        assert!(validate_password("").is_err());
        assert!(validate_password("12345678").is_ok());
        // Length is counted in characters, not bytes.
        assert!(validate_password("\u{1f600}\u{1f600}\u{1f600}").is_err());
    }
}
