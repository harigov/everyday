//! Templates over a real SQLite backend: the one-time move of the meeting
//! settings' own templates into the shared table, and a journal choosing
//! one for a day.

mod support;
use support::vault;

use jiff::civil::Date;
use karmayoga_core::meeting::{MeetingSettings, NoteTemplate};
use karmayoga_core::template::{JournalTemplates, Template, TemplateRule, TemplateUse, When};
use karmayoga_core::{Entry, Journal, TemplateId};

fn starter_id() -> TemplateId {
    karmayoga_core::meeting::template::starter().id
}

#[test]
fn a_new_vault_has_the_meeting_starter_and_it_stays_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let all = vault.templates().unwrap();
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].id, starter_id());
    assert_eq!(all[0].uses, vec![TemplateUse::Meetings]);
    assert!(vault.meeting_settings().unwrap().templates_moved);
    assert_eq!(vault.templates().unwrap().len(), 1, "moved once, not on every read");

    vault.delete_template(starter_id()).unwrap();
    assert!(vault.templates().unwrap().is_empty());

    // A form that knows nothing of the flag saves the settings -- serde's
    // default seeds the starter again and says nothing has been moved. The
    // vault must not take its word for it.
    vault.save_meeting_settings(&MeetingSettings::default()).unwrap();
    assert!(vault.templates().unwrap().is_empty(), "a deleted starter must stay deleted");

    // And a meeting still has something to be written from.
    assert_eq!(vault.meeting_template(None).id, starter_id());
}

#[test]
fn an_older_builds_meeting_templates_move_in_order_and_keep_their_ids() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let standup = NoteTemplate {
        id: TemplateId::new(),
        name: "Standup".into(),
        body: "## Yesterday\n## Today\n".into(),
    };
    let retro =
        NoteTemplate { id: TemplateId::new(), name: "Retro".into(), body: "## Wins\n".into() };
    // What an older build saved: the list inside the settings, no flag. Its
    // own default named the retro.
    let legacy = MeetingSettings {
        legacy_templates: vec![standup.clone(), retro.clone()],
        default_template: Some(retro.id),
        templates_moved: false,
        ..MeetingSettings::default()
    };
    save_raw_settings(&vault, &legacy);

    let all = vault.templates().unwrap();
    let names: Vec<&str> = all.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["Standup", "Retro"], "the settings' order, which decided the fallback");
    assert!(all.iter().all(|t| t.uses == vec![TemplateUse::Meetings]));
    assert_eq!(all[1].id, retro.id);
    assert_eq!(vault.meeting_template(None).id, retro.id, "the default still applies");
    assert_eq!(vault.meeting_template(Some(standup.id)).id, standup.id);
}

/// Write meeting settings the way an older build did: the list inside them,
/// no flag, and nothing in the templates table.
fn save_raw_settings(vault: &karmayoga_core::Vault, settings: &MeetingSettings) {
    vault.put_meeting_settings_as_an_older_build_did(settings).unwrap();
}

#[test]
fn a_journal_starts_each_days_entry_from_its_rules() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());

    let mut daily = Template::new("Daily");
    daily.uses = vec![TemplateUse::Journal];
    daily.title = "{{weekday}}".into();
    daily.body = "## {{date}}\nWhat today is for.\n".into();
    daily.tags = vec!["daily".into()];
    vault.save_template(&daily).unwrap();
    let mut review = Template::new("Weekly review");
    review.uses = vec![TemplateUse::Journal];
    review.title = "Week {{week}} in {{journal}}".into();
    review.body = "## What went well\n".into();
    vault.save_template(&review).unwrap();

    let mut journal = Journal::new("Personal");
    journal.templates = JournalTemplates {
        default: Some(daily.id),
        rules: vec![TemplateRule { when: When::Weekdays { days: vec![7] }, template: review.id }],
    };
    vault.save_journal(&journal).unwrap();

    let mut sunday = Entry::new(journal.id, "UTC");
    sunday.local_date = Date::constant(2026, 11, 15);
    assert!(vault.start_entry_from_template(&mut sunday).unwrap());
    assert_eq!(sunday.title, "Week 46 in Personal");
    assert!(sunday.body.to_markdown().contains("What went well"));

    let mut friday = Entry::new(journal.id, "UTC");
    friday.local_date = Date::constant(2026, 11, 13);
    assert!(vault.start_entry_from_template(&mut friday).unwrap());
    assert_eq!(friday.title, "Friday");
    assert!(friday.body.to_markdown().contains("Friday 13 November 2026"));
    assert_eq!(friday.tags, vec!["daily".to_string()]);

    // Deleting the Sunday template leaves the rule naming nothing: Sunday
    // falls through to the default rather than failing.
    vault.delete_template(review.id).unwrap();
    let mut next = Entry::new(journal.id, "UTC");
    next.local_date = Date::constant(2026, 11, 22);
    assert!(vault.start_entry_from_template(&mut next).unwrap());
    assert_eq!(next.title, "Sunday");

    let plain = Journal::new("Work");
    vault.save_journal(&plain).unwrap();
    let mut blank = Entry::new(plain.id, "UTC");
    assert!(!vault.start_entry_from_template(&mut blank).unwrap());
    assert!(blank.body.is_empty());
}

#[test]
fn a_journal_with_an_impossible_rule_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut journal = Journal::new("Personal");
    journal.templates.rules =
        vec![TemplateRule { when: When::MonthDay { day: 40 }, template: TemplateId::new() }];
    assert_eq!(vault.save_journal(&journal).unwrap_err().code(), "invalid");
}

#[test]
fn a_template_with_no_name_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut t = Template::new("");
    t.body = "x".into();
    assert_eq!(vault.save_template(&t).unwrap_err().code(), "invalid");
}
