//! Out of a vault and back into an empty one, for every app at once.
//!
//! The unit tests in the crate check that one format says what it means. This
//! checks the claim the feature actually makes: **you can leave with your
//! writing and come back with it.** A vault is filled with one of everything,
//! exported, and imported into a vault that has never seen any of it -- and
//! then the two are compared record by record.
//!
//! It is one test rather than nine because the failure it is there to catch
//! is a joint one. A part that exports a task perfectly and drops the id has
//! passed every test it owns and lost the tree; a part that writes a purpose
//! and imports it before the goal exists has lost the attribution. Those only
//! show up end to end.

use everyday_core::library::{Item, ItemStatus, Kind};
use everyday_core::model::{Entry, Journal};
use everyday_core::note::Note;
use everyday_core::purpose::{Goal, Purpose, Role};
use everyday_core::richtext::RichDoc;
use everyday_core::store::calendars::EventQuery;
use everyday_core::store::library::{ItemQuery, LogQuery};
use everyday_core::store::notes::NoteQuery;
use everyday_core::store::purpose::GoalQuery;
use everyday_core::store::tasks::{BlockQuery, TaskQuery};
use everyday_core::store::trackers::ReadingQuery;
use everyday_core::task::{
    BlockKind, BlockSubject, Priority, Project, Task, TaskStatus, TimeBlock,
};
use everyday_core::tracker::{Reading, Tracker, TrackerKind};
use everyday_core::{Vault, VaultConfig};
use everyday_transfer::{Mode, Options, zip};

fn vault(dir: &std::path::Path) -> Vault {
    everyday_vault::create(
        dir,
        VaultConfig {
            name: "Test".into(),
            backend: "sqlite".into(),
            settings: Default::default(),
            password: None,
            kdf: everyday_core::crypto::KdfParams::insecure_fast(),
            auto_lock_seconds: 0,
            forget_key_seconds: 0,
        },
    )
    .unwrap()
}

/// One of everything, with the pointers between them set: an entry filed
/// under a goal, a subtask under a task, an hour booked against that task.
fn fill(vault: &Vault) {
    let role = Role::new("Parent");
    let goal = Goal::new(role.id, "Viya rides without stabilisers");
    let journal = Journal::new("Personal");
    let mut entry = Entry::new(journal.id, "Europe/London");
    entry.title = "Morning: a start".into();
    entry.body = RichDoc::from_markdown("It **rained**.\n\n- one\n- two\n");
    entry.tags = vec!["weather".into(), "walk".into()];
    entry.starred = true;
    entry.purpose = Some(Purpose::Goal { id: goal.id });
    entry.local_date = jiff::civil::date(2026, 9, 10);

    let mut note = Note::new("Recipes worth keeping");
    note.body = RichDoc::from_markdown("## Dal\n\nSoak the lentils.\n");
    note.tags = vec!["food".into()];
    note.pinned = true;

    let mut project = Project::new("Home renovation");
    project.notes = "Rip out the insulation first.".into();
    project.tags = vec!["house".into()];
    project.purpose = Some(Purpose::Role { id: role.id });
    let mut task = Task::new("Book the plasterer").in_project(project.id);
    task.priority = Priority::High;
    task.estimate_minutes = Some(90);
    task.due_date = Some(jiff::civil::date(2026, 9, 12));
    task.tags = vec!["phone".into()];
    task.status = TaskStatus::Blocked;
    let mut subtask = Task::new("Find the quote").in_project(project.id).under(task.id);
    subtask.status = TaskStatus::Done;

    let mut block = TimeBlock::new(
        BlockSubject::Task { id: task.id },
        "2026-09-11T09:00:00Z".parse().unwrap(),
        60,
        "Europe/London",
    );
    block.kind = BlockKind::Actual;
    block.notes = "Longer than it should have been.".into();

    let kind = Kind::new("books", "Books", "Book");
    let mut item = Item::new(kind.id, "The Dispossessed");
    item.creator = "Ursula K. Le Guin".into();
    item.year = Some(1974);
    item.status = ItemStatus::Done;
    item.rating = Some(9);
    item.tags = vec!["sci-fi".into()];
    item.finished_on = Some(jiff::civil::date(2026, 8, 2));

    let tracker = Tracker::new("Weight", TrackerKind::Amount);
    let mut reading = Reading::on(tracker.id, jiff::civil::date(2026, 9, 10), 72.5);
    reading.note = "after a run".into();

    vault.save_journal(&journal).unwrap();
    vault
        .with_store(|store| {
            let purpose = store.purpose().unwrap();
            purpose.put_role(&role)?;
            purpose.put_goal(&goal)?;
            store.put_entry(&entry)?;
            store.notes().unwrap().put_note(&note)?;
            let tasks = store.tasks().unwrap();
            tasks.put_project(&project)?;
            tasks.put_task(&task)?;
            tasks.put_task(&subtask)?;
            tasks.put_block(&block)?;
            let library = store.library().unwrap();
            library.put_kind(&kind)?;
            library.put_item(&item)?;
            let trackers = store.trackers().unwrap();
            trackers.put_tracker(&tracker)?;
            trackers.put_reading(&reading)?;
            Ok(())
        })
        .unwrap();
}

/// Every part that can be read back, so the test is over the whole surface
/// rather than over whichever one is being worked on.
fn importable() -> Vec<String> {
    everyday_transfer::PARTS
        .iter()
        .map(|p| p.spec())
        .filter(|s| s.imports)
        .map(|s| s.id.to_string())
        .collect()
}

#[test]
fn a_vault_comes_back_out_of_its_own_archive() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = vault(source_dir.path());
    fill(&source);

    let opts = Options { parts: Vec::new(), media: true };
    let (bytes, manifest) = everyday_transfer::export(&source, &opts, Vec::new()).unwrap();
    assert!(manifest.records() > 0, "the export is empty");

    // The archive says what it is, and the parts it names are the parts that
    // are there.
    let archive = zip::Reader::open(&bytes).unwrap();
    assert!(archive.contains("everyday.json"));
    assert!(archive.contains("README.md"));
    let inspected = everyday_transfer::inspect(&archive).unwrap();
    let ids: Vec<&str> = inspected.parts.iter().map(|p| p.id.as_str()).collect();
    for expected in ["journal", "notes", "todo", "library", "trackers", "purpose"] {
        assert!(ids.contains(&expected), "{expected} is missing from {ids:?}");
    }

    let target_dir = tempfile::tempdir().unwrap();
    let target = vault(target_dir.path());
    let reports = everyday_transfer::import(&target, &archive, &importable(), Mode::Skip).unwrap();
    let problems: Vec<&String> = reports.iter().flat_map(|r| &r.problems).collect();
    assert!(problems.is_empty(), "{problems:?}");
    assert!(reports.iter().all(|r| r.added > 0), "a part imported nothing: {reports:?}");

    target
        .with_store(|store| {
            let entries = store.all_entries()?;
            assert_eq!(entries.len(), 1);
            let entry = &entries[0];
            assert_eq!(entry.title, "Morning: a start");
            assert_eq!(entry.local_date, jiff::civil::date(2026, 9, 10));
            assert_eq!(entry.tags, ["weather", "walk"]);
            assert!(entry.starred);
            assert!(entry.purpose.is_some(), "the entry lost what it was for");
            assert!(entry.body.plain_text().contains("rained"));
            // The sidecar is what makes this exact rather than approximate:
            // a bold run and a bullet list both survive the trip.
            assert!(
                entry.body.to_markdown().contains("**rained**"),
                "{}",
                entry.body.to_markdown()
            );

            let notes = store.notes().unwrap().list_notes(&NoteQuery::default())?;
            assert_eq!(notes.len(), 1);
            assert!(notes[0].pinned);
            assert_eq!(notes[0].tags, ["food"]);

            let tasks = store.tasks().unwrap();
            assert_eq!(tasks.list_projects()?.len(), 1);
            let all = tasks.list_tasks(&TaskQuery::default())?;
            assert_eq!(all.len(), 2);
            let parent = all.iter().find(|t| t.title == "Book the plasterer").unwrap();
            let child = all.iter().find(|t| t.title == "Find the quote").unwrap();
            assert_eq!(child.parent_id, Some(parent.id), "the subtask lost its parent");
            assert_eq!(parent.priority, Priority::High);
            assert_eq!(parent.estimate_minutes, Some(90));
            assert_eq!(parent.status, TaskStatus::Blocked);
            assert_eq!(child.status, TaskStatus::Done);
            assert!(child.completed_at.is_some(), "a done task with no completion date");

            let blocks = tasks.list_blocks(&BlockQuery::default())?;
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].kind, BlockKind::Actual);
            assert_eq!(blocks[0].subject, BlockSubject::Task { id: parent.id });

            let library = store.library().unwrap();
            let items = library.list_items(&ItemQuery::default())?;
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].creator, "Ursula K. Le Guin");
            assert_eq!(items[0].rating, Some(9));
            assert_eq!(items[0].status, ItemStatus::Done);
            assert_eq!(library.list_logs(&LogQuery::default())?.len(), 0);

            let trackers = store.trackers().unwrap();
            assert_eq!(trackers.list_trackers()?.len(), 1);
            let readings = trackers.list_readings(&ReadingQuery::default())?;
            assert_eq!(readings.len(), 1);
            assert_eq!(readings[0].value, 72.5);
            assert_eq!(readings[0].note, "after a run");

            let purpose = store.purpose().unwrap();
            assert_eq!(purpose.list_roles()?.len(), 1);
            let goals = purpose.list_goals(&GoalQuery::default())?;
            assert_eq!(goals.len(), 1);
            assert_eq!(goals[0].title, "Viya rides without stabilisers");
            // The pointer on the entry has to resolve to the goal that came
            // out of the same archive, or the attribution is decoration.
            assert_eq!(entry.purpose, Some(Purpose::Goal { id: goals[0].id }));

            assert_eq!(store.calendars().unwrap().list_events(&EventQuery::default())?.len(), 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn importing_the_same_archive_twice_changes_nothing_the_second_time() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = vault(source_dir.path());
    fill(&source);
    let (bytes, _) =
        everyday_transfer::export(&source, &Options { parts: Vec::new(), media: true }, Vec::new())
            .unwrap();
    let archive = zip::Reader::open(&bytes).unwrap();

    let target_dir = tempfile::tempdir().unwrap();
    let target = vault(target_dir.path());
    everyday_transfer::import(&target, &archive, &importable(), Mode::Skip).unwrap();
    let before = target.with_store(|s| Ok(s.all_entries()?.len())).unwrap();

    let again = everyday_transfer::import(&target, &archive, &importable(), Mode::Skip).unwrap();
    assert!(again.iter().all(|r| r.added == 0), "the second import added records: {again:?}");
    assert!(again.iter().all(|r| r.replaced == 0));
    assert_eq!(target.with_store(|s| Ok(s.all_entries()?.len())).unwrap(), before);
}

#[test]
fn an_edited_file_lands_on_the_record_it_came_from() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    fill(&vault);
    let (bytes, _) =
        everyday_transfer::export(&vault, &Options { parts: Vec::new(), media: false }, Vec::new())
            .unwrap();

    // What somebody does with an export: unzip it, open a note in an editor,
    // change a word, and put it back. The sidecar still holds the old body,
    // and must lose to the file that was edited.
    let archive = zip::Reader::open(&bytes).unwrap();
    let mut files: std::collections::BTreeMap<String, Vec<u8>> = archive
        .names()
        .map(|name| (name.to_string(), archive.get(name).unwrap().to_vec()))
        .collect();
    let note = files
        .iter()
        .find(|(name, _)| name.starts_with("notes/") && name.ends_with(".md"))
        .map(|(name, body)| (name.clone(), String::from_utf8(body.clone()).unwrap()))
        .expect("no note in the archive");
    files.insert(note.0, note.1.replace("Soak the lentils.", "Soak the lentils overnight.").into());

    let edited = zip::Reader::from_files(files);
    everyday_transfer::import(&vault, &edited, &importable(), Mode::Replace).unwrap();

    vault
        .with_store(|store| {
            let notes = store.notes().unwrap().all_notes()?;
            assert_eq!(notes.len(), 1, "the edit made a second note instead of changing one");
            assert!(
                notes[0].body.plain_text().contains("overnight"),
                "the edit was overridden: {}",
                notes[0].body.plain_text()
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_folder_of_plain_markdown_imports_without_a_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    vault.save_journal(&Journal::new("Journal")).unwrap();

    // No `everyday.json`, no front matter, no ids: a folder somebody made in
    // a text editor. This is the path that has to work, because it is the
    // only one another program can produce.
    let files = [
        ("notes/Shopping.md", "Milk, bread, and the good coffee.\n"),
        ("journal/2026-01-02-a-day.md", "# A day\n\nIt was quiet.\n"),
    ]
    .into_iter()
    .map(|(name, body)| (name.to_string(), body.as_bytes().to_vec()))
    .collect();

    let archive = zip::Reader::from_files(files);
    let manifest = everyday_transfer::inspect(&archive).unwrap();
    assert_eq!(manifest.parts.len(), 2);

    let reports = everyday_transfer::import(&vault, &archive, &importable(), Mode::Skip).unwrap();
    assert!(reports.iter().flat_map(|r| &r.problems).next().is_none(), "{reports:?}");

    vault
        .with_store(|store| {
            let notes = store.notes().unwrap().all_notes()?;
            assert_eq!(notes.len(), 1);
            assert_eq!(notes[0].title, "Shopping", "the filename should have been the title");
            assert!(notes[0].body.plain_text().contains("good coffee"));

            let entries = store.all_entries()?;
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].title, "A day");
            assert_eq!(
                entries[0].local_date,
                jiff::civil::date(2026, 1, 2),
                "the date in the file name should have been used"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn choosing_one_part_exports_only_that_part() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    fill(&vault);

    let opts = Options { parts: vec!["notes".into()], media: false };
    let (bytes, manifest) = everyday_transfer::export(&vault, &opts, Vec::new()).unwrap();
    assert_eq!(manifest.parts.len(), 1);
    assert_eq!(manifest.parts[0].id, "notes");

    let archive = zip::Reader::open(&bytes).unwrap();
    assert!(
        archive.names().all(|n| n.starts_with("notes/") || !n.contains('/')),
        "something outside notes was written: {:?}",
        archive.names().collect::<Vec<_>>()
    );
}

#[test]
fn an_export_without_attachments_carries_no_media() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    fill(&vault);
    let blob = vault.put_blob(b"pretend this is a photograph").unwrap();
    vault
        .with_store(|store| {
            let mut entry = store.all_entries()?.remove(0);
            entry.attachments.push(everyday_core::model::Attachment {
                blob,
                kind: everyday_core::model::MediaKind::Image,
                mime: "image/jpeg".into(),
                filename: "IMG_0042.jpg".into(),
                byte_len: 28,
                width: None,
                height: None,
                duration_ms: None,
                caption: "the harbour".into(),
            });
            store.put_entry(&entry)
        })
        .unwrap();

    let with = everyday_transfer::export(
        &vault,
        &Options { parts: vec!["journal".into()], media: true },
        Vec::new(),
    )
    .unwrap()
    .0;
    let without = everyday_transfer::export(
        &vault,
        &Options { parts: vec!["journal".into()], media: false },
        Vec::new(),
    )
    .unwrap()
    .0;

    let with = zip::Reader::open(&with).unwrap();
    let without = zip::Reader::open(&without).unwrap();
    assert!(with.names().any(|n| n.contains("media/")), "the photograph was not exported");
    assert!(
        !without.names().any(|n| n.contains("media/")),
        "media was exported when it was not asked for"
    );
}

#[test]
fn a_read_only_vault_refuses_an_import_rather_than_half_doing_one() {
    let dir = tempfile::tempdir().unwrap();
    let first = vault(dir.path());
    fill(&first);
    let (bytes, _) =
        everyday_transfer::export(&first, &Options { parts: Vec::new(), media: false }, Vec::new())
            .unwrap();
    let archive = zip::Reader::open(&bytes).unwrap();

    // A second handle on the same directory gets it read-only: the first
    // still holds the writer's lock.
    let second = everyday_vault::open(dir.path()).unwrap();
    assert!(!second.is_writable());
    let e = everyday_transfer::import(&second, &archive, &importable(), Mode::Skip).unwrap_err();
    assert_eq!(e.code(), "unsupported");
}

#[test]
fn an_attachment_is_named_for_what_it_actually_is() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    fill(&vault);

    // A PNG whose record does not know its own name -- a cover fetched from
    // the web, which has a URL and nothing else. The bytes say what it is.
    let png = [b"\x89PNG\r\n\x1a\n".as_slice(), &[0u8; 64]].concat();
    let blob = vault.put_blob(&png).unwrap();
    vault
        .with_store(|store| {
            let library = store.library().unwrap();
            let mut item = library.list_items(&ItemQuery::default())?.remove(0);
            item.cover = Some(blob);
            library.put_item(&item)
        })
        .unwrap();

    let (bytes, _) = everyday_transfer::export(
        &vault,
        &Options { parts: vec!["library".into()], media: true },
        Vec::new(),
    )
    .unwrap();
    let archive = zip::Reader::open(&bytes).unwrap();
    let cover = archive
        .names()
        .find(|n| n.contains("media/"))
        .unwrap_or_else(|| panic!("no cover in {:?}", archive.names().collect::<Vec<_>>()));
    assert!(cover.ends_with(".png"), "{cover}");
    assert_eq!(archive.get(cover).unwrap(), png.as_slice());
}

#[test]
fn one_photograph_in_two_entries_is_one_file() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let journal = everyday_core::Journal::new("Personal");
    vault.save_journal(&journal).unwrap();
    let blob = vault.put_blob(&[b"\x89PNG\r\n\x1a\n".as_slice(), &[7u8; 200]].concat()).unwrap();

    for day in [1, 2] {
        let mut entry = Entry::new(journal.id, "UTC");
        entry.local_date = jiff::civil::date(2026, 3, day);
        entry.title = format!("Day {day}");
        entry.attachments.push(everyday_core::model::Attachment {
            blob,
            kind: everyday_core::model::MediaKind::Image,
            mime: "image/png".into(),
            filename: "shared.png".into(),
            byte_len: 208,
            width: None,
            height: None,
            duration_ms: None,
            caption: String::new(),
        });
        vault.save_entry(&entry, None).unwrap();
    }

    let (bytes, _) = everyday_transfer::export(
        &vault,
        &Options { parts: vec!["journal".into()], media: true },
        Vec::new(),
    )
    .unwrap();
    let archive = zip::Reader::open(&bytes).unwrap();
    let media: Vec<&str> = archive.names().filter(|n| n.contains("media/")).collect();
    assert_eq!(media.len(), 1, "the same picture was written twice: {media:?}");

    // Both entries still point at it, from a folder down.
    let entries: Vec<&str> = archive
        .names()
        .filter(|n| n.starts_with("journal/") && n.ends_with(".md") && !n.contains("_journal"))
        .collect();
    assert_eq!(entries.len(), 2);
    for name in entries {
        let text = std::str::from_utf8(archive.get(name).unwrap()).unwrap();
        assert!(text.contains("../media/"), "{name} links nowhere: {text}");
    }
}

#[test]
fn a_title_that_trails_off_does_not_cost_the_whole_archive() {
    let dir = tempfile::tempdir().unwrap();
    let source = vault(dir.path());
    let journal = Journal::new("Personal");
    source.save_journal(&journal).unwrap();
    let mut entry = Entry::new(journal.id, "UTC");
    entry.title = "Wait... what happened".into();
    entry.body = RichDoc::from_markdown("Nothing, in the end.\n");
    source.save_entry(&entry, None).unwrap();

    let (bytes, _) = everyday_transfer::export(
        &source,
        &Options { parts: vec!["journal".into()], media: false },
        Vec::new(),
    )
    .unwrap();

    // `..` is the one sequence an archive path may not hold, so a name that
    // kept it made the whole file unreadable rather than that one entry.
    let archive = zip::Reader::open(&bytes).expect("the archive would not reopen");
    assert!(
        archive.names().all(|n| !n.contains("..")),
        "{:?}",
        archive.names().collect::<Vec<_>>()
    );

    let target_dir = tempfile::tempdir().unwrap();
    let target = vault(target_dir.path());
    let reports = everyday_transfer::import(&target, &archive, &importable(), Mode::Skip).unwrap();
    assert!(reports.iter().flat_map(|r| &r.problems).next().is_none(), "{reports:?}");
    target
        .with_store(|store| {
            let entries = store.all_entries()?;
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].title, "Wait... what happened");
            Ok(())
        })
        .unwrap();
}

#[test]
fn two_shelves_that_reduce_to_one_name_are_still_two_files() {
    let dir = tempfile::tempdir().unwrap();
    let source = vault(dir.path());
    let a = Kind::new("books", "Books", "Book");
    let b = Kind::new("books-2", "Books!", "Book");
    let one = Item::new(a.id, "The Dispossessed");
    let two = Item::new(b.id, "Piranesi");
    source
        .with_store(|store| {
            let library = store.library().unwrap();
            library.put_kind(&a)?;
            library.put_kind(&b)?;
            library.put_item(&one)?;
            library.put_item(&two)
        })
        .unwrap();

    let (bytes, _) = everyday_transfer::export(
        &source,
        &Options { parts: vec!["library".into()], media: false },
        Vec::new(),
    )
    .unwrap();
    let archive = zip::Reader::open(&bytes).unwrap();

    let target_dir = tempfile::tempdir().unwrap();
    let target = vault(target_dir.path());
    everyday_transfer::import(&target, &archive, &importable(), Mode::Skip).unwrap();
    target
        .with_store(|store| {
            let library = store.library().unwrap();
            let titles: Vec<String> =
                library.list_items(&ItemQuery::default())?.into_iter().map(|i| i.title).collect();
            assert_eq!(titles.len(), 2, "a shelf overwrote another: {titles:?}");
            assert_eq!(library.list_kinds()?.len(), 2);
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_role_and_a_goal_keep_what_was_written_about_them() {
    let dir = tempfile::tempdir().unwrap();
    let source = vault(dir.path());
    let mut role = Role::new("Parent");
    role.notes = "Present, not merely around.\nThe hard one.".into();
    let mut goal = Goal::new(role.id, "Viya rides without stabilisers");
    goal.notes = "Saturdays, in the park behind the school.".into();
    source
        .with_store(|store| {
            let purpose = store.purpose().unwrap();
            purpose.put_role(&role)?;
            purpose.put_goal(&goal)
        })
        .unwrap();

    let (bytes, _) = everyday_transfer::export(
        &source,
        &Options { parts: vec!["purpose".into()], media: false },
        Vec::new(),
    )
    .unwrap();
    let archive = zip::Reader::open(&bytes).unwrap();

    let target_dir = tempfile::tempdir().unwrap();
    let target = vault(target_dir.path());
    everyday_transfer::import(&target, &archive, &importable(), Mode::Skip).unwrap();
    target
        .with_store(|store| {
            let purpose = store.purpose().unwrap();
            let roles = purpose.list_roles()?;
            assert_eq!(roles.len(), 1);
            assert_eq!(roles[0].notes, role.notes, "the role's own words were dropped");
            let goals = purpose.list_goals(&GoalQuery::default())?;
            assert_eq!(goals.len(), 1);
            assert_eq!(goals[0].notes, goal.notes, "the goal's own words were dropped");
            Ok(())
        })
        .unwrap();
}

#[test]
fn an_entry_whose_journal_is_gone_is_filed_somewhere_that_exists() {
    let dir = tempfile::tempdir().unwrap();
    let source = vault(dir.path());
    let journal = Journal::new("Personal");
    source.save_journal(&journal).unwrap();
    let mut entry = Entry::new(journal.id, "UTC");
    entry.title = "Orphan".into();
    source.save_entry(&entry, None).unwrap();
    // The journal goes; the entry stays, which is what `Unfiled/` is for.
    source
        .with_store(|store| {
            store.delete_journal(journal.id).ok();
            store.put_entry(&entry)
        })
        .unwrap();

    let (bytes, _) = everyday_transfer::export(
        &source,
        &Options { parts: vec!["journal".into()], media: false },
        Vec::new(),
    )
    .unwrap();
    let archive = zip::Reader::open(&bytes).unwrap();

    let target_dir = tempfile::tempdir().unwrap();
    let target = vault(target_dir.path());
    everyday_transfer::import(&target, &archive, &importable(), Mode::Skip).unwrap();
    target
        .with_store(|store| {
            let entries = store.all_entries()?;
            let orphan = entries.iter().find(|e| e.title == "Orphan").expect("the entry was lost");
            // Taking the front matter's `journalId` on trust would file it
            // under a journal that is not here: invisible in every list.
            assert!(
                store.get_journal(orphan.journal_id).is_ok(),
                "the entry points at a journal that does not exist"
            );
            Ok(())
        })
        .unwrap();
}
