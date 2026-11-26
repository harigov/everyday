//! Health: the samples a watch and a phone measured, filed by metric and
//! day; the connections they arrive through; workouts; medications and the
//! doses taken of them; conditions; lab results; clinical documents;
//! allergies; foods and meals; and the settings. See
//! `karmayoga_core::store::health` for the rules an implementation keeps and
//! `schema.rs`'s `v30` for the tables.
//!
//! What stays in the clear is only what an index is built from: a series'
//! keyed hash and its UTC day; ids; timestamps (a workout's start and end, a
//! dose's time, a meal's time, a food's last use, and every record's two
//! times); a medication's sort order; a dose's medication; a lab result's
//! collection date and a document's date; and the keyed dedupe keys of
//! workouts and doses. Everything a person would recognise -- the name of a
//! metric, a value, the name of a device or a source, a workout's name, a
//! medication, a condition, a diagnosis, a lab result's name and number, a
//! document's title and every word of its text, an allergy, a food, a meal --
//! is in the sealed payload beside them.
//!
//! A series is the keyed hash of a metric's name (under the vault's health
//! subkey, `karmayoga_core::health::HEALTH_KEY_LABEL`), computed by the vault
//! and handed over as an opaque string, because "this
//! vault measures glucose" is itself a diagnosis. What a database holder
//! learns is that something was measured on a day, and how many things are
//! measured.
//!
//! # Every row is bound to its own name
//!
//! Each sealed payload is bound by its associated data to the row it lives
//! in -- a record to its id, a series day to its series *and* its day, and
//! the series' summary under a label of its own, so one half of a day cannot
//! be passed off as the other, nor a day as another day, nor a record as
//! another record. A payload copied into another row, by somebody who can
//! write the database but does not hold the key, will not open there.
//!
//! # A series day is two columns
//!
//! The samples are `data`, a small quarter-hour summary is `summary`. Range
//! reads that only want totals ask for `summary`, and [`series_summaries`]
//! never selects `data`, so a year of resting heart rate is 365 small
//! payloads opened and not a million samples.
//!
//! # Where the filtering happens
//!
//! Only by the clear columns. A day's samples are a range over `day` within a
//! series; workouts are a range over `start_us` narrowed by `end_us`; doses
//! and meals are a range over their time. Every query that returns records
//! orders by clear columns too, so a page is an index scan that decrypts
//! exactly what it returns.
//!
//! # The dedupe keys' uniqueness
//!
//! [`put_workouts`] and [`put_doses`] refuse -- writing nothing -- a batch
//! that would leave one dedupe key under two ids. The unique indexes are the
//! backstop, but they are not the check: a violated index answers with a
//! driver's own error, different in each database, and in Postgres it aborts
//! the transaction it happened in. So the check is made first, inside the
//! same transaction as the writes, and answers with [`Error::Invalid`] the
//! same way on both. A dose without a key (one typed in here, not a source's
//! event) holds none, and any number of them may share the empty one; a
//! workout always has one, and an empty one is held like any other. See
//! [`check_keys`] for the order a batch is walked in.
//!
//! [`series_summaries`]: HealthStore::series_summaries
//! [`put_workouts`]: HealthStore::put_workouts
//! [`put_doses`]: HealthStore::put_doses

use jiff::Timestamp;
use jiff::civil::Date;
use karmayoga_core::error::{Error, Result};
use karmayoga_core::health::{
    Allergy, Condition, Dose, Food, HealthDocument, HealthSettings, HealthSource, LabResult, Meal,
    Medication, SeriesDay, SeriesSummary, Workout,
};
use karmayoga_core::id::{
    AllergyId, BlobId, ConditionId, DoseId, FoodId, HealthDocumentId, HealthSourceId, LabResultId,
    MealId, MedicationId, WorkoutId,
};
use karmayoga_core::store::health::{
    HealthStore, SeriesExtent, SeriesRow, allergy_aad, condition_aad, dose_aad, food_aad,
    health_document_aad, health_series_aad, health_settings_aad, health_source_aad,
    health_summary_aad, lab_result_aad, meal_aad, medication_aad, workout_aad,
};
use std::collections::{BTreeSet, HashMap};

use crate::conn::{Sql, SqlExt, ToValue, Value, Where};
use crate::record::{Record, upsert_stmt};
use crate::{SqlStore, placeholders, to_us, vals};

impl Record for HealthSource {
    const TABLE: &'static str = "health_sources";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // Not the name, the connector, whether it is paused, or anything it
        // has heard: a database that said "Dexcom" would be saying what
        // somebody lives with.
        vec![
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for Workout {
    const TABLE: &'static str = "health_workouts";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // The keyed hash, when it began and when it ended. Not the activity,
        // the name, a distance, a heart rate, or the source's own id -- which
        // is what the key is a keyed hash *of*, so this column cannot be
        // matched against it.
        vec![
            ("dedupe_key", self.key.to_value()),
            ("start_us", to_us(self.start).to_value()),
            ("end_us", to_us(self.end).to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for Medication {
    const TABLE: &'static str = "health_medications";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // Not the name, the strength, the schedule, the tags, what it is
        // for, who prescribed it or how much is left.
        vec![
            ("sort_order", self.sort_order.to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for Dose {
    const TABLE: &'static str = "health_doses";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // Which medication, so that deleting one can find its doses without
        // opening every dose; the keyed hash of a source's dose event, empty
        // for a dose typed in here; and when. Not whether it was taken or
        // skipped, how much, or a word of a note.
        vec![
            ("medication_id", self.medication_id.to_string().to_value()),
            ("dedupe_key", self.key.to_value()),
            ("at_us", to_us(self.at).to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for Condition {
    const TABLE: &'static str = "health_conditions";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // Not the name, the template, the diagnosis date, the codes or a
        // word of the care plan.
        vec![
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for LabResult {
    const TABLE: &'static str = "health_labs";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // The day it was collected, which a list is ordered by. Not the
        // test, the value, the unit, the range, the flag or the laboratory.
        vec![
            ("collected", self.collected.to_string().to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for HealthDocument {
    const TABLE: &'static str = "health_documents";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // The date on the paper, or nothing for a paper with none. Not the
        // title, the kind, the clinic, a file's name, or a word of its text.
        vec![
            ("doc_date", self.date.map(|d| d.to_string()).unwrap_or_default().to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for Allergy {
    const TABLE: &'static str = "health_allergies";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // Not the substance, the kind, the reaction or how bad it was.
        vec![
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for Food {
    const TABLE: &'static str = "health_foods";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // When it was last eaten, which the list is ordered by, and nothing
        // when it never was. Clamped to at least one, so that a time before
        // 1970 -- which nothing writes, but which a clock can say -- does
        // not sort below a food nobody has eaten. Not the name, the brand,
        // the barcode or a nutrient.
        let used = self.last_used.map(|t| to_us(t).max(1)).unwrap_or(0);
        vec![
            ("used_us", used.to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

impl Record for Meal {
    const TABLE: &'static str = "health_meals";

    fn columns(&self) -> Vec<(&'static str, Value)> {
        // When it was eaten. Not what, how much, the slot, or the photograph.
        vec![
            ("at_us", to_us(self.at).to_value()),
            ("created_us", to_us(self.created_at).to_value()),
            ("updated_us", to_us(self.updated_at).to_value()),
        ]
    }
}

/// How many ids go into one `IN (...)`. SQLite refuses a statement with more
/// than 32,766 bound parameters and Postgres more than 65,535, and an Apple
/// export brings years of workouts at once. `money.rs`, `news.rs` and
/// `screentime.rs` keep the same number for the same reason.
const IN_CHUNK: usize = 500;

/// The one statement a series day is written with: insert, or replace by
/// `(series, day)`. Both payloads and the time move together.
const PUT_SERIES: &str = "INSERT INTO health_series (series, day, updated_us, data, summary)
     VALUES (?1, ?2, ?3, ?4, ?5)
     ON CONFLICT (series, day) DO UPDATE SET updated_us = ?3, data = ?4, summary = ?5";

/// A day as the clear `day` column holds it: `YYYY-MM-DD`, which sorts as
/// dates do in both dialects -- but only for years `0000` to `9999`. jiff's
/// dates reach back to year -9999, written with a sign and six digits, and
/// those sort among themselves backwards; [`check_day`] keeps such a day out
/// of the table. A *range* may name one (the widest range there is starts at
/// the earliest date) and still means what it says, because the sign sorts
/// before every digit.
fn day_text(day: Date) -> String {
    day.to_string()
}

/// Refuse a day [`day_text`] could not store in order. No watch means one: a
/// sample dated like that is a parser's mistake, and storing it would put the
/// reading in the wrong place in every chart rather than saying so.
fn check_day(day: Date) -> Result<()> {
    if (0..=9999).contains(&day.year()) {
        Ok(())
    } else {
        Err(Error::Invalid(format!("{day} is not a date a health reading can be filed under")))
    }
}

fn day_of(text: &str) -> Result<Date> {
    text.parse::<Date>().map_err(|e| Error::Invalid(format!("a stored day is not a date: {e}")))
}

/// Refuse a batch -- before any of it is written -- if writing it would leave
/// one `dedupe_key` of `table` under two ids. `rows` is each row's id and key,
/// in the order the rows will be written; `noun` names the record in the
/// refusal. With `empty_is_free` an empty key is nobody's (a dose typed in
/// here), and without it an empty key is held like any other.
///
/// The batch is walked in order, against what the table holds now. A row
/// already stored frees the key it holds when it is rewritten, so a batch that
/// moves one row off a key and then gives that key to another is accepted: at
/// no point does the key have two holders. Two different ids wanting one key,
/// in the batch or between the batch and what is stored, is refused. The same
/// id twice is two writes of one row, and the second wins, as it would in the
/// table.
///
/// Two queries a chunk rather than one a row, because each statement is a
/// round trip to a Postgres server, and an export can bring thousands of
/// workouts: which key each of the batch's ids holds today, and who holds
/// every key the batch wants.
fn check_keys(
    tx: &mut dyn Sql,
    table: &str,
    rows: &[(String, &str)],
    empty_is_free: bool,
    noun: &str,
) -> Result<()> {
    let nobody = |key: &str| empty_is_free && key.is_empty();
    // key -> id, and id -> key, as stored now.
    let mut holder: HashMap<String, String> = HashMap::new();
    let mut key_of: HashMap<String, String> = HashMap::new();

    let ids: Vec<String> =
        rows.iter().map(|(id, _)| id.clone()).collect::<BTreeSet<_>>().into_iter().collect();
    for chunk in ids.chunks(IN_CHUNK) {
        let args: Vec<Value> = chunk.iter().cloned().map(Value::Text).collect();
        let found = tx.query(
            &format!(
                "SELECT id, dedupe_key FROM {table} WHERE id IN ({})",
                placeholders(1, chunk.len())
            ),
            &args,
        )?;
        for row in &found {
            let (id, key) = (row.text(0)?, row.text(1)?);
            if !nobody(&key) {
                holder.insert(key.clone(), id.clone());
            }
            key_of.insert(id, key);
        }
    }

    let wanted: BTreeSet<&str> =
        rows.iter().map(|(_, key)| *key).filter(|key| !nobody(key)).collect();
    let wanted: Vec<&str> = wanted.into_iter().collect();
    for chunk in wanted.chunks(IN_CHUNK) {
        let args: Vec<Value> = chunk.iter().map(|k| Value::Text((*k).to_string())).collect();
        let found = tx.query(
            &format!(
                "SELECT dedupe_key, id FROM {table} WHERE dedupe_key IN ({})",
                placeholders(1, chunk.len())
            ),
            &args,
        )?;
        for row in &found {
            holder.insert(row.text(0)?, row.text(1)?);
        }
    }

    for (id, key) in rows {
        if let Some(old) = key_of.remove(id)
            && holder.get(&old) == Some(id)
        {
            holder.remove(&old);
        }
        if nobody(key) {
            continue;
        }
        if let Some(other) = holder.get(*key)
            && other != id
        {
            // The ids are clear columns already; the key, though only a
            // keyed hash, is left out because nothing reading this needs it.
            return Err(Error::Invalid(format!(
                "{noun} {id} has the dedupe key of {noun} {other}"
            )));
        }
        holder.insert((*key).to_string(), id.clone());
        key_of.insert(id.clone(), (*key).to_string());
    }
    Ok(())
}

impl HealthStore for SqlStore {
    // ---- series ---------------------------------------------------------

    fn put_series(&self, rows: &[SeriesRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        // Sealed before the writer is taken, as `put_txns` does: an Apple
        // export is hundreds of thousands of samples, and nothing else needs
        // to wait behind their encryption. Each half under its own label,
        // bound to the series and the day.
        let mut sealed = Vec::with_capacity(rows.len());
        for row in rows {
            check_day(row.day)?;
            let data = self.seal(&health_series_aad(&row.series, row.day), &row.data)?;
            let summary = self.seal(&health_summary_aad(&row.series, row.day), &row.summary)?;
            sealed.push((row, data, summary));
        }
        let now = to_us(Timestamp::now());

        // One transaction. An early `?` drops it, which rolls it back: all of
        // the batch or none of it, so an import that half landed -- a day's
        // samples written and its summary not -- is never what a chart reads.
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        for (row, data, summary) in sealed {
            let args = vals![row.series.clone(), day_text(row.day), now, data, summary];
            tx.execute(PUT_SERIES, &args)?;
        }
        tx.commit()
    }

    fn series_days(&self, series: &str, from: Date, to: Date) -> Result<Vec<SeriesDay>> {
        let rows = self.read().query(
            "SELECT day, data FROM health_series
             WHERE series = ?1 AND day >= ?2 AND day <= ?3 ORDER BY day",
            &vals![series, day_text(from), day_text(to)],
        )?;
        rows.iter()
            .map(|row| {
                let day = day_of(&row.text(0)?)?;
                self.unseal(&health_series_aad(series, day), &row.bytes(1)?)
            })
            .collect()
    }

    fn series_summaries(&self, series: &str, from: Date, to: Date) -> Result<Vec<SeriesSummary>> {
        // `summary` and never `data`: the point of the second column.
        let rows = self.read().query(
            "SELECT day, summary FROM health_series
             WHERE series = ?1 AND day >= ?2 AND day <= ?3 ORDER BY day",
            &vals![series, day_text(from), day_text(to)],
        )?;
        rows.iter()
            .map(|row| {
                let day = day_of(&row.text(0)?)?;
                self.unseal(&health_summary_aad(series, day), &row.bytes(1)?)
            })
            .collect()
    }

    fn latest_series_day(&self, series: &str, on: Date) -> Result<Option<SeriesDay>> {
        // `(series, day)` is the primary key, so this is one seek however many
        // days the series holds.
        let row = self.read().query_opt(
            "SELECT day, data FROM health_series WHERE series = ?1 AND day <= ?2
             ORDER BY day DESC LIMIT 1",
            &vals![series, day_text(on)],
        )?;
        row.map(|row| {
            let day = day_of(&row.text(0)?)?;
            self.unseal(&health_series_aad(series, day), &row.bytes(1)?)
        })
        .transpose()
    }

    fn delete_series(&self, series: &str, from: Date, to: Date) -> Result<u64> {
        self.write().execute(
            "DELETE FROM health_series WHERE series = ?1 AND day >= ?2 AND day <= ?3",
            &vals![series, day_text(from), day_text(to)],
        )
    }

    fn series_extents(&self) -> Result<Vec<SeriesExtent>> {
        let rows = self.read().query(
            "SELECT series, MIN(day), MAX(day), COUNT(*) FROM health_series
             GROUP BY series ORDER BY series",
            &[],
        )?;
        rows.iter()
            .map(|row| {
                Ok(SeriesExtent {
                    series: row.text(0)?,
                    first: day_of(&row.text(1)?)?,
                    last: day_of(&row.text(2)?)?,
                    days: row.u64(3)?,
                })
            })
            .collect()
    }

    // ---- settings -------------------------------------------------------

    fn health_settings(&self) -> Result<HealthSettings> {
        // Never saved. The defaults are a real answer, as screen time's and
        // the meeting settings' are.
        Ok(self.get_singleton("health_settings", &health_settings_aad())?.unwrap_or_default())
    }

    fn put_health_settings(&self, settings: &HealthSettings) -> Result<()> {
        self.put_singleton("health_settings", &health_settings_aad(), settings)
    }

    // ---- sources --------------------------------------------------------

    fn list_sources(&self) -> Result<Vec<HealthSource>> {
        let rows = self
            .read()
            .records("SELECT id, data FROM health_sources ORDER BY created_us, id", &[])?;
        self.collect(rows, health_source_aad)
    }

    fn get_source(&self, id: HealthSourceId) -> Result<HealthSource> {
        self.get(id)
    }

    fn put_source(&self, source: &HealthSource) -> Result<()> {
        self.upsert(source)
    }

    fn delete_source(&self, id: HealthSourceId) -> Result<()> {
        // Only the source. A workout that named it keeps its own copy of the
        // source's name, and what it brought stays.
        self.delete_by_id::<HealthSource>(id)?;
        Ok(())
    }

    // ---- workouts -------------------------------------------------------

    fn workouts(&self, from: Timestamp, to: Timestamp) -> Result<Vec<Workout>> {
        // Overlap, both ends open: a workout that ended exactly at `from`, or
        // began exactly at `to`, shares no time with the range. Days laid end
        // to end therefore never count a workout's boundary twice.
        let rows = self.read().records(
            "SELECT id, data FROM health_workouts WHERE start_us < ?1 AND end_us > ?2
             ORDER BY start_us, id",
            &vals![to_us(to), to_us(from)],
        )?;
        self.collect(rows, workout_aad)
    }

    fn get_workout(&self, id: WorkoutId) -> Result<Workout> {
        self.get(id)
    }

    fn workout_by_key(&self, key: &str) -> Result<Option<Workout>> {
        let rows = self
            .read()
            .records("SELECT id, data FROM health_workouts WHERE dedupe_key = ?1", &vals![key])?;
        let found: Vec<Workout> = self.collect(rows, workout_aad)?;
        Ok(found.into_iter().next())
    }

    fn put_workouts(&self, workouts: &[Workout]) -> Result<()> {
        if workouts.is_empty() {
            return Ok(());
        }
        let mut sealed = Vec::with_capacity(workouts.len());
        for workout in workouts {
            sealed.push((workout, self.seal(&workout_aad(workout.id), workout)?));
        }

        // Checked and written on the writer in one transaction, so a key the
        // check found free cannot be taken by another write before this one
        // lands. An early `?` drops the transaction, which rolls it back.
        let keyed: Vec<(String, &str)> =
            workouts.iter().map(|w| (w.id.to_string(), w.key.as_str())).collect();
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        check_keys(tx.as_mut(), "health_workouts", &keyed, false, "workout")?;
        for (workout, data) in sealed {
            let (sql, args) = upsert_stmt(workout, data);
            tx.execute(&sql, &args)?;
        }
        tx.commit()
    }

    fn delete_workout(&self, id: WorkoutId) -> Result<()> {
        // Only the workout. Its heart rate is ordinary samples, which stay;
        // its route is a blob the garbage collector will find unreferenced.
        self.delete_by_id::<Workout>(id)?;
        Ok(())
    }

    // ---- medications and doses -----------------------------------------

    fn list_medications(&self) -> Result<Vec<Medication>> {
        let rows = self
            .read()
            .records("SELECT id, data FROM health_medications ORDER BY sort_order, id", &[])?;
        self.collect(rows, medication_aad)
    }

    fn get_medication(&self, id: MedicationId) -> Result<Medication> {
        self.get(id)
    }

    fn put_medication(&self, medication: &Medication) -> Result<()> {
        self.upsert(medication)
    }

    fn delete_medication(&self, id: MedicationId) -> Result<()> {
        // One transaction for the medication and every dose taken of it: a
        // medication gone with its doses left behind is a log of something
        // nobody can name, and the reverse is a medication that looks
        // untaken. Indexed statements rather than a foreign-key cascade, for
        // the reason `delete_link` gives.
        let medication = id.to_string();
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        tx.execute("DELETE FROM health_doses WHERE medication_id = ?1", &vals![medication])?;
        tx.execute("DELETE FROM health_medications WHERE id = ?1", &vals![medication])?;
        tx.commit()
    }

    fn doses(
        &self,
        from: Timestamp,
        to: Timestamp,
        medication: Option<MedicationId>,
    ) -> Result<Vec<Dose>> {
        let mut w = Where::new().gte("at_us", to_us(from)).lt("at_us", to_us(to));
        if let Some(medication) = medication {
            w = w.eq("medication_id", medication.to_string());
        }
        let (where_sql, args) = w.finish();
        let rows = self.read().records(
            &format!("SELECT id, data FROM health_doses WHERE {where_sql} ORDER BY at_us, id"),
            &args,
        )?;
        self.collect(rows, dose_aad)
    }

    fn get_dose(&self, id: DoseId) -> Result<Dose> {
        self.get(id)
    }

    fn dose_by_key(&self, key: &str) -> Result<Option<Dose>> {
        // An empty key is nobody's: every dose typed in here has one.
        if key.is_empty() {
            return Ok(None);
        }
        let rows = self
            .read()
            .records("SELECT id, data FROM health_doses WHERE dedupe_key = ?1", &vals![key])?;
        let found: Vec<Dose> = self.collect(rows, dose_aad)?;
        Ok(found.into_iter().next())
    }

    fn put_doses(&self, doses: &[Dose]) -> Result<()> {
        if doses.is_empty() {
            return Ok(());
        }
        let mut sealed = Vec::with_capacity(doses.len());
        for dose in doses {
            sealed.push((dose, self.seal(&dose_aad(dose.id), dose)?));
        }

        let keyed: Vec<(String, &str)> =
            doses.iter().map(|d| (d.id.to_string(), d.key.as_str())).collect();
        let mut conn = self.write();
        let mut tx = conn.begin()?;
        check_keys(tx.as_mut(), "health_doses", &keyed, true, "dose")?;
        for (dose, data) in sealed {
            let (sql, args) = upsert_stmt(dose, data);
            tx.execute(&sql, &args)?;
        }
        tx.commit()
    }

    fn delete_dose(&self, id: DoseId) -> Result<()> {
        self.delete_by_id::<Dose>(id)?;
        Ok(())
    }

    // ---- conditions -----------------------------------------------------

    fn list_conditions(&self) -> Result<Vec<Condition>> {
        let rows = self
            .read()
            .records("SELECT id, data FROM health_conditions ORDER BY created_us, id", &[])?;
        self.collect(rows, condition_aad)
    }

    fn get_condition(&self, id: ConditionId) -> Result<Condition> {
        self.get(id)
    }

    fn put_condition(&self, condition: &Condition) -> Result<()> {
        self.upsert(condition)
    }

    fn delete_condition(&self, id: ConditionId) -> Result<()> {
        // Only the condition. A medication still naming it is the vault's to
        // tidy, under its own write lock -- the condition's id is inside the
        // medication's sealed payload, which this could only reach by opening
        // every medication.
        self.delete_by_id::<Condition>(id)?;
        Ok(())
    }

    // ---- lab results ----------------------------------------------------

    fn list_labs(&self) -> Result<Vec<LabResult>> {
        let rows =
            self.read().records("SELECT id, data FROM health_labs ORDER BY collected, id", &[])?;
        self.collect(rows, lab_result_aad)
    }

    fn get_lab(&self, id: LabResultId) -> Result<LabResult> {
        self.get(id)
    }

    fn put_labs(&self, labs: &[LabResult]) -> Result<()> {
        // All or nothing, whatever the size: a document's accepted results
        // that half landed is a report with some of its numbers missing.
        self.upsert_many(labs)
    }

    fn delete_lab(&self, id: LabResultId) -> Result<()> {
        self.delete_by_id::<LabResult>(id)?;
        Ok(())
    }

    // ---- documents ------------------------------------------------------

    fn list_documents(&self) -> Result<Vec<HealthDocument>> {
        // `doc_date` is empty for a document with no date, and an empty
        // string sorts before every date, so descending puts them last.
        let rows = self.read().records(
            "SELECT id, data FROM health_documents ORDER BY doc_date DESC, id DESC",
            &[],
        )?;
        self.collect(rows, health_document_aad)
    }

    fn get_document(&self, id: HealthDocumentId) -> Result<HealthDocument> {
        self.get(id)
    }

    fn put_document(&self, document: &HealthDocument) -> Result<()> {
        self.upsert(document)
    }

    fn delete_document(&self, id: HealthDocumentId) -> Result<()> {
        // Only the document. A lab result that read from it keeps its number,
        // and the files are blobs the garbage collector will find unreferenced.
        self.delete_by_id::<HealthDocument>(id)?;
        Ok(())
    }

    // ---- allergies ------------------------------------------------------

    fn list_allergies(&self) -> Result<Vec<Allergy>> {
        let rows = self
            .read()
            .records("SELECT id, data FROM health_allergies ORDER BY created_us, id", &[])?;
        self.collect(rows, allergy_aad)
    }

    fn get_allergy(&self, id: AllergyId) -> Result<Allergy> {
        self.get(id)
    }

    fn put_allergy(&self, allergy: &Allergy) -> Result<()> {
        self.upsert(allergy)
    }

    fn delete_allergy(&self, id: AllergyId) -> Result<()> {
        self.delete_by_id::<Allergy>(id)?;
        Ok(())
    }

    // ---- foods and meals ------------------------------------------------

    fn list_foods(&self) -> Result<Vec<Food>> {
        // Most recently used first; `used_us` is 0 for a food nobody has
        // eaten, which is last when it is descending.
        let rows = self
            .read()
            .records("SELECT id, data FROM health_foods ORDER BY used_us DESC, id", &[])?;
        self.collect(rows, food_aad)
    }

    fn get_food(&self, id: FoodId) -> Result<Food> {
        self.get(id)
    }

    fn put_food(&self, food: &Food) -> Result<()> {
        self.upsert(food)
    }

    fn delete_food(&self, id: FoodId) -> Result<()> {
        // Only the food. A meal that was made of it keeps the nutrients it
        // worked out when it was logged.
        self.delete_by_id::<Food>(id)?;
        Ok(())
    }

    fn meals(&self, from: Timestamp, to: Timestamp) -> Result<Vec<Meal>> {
        let rows = self.read().records(
            "SELECT id, data FROM health_meals WHERE at_us >= ?1 AND at_us < ?2
             ORDER BY at_us, id",
            &vals![to_us(from), to_us(to)],
        )?;
        self.collect(rows, meal_aad)
    }

    fn get_meal(&self, id: MealId) -> Result<Meal> {
        self.get(id)
    }

    fn put_meal(&self, meal: &Meal) -> Result<()> {
        self.upsert(meal)
    }

    fn delete_meal(&self, id: MealId) -> Result<()> {
        self.delete_by_id::<Meal>(id)?;
        Ok(())
    }

    // ---- blobs ----------------------------------------------------------

    fn blob_refs(&self) -> Result<Vec<BlobId>> {
        // Two kinds of record hold a blob, and which ones do is written only
        // inside their sealed payloads, so each table is opened whole: what
        // a connector sent for a document, and a workout's route. One pass
        // over each, in the garbage collector's own time. A document's pages
        // and a meal's photograph are files in the catalog, whose own
        // records keep their blobs.
        let mut refs: Vec<BlobId> = Vec::new();
        for document in self.list_documents()? {
            refs.extend(document.source.map(|s| s.blob));
        }
        let rows = self.read().records("SELECT id, data FROM health_workouts", &[])?;
        let workouts: Vec<Workout> = self.collect(rows, workout_aad)?;
        refs.extend(workouts.iter().filter_map(|w| w.route));
        Ok(refs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::Dialect;
    use crate::schema::steps;
    use serde_json::json;

    /// The columns `v30` declares for `table`, in order, `id` and `data` left
    /// out -- the list [`Record::columns`] has to match -- and the table's
    /// own primary-key line, which is not a column.
    fn declared(table: &str) -> Vec<String> {
        let head = format!("CREATE TABLE IF NOT EXISTS {table} (");
        let ddl = steps(Dialect::Sqlite)
            .concat()
            .into_iter()
            .find(|sql| sql.contains(&head))
            .unwrap_or_else(|| panic!("no CREATE TABLE for {table}"));
        let open = ddl.find('(').expect("an open paren");
        let close = ddl.rfind(')').expect("a close paren");
        let body = &ddl[open + 1..close];
        body.lines()
            .filter_map(|line| line.split_whitespace().next())
            .map(|word| word.trim_end_matches(',').to_string())
            .filter(|word| word != "id" && word != "data" && word != "PRIMARY")
            .collect()
    }

    fn names<R: Record>(record: &R) -> Vec<String> {
        record.columns().into_iter().map(|(name, _)| name.to_string()).collect()
    }

    /// A word every record below carries in each field a person would
    /// recognise, for the test that looks for it in the clear columns.
    const WORD: &str = "Platypus";
    const T: &str = "2026-11-15T09:00:00Z";

    /// Records are built from the JSON their own serde reads, naming only
    /// what they require: every other field has its default, and what these
    /// tests are about is the columns, not the payload.
    fn from<R: serde::de::DeserializeOwned>(value: serde_json::Value) -> R {
        serde_json::from_value(value).expect("a record the type can read")
    }

    fn source() -> HealthSource {
        from(json!({
            "id": HealthSourceId::new(), "connector": "healthAutoExport", "name": WORD,
            "createdAt": T, "updatedAt": T,
        }))
    }

    fn workout() -> Workout {
        from(json!({
            "id": WorkoutId::new(), "activity": "running", "name": WORD, "note": WORD,
            "source": WORD, "externalId": WORD, "key": "0f3a9c",
            "start": "2026-11-15T07:00:00Z", "end": "2026-11-15T07:30:00Z",
            "distanceM": 5012.5, "avgHr": 151.0,
            "createdAt": T, "updatedAt": T,
        }))
    }

    fn medication() -> Medication {
        from(json!({
            "id": MedicationId::new(), "name": WORD, "strength": WORD, "prescriber": WORD,
            "notes": WORD, "tags": [WORD], "sortOrder": 3,
            "createdAt": T, "updatedAt": T,
        }))
    }

    fn dose(medication: &Medication) -> Dose {
        from(json!({
            "id": DoseId::new(), "medicationId": medication.id, "at": T, "note": WORD,
            "key": "0f3a9c", "amount": 4321.5,
            "createdAt": T, "updatedAt": T,
        }))
    }

    fn condition() -> Condition {
        from(json!({
            "id": ConditionId::new(), "name": WORD, "notes": WORD,
            "diagnosed": "2020-02-02", "createdAt": T, "updatedAt": T,
        }))
    }

    fn lab() -> LabResult {
        from(json!({
            "id": LabResultId::new(), "name": WORD, "value": 4321.5, "unit": WORD, "lab": WORD,
            "collected": "2026-11-10", "createdAt": T, "updatedAt": T,
        }))
    }

    fn document() -> HealthDocument {
        from(json!({
            "id": HealthDocumentId::new(), "title": WORD, "provider": WORD, "text": WORD,
            "date": "2026-11-12", "externalId": WORD, "createdAt": T, "updatedAt": T,
        }))
    }

    fn allergy() -> Allergy {
        from(json!({
            "id": AllergyId::new(), "substance": WORD, "reaction": WORD,
            "createdAt": T, "updatedAt": T,
        }))
    }

    fn food() -> Food {
        from(json!({
            "id": FoodId::new(), "name": WORD, "brand": WORD, "barcode": WORD,
            "per100g": { "kcal": 4321.5, "proteinG": 1.0, "carbsG": 2.0, "fatG": 3.0 },
            "lastUsed": T, "createdAt": T, "updatedAt": T,
        }))
    }

    fn meal() -> Meal {
        from(json!({
            "id": MealId::new(), "at": T, "name": WORD, "note": WORD,
            "createdAt": T, "updatedAt": T,
        }))
    }

    #[test]
    fn the_clear_columns_are_exactly_the_ones_v30_declares() {
        // `upsert_stmt` writes `columns()` into the table by name, so a
        // column here that the table lacks fails every write, and one the
        // table has that is missing here is left at its default forever.
        let medication = medication();
        assert_eq!(names(&source()), declared(HealthSource::TABLE));
        assert_eq!(names(&workout()), declared(Workout::TABLE));
        assert_eq!(names(&medication), declared(Medication::TABLE));
        assert_eq!(names(&dose(&medication)), declared(Dose::TABLE));
        assert_eq!(names(&condition()), declared(Condition::TABLE));
        assert_eq!(names(&lab()), declared(LabResult::TABLE));
        assert_eq!(names(&document()), declared(HealthDocument::TABLE));
        assert_eq!(names(&allergy()), declared(Allergy::TABLE));
        assert_eq!(names(&food()), declared(Food::TABLE));
        assert_eq!(names(&meal()), declared(Meal::TABLE));
    }

    #[test]
    fn the_series_statement_names_every_column_v30_declares() {
        // A series day is written by hand rather than through `Record`, so
        // the same promise is checked against the statement's own text.
        // `declared` leaves out `data`, the sealed samples, as it does for
        // every table; the statement names it beside `summary`.
        let head = PUT_SERIES.split_once('(').unwrap().1.split_once(')').unwrap().0;
        let got: Vec<String> =
            head.split(',').map(|c| c.trim().to_string()).filter(|c| c != "data").collect();
        assert_eq!(got, declared("health_series"));
        assert!(PUT_SERIES.contains("data = ?4, summary = ?5"), "both payloads are replaced");
    }

    #[test]
    fn no_clear_column_holds_anything_a_person_would_recognise() {
        // What a database holder sees of each record is exactly its
        // `columns()`: ids, a medication, a keyed hash, a date, an order and
        // two times. Not a name, a value, a note or a source, and never a
        // sealed payload in a column that is not `data`.
        let medication = medication();
        let all: Vec<(&str, Vec<(&'static str, Value)>)> = vec![
            ("source", source().columns()),
            ("workout", workout().columns()),
            ("medication", medication.columns()),
            ("dose", dose(&medication).columns()),
            ("condition", condition().columns()),
            ("lab", lab().columns()),
            ("document", document().columns()),
            ("allergy", allergy().columns()),
            ("food", food().columns()),
            ("meal", meal().columns()),
        ];
        for (record, columns) in all {
            for (name, value) in columns {
                match &value {
                    Value::Text(text) => {
                        assert!(!text.contains(WORD), "{record}.{name} holds {text:?}");
                    }
                    Value::Int(n) => {
                        assert!(*n != 4321 && *n != 4322, "{record}.{name} holds a value");
                    }
                    Value::Bytes(_) | Value::Real(_) => {
                        panic!("{record}.{name} is not a clear column of a kind v30 declares")
                    }
                    Value::Null | Value::Bool(_) => {}
                }
            }
        }
    }

    #[test]
    fn a_dose_without_a_key_holds_the_empty_one_and_a_document_without_a_date_holds_none() {
        // `check_keys` and the document order both lean on what an absent
        // value is written as.
        let mut dose = dose(&medication());
        dose.key.clear();
        let key = dose.columns().into_iter().find(|(name, _)| *name == "dedupe_key");
        assert_eq!(key, Some(("dedupe_key", Value::Text(String::new()))));

        let mut document = document();
        document.date = None;
        let date = document.columns().into_iter().find(|(name, _)| *name == "doc_date");
        assert_eq!(date, Some(("doc_date", Value::Text(String::new()))));
    }

    #[test]
    fn a_food_nobody_has_eaten_sorts_after_one_somebody_has() {
        let mut never = food();
        never.last_used = None;
        let mut long_ago = food();
        long_ago.last_used = Some(Timestamp::from_second(-86_400).unwrap());
        let used = |food: &Food| {
            food.columns().into_iter().find(|(name, _)| *name == "used_us").unwrap().1
        };
        assert_eq!(used(&never), Value::Int(0));
        assert_eq!(used(&long_ago), Value::Int(1), "a clock before 1970 is still a use");
    }

    #[test]
    fn a_day_outside_four_digit_years_is_refused_rather_than_misfiled() {
        assert!(check_day(Date::constant(2026, 11, 15)).is_ok());
        assert!(check_day(Date::constant(9999, 12, 31)).is_ok());
        assert!(check_day(Date::constant(0, 1, 1)).is_ok());
        assert!(check_day(Date::constant(-1, 1, 1)).is_err());
        assert_eq!(day_text(Date::constant(2026, 1, 5)), "2026-01-05");
        assert_eq!(day_of("2026-01-05").unwrap(), Date::constant(2026, 1, 5));
        // Why: before year 0 the text sorts backwards among itself.
        assert!(day_text(Date::constant(-1, 1, 1)) < day_text(Date::constant(-2, 1, 1)));
        // And a range may still begin at the earliest date there is: the sign
        // sorts before every digit.
        assert!(day_text(Date::MIN) < day_text(Date::constant(0, 1, 1)));
    }
}
