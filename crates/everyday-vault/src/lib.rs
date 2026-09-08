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

    /// A real vault over the SQLite backend, unencrypted for speed: these
    /// tests are about the rules the vault applies, not about the envelope.
    fn a_vault(dir: &Path) -> Vault {
        create(dir, VaultConfig { password: None, ..Default::default() }).unwrap()
    }

    fn a_journal_tracking(
        vault: &Vault,
        tracker: everyday_core::Tracker,
    ) -> everyday_core::Journal {
        let mut journal = everyday_core::Journal::new("Health");
        journal.trackers.push(tracker);
        vault.save_journal(&journal).unwrap();
        vault.journal(journal.id).unwrap()
    }

    #[test]
    fn a_reading_is_clamped_by_the_tracker_that_defines_it() {
        // The reason the vault looks the definition up rather than trusting
        // the caller: a severity of 99 on a scale of ten is a chart with an
        // axis to the moon and a thousand rows to search for the cause.
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());
        let mut pain = everyday_core::Tracker::new("Headache", everyday_core::TrackerKind::Scale);
        pain.scale_max = 10.0;
        let journal = a_journal_tracking(&vault, pain);
        let tracker_id = journal.trackers[0].id;

        let reading = everyday_core::Reading::on(
            journal.id,
            tracker_id,
            jiff::civil::Date::constant(2026, 3, 14),
            99.0,
        );
        vault.save_reading(&reading).unwrap();
        assert_eq!(vault.reading(reading.id).unwrap().value, 10.0);
    }

    #[test]
    fn a_reading_naming_no_tracker_is_refused() {
        // Otherwise it is an unnameable row: a number against an id that
        // nothing in the vault can turn back into a word.
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());
        let journal = everyday_core::Journal::new("Health");
        vault.save_journal(&journal).unwrap();

        let orphan = everyday_core::Reading::on(
            journal.id,
            everyday_core::TrackerId::new(),
            jiff::civil::Date::constant(2026, 3, 14),
            1.0,
        );
        let err = vault.save_reading(&orphan).unwrap_err();
        assert_eq!(err.code(), "invalid", "got {err}");
    }

    #[test]
    fn a_timed_reading_is_filed_under_the_day_its_instant_falls_on() {
        // The instant is the more precise of the two, so it decides the day.
        // Otherwise a dose taken at 00:10 lands on the calendar a day away
        // from the entry it was ticked under.
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());
        let journal = a_journal_tracking(
            &vault,
            everyday_core::Tracker::new("Ibuprofen", everyday_core::TrackerKind::Dose),
        );

        let mut reading = everyday_core::Reading::on(
            journal.id,
            journal.trackers[0].id,
            jiff::civil::Date::constant(2026, 3, 14),
            400.0,
        );
        reading.at = Some("2026-03-15T09:00:00Z".parse().unwrap());
        reading.tz = "UTC".into();
        vault.save_reading(&reading).unwrap();

        let stored = vault.reading(reading.id).unwrap();
        assert_eq!(stored.local_date, jiff::civil::Date::constant(2026, 3, 15));
    }

    #[test]
    fn deleting_a_tracker_takes_its_readings_and_leaves_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());
        let mut journal = everyday_core::Journal::new("Health");
        journal
            .trackers
            .push(everyday_core::Tracker::new("Ibuprofen", everyday_core::TrackerKind::Dose));
        journal
            .trackers
            .push(everyday_core::Tracker::new("Floss", everyday_core::TrackerKind::Check));
        vault.save_journal(&journal).unwrap();
        let (gone, kept) = (journal.trackers[0].id, journal.trackers[1].id);

        let day = jiff::civil::Date::constant(2026, 3, 14);
        for (tracker, value) in [(gone, 400.0), (gone, 400.0), (kept, 1.0)] {
            vault
                .save_reading(&everyday_core::Reading::on(journal.id, tracker, day, value))
                .unwrap();
        }

        assert_eq!(vault.delete_tracker(journal.id, gone).unwrap(), 2);

        let left = vault.readings(&everyday_core::ReadingQuery::default()).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].tracker_id, kept);
        // And the definition has left the journal with them.
        let after = vault.journal(journal.id).unwrap();
        assert_eq!(after.trackers.len(), 1);
        assert_eq!(after.trackers[0].id, kept);
    }

    #[test]
    fn archiving_a_tracker_keeps_every_reading_it_ever_made() {
        // The non-destructive half of the pair, and the usual answer to "I
        // stopped taking this in March": off the page, still in the data.
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());
        let journal = a_journal_tracking(
            &vault,
            everyday_core::Tracker::new("Sertraline", everyday_core::TrackerKind::Dose),
        );
        let tracker_id = journal.trackers[0].id;
        vault
            .save_reading(&everyday_core::Reading::on(
                journal.id,
                tracker_id,
                jiff::civil::Date::constant(2026, 3, 14),
                50.0,
            ))
            .unwrap();

        let mut journal = vault.journal(journal.id).unwrap();
        journal.trackers[0].archived = true;
        vault.save_journal(&journal).unwrap();

        let after = vault.journal(journal.id).unwrap();
        assert_eq!(after.active_trackers().count(), 0, "an archived tracker leaves the page");
        assert_eq!(
            vault.readings(&everyday_core::ReadingQuery::default()).unwrap().len(),
            1,
            "and takes nothing with it"
        );
    }

    #[test]
    fn a_tracker_saved_with_nonsense_in_it_is_tidied_on_the_way_in() {
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());
        let mut journal = everyday_core::Journal::new("Health");
        let mut tracker =
            everyday_core::Tracker::new("  Water  ", everyday_core::TrackerKind::Amount);
        tracker.scale_max = f64::NAN;
        tracker.default_value = -4.0;
        journal.trackers.push(tracker);
        // A nameless tracker is not a tracker; it is an empty row in a form.
        journal
            .trackers
            .push(everyday_core::Tracker::new("   ", everyday_core::TrackerKind::Check));
        vault.save_journal(&journal).unwrap();

        let stored = vault.journal(journal.id).unwrap();
        assert_eq!(stored.trackers.len(), 1);
        assert_eq!(stored.trackers[0].name, "Water");
        assert_eq!(stored.trackers[0].default_value, 1.0);
        assert!(stored.trackers[0].scale_max.is_finite());
    }

    #[test]
    fn a_markdown_vault_says_it_cannot_track_rather_than_failing_at_click_time() {
        let dir = tempfile::tempdir().unwrap();
        let vault = create(
            dir.path(),
            VaultConfig { password: None, backend: "markdown".into(), ..Default::default() },
        )
        .unwrap();
        assert!(!vault.supports_trackers());
        assert!(!vault.status().capabilities.unwrap().trackers);
        // And the call that would be hidden behind that flag fails clearly.
        let err = vault.readings(&everyday_core::ReadingQuery::default()).unwrap_err();
        assert_eq!(err.code(), "unsupported", "got {err}");
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

    // ---- the assistant ---------------------------------------------------

    #[test]
    fn a_message_names_its_thread_and_floats_it_to_the_top() {
        // Two facts that must not be separable: a turn that did not move its
        // conversation up the history list is one somebody will fail to find
        // again, and an untitled thread is indistinguishable from every
        // other untitled thread.
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());

        let older = everyday_core::Conversation::new();
        vault.save_conversation(&older).unwrap();
        let newer = everyday_core::Conversation::new();
        vault.save_conversation(&newer).unwrap();

        vault
            .save_message(&everyday_core::Message::user(
                older.id,
                "Go through my open projects and tell me which have gone stale",
            ))
            .unwrap();

        let listed = vault.conversations(&everyday_core::ConversationQuery::default()).unwrap();
        assert_eq!(listed[0].id, older.id, "the thread just written to should be first");
        assert!(
            listed[0].title.starts_with("Go through my open projects"),
            "got {:?}",
            listed[0].title
        );

        // ...and the title is only taken once. A second question must not
        // rename the thread out from under whoever is reading the list.
        vault
            .save_message(&everyday_core::Message::user(older.id, "actually, never mind"))
            .unwrap();
        let again = vault.conversation(older.id).unwrap();
        assert!(again.title.starts_with("Go through my open projects"));
        assert_eq!(vault.message_count(older.id).unwrap(), 2);
    }

    #[test]
    fn an_assistants_own_turn_never_becomes_the_title() {
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());
        let c = everyday_core::Conversation::new();
        vault.save_conversation(&c).unwrap();

        vault.save_message(&everyday_core::Message::assistant(c.id, "Hello!")).unwrap();
        assert!(vault.conversation(c.id).unwrap().title.is_empty());
    }

    #[test]
    fn the_oldest_memories_are_evicted_and_the_pinned_ones_are_not() {
        // The trim is the vault's job rather than the assistant's because an
        // agent asked to tidy up after itself does not, and every memory is
        // loaded into the system prompt on every request forever.
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());
        let cap = everyday_core::agent::MAX_MEMORIES;

        // One pinned memory, written first so it is the oldest and would be
        // the first thing an unguarded trim reached for.
        let pinned =
            everyday_core::Memory { pinned: true, ..everyday_core::Memory::new("Typed by hand") };
        assert!(vault.save_memory(&pinned).unwrap().is_empty());

        for i in 0..cap - 1 {
            let m = everyday_core::Memory::new(format!("fact {i}"));
            assert!(vault.save_memory(&m).unwrap().is_empty(), "under the cap, nothing is dropped");
        }
        assert_eq!(vault.memories().unwrap().len(), cap);

        let overflow = everyday_core::Memory::new("one too many");
        let evicted = vault.save_memory(&overflow).unwrap();
        assert_eq!(evicted.len(), 1, "exactly the overflow is dropped");
        assert_eq!(evicted[0].text, "fact 0", "the oldest unpinned memory goes");

        let kept = vault.memories().unwrap();
        assert_eq!(kept.len(), cap);
        assert!(kept.iter().any(|m| m.id == pinned.id), "a hand-written memory must survive");
        assert!(kept.iter().any(|m| m.id == overflow.id), "the memory just written must survive");
    }

    #[test]
    fn the_api_key_never_comes_back_through_the_settings() {
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());

        vault.set_agent_key("sk-secret").unwrap();
        let settings = vault.agent_settings().unwrap();
        assert!(settings.has_key, "the pane needs to know one is set");
        // The only place the key is reachable is `agent_credentials`, and
        // that is gated on the assistant being switched on.
        let json = serde_json::to_string(&settings).unwrap();
        assert!(!json.contains("sk-secret"), "the key must not ride along with the settings");
    }

    #[test]
    fn credentials_are_refused_until_the_assistant_is_configured() {
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());

        // Off is off, key or no key.
        vault.set_agent_key("sk-secret").unwrap();
        assert!(vault.agent_credentials().is_err(), "a switched-off assistant has no credentials");

        let mut settings = everyday_core::AgentSettings { enabled: true, ..Default::default() };
        vault.save_agent_settings(&settings).unwrap();
        let (back, key) = vault.agent_credentials().unwrap();
        assert!(back.enabled);
        assert_eq!(key.as_deref(), Some("sk-secret"));

        // Enabled, remote, and keyless is the half-configured state that
        // must fail here rather than as a 401 nobody can act on.
        vault.clear_agent_key().unwrap();
        let err = vault.agent_credentials().unwrap_err();
        assert!(err.to_string().contains("API key"), "got {err}");

        // ...but a model on this machine needs no key at all.
        settings.model.base_url = Some("http://localhost:11434/v1".into());
        vault.save_agent_settings(&settings).unwrap();
        let (_, key) = vault.agent_credentials().unwrap();
        assert!(key.is_none(), "a local model should work with nothing configured but its address");
    }

    #[test]
    fn settings_that_could_not_be_acted_on_are_refused_at_the_pane() {
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());

        let mut settings = everyday_core::AgentSettings { enabled: true, ..Default::default() };
        settings.model.model = String::new();
        assert!(vault.save_agent_settings(&settings).is_err(), "a model name is required");

        settings.model.model = "gpt-5.1".into();
        settings.model.base_url = Some("http://gateway.example.com/v1".into());
        assert!(
            vault.save_agent_settings(&settings).is_err(),
            "plain http off this machine would leak the key"
        );
    }

    // ---- the assistant's tools, end to end -------------------------------
    //
    // These run the real catalogue against a real vault. Everything the
    // assistant can do to somebody's data goes through `dispatch`, so this
    // is where "it marked the wrong task done" is caught -- offline, with no
    // API key, and independently of whichever harness is calling it.

    use everyday_core::agent::tools::{self, Effect, ToolContext};

    const TODAY: jiff::civil::Date = jiff::civil::Date::constant(2026, 9, 8);

    fn ctx(vault: &Vault) -> ToolContext<'_> {
        ToolContext { vault, today: TODAY, tz: "UTC", conversation: None }
    }

    fn call(vault: &Vault, tool: &str, args: serde_json::Value) -> serde_json::Value {
        tools::dispatch(&ctx(vault), tool, &args).unwrap_or_else(|e| panic!("{tool} failed: {e}"))
    }

    fn call_err(vault: &Vault, tool: &str, args: serde_json::Value) -> String {
        tools::dispatch(&ctx(vault), tool, &args)
            .map(|v| panic!("{tool} unexpectedly succeeded: {v}"))
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn a_task_can_be_captured_found_and_finished_without_ever_being_deleted() {
        // The whole point of the tool surface, in one test: the assistant
        // completes work by moving its status, not by removing the record.
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());

        let project = call(&vault, "create_project", serde_json::json!({ "name": "The deck" }));
        let project_id = project["id"].as_str().unwrap().to_string();

        let created = call(
            &vault,
            "create_task",
            serde_json::json!({
                "title": "Order the timber",
                "project_id": project_id,
                "due_date": "2026-09-14",
                "priority": "high",
                "tags": ["shopping"],
            }),
        );
        assert_eq!(created["action"], "created");
        let task_id = created["id"].as_str().unwrap().to_string();

        // Found by the filters a model would actually reach for.
        let found = call(
            &vault,
            "list_tasks",
            serde_json::json!({ "project_id": project_id, "open_only": true }),
        );
        assert_eq!(found["count"], 1);
        assert_eq!(found["tasks"][0]["title"], "Order the timber");
        assert_eq!(found["tasks"][0]["due_date"], "2026-09-14");
        assert_eq!(found["tasks"][0]["priority"], "high");

        // Finished, not deleted.
        call(&vault, "update_task", serde_json::json!({ "task_id": task_id, "status": "done" }));
        let after = call(&vault, "get_task", serde_json::json!({ "task_id": task_id }));
        assert_eq!(after["status"], "done");
        assert_eq!(
            call(&vault, "list_tasks", serde_json::json!({ "open_only": true }))["count"],
            0
        );

        // ...and the project count in the overview follows.
        let overview = call(&vault, "overview", serde_json::json!({}));
        assert_eq!(overview["tasks"]["open"], 0);
        assert_eq!(overview["tasks"]["done"], 1);
        assert_eq!(overview["today"], "2026-09-08");
    }

    #[test]
    fn an_update_leaves_every_field_it_was_not_given() {
        // The failure this guards against is the quiet one: a model that
        // sends only the field it means to change must not blank the rest.
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());

        let id = call(
            &vault,
            "create_task",
            serde_json::json!({
                "title": "Ring the vet",
                "notes": "About the booster",
                "due_date": "2026-09-10",
                "tags": ["pets", "calls"],
                "estimate_minutes": 15,
            }),
        )["id"]
            .as_str()
            .unwrap()
            .to_string();

        call(&vault, "update_task", serde_json::json!({ "task_id": id, "priority": "urgent" }));

        let t = call(&vault, "get_task", serde_json::json!({ "task_id": id }));
        assert_eq!(t["priority"], "urgent");
        assert_eq!(t["title"], "Ring the vet", "the title must survive an unrelated edit");
        assert_eq!(t["notes"], "About the booster");
        assert_eq!(t["due_date"], "2026-09-10");
        assert_eq!(t["tags"], serde_json::json!(["pets", "calls"]));
        assert_eq!(t["estimate_minutes"], 15);
    }

    #[test]
    fn clearing_a_field_needs_its_own_flag_because_a_string_cannot_say_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());

        let project_id =
            call(&vault, "create_project", serde_json::json!({ "name": "Deck" }))["id"]
                .as_str()
                .unwrap()
                .to_string();
        let id = call(
            &vault,
            "create_task",
            serde_json::json!({
                "title": "Order timber",
                "project_id": project_id,
                "due_date": "2026-09-14",
            }),
        )["id"]
            .as_str()
            .unwrap()
            .to_string();

        call(
            &vault,
            "update_task",
            serde_json::json!({ "task_id": id, "clear_due_date": true, "clear_project": true }),
        );
        let t = call(&vault, "get_task", serde_json::json!({ "task_id": id }));
        assert!(t.get("due_date").is_none(), "the deadline should be gone: {t}");
        assert!(t.get("project_id").is_none(), "it should be back in the inbox: {t}");
    }

    #[test]
    fn an_entry_round_trips_as_markdown_because_that_is_what_a_model_writes() {
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());
        let journal = everyday_core::Journal::new("Daily");
        vault.save_journal(&journal).unwrap();

        let created = call(
            &vault,
            "create_entry",
            serde_json::json!({
                "journal_id": journal.id.to_string(),
                "title": "Deck, day one",
                "body": "Cut the joists.\n\nRan out of screws.",
                "date": "2026-09-07",
                "tags": ["deck"],
            }),
        );
        let id = created["id"].as_str().unwrap().to_string();

        let back = call(&vault, "get_entry", serde_json::json!({ "entry_id": id }));
        assert_eq!(back["title"], "Deck, day one");
        assert!(back["body"].as_str().unwrap().contains("Cut the joists"));
        assert!(back["body"].as_str().unwrap().contains("Ran out of screws"));
        assert_eq!(back["date"], "2026-09-07", "back-dating must work");

        // And it is findable by the search tool, which is how a model gets
        // an id in the first place.
        let hits = call(&vault, "search", serde_json::json!({ "query": "joists" }));
        assert_eq!(hits["count"], 1, "got {hits}");
        assert_eq!(hits["results"][0]["id"], id);
    }

    #[test]
    fn a_reading_reports_the_value_that_was_stored_rather_than_the_one_asked_for() {
        // The vault clamps to the tracker's scale. If the tool echoed the
        // argument, the assistant would tell somebody it recorded a 99 when
        // the vault holds a 10.
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());
        let mut pain = everyday_core::Tracker::new("Headache", everyday_core::TrackerKind::Scale);
        pain.scale_max = 10.0;
        let journal = a_journal_tracking(&vault, pain);
        let tracker_id = journal.trackers[0].id.to_string();

        let logged = call(
            &vault,
            "log_reading",
            serde_json::json!({ "tracker_id": tracker_id, "value": 99, "date": "2026-09-08" }),
        );
        assert_eq!(logged["value"], 10.0, "the clamped value is what to report");
        assert_eq!(logged["name"], "Headache");

        let summary = call(
            &vault,
            "tracker_summary",
            serde_json::json!({ "from": "2026-09-01", "to": "2026-09-08" }),
        );
        assert_eq!(summary["count"], 1);
        assert_eq!(summary["days"][0]["value"], 10.0);
        // A severity averages rather than sums: a 3 in the morning and a 3
        // at night is not a 6. The aggregate is named in the reply so the
        // model can say "averaging 10" rather than a bare number.
        assert_eq!(summary["days"][0]["aggregate"], "mean");
    }

    #[test]
    fn finishing_something_on_a_shelf_writes_the_log_the_year_in_review_reads() {
        // An item marked done without a log line is a book that silently
        // misses the list.
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());
        vault.seed_library().unwrap();

        let shelves = call(&vault, "list_shelves", serde_json::json!({}));
        let shelf = shelves[0]["id"].as_str().unwrap().to_string();

        let id = call(
            &vault,
            "create_item",
            serde_json::json!({ "shelf_id": shelf, "title": "Piranesi", "creator": "Susanna Clarke" }),
        )["id"]
            .as_str()
            .unwrap()
            .to_string();

        call(
            &vault,
            "update_item",
            serde_json::json!({ "item_id": id, "status": "done", "rating": 9 }),
        );

        let items = call(&vault, "list_items", serde_json::json!({ "status": "done" }));
        assert_eq!(items["count"], 1);
        assert_eq!(items["items"][0]["rating_out_of_10"], 9.0, "a rating out of ten survives");
        assert_eq!(
            items["items"][0]["finished_on"], "2026-09-08",
            "finishing without a date means today"
        );

        let logs = vault.logs(&everyday_core::LogQuery::default()).unwrap();
        assert_eq!(logs.len(), 1, "the finish must be logged");
        assert_eq!(logs[0].event, everyday_core::LogEvent::Finished);
    }

    #[test]
    fn a_rating_on_the_wrong_scale_is_refused_rather_than_recorded() {
        // 80 and 8 must not silently record wildly different opinions.
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());
        vault.seed_library().unwrap();
        let shelf = call(&vault, "list_shelves", serde_json::json!({}))[0]["id"]
            .as_str()
            .unwrap()
            .to_string();

        let err = call_err(
            &vault,
            "create_item",
            serde_json::json!({ "shelf_id": shelf, "title": "Dune", "rating": 80 }),
        );
        assert!(err.contains("out of 10"), "got {err}");
    }

    #[test]
    fn a_time_block_is_placed_at_the_wall_clock_time_it_was_given() {
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());
        let task_id =
            call(&vault, "create_task", serde_json::json!({ "title": "Deep work" }))["id"]
                .as_str()
                .unwrap()
                .to_string();

        call(
            &vault,
            "create_time_block",
            serde_json::json!({
                "date": "2026-09-09",
                "start_time": "09:00",
                "end_time": "10:30",
                "task_id": task_id,
            }),
        );

        let blocks = call(
            &vault,
            "list_time_blocks",
            serde_json::json!({ "from": "2026-09-09", "to": "2026-09-09" }),
        );
        assert_eq!(blocks["count"], 1);
        assert_eq!(blocks["blocks"][0]["minutes"], 90);
        assert_eq!(blocks["blocks"][0]["kind"], "planned");
        assert_eq!(blocks["blocks"][0]["for"]["task_id"], task_id);
    }

    #[test]
    fn a_block_must_say_what_the_time_is_for_and_may_only_say_it_once() {
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());
        let base = serde_json::json!({
            "date": "2026-09-09", "start_time": "09:00", "end_time": "10:00",
        });

        let err = call_err(&vault, "create_time_block", base.clone());
        assert!(err.contains("give one of"), "got {err}");

        let task_id = call(&vault, "create_task", serde_json::json!({ "title": "x" }))["id"]
            .as_str()
            .unwrap()
            .to_string();
        let mut both = base.clone();
        both["task_id"] = serde_json::json!(task_id);
        both["label"] = serde_json::json!("Lunch");
        let err = call_err(&vault, "create_time_block", both);
        assert!(err.contains("only one of"), "got {err}");

        // Backwards is refused rather than silently stored as zero minutes.
        let mut backwards = base;
        backwards["label"] = serde_json::json!("Lunch");
        backwards["end_time"] = serde_json::json!("08:00");
        let err = call_err(&vault, "create_time_block", backwards);
        assert!(err.contains("after"), "got {err}");
    }

    #[test]
    fn a_tool_that_names_a_record_that_does_not_exist_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());

        let err = call_err(
            &vault,
            "update_task",
            serde_json::json!({ "task_id": everyday_core::TaskId::new().to_string(), "status": "done" }),
        );
        assert!(err.contains("not found") || err.contains("task"), "got {err}");

        // And an invented tool name comes back with the real ones, which is
        // what a model needs in order to recover on the next turn.
        let err = call_err(&vault, "add_task", serde_json::json!({ "title": "x" }));
        assert!(err.contains("no tool called"), "got {err}");
        assert!(err.contains("create_task"), "should list the real names: {err}");
    }

    #[test]
    fn the_assistant_can_read_a_markdown_vault_but_is_not_offered_tasks() {
        // The backend decides the catalogue, so a model is never told about
        // a tool whose storage does not exist and cannot claim to have used
        // one.
        let dir = tempfile::tempdir().unwrap();
        let vault = create(
            dir.path(),
            VaultConfig { password: None, backend: "markdown".into(), ..Default::default() },
        )
        .unwrap();

        let offered: Vec<&str> = tools::available(&vault).iter().map(|t| t.name).collect();
        assert!(offered.contains(&"create_entry"), "journals work on every backend: {offered:?}");
        assert!(!offered.contains(&"create_task"), "markdown stores no tasks: {offered:?}");
        assert!(!offered.contains(&"list_shelves"), "nor a library: {offered:?}");

        let err = call_err(&vault, "create_task", serde_json::json!({ "title": "x" }));
        assert!(err.contains("does not store"), "got {err}");
    }

    #[test]
    fn the_catalogue_marks_exactly_the_tools_the_confirmation_gate_must_catch() {
        // The gate reads `Effect`, so this is the list that decides what a
        // person gets asked about. Worth asserting by name rather than by
        // count, so adding a destructive tool is a deliberate edit here.
        let destructive: Vec<&str> = tools::catalog()
            .iter()
            .filter(|t| t.effect == Effect::Destructive)
            .map(|t| t.name)
            .collect();
        assert_eq!(
            destructive,
            vec![
                "delete_entry",
                "delete_project",
                "delete_task",
                "delete_time_block",
                "delete_item",
                "forget",
            ]
        );
    }

    #[test]
    fn remembering_and_forgetting_go_through_the_settings_that_govern_them() {
        let dir = tempfile::tempdir().unwrap();
        let vault = a_vault(dir.path());

        // Off by default in a fresh vault? No -- remembering is on, but the
        // switch has to actually be honoured.
        vault
            .save_agent_settings(&everyday_core::AgentSettings {
                remember: false,
                ..Default::default()
            })
            .unwrap();
        let err = call_err(&vault, "remember", serde_json::json!({ "fact": "Plans on Sundays" }));
        assert!(err.contains("switched off"), "got {err}");

        vault.save_agent_settings(&everyday_core::AgentSettings::default()).unwrap();
        let saved = call(&vault, "remember", serde_json::json!({ "fact": "Plans on Sundays" }));
        let id = saved["id"].as_str().unwrap().to_string();
        assert_eq!(saved["forgotten_to_make_room"], serde_json::json!([]));

        let listed = call(&vault, "list_memories", serde_json::json!({}));
        assert_eq!(listed[0]["fact"], "Plans on Sundays");

        call(&vault, "forget", serde_json::json!({ "memory_id": id }));
        assert!(vault.memories().unwrap().is_empty());
    }

    #[test]
    fn a_read_only_vault_refuses_every_write_and_allows_every_read() {
        // Two processes on one vault: the second opens read-only. The
        // assistant must say so rather than failing somewhere deeper with a
        // message about lock files.
        let dir = tempfile::tempdir().unwrap();
        let first = a_vault(dir.path());
        first.save_journal(&everyday_core::Journal::new("Daily")).unwrap();

        let second = open(dir.path()).unwrap();
        second.unlock(None).unwrap();
        assert!(!second.is_writable(), "the second open should be read-only");

        // Reads still work.
        assert_eq!(call(&second, "list_journals", serde_json::json!({}))[0]["name"], "Daily");

        let err = call_err(&second, "create_task", serde_json::json!({ "title": "x" }));
        assert!(err.contains("read-only"), "got {err}");
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
