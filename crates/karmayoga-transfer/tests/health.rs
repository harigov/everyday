//! Health, out of a vault and back into another.
//!
//! Its own file for Money's and Screen time's reason, and one more: what
//! matters about this export is both what is *not* in it -- a dedupe key, a
//! provider's id, the id of a push token, what the person deleted -- and what
//! does not come back. A watch's samples, its workouts and the doses its
//! phone recorded are exported and are not imported: their keys are hashed
//! under the vault's own key, which an import does not hold, and the phone
//! brings them back anyway.
//!
//! The records are written straight into the store rather than through the
//! vault's facade, so these tests are about the part and not about how the
//! vault files a heart rate. They are built from the JSON their own types
//! read, naming what a test looks at: everything else has its default.

use jiff::Timestamp;
use karmayoga_core::health::{
    Allergy, Condition, Dose, Food, HealthDocument, HealthSettings, LabResult, Meal, Medication,
};
use karmayoga_core::store::JournalStore;
use karmayoga_core::store::health::SeriesRow;
use karmayoga_core::{
    AllergyId, ConditionId, DoseId, FoodId, HealthDocumentId, HealthSourceId, HealthStore,
    LabResultId, MealId, MedicationId, Vault, VaultConfig, WorkoutId,
};
use karmayoga_transfer::{Mode, Options, zip};
use serde_json::json;
use std::collections::BTreeMap;

const DOSE_KEY: &str = "dose-secret-key-1";
const HIDDEN_DOSE_KEY: &str = "dose-secret-key-2";
const WORKOUT_KEY: &str = "workout-secret-key-1";
const EXTERNAL_WORKOUT: &str = "external-workout-secret";
const EXTERNAL_DOCUMENT: &str = "fhir-resource-secret";
const TOKEN_ID: &str = "token-id-secret-1";
const FORMULA: &str = "=HYPERLINK(\"https://evil.example\",\"click\")";
const MAC_SOURCE: &str = "=cmd|' /C calc'!A0";

const T0: &str = "2026-11-05T09:00:00Z";
const T1: &str = "2026-11-05T12:00:00Z";
/// 2026-11-05T00:00:00Z, which the samples are an hour or two after.
const MIDNIGHT: i64 = 1_793_836_800;

const REPORT: &[u8] = b"%PDF-1.4 a scanned lab report";
const SCAN: &[u8] = b"\x89PNG\r\n\x1a\n not really a picture";
const PLATE: &[u8] = b"\xff\xd8\xff not really a photograph of a plate";

fn vault(dir: &std::path::Path) -> Vault {
    karmayoga_vault::create(
        dir,
        VaultConfig {
            name: "Test".into(),
            backend: "sqlite".into(),
            settings: Default::default(),
            password: None,
            kdf: karmayoga_core::crypto::KdfParams::insecure_fast(),
            auto_lock_seconds: 0,
            forget_key_seconds: 0,
        },
    )
    .unwrap()
}

fn only_health(media: bool) -> Options {
    Options { parts: vec!["health".into()], media }
}

fn health_part() -> Vec<String> {
    vec!["health".to_string()]
}

fn text<'a>(archive: &'a zip::Reader, name: &str) -> &'a str {
    std::str::from_utf8(archive.get(name).unwrap_or_else(|| panic!("no {name} in the archive")))
        .unwrap()
}

fn with_store<T>(vault: &Vault, f: impl FnOnce(&dyn JournalStore) -> T) -> T {
    vault.with_store(|store| Ok(f(store))).unwrap()
}

/// Run `f` against the vault's health store.
fn with<T>(vault: &Vault, f: impl FnOnce(&dyn HealthStore) -> T) -> T {
    with_store(vault, |store| f(store.health().expect("a health store")))
}

/// A record, from the JSON its own serde reads.
fn make<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
    serde_json::from_value(value).expect("a record the type can read")
}

fn everything() -> (Timestamp, Timestamp) {
    (Timestamp::MIN, Timestamp::MAX)
}

fn every_dose(vault: &Vault) -> Vec<Dose> {
    with(vault, |h| h.doses(everything().0, everything().1, None).unwrap())
}

fn every_meal(vault: &Vault) -> Vec<Meal> {
    with(vault, |h| h.meals(everything().0, everything().1).unwrap())
}

fn by_id<T, K: Ord>(mut list: Vec<T>, key: impl Fn(&T) -> K) -> Vec<T> {
    list.sort_by_key(key);
    list
}

/// What the vault was filled with, for what comes back to be held against.
struct Filled {
    levothyroxine: Medication,
    vitamin: Medication,
    condition: Condition,
    allergy: Allergy,
    food: Food,
    labs: Vec<LabResult>,
    document: HealthDocument,
    meal: Meal,
    /// The doses typed in or tapped from a reminder: the ones that come back.
    typed_doses: Vec<Dose>,
}

/// Two medications, a condition, an allergy, a food, two lab results (one
/// whose name is a spreadsheet formula), a document with two files, a meal
/// with a photograph, four doses (two typed, one a source's event, one a
/// source's event the person deleted), two workouts (one deleted), a source
/// with a push token, changed settings, and three samples of heart rate -- one
/// from a device whose name is a formula.
fn fill(vault: &Vault) -> Filled {
    with_store(vault, |store| {
        let health = store.health().expect("a health store");

        let source: karmayoga_core::health::HealthSource = make(json!({
            "id": HealthSourceId::new(), "connector": "healthAutoExport",
            "name": "Hari's iPhone", "paused": false,
            "status": { "lastError": "the vault was locked", "devices": ["Apple Watch"] },
            "tokenId": TOKEN_ID, "createdAt": T0, "updatedAt": T0,
        }));
        health.put_source(&source).unwrap();

        let levothyroxine: Medication = make(json!({
            "id": MedicationId::new(), "name": "Levothyroxine", "kind": "prescription",
            "form": "tablet", "strength": "50 mcg", "dose": 1.0, "doseUnit": "tablet",
            "schedule": { "type": "daily", "times": ["06:30:00"] },
            "rules": [{ "type": "emptyStomach", "minutes": 30 }],
            "tags": ["thyroid"], "status": "active", "started": "2024-03-01",
            "history": [{ "on": "2024-03-01", "dose": 1.0, "strength": "25 mcg", "note": "low" }],
            "prescriber": "Dr. Okafor", "supply": { "remaining": 42.0, "refillDays": 10 },
            "codes": [{ "system": "rxnorm", "code": "10582", "display": "levothyroxine" }],
            "notes": FORMULA, "sortOrder": 1, "createdAt": T0, "updatedAt": T1,
        }));
        let vitamin: Medication = make(json!({
            "id": MedicationId::new(), "name": "Vitamin D", "kind": "supplement",
            "schedule": { "type": "asNeeded" }, "sortOrder": 2, "createdAt": T0, "updatedAt": T0,
        }));
        health.put_medication(&levothyroxine).unwrap();
        health.put_medication(&vitamin).unwrap();

        let condition: Condition = make(json!({
            "id": ConditionId::new(), "name": "Hypothyroidism", "template": "hypothyroidism",
            "status": "active", "diagnosed": "2023-05-04",
            "plan": {
                "watch": [
                    {
                        "type": "metric", "metric": "resting_heart_rate",
                        "stat": { "type": "mean" },
                    },
                    { "type": "lab", "code": "3016-3" },
                ],
                "targets": [{
                    "label": "TSH inside the lab's own range",
                    "measure": { "type": "lab", "code": "3016-3" },
                    "useReferenceRange": true,
                }],
                "labs": [{
                    "code": "3016-3", "name": "TSH", "everyDays": 365,
                    "afterChangeDays": 42, "afterChangeTag": "thyroid",
                }],
                "medRules": { "thyroid": [{ "type": "emptyStomach", "minutes": 30 }] },
                "templateVersion": 1,
            },
            "createdAt": T0, "updatedAt": T1,
        }));
        health.put_condition(&condition).unwrap();
        let allergy: Allergy = make(json!({
            "id": AllergyId::new(), "substance": "Penicillin", "kind": "drug",
            "reaction": "hives", "severity": "severe", "noted": "2019-07-01",
            "createdAt": T0, "updatedAt": T0,
        }));
        health.put_allergy(&allergy).unwrap();
        let food: Food = make(json!({
            "id": FoodId::new(), "name": "Brown rice, cooked", "barcode": "0123456789012",
            "source": { "type": "usda", "fdcId": 169704 },
            "per100g": {
                "kcal": 123.0, "proteinG": 2.7, "carbsG": 25.6, "fatG": 1.0,
                "fiberG": 1.6, "extra": { "calcium_mg": 10.0 },
            },
            "servings": [{ "name": "1 cup", "grams": 195.0 }],
            "favorite": true, "lastUsed": T1, "useCount": 3,
            "createdAt": T0, "updatedAt": T1,
        }));
        health.put_food(&food).unwrap();

        // A document with two files, and a result read out of it waiting to
        // be accepted.
        let report = store.put_blob(REPORT).unwrap();
        let scan = store.put_blob(SCAN).unwrap();
        let document_id = HealthDocumentId::new();
        let document: HealthDocument = make(json!({
            "id": document_id, "title": "Thyroid panel", "kind": "labReport",
            "date": "2026-10-02", "provider": "Quest Diagnostics",
            "files": [
                { "blob": report, "name": "report.pdf", "mime": "application/pdf",
                  "size": REPORT.len() },
                { "blob": scan, "name": "page 2.png", "mime": "image/png", "size": SCAN.len() },
            ],
            "text": "TSH 2.4 mIU/L (0.4-4.0)",
            "fhir": [{
                "resourceType": "Observation", "display": "TSH", "date": "2026-10-02",
                "value": "2.4 mIU/L",
            }],
            "extraction": "proposed",
            "pendingLabs": [{
                "id": LabResultId::new(), "name": "Free T4", "value": 1.2, "unit": "ng/dL",
                "collected": "2026-10-02", "origin": "extracted", "createdAt": T0, "updatedAt": T0,
            }],
            "conditionIds": [condition.id], "externalId": EXTERNAL_DOCUMENT,
            "createdAt": T0, "updatedAt": T1,
        }));
        health.put_document(&document).unwrap();

        let tsh: LabResult = make(json!({
            "id": LabResultId::new(), "code": "3016-3", "name": "TSH", "value": 2.4,
            "unit": "mIU/L", "refLow": 0.4, "refHigh": 4.0, "refText": "0.4-4.0",
            "flag": "normal", "collected": "2026-10-02", "collectedAt": T0,
            "lab": "Quest", "panel": "Thyroid", "documentId": document_id,
            "origin": "extracted", "note": "fasting", "createdAt": T0, "updatedAt": T1,
        }));
        let odd: LabResult = make(json!({
            "id": LabResultId::new(), "name": FORMULA, "text": "See note, with a comma\nand a line",
            "flag": "abnormal", "collected": "2026-09-01", "createdAt": T0, "updatedAt": T0,
        }));
        health.put_labs(&[tsh.clone(), odd.clone()]).unwrap();

        let photo = store.put_blob(PLATE).unwrap();
        let meal: Meal = make(json!({
            "id": MealId::new(), "at": T1, "offsetMin": -480, "slot": "dinner",
            "name": "Chicken and rice",
            "items": [{
                "foodId": food.id, "name": "Brown rice, cooked", "amount": 1.5, "unit": "cup",
                "grams": 292.5, "estimated": false,
                "nutrients": { "kcal": 205.0, "proteinG": 4.3, "carbsG": 45.0, "fatG": 0.4 },
            }, {
                "name": "Chicken, roasted", "amount": 120.0, "unit": "g", "estimated": true,
                "nutrients": { "kcal": 190.0, "proteinG": 30.0, "carbsG": 0.0, "fatG": 7.0 },
            }],
            "photo": photo, "note": "second helping", "createdAt": T0, "updatedAt": T1,
        }));
        health.put_meal(&meal).unwrap();

        let dose = |extra: serde_json::Value| -> Dose {
            let mut value = json!({
                "id": DoseId::new(), "medicationId": levothyroxine.id, "at": T1,
                "status": "taken", "origin": "manual", "createdAt": T0, "updatedAt": T0,
            });
            value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            make(value)
        };
        let typed = dose(json!({ "note": "with water" }));
        let reminded = dose(json!({
            "at": T0, "origin": "reminder", "status": "skipped", "amount": 0.5,
            "scheduledAt": "2026-11-05T08:30:00Z", "note": "felt sick",
        }));
        let event = dose(json!({ "origin": "source", "key": DOSE_KEY }));
        let deleted = dose(json!({ "origin": "source", "key": HIDDEN_DOSE_KEY, "hidden": true }));
        health
            .put_doses(&[typed.clone(), reminded.clone(), event.clone(), deleted.clone()])
            .unwrap();

        let run: karmayoga_core::health::Workout = make(json!({
            "id": WorkoutId::new(), "activity": "running", "name": "Outdoor Run",
            "start": T0, "end": "2026-11-05T09:30:00Z", "offsetMin": -480,
            "distanceM": 5012.5, "avgHr": 151.0, "source": "Apple Watch",
            "externalId": EXTERNAL_WORKOUT, "note": "felt good", "key": WORKOUT_KEY,
            "createdAt": T0, "updatedAt": T0,
        }));
        let forgotten: karmayoga_core::health::Workout = make(json!({
            "id": WorkoutId::new(), "activity": "walking", "name": "A hidden workout",
            "start": T1, "end": "2026-11-05T12:20:00Z", "key": "workout-secret-key-2",
            "hidden": true, "createdAt": T0, "updatedAt": T0,
        }));
        health.put_workouts(&[run, forgotten]).unwrap();

        health
            .put_health_settings(&HealthSettings {
                units: make(json!({
                    "glucose": "mmol/L", "weight": "lb", "distance": "mi",
                    "energy": "kJ", "temperature": "F", "length": "in",
                })),
                priority: vec!["Apple Watch".into(), "iPhone".into()],
                ..HealthSettings::default()
            })
            .unwrap();

        // Three samples of heart rate on one day: an instant, one that
        // lasted a minute, and one from a device named like a formula.
        let sample = |start: i64, secs: u32, value: f64, source: u16| json!({ "s": MIDNIGHT + start, "d": secs, "v": value, "i": source, "o": -480 });
        let day = jiff::civil::date(2026, 11, 5);
        health
            .put_series(&[SeriesRow {
                series: "series-of-heart-rate".into(),
                day,
                data: make(json!({
                    "metric": "heart_rate", "day": "2026-11-05",
                    "sources": ["Apple Watch", MAC_SOURCE],
                    "samples": [
                        sample(3_600, 0, 61.0, 0),
                        sample(3_660, 60, 62.5, 0),
                        sample(7_200, 0, 70.0, 1),
                    ],
                })),
                summary: make(json!({ "metric": "heart_rate", "day": "2026-11-05" })),
            }])
            .unwrap();

        Filled {
            levothyroxine,
            vitamin,
            condition,
            allergy,
            food,
            labs: vec![tsh, odd],
            document,
            meal,
            typed_doses: vec![typed, reminded],
        }
    })
}

/// Every body in the archive, as text: a binary file is read lossily, which
/// is all that looking for a word in it needs.
fn everything_written(archive: &zip::Reader) -> String {
    archive
        .names()
        .map(|name| format!("{name}\n{}", String::from_utf8_lossy(archive.get(name).unwrap())))
        .collect::<Vec<_>>()
        .join("\n")
}

fn archive_of(vault: &Vault, media: bool) -> zip::Reader {
    let (bytes, _) = karmayoga_transfer::export(vault, &only_health(media), Vec::new()).unwrap();
    zip::Reader::open(&bytes).unwrap()
}

#[test]
fn health_comes_back_whole_but_for_what_the_phone_brings_again() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = vault(source_dir.path());
    let filled = fill(&source);

    let (bytes, manifest) =
        karmayoga_transfer::export(&source, &only_health(true), Vec::new()).unwrap();
    assert_eq!(manifest.parts.len(), 1);
    assert_eq!(manifest.parts[0].id, "health");
    let archive = zip::Reader::open(&bytes).unwrap();
    for name in [
        "health/samples/heart_rate.csv",
        "health/workouts.csv",
        "health/doses.csv",
        "health/labs.csv",
        "health/meals.csv",
        "health/medications.json",
        "health/conditions.json",
        "health/allergies.json",
        "health/foods.json",
        "health/sources.json",
        "health/settings.json",
    ] {
        assert!(archive.contains(name), "{name} is missing");
    }
    // Each document is a folder: its own record, and its files beside it.
    let folder = format!("health/documents/{}/", filled.document.id);
    let in_folder: Vec<&str> = archive.names().filter(|n| n.starts_with(&folder)).collect();
    assert_eq!(in_folder.len(), 3, "{in_folder:?}");
    assert!(archive.contains(&format!("{folder}document.json")));
    assert!(in_folder.iter().any(|n| n.ends_with("-report.pdf")), "{in_folder:?}");
    assert!(in_folder.iter().any(|n| n.ends_with("-page-2.png")), "{in_folder:?}");
    assert!(archive.names().any(|n| n.starts_with("health/media/")), "the meal's photograph");

    // What the samples are: a header naming the unit, a row a sample, an
    // instant ending where it started, and a device named like a formula
    // written so that a spreadsheet shows it as text.
    let samples = text(&archive, "health/samples/heart_rate.csv");
    let header = samples.lines().next().unwrap();
    assert!(header.starts_with("start,end,value") && header.ends_with(",source"), "{header}");
    assert_eq!(samples.lines().count(), 4, "{samples}");
    let at = |seconds: i64| Timestamp::from_second(MIDNIGHT + seconds).unwrap().to_string();
    assert!(samples.contains(&format!("{0},{0},61,Apple Watch\r\n", at(3_600))), "{samples}");
    assert!(samples.contains(&format!("{},{},62.5,", at(3_660), at(3_720))), "{samples}");
    assert!(samples.contains(",'=cmd|' /C calc'!A0\r\n"), "{samples}");
    assert!(!samples.contains(",=cmd"), "{samples}");

    // What the person deleted is not handed over.
    let workouts = text(&archive, "health/workouts.csv");
    assert_eq!(workouts.lines().count(), 2, "{workouts}");
    assert!(workouts.contains("Outdoor Run") && !workouts.contains("A hidden workout"));
    let doses = text(&archive, "health/doses.csv");
    assert_eq!(doses.lines().count(), 4, "three doses and a header: {doses}");
    assert!(doses.contains(",true,"), "the source's event says it was one: {doses}");

    // Never a key, a provider's id, or the id of a token.
    let written = everything_written(&archive);
    for secret in
        [DOSE_KEY, HIDDEN_DOSE_KEY, WORKOUT_KEY, EXTERNAL_WORKOUT, EXTERNAL_DOCUMENT, TOKEN_ID]
    {
        assert!(!written.contains(secret), "{secret} is in the archive");
    }
    let sources = text(&archive, "health/sources.json");
    assert!(sources.contains("Hari's iPhone") && sources.contains("healthAutoExport"));
    assert!(!sources.contains("tokenId") && !sources.contains("lastError"), "{sources}");
    // A cell that would run as a formula is text, and a text that was
    // quoted gets its quotes doubled.
    let labs = text(&archive, "health/labs.csv");
    assert!(labs.contains("\"'=HYPERLINK(\"\"https://evil.example\"\""), "{labs}");

    // What the settings dialog is told: every record, and no sample.
    let part = karmayoga_transfer::survey(&source)
        .unwrap()
        .into_iter()
        .find(|p| p.id == "health")
        .expect("health is offered");
    // A source, a workout, three doses, two medications, a condition, two
    // results, a document, an allergy, a food and a meal.
    assert_eq!(part.records, 14);
    assert!(part.media && part.imports);

    // Into a vault that has never heard of any of it.
    let target_dir = tempfile::tempdir().unwrap();
    let target = vault(target_dir.path());
    let reports =
        karmayoga_transfer::import(&target, &archive, &health_part(), Mode::Skip).unwrap();
    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert!(report.problems.is_empty(), "{report:?}");
    // Two medications, a condition, an allergy, a food, a document, two
    // results, two doses and a meal; and the one dose that was a source's
    // event is left for the phone.
    assert_eq!((report.added, report.skipped, report.replaced), (11, 1, 0), "{report:?}");

    let meds = with(&target, |h| h.list_medications().unwrap());
    assert_eq!(meds, vec![filled.levothyroxine.clone(), filled.vitamin.clone()]);
    let conditions = with(&target, |h| h.list_conditions().unwrap());
    assert_eq!(conditions, vec![filled.condition.clone()]);
    let allergies = with(&target, |h| h.list_allergies().unwrap());
    assert_eq!(allergies, vec![filled.allergy.clone()]);
    let foods = with(&target, |h| h.list_foods().unwrap());
    assert_eq!(foods, vec![filled.food.clone()]);
    let labs = with(&target, |h| h.list_labs().unwrap());
    assert_eq!(by_id(labs, |l| l.id), by_id(filled.labs.clone(), |l| l.id), "every field of both");
    let doses = every_dose(&target);
    assert_eq!(by_id(doses, |d| d.id), by_id(filled.typed_doses.clone(), |d| d.id));

    // The meal, photograph aside: that is a blob of this vault's own.
    let meals = every_meal(&target);
    assert_eq!(meals.len(), 1);
    let without_photo = |meal: &Meal| Meal { photo: None, ..meal.clone() };
    assert_eq!(without_photo(&meals[0]), without_photo(&filled.meal));
    let photo = meals[0].photo.expect("the photograph came back");
    assert_eq!(with_store(&target, |s| s.get_blob(photo).unwrap()), PLATE);

    // The document, files aside, and without the provider's id; and each
    // file with the bytes it had.
    let documents = with(&target, |h| h.list_documents().unwrap());
    assert_eq!(documents.len(), 1);
    let kept = HealthDocument { files: Vec::new(), ..documents[0].clone() };
    let was =
        HealthDocument { files: Vec::new(), external_id: String::new(), ..filled.document.clone() };
    assert_eq!(kept, was);
    let files = &documents[0].files;
    assert_eq!(files.len(), 2);
    assert_eq!((files[0].name.as_str(), files[0].mime.as_str()), ("report.pdf", "application/pdf"));
    assert_eq!(files[1].name, "page 2.png", "the name the person gave it");
    assert_eq!(files[0].size, REPORT.len() as u64);
    assert_eq!(with_store(&target, |s| s.get_blob(files[0].blob).unwrap()), REPORT);
    assert_eq!(with_store(&target, |s| s.get_blob(files[1].blob).unwrap()), SCAN);
    assert_eq!(documents[0].pending_labs, filled.document.pending_labs);

    // And what is not imported is not there: no sample, no workout, no
    // source, the settings as they were, and the source's dose with it.
    assert!(with(&target, |h| h.series_extents().unwrap()).is_empty());
    assert!(with(&target, |h| h.workouts(everything().0, everything().1).unwrap()).is_empty());
    assert!(with(&target, |h| h.list_sources().unwrap()).is_empty());
    assert_eq!(with(&target, |h| h.health_settings().unwrap()), HealthSettings::default());
    assert_eq!(every_dose(&target).len(), 2);
    assert!(with(&target, |h| h.dose_by_key(DOSE_KEY).unwrap()).is_none());
}

#[test]
fn a_second_import_changes_nothing_and_replace_keeps_a_key_the_vault_gave() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = vault(source_dir.path());
    let filled = fill(&source);
    let archive = archive_of(&source, true);

    let target_dir = tempfile::tempdir().unwrap();
    let target = vault(target_dir.path());
    let first = karmayoga_transfer::import(&target, &archive, &health_part(), Mode::Skip).unwrap();
    assert_eq!(first[0].added, 11, "{first:?}");

    // Nothing is added, nothing changes, and nothing is written twice: the
    // blobs of a document are not piled up either.
    let doses = by_id(every_dose(&target), |d| d.id);
    let blobs = with_store(&target, |s| s.list_blobs().unwrap());
    let again = karmayoga_transfer::import(&target, &archive, &health_part(), Mode::Skip).unwrap();
    assert!(again[0].problems.is_empty(), "{again:?}");
    assert_eq!((again[0].added, again[0].skipped, again[0].replaced), (0, 12, 0), "{again:?}");
    assert_eq!(by_id(every_dose(&target), |d| d.id), doses);
    let now = by_id(with_store(&target, |s| s.list_blobs().unwrap()), |b| *b);
    assert_eq!(now, by_id(blobs, |b| *b));

    // Changed here, and then Replace: the file's word wins -- except that a
    // dose here has a key the file never held, and the file does not take it
    // away.
    let mut renamed = filled.levothyroxine.clone();
    renamed.name = "Something else".into();
    let keyed = Dose { key: "a-key-this-vault-gave".into(), ..filled.typed_doses[0].clone() };
    with(&target, |h| {
        h.put_medication(&renamed).unwrap();
        h.put_doses(std::slice::from_ref(&keyed)).unwrap();
    });
    let replaced =
        karmayoga_transfer::import(&target, &archive, &health_part(), Mode::Replace).unwrap();
    assert!(replaced[0].problems.is_empty(), "{replaced:?}");
    // Every record but the doses is written over; the doses, the same as the
    // file's, are left as they are.
    assert_eq!((replaced[0].added, replaced[0].replaced), (0, 9), "{replaced:?}");
    let med = with(&target, |h| h.get_medication(filled.levothyroxine.id).unwrap());
    assert_eq!(med, filled.levothyroxine);
    let dose = with(&target, |h| h.dose_by_key("a-key-this-vault-gave").unwrap());
    assert_eq!(dose.map(|d| d.id), Some(keyed.id), "the key stayed");
    assert_eq!(every_dose(&target).len(), 2);
}

#[test]
fn without_attachments_a_document_comes_back_without_its_files() {
    let source_dir = tempfile::tempdir().unwrap();
    let source = vault(source_dir.path());
    let filled = fill(&source);
    let archive = archive_of(&source, false);

    let folder = format!("health/documents/{}/", filled.document.id);
    let in_folder: Vec<&str> = archive.names().filter(|n| n.starts_with(&folder)).collect();
    assert_eq!(in_folder, vec![format!("{folder}document.json")]);
    assert!(archive.names().all(|n| !n.starts_with("health/media/")));
    // The record still says what files it had, and that none was written.
    let json: serde_json::Value =
        serde_json::from_str(text(&archive, &format!("{folder}document.json"))).unwrap();
    let files = json["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["name"], "report.pdf");
    assert!(files[0].get("file").is_none(), "{files:?}");
    assert!(files[0].get("blob").is_none(), "a blob's address is the vault's own: {files:?}");
    assert!(json.get("externalId").is_none());

    let target_dir = tempfile::tempdir().unwrap();
    let target = vault(target_dir.path());
    let reports =
        karmayoga_transfer::import(&target, &archive, &health_part(), Mode::Skip).unwrap();
    assert!(reports[0].problems.is_empty(), "{:?}", reports[0]);
    let documents = with(&target, |h| h.list_documents().unwrap());
    assert_eq!(documents.len(), 1);
    assert!(documents[0].files.is_empty());
    assert_eq!(documents[0].text, filled.document.text, "the words came back");
    assert!(every_meal(&target)[0].photo.is_none());
    assert_eq!(every_meal(&target)[0].items, filled.meal.items);
}

#[test]
fn a_hand_made_archive_lands_what_it_can_and_names_the_rest() {
    let known = MedicationId::new();
    let medications = json!({ "medications": [
        { "id": known, "name": "Levothyroxine", "createdAt": T0, "updatedAt": T0 },
        { "name": "No id, so no medication" },
        { "id": MedicationId::new(), "name": "   ", "createdAt": T0, "updatedAt": T0 },
    ]});
    let doses = "at,medication,status,keyed\r\n\
         2026-11-05T14:00:00Z,levothyroxine,taken,\r\n\
         2026-11-05T15:00:00Z,Aspirin,taken,\r\n\
         yesterday,levothyroxine,taken,\r\n\
         2026-11-05T16:00:00Z,Levothyroxine,skipped,true\r\n";
    let labs = "name,collected,value\r\n\
         TSH,2026-10-02,2.4\r\n\
         ,2026-10-02,1\r\n\
         A1c,nope,5\r\n";
    let mut files = BTreeMap::new();
    files.insert("health/medications.json".to_string(), medications.to_string().into_bytes());
    files.insert("health/doses.csv".to_string(), doses.as_bytes().to_vec());
    files.insert("health/labs.csv".to_string(), labs.as_bytes().to_vec());
    let archive = zip::Reader::from_files(files);

    let dir = tempfile::tempdir().unwrap();
    let target = vault(dir.path());
    let reports =
        karmayoga_transfer::import(&target, &archive, &health_part(), Mode::Skip).unwrap();
    let report = &reports[0];
    // A medication, a dose named by its medication's name, and a lab result;
    // and the one that was a source's event is left for the phone.
    assert_eq!((report.added, report.skipped), (3, 1), "{report:?}");
    // No id, no name; a medication this vault has not got; a time that is
    // not one; no name; a date that is not one.
    assert_eq!(report.problems.len(), 6, "{:#?}", report.problems);
    let named = report.problems.iter().any(|p| p.starts_with("doses.csv: row 3"));
    assert!(named, "{:?}", report.problems);

    let meds = with(&target, |h| h.list_medications().unwrap());
    assert_eq!(meds.len(), 1);
    assert_eq!(meds[0].id, known);
    let doses = every_dose(&target);
    assert_eq!(doses.len(), 1);
    assert_eq!(doses[0].medication_id, known);
    assert!(doses[0].key.is_empty());
    let labs = with(&target, |h| h.list_labs().unwrap());
    assert_eq!(labs.len(), 1);
    assert_eq!((labs[0].name.as_str(), labs[0].value), ("TSH", Some(2.4)));
}

#[test]
fn a_vault_that_never_used_health_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault(dir.path());
    // Nothing alone is something anybody made -- and neither is a workout
    // that was deleted and is only kept so that it does not come back.
    let forgotten: karmayoga_core::health::Workout = make(json!({
        "id": WorkoutId::new(), "activity": "walking", "start": T0, "end": T1,
        "key": "workout-secret-key-3", "hidden": true, "createdAt": T0, "updatedAt": T0,
    }));
    with(&vault, |h| h.put_workouts(&[forgotten]).unwrap());

    let (bytes, manifest) =
        karmayoga_transfer::export(&vault, &only_health(true), Vec::new()).unwrap();
    assert!(manifest.parts.is_empty(), "{:?}", manifest.parts);
    let archive = zip::Reader::open(&bytes).unwrap();
    assert!(archive.names().all(|n| !n.starts_with("health/")));
    let part = karmayoga_transfer::survey(&vault)
        .unwrap()
        .into_iter()
        .find(|p| p.id == "health")
        .expect("health is offered even when empty");
    assert_eq!(part.records, 0);
}
