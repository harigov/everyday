//! Medications and doses, allergies, conditions, labs, documents, foods
//! and meals. See the module docs one level up.
//!
//! # What is checked here
//!
//! Every save goes through a `clean_*` function first: names are trimmed and
//! one to 200 characters, numbers are finite (a dose above zero, a nutrient
//! not below it), a schedule has at most twelve times, tags are trimmed and
//! lower-cased so a rule's `calcium` finds a medication's ` Calcium `, and a
//! document has at most twenty files, each of which the blob store must
//! hold. A refusal is an [`Error::Invalid`] with a sentence a person can
//! read; it never quotes what was typed, since this is health data.
//!
//! # Choices worth knowing
//!
//! - A medication's `created_at` is the stored one on an edit and now on a
//!   new one, whatever the caller sent; `updated_at` is always now.
//! - A dose's `key` is the vault's to set: a person's dose has none, and an
//!   edit of a source's dose keeps the one it had.
//! - A food's use is counted when a meal names it for the first time -- a
//!   new meal, or an edit that adds the food -- not on every edit, so
//!   fixing a typo in lunch does not make lunch more popular.
//! - Documents and labs from a connector are an outsider's words: invisible
//!   and control characters are stripped and lengths capped, and a lab that
//!   fails the checks is left out rather than failing the whole import.

use crate::crypto::SecretKey;
use crate::error::{Error, Result};
use crate::files::{RefKind, RefOwner};
use crate::health::{
    Adherence, Allergy, CarePlan, CareTarget, Code, Condition, ConditionReport, ConditionStatus,
    DocFile, DocKind, DocSource, Dose, DoseChange, DoseOrigin, DoseStatus, DueDose,
    ExtractionState, Food, HealthDocument, LabOrigin, LabResult, LabTrend, MAX_RANGE_DAYS, Meal,
    MealItem, MealResponse, Measure, MedRule, MedSchedule, MedStatus, Medication, Metric,
    Nutrients, NutritionDay, RawDocument, RawDose, RuleAlert, Stat, TargetStanding, TemplateChoice,
    TemplateInfo, TemplateKey, VisitFacts, glucose, nutrition, rules, schedule, templates,
};
use crate::id::{
    AllergyId, ConditionId, DoseId, FileId, FoodId, HealthDocumentId, LabResultId, MealId,
    MedicationId, TrackerId,
};
use crate::record::RecordKind;
use crate::store::JournalStore;
use crate::store::health::HealthStore;
use crate::tracker::{TRACKER_COLORS, Tracker, TrackerKind};
use crate::vault::Vault;
use crate::vault::session::{Domain, pick_domain};
use jiff::civil::{Date, Time};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Span, Timestamp};
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// The longest name a person may give anything here, in characters after
/// trimming.
const MAX_NAME: usize = 200;
/// The longest note, reason or other free text.
const MAX_NOTES: usize = 5000;
/// The longest unit, code, barcode, tag or other short token.
const MAX_SHORT: usize = 64;
/// The longest file name or media type.
const MAX_FILE_NAME: usize = 255;
const MAX_MIME: usize = 127;
/// The most times a day a medication may be scheduled.
const MAX_TIMES: usize = 12;
const MAX_TAGS: usize = 20;
const MAX_RULES: usize = 10;
const MAX_CODES: usize = 20;
const MAX_SOURCE_NAMES: usize = 50;
/// The most files one document holds.
const MAX_FILES: usize = 20;
/// The most of one file read whole for the model: a photographed page or a
/// plate, which the model is shown at 2048 pixels anyway.
const MAX_PAGE_BYTES: u64 = 64 * 1024 * 1024;
/// The most results one document waits on, or one accept saves.
const MAX_LABS: usize = 500;
/// The most things of one sort a care plan, a food or a meal holds.
const MAX_ITEMS: usize = 200;
/// The longest a target's averaging window may be, in days.
const MAX_WINDOW_DAYS: u32 = 365;
/// The longest a rule may ask a person to wait, in minutes and in hours.
const MAX_RULE_MINUTES: u32 = 24 * 60;
const MAX_RULE_HOURS: f64 = 72.0;
/// How much of a document's text, or an outsider's free text, is kept.
const MAX_DOCUMENT_TEXT: usize = 1_000_000;
const MAX_FHIR_ENTRIES: usize = 1000;
/// A source's own id for a document.
const MAX_EXTERNAL_ID: usize = 256;
/// The widest UTC offset a meal may carry, in minutes.
const MAX_OFFSET_MIN: u16 = 18 * 60;
/// How far either side of a range doses and meals are read, so a due time
/// near its edge still finds the dose that answers it and a timing rule near
/// its edge still sees the meal beside it.
const MARGIN_SECS: i64 = 12 * 3600;
/// A target with no window reads the latest day with a value this far back.
const LATEST_LOOKBACK_DAYS: i64 = 14;
/// How far back a condition's adherence and alerts look, in days.
const ADHERENCE_DAYS: i64 = 30;
const ALERT_DAYS: i64 = 7;

/// What a visit brief calls the latest vitals.
const VITALS: &[Metric] = &[
    Metric::Weight,
    Metric::Bmi,
    Metric::BloodPressureSystolic,
    Metric::BloodPressureDiastolic,
    Metric::RestingHeartRate,
    Metric::OxygenSaturation,
    Metric::BodyTemperature,
    Metric::BloodGlucose,
];

/// File a source's dose events inside an ingest's write: each matched to a
/// medication by code, then by a name it was seen under before
/// ([`Medication::source_names`]), then by name; deduplicated by a keyed
/// hash under `key` of what the *source* calls the event -- its first code
/// (or its name) and its time -- never of the status or the medication it
/// matched. A dose corrected on the phone from skipped to taken is then the
/// same event and is updated in place, and re-matching it after a
/// medication is re-added does not file it twice. One the person deleted
/// here stays hidden. Answers (added, unmatched). Unmatched events are not
/// stored -- the report says how many, and the medication screen offers to
/// add one.
pub(crate) fn file_source_doses(
    health: &dyn HealthStore,
    key: &SecretKey,
    doses: &[RawDose],
) -> Result<(u64, u64)> {
    if doses.is_empty() {
        return Ok((0, 0));
    }
    let mut medications = health.list_medications()?;
    let mut learned: BTreeSet<usize> = BTreeSet::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut fresh: Vec<Dose> = Vec::new();
    let mut unmatched = 0u64;
    let now = Timestamp::now();
    for raw in doses {
        let Some(at) = match_medication(&medications, raw) else {
            unmatched += 1;
            continue;
        };
        // The name this source used is remembered on a match however it was
        // reached, so the next event under it needs no code and no guess.
        let name = raw.name.trim();
        let medication = &mut medications[at];
        if !name.is_empty()
            && medication.source_names.len() < MAX_SOURCE_NAMES
            && name.chars().count() <= MAX_NAME
            && !is_own_name(medication, name)
            && !medication.source_names.iter().any(|n| n.trim().eq_ignore_ascii_case(name))
        {
            medication.source_names.push(name.to_string());
            learned.insert(at);
        }
        let medication_id = medication.id;
        let identity = match raw.codes.first() {
            Some(code) => format!("{}:{}", code.system.trim(), code.code.trim()),
            None => name.to_lowercase(),
        };
        let dedupe = key.keyed_id(&format!("source|{identity}|{}", raw.at.as_second()));
        if !seen.insert(dedupe.clone()) {
            continue;
        }
        if let Some(mut held) = health.dose_by_key(&dedupe)? {
            // The phone's word on what happened is the latest one, unless the
            // person deleted the dose here.
            let amount = raw.amount.filter(|a| a.is_finite() && *a > 0.0);
            if !held.hidden
                && (held.status != raw.status
                    || held.amount != amount
                    || held.scheduled_at != raw.scheduled_at)
            {
                held.status = raw.status;
                held.amount = amount;
                held.scheduled_at = raw.scheduled_at;
                held.updated_at = now;
                fresh.push(held);
            }
            continue;
        }
        fresh.push(Dose {
            id: DoseId::new(),
            medication_id,
            scheduled_at: raw.scheduled_at,
            at: raw.at,
            status: raw.status,
            amount: raw.amount.filter(|a| a.is_finite() && *a > 0.0),
            origin: DoseOrigin::Source,
            note: String::new(),
            key: dedupe,
            hidden: false,
            created_at: now,
            updated_at: now,
        });
    }
    // Doses first: if the batch is refused nothing was learned from it.
    let added = fresh.iter().filter(|d| d.created_at == now).count() as u64;
    if !fresh.is_empty() {
        health.put_doses(&fresh)?;
    }
    for at in learned {
        health.put_medication(&medications[at])?;
    }
    Ok((added, unmatched))
}

/// File a connector's documents inside an ingest's write: what the connector
/// sent sealed into the blob store as the document's [`DocSource`] -- the
/// record's own data, not a file for the files catalog -- and the document
/// saved with its FHIR summary and its labs waiting in `pending_labs`. One
/// already held (same `external_id`) is skipped. Answers how many were
/// added.
pub(crate) fn file_documents(store: &dyn JournalStore, documents: Vec<RawDocument>) -> Result<u64> {
    if documents.is_empty() {
        return Ok(0);
    }
    let health = health_of(store)?;
    let mut held: HashSet<String> = health
        .list_documents()?
        .into_iter()
        .map(|d| d.external_id)
        .filter(|e| !e.is_empty())
        .collect();
    let now = Timestamp::now();
    let mut added = 0u64;
    for raw in documents {
        let external_id = outside_text(&raw.external_id, MAX_EXTERNAL_ID);
        // The set takes this batch's own ids too, so one a connector lists
        // twice lands once.
        if !external_id.is_empty() && !held.insert(external_id.clone()) {
            continue;
        }
        let id = HealthDocumentId::new();
        let source = if raw.bytes.is_empty() {
            None
        } else {
            let blob = store.put_blob(&raw.bytes)?;
            let mut name = outside_text(&raw.file_name, MAX_FILE_NAME);
            if name.is_empty() {
                name = "record".into();
            }
            Some(DocSource {
                blob,
                name,
                mime: outside_text(&raw.mime, MAX_MIME),
                size: raw.bytes.len() as u64,
            })
        };
        let pending: Vec<LabResult> = raw
            .labs
            .into_iter()
            .filter_map(|mut lab| {
                clean_lab(&mut lab).ok()?;
                lab.id = LabResultId::new();
                lab.document_id = Some(id);
                lab.created_at = now;
                lab.updated_at = now;
                Some(lab)
            })
            .take(MAX_LABS)
            .collect();
        let mut title = outside_text(&raw.title, MAX_NAME);
        if title.is_empty() {
            title = "Imported record".into();
        }
        let mut fhir = raw.fhir;
        fhir.truncate(MAX_FHIR_ENTRIES);
        health.put_document(&HealthDocument {
            id,
            title,
            kind: raw.kind,
            date: raw.date,
            provider: outside_text(&raw.provider, MAX_NAME),
            files: Vec::new(),
            source,
            text: String::new(),
            fhir,
            extraction: if pending.is_empty() {
                ExtractionState::None
            } else {
                ExtractionState::Proposed
            },
            extraction_error: String::new(),
            pending_labs: pending,
            condition_ids: Vec::new(),
            external_id,
            notes: String::new(),
            created_at: now,
            updated_at: now,
        })?;
        added += 1;
    }
    Ok(added)
}

impl Vault {
    // ---- medications ----------------------------------------------------

    pub fn list_medications(&self) -> Result<Vec<Medication>> {
        self.with_health(|h| h.list_medications())
    }

    pub fn get_medication(&self, id: MedicationId) -> Result<Medication> {
        self.with_health(|h| h.get_medication(id))
    }

    /// Insert or update. A change of dose or strength on an existing one is
    /// recorded in its `history` (dated today) if the caller did not.
    pub fn save_medication(&self, medication: &Medication) -> Result<Medication> {
        self.writable()?;
        let mut medication = medication.clone();
        clean_medication(&mut medication)?;
        let today = today();
        let saved = self.write(|u| {
            let health = health_of(u.store.as_ref())?;
            existing_conditions(health, &medication.condition_ids)?;
            let now = Timestamp::now();
            match existing(health.get_medication(medication.id))? {
                Some(stored) => {
                    medication.created_at = stored.created_at;
                    // What a lab recheck is counted from: the day the dose
                    // or strength was changed, unless the person dated it.
                    let changed =
                        stored.dose != medication.dose || stored.strength != medication.strength;
                    if changed && !medication.history.iter().any(|c| c.on == today) {
                        medication.history.push(DoseChange {
                            on: today,
                            dose: medication.dose,
                            strength: medication.strength.clone(),
                            note: String::new(),
                        });
                        medication.history.sort_by_key(|c| c.on);
                    }
                }
                None => medication.created_at = now,
            }
            medication.updated_at = now;
            health.put_medication(&medication)?;
            Ok(medication.clone())
        })?;
        self.wrote(RecordKind::Medication, saved.id);
        Ok(saved)
    }

    /// Deletes its doses too.
    pub fn delete_medication(&self, id: MedicationId) -> Result<()> {
        self.writable()?;
        self.with_health(|h| h.delete_medication(id))?;
        self.wrote(RecordKind::Medication, id);
        Ok(())
    }

    // ---- doses ----------------------------------------------------------

    pub fn list_doses(
        &self,
        from: Timestamp,
        to: Timestamp,
        medication: Option<MedicationId>,
    ) -> Result<Vec<Dose>> {
        self.with_health(|h| Ok(shown(h.doses(from, to, medication)?)))
    }

    /// Log (or edit) a dose. A new taken dose comes off the supply.
    ///
    /// Only a new one: editing a dose (its note, its time) or turning a
    /// skipped one into a taken one leaves the count alone, because the
    /// count is of pills handed over and an edit hands over none. The
    /// supply never goes below nothing.
    pub fn log_dose(&self, dose: &Dose) -> Result<Dose> {
        self.writable()?;
        let mut dose = dose.clone();
        dose.note = person_text("a note", &dose.note, MAX_NOTES)?;
        // A skipped dose took nothing, so whatever amount came with it is
        // dropped rather than believed.
        dose.amount = match (dose.status, dose.amount) {
            (DoseStatus::Taken, Some(amount)) => Some(positive("a dose", amount)?),
            _ => None,
        };
        let (saved, drew) = self.write(|u| {
            let health = health_of(u.store.as_ref())?;
            let Some(mut medication) = existing(health.get_medication(dose.medication_id))? else {
                return Err(Error::Invalid(
                    "that medication does not exist; it may have been deleted".into(),
                ));
            };
            let now = Timestamp::now();
            let stored = existing(health.get_dose(dose.id))?;
            if let Some(stored) = &stored {
                dose.created_at = stored.created_at;
                dose.key = stored.key.clone();
                dose.origin = stored.origin;
            } else {
                dose.created_at = now;
                dose.key = String::new();
            }
            dose.updated_at = now;
            health.put_doses(std::slice::from_ref(&dose))?;
            let mut drew = false;
            if stored.is_none()
                && dose.status == DoseStatus::Taken
                && let Some(supply) = medication.supply.as_mut()
            {
                let amount = dose.amount.unwrap_or(medication.dose);
                supply.remaining = (supply.remaining - amount).max(0.0);
                medication.updated_at = now;
                health.put_medication(&medication)?;
                drew = true;
            }
            Ok((dose.clone(), drew))
        })?;
        self.wrote(RecordKind::Dose, saved.id);
        if drew {
            self.wrote(RecordKind::Medication, saved.medication_id);
        }
        Ok(saved)
    }

    /// A dose a source brought is hidden rather than deleted, so its key
    /// still matches when the phone sends the week again; one logged here
    /// goes for good.
    pub fn delete_dose(&self, id: DoseId) -> Result<()> {
        self.writable()?;
        self.write(|u| {
            let health = health_of(u.store.as_ref())?;
            let mut dose = health.get_dose(id)?;
            if dose.key.is_empty() {
                health.delete_dose(id)
            } else {
                dose.hidden = true;
                dose.updated_at = Timestamp::now();
                health.put_doses(&[dose])
            }
        })?;
        self.wrote(RecordKind::Dose, id);
        Ok(())
    }

    /// Every due time on `date` (`tz` empty is the system's).
    pub fn doses_due(&self, date: Date, tz: &str) -> Result<Vec<DueDose>> {
        let zone = zone_of(tz)?;
        let now = Timestamp::now();
        let (start, end) = day_bounds(date, date, &zone);
        let (from, to) = padded(start, end);
        self.with_health(|h| {
            let medications = h.list_medications()?;
            let doses = shown(h.doses(from, to, None)?);
            Ok(schedule::day_doses(&medications, &doses, date, &zone, now))
        })
    }

    pub fn medication_alerts(&self, from: Date, to: Date, tz: &str) -> Result<Vec<RuleAlert>> {
        range_days(from, to)?;
        let zone = zone_of(tz)?;
        self.with_health(|h| {
            let medications = h.list_medications()?;
            alerts_in(h, &medications, from, to, &zone)
        })
    }

    pub fn medication_adherence(
        &self,
        medication: Option<MedicationId>,
        from: Date,
        to: Date,
        tz: &str,
    ) -> Result<Adherence> {
        range_days(from, to)?;
        let zone = zone_of(tz)?;
        let now = Timestamp::now();
        self.with_health(|h| {
            let medications = match medication {
                Some(id) => vec![h.get_medication(id)?],
                None => h.list_medications()?,
            };
            adherence_in(h, &medications, from, to, &zone, now)
        })
    }

    // ---- allergies ------------------------------------------------------

    pub fn list_allergies(&self) -> Result<Vec<Allergy>> {
        self.with_health(|h| h.list_allergies())
    }

    pub fn save_allergy(&self, allergy: &Allergy) -> Result<Allergy> {
        self.writable()?;
        let mut allergy = allergy.clone();
        allergy.substance = name_text("an allergy's substance", &allergy.substance)?;
        allergy.reaction = person_text("a reaction", &allergy.reaction, MAX_NAME)?;
        allergy.notes = person_text("a note", &allergy.notes, MAX_NOTES)?;
        let saved = self.write(|u| {
            let health = health_of(u.store.as_ref())?;
            let now = Timestamp::now();
            allergy.created_at =
                existing(health.get_allergy(allergy.id))?.map_or(now, |stored| stored.created_at);
            allergy.updated_at = now;
            health.put_allergy(&allergy)?;
            Ok(allergy.clone())
        })?;
        self.wrote(RecordKind::Allergy, saved.id);
        Ok(saved)
    }

    pub fn delete_allergy(&self, id: AllergyId) -> Result<()> {
        self.writable()?;
        self.with_health(|h| h.delete_allergy(id))?;
        self.wrote(RecordKind::Allergy, id);
        Ok(())
    }

    // ---- conditions -----------------------------------------------------

    pub fn list_conditions(&self) -> Result<Vec<Condition>> {
        self.with_health(|h| h.list_conditions())
    }

    pub fn get_condition(&self, id: ConditionId) -> Result<Condition> {
        self.with_health(|h| h.get_condition(id))
    }

    pub fn save_condition(&self, condition: &Condition) -> Result<Condition> {
        self.writable()?;
        let mut condition = condition.clone();
        condition.name = name_text("a condition's name", &condition.name)?;
        condition.notes = person_text("a note", &condition.notes, MAX_NOTES)?;
        clean_codes(&mut condition.codes)?;
        clean_plan(&mut condition.plan)?;
        let saved = self.write(|u| {
            let health = health_of(u.store.as_ref())?;
            let now = Timestamp::now();
            condition.created_at = existing(health.get_condition(condition.id))?
                .map_or(now, |stored| stored.created_at);
            condition.updated_at = now;
            health.put_condition(&condition)?;
            Ok(condition.clone())
        })?;
        self.wrote(RecordKind::Condition, saved.id);
        Ok(saved)
    }

    /// Delete it; with `remove_trackers`, the trackers its plan created
    /// too. Medications and documents naming it lose the reference.
    ///
    /// A tracker another condition's plan still lists is kept: removing
    /// the tracker of one plan must not empty another's dashboard. The
    /// trackers go through the tracker API after the write, one at a time,
    /// ones already gone are not an error, and if one fails for another
    /// reason the rest are still tried and the first failure is answered.
    pub fn delete_condition(&self, id: ConditionId, remove_trackers: bool) -> Result<()> {
        self.writable()?;
        let (medications, documents, trackers) = self.write(|u| {
            let health = health_of(u.store.as_ref())?;
            let condition = health.get_condition(id)?;
            let now = Timestamp::now();
            let mut medications = Vec::new();
            for mut medication in health.list_medications()? {
                if medication.condition_ids.contains(&id) {
                    medication.condition_ids.retain(|c| *c != id);
                    medication.updated_at = now;
                    health.put_medication(&medication)?;
                    medications.push(medication.id);
                }
            }
            let mut documents = Vec::new();
            for mut document in health.list_documents()? {
                if document.condition_ids.contains(&id) {
                    document.condition_ids.retain(|c| *c != id);
                    document.updated_at = now;
                    health.put_document(&document)?;
                    documents.push(document.id);
                }
            }
            health.delete_condition(id)?;
            let mut trackers = Vec::new();
            if remove_trackers {
                let kept: HashSet<TrackerId> = health
                    .list_conditions()?
                    .into_iter()
                    .flat_map(|c| c.plan.tracker_ids)
                    .collect();
                trackers =
                    condition.plan.tracker_ids.into_iter().filter(|t| !kept.contains(t)).collect();
            }
            Ok((medications, documents, trackers))
        })?;
        self.wrote(RecordKind::Condition, id);
        for medication in &medications {
            self.wrote(RecordKind::Medication, medication);
        }
        for document in &documents {
            self.wrote(RecordKind::HealthDocument, document);
        }
        let mut failure = None;
        for tracker in trackers {
            match self.delete_tracker(tracker) {
                Ok(_) | Err(Error::NotFound { .. }) => {}
                Err(e) => {
                    failure.get_or_insert(e);
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }

    pub fn condition_templates(&self) -> Vec<TemplateInfo> {
        crate::health::templates::all()
    }

    /// Create a condition from a template as the person ticked it: the
    /// plan copied, the symptom trackers created (0-10 scales) and their
    /// ids kept on the plan.
    ///
    /// The trackers and the condition are written together: if the
    /// condition cannot be saved the trackers made for it are taken back,
    /// so a failed attempt leaves nothing on the Overview. The plan's own
    /// `tracker_ids` are replaced by the ones just made, since a plan being
    /// copied has no trackers of its own and an id it carried would be one
    /// somebody else's condition could end up deleting.
    pub fn apply_condition_template(
        &self,
        key: TemplateKey,
        choice: &TemplateChoice,
    ) -> Result<Condition> {
        self.writable()?;
        let template = templates::get(key);
        let mut plan = choice.plan.clone().unwrap_or_else(|| template.plan.clone());
        clean_plan(&mut plan)?;
        plan.template_version = template.version;
        let names = symptom_names(&choice.symptoms)?;
        let now = Timestamp::now();
        let mut condition = Condition {
            id: ConditionId::new(),
            name: template.name,
            template: Some(key),
            status: ConditionStatus::Active,
            diagnosed: choice.diagnosed,
            codes: template.codes,
            plan,
            notes: String::new(),
            created_at: now,
            updated_at: now,
        };
        let (condition, made) = self.write(|u| {
            let mut made = Vec::new();
            if let Err(e) = create_condition(u.store.as_ref(), &mut condition, &names, &mut made) {
                discard_trackers(u.store.as_ref(), &made);
                return Err(e);
            }
            Ok((condition.clone(), made))
        })?;
        self.wrote(RecordKind::Condition, condition.id);
        for tracker in &made {
            self.wrote(RecordKind::Tracker, tracker);
        }
        Ok(condition)
    }

    /// The condition's dashboard on `date`.
    ///
    /// A target on a metric is the mean of its daily values over the
    /// `window_days` ending on `date` (a time-in-range target's daily
    /// values are shares, and are compared to `low` and `high` as shares);
    /// with no window it is the latest day that has one in the fortnight
    /// before. A target on a lab is [`templates::lab_standing`]. A target
    /// with nothing to judge by, or no bound to judge against, is
    /// `unknown`. Adherence is over the last 30 days and alerts the last
    /// seven, for this condition's medications.
    pub fn condition_report(
        &self,
        id: ConditionId,
        date: Date,
        tz: &str,
    ) -> Result<ConditionReport> {
        let zone = zone_of(tz)?;
        let now = Timestamp::now();
        let (condition, all, labs) = self
            .with_health(|h| Ok((h.get_condition(id)?, h.list_medications()?, h.list_labs()?)))?;
        let mut targets = Vec::with_capacity(condition.plan.targets.len());
        for target in &condition.plan.targets {
            targets.push(self.target_standing(target, &labs, date, tz, &zone)?);
        }
        let labs_due = templates::labs_due(&condition.plan, &labs, &all, date);
        let mine: Vec<Medication> =
            all.iter().filter(|m| m.condition_ids.contains(&id)).cloned().collect();
        let (adherence, alerts) = self.with_health(|h| {
            let adherence =
                adherence_in(h, &mine, days_before(date, ADHERENCE_DAYS - 1), date, &zone, now)?;
            // The rules compare a medication with the others it is kept
            // apart from, so every medication is checked; only the alerts
            // about this condition's own are reported.
            let ids: HashSet<MedicationId> = mine.iter().map(|m| m.id).collect();
            let alerts: Vec<RuleAlert> =
                alerts_in(h, &all, days_before(date, ALERT_DAYS - 1), date, &zone)?
                    .into_iter()
                    .filter(|a| ids.contains(&a.medication_id))
                    .collect();
            Ok((adherence, alerts))
        })?;
        Ok(ConditionReport { condition, targets, labs_due, medications: mine, adherence, alerts })
    }

    fn target_standing(
        &self,
        target: &CareTarget,
        labs: &[LabResult],
        date: Date,
        tz: &str,
        zone: &TimeZone,
    ) -> Result<TargetStanding> {
        let (metric, stat) = match &target.measure {
            Measure::Lab { .. } => return Ok(templates::lab_standing(target, labs)),
            Measure::Metric { metric, stat } => (*metric, *stat),
        };
        let latest_only = target.window_days == 0;
        let window = if latest_only { LATEST_LOOKBACK_DAYS } else { i64::from(target.window_days) };
        let daily = self.health_daily(metric, stat, days_before(date, window - 1), date, tz)?;
        let value = if latest_only {
            daily.values().next_back().copied()
        } else {
            mean(daily.values().copied())
        };
        let at =
            daily.keys().next_back().and_then(|day| zone.to_timestamp(day.at(0, 0, 0, 0)).ok());
        let state = match value {
            Some(v) => judge(v, target),
            None => "unknown",
        };
        Ok(TargetStanding { target: target.clone(), value, at, state: state.into() })
    }

    // ---- labs -----------------------------------------------------------

    /// Every result, or only one test's (by LOINC code, or `name:<name>`).
    pub fn list_labs(&self, key: Option<&str>) -> Result<Vec<LabResult>> {
        let wanted = key.map(canonical_key).filter(|k| !k.is_empty());
        self.with_health(|h| {
            let mut labs = h.list_labs()?;
            if let Some(wanted) = &wanted {
                labs.retain(|lab| lab_key(lab) == *wanted);
            }
            Ok(labs)
        })
    }

    pub fn save_lab(&self, lab: &LabResult) -> Result<LabResult> {
        self.writable()?;
        let mut lab = lab.clone();
        clean_lab(&mut lab)?;
        let saved = self.write(|u| {
            let health = health_of(u.store.as_ref())?;
            if let Some(document) = lab.document_id
                && existing(health.get_document(document))?.is_none()
            {
                return Err(Error::Invalid(
                    "that document does not exist; it may have been deleted".into(),
                ));
            }
            let now = Timestamp::now();
            lab.created_at =
                existing(health.get_lab(lab.id))?.map_or(now, |stored| stored.created_at);
            lab.updated_at = now;
            health.put_labs(std::slice::from_ref(&lab))?;
            Ok(lab.clone())
        })?;
        self.wrote(RecordKind::LabResult, saved.id);
        Ok(saved)
    }

    pub fn delete_lab(&self, id: LabResultId) -> Result<()> {
        self.writable()?;
        self.with_health(|h| h.delete_lab(id))?;
        self.wrote(RecordKind::LabResult, id);
        Ok(())
    }

    /// Results grouped by test, most recently measured test first.
    pub fn lab_trends(&self) -> Result<Vec<LabTrend>> {
        Ok(trends(self.with_health(|h| h.list_labs())?))
    }

    // ---- documents ------------------------------------------------------

    pub fn list_health_documents(&self) -> Result<Vec<HealthDocument>> {
        self.with_health(|h| h.list_documents())
    }

    pub fn get_health_document(&self, id: HealthDocumentId) -> Result<HealthDocument> {
        self.with_health(|h| h.get_document(id))
    }

    /// A new document of files already in the files catalog (the interface
    /// adds them through Files first, sealed, as Health's). Refuses a file
    /// the catalog does not hold or holds in the trash. Each file's name,
    /// type and size are the catalog's, not the caller's.
    pub fn add_health_document(
        &self,
        title: &str,
        kind: DocKind,
        date: Option<Date>,
        files: &[FileId],
    ) -> Result<HealthDocument> {
        self.writable()?;
        let title = name_text("a document's title", title)?;
        if files.len() > MAX_FILES {
            return Err(Error::Invalid(format!("a document can have at most {MAX_FILES} files")));
        }
        let files: Vec<DocFile> = self
            .health_files(files)?
            .into_iter()
            .map(|entry| DocFile {
                file: entry.id,
                name: entry.name,
                mime: entry.mime,
                size: entry.size,
            })
            .collect();
        let document = self.write(|u| {
            let health = health_of(u.store.as_ref())?;
            let now = Timestamp::now();
            let document = HealthDocument {
                id: HealthDocumentId::new(),
                title,
                kind,
                date,
                provider: String::new(),
                files,
                source: None,
                text: String::new(),
                fhir: Vec::new(),
                extraction: ExtractionState::None,
                extraction_error: String::new(),
                pending_labs: Vec::new(),
                condition_ids: Vec::new(),
                external_id: String::new(),
                notes: String::new(),
                created_at: now,
                updated_at: now,
            };
            health.put_document(&document)?;
            Ok(document)
        })?;
        self.track_file_refs(
            RefOwner::new(RefKind::HealthDocument, document.id),
            document.files.iter().map(|f| f.file),
        );
        self.wrote(RecordKind::HealthDocument, document.id);
        Ok(document)
    }

    /// Save what a person may edit: title, kind, date, provider, notes,
    /// conditions, and the pending labs as they reviewed them. Files, text,
    /// FHIR summary, extraction state and the source's own id are kept as
    /// stored.
    pub fn save_health_document(&self, document: &HealthDocument) -> Result<HealthDocument> {
        self.writable()?;
        let title = name_text("a document's title", &document.title)?;
        let provider = person_text("a provider", &document.provider, MAX_NAME)?;
        let notes = person_text("a note", &document.notes, MAX_NOTES)?;
        let mut condition_ids = document.condition_ids.clone();
        dedupe(&mut condition_ids);
        let pending = review_ready(document.id, document.pending_labs.clone())?;
        let saved = self.write(|u| {
            let health = health_of(u.store.as_ref())?;
            existing_conditions(health, &condition_ids)?;
            let mut stored = health.get_document(document.id)?;
            stored.title = title;
            stored.kind = document.kind;
            stored.date = document.date;
            stored.provider = provider;
            stored.notes = notes;
            stored.condition_ids = condition_ids;
            stored.pending_labs = pending;
            stored.updated_at = Timestamp::now();
            health.put_document(&stored)?;
            Ok(stored)
        })?;
        self.wrote(RecordKind::HealthDocument, saved.id);
        Ok(saved)
    }

    /// What extraction found (or that it failed): text, state, pending labs.
    ///
    /// The model's words are an outsider's: the text is stripped of
    /// invisible characters and capped, and a result that fails the lab
    /// checks is left out of the pending list rather than failing the
    /// whole reading.
    pub fn set_document_extraction(
        &self,
        id: HealthDocumentId,
        state: ExtractionState,
        text: Option<String>,
        pending: Vec<LabResult>,
        error: &str,
    ) -> Result<HealthDocument> {
        self.writable()?;
        let pending = extracted_labs(id, pending);
        let error = outside_text(error, MAX_NOTES);
        let saved = self.write(|u| {
            let health = health_of(u.store.as_ref())?;
            let mut document = health.get_document(id)?;
            document.extraction = state;
            if let Some(text) = text {
                document.text = document_text(&text, MAX_DOCUMENT_TEXT);
            }
            document.pending_labs = pending;
            document.extraction_error = error;
            document.updated_at = Timestamp::now();
            health.put_document(&document)?;
            Ok(document)
        })?;
        self.wrote(RecordKind::HealthDocument, saved.id);
        Ok(saved)
    }

    /// Save these results (as reviewed) as lab results pointing at the
    /// document, and clear them from its pending list.
    ///
    /// A result whose id is another stored result's, or repeats within the
    /// batch, gets a fresh one rather than writing over it; one whose id is
    /// a result already accepted from this same document is that result
    /// again, and replaces it, so a second click does not file it twice.
    /// The document is `Done` once nothing is left pending.
    pub fn accept_document_labs(
        &self,
        id: HealthDocumentId,
        labs: &[LabResult],
    ) -> Result<Vec<LabResult>> {
        self.writable()?;
        if labs.len() > MAX_LABS {
            return Err(Error::Invalid(format!("at most {MAX_LABS} results can be saved at once")));
        }
        let mut accepted = labs.to_vec();
        for lab in &mut accepted {
            clean_lab(lab)?;
        }
        let saved = self.write(|u| {
            let health = health_of(u.store.as_ref())?;
            let mut document = health.get_document(id)?;
            let now = Timestamp::now();
            let reviewed: HashSet<LabResultId> = accepted.iter().map(|l| l.id).collect();
            let mut taken: HashSet<LabResultId> = HashSet::new();
            for lab in &mut accepted {
                lab.created_at = now;
                if !taken.insert(lab.id) {
                    lab.id = LabResultId::new();
                    taken.insert(lab.id);
                } else if let Some(stored) = existing(health.get_lab(lab.id))? {
                    if stored.document_id == Some(id) {
                        lab.created_at = stored.created_at;
                    } else {
                        lab.id = LabResultId::new();
                        taken.insert(lab.id);
                    }
                }
                lab.document_id = Some(id);
                if lab.origin != LabOrigin::Fhir {
                    lab.origin = LabOrigin::Extracted;
                }
                lab.updated_at = now;
            }
            if !accepted.is_empty() {
                health.put_labs(&accepted)?;
            }
            document.pending_labs.retain(|p| !reviewed.contains(&p.id));
            if document.pending_labs.is_empty()
                && (document.extraction == ExtractionState::Proposed || !accepted.is_empty())
            {
                document.extraction = ExtractionState::Done;
            }
            document.updated_at = now;
            health.put_document(&document)?;
            Ok(accepted)
        })?;
        for lab in &saved {
            self.wrote(RecordKind::LabResult, lab.id);
        }
        self.wrote(RecordKind::HealthDocument, id);
        Ok(saved)
    }

    /// Delete it; with `delete_labs`, the results accepted from it too
    /// (otherwise they lose the reference). Its pages go to the Files trash
    /// unless something else refers to them (see
    /// [`Vault::release_health_files`]); what a connector sent goes at the
    /// next sweep.
    ///
    /// The results are dealt with before the document goes, so a failure
    /// part-way leaves a document that is still there rather than results
    /// pointing at one that is not.
    pub fn delete_health_document(&self, id: HealthDocumentId, delete_labs: bool) -> Result<()> {
        self.writable()?;
        let (deleted, detached, pages) = self.write(|u| {
            let health = health_of(u.store.as_ref())?;
            let pages: Vec<FileId> =
                health.get_document(id)?.files.iter().map(|f| f.file).collect();
            let now = Timestamp::now();
            let mut deleted = Vec::new();
            let mut detached = Vec::new();
            for mut lab in health.list_labs()? {
                if lab.document_id != Some(id) {
                    continue;
                }
                if delete_labs {
                    health.delete_lab(lab.id)?;
                    deleted.push(lab.id);
                } else {
                    lab.document_id = None;
                    lab.updated_at = now;
                    detached.push(lab);
                }
            }
            if !detached.is_empty() {
                health.put_labs(&detached)?;
            }
            health.delete_document(id)?;
            Ok((deleted, detached.into_iter().map(|l| l.id).collect::<Vec<_>>(), pages))
        })?;
        self.drop_file_refs(RefOwner::new(RefKind::HealthDocument, id));
        self.release_health_files(&pages);
        self.wrote(RecordKind::HealthDocument, id);
        for lab in deleted.iter().chain(&detached) {
            self.wrote(RecordKind::LabResult, lab);
        }
        Ok(())
    }

    /// One of a document's pages, whole, for the model to read. Refuses a
    /// file the document does not list, and one larger than
    /// `MAX_PAGE_BYTES`: what is read here is held in memory.
    pub fn health_document_file(&self, id: HealthDocumentId, file: FileId) -> Result<Vec<u8>> {
        let document = self.with_health(|h| h.get_document(id))?;
        if !document.files.iter().any(|f| f.file == file) {
            return Err(Error::Invalid("that file is not one of this document's".into()));
        }
        self.health_file_bytes(file)
    }

    /// A file Health keeps, whole: a document's page or a meal's photograph,
    /// for the model. Refuses a file that is not Health's (see
    /// [`Vault::health_files`] for why), and one larger than
    /// `MAX_PAGE_BYTES`.
    pub fn health_file_bytes(&self, file: FileId) -> Result<Vec<u8>> {
        self.health_files(&[file])?;
        let len = self.file_len(file)?;
        if len > MAX_PAGE_BYTES {
            return Err(Error::Invalid(format!(
                "that file is {} MB; at most {} MB can be read",
                len / 1_048_576,
                MAX_PAGE_BYTES / 1_048_576
            )));
        }
        self.read_file_range(file, 0, len)
    }

    // ---- foods and meals ------------------------------------------------

    /// Foods whose name, brand or barcode contains `query` (case-folded);
    /// all of them, most recently used first, for an empty one.
    pub fn list_foods(&self, query: &str) -> Result<Vec<Food>> {
        let needle = query.trim().to_lowercase();
        self.with_health(|h| {
            let mut foods = h.list_foods()?;
            if !needle.is_empty() {
                foods.retain(|food| {
                    [&food.name, &food.brand, &food.barcode]
                        .iter()
                        .any(|field| field.to_lowercase().contains(&needle))
                });
            }
            Ok(foods)
        })
    }

    /// Insert or update. How often and when a food was last used belongs to
    /// the meals that named it, so an edit keeps the stored count, and a new
    /// food starts at none whatever the caller sent.
    pub fn save_food(&self, food: &Food) -> Result<Food> {
        self.writable()?;
        let mut food = food.clone();
        clean_food(&mut food)?;
        let saved = self.write(|u| {
            let health = health_of(u.store.as_ref())?;
            let now = Timestamp::now();
            match existing(health.get_food(food.id))? {
                Some(stored) => {
                    food.created_at = stored.created_at;
                    food.last_used = stored.last_used;
                    food.use_count = stored.use_count;
                }
                None => {
                    food.created_at = now;
                    food.last_used = None;
                    food.use_count = 0;
                }
            }
            food.updated_at = now;
            health.put_food(&food)?;
            Ok(food.clone())
        })?;
        self.wrote(RecordKind::Food, saved.id);
        Ok(saved)
    }

    pub fn delete_food(&self, id: FoodId) -> Result<()> {
        self.writable()?;
        self.with_health(|h| h.delete_food(id))?;
        self.wrote(RecordKind::Food, id);
        Ok(())
    }

    pub fn list_meals(&self, from: Timestamp, to: Timestamp) -> Result<Vec<Meal>> {
        self.with_health(|h| h.meals(from, to))
    }

    /// Insert or update. Each food it names has its last use and count
    /// moved on -- the last use to the meal's time if that is later, the
    /// count by one -- but only for a food the meal did not name before, so
    /// a new meal counts all its foods and an edit counts only the ones it
    /// adds. A food that no longer exists is skipped.
    pub fn save_meal(&self, meal: &Meal) -> Result<Meal> {
        self.writable()?;
        let mut meal = meal.clone();
        meal.name = person_text("a meal's name", &meal.name, MAX_NAME)?;
        meal.note = person_text("a note", &meal.note, MAX_NOTES)?;
        if meal.items.len() > MAX_ITEMS {
            return Err(Error::Invalid(format!("a meal can have at most {MAX_ITEMS} foods")));
        }
        if meal.offset_min.unsigned_abs() > MAX_OFFSET_MIN {
            return Err(Error::Invalid("a time zone is at most 18 hours from UTC".into()));
        }
        for item in &mut meal.items {
            clean_item(item)?;
        }
        // Only a photo new to the meal is checked: one it already carries
        // may since have been put in the trash in Files, and that is no
        // reason to refuse fixing a typo in its name.
        if let Some(photo) = meal.photo {
            let carried =
                self.with_health(|h| Ok(existing(h.get_meal(meal.id))?.and_then(|m| m.photo)))?;
            if carried != Some(photo) {
                self.health_files(&[photo])?;
            }
        }
        let (saved, used, before_photo) = self.write(|u| {
            let health = health_of(u.store.as_ref())?;
            let now = Timestamp::now();
            let stored = existing(health.get_meal(meal.id))?;
            let before_photo = stored.as_ref().and_then(|s| s.photo);
            meal.created_at = stored.as_ref().map_or(now, |s| s.created_at);
            meal.updated_at = now;
            let before: HashSet<FoodId> =
                stored.iter().flat_map(|s| &s.items).filter_map(|i| i.food_id).collect();
            let named: BTreeSet<FoodId> = meal
                .items
                .iter()
                .filter_map(|i| i.food_id)
                .filter(|f| !before.contains(f))
                .collect();
            health.put_meal(&meal)?;
            let mut used = Vec::new();
            for id in named {
                let Some(mut food) = existing(health.get_food(id))? else { continue };
                if food.last_used.is_none_or(|last| last < meal.at) {
                    food.last_used = Some(meal.at);
                }
                food.use_count = food.use_count.saturating_add(1);
                health.put_food(&food)?;
                used.push(id);
            }
            Ok((meal.clone(), used, before_photo))
        })?;
        self.track_file_refs(RefOwner::new(RefKind::Meal, saved.id), saved.photo);
        if let Some(old) = before_photo.filter(|old| saved.photo != Some(*old)) {
            self.release_health_files(&[old]);
        }
        self.wrote(RecordKind::Meal, saved.id);
        for food in &used {
            self.wrote(RecordKind::Food, food);
        }
        Ok(saved)
    }

    /// Delete it. Its photograph goes to the Files trash unless something
    /// else refers to it (see [`Vault::release_health_files`]).
    pub fn delete_meal(&self, id: MealId) -> Result<()> {
        self.writable()?;
        let photo = self.with_health(|h| {
            let photo = existing(h.get_meal(id))?.and_then(|m| m.photo);
            h.delete_meal(id)?;
            Ok(photo)
        })?;
        self.drop_file_refs(RefOwner::new(RefKind::Meal, id));
        self.release_health_files(&photo.into_iter().collect::<Vec<_>>());
        self.wrote(RecordKind::Meal, id);
        Ok(())
    }

    /// Each local day of `from..=to`: what was logged here, and what other
    /// apps wrote to the phone (each dietary metric's daily sum).
    pub fn health_nutrition(&self, from: Date, to: Date, tz: &str) -> Result<Vec<NutritionDay>> {
        range_days(from, to)?;
        let zone = zone_of(tz)?;
        let (start, end) = day_bounds(from, to, &zone);
        // A day either side, so a meal near midnight is read whichever way
        // the local day falls; `nutrition::days` cuts them to the local days.
        let margin = SignedDuration::from_secs(24 * 3600);
        let (from_t, to_t) =
            (start.checked_sub(margin).unwrap_or(start), end.checked_add(margin).unwrap_or(end));
        let meals = self.with_health(|h| h.meals(from_t, to_t))?;
        let mut imported = BTreeMap::new();
        for metric in nutrition::DIETARY_METRICS {
            imported.insert(*metric, self.health_daily(*metric, Stat::Sum, from, to, tz)?);
        }
        Ok(nutrition::days(&meals, &imported, from, to, &zone))
    }

    /// What glucose did after a meal: the readings from half an hour before
    /// it to three hours after.
    pub fn meal_response(&self, id: MealId) -> Result<MealResponse> {
        let meal = self.with_health(|h| h.get_meal(id))?;
        let before = SignedDuration::from_secs(30 * 60);
        let after = SignedDuration::from_secs(3 * 3600);
        let from = meal.at.checked_sub(before).unwrap_or(meal.at);
        let to = meal.at.checked_add(after).unwrap_or(meal.at);
        let points = self.health_samples(Metric::BloodGlucose, from, to)?;
        Ok(glucose::meal_response(&meal, &points))
    }

    // ---- a visit --------------------------------------------------------

    /// The numbers a visit brief is written from, `since..=until`, for one
    /// condition or all of the active ones. Medications are the active ones;
    /// changes are every medication's, stopped or not, dated inside the
    /// range. Glucose is there only when there were readings; the weight
    /// change is the last day's weight less the first day's, and needs two
    /// days with one.
    pub fn visit_facts(
        &self,
        condition: Option<ConditionId>,
        since: Date,
        until: Date,
        tz: &str,
    ) -> Result<VisitFacts> {
        range_days(since, until)?;
        zone_of(tz)?;
        let ids: Vec<ConditionId> = match condition {
            Some(id) => vec![id],
            None => self
                .with_health(|h| h.list_conditions())?
                .into_iter()
                .filter(|c| c.status == ConditionStatus::Active)
                .map(|c| c.id)
                .collect(),
        };
        let mut conditions = Vec::with_capacity(ids.len());
        for id in ids {
            conditions.push(self.condition_report(id, until, tz)?);
        }
        let (medications, medication_changes, labs, allergies) = self.with_health(|h| {
            let all = h.list_medications()?;
            let mut changes: Vec<(String, DoseChange)> = Vec::new();
            for medication in &all {
                for change in &medication.history {
                    if change.on >= since && change.on <= until {
                        changes.push((medication.name.clone(), change.clone()));
                    }
                }
            }
            changes.sort_by(|a, b| a.1.on.cmp(&b.1.on).then_with(|| a.0.cmp(&b.0)));
            let active: Vec<Medication> =
                all.into_iter().filter(|m| m.status == MedStatus::Active).collect();
            let labs: Vec<LabResult> = h
                .list_labs()?
                .into_iter()
                .filter(|l| l.collected >= since && l.collected <= until)
                .collect();
            Ok((active, changes, labs, h.list_allergies()?))
        })?;
        let glucose = Some(self.health_glucose(since, until, tz)?).filter(|g| g.readings > 0);
        let latest = self.health_latest(VITALS, until)?;
        let weights = self.health_daily(Metric::Weight, Stat::Auto, since, until, tz)?;
        let weight_change_kg = match (weights.values().next(), weights.values().next_back()) {
            (Some(first), Some(last)) if weights.len() > 1 => Some(last - first),
            _ => None,
        };
        let nights = self.health_sleep(since, until, tz)?;
        let mean_sleep_min =
            mean(nights.iter().map(|n| n.asleep_min).filter(|minutes| *minutes > 0.0));
        let steps = self.health_daily(Metric::Steps, Stat::Sum, since, until, tz)?;
        let mean_steps = mean(steps.into_values());
        Ok(VisitFacts {
            since,
            until,
            conditions,
            medications,
            medication_changes,
            labs,
            allergies,
            glucose,
            latest,
            weight_change_kg,
            mean_sleep_min,
            mean_steps,
        })
    }
}

// ---- reading around a range ----------------------------------------------

/// The doses a range's medications were answered by, and how steadily.
fn adherence_in(
    health: &dyn HealthStore,
    medications: &[Medication],
    from: Date,
    to: Date,
    zone: &TimeZone,
    now: Timestamp,
) -> Result<Adherence> {
    if medications.is_empty() {
        return Ok(Adherence::default());
    }
    let (start, end) = day_bounds(from, to, zone);
    let (a, b) = padded(start, end);
    let ids: HashSet<MedicationId> = medications.iter().map(|m| m.id).collect();
    let doses: Vec<Dose> = shown(health.doses(a, b, None)?)
        .into_iter()
        .filter(|d| ids.contains(&d.medication_id))
        .collect();
    Ok(schedule::adherence(medications, &doses, from, to, zone, now))
}

/// The timing rules broken in `from..=to`. The doses and meals are read a
/// little wider than the range, so a rule across midnight is seen, and the
/// alerts cut back to the range afterwards.
fn alerts_in(
    health: &dyn HealthStore,
    medications: &[Medication],
    from: Date,
    to: Date,
    zone: &TimeZone,
) -> Result<Vec<RuleAlert>> {
    // Only a medication's own rules raise an alert, so with none there is
    // nothing to read doses and meals for.
    if medications.iter().all(|m| m.rules.is_empty()) {
        return Ok(Vec::new());
    }
    let (start, end) = day_bounds(from, to, zone);
    let (a, b) = padded(start, end);
    let doses = shown(health.doses(a, b, None)?);
    let meals = health.meals(a, b)?;
    Ok(rules::check(medications, &doses, &meals)
        .into_iter()
        .filter(|alert| alert.at >= start && alert.at < end)
        .collect())
}

/// Where a value stands against a target's bounds.
fn judge(value: f64, target: &CareTarget) -> &'static str {
    if target.low.is_none() && target.high.is_none() {
        return "unknown";
    }
    if target.low.is_some_and(|low| value < low) || target.high.is_some_and(|high| value > high) {
        "notMet"
    } else {
        "met"
    }
}

fn mean(values: impl IntoIterator<Item = f64>) -> Option<f64> {
    let (sum, count) =
        values.into_iter().filter(|v| v.is_finite()).fold((0.0, 0u32), |(s, n), v| (s + v, n + 1));
    (count > 0).then(|| sum / f64::from(count))
}

// ---- time ----------------------------------------------------------------

/// Today, where this computer is: the day a dose change is dated.
fn today() -> Date {
    jiff::Zoned::now().date()
}

/// A time zone by IANA name; empty is the system's.
fn zone_of(tz: &str) -> Result<TimeZone> {
    let tz = tz.trim();
    if tz.is_empty() {
        return Ok(TimeZone::system());
    }
    TimeZone::get(tz).map_err(|_| {
        Error::Invalid(format!("{tz:?} is not a time zone; name one like Europe/London"))
    })
}

/// The instants `from` begins and the day after `to` begins, in `zone`.
fn day_bounds(from: Date, to: Date, zone: &TimeZone) -> (Timestamp, Timestamp) {
    let start = zone.to_timestamp(from.at(0, 0, 0, 0)).unwrap_or(Timestamp::MIN);
    let end = to
        .tomorrow()
        .ok()
        .and_then(|day| zone.to_timestamp(day.at(0, 0, 0, 0)).ok())
        .unwrap_or(Timestamp::MAX);
    (start, end)
}

/// A range's bounds moved out by [`MARGIN_SECS`] each way.
fn padded(start: Timestamp, end: Timestamp) -> (Timestamp, Timestamp) {
    let margin = SignedDuration::from_secs(MARGIN_SECS);
    (start.checked_sub(margin).unwrap_or(start), end.checked_add(margin).unwrap_or(end))
}

fn days_before(date: Date, days: i64) -> Date {
    date.checked_sub(Span::new().days(days)).unwrap_or(date)
}

/// Refuses a range that runs backwards or is longer than a report reads.
fn range_days(from: Date, to: Date) -> Result<()> {
    if from > to {
        return Err(Error::Invalid("a range's last day cannot be before its first".into()));
    }
    let days =
        from.until(to).map(|s| i64::from(s.get_days())).unwrap_or(i64::MAX).saturating_add(1);
    if days > MAX_RANGE_DAYS {
        return Err(Error::Invalid(format!(
            "a health report covers at most {MAX_RANGE_DAYS} days"
        )));
    }
    Ok(())
}

// ---- stores --------------------------------------------------------------

/// The health store, or the usual explanation that this backend has none.
/// Doses with the ones a person deleted (hidden source events) left out.
pub(super) fn shown(doses: Vec<Dose>) -> Vec<Dose> {
    doses.into_iter().filter(|d| !d.hidden).collect()
}

fn health_of(store: &dyn JournalStore) -> Result<&dyn HealthStore> {
    pick_domain(store, Domain::Health, |s| s.health())
}

/// `None` for a record that is not there, so a save can tell an edit from a
/// new one; any other failure is the answer.
fn existing<T>(found: Result<T>) -> Result<Option<T>> {
    match found {
        Ok(record) => Ok(Some(record)),
        Err(Error::NotFound { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Every condition named must be there. One that is not is the caller's
/// mistake, not a missing page.
fn existing_conditions(health: &dyn HealthStore, ids: &[ConditionId]) -> Result<()> {
    for id in ids {
        if existing(health.get_condition(*id))?.is_none() {
            return Err(Error::Invalid(
                "that condition does not exist; it may have been deleted".into(),
            ));
        }
    }
    Ok(())
}

// ---- matching a source's doses ---------------------------------------------

/// The medication a source's dose event is about, by any code equal (system
/// and code), then by a name this source used before, then by the
/// medication's own name -- alone or with its strength -- all
/// case-insensitive.
fn match_medication(medications: &[Medication], raw: &RawDose) -> Option<usize> {
    for code in &raw.codes {
        let (system, value) = (code.system.trim(), code.code.trim());
        if value.is_empty() {
            continue;
        }
        let found = medications.iter().position(|m| {
            m.codes
                .iter()
                .any(|c| c.system.trim().eq_ignore_ascii_case(system) && c.code.trim() == value)
        });
        if found.is_some() {
            return found;
        }
    }
    let name = raw.name.trim().to_lowercase();
    if name.is_empty() {
        return None;
    }
    medications
        .iter()
        .position(|m| m.source_names.iter().any(|n| n.trim().to_lowercase() == name))
        .or_else(|| medications.iter().position(|m| is_own_name(m, &name)))
}

/// Whether `name` is what the medication is called itself: its name, or its
/// name and strength ("Metformin 500 mg"), in any case.
fn is_own_name(medication: &Medication, name: &str) -> bool {
    let name = name.trim().to_lowercase();
    let own = medication.name.trim().to_lowercase();
    let strength = medication.strength.trim().to_lowercase();
    own == name || (!strength.is_empty() && format!("{own} {strength}") == name)
}

// ---- conditions and templates ---------------------------------------------

/// The symptom names a person ticked: trimmed, each once (case-folded), at
/// most [`MAX_ITEMS`].
fn symptom_names(names: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        let name = person_text("a symptom's name", name, MAX_NAME)?;
        if !name.is_empty() && !out.iter().any(|o| o.to_lowercase() == name.to_lowercase()) {
            out.push(name);
        }
    }
    if out.len() > MAX_ITEMS {
        return Err(Error::Invalid(format!("at most {MAX_ITEMS} symptoms can be tracked at once")));
    }
    Ok(out)
}

/// Make the symptom trackers -- a 0-10 scale each -- and save the
/// condition with their ids on its plan. `made` names every tracker created
/// so far, so a caller whose condition could not be saved can take them back.
fn create_condition(
    store: &dyn JournalStore,
    condition: &mut Condition,
    names: &[String],
    made: &mut Vec<TrackerId>,
) -> Result<()> {
    let health = health_of(store)?;
    if !names.is_empty() {
        let trackers = pick_domain(store, Domain::Trackers, |s| s.trackers())?;
        for (n, name) in names.iter().enumerate() {
            let mut tracker = Tracker::new(name.clone(), TrackerKind::Scale);
            tracker.icon = "pulse".into();
            tracker.color = TRACKER_COLORS[n % TRACKER_COLORS.len()].to_string();
            tracker.normalize();
            trackers.put_tracker(&tracker)?;
            made.push(tracker.id);
        }
    }
    condition.plan.tracker_ids = made.clone();
    health.put_condition(condition)
}

/// Best effort: trackers that were made for a condition that was not.
fn discard_trackers(store: &dyn JournalStore, made: &[TrackerId]) {
    if let Ok(trackers) = pick_domain(store, Domain::Trackers, |s| s.trackers()) {
        for id in made {
            let _ = trackers.delete_tracker(*id);
        }
    }
}

// ---- labs ----------------------------------------------------------------

/// What results are grouped by: the LOINC code, or `name:` and the name
/// lower-cased for one without.
fn lab_key(lab: &LabResult) -> String {
    let code = lab.code.trim();
    if code.is_empty() {
        format!("name:{}", lab.name.trim().to_lowercase())
    } else {
        code.to_string()
    }
}

/// A key a caller typed, in the form [`lab_key`] makes.
fn canonical_key(key: &str) -> String {
    let key = key.trim();
    match key.strip_prefix("name:") {
        Some(name) => format!("name:{}", name.trim().to_lowercase()),
        None => key.to_string(),
    }
}

/// Group results by test, each group oldest first, the groups ordered by
/// their latest result, newest first. A group's name and unit are its
/// latest result's: a lab that changes how it prints a test changes it for
/// the whole line.
fn trends(mut labs: Vec<LabResult>) -> Vec<LabTrend> {
    labs.sort_by(|a, b| {
        (a.collected, a.collected_at, a.id).cmp(&(b.collected, b.collected_at, b.id))
    });
    let mut groups: BTreeMap<String, Vec<LabResult>> = BTreeMap::new();
    for lab in labs {
        groups.entry(lab_key(&lab)).or_default().push(lab);
    }
    let mut trends: Vec<LabTrend> = groups
        .into_iter()
        .map(|(key, results)| {
            let (name, unit) =
                results.last().map(|l| (l.name.clone(), l.unit.clone())).unwrap_or_default();
            LabTrend { key, name, unit, results }
        })
        .collect();
    let latest = |t: &LabTrend| t.results.last().map(|l| (l.collected, l.collected_at, l.id));
    trends.sort_by(|a, b| latest(b).cmp(&latest(a)).then_with(|| a.key.cmp(&b.key)));
    trends
}

/// Pending results as a person reviewed them: each checked (a failure is
/// theirs to fix, so it is answered), pointing at the document, and none
/// sharing an id.
fn review_ready(document: HealthDocumentId, labs: Vec<LabResult>) -> Result<Vec<LabResult>> {
    if labs.len() > MAX_LABS {
        return Err(Error::Invalid(format!("a document can wait on at most {MAX_LABS} results")));
    }
    let mut seen: HashSet<LabResultId> = HashSet::new();
    let mut out = Vec::with_capacity(labs.len());
    for mut lab in labs {
        clean_lab(&mut lab)?;
        lab.document_id = Some(document);
        if !seen.insert(lab.id) {
            lab.id = LabResultId::new();
            seen.insert(lab.id);
        }
        out.push(lab);
    }
    Ok(out)
}

/// Pending results as an extraction made them: the model's output, so one
/// that fails the checks is left out rather than refusing the rest, each
/// with a fresh id and the document's, read out of it.
fn extracted_labs(document: HealthDocumentId, labs: Vec<LabResult>) -> Vec<LabResult> {
    let now = Timestamp::now();
    labs.into_iter()
        .filter_map(|mut lab| {
            clean_lab(&mut lab).ok()?;
            lab.id = LabResultId::new();
            lab.document_id = Some(document);
            if lab.origin == LabOrigin::Manual {
                lab.origin = LabOrigin::Extracted;
            }
            lab.created_at = now;
            lab.updated_at = now;
            Some(lab)
        })
        .take(MAX_LABS)
        .collect()
}

// ---- checking what a person typed --------------------------------------------

/// A string a person typed, trimmed and no longer than `max` characters.
fn person_text(what: &str, text: &str, max: usize) -> Result<String> {
    let trimmed = text.trim();
    if trimmed.chars().count() > max {
        return Err(Error::Invalid(format!("{what} can be at most {max} characters")));
    }
    Ok(trimmed.to_string())
}

/// As [`person_text`] for a name: required, and at most [`MAX_NAME`].
fn name_text(what: &str, text: &str) -> Result<String> {
    let name = person_text(what, text, MAX_NAME)?;
    if name.is_empty() {
        return Err(Error::Invalid(format!("{what} cannot be blank")));
    }
    Ok(name)
}

fn number(what: &str, value: f64) -> Result<f64> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Error::Invalid(format!("{what} must be a number")))
    }
}

fn positive(what: &str, value: f64) -> Result<f64> {
    if number(what, value)? > 0.0 {
        Ok(value)
    } else {
        Err(Error::Invalid(format!("{what} must be above zero")))
    }
}

fn not_negative(what: &str, value: f64) -> Result<f64> {
    if number(what, value)? >= 0.0 {
        Ok(value)
    } else {
        Err(Error::Invalid(format!("{what} cannot be negative")))
    }
}

fn opt_number(what: &str, value: Option<f64>) -> Result<Option<f64>> {
    value.map(|v| number(what, v)).transpose()
}

fn opt_not_negative(what: &str, value: Option<f64>) -> Result<Option<f64>> {
    value.map(|v| not_negative(what, v)).transpose()
}

/// Each id once, in the order first seen.
fn dedupe<T: Copy + Eq + std::hash::Hash>(ids: &mut Vec<T>) {
    let mut seen = HashSet::new();
    ids.retain(|id| seen.insert(*id));
}

/// Tags as rules match them: trimmed and lower-cased, each once.
fn clean_tags(what: &str, tags: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        let tag = person_text(what, tag, MAX_SHORT)?.to_lowercase();
        if !tag.is_empty() && !out.contains(&tag) {
            out.push(tag);
        }
    }
    if out.len() > MAX_TAGS {
        return Err(Error::Invalid(format!("at most {MAX_TAGS} tags can be set")));
    }
    Ok(out)
}

fn clean_codes(codes: &mut Vec<Code>) -> Result<()> {
    for code in codes.iter_mut() {
        code.system = person_text("a code's system", &code.system, MAX_NAME)?;
        code.code = person_text("a code", &code.code, MAX_SHORT)?;
        code.display = person_text("a code's name", &code.display, MAX_NAME)?;
    }
    codes.retain(|c| !c.code.is_empty());
    if codes.len() > MAX_CODES {
        return Err(Error::Invalid(format!("at most {MAX_CODES} codes can be set")));
    }
    Ok(())
}

fn clean_times(times: &mut Vec<Time>) -> Result<()> {
    times.sort();
    times.dedup();
    if times.len() > MAX_TIMES {
        return Err(Error::Invalid(format!("a schedule can have at most {MAX_TIMES} times a day")));
    }
    Ok(())
}

fn clean_schedule(schedule: &mut MedSchedule) -> Result<()> {
    match schedule {
        MedSchedule::Daily { times } => clean_times(times),
        MedSchedule::Weekly { days, times } => {
            if days.is_empty() || days.iter().any(|d| !(1..=7).contains(d)) {
                return Err(Error::Invalid(
                    "a weekly schedule needs weekdays from 1 (Monday) to 7 (Sunday)".into(),
                ));
            }
            days.sort_unstable();
            days.dedup();
            clean_times(times)
        }
        MedSchedule::EveryNDays { n, times, .. } => {
            if *n == 0 {
                return Err(Error::Invalid(
                    "a schedule every few days needs at least one day between doses".into(),
                ));
            }
            clean_times(times)
        }
        MedSchedule::AsNeeded => Ok(()),
    }
}

fn clean_rules(rules: &mut [MedRule]) -> Result<()> {
    if rules.len() > MAX_RULES {
        return Err(Error::Invalid(format!("at most {MAX_RULES} timing rules can be set")));
    }
    for rule in rules.iter_mut() {
        match rule {
            MedRule::EmptyStomach { minutes } | MedRule::WithFood { minutes } => {
                if *minutes > MAX_RULE_MINUTES {
                    return Err(Error::Invalid("a timing rule cannot be longer than a day".into()));
                }
            }
            MedRule::ApartFrom { tags: wanted, hours } => {
                *wanted = clean_tags("a rule's tag", wanted)?;
                if wanted.is_empty() {
                    return Err(Error::Invalid(
                        "a rule about keeping apart needs at least one tag".into(),
                    ));
                }
                number("the hours between doses", *hours)?;
                if *hours < 0.0 || *hours > MAX_RULE_HOURS {
                    return Err(Error::Invalid(format!(
                        "a rule can keep doses apart by up to {MAX_RULE_HOURS} hours"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn clean_medication(medication: &mut Medication) -> Result<()> {
    medication.name = name_text("a medication's name", &medication.name)?;
    medication.form = person_text("a form", &medication.form, MAX_NAME)?;
    medication.strength = person_text("a strength", &medication.strength, MAX_NAME)?;
    medication.dose = positive("a dose", medication.dose)?;
    medication.dose_unit = person_text("a dose's unit", &medication.dose_unit, MAX_SHORT)?;
    clean_schedule(&mut medication.schedule)?;
    clean_rules(&mut medication.rules)?;
    medication.tags = clean_tags("a tag", &medication.tags)?;
    medication.stop_reason =
        person_text("a reason for stopping", &medication.stop_reason, MAX_NOTES)?;
    medication.prescriber = person_text("a prescriber", &medication.prescriber, MAX_NAME)?;
    medication.pharmacy = person_text("a pharmacy", &medication.pharmacy, MAX_NAME)?;
    medication.notes = person_text("a note", &medication.notes, MAX_NOTES)?;
    for change in &mut medication.history {
        change.dose = not_negative("a dose", change.dose)?;
        change.strength = person_text("a strength", &change.strength, MAX_NAME)?;
        change.note = person_text("a note", &change.note, MAX_NOTES)?;
    }
    medication.history.sort_by_key(|c| c.on);
    if let Some(supply) = &mut medication.supply {
        supply.remaining = not_negative("a supply", supply.remaining)?;
    }
    clean_codes(&mut medication.codes)?;
    let mut names: Vec<String> = Vec::new();
    for name in &medication.source_names {
        let name = person_text("a name", name, MAX_NAME)?;
        if !name.is_empty() && !names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
            names.push(name);
        }
    }
    if names.len() > MAX_SOURCE_NAMES {
        return Err(Error::Invalid(format!("at most {MAX_SOURCE_NAMES} source names can be kept")));
    }
    medication.source_names = names;
    dedupe(&mut medication.condition_ids);
    Ok(())
}

fn clean_plan(plan: &mut CarePlan) -> Result<()> {
    if plan.watch.len() > MAX_ITEMS
        || plan.targets.len() > MAX_ITEMS
        || plan.labs.len() > MAX_ITEMS
        || plan.tracker_ids.len() > MAX_ITEMS
    {
        return Err(Error::Invalid(format!(
            "a care plan can hold at most {MAX_ITEMS} of each part"
        )));
    }
    for target in &mut plan.targets {
        target.label = name_text("a target's label", &target.label)?;
        target.note = person_text("a target's note", &target.note, MAX_NOTES)?;
        target.low = opt_number("a target's low end", target.low)?;
        target.high = opt_number("a target's high end", target.high)?;
        if target.window_days > MAX_WINDOW_DAYS {
            return Err(Error::Invalid(format!(
                "a target can be averaged over at most {MAX_WINDOW_DAYS} days"
            )));
        }
        if let Measure::Lab { code } = &mut target.measure {
            *code = person_text("a test's code", code, MAX_SHORT)?;
        }
    }
    for lab in &mut plan.labs {
        lab.code = person_text("a test's code", &lab.code, MAX_SHORT)?;
        if lab.code.is_empty() {
            return Err(Error::Invalid("a scheduled test needs a code".into()));
        }
        lab.name = name_text("a test's name", &lab.name)?;
        lab.after_change_tag =
            person_text("a medication tag", &lab.after_change_tag, MAX_SHORT)?.to_lowercase();
    }
    let mut by_tag: BTreeMap<String, Vec<MedRule>> = BTreeMap::new();
    for (tag, mut list) in std::mem::take(&mut plan.med_rules) {
        let tag = person_text("a medication tag", &tag, MAX_SHORT)?.to_lowercase();
        if tag.is_empty() {
            continue;
        }
        clean_rules(&mut list)?;
        by_tag.insert(tag, list);
    }
    plan.med_rules = by_tag;
    dedupe(&mut plan.tracker_ids);
    Ok(())
}

fn clean_lab(lab: &mut LabResult) -> Result<()> {
    lab.name = name_text("a test's name", &lab.name)?;
    lab.code = person_text("a test's code", &lab.code, MAX_SHORT)?;
    lab.unit = person_text("a unit", &lab.unit, MAX_SHORT)?;
    lab.text = person_text("a result", &lab.text, MAX_NAME)?;
    lab.ref_text = person_text("a reference range", &lab.ref_text, MAX_NAME)?;
    lab.lab = person_text("a lab's name", &lab.lab, MAX_NAME)?;
    lab.panel = person_text("a panel", &lab.panel, MAX_NAME)?;
    lab.note = person_text("a note", &lab.note, MAX_NOTES)?;
    lab.value = opt_number("a result", lab.value)?;
    lab.ref_low = opt_number("a reference range's low end", lab.ref_low)?;
    lab.ref_high = opt_number("a reference range's high end", lab.ref_high)?;
    if lab.value.is_none() && lab.text.is_empty() {
        return Err(Error::Invalid("a result needs a value or some text".into()));
    }
    Ok(())
}

fn clean_nutrients(nutrients: &mut Nutrients) -> Result<()> {
    for (what, value) in [
        ("energy", nutrients.kcal),
        ("protein", nutrients.protein_g),
        ("carbohydrate", nutrients.carbs_g),
        ("fat", nutrients.fat_g),
    ] {
        not_negative(what, value)?;
    }
    for (what, value) in [
        ("fiber", nutrients.fiber_g),
        ("sugar", nutrients.sugar_g),
        ("saturated fat", nutrients.sat_fat_g),
        ("sodium", nutrients.sodium_mg),
    ] {
        opt_not_negative(what, value)?;
    }
    if nutrients.extra.len() > MAX_ITEMS {
        return Err(Error::Invalid(format!("at most {MAX_ITEMS} other nutrients can be kept")));
    }
    let mut extra = BTreeMap::new();
    for (name, value) in std::mem::take(&mut nutrients.extra) {
        let name = person_text("a nutrient's name", &name, MAX_SHORT)?;
        if !name.is_empty() {
            extra.insert(name, not_negative("a nutrient", value)?);
        }
    }
    nutrients.extra = extra;
    Ok(())
}

fn clean_food(food: &mut Food) -> Result<()> {
    food.name = name_text("a food's name", &food.name)?;
    food.brand = person_text("a brand", &food.brand, MAX_NAME)?;
    food.barcode = person_text("a barcode", &food.barcode, MAX_SHORT)?;
    clean_nutrients(&mut food.per_100g)?;
    if food.servings.len() > MAX_ITEMS {
        return Err(Error::Invalid(format!("a food can have at most {MAX_ITEMS} servings")));
    }
    for serving in &mut food.servings {
        serving.name = name_text("a serving's name", &serving.name)?;
        serving.grams = positive("a serving's weight", serving.grams)?;
    }
    Ok(())
}

fn clean_item(item: &mut MealItem) -> Result<()> {
    item.name = name_text("a food's name", &item.name)?;
    item.amount = not_negative("an amount", item.amount)?;
    item.unit = person_text("a unit", &item.unit, MAX_SHORT)?;
    item.grams = opt_not_negative("a weight", item.grams)?;
    clean_nutrients(&mut item.nutrients)
}

// ---- an outsider's words ------------------------------------------------

/// Text from a connector, a file or a model: bidirectional and zero-width
/// characters gone (they make one name read as another), control characters
/// made spaces, trimmed and cut to `max` characters. Kept to a line --
/// see [`document_text`] for the one that keeps its paragraphs.
fn outside_text(text: &str, max: usize) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| !crate::money::text::is_invisible(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    cleaned.trim().chars().take(max).collect()
}

/// A document's text: as [`outside_text`], but a document's line breaks and
/// tabs stay, since a lab report read as one line is no longer a table.
fn document_text(text: &str, max: usize) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| !crate::money::text::is_invisible(*c))
        .map(|c| if c.is_control() && !matches!(c, '\n' | '\t') { ' ' } else { c })
        .collect();
    cleaned.trim().chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::{LabFlag, MedKind};

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
            schedule: MedSchedule::default(),
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

    fn raw(name: &str, codes: Vec<Code>) -> RawDose {
        RawDose {
            name: name.into(),
            codes,
            at: "2026-11-10T08:00:00Z".parse().unwrap(),
            scheduled_at: None,
            status: DoseStatus::Taken,
            amount: None,
            unit: String::new(),
            source: String::new(),
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

    #[test]
    fn a_source_dose_matches_by_code_before_name() {
        let mut thyroid = medication("Levothyroxine");
        thyroid.codes = vec![code("rxnorm", "10582")];
        let mut other = medication("Synthroid");
        other.codes = vec![code("rxnorm", "999")];
        let all = [other, thyroid];
        // The name says the second; the code says the first, and wins.
        let found = match_medication(&all, &raw("Synthroid", vec![code("RxNorm", "10582")]));
        assert_eq!(found, Some(1));
    }

    #[test]
    fn a_source_dose_matches_a_remembered_name_then_the_name_with_or_without_strength() {
        let mut metformin = medication("Metformin");
        metformin.strength = "500 mg".into();
        let mut thyroid = medication("Levothyroxine");
        thyroid.source_names = vec!["Synthroid".into()];
        let all = [metformin, thyroid];
        assert_eq!(match_medication(&all, &raw("SYNTHROID", vec![])), Some(1));
        assert_eq!(match_medication(&all, &raw("metformin", vec![])), Some(0));
        assert_eq!(match_medication(&all, &raw("Metformin 500 MG", vec![])), Some(0));
        assert_eq!(match_medication(&all, &raw("Aspirin", vec![])), None);
        assert_eq!(match_medication(&all, &raw("  ", vec![])), None);
    }

    #[test]
    fn a_dose_key_is_thirty_two_hex_characters_and_depends_on_the_key() {
        let a = SecretKey::from_bytes([1; 32]).keyed_id("source|m|1|taken");
        let b = SecretKey::from_bytes([2; 32]).keyed_id("source|m|1|taken");
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')));
        assert_ne!(a, b);
        assert_eq!(a, SecretKey::from_bytes([1; 32]).keyed_id("source|m|1|taken"));
    }

    #[test]
    fn trends_group_by_code_or_name_and_put_the_newest_test_first() {
        let day = |m: i8, d: i8| Date::constant(2026, m, d);
        let trends = trends(vec![
            lab("4548-4", "A1c", 7.4, day(3, 1), "%"),
            lab("", "Ferritin", 30.0, day(5, 20), "ng/mL"),
            lab("4548-4", "Hemoglobin A1c", 6.8, day(5, 1), "% of Hb"),
            lab("3016-3", "TSH", 2.1, day(5, 10), "mIU/L"),
            lab("", " ferritin ", 41.0, day(6, 2), "ug/L"),
        ]);
        let keys: Vec<&str> = trends.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, ["name:ferritin", "3016-3", "4548-4"]);
        let a1c = &trends[2];
        let dates: Vec<Date> = a1c.results.iter().map(|r| r.collected).collect();
        assert_eq!(dates, [day(3, 1), day(5, 1)]);
        assert_eq!((a1c.name.as_str(), a1c.unit.as_str()), ("Hemoglobin A1c", "% of Hb"));
        assert_eq!(trends[0].unit, "ug/L");
    }

    #[test]
    fn a_lab_key_typed_by_a_person_is_made_canonical() {
        assert_eq!(canonical_key(" name: TSH Total "), "name:tsh total");
        assert_eq!(canonical_key(" 4548-4 "), "4548-4");
    }

    #[test]
    fn a_schedule_is_limited_sorted_and_tidied() {
        let at = |h: i8| Time::constant(h, 0, 0, 0);
        let mut daily = MedSchedule::Daily { times: vec![at(20), at(8), at(8)] };
        clean_schedule(&mut daily).unwrap();
        assert_eq!(daily, MedSchedule::Daily { times: vec![at(8), at(20)] });

        let mut crowded = MedSchedule::Daily { times: (0..13).map(at).collect() };
        assert_eq!(clean_schedule(&mut crowded).unwrap_err().code(), "invalid");
        let mut twelve = MedSchedule::Daily { times: (0..12).map(at).collect() };
        clean_schedule(&mut twelve).unwrap();

        let mut weekly = MedSchedule::Weekly { days: vec![5, 1, 5], times: vec![at(8)] };
        clean_schedule(&mut weekly).unwrap();
        assert_eq!(weekly, MedSchedule::Weekly { days: vec![1, 5], times: vec![at(8)] });
        let mut bad_day = MedSchedule::Weekly { days: vec![0], times: vec![at(8)] };
        assert!(clean_schedule(&mut bad_day).is_err());
        let mut never =
            MedSchedule::EveryNDays { n: 0, from: Date::constant(2026, 1, 1), times: vec![] };
        assert!(clean_schedule(&mut never).is_err());
    }

    #[test]
    fn tags_are_lower_cased_and_a_rules_tags_with_them() {
        let typed = [" Calcium ".to_string(), "IRON".into(), "iron".into(), " ".into()];
        assert_eq!(clean_tags("a tag", &typed).unwrap(), ["calcium", "iron"]);
        let mut rules = vec![MedRule::ApartFrom { tags: vec![" Calcium".into()], hours: 4.0 }];
        clean_rules(&mut rules).unwrap();
        assert_eq!(rules, vec![MedRule::ApartFrom { tags: vec!["calcium".into()], hours: 4.0 }]);
        let mut empty = vec![MedRule::ApartFrom { tags: vec![], hours: 4.0 }];
        assert!(clean_rules(&mut empty).is_err());
    }

    #[test]
    fn a_medication_needs_a_name_and_a_dose_above_zero() {
        let mut nameless = medication("   ");
        assert_eq!(clean_medication(&mut nameless).unwrap_err().code(), "invalid");
        let mut none = medication("Aspirin");
        none.dose = 0.0;
        assert!(clean_medication(&mut none).is_err());
        let mut nan = medication("Aspirin");
        nan.dose = f64::NAN;
        assert!(clean_medication(&mut nan).is_err());
        let mut long = medication(&"x".repeat(201));
        assert!(clean_medication(&mut long).is_err());
        let mut fine = medication("  Aspirin ");
        clean_medication(&mut fine).unwrap();
        assert_eq!(fine.name, "Aspirin");
    }

    #[test]
    fn a_target_is_judged_by_its_bounds_and_unknown_without_any() {
        let target = |low: Option<f64>, high: Option<f64>| CareTarget {
            label: "x".into(),
            measure: Measure::Lab { code: "4548-4".into() },
            low,
            high,
            use_reference_range: false,
            window_days: 0,
            note: String::new(),
        };
        assert_eq!(judge(6.4, &target(None, Some(7.0))), "met");
        assert_eq!(judge(7.1, &target(None, Some(7.0))), "notMet");
        assert_eq!(judge(0.6, &target(Some(0.7), None)), "notMet");
        assert_eq!(judge(0.7, &target(Some(0.7), None)), "met");
        assert_eq!(judge(5.0, &target(None, None)), "unknown");
    }

    #[test]
    fn outside_text_loses_what_hides_and_documents_keep_their_lines() {
        assert_eq!(outside_text("  Dr\u{202E} Who\u{200B}\n ", 100), "Dr Who");
        assert_eq!(outside_text("abcdef", 3), "abc");
        assert_eq!(document_text("TSH\t2.1\r\nT4\t1.0\u{7}", 100), "TSH\t2.1 \nT4\t1.0");
    }

    #[test]
    fn a_lab_needs_a_value_or_text() {
        let mut empty = lab("3016-3", "TSH", 1.0, Date::constant(2026, 1, 1), "");
        empty.value = None;
        assert!(clean_lab(&mut empty).is_err());
        empty.text = "Negative".into();
        clean_lab(&mut empty).unwrap();
        let mut infinite = lab("3016-3", "TSH", f64::INFINITY, Date::constant(2026, 1, 1), "");
        assert!(clean_lab(&mut infinite).is_err());
    }
}
