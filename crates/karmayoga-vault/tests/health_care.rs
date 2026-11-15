//! The rules the vault applies to the care half of Health, over a real
//! SQLite backend: a dose change remembered, a supply that runs down, due
//! doses and adherence, a source's dose events matched and not filed twice,
//! a template that makes its symptom trackers, a condition's targets, a
//! document's results accepted, foods that know when they were last eaten,
//! and what deleting a condition leaves behind.

mod support;
use support::vault;

use jiff::civil::{Date, Time};
use jiff::{Span, Timestamp};
use karmayoga_core::health::templates;
use karmayoga_core::health::{
    Adherence, Allergy, AllergyKind, CarePlan, CareTarget, Code, Condition, ConditionStatus,
    Connector, DocFile, DocKind, Dose, DoseChange, DoseOrigin, DoseStatus, ExtractionState, Food,
    FoodSource, HealthBatch, HealthSource, LabFlag, LabOrigin, LabResult, LabSchedule, Meal,
    MealItem, MealSlot, Measure, MedKind, MedSchedule, MedStatus, Medication, Metric, Nutrients,
    RawDose, Severity, SourceStatus, Stat, Supply, TemplateChoice, TemplateKey,
};
use karmayoga_core::{
    AllergyId, BlobId, ConditionId, DoseId, FoodId, HealthSourceId, LabResultId, MealId,
    MedicationId, TrackerKind, Vault,
};

// ---- things to build -------------------------------------------------------

fn today() -> Date {
    jiff::Zoned::now().date()
}

fn days_ago(days: i64) -> Date {
    today().checked_sub(Span::new().days(days)).unwrap()
}

/// `hour:minute` UTC on `date`.
fn at(date: Date, hour: i8, minute: i8) -> Timestamp {
    format!("{date}T{hour:02}:{minute:02}:00Z").parse().unwrap()
}

fn medication(name: &str) -> Medication {
    let now = Timestamp::now();
    Medication {
        id: MedicationId::new(),
        name: name.into(),
        kind: MedKind::Prescription,
        form: String::new(),
        strength: String::new(),
        dose: 1.0,
        dose_unit: String::new(),
        schedule: MedSchedule::Daily { times: vec![Time::constant(8, 0, 0, 0)] },
        rules: Vec::new(),
        tags: Vec::new(),
        status: MedStatus::Active,
        started: None,
        stopped: None,
        stop_reason: String::new(),
        history: Vec::new(),
        condition_ids: Vec::new(),
        prescriber: String::new(),
        pharmacy: String::new(),
        supply: None,
        remind: true,
        codes: Vec::new(),
        source_names: Vec::new(),
        notes: String::new(),
        sort_order: 0,
        created_at: now,
        updated_at: now,
    }
}

fn dose(medication: &Medication, at: Timestamp, status: DoseStatus, amount: Option<f64>) -> Dose {
    let now = Timestamp::now();
    Dose {
        id: DoseId::new(),
        medication_id: medication.id,
        scheduled_at: None,
        at,
        status,
        amount,
        origin: DoseOrigin::Manual,
        note: String::new(),
        key: String::new(),
        hidden: false,
        created_at: now,
        updated_at: now,
    }
}

fn raw_dose(name: &str, codes: Vec<Code>, at: Timestamp) -> RawDose {
    RawDose {
        name: name.into(),
        codes,
        at,
        scheduled_at: None,
        status: DoseStatus::Taken,
        amount: None,
        unit: String::new(),
        source: "Health".into(),
    }
}

fn code(system: &str, value: &str) -> Code {
    Code { system: system.into(), code: value.into(), display: String::new() }
}

fn lab(code: &str, name: &str, value: f64, collected: Date, unit: &str) -> LabResult {
    let now = Timestamp::now();
    LabResult {
        id: LabResultId::new(),
        code: code.into(),
        name: name.into(),
        value: Some(value),
        text: String::new(),
        unit: unit.into(),
        ref_low: None,
        ref_high: None,
        ref_text: String::new(),
        flag: LabFlag::None,
        collected,
        collected_at: None,
        lab: String::new(),
        panel: String::new(),
        document_id: None,
        origin: LabOrigin::Manual,
        note: String::new(),
        created_at: now,
        updated_at: now,
    }
}

fn condition(name: &str, plan: CarePlan) -> Condition {
    let now = Timestamp::now();
    Condition {
        id: ConditionId::new(),
        name: name.into(),
        template: None,
        status: ConditionStatus::Active,
        diagnosed: None,
        codes: Vec::new(),
        plan,
        notes: String::new(),
        created_at: now,
        updated_at: now,
    }
}

fn food(name: &str) -> Food {
    let now = Timestamp::now();
    Food {
        id: FoodId::new(),
        name: name.into(),
        brand: String::new(),
        barcode: String::new(),
        source: FoodSource::Own,
        per_100g: Nutrients { kcal: 52.0, carbs_g: 14.0, ..Default::default() },
        servings: Vec::new(),
        favorite: false,
        last_used: None,
        use_count: 0,
        created_at: now,
        updated_at: now,
    }
}

fn eaten(food: &Food, kcal: f64) -> MealItem {
    MealItem {
        food_id: Some(food.id),
        name: food.name.clone(),
        amount: 100.0,
        unit: "g".into(),
        grams: Some(100.0),
        nutrients: Nutrients { kcal, ..Default::default() },
        estimated: false,
    }
}

fn meal(at: Timestamp, items: Vec<MealItem>) -> Meal {
    let now = Timestamp::now();
    Meal {
        id: MealId::new(),
        at,
        offset_min: 0,
        slot: MealSlot::Lunch,
        name: String::new(),
        items,
        photo: None,
        note: String::new(),
        created_at: now,
        updated_at: now,
    }
}

fn a_source(vault: &Vault) -> HealthSource {
    let now = Timestamp::now();
    vault
        .save_health_source(&HealthSource {
            id: HealthSourceId::new(),
            connector: Connector::AppleExport,
            name: "Apple Health".into(),
            paused: false,
            status: SourceStatus::default(),
            token_id: String::new(),
            created_at: now,
            updated_at: now,
        })
        .unwrap()
}

fn choosing(symptoms: &[&str]) -> TemplateChoice {
    TemplateChoice {
        plan: None,
        symptoms: symptoms.iter().map(|s| s.to_string()).collect(),
        diagnosed: None,
    }
}

/// A file uploaded the way the interface does it: bytes into the blob store,
/// then a `DocFile` naming them.
fn uploaded(vault: &Vault, bytes: &[u8], name: &str) -> DocFile {
    let blob = vault.put_blob(bytes).unwrap();
    DocFile { blob, name: name.into(), mime: "application/pdf".into(), size: 0 }
}

fn remaining(vault: &Vault, medication: &Medication) -> f64 {
    vault.get_medication(medication.id).unwrap().supply.unwrap().remaining
}

// ---- medications -----------------------------------------------------------

#[test]
fn a_change_of_dose_is_written_into_the_history_dated_today() {
    // What a lab recheck is counted from. Nobody types "I changed it
    // today" into a form that is already asking about the new dose, so the
    // vault does.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut thyroid = medication("Levothyroxine");
    thyroid.dose = 50.0;
    thyroid.strength = "50 mcg".into();
    let first = vault.save_medication(&thyroid).unwrap();
    assert!(first.history.is_empty(), "a first prescription is not a change");

    let before = today();
    let mut raised = first.clone();
    raised.dose = 75.0;
    raised.strength = "75 mcg".into();
    let second = vault.save_medication(&raised).unwrap();
    let after = today();
    assert_eq!(second.history.len(), 1, "{:?}", second.history);
    let change = &second.history[0];
    assert!(change.on == before || change.on == after, "dated {}", change.on);
    assert_eq!((change.dose, change.strength.as_str()), (75.0, "75 mcg"));

    // Changing something else is not a change of dose.
    let mut noted = second.clone();
    noted.notes = "with water".into();
    assert_eq!(vault.save_medication(&noted).unwrap().history.len(), 1);

    // A second change on the same day is not written beside the first.
    let mut again = vault.get_medication(thyroid.id).unwrap();
    again.dose = 88.0;
    assert_eq!(vault.save_medication(&again).unwrap().history.len(), 1);
}

#[test]
fn a_change_the_caller_dated_is_not_written_a_second_time() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut thyroid = medication("Levothyroxine");
    thyroid.dose = 50.0;
    vault.save_medication(&thyroid).unwrap();

    let mut raised = vault.get_medication(thyroid.id).unwrap();
    raised.dose = 75.0;
    raised.history.push(DoseChange {
        on: today(),
        dose: 75.0,
        strength: String::new(),
        note: "after the October labs".into(),
    });
    let saved = vault.save_medication(&raised).unwrap();
    assert_eq!(saved.history.len(), 1);
    assert_eq!(saved.history[0].note, "after the October labs");
}

#[test]
fn a_taken_dose_comes_off_the_supply_and_never_below_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut pills = medication("Metformin");
    pills.dose = 2.0;
    pills.supply = Some(Supply { remaining: 30.0, refill_days: 7, last_refill: None });
    vault.save_medication(&pills).unwrap();
    let day = days_ago(1);

    // No amount: the medication's own dose.
    let taken = vault.log_dose(&dose(&pills, at(day, 8, 0), DoseStatus::Taken, None)).unwrap();
    assert_eq!(remaining(&vault, &pills), 28.0);
    assert!(taken.key.is_empty(), "a typed-in dose has no source key");

    vault.log_dose(&dose(&pills, at(day, 9, 0), DoseStatus::Taken, Some(5.0))).unwrap();
    assert_eq!(remaining(&vault, &pills), 23.0);

    // Skipped took nothing, and an amount it came with is not believed.
    let skipped =
        vault.log_dose(&dose(&pills, at(day, 10, 0), DoseStatus::Skipped, Some(9.0))).unwrap();
    assert_eq!(skipped.amount, None);
    assert_eq!(remaining(&vault, &pills), 23.0);

    // Editing a dose already logged hands over no more pills.
    let mut edited = taken.clone();
    edited.note = "with lunch".into();
    vault.log_dose(&edited).unwrap();
    assert_eq!(remaining(&vault, &pills), 23.0);

    vault.log_dose(&dose(&pills, at(day, 11, 0), DoseStatus::Taken, Some(100.0))).unwrap();
    assert_eq!(remaining(&vault, &pills), 0.0);

    let all = vault.list_doses(at(days_ago(2), 0, 0), at(today(), 23, 59), Some(pills.id)).unwrap();
    assert_eq!(all.len(), 4, "the edit was the first dose again, not a fifth");
    assert_eq!(all.iter().find(|d| d.id == taken.id).unwrap().note, "with lunch");
}

#[test]
fn a_dose_of_a_medication_that_is_not_there_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let ghost = medication("Never saved");
    let err =
        vault.log_dose(&dose(&ghost, at(days_ago(1), 8, 0), DoseStatus::Taken, None)).unwrap_err();
    assert_eq!(err.code(), "invalid", "{err}");
}

#[test]
fn deleting_a_medication_takes_its_doses() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let pills = medication("Aspirin");
    vault.save_medication(&pills).unwrap();
    vault.log_dose(&dose(&pills, at(days_ago(1), 8, 0), DoseStatus::Taken, None)).unwrap();

    vault.delete_medication(pills.id).unwrap();
    assert!(vault.list_medications().unwrap().is_empty());
    let left = vault.list_doses(at(days_ago(2), 0, 0), at(today(), 23, 59), None).unwrap();
    assert!(left.is_empty(), "{left:?}");
}

// ---- due doses and adherence -----------------------------------------------

#[test]
fn due_doses_are_answered_by_what_was_logged_and_adherence_counts_the_rest() {
    // Three days back, so every due time is long past its window whatever
    // hour the test runs at.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let date = days_ago(3);
    let mut thyroid = medication("Levothyroxine");
    thyroid.schedule =
        MedSchedule::Daily { times: vec![Time::constant(8, 0, 0, 0), Time::constant(20, 0, 0, 0)] };
    vault.save_medication(&thyroid).unwrap();
    vault.log_dose(&dose(&thyroid, at(date, 8, 5), DoseStatus::Taken, None)).unwrap();

    let due = vault.doses_due(date, "UTC").unwrap();
    assert_eq!(due.len(), 2, "{due:?}");
    assert_eq!(due[0].due_at, at(date, 8, 0));
    assert!(due[0].dose.is_some());
    assert_eq!(due[1].due_at, at(date, 20, 0));
    assert!(due[1].dose.is_none() && due[1].overdue);

    let expected = Adherence { due: 2, taken: 1, skipped: 0, missed: 1, on_time: 1 };
    assert_eq!(vault.medication_adherence(Some(thyroid.id), date, date, "UTC").unwrap(), expected);
    assert_eq!(vault.medication_adherence(None, date, date, "UTC").unwrap(), expected);

    let unknown = vault.medication_adherence(Some(MedicationId::new()), date, date, "UTC");
    assert_eq!(unknown.unwrap_err().code(), "not_found");
    assert_eq!(vault.doses_due(date, "Mars/Olympus").unwrap_err().code(), "invalid");
    let backwards = vault.medication_adherence(None, date, days_ago(5), "UTC");
    assert_eq!(backwards.unwrap_err().code(), "invalid");
}

// ---- a source's dose events ------------------------------------------------

#[test]
fn a_sources_dose_events_are_matched_by_code_then_name_and_filed_once() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let mut thyroid = medication("Levothyroxine");
    thyroid.codes = vec![code("rxnorm", "10582")];
    let mut metformin = medication("Metformin");
    metformin.strength = "500 mg".into();
    vault.save_medication(&thyroid).unwrap();
    vault.save_medication(&metformin).unwrap();
    let source = a_source(&vault);
    let day = Date::constant(2026, 11, 10);

    let events = vec![
        // By code, under a brand the list does not know.
        raw_dose("Synthroid", vec![code("RxNorm", "10582")], at(day, 8, 0)),
        // By the name that one just taught, with no code.
        raw_dose("SYNTHROID", vec![], at(day, 20, 0)),
        // By name and strength.
        raw_dose("metformin 500 mg", vec![], at(day, 9, 0)),
        // By nothing.
        raw_dose("Mystery pill", vec![], at(day, 10, 0)),
    ];
    let batch = || HealthBatch { doses: events.clone(), ..Default::default() };
    let report = vault.ingest_health(source.id, batch(), None).unwrap();
    assert_eq!((report.doses_added, report.doses_unmatched), (3, 1), "{report:?}");

    let from = at(day, 0, 0);
    let to = at(day.tomorrow().unwrap(), 0, 0);
    let filed = vault.list_doses(from, to, None).unwrap();
    assert_eq!(filed.len(), 3);
    assert!(filed.iter().all(|d| d.origin == DoseOrigin::Source));
    assert!(filed.iter().all(|d| {
        d.key.len() == 32 && d.key.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
    }));
    let of = |m: &Medication| filed.iter().filter(|d| d.medication_id == m.id).count();
    assert_eq!((of(&thyroid), of(&metformin)), (2, 1));

    // The brand is remembered once; a medication's own name is not.
    let names = vault.get_medication(thyroid.id).unwrap().source_names;
    assert_eq!(names, ["Synthroid"]);
    assert!(vault.get_medication(metformin.id).unwrap().source_names.is_empty());

    // The same push again files nothing, and the stranger is counted again.
    let again = vault.ingest_health(source.id, batch(), None).unwrap();
    assert_eq!((again.doses_added, again.doses_unmatched), (0, 1), "{again:?}");
    assert_eq!(vault.list_doses(from, to, None).unwrap().len(), 3);

    // Skipped at the same minute is another event, not a repeat of that one.
    let mut skipped = raw_dose("Synthroid", vec![], at(day, 8, 0));
    skipped.status = DoseStatus::Skipped;
    let batch = HealthBatch { doses: vec![skipped], ..Default::default() };
    assert_eq!(vault.ingest_health(source.id, batch, None).unwrap().doses_added, 1);
    assert_eq!(vault.list_doses(from, to, None).unwrap().len(), 4);
}

// ---- conditions ------------------------------------------------------------

#[test]
fn a_template_makes_its_symptom_trackers_and_a_plan_the_person_owns() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let template = templates::get(TemplateKey::Hypothyroidism);
    let choice = TemplateChoice {
        diagnosed: Some(Date::constant(2025, 3, 1)),
        ..choosing(&["Fatigue", " feeling cold ", "FATIGUE"])
    };

    let made = vault.apply_condition_template(TemplateKey::Hypothyroidism, &choice).unwrap();
    assert_eq!(made.template, Some(TemplateKey::Hypothyroidism));
    assert_eq!(made.name, template.name);
    assert_eq!(made.status, ConditionStatus::Active);
    assert_eq!(made.diagnosed, Some(Date::constant(2025, 3, 1)));
    assert_eq!(made.codes, template.codes);
    assert_eq!(made.plan.targets, template.plan.targets);
    assert_eq!(made.plan.labs, template.plan.labs);
    assert_eq!(made.plan.template_version, template.version);

    // "FATIGUE" is the symptom "Fatigue" again.
    assert_eq!(made.plan.tracker_ids.len(), 2);
    let names: Vec<String> =
        made.plan.tracker_ids.iter().map(|id| vault.tracker(*id).unwrap().name).collect();
    assert_eq!(names, ["Fatigue", "feeling cold"]);
    for id in &made.plan.tracker_ids {
        let tracker = vault.tracker(*id).unwrap();
        assert_eq!(tracker.kind, TrackerKind::Scale);
        assert_eq!(tracker.scale_max, 10.0);
    }
    assert_eq!(vault.get_condition(made.id).unwrap().plan.tracker_ids, made.plan.tracker_ids);
}

#[test]
fn a_plan_edited_on_the_checklist_is_kept_and_carries_no_stranger_trackers() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let target = CareTarget {
        label: "A1c below 7".into(),
        measure: Measure::Lab { code: "4548-4".into() },
        low: None,
        high: Some(7.0),
        use_reference_range: false,
        window_days: 0,
        note: String::new(),
    };
    let foreign = karmayoga_core::TrackerId::new();
    let plan = CarePlan { targets: vec![target], tracker_ids: vec![foreign], ..Default::default() };
    let choice = TemplateChoice { plan: Some(plan), ..choosing(&[]) };

    let made = vault.apply_condition_template(TemplateKey::Type2Diabetes, &choice).unwrap();
    assert_eq!(made.plan.targets.len(), 1);
    assert_eq!(made.plan.targets[0].label, "A1c below 7");
    assert!(made.plan.tracker_ids.is_empty(), "an id the checklist carried must not stay");
}

#[test]
fn a_condition_report_says_where_a_lab_target_stands_and_what_is_due() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let a1c = CareTarget {
        label: "A1c below 7".into(),
        measure: Measure::Lab { code: "4548-4".into() },
        low: None,
        high: Some(7.0),
        use_reference_range: false,
        window_days: 0,
        note: String::new(),
    };
    // Nothing has been measured for this one, so there is nothing to judge.
    let steps = CareTarget {
        label: "8,000 steps".into(),
        measure: Measure::Metric { metric: Metric::Steps, stat: Stat::Sum },
        low: Some(8000.0),
        high: None,
        use_reference_range: false,
        window_days: 7,
        note: String::new(),
    };
    let recheck = LabSchedule {
        code: "4548-4".into(),
        name: "A1c".into(),
        every_days: 90,
        after_change_days: None,
        after_change_tag: String::new(),
    };
    let plan = CarePlan { targets: vec![a1c, steps], labs: vec![recheck], ..Default::default() };
    let diabetes = vault.save_condition(&condition("Type 2 diabetes", plan)).unwrap();
    let mut mine = medication("Metformin");
    mine.condition_ids = vec![diabetes.id];
    vault.save_medication(&mine).unwrap();
    vault.save_medication(&medication("Vitamin D")).unwrap();
    vault.save_lab(&lab("4548-4", "Hemoglobin A1c", 6.4, days_ago(30), "%")).unwrap();

    let report = vault.condition_report(diabetes.id, today(), "UTC").unwrap();
    assert_eq!(report.targets.len(), 2);
    assert_eq!((report.targets[0].state.as_str(), report.targets[0].value), ("met", Some(6.4)));
    assert_eq!((report.targets[1].state.as_str(), report.targets[1].value), ("unknown", None));
    assert_eq!(report.medications.len(), 1);
    assert_eq!(report.medications[0].id, mine.id);
    assert_eq!(report.labs_due.len(), 1);
    assert_eq!(report.labs_due[0].last, Some(days_ago(30)));
    assert!(!report.labs_due[0].overdue, "ninety days from thirty days ago is not yet");

    // A newer, higher result is the one that counts.
    vault.save_lab(&lab("4548-4", "Hemoglobin A1c", 7.8, days_ago(1), "%")).unwrap();
    let report = vault.condition_report(diabetes.id, today(), "UTC").unwrap();
    assert_eq!((report.targets[0].state.as_str(), report.targets[0].value), ("notMet", Some(7.8)));

    let missing = vault.condition_report(ConditionId::new(), today(), "UTC");
    assert_eq!(missing.unwrap_err().code(), "not_found");
}

#[test]
fn deleting_a_condition_strips_it_from_what_named_it_and_removes_its_trackers_on_request() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let doomed = vault
        .apply_condition_template(TemplateKey::Hypothyroidism, &choosing(&["Fatigue", "Brain fog"]))
        .unwrap();
    let kept =
        vault.apply_condition_template(TemplateKey::Type2Diabetes, &choosing(&["Thirst"])).unwrap();
    assert_eq!(doomed.plan.tracker_ids.len(), 2);

    let mut pills = medication("Levothyroxine");
    pills.condition_ids = vec![doomed.id, kept.id];
    vault.save_medication(&pills).unwrap();
    let file = uploaded(&vault, b"a visit note", "visit.pdf");
    let note = vault.add_health_document("Visit", DocKind::VisitNote, None, vec![file]).unwrap();
    let mut tagged = note.clone();
    tagged.condition_ids = vec![doomed.id];
    vault.save_health_document(&tagged).unwrap();

    vault.delete_condition(doomed.id, true).unwrap();
    assert_eq!(vault.get_condition(doomed.id).unwrap_err().code(), "not_found");
    assert_eq!(vault.get_medication(pills.id).unwrap().condition_ids, [kept.id]);
    assert!(vault.get_health_document(note.id).unwrap().condition_ids.is_empty());
    for tracker in &doomed.plan.tracker_ids {
        assert_eq!(vault.tracker(*tracker).unwrap_err().code(), "not_found");
    }
    for tracker in &kept.plan.tracker_ids {
        vault.tracker(*tracker).unwrap();
    }

    // Without the flag the trackers stay: their readings are a history.
    vault.delete_condition(kept.id, false).unwrap();
    assert!(vault.get_medication(pills.id).unwrap().condition_ids.is_empty());
    for tracker in &kept.plan.tracker_ids {
        vault.tracker(*tracker).unwrap();
    }
}

// ---- labs ------------------------------------------------------------------

#[test]
fn results_are_grouped_by_test_and_found_by_code_or_name() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let day = |m: i8, d: i8| Date::constant(2026, m, d);
    for result in [
        lab("4548-4", "A1c", 7.4, day(3, 1), "%"),
        lab("", "Ferritin", 30.0, day(5, 20), "ng/mL"),
        lab("4548-4", "Hemoglobin A1c", 6.8, day(5, 1), "%"),
        lab("3016-3", "TSH", 2.1, day(5, 10), "mIU/L"),
        lab("", " ferritin ", 41.0, day(6, 2), "ng/mL"),
    ] {
        vault.save_lab(&result).unwrap();
    }

    let trends = vault.lab_trends().unwrap();
    let keys: Vec<&str> = trends.iter().map(|t| t.key.as_str()).collect();
    assert_eq!(keys, ["name:ferritin", "3016-3", "4548-4"]);
    let values: Vec<Option<f64>> = trends[2].results.iter().map(|r| r.value).collect();
    assert_eq!(values, [Some(7.4), Some(6.8)], "oldest first");
    assert_eq!(trends[2].name, "Hemoglobin A1c", "the latest result's name");

    assert_eq!(vault.list_labs(None).unwrap().len(), 5);
    assert_eq!(vault.list_labs(Some("4548-4")).unwrap().len(), 2);
    assert_eq!(vault.list_labs(Some("name:FERRITIN")).unwrap().len(), 2);
    assert!(vault.list_labs(Some("nothing")).unwrap().is_empty());
}

// ---- documents -------------------------------------------------------------

#[test]
fn a_document_holds_only_files_the_vault_has_and_at_most_twenty() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let bytes = b"%PDF-1.4 a lab report";
    let file = uploaded(&vault, bytes, "report.pdf");

    let doc = vault
        .add_health_document("  Quarterly labs ", DocKind::LabReport, None, vec![file.clone()])
        .unwrap();
    assert_eq!(doc.title, "Quarterly labs");
    assert_eq!(doc.files[0].size, bytes.len() as u64, "the size is the blob's own");
    assert_eq!(doc.extraction, ExtractionState::None);
    assert_eq!(vault.health_document_file(doc.id, file.blob).unwrap(), bytes);

    let ghost = DocFile { blob: BlobId::of(b"never stored"), ..file.clone() };
    let refused = vault.add_health_document("Ghost", DocKind::Other, None, vec![ghost]);
    assert_eq!(refused.unwrap_err().code(), "invalid");
    let crowded = vault.add_health_document("Crowd", DocKind::Other, None, vec![file.clone(); 21]);
    assert_eq!(crowded.unwrap_err().code(), "invalid");
    let blank = vault.add_health_document("   ", DocKind::Other, None, vec![file.clone()]);
    assert_eq!(blank.unwrap_err().code(), "invalid");

    // A blob the vault holds but this document does not list is not its to hand out.
    let other = uploaded(&vault, b"another document's page", "other.pdf");
    let stranger = vault.health_document_file(doc.id, other.blob);
    assert_eq!(stranger.unwrap_err().code(), "invalid");
}

#[test]
fn accepting_a_documents_results_moves_them_out_of_its_pending_list() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let day = Date::constant(2026, 10, 12);
    let file = uploaded(&vault, b"%PDF-1.4 labs", "labs.pdf");
    let doc = vault.add_health_document("Labs", DocKind::LabReport, Some(day), vec![file]).unwrap();

    let found = vec![
        lab("4548-4", "Hemoglobin A1c", 6.1, day, "%"),
        lab("3016-3", "TSH", 2.4, day, "mIU/L"),
    ];
    let text = Some("HbA1c 6.1 %".to_string());
    let doc =
        vault.set_document_extraction(doc.id, ExtractionState::Proposed, text, found, "").unwrap();
    assert_eq!(doc.extraction, ExtractionState::Proposed);
    assert_eq!(doc.text, "HbA1c 6.1 %");
    assert_eq!(doc.pending_labs.len(), 2);
    assert!(doc.pending_labs.iter().all(|l| l.document_id == Some(doc.id)));
    assert!(vault.list_labs(None).unwrap().is_empty(), "nothing is a result until accepted");

    // The person corrects one beside the page, and accepts it alone.
    let mut reviewed = doc.pending_labs[0].clone();
    reviewed.value = Some(6.2);
    let saved = vault.accept_document_labs(doc.id, &[reviewed]).unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].value, Some(6.2));
    assert_eq!(saved[0].document_id, Some(doc.id));
    assert_eq!(saved[0].origin, LabOrigin::Extracted);
    assert_eq!(vault.list_labs(None).unwrap().len(), 1);

    let doc = vault.get_health_document(doc.id).unwrap();
    assert_eq!(doc.pending_labs.len(), 1);
    assert_eq!(doc.extraction, ExtractionState::Proposed, "one is still waiting");

    // The rest, and the document is done.
    let rest = doc.pending_labs.clone();
    vault.accept_document_labs(doc.id, &rest).unwrap();
    let doc = vault.get_health_document(doc.id).unwrap();
    assert!(doc.pending_labs.is_empty());
    assert_eq!(doc.extraction, ExtractionState::Done);
    assert_eq!(vault.list_labs(None).unwrap().len(), 2);

    // The same result accepted again is that result, not a second one.
    vault.accept_document_labs(doc.id, &saved).unwrap();
    assert_eq!(vault.list_labs(None).unwrap().len(), 2);
}

#[test]
fn saving_a_document_keeps_what_extraction_found_and_what_the_person_reviewed() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let file = uploaded(&vault, b"%PDF-1.4 labs", "labs.pdf");
    let doc = vault.add_health_document("Labs", DocKind::LabReport, None, vec![file]).unwrap();
    let day = Date::constant(2026, 10, 12);
    let doc = vault
        .set_document_extraction(
            doc.id,
            ExtractionState::Proposed,
            Some("the text".into()),
            vec![lab("3016-3", "TSH", 2.4, day, "mIU/L")],
            "",
        )
        .unwrap();

    let mut edited = doc.clone();
    edited.title = "October labs".into();
    edited.provider = "Quest".into();
    edited.pending_labs[0].value = Some(2.5);
    // Things a person does not edit, sent back wrong on purpose.
    edited.text = "tampered".into();
    edited.files.clear();
    edited.extraction = ExtractionState::Done;
    let saved = vault.save_health_document(&edited).unwrap();

    assert_eq!((saved.title.as_str(), saved.provider.as_str()), ("October labs", "Quest"));
    assert_eq!(saved.pending_labs[0].value, Some(2.5));
    assert_eq!(saved.text, "the text");
    assert_eq!(saved.files, doc.files);
    assert_eq!(saved.extraction, ExtractionState::Proposed);
}

#[test]
fn deleting_a_document_detaches_or_removes_the_results_accepted_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let day = Date::constant(2026, 10, 12);
    let accepted_from = |title: &str| {
        let file = uploaded(&vault, title.as_bytes(), "page.pdf");
        let doc = vault.add_health_document(title, DocKind::LabReport, None, vec![file]).unwrap();
        let mut result = lab("3016-3", "TSH", 2.4, day, "mIU/L");
        result.document_id = Some(doc.id);
        vault.accept_document_labs(doc.id, &[result]).unwrap();
        doc
    };
    let keeps = accepted_from("keep the results");
    let drops = accepted_from("drop the results");
    assert_eq!(vault.list_labs(None).unwrap().len(), 2);

    vault.delete_health_document(drops.id, true).unwrap();
    assert_eq!(vault.get_health_document(drops.id).unwrap_err().code(), "not_found");
    let left = vault.list_labs(None).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].document_id, Some(keeps.id));

    vault.delete_health_document(keeps.id, false).unwrap();
    let left = vault.list_labs(None).unwrap();
    assert_eq!(left.len(), 1, "the result stays");
    assert_eq!(left[0].document_id, None, "and no longer points at a document that is gone");
}

// ---- foods and meals -------------------------------------------------------

#[test]
fn a_meal_moves_each_foods_last_use_and_count_once() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let apple = vault.save_food(&food("Apple")).unwrap();
    let oats = vault.save_food(&food("Oats")).unwrap();
    let nuts = vault.save_food(&food("Almonds")).unwrap();
    assert_eq!((apple.use_count, apple.last_used), (0, None));
    let used = |food: &Food| {
        let stored = vault.list_foods("").unwrap().into_iter().find(|f| f.id == food.id).unwrap();
        (stored.use_count, stored.last_used)
    };
    let day = Date::constant(2026, 11, 10);

    let at_noon = meal(at(day, 12, 0), vec![eaten(&apple, 52.0), eaten(&oats, 380.0)]);
    let lunch = vault.save_meal(&at_noon).unwrap();
    assert_eq!(used(&apple), (1, Some(at(day, 12, 0))));
    assert_eq!(used(&oats), (1, Some(at(day, 12, 0))));

    // An earlier meal counts, but the last use stays the later one.
    vault.save_meal(&meal(at(day, 7, 0), vec![eaten(&apple, 52.0)])).unwrap();
    assert_eq!(used(&apple), (2, Some(at(day, 12, 0))));

    // Fixing a typo in lunch is not eating it again.
    let mut typo = lunch.clone();
    typo.note = "big one".into();
    vault.save_meal(&typo).unwrap();
    assert_eq!((used(&apple).0, used(&oats).0), (2, 1));

    // Adding a food to it is eating that food, and only that one.
    let mut more = typo.clone();
    more.items.push(eaten(&nuts, 600.0));
    vault.save_meal(&more).unwrap();
    assert_eq!((used(&apple).0, used(&oats).0, used(&nuts).0), (2, 1, 1));

    // Searching is case-folded; editing a food keeps how it has been used.
    let found = vault.list_foods("APP").unwrap();
    assert_eq!(found.len(), 1);
    let mut renamed = found[0].clone();
    renamed.name = "Green apple".into();
    renamed.use_count = 0;
    renamed.last_used = None;
    let renamed = vault.save_food(&renamed).unwrap();
    assert_eq!((renamed.use_count, renamed.last_used), (2, Some(at(day, 12, 0))));
}

#[test]
fn a_days_nutrition_is_what_was_logged_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let apple = vault.save_food(&food("Apple")).unwrap();
    let day = Date::constant(2026, 11, 10);
    let lunch = meal(at(day, 12, 0), vec![eaten(&apple, 52.0), eaten(&apple, 100.0)]);
    vault.save_meal(&lunch).unwrap();

    let days = vault.health_nutrition(day, day, "UTC").unwrap();
    assert_eq!(days.len(), 1, "{days:?}");
    assert_eq!(days[0].date, day);
    assert_eq!(days[0].meals, 1);
    assert_eq!(days[0].logged.kcal, 152.0);

    let listed = vault.list_meals(at(day, 0, 0), at(day, 23, 59)).unwrap();
    assert_eq!(listed.len(), 1);
}

#[test]
fn a_meal_without_glucose_beside_it_answers_with_nothing_to_say() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let lunch = vault.save_meal(&meal(at(Date::constant(2026, 11, 10), 12, 0), vec![])).unwrap();
    let response = vault.meal_response(lunch.id).unwrap();
    assert_eq!(response.meal_id, lunch.id);
    assert!(response.baseline.is_none() && response.peak.is_none());
    assert!(response.points.is_empty());
}

// ---- what is checked -------------------------------------------------------

#[test]
fn what_is_saved_is_checked_and_tidied() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let refused = |m: Medication| vault.save_medication(&m).unwrap_err().code();

    assert_eq!(refused(medication("   ")), "invalid", "a medication needs a name");
    assert_eq!(refused(medication(&"x".repeat(201))), "invalid", "and not a long one");
    let mut none = medication("Aspirin");
    none.dose = 0.0;
    assert_eq!(refused(none), "invalid", "a dose is above zero");
    let mut nan = medication("Aspirin");
    nan.dose = f64::NAN;
    assert_eq!(refused(nan), "invalid", "and a number");
    let mut crowded = medication("Aspirin");
    crowded.schedule =
        MedSchedule::Daily { times: (0..13).map(|h| Time::constant(h, 0, 0, 0)).collect() };
    assert_eq!(refused(crowded), "invalid", "thirteen times a day is too many");
    let mut orphan = medication("Aspirin");
    orphan.condition_ids = vec![ConditionId::new()];
    assert_eq!(refused(orphan), "invalid", "a condition that is not there");

    let mut twelve = medication("Vitamin C");
    twelve.schedule =
        MedSchedule::Daily { times: (0..12).map(|h| Time::constant(h, 0, 0, 0)).collect() };
    vault.save_medication(&twelve).unwrap();

    let mut calcium = medication("  Calcium ");
    calcium.tags = vec![" Calcium ".into(), "IRON".into()];
    let saved = vault.save_medication(&calcium).unwrap();
    assert_eq!(saved.name, "Calcium");
    assert_eq!(saved.tags, ["calcium", "iron"]);

    let mut sour = food("Lemon");
    sour.per_100g.carbs_g = -1.0;
    assert_eq!(vault.save_food(&sour).unwrap_err().code(), "invalid", "a nutrient is not negative");
    let mut photo = meal(at(Date::constant(2026, 11, 10), 12, 0), vec![]);
    photo.photo = Some(BlobId::of(b"no such photo"));
    assert_eq!(vault.save_meal(&photo).unwrap_err().code(), "invalid", "a photo the vault lacks");
    let mut broken = lab("3016-3", "TSH", f64::INFINITY, Date::constant(2026, 1, 1), "");
    assert_eq!(vault.save_lab(&broken).unwrap_err().code(), "invalid");
    broken.value = Some(2.0);
    broken.document_id = Some(karmayoga_core::HealthDocumentId::new());
    assert_eq!(vault.save_lab(&broken).unwrap_err().code(), "invalid", "a document not there");
}

#[test]
fn an_allergy_is_kept_edited_and_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let now = Timestamp::now();
    let penicillin = Allergy {
        id: AllergyId::new(),
        substance: " Penicillin ".into(),
        kind: AllergyKind::Drug,
        reaction: "hives".into(),
        severity: Severity::Moderate,
        noted: None,
        notes: String::new(),
        created_at: now,
        updated_at: now,
    };
    let saved = vault.save_allergy(&penicillin).unwrap();
    assert_eq!(saved.substance, "Penicillin");
    let stored = vault.list_allergies().unwrap().remove(0);

    // An edit keeps when it was first written down.
    let mut worse = saved.clone();
    worse.severity = Severity::Severe;
    let again = vault.save_allergy(&worse).unwrap();
    assert_eq!(again.created_at, stored.created_at);
    assert_eq!(vault.list_allergies().unwrap().len(), 1);

    let mut blank = saved.clone();
    blank.substance = "  ".into();
    assert_eq!(vault.save_allergy(&blank).unwrap_err().code(), "invalid");

    vault.delete_allergy(saved.id).unwrap();
    assert!(vault.list_allergies().unwrap().is_empty());
}

// ---- a visit ---------------------------------------------------------------

#[test]
fn a_visit_is_summed_up_from_what_fell_inside_it() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    let day = |m: i8, d: i8| Date::constant(2026, m, d);
    let change = |on: Date, dose: f64, strength: &str| DoseChange {
        on,
        dose,
        strength: strength.into(),
        note: String::new(),
    };

    let mut thyroid = medication("Levothyroxine");
    thyroid.dose = 75.0;
    thyroid.history = vec![change(day(9, 1), 50.0, "50 mcg"), change(day(10, 5), 75.0, "75 mcg")];
    let mut old = medication("Amoxicillin");
    old.status = MedStatus::Stopped;
    old.history = vec![change(day(10, 20), 500.0, "500 mg")];
    vault.save_medication(&thyroid).unwrap();
    vault.save_medication(&old).unwrap();
    vault.save_lab(&lab("3016-3", "TSH", 2.0, day(9, 15), "mIU/L")).unwrap();
    vault.save_lab(&lab("3016-3", "TSH", 1.5, day(10, 10), "mIU/L")).unwrap();
    let now = Timestamp::now();
    vault
        .save_allergy(&Allergy {
            id: AllergyId::new(),
            substance: "Penicillin".into(),
            kind: AllergyKind::Drug,
            reaction: String::new(),
            severity: Severity::Mild,
            noted: None,
            notes: String::new(),
            created_at: now,
            updated_at: now,
        })
        .unwrap();

    let facts = vault.visit_facts(None, day(10, 1), day(10, 31), "UTC").unwrap();
    assert_eq!((facts.since, facts.until), (day(10, 1), day(10, 31)));
    let active: Vec<&str> = facts.medications.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(active, ["Levothyroxine"], "only what is being taken");
    let changes: Vec<(&str, Date)> =
        facts.medication_changes.iter().map(|(name, c)| (name.as_str(), c.on)).collect();
    assert_eq!(changes, [("Levothyroxine", day(10, 5)), ("Amoxicillin", day(10, 20))]);
    assert_eq!(facts.labs.len(), 1);
    assert_eq!(facts.labs[0].collected, day(10, 10));
    assert_eq!(facts.allergies.len(), 1);
    assert!(facts.conditions.is_empty());
    assert!(facts.glucose.is_none(), "no readings, no glucose report");
    assert_eq!(facts.weight_change_kg, None);
    assert_eq!(facts.mean_sleep_min, None);
    assert_eq!(facts.mean_steps, None);

    let backwards = vault.visit_facts(None, day(10, 31), day(10, 1), "UTC");
    assert_eq!(backwards.unwrap_err().code(), "invalid");
}
