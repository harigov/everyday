# Health: activity, sleep, vitals, medications, meals and medical records

> **Delivered** (15 Nov 2026), not yet tried against a real phone or a real
> export. Built by fifteen agents against the contract below, laid down and
> compiled first. What went beyond the plan, each for a reason:
>
> - **A phone's token is good for its own source only.** `health_push_setup`
>   binds the minted token to the source (`set_health_source_token`, local
>   callers only), and `record_health_push` refuses a device whose token id
>   is not the source's. Deleting a source revokes its token first.
>   `GET /v1/blob/{id}` refuses a write-only token (heartbeats or a push).
> - **A document's pages and a meal's photo are files in the Files catalog**
>   (Files landed on main the same day), not blobs of Health's own. They are
>   added through Files' uploads -- in 4 MB pieces, or by path on the vault's
>   machine -- always sealed, with `FileOrigin::Health`; the record names them
>   by `FileId`. That brings Files' viewer and thumbnails, "used in"
>   (`RefKind::HealthDocument`, `RefKind::Meal`), exports that carry them,
>   and a trash: deleting a document, or replacing a meal's photo, moves what
>   Health sealed and nothing else uses to the Files trash. What a connector
>   sent (a FHIR resource, an ECG's CSV) is the record's own data, kept as a
>   sealed blob in `HealthDocument::source`, never a catalogued file.
> - **Health's files are out of the file tools**: `find_files` leaves origin
>   `health` out, and `file_info`, `read_file`, `organize_files` and
>   `trash_files` refuse it, so a token or a setting that opens somebody's
>   files does not open their lab reports; the screen context skips them; and
>   `/v1/files/file/{id}` wants `health` as well as `files` for one.
> - **An `export.zip` can be imported from Files** (`import_apple_health_file`):
>   one a sync tool brought into the folder from the phone, or one uploaded
>   from a window on another machine, which then goes to the trash once read.
>   A sealed one is read from a decrypted copy deleted the moment the import
>   ends.
> - **Deleting is not undone by the next push.** A removed sample leaves a
>   tombstone in its series day; a source's workout or dose is hidden, not
>   deleted, so its key still matches when "Previous 7 Days" sends it again.
> - **An instant and an interval with one start and value are one reading**
>   -- Health Auto Export sends most quantities with only their start, the
>   export with their interval -- and the interval wins.
> - **A workout's per-minute heart rate from Health Auto Export is not
>   stored**: it would double the raw watch samples beside it.
> - **A source's dose event is keyed by what the source calls it** (its code
>   or name, and its time), never its status or the medication it matched, so
>   a dose corrected on the phone updates in place.
> - **A dose change decides a recheck outright**, sooner or later than the
>   calendar's, and starting a medication counts as one.
> - **Due doses follow dates, not today's status**: a medication stopped last
>   week still counts for the weeks it was taken.
> - **A Health tracker on a mean, last or in-range metric averages over a
>   target's period** (`Tracker::aggregate`) and reads in words a person says
>   ("steps", "bpm"), not the catalogue's `count`.
> - **Ranges are rolling** (the last 7, 30, 91 or 365 days), not calendar
>   weeks: on a Sunday a calendar week held one day.
> - **`HealthSettings::default()` is written out** so a vault that never
>   saved settings still reminds (`dose_reminders: true`, `stale_hours: 24`);
>   `HealthKeys` redacts its key in `Debug`.
>
> **Still open:**
>
> - Never run against a real Health Auto Export push or a real `export.zip`.
>   The HAE metric names in the catalogue are from memory and need checking
>   against a real push; the first test is importing one day both ways and
>   comparing totals.
> - Changing the source priority does not rebuild stored summaries; days
>   written after the change use it.
> - A series answer's `count` for a metric that adds up counts quarter-hour
>   parts, not samples.
> - `clippy --features speech,decisions` was not run locally (the health code
>   uses neither feature).

A plan, written 15 November 2026 against `9ec8f366`. A twelfth app, **Health**
(`health` in code), fusing what Apple Fitness, Apple Health and MyFitnessPal
each do, aimed at living with a chronic condition.

## Decisions (Hari's)

- **One app**, with sections in its sidebar: Today, Activity, Sleep, Vitals,
  Nutrition, Medications, Conditions, Records. Managing a condition needs
  glucose beside meals, doses and workouts on one timeline.
- **Health data may go to cloud models**, like any other domain. No
  local-only gate for now.
- **Clinical documents are sealed in the vault**, like Money's data -- not
  plain files in the Files folder. (Delivered as sealed files in the Files
  catalog: see the note at the top.)
- **Apple Watch first**, through Health Auto Export (a paid iOS app whose
  REST API automations push JSON) plus Apple's `export.zip` for history,
  **behind a connector interface** so Oura, Withings, Dexcom or FIT files are
  a module each later.
- **Condition templates, not programs.** A template is data that fills in a
  condition's care plan; the plan is the person's from then on. First two:
  **diabetes** (type 1 and type 2) and **hypothyroidism**.

## Decisions I made (changeable)

- **Samples are not records.** One sealed row per metric per UTC day (raw
  samples) with a small sealed quarter-hour summary beside it, which range
  reads open instead. A year of resting heart rate is 365 small reads.
- **A sample has no id.** It is its metric, start, duration and value --
  never the source -- so the same reading arriving from the export and from
  the phone is stored once.
- **Source priority at read time**, as Apple does: a cumulative metric's
  quarter hour (steps, energy) counts only the best-ranked source that
  reported in it; Watch, then iPhone, then other apps, then typed in.
  Reorderable in Settings → Health.
- **Canonical units in storage** (kg, m, kcal, mg/dL, °C, ms), converted
  once on the way in and once for display.
- **The medication list lives here.** Apple Health dose events (iOS 26) are
  matched onto it by code, then by a name seen before, then by name.
- **Symptoms are trackers** (0-10 scales), which already exist; a template
  creates them. A health metric becomes a tracker through a new
  `TrackerSource::Health { metric, stat }`, so goals, targets, streaks and
  the Overview work for every metric with no extra machinery.
- **Not a medical device.** No diagnosis, no dose advice, no alarms:
  thresholds are the person's, alerts are notices after the fact, and a
  CGM's own alarms stay the safety alarm. The assistant never changes a dose
  or a plan.
- **Activity "rings" are meters**, not concentric arcs: the house style has
  no donuts.
- **No drug-interaction checks.** NLM retired its interaction API in 2024
  and nothing free and trustworthy replaced it.

## Getting data in

```text
  export.zip ──(connect::apple)──┐
  HAE push ────(connect::hae)────┼──▶ HealthBatch ──▶ Vault::ingest_health ──▶ series days,
  FHIR files ──(connect::fhir)───┘                                            workouts, doses,
  a person ──(add_health_reading, log_dose, save_meal...)                    documents
```

### Health Auto Export (live)

- **Where it posts:** `POST https://<machine>.<tailnet>.ts.net:7400/v1/health/hae/<source id>`
  -- web access, the one listener a phone reaches with a certificate it
  already trusts. Its gatekeeper already admits only the same tailnet user's
  devices. The route is the one on web access authenticated by a **bearer
  token** rather than the browser session: a write-only `healthIngest` token,
  minted by Settings → Health for one source and shown once.
- **Recommended automations**, one per data type (HAE sends one type per
  automation): Health Metrics, Workouts, Medications (iOS 26). Each JSON,
  Export Version 2, **Summarize Data off**, Batch Requests on, header
  `Authorization: Bearer <token>`. Hourly with "Since Last Sync", plus a
  daily "Previous 7 Days" twin: HAE's retry behaviour is undocumented and the
  vault may be locked when it runs; ingest is idempotent, so overlap is free.
- **Refused:** a push whose `automation-aggregation` header says it is
  summarised (summed buckets would not deduplicate against raw samples), and
  any push to a paused source.
- **Not possible:** deletions on the phone never arrive; delete here by hand.
- **Stale:** Today shows when each source was last heard from; after
  `stale_hours` (24) a notice, once a day.

### Apple's export.zip (history)

Picked in Settings → Health (a shell dialog; the path never touches the
webview), or -- from a window on another machine, or for an export a sync
tool brought into the files folder -- read from a file in Files
(`import_apple_health_file`). Read on a blocking thread with progress and
cancel. Streamed: zip
entry by entry, `export.xml` with a pull parser in batches of 50,000. Brings
`Record`s, `Workout`s (+ `workout-routes/*.gpx`), and `clinical-records/*.json`
(FHIR) as documents with lab results waiting to be accepted.
`export_cda.xml` is ignored (a duplicate). Unknown types are counted and
listed in the report, never silently dropped.

### Food lookup

USDA FoodData Central (public domain; the person's free api.data.gov key or
`DEMO_KEY`) and Open Food Facts for barcodes. Only the search text or
barcode leaves the machine. A photo of a plate goes to the medium model,
which answers foods and grams, every item marked `estimated`.

### Documents

Added to the Files catalog (sealed, origin `health`), then extracted on
request: the PDF's text layer
(read by the interface's pdf.js) or the pages as pictures go to the large
model, which answers lab results in a strict JSON shape; they wait in the
document's `pending_labs` until the person reviews them beside the page.

## Condition templates

**Diabetes (type 1 / type 2).** Watch glucose, time in range, insulin (T1),
carbs, weight, blood pressure, activity, sleep. Targets (consensus
defaults, editable): time in range 70-180 mg/dL above 70%, below 70 under 4%,
below 54 under 1% (14 days); A1c below 7%; BP below 130/80. Tests: A1c every
3 months; lipids, eGFR and urine albumin/creatinine, eye, foot exams yearly.
Dashboard: the day overlay (glucose with meals, doses, insulin, workouts,
sleep), the 14-day AGP, lows and highs, meal responses.

**Hypothyroidism.** Watch TSH and free T4 against each report's own
reference range, resting heart rate, weight, sleep, symptoms (fatigue,
feeling cold, low mood, constipation as 0-10 trackers). Tests: TSH (+ free
T4) 6 weeks after any dose change, else every 12 months. Medication rules for
anything tagged `thyroid`: empty stomach (nothing to eat for 30 minutes
after), 4 hours apart from anything tagged `calcium` or `iron`. Dashboard:
TSH and free T4 over time with dose changes marked.

## The contract

Laid down and compiled before the build. Read the code for the exact
shapes; this is the map.

### Core (`karmayoga_core::health`)

- `mod.rs` -- every record and DTO. Records (each a `RecordKind`, sealed,
  `everyday.<word>.v1:<id>`): `HealthSource`, `Workout`, `Medication`,
  `Dose`, `Condition`, `LabResult`, `HealthDocument`, `Allergy`, `Food`,
  `Meal`. Not records: `SeriesDay` / `SeriesSummary` (samples), `HealthSettings`
  (singleton).
- `metric.rs` -- the `Metric` enum (closed catalogue) and `MetricInfo`.
- `units.rs`, `chunk.rs` (merge, dedupe, summarise, source rank),
  `report.rs` (buckets, daily values, activity), `sleep.rs`, `glucose.rs`,
  `schedule.rs` (due doses, adherence, supply), `rules.rs` (timing rules),
  `templates.rs`, `nutrition.rs`, `connect/{apple,hae,fhir}.rs`.
- `store/health.rs` -- `HealthStore` and every aad label.
- `vault/health/{mod,data,care}.rs` -- the facade. `data.rs`: settings,
  sources, `ingest_health`, series and reports, workouts, keys, health
  tracker days. `care.rs`: medications, doses, allergies, conditions and
  templates, labs, documents, foods, meals, visit facts, and the two
  ingest helpers `file_source_doses` / `file_documents`.
- `tracker.rs` -- `TrackerSource::Health { metric, stat }`; readings are a
  metric's day under the stat, an `InRange` share as a percentage.
- Keyed hashes under the subkey `karmayoga.health.key.v1`: series (of
  `Metric::key()`), workout keys (activity|start|end), dose keys.
- Food-lookup keys: one sealed secret, owner `("health-keys", "vault")`.

### Storage (schema v30)

`health_series (series, day, updated_us, data, summary)`,
`health_settings`, `health_sources`, `health_workouts (dedupe_key unique,
start_us, end_us)`, `health_medications (sort_order)`, `health_doses
(medication_id, dedupe_key unique when non-empty, at_us)`,
`health_conditions`, `health_labs (collected)`, `health_documents (doc_date)`,
`health_allergies`, `health_foods (used_us)`, `health_meals (at_us)`.
`HealthStore::blob_refs` (a document's `source`, a workout's route) is
walked by `collect_garbage`; pages and photos are kept by the files catalog.

### Scopes and gates

`health` (the app) and `healthIngest` (write-only push). Setting the food
keys needs `all`. MCP reaches health tools only with `health` named in the
token. Research jobs are never offered Health. A health read taints the
turn (the fetch check asks), and a document's text, a food database's
names and a device's name reach the model as untrusted text.

### Commands (`domains::health`; scope `health` unless noted)

| command | effect | change | args | returns |
|---|---|---|---|---|
| `health_catalog` | read | | | `MetricEntry[]` |
| `health_settings` | read | | | `HealthSettings` |
| `save_health_settings` | write | settings/updated | `settings` | void |
| `list_health_sources` | read | | | `HealthSource[]` |
| `save_health_source` | write | healthSource/updated | `source` | `HealthSource` |
| `delete_health_source` | destructive | healthSource/deleted | `id` | void |
| `record_health_push` (**healthIngest**) | write | healthSample/updated | `sourceId`, `headers: PushHeaders`, `body` (JSON) | `HealthIngestReport` |
| `import_apple_health` | write | healthSample/updated | `path`, `sourceId?` | `ImportStatus` |
| `import_apple_health_file` | write | healthSample/updated | `file: FileId`, `sourceId?`, `trashAfter?` | `ImportStatus` |
| `apple_import_status` | read | | | `ImportStatus` |
| `cancel_apple_import` | write | | | boolean |
| `health_keys` | read | | | `HealthKeysInfo` |
| `save_health_keys` (**all**) | write | settings/updated | `fdcKey` | void |
| `clear_health_keys` (**all**) | destructive | settings/updated | | void |
| `health_today` | read | | `date`, `tz` | `HealthDay` |
| `health_series` | read | | `query: SeriesQuery` | `SeriesResult` |
| `health_samples` | read | | `metric`, `from`, `to` (instants, ≤ 3 days) | `SamplePoint[]` |
| `health_latest` | read | | `metrics`, `on` | `Latest[]` |
| `add_health_reading` | write | healthSample/updated | `reading: ManualReading` | void |
| `delete_health_samples` | destructive | healthSample/deleted | `metric`, `from`, `to`, `source?` | number |
| `health_activity` / `health_sleep` / `health_glucose` / `health_nutrition` | read | | `from`, `to`, `tz` | `ActivityDay[]` / `SleepNight[]` / `GlucoseReport` / `NutritionDay[]` |
| `health_day_timeline` | read | | `date`, `tz` | `DayTimeline` |
| `list_workouts` | read | | `from`, `to` (instants) | `Workout[]` |
| `get_workout` | read | | `id` | `WorkoutDetail` |
| `save_workout` / `delete_workout` | write / destructive | workout/* | `workout` / `id` | `Workout` / void |
| `list_medications` | read | | | `Medication[]` |
| `save_medication` / `delete_medication` | write / destructive | medication/* | `medication` / `id` | `Medication` / void |
| `list_doses` | read | | `from`, `to`, `medicationId?` | `Dose[]` |
| `log_dose` / `delete_dose` | write / destructive | dose/* | `dose` / `id` | `Dose` / void |
| `doses_due` | read | | `date`, `tz` | `DueDose[]` |
| `medication_alerts` | read | | `from`, `to`, `tz` | `RuleAlert[]` |
| `medication_adherence` | read | | `medicationId?`, `from`, `to`, `tz` | `Adherence` |
| `list_allergies`, `save_allergy`, `delete_allergy` | | allergy/* | | |
| `list_conditions`, `save_condition` | | condition/updated | `condition` | `Condition` |
| `delete_condition` | destructive | condition/deleted | `id`, `removeTrackers` | void |
| `condition_templates` | read | | | `TemplateInfo[]` |
| `apply_condition_template` | write | condition/created | `template`, `choice: TemplateChoice` | `Condition` |
| `condition_report` | read | | `id`, `date`, `tz` | `ConditionReport` |
| `list_labs` | read | | `key?` | `LabResult[]` |
| `save_lab` / `delete_lab` | write / destructive | labResult/* | `lab` / `id` | `LabResult` / void |
| `lab_trends` | read | | | `LabTrend[]` |
| `list_health_documents` / `get_health_document` | read | | / `id` | `HealthDocument[]` / `HealthDocument` |
| `add_health_document` | write | healthDocument/created | `title`, `kind`, `date?`, `files: FileId[]` | `HealthDocument` |
| `save_health_document` | write | healthDocument/updated | `document` | `HealthDocument` |
| `delete_health_document` | destructive | healthDocument/deleted | `id`, `deleteLabs` | void |
| `extract_health_document` | write | healthDocument/updated | `id`, `text?` | `HealthDocument` |
| `accept_document_labs` | write | labResult/created | `id`, `labs` | `LabResult[]` |
| `list_foods` | read | | `query` | `Food[]` |
| `save_food` / `delete_food` | write / destructive | food/* | `food` / `id` | `Food` / void |
| `search_foods` | read | | `query` | `FoodHit[]` |
| `lookup_barcode` | read | | `code` | `FoodHit \| null` |
| `list_meals` | read | | `from`, `to` (instants) | `Meal[]` |
| `save_meal` / `delete_meal` | write / destructive | meal/* | `meal` / `id` | `Meal` / void |
| `estimate_meal_photo` | read | | `photo: FileId` | `MealItem[]` |
| `meal_response` | read | | `id` | `MealResponse` |
| `health_visit_brief` | read | | `conditionId?`, `since`, `tz` | `VisitBrief` |

Shell commands (`karmayoga-app/src/health.rs`): `health_push_setup
{sourceId}` → `PushSetup`, `health_push_revoke {sourceId}` → boolean,
`health_import_apple` → `ImportStatus | null`.

### The interface

- `lib/types.ts` -- the Health section mirrors `mod.rs` (`HealthIngestReport`,
  not `IngestReport`, which Screen time owns).
- `lib/health.svelte.ts` -- the store; its public names are the contract.
- `lib/health.ts` -- pure helpers (ranges, units, names, colours, nutrition).
- Components (`components/Health*.svelte`), all on the shared `app.css`
  classes (`.side-*`, `.list-row`, `.pane-title`, pill `.btn`) and the
  existing charts where they fit (`ColumnChart`, `Meter`, `StatTile`,
  `ShareBar`), plus two shared Health charts with fixed props:
  `HealthBars` (bars over time, optional goal line) and `HealthLine` (a line
  with an optional band and markers).

## Deliberately not here

- Our own iOS app; Android (Health Connect has no cloud API).
- Pull connectors (Oura, Withings, Dexcom, Fitbit) and FIT/GPX files: the
  connector interface is ready for them.
- Drug interactions, dose advice, alarms.
- Karma for health deeds: Karma is not built yet. Imported watch data will
  need its own rule, since the Karma design refuses imported records.
- Importing samples and workouts from a Karma Yoga export: their keys are
  keyed by the vault's own key, which an import does not hold, and the phone
  and the Apple export bring them back anyway.
