// State for Health.
//
// A sibling of `money.svelte.ts` and `screentime.svelte.ts`. Everything here
// goes through `api` -- the mock backend (`mock-health.ts`) stands in for the
// vault and the shell in a browser.
//
// The public names below are the contract `components/Health*.svelte` are
// written against: each name and signature was laid down before the bodies
// were (docs/plans/health.md, "The contract"). Members added since are
// marked as such in their own comments.
//
// What is loaded follows what is on screen: Today reads `health_today`, a
// range pane (activity, sleep, vitals, nutrition) its range, the medication,
// condition and record panes their lists. The catalogue, settings and
// sources are small and every pane needs them, so they are always read.
//
// Rules that shape it (Money's and Screen time's):
//
//   1. Every navigation reloads through a `latest()` generation, so when the
//      range is stepped three times quickly only the last answer lands.
//   2. Deleting -- samples, a medication, a condition, a document -- asks
//      first, through `question`, which `App.svelte` draws.
//   3. What somebody's body and medicines are is theirs. The assistant is
//      told the pane and the range, and the id of a record that is open --
//      never a value, a medication's name or a diagnosis in words. And
//      nothing in here writes a value, a name or a document's text to a log.

import { api, mediaUrl } from './api'
import { dateFormat, plural } from './format'
import {
  DEFAULT_UNITS,
  HEALTH_ACCENT,
  METRICS,
  VITALS_METRICS,
  bucketFor,
  dayInstants,
  digitsFor,
  formatMetric,
  isGlucoseCondition,
  newId,
  newMeal,
  newMedication,
  rangeLabel as labelOfRange,
  rangeOf,
  stepAnchor,
  toDisplay as convert,
  unitFor,
} from './health'
import { notify } from './notify.svelte'
import { seen, type Seen, type Showing } from './onscreen'
import { pref } from './prefs'
import { localZone } from './screentime'
import { app, errorMessage, handle, isLocked, quietly } from './state.svelte'
import { latest } from './store/latest'
import { guardedRefresh } from './store/refresh'
import { addDays, localeWeekStart, todayIso } from './time'
import { tracking } from './tracking.svelte'
import type {
  ActivityDay,
  Allergy,
  BlobId,
  Condition,
  ConditionId,
  ConditionReport,
  Connector,
  DayTimeline,
  DocFile,
  DocKind,
  Dose,
  DoseStatus,
  DueDose,
  Food,
  FoodHit,
  GlucoseReport,
  HealthDay,
  HealthDocument,
  HealthDocumentId,
  HealthKeysInfo,
  HealthSettings,
  HealthSource,
  HealthSourceId,
  ImportStatus,
  LabResult,
  LabTrend,
  Latest,
  ManualReading,
  Meal,
  MealId,
  MealItem,
  MealResponse,
  Medication,
  MedicationId,
  Metric,
  MetricEntry,
  NutritionDay,
  PushSetup,
  RuleAlert,
  SeriesQuery,
  SeriesResult,
  SleepNight,
  Stat,
  TemplateChoice,
  TemplateInfo,
  TemplateKey,
  VisitBrief,
  Workout,
  WorkoutDetail,
  WorkoutId,
} from './types'

/** The panes of the app, as the sidebar lists them. */
export const HEALTH_VIEWS = [
  'today',
  'activity',
  'sleep',
  'vitals',
  'nutrition',
  'medications',
  'conditions',
  'records',
] as const
export type HealthView = (typeof HEALTH_VIEWS)[number]

/** How wide a range pane's window is. */
export const HEALTH_RANGES = ['week', 'month', 'quarter', 'year'] as const
export type HealthRange = (typeof HEALTH_RANGES)[number]

/** A destructive act waiting to hear yes or no -- shaped for `ChoiceDialog`,
 *  as Money's and Screen time's are. */
export interface HealthQuestion {
  title: string
  body: string
  confirm: string
  /** Run on yes. */
  run: () => Promise<void>
  /** A second, alternative yes ("Delete with its results"), if any. */
  alternative?: { label: string; run: () => Promise<void> } | null
}

/** What `logDose` may say beyond the medication. */
export interface DoseOptions {
  status?: DoseStatus
  /** When it was taken; now when absent. */
  at?: string
  /** The due time it answers. */
  scheduledAt?: string | null
  amount?: number | null
  note?: string
}

/** The panes whose content is a window of days, stepped back and forward. */
export const HEALTH_RANGE_VIEWS: readonly HealthView[] = [
  'activity',
  'sleep',
  'vitals',
  'nutrition',
]

/**
 * Which pane was last open, how wide a window the range panes show, and the
 * metric Vitals has open -- remembered on this device: chrome, not a fact
 * about the vault. Read in the constructor, so the app does not draw Today
 * and visibly flip.
 */
const viewPref = pref<HealthView>(
  'karmayoga.health.view',
  (raw) => HEALTH_VIEWS.find((v) => v === raw) ?? 'today',
  'today',
)
const rangePref = pref<HealthRange>(
  'karmayoga.health.range',
  (raw) => HEALTH_RANGES.find((r) => r === raw) ?? 'week',
  'week',
)
const vitalsPref = pref<Metric>(
  'karmayoga.health.vitals',
  (raw) => METRICS.find((m) => m === raw) ?? 'resting_heart_rate',
  'resting_heart_rate',
)

/**
 * At most one reload a minute for samples arriving. A phone pushes every
 * hour and an import a batch at a time, so without this a long import would
 * read the pane again for every batch.
 */
const LIVE_EVERY_MS = 60_000
/** How often a running Apple import is asked how far it has got. */
const IMPORT_POLL_MS = 2_000
/** The days of doses and rule alerts the Medications pane looks back over. */
const RECENT_DAYS = 30
/** The days a diabetes dashboard's glucose report (and its AGP) covers. */
const GLUCOSE_DAYS = 14
/** A PDF's text layer is read for the first pages only, and no more than
 *  this much of it is sent: a lab report is a few pages. */
const MAX_PDF_PAGES = 40
const MAX_PDF_CHARS = 120_000
/** A text layer shorter than this is a scan with a few stray marks, not text. */
const MIN_PDF_CHARS = 40

/** The extensions a picked file is given a type for when the browser gives
 *  none, as a document's files are shown by their type. */
const MIME_BY_EXTENSION: Record<string, string> = {
  pdf: 'application/pdf',
  png: 'image/png',
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  heic: 'image/heic',
  webp: 'image/webp',
  txt: 'text/plain',
  json: 'application/json',
}

function mimeOf(file: File): string {
  if (file.type) return file.type
  const extension = file.name.split('.').pop()?.toLowerCase() ?? ''
  return MIME_BY_EXTENSION[extension] ?? 'application/octet-stream'
}

/**
 * The text layer of a PDF, read with the pdf.js the attachment viewer
 * already bundles -- lazily, so none of it is loaded until somebody asks for
 * a document to be read. `undefined` for a scan (no text layer to speak of),
 * a protected file, or anything that could not be read at all: the model is
 * sent the pages as pictures instead.
 */
async function pdfText(blob: BlobId): Promise<string | undefined> {
  try {
    const response = await fetch(mediaUrl(blob))
    if (!response.ok) return undefined
    const bytes = new Uint8Array(await response.arrayBuffer())
    const { openPdf } = await import('./pdf')
    const task = openPdf(bytes)
    try {
      const doc = await task.promise
      const pages: string[] = []
      let length = 0
      for (let n = 1; n <= Math.min(doc.numPages, MAX_PDF_PAGES) && length < MAX_PDF_CHARS; n++) {
        const content = await (await doc.getPage(n)).getTextContent()
        let text = ''
        for (const item of content.items) {
          if ('str' in item) text += item.hasEOL ? `${item.str}\n` : `${item.str} `
        }
        pages.push(text.trim())
        length += text.length
      }
      const all = pages.join('\n\n').trim()
      return all.length < MIN_PDF_CHARS ? undefined : all.slice(0, MAX_PDF_CHARS)
    } finally {
      void task.destroy()
    }
  } catch {
    return undefined
  }
}

/** What to read again after a write. */
type Reload = 'pane' | 'basics' | 'both' | 'none'

class HealthState {
  // ── navigation ───────────────────────────────────────────────────────

  view = $state<HealthView>('today')
  /** The day Today and the day overlay show, `YYYY-MM-DD`. */
  date = $state<string>(todayIso())
  /** The window the range panes show, and the day inside it they step from. */
  range = $state<HealthRange>('week')
  anchor = $state<string>(todayIso())
  /** The metric the Vitals pane has open. */
  vitalsMetric = $state<Metric>('resting_heart_rate')
  conditionId = $state<ConditionId | null>(null)
  documentId = $state<HealthDocumentId | null>(null)
  workoutId = $state<WorkoutId | null>(null)

  loaded = $state(false)
  busy = $state(false)
  error = $state<string | null>(null)
  question = $state.raw<HealthQuestion | null>(null)

  // ── always loaded ────────────────────────────────────────────────────

  catalog = $state<MetricEntry[]>([])
  settings = $state<HealthSettings | null>(null)
  sources = $state<HealthSource[]>([])
  keys = $state<HealthKeysInfo | null>(null)
  importStatus = $state<ImportStatus | null>(null)

  // ── per pane ─────────────────────────────────────────────────────────

  today = $state<HealthDay | null>(null)
  timeline = $state<DayTimeline | null>(null)
  activity = $state<ActivityDay[]>([])
  workouts = $state<Workout[]>([])
  workout = $state<WorkoutDetail | null>(null)
  sleep = $state<SleepNight[]>([])
  series = $state<SeriesResult | null>(null)
  latest = $state<Latest[]>([])
  glucose = $state<GlucoseReport | null>(null)
  meals = $state<Meal[]>([])
  nutrition = $state<NutritionDay[]>([])
  foods = $state<Food[]>([])
  medications = $state<Medication[]>([])
  doses = $state<Dose[]>([])
  dosesDue = $state<DueDose[]>([])
  alerts = $state<RuleAlert[]>([])
  allergies = $state<Allergy[]>([])
  conditions = $state<Condition[]>([])
  templates = $state<TemplateInfo[]>([])
  conditionReport = $state<ConditionReport | null>(null)
  labs = $state<LabResult[]>([])
  labTrends = $state<LabTrend[]>([])
  documents = $state<HealthDocument[]>([])

  // ── editors ──────────────────────────────────────────────────────────
  //
  // `create` opens one of these on the pane that is showing; the editors
  // are components that read them. A save of the same record closes it.

  /** The meal the Nutrition pane's editor has open, if any. */
  editingMeal = $state<Meal | null>(null)
  /** The medication the Medications pane's editor has open, if any. */
  editingMedication = $state<Medication | null>(null)
  /** Whether the dialog that types in one reading is open. */
  readingOpen = $state(false)

  // -- added after the skeleton ---------------------------------------------
  /** The second line of a pair: the diastolic series when `vitalsMetric` is
   *  the systolic one, so blood pressure is drawn as the two numbers it is. */
  seriesSecond = $state<SeriesResult | null>(null)
  /** How often a running import is asked how far it has got. A field so a
   *  test does not have to wait two seconds a poll. */
  pollMs = IMPORT_POLL_MS

  /** Which load is the current one, per thing loaded. */
  #basicsGen = latest()
  #paneGen = latest()
  #keysGen = latest()
  #workoutGen = latest()
  #importGen = latest()
  /** Pane loads and writes in flight, so `busy` lasts until the last answers. */
  #loads = 0
  /** When samples arriving last caused a reload. See `LIVE_EVERY_MS`. */
  #liveAt = 0
  /** The day `date` and `anchor` were last put on, so a window left open past
   *  midnight moves them on when Health is opened again. */
  #today = todayIso()
  /** Read once: `localeWeekStart` builds an `Intl.Locale`, and `from`/`to`
   *  are read on every render. */
  #weekStart = localeWeekStart()
  /** Writes sent and not yet answered, so the close handshake can wait. */
  #writes = new Set<Promise<unknown>>()
  /** The timer between two asks of a running import. Armed only by an
   *  import, and cleared when it ends and when the vault locks. */
  #importTimer: ReturnType<typeof setTimeout> | null = null

  constructor() {
    // Registered once, here, as Money's are: the view calls `start` on
    // every mount.
    app.onLock(() => this.reset())
    app.onFlush(() => this.flush())
    this.view = viewPref.get()
    this.range = rangePref.get()
    this.vitalsMetric = vitalsPref.get()
  }

  // ── derived ──────────────────────────────────────────────────────────

  /** First and last day of the range on screen. */
  get from(): string {
    return rangeOf(this.range, this.anchor, this.#weekStart).from
  }
  get to(): string {
    return rangeOf(this.range, this.anchor, this.#weekStart).to
  }
  /** "Last 7 days", "Last 3 months", "3–9 Nov" -- and on Today, the day on
   *  screen: "Today", "Yesterday", "Mon 3 Nov". */
  get rangeLabel(): string {
    if (this.view === 'today') return this.#dayLabel(this.date)
    return labelOfRange(this.range, this.from, this.to, todayIso())
  }
  /** Is the range on screen the current one (nothing to step forward to)? */
  get atPresent(): boolean {
    return this.view === 'today' ? this.date >= todayIso() : this.to >= todayIso()
  }
  get document(): HealthDocument | null {
    return this.documents.find((d) => d.id === this.documentId) ?? null
  }
  get condition(): Condition | null {
    return this.conditions.find((c) => c.id === this.conditionId) ?? null
  }
  /** The colour the window tints itself while Health is open. */
  get accent(): string {
    return HEALTH_ACCENT
  }

  /**
   * What is on screen, for the assistant -- see `onscreen.ts`.
   *
   * The pane and the range in the interface's own words, and the id of a
   * record that is open. Never a value, a medication's or a diagnosis's
   * name, or a document's title: those are the person's, and the service
   * looks each id up from the vault when it describes it. The labels on
   * the references are as plain as the words: they are drawn above the
   * composer, where nobody should have to see "Metformin" read out.
   */
  get showing(): Showing {
    const day = this.#dayWords(this.date)
    const span = this.#rangeWords()
    const where: Record<HealthView, string> = {
      today: `their health summary for ${day}`,
      activity: `their activity and workouts for ${span}`,
      sleep: `their sleep for ${span}`,
      vitals: `their vitals for ${span}`,
      nutrition: `their meals and nutrition for ${span}`,
      medications: 'their medications, when each is due, and their allergies',
      conditions: 'their conditions and care plans',
      records: 'their medical records and lab results',
    }
    // A record counts as open only where it is drawn, and only once the
    // vault has it: a medication being typed in is not yet anything to look up.
    const open: Seen[] = []
    if (this.view === 'activity') open.push(...seen('workout', this.workoutId, 'A workout'))
    if (this.view === 'conditions') open.push(...seen('condition', this.conditionId, 'A condition'))
    if (this.view === 'records') {
      open.push(...seen('healthDocument', this.documentId, 'A medical document'))
    }
    if (this.view === 'nutrition' && this.editingMeal) {
      const id = this.editingMeal.id
      if (this.meals.some((m) => m.id === id)) open.push(...seen('meal', id, 'A meal'))
    }
    if (this.view === 'medications' && this.editingMedication) {
      const id = this.editingMedication.id
      if (this.medications.some((m) => m.id === id)) {
        open.push(...seen('medication', id, 'A medication'))
      }
    }
    return { view: where[this.view], open }
  }

  metricInfo(metric: Metric): MetricEntry | undefined {
    return this.catalog.find((m) => m.metric === metric)
  }
  /** The unit `metric` is shown in under the person's settings. */
  displayUnit(metric: Metric): string {
    return unitFor(metric, this.#unitsNow(), this.catalog)
  }
  /** A canonical value converted and formatted with its unit: "72 bpm",
   *  "81.4 kg", "6.1 mmol/L". */
  display(metric: Metric, value: number): string {
    return formatMetric(metric, value, this.#unitsNow(), this.catalog)
  }
  /** The same value converted, without the unit, for an input -- rounded to
   *  what `display` shows, so what is edited is what was read. */
  toDisplay(metric: Metric, value: number): number {
    const shown = convert(metric, value, this.#unitsNow())
    const scale = 10 ** digitsFor(metric, this.#unitsNow())
    return Math.round(shown * scale) / scale
  }

  /** How values are shown: the person's choice, or the defaults before the
   *  settings are read. */
  #unitsNow() {
    return this.settings?.units ?? DEFAULT_UNITS
  }

  /** "Today", "Yesterday", "Mon 3 Nov" (with the year when it is not this one's). */
  #dayLabel(day: string): string {
    const today = todayIso()
    if (day === today) return 'Today'
    if (day === addDays(today, -1)) return 'Yesterday'
    const at = new Date(`${day}T12:00:00`)
    return dateFormat({
      weekday: 'short',
      day: 'numeric',
      month: 'short',
      year: day.slice(0, 4) === today.slice(0, 4) ? undefined : 'numeric',
    }).format(at)
  }

  /** The day, lower-cased where it is a word: "today", "Mon 3 Nov". */
  #dayWords(day: string): string {
    const label = this.#dayLabel(day)
    return label === 'Today' || label === 'Yesterday' ? label.toLowerCase() : label
  }

  /** The range on screen as it reads in a sentence. */
  #rangeWords(): string {
    const label = labelOfRange(this.range, this.from, this.to, todayIso())
    return label.startsWith('Last ') ? `the ${label.toLowerCase()}` : label
  }

  // ── lifecycle ────────────────────────────────────────────────────────

  /** Called when the app is opened. Loads the catalogue, settings, sources
   *  and the key's state, and whatever the pane on screen needs. */
  async start(): Promise<void> {
    if (!app.supportsHealth) return
    // A window left open past midnight: what was today moves on with it.
    const today = todayIso()
    if (this.date === this.#today) this.date = today
    if (this.anchor === this.#today) this.anchor = today
    this.#today = today
    await Promise.all([this.reload(), this.#loadKeys(), this.#loadImport()])
    if (app.screen === 'main') this.loaded = true
  }

  /** Read again whatever is on screen. */
  async reload(): Promise<void> {
    if (!app.supportsHealth) return
    // An answer that arrives after a lock must not fill the screen back in.
    if (app.screen !== 'main') return
    await Promise.all([this.#loadBasics(false), this.#loadPane(false)])
  }

  /** Forget everything (the vault locked). The pane, the window and the
   *  metric stay: they are chrome. What was measured, taken and eaten is
   *  exactly what a lock exists to put away. */
  reset(): void {
    for (const gen of [
      this.#basicsGen,
      this.#paneGen,
      this.#keysGen,
      this.#workoutGen,
      this.#importGen,
    ]) {
      gen.next()
    }
    this.#stopPolling()
    this.#loads = 0
    this.#liveAt = 0
    this.question = null
    this.loaded = false
    this.busy = false
    this.error = null
    this.#today = todayIso()
    this.date = this.#today
    this.anchor = this.#today
    this.conditionId = null
    this.documentId = null
    this.workoutId = null
    this.editingMeal = null
    this.editingMedication = null
    this.readingOpen = false
    this.catalog = []
    this.settings = null
    this.sources = []
    this.keys = null
    this.importStatus = null
    this.today = null
    this.timeline = null
    this.activity = []
    this.workouts = []
    this.workout = null
    this.sleep = []
    this.series = null
    this.seriesSecond = null
    this.latest = []
    this.glucose = null
    this.meals = []
    this.nutrition = []
    this.foods = []
    this.medications = []
    this.doses = []
    this.dosesDue = []
    this.alerts = []
    this.allergies = []
    this.conditions = []
    this.templates = []
    this.conditionReport = null
    this.labs = []
    this.labTrends = []
    this.documents = []
  }

  /** Wait for anything outstanding. Part of the close handshake. */
  async flush(): Promise<void> {
    await Promise.allSettled([...this.#writes])
  }

  /**
   * Samples arrived from the phone or an import: reload what shows them, at
   * most once a minute -- and only if what is on screen would show anything
   * new: a window that reaches today, or today's own page.
   */
  samplesArrived(): Promise<void> | undefined {
    if (!this.loaded || app.screen !== 'main' || app.section !== 'health') return undefined
    const today = todayIso()
    const showsToday =
      (this.view === 'today' && this.date === today) ||
      (HEALTH_RANGE_VIEWS.includes(this.view) && this.from <= today && this.to >= today) ||
      (this.view === 'conditions' && this.conditionId !== null)
    if (!showsToday) return undefined
    const now = Date.now()
    if (now - this.#liveAt < LIVE_EVERY_MS) return undefined
    this.#liveAt = now
    // Sources too: what a push brings includes when it was last heard from.
    return Promise.all([this.#loadPane(true), this.#loadBasics(true)]).then(() => undefined)
  }

  /** The app bar's "new" in Health: log a meal on Nutrition, add a
   *  medication on Medications, a reading elsewhere. */
  create(): void {
    if (this.view === 'nutrition') {
      this.editingMeal = newMeal()
    } else if (this.view === 'medications') {
      const last = this.medications.reduce((n, m) => Math.max(n, m.sortOrder + 1), 0)
      this.editingMedication = newMedication(last)
    } else {
      this.readingOpen = true
    }
  }

  // ── navigation ───────────────────────────────────────────────────────

  setView(view: HealthView): void {
    if (view === this.view) return
    this.view = view
    viewPref.set(view)
    void this.#loadPane(true)
  }

  setRange(range: HealthRange): void {
    if (range === this.range) return
    this.range = range
    rangePref.set(range)
    // A window reaching past today is cut back to the one holding today.
    if (this.from > todayIso()) this.anchor = todayIso()
    if (HEALTH_RANGE_VIEWS.includes(this.view)) void this.#loadPane(true)
  }

  /** One window back (-1) or forward (+1) -- on Today, one day. */
  step(delta: -1 | 1): void {
    const today = todayIso()
    if (this.view === 'today') {
      if (delta > 0 && this.date >= today) return
      this.setDate(addDays(this.date, delta))
      return
    }
    if (!HEALTH_RANGE_VIEWS.includes(this.view)) return
    if (delta > 0 && this.atPresent) return
    const next = stepAnchor(this.range, this.anchor, delta)
    // Forward into the window holding today lands on today, not on the same
    // weekday next week.
    this.anchor = next > today ? today : next
    void this.#loadPane(true)
  }

  goToday(): void {
    const today = todayIso()
    this.anchor = today
    this.date = today
    if (
      this.view === 'today' ||
      this.view === 'conditions' ||
      HEALTH_RANGE_VIEWS.includes(this.view)
    ) {
      void this.#loadPane(true)
    }
  }

  /** Show one day: Today's page, and the day overlay on a condition. */
  setDate(date: string): void {
    const today = todayIso()
    const next = date > today ? today : date
    if (next === this.date) return
    this.date = next
    if (this.view === 'today' || this.view === 'conditions') void this.#loadPane(true)
  }

  setVitalsMetric(metric: Metric): void {
    if (metric === this.vitalsMetric) return
    this.vitalsMetric = metric
    vitalsPref.set(metric)
    if (this.view === 'vitals') void this.#loadPane(true)
  }

  /** Open a condition, or close it (`null`). Opening one from anywhere goes
   *  to the pane it is drawn on. */
  openCondition(id: ConditionId | null): void {
    this.conditionId = id
    if (id && this.view !== 'conditions') this.setView('conditions')
    else if (this.view === 'conditions') void this.#loadPane(true)
  }

  /** Open a document, or close it (`null`). Opening one from anywhere goes
   *  to the pane it is drawn on. */
  openDocument(id: HealthDocumentId | null): void {
    this.documentId = id
    if (id && this.view !== 'records') this.setView('records')
  }

  /** Open a workout, or close it (`null`). Opening one from anywhere goes to
   *  the pane it is drawn on. */
  openWorkout(id: WorkoutId | null): void {
    this.workoutId = id
    if (id && this.view !== 'activity') this.setView('activity')
    void this.#loadWorkout()
  }

  // ── readings and workouts ────────────────────────────────────────────

  async addReading(reading: ManualReading): Promise<boolean> {
    if (!app.supportsHealth) return false
    const plain = $state.snapshot(reading)
    if (!plain.tz) plain.tz = localZone()
    const done = await this.#drop('The reading could not be saved', () =>
      api.addHealthReading(plain),
    )
    if (done) this.readingOpen = false
    return done
  }

  /** Asks first. `from` and `to` are instants. */
  deleteSamples(metric: Metric, from: string, to: string, label: string): void {
    this.#confirm(
      `Delete ${label}?`,
      'Every reading of it in that time, from every source, is deleted from this vault for good. ' +
        'A phone or an export that sends them again will bring them back.',
      'Delete',
      async () => {
        const gone = await this.#save('The readings could not be deleted', () =>
          api.deleteHealthSamples(metric, from, to),
        )
        if (gone !== null) {
          notify.info(
            gone === 0 ? 'Nothing was recorded then.' : `Deleted ${plural(gone, 'reading')}.`,
            {
              key: 'health-deleted',
            },
          )
        }
      },
    )
  }

  async saveWorkout(workout: Workout): Promise<Workout | null> {
    if (!app.supportsHealth) return null
    const plain = { ...$state.snapshot(workout), updatedAt: new Date().toISOString() }
    return this.#save('The workout could not be saved', () => api.saveWorkout(plain))
  }

  deleteWorkout(workout: Workout): void {
    const id = workout.id
    this.#confirm(
      'Delete this workout?',
      'The workout is deleted from this vault for good. The heart rate and energy it was made from stay.',
      'Delete',
      async () => {
        if (await this.#drop('The workout could not be deleted', () => api.deleteWorkout(id))) {
          if (this.workoutId === id) this.openWorkout(null)
        }
      },
    )
  }

  /** Make a tracker for `metric` so it shows on the Overview and can carry
   *  a goal's target. */
  async trackOnOverview(metric: Metric, stat: Stat, name: string): Promise<void> {
    if (!app.supportsHealth) return
    const title = name.trim() || this.metricInfo(metric)?.name || 'Health'
    // Through the tracking store, so the Overview has it the moment it is made.
    const made = await tracking.addTracker(title, 'amount')
    if (!made) return
    await tracking.saveTracker({
      ...$state.snapshot(made),
      icon: 'pulse',
      source: { type: 'health', metric, stat: $state.snapshot(stat) },
    })
    notify.success('Added to the Overview.', { key: 'health-tracked' })
  }

  // ── medications ──────────────────────────────────────────────────────

  async saveMedication(medication: Medication): Promise<Medication | null> {
    if (!app.supportsHealth) return null
    const plain = { ...$state.snapshot(medication), updatedAt: new Date().toISOString() }
    plain.name = plain.name.trim()
    // Tags are what other medications' rules match against: spelled one way.
    plain.tags = [...new Set(plain.tags.map((t) => t.trim().toLowerCase()).filter(Boolean))]
    const saved = await this.#save('The medication could not be saved', () =>
      api.saveMedication(plain),
    )
    if (saved && this.editingMedication?.id === saved.id) this.editingMedication = null
    return saved
  }

  deleteMedication(medication: Medication): void {
    const id = medication.id
    this.#confirm(
      `Delete “${medication.name}”?`,
      'Every dose logged for it is deleted with it. To keep the history, stop it instead.',
      'Delete',
      async () => {
        const done = await this.#drop('The medication could not be deleted', () =>
          api.deleteMedication(id),
        )
        if (done && this.editingMedication?.id === id) this.editingMedication = null
      },
    )
  }

  async logDose(medicationId: MedicationId, options: DoseOptions = {}): Promise<Dose | null> {
    if (!app.supportsHealth) return null
    const now = new Date().toISOString()
    const dose: Dose = {
      id: newId(),
      medicationId,
      scheduledAt: options.scheduledAt ?? null,
      at: options.at ?? now,
      status: options.status ?? 'taken',
      amount: options.amount ?? null,
      origin: 'manual',
      note: options.note ?? '',
      key: '',
      hidden: false,
      createdAt: now,
      updatedAt: now,
    }
    // On screen first: the due time it answers shows as done at once, and is
    // put back if the vault refuses.
    for (const list of [this.dosesDue, this.today?.dosesDue ?? []]) {
      const due = list.find(
        (d) => d.medicationId === medicationId && !d.dose && d.dueAt === dose.scheduledAt,
      )
      if (due) due.dose = dose
    }
    const saved = await this.#save('The dose could not be saved', () => api.logDose(dose))
    // A refusal reads the screen back as the vault has it.
    if (saved === null) await this.#loadPane(true)
    return saved
  }

  async deleteDose(dose: Dose): Promise<void> {
    if (!app.supportsHealth) return
    const id = dose.id
    for (const list of [this.dosesDue, this.today?.dosesDue ?? []]) {
      for (const due of list) if (due.dose?.id === id) due.dose = null
    }
    this.doses = this.doses.filter((d) => d.id !== id)
    const done = await this.#drop('The dose could not be deleted', () => api.deleteDose(id))
    if (!done) await this.#loadPane(true)
  }

  async saveAllergy(allergy: Allergy): Promise<Allergy | null> {
    if (!app.supportsHealth) return null
    const plain = { ...$state.snapshot(allergy), updatedAt: new Date().toISOString() }
    plain.substance = plain.substance.trim()
    return this.#save('The allergy could not be saved', () => api.saveAllergy(plain))
  }

  deleteAllergy(allergy: Allergy): void {
    const id = allergy.id
    this.#confirm(
      `Delete “${allergy.substance}”?`,
      'It is taken off the list of allergies in this vault.',
      'Delete',
      async () => {
        await this.#drop('The allergy could not be deleted', () => api.deleteAllergy(id))
      },
    )
  }

  // ── conditions ───────────────────────────────────────────────────────

  async applyTemplate(key: TemplateKey, choice: TemplateChoice): Promise<Condition | null> {
    if (!app.supportsHealth) return null
    const plain = $state.snapshot(choice)
    const made = await this.#save(
      'The condition could not be added',
      () => api.applyConditionTemplate(key, plain),
      'none',
    )
    if (made) {
      // Open what was just made.
      this.conditionId = made.id
      await this.#loadPane(true)
    }
    return made
  }

  async saveCondition(condition: Condition): Promise<Condition | null> {
    if (!app.supportsHealth) return null
    const plain = { ...$state.snapshot(condition), updatedAt: new Date().toISOString() }
    plain.name = plain.name.trim()
    return this.#save('The condition could not be saved', () => api.saveCondition(plain))
  }

  /** Asks first, offering to remove the trackers its plan made. */
  deleteCondition(condition: Condition): void {
    const id = condition.id
    const remove = async (removeTrackers: boolean) => {
      const done = await this.#drop('The condition could not be deleted', () =>
        api.deleteCondition(id, removeTrackers),
      )
      if (done && this.conditionId === id) this.openCondition(null)
    }
    this.#confirm(
      `Delete “${condition.name}”?`,
      'Its care plan is deleted. Your medications, results, readings and documents stay.',
      'Delete',
      () => remove(false),
      condition.plan.trackerIds.length > 0
        ? { label: 'Delete and remove its trackers', run: () => remove(true) }
        : undefined,
    )
  }

  async visitBrief(conditionId: ConditionId | null, since: string): Promise<VisitBrief | null> {
    if (!app.supportsHealth) return null
    return this.#save(
      'The summary could not be written',
      () => api.healthVisitBrief(conditionId, since, localZone()),
      'none',
    )
  }

  // ── labs and documents ───────────────────────────────────────────────

  async saveLab(lab: LabResult): Promise<LabResult | null> {
    if (!app.supportsHealth) return null
    const plain = { ...$state.snapshot(lab), updatedAt: new Date().toISOString() }
    return this.#save('The result could not be saved', () => api.saveLab(plain))
  }

  deleteLab(lab: LabResult): void {
    const id = lab.id
    this.#confirm(
      `Delete the ${lab.name} result?`,
      'The result is deleted from this vault. The document it came from stays.',
      'Delete',
      async () => {
        await this.#drop('The result could not be deleted', () => api.deleteLab(id))
      },
    )
  }

  /** Upload the files (`api.putBlob`) and make one document of them. */
  async addDocument(
    files: File[],
    title: string,
    kind: DocKind,
    date?: string | null,
  ): Promise<HealthDocument | null> {
    if (!app.supportsHealth || files.length === 0) return null
    const name = title.trim() || files[0]?.name.replace(/\.[^.]+$/, '') || 'Document'
    return this.#save('The document could not be added', async () => {
      const uploaded: DocFile[] = []
      for (const file of files) {
        const blob = await api.putBlob(new Uint8Array(await file.arrayBuffer()))
        uploaded.push({ blob, name: file.name, mime: mimeOf(file), size: file.size })
      }
      return api.addHealthDocument(name, kind, date ?? null, uploaded)
    })
  }

  async saveDocument(document: HealthDocument): Promise<HealthDocument | null> {
    if (!app.supportsHealth) return null
    const plain = { ...$state.snapshot(document), updatedAt: new Date().toISOString() }
    plain.title = plain.title.trim()
    return this.#save('The document could not be saved', () => api.saveHealthDocument(plain))
  }

  /** Asks first, offering to delete the results accepted from it too. */
  deleteDocument(document: HealthDocument): void {
    const id = document.id
    const remove = async (deleteLabs: boolean) => {
      const done = await this.#drop('The document could not be deleted', () =>
        api.deleteHealthDocument(id, deleteLabs),
      )
      if (done && this.documentId === id) this.openDocument(null)
    }
    const hasResults = this.labs.some((l) => l.documentId === id)
    this.#confirm(
      `Delete “${document.title}”?`,
      hasResults
        ? 'The document and its files are deleted. The results accepted from it stay, without a document.'
        : 'The document and its files are deleted from this vault for good.',
      'Delete',
      () => remove(false),
      hasResults ? { label: 'Delete with its results', run: () => remove(true) } : undefined,
    )
  }

  /** Read lab results out of it with the model. A PDF's text layer is read
   *  here (pdf.js) and sent along; a scan's pages go as pictures. */
  async extractDocument(id: HealthDocumentId): Promise<void> {
    if (!app.supportsHealth) return
    const known = this.documents.find((d) => d.id === id)
    const before = known?.extraction
    // On screen first: it is being read.
    if (known) known.extraction = 'running'
    const done = await this.#save(
      'The document could not be read',
      async () => {
        const doc = known ?? (await api.getHealthDocument(id))
        const pdf = doc.files.find((f) => f.mime === 'application/pdf')
        const text = pdf ? await pdfText(pdf.blob) : undefined
        return api.extractHealthDocument(id, text)
      },
      'none',
    )
    if (done) {
      const at = this.documents.findIndex((d) => d.id === id)
      if (at >= 0) this.documents[at] = done
    } else if (known && before) {
      known.extraction = before
    }
    // A failed read leaves the document as the vault has it, with its error.
    await this.#loadPane(true)
  }

  /** Save the reviewed results and clear them from the pending list. */
  async acceptLabs(id: HealthDocumentId, labs: LabResult[]): Promise<void> {
    if (!app.supportsHealth) return
    const plain = $state.snapshot(labs)
    const saved = await this.#save('The results could not be saved', () =>
      api.acceptDocumentLabs(id, plain),
    )
    if (saved) {
      notify.success(`Saved ${plural(saved.length, 'result')}.`, { key: 'health-accepted' })
    }
  }

  /** An address the viewer can open one of a document's files at. Only a file
   *  the document lists is given one. */
  documentFileUrl(document: HealthDocument, blob: BlobId): string {
    return document.files.some((f) => f.blob === blob) ? mediaUrl(blob) : ''
  }

  // ── nutrition ────────────────────────────────────────────────────────

  async searchFoods(query: string): Promise<FoodHit[]> {
    const text = query.trim()
    if (!app.supportsHealth || !text) return []
    try {
      return await api.searchFoods(text)
    } catch (e) {
      await this.#fail(e, 'The food search failed')
      return []
    }
  }

  async lookupBarcode(code: string): Promise<FoodHit | null> {
    const text = code.trim()
    if (!app.supportsHealth || !text) return null
    try {
      return await api.lookupBarcode(text)
    } catch (e) {
      await this.#fail(e, 'The barcode could not be looked up')
      return null
    }
  }

  async saveFood(food: Food): Promise<Food | null> {
    if (!app.supportsHealth) return null
    const plain = { ...$state.snapshot(food), updatedAt: new Date().toISOString() }
    plain.name = plain.name.trim()
    return this.#save('The food could not be saved', () => api.saveFood(plain))
  }

  deleteFood(food: Food): void {
    const id = food.id
    this.#confirm(
      `Delete “${food.name}”?`,
      'It is taken off your list of foods. Meals already logged with it keep what they were worked out as.',
      'Delete',
      async () => {
        await this.#drop('The food could not be deleted', () => api.deleteFood(id))
      },
    )
  }

  async saveMeal(meal: Meal): Promise<Meal | null> {
    if (!app.supportsHealth) return null
    const plain = { ...$state.snapshot(meal), updatedAt: new Date().toISOString() }
    plain.name = plain.name.trim()
    const saved = await this.#save('The meal could not be saved', () => api.saveMeal(plain))
    if (saved && this.editingMeal?.id === saved.id) this.editingMeal = null
    return saved
  }

  deleteMeal(meal: Meal): void {
    const id = meal.id
    this.#confirm(
      meal.name ? `Delete “${meal.name}”?` : 'Delete this meal?',
      'The meal is deleted from this vault for good.',
      'Delete',
      async () => {
        const done = await this.#drop('The meal could not be deleted', () => api.deleteMeal(id))
        if (done && this.editingMeal?.id === id) this.editingMeal = null
      },
    )
  }

  /** Upload the photo and ask the model what is on the plate. */
  async estimatePhoto(file: File): Promise<{ photo: BlobId; items: MealItem[] } | null> {
    if (!app.supportsHealth) return null
    return this.#save(
      'The photo could not be read',
      async () => {
        const photo = await api.putBlob(new Uint8Array(await file.arrayBuffer()))
        return { photo, items: await api.estimateMealPhoto(photo) }
      },
      'none',
    )
  }

  async mealResponse(id: MealId): Promise<MealResponse | null> {
    if (!app.supportsHealth) return null
    try {
      return await api.mealResponse(id)
    } catch (e) {
      await this.#fail(e, 'The glucose response could not be read')
      return null
    }
  }

  // ── sources and settings ─────────────────────────────────────────────

  async saveSettings(settings: HealthSettings): Promise<void> {
    if (!app.supportsHealth) return
    const plain = $state.snapshot(settings)
    // On screen first: a unit chosen changes every number at once.
    this.settings = plain
    const done = await this.#drop(
      'The settings could not be saved',
      () => api.saveHealthSettings(plain),
      'both',
    )
    if (!done) await this.#loadBasics(true)
  }

  async createSource(connector: Connector, name: string): Promise<HealthSource | null> {
    if (!app.supportsHealth) return null
    const now = new Date().toISOString()
    const source: HealthSource = {
      id: newId(),
      connector,
      name:
        name.trim() || (connector === 'appleExport' ? 'Apple Health export' : 'Health Auto Export'),
      paused: false,
      status: { lastError: '', samplesTotal: 0, automations: [], devices: [] },
      tokenId: '',
      createdAt: now,
      updatedAt: now,
    }
    return this.#save('The source could not be added', () => api.saveHealthSource(source), 'basics')
  }

  async saveSource(source: HealthSource): Promise<void> {
    if (!app.supportsHealth) return
    const plain = { ...$state.snapshot(source), updatedAt: new Date().toISOString() }
    plain.name = plain.name.trim()
    await this.#drop('The source could not be saved', () => api.saveHealthSource(plain), 'basics')
  }

  /** Asks first. What it brought stays. A phone's token is revoked before the
   *  source goes, so deleting one never leaves the phone able to write. */
  deleteSource(source: HealthSource): void {
    const id = source.id
    const phone = source.connector === 'healthAutoExport' || source.tokenId !== ''
    this.#confirm(
      `Delete “${source.name}”?`,
      'Nothing else is sent through it, and its token stops working. What it already brought stays.',
      'Delete',
      async () => {
        // If the token cannot be revoked the source stays, with the reason said:
        // a deleted source whose token still works is the one outcome to avoid.
        if (
          phone &&
          !(await this.#drop(
            'The phone’s token could not be revoked, so the source was not deleted',
            () => api.healthPushRevoke(id),
            'none',
          ))
        ) {
          return
        }
        await this.#drop(
          'The source could not be deleted',
          () => api.deleteHealthSource(id),
          'basics',
        )
      },
    )
  }

  /** Mint the phone's token for a source (a shell call). The token is
   *  shown once and kept nowhere here. */
  async pushSetup(sourceId: HealthSourceId): Promise<PushSetup | null> {
    if (!app.supportsHealth) return null
    return this.#save(
      'The phone’s address could not be made',
      () => api.healthPushSetup(sourceId),
      'none',
    )
  }

  /** Revoke the phone's token for a source. The vault answers whether there
   *  was one; only then is the phone told it can no longer send. */
  async revokePush(sourceId: HealthSourceId): Promise<void> {
    if (!app.supportsHealth) return
    const had = await this.#save(
      'The token could not be revoked',
      () => api.healthPushRevoke(sourceId),
      'basics',
    )
    if (had) notify.success('The phone can no longer send readings.', { key: 'health-revoked' })
  }

  /** Pick an `export.zip` and import it (a shell call); polls until done. */
  async importApple(): Promise<void> {
    if (!app.supportsHealth) return
    const status = await this.#save(
      'The import could not be started',
      () => api.healthImportApple(),
      'none',
    )
    // Nothing was picked.
    if (!status) return
    await this.#watchImport(status)
  }

  async cancelImport(): Promise<void> {
    if (!app.supportsHealth) return
    await this.#drop('The import could not be stopped', () => api.cancelAppleImport(), 'none')
    const status = await this.#readImport()
    if (status) await this.#watchImport(status)
  }

  async saveKeys(fdcKey: string): Promise<void> {
    if (!app.supportsHealth) return
    const key = fdcKey.trim()
    if (!key) return
    const done = await this.#drop(
      'The key could not be saved',
      () => api.saveHealthKeys(key),
      'none',
    )
    if (done) {
      await this.#loadKeys()
      notify.success('Food database key saved.', { key: 'health-keys' })
    }
  }

  async clearKeys(): Promise<void> {
    if (!app.supportsHealth) return
    const done = await this.#drop(
      'The key could not be removed',
      () => api.clearHealthKeys(),
      'none',
    )
    if (done) await this.#loadKeys()
  }

  // ── asking ───────────────────────────────────────────────────────────

  /**
   * Ask before something that cannot be taken back. One question at a time:
   * a second replaces the first, which is answered no by never being run.
   * The question is gone from the screen before what it asked runs, so a
   * slow delete is not asked again by a second click.
   */
  #confirm(
    title: string,
    body: string,
    confirm: string,
    run: () => Promise<void>,
    alternative?: { label: string; run: () => Promise<void> },
  ): void {
    const question: HealthQuestion = {
      title,
      body,
      confirm,
      run: async () => {
        if (this.question === question) this.question = null
        await run()
      },
      alternative: alternative
        ? {
            label: alternative.label,
            run: async () => {
              if (this.question === question) this.question = null
              await alternative.run()
            },
          }
        : null,
    }
    this.question = question
  }

  // ── writing ──────────────────────────────────────────────────────────

  /** Keep a write in view until it settles. See `flush`. */
  #track<T>(write: Promise<T>): Promise<T> {
    this.#writes.add(write)
    const done = () => this.#writes.delete(write)
    void write.then(done, done)
    return write
  }

  /** A failure, said: a lock goes to the lock screen, anything else is a
   *  notice. What failed is named; the vault's own sentence follows. */
  async #fail(e: unknown, what: string): Promise<void> {
    if (isLocked(e)) {
      await handle(e)
      return
    }
    notify.error(what, { body: errorMessage(e), key: 'health-failed' })
  }

  /** What was touched, read again. */
  async #refresh(reload: Reload): Promise<void> {
    if (reload === 'none') return
    await Promise.all([
      reload === 'basics' || reload === 'both' ? this.#loadBasics(true) : undefined,
      reload === 'pane' || reload === 'both' ? this.#loadPane(true) : undefined,
    ])
  }

  /**
   * One write: tracked, said if it fails, and what it touched read again.
   * Answers what the vault answered, or `null` when it did not.
   */
  async #save<T>(what: string, work: () => Promise<T>, reload: Reload = 'pane'): Promise<T | null> {
    this.#busy(1)
    try {
      const value = await this.#track(work())
      await this.#refresh(reload)
      return value
    } catch (e) {
      await this.#fail(e, what)
      return null
    } finally {
      this.#busy(-1)
    }
  }

  /** A write that answers nothing worth keeping: did it work? */
  async #drop(
    what: string,
    work: () => Promise<unknown>,
    reload: Reload = 'pane',
  ): Promise<boolean> {
    const done = await this.#save(
      what,
      async () => {
        await work()
        return true
      },
      reload,
    )
    return done === true
  }

  /** `busy` stays on until the last read or write answers. */
  #busy(by: 1 | -1): void {
    this.#loads = Math.max(0, this.#loads + by)
    this.busy = this.#loads > 0
  }

  // ── importing ────────────────────────────────────────────────────────

  async #readImport(): Promise<ImportStatus | null> {
    try {
      const status = await api.appleImportStatus()
      this.importStatus = status
      return status
    } catch (e) {
      await quietly(e)
      return null
    }
  }

  /** On `start`: an import begun before this window was opened is still an
   *  import, and is asked after like one. */
  async #loadImport(): Promise<void> {
    const status = await this.#readImport()
    if (status?.state === 'running') await this.#watchImport(status)
  }

  /**
   * Keep asking how a running import is going, every `pollMs`, until it is
   * not running. The timer exists only while an import does: it is armed
   * here, cleared when the import ends, when another begins and when the
   * vault locks.
   */
  async #watchImport(status: ImportStatus): Promise<void> {
    this.#stopPolling()
    const token = this.#importGen.next()
    this.importStatus = status
    if (status.state === 'running') {
      this.#schedulePoll(token)
      return
    }
    await this.#importEnded(status)
  }

  #schedulePoll(token: number): void {
    this.#importTimer = setTimeout(() => {
      this.#importTimer = null
      void this.#poll(token)
    }, this.pollMs)
  }

  async #poll(token: number): Promise<void> {
    if (!this.#importGen.isCurrent(token)) return
    let status: ImportStatus
    try {
      status = await api.appleImportStatus()
    } catch (e) {
      // Locked, or the shell went away: stop asking.
      if (this.#importGen.isCurrent(token)) await this.#fail(e, 'The import could not be followed')
      return
    }
    if (!this.#importGen.isCurrent(token)) return
    this.importStatus = status
    if (status.state === 'running') {
      this.#schedulePoll(token)
      return
    }
    await this.#importEnded(status)
  }

  /** It finished, failed or was stopped: what it brought is read, and said. */
  async #importEnded(status: ImportStatus): Promise<void> {
    if (status.state === 'done') {
      const added = status.report.samplesAdded
      notify.success('Apple Health import finished.', {
        body: `${plural(added, 'reading')} added${
          status.report.workoutsAdded > 0
            ? `, ${plural(status.report.workoutsAdded, 'workout')}`
            : ''
        }.`,
        key: 'health-imported',
      })
    } else if (status.state === 'failed') {
      notify.error('The Apple Health import failed.', {
        body: status.error || undefined,
        key: 'health-imported',
      })
    }
    // Sources know how many samples they hold now; the pane may be showing them.
    await Promise.all([this.#loadBasics(true), this.#loadPane(true)])
  }

  #stopPolling(): void {
    this.#importGen.next()
    if (this.#importTimer) clearTimeout(this.#importTimer)
    this.#importTimer = null
  }

  // ── loading ──────────────────────────────────────────────────────────

  /** The catalogue (once), settings and sources: small, and wanted by every pane. */
  async #loadBasics(quiet: boolean): Promise<void> {
    if (!app.supportsHealth) return
    const token = this.#basicsGen.next()
    try {
      // The sidebar lists every condition and counts the doses due now
      // whichever pane is open, so both are read with the basics rather than
      // only by the panes that draw them in full.
      const [catalog, settings, sources, conditions, dosesDue] = await Promise.all([
        this.catalog.length === 0 ? api.healthCatalog() : Promise.resolve(null),
        api.healthSettings(),
        api.listHealthSources(),
        api.listConditions(),
        api.dosesDue(todayIso(), localZone()),
      ])
      if (!this.#basicsGen.isCurrent(token)) return
      if (catalog) this.catalog = catalog
      this.settings = settings
      this.sources = sources
      this.conditions = conditions
      this.dosesDue = dosesDue
    } catch (e) {
      if (quiet) await quietly(e)
      else await handle(e)
    }
  }

  async #loadKeys(): Promise<void> {
    if (!app.supportsHealth) return
    const token = this.#keysGen.next()
    try {
      const keys = await api.healthKeys()
      if (this.#keysGen.isCurrent(token)) this.keys = keys
    } catch (e) {
      await quietly(e)
    }
  }

  /** The open workout's heart rate and route. */
  async #loadWorkout(): Promise<void> {
    const token = this.#workoutGen.next()
    const id = this.workoutId
    if (!id || !app.supportsHealth) {
      this.workout = null
      return
    }
    try {
      const detail = await api.getWorkout(id)
      if (this.#workoutGen.isCurrent(token)) this.workout = detail
    } catch (e) {
      if (!this.#workoutGen.isCurrent(token)) return
      this.workout = null
      await quietly(e)
    }
  }

  /**
   * What the pane on screen draws. One generation for all of them -- a pane
   * switched while the last one's answer is on its way has no use for it --
   * and every answer lands together, so a pane never draws half of one
   * window and half of another.
   */
  async #loadPane(quiet: boolean): Promise<void> {
    if (!app.supportsHealth || app.screen !== 'main') return
    const view = this.view
    this.#busy(1)
    try {
      await guardedRefresh(
        this.#paneGen,
        async (isCurrent) => {
          try {
            await this.#read(view, isCurrent)
            if (isCurrent()) this.error = null
          } catch (e) {
            // An answer nobody is waiting for any more is not worth a word.
            if (!isCurrent()) return
            if (!isLocked(e)) this.error = errorMessage(e)
            throw e
          }
        },
        { onError: (e) => (quiet ? quietly(e) : handle(e)) },
      )
    } finally {
      this.#busy(-1)
    }
  }

  /** Read what `view` draws and put it on screen, if still wanted. */
  async #read(view: HealthView, isCurrent: () => boolean): Promise<void> {
    const tz = localZone()
    const today = todayIso()
    switch (view) {
      case 'today': {
        const date = this.date
        const [day, due] = await Promise.all([api.healthToday(date, tz), api.dosesDue(date, tz)])
        if (!isCurrent()) return
        this.today = day
        this.dosesDue = due
        return
      }
      case 'activity': {
        const { from, to } = this
        const span = dayInstants(from, to)
        const [activity, workouts] = await Promise.all([
          api.healthActivity(from, to, tz),
          api.listWorkouts(span.from, span.to),
        ])
        if (!isCurrent()) return
        this.activity = activity
        this.workouts = workouts
        if (this.workoutId) void this.#loadWorkout()
        return
      }
      case 'sleep': {
        const { from, to } = this
        const nights = await api.healthSleep(from, to, tz)
        if (isCurrent()) this.sleep = nights
        return
      }
      case 'vitals': {
        const { from, to } = this
        const metric = this.vitalsMetric
        const query = (m: Metric): SeriesQuery => ({
          metric: m,
          from,
          to,
          tz,
          bucket: bucketFor(from, to),
          stat: { type: 'auto' },
        })
        const [series, second, latest] = await Promise.all([
          api.healthSeries(query(metric)),
          // Blood pressure is two numbers; the second is read beside the first.
          metric === 'blood_pressure_systolic'
            ? api.healthSeries(query('blood_pressure_diastolic'))
            : Promise.resolve(null),
          api.healthLatest([...VITALS_METRICS], to < today ? to : today),
        ])
        if (!isCurrent()) return
        this.series = series
        this.seriesSecond = second
        this.latest = latest
        return
      }
      case 'nutrition': {
        const { from, to } = this
        const span = dayInstants(from, to)
        const [meals, nutrition, foods] = await Promise.all([
          api.listMeals(span.from, span.to),
          api.healthNutrition(from, to, tz),
          api.listFoods(''),
        ])
        if (!isCurrent()) return
        this.meals = meals
        this.nutrition = nutrition
        this.foods = foods
        return
      }
      case 'medications': {
        const since = addDays(today, -(RECENT_DAYS - 1))
        const span = dayInstants(since, today)
        const [medications, due, alerts, doses, allergies] = await Promise.all([
          api.listMedications(),
          api.dosesDue(today, tz),
          api.medicationAlerts(since, today, tz),
          api.listDoses(span.from, span.to),
          api.listAllergies(),
        ])
        if (!isCurrent()) return
        this.medications = medications
        this.dosesDue = due
        this.alerts = alerts
        this.doses = doses
        this.allergies = allergies
        return
      }
      case 'conditions': {
        const [conditions, templates] = await Promise.all([
          api.listConditions(),
          api.conditionTemplates(),
        ])
        if (!isCurrent()) return
        this.conditions = conditions
        this.templates = templates
        const open = conditions.find((c) => c.id === this.conditionId)
        if (!open) {
          // Closed, or deleted elsewhere.
          this.conditionId = null
          this.conditionReport = null
          this.glucose = null
          this.timeline = null
          return
        }
        const glucoseCondition = isGlucoseCondition(open)
        const date = this.date
        // The dashboard lists the condition's medicines with how steadily
        // they were taken (the last thirty days of doses), draws lab trends,
        // and starts a visit brief from the last visit note.
        const since = addDays(today, -(RECENT_DAYS - 1))
        const span = dayInstants(since, today)
        const [report, glucose, timeline, doses, trends, documents] = await Promise.all([
          api.conditionReport(open.id, today, tz),
          glucoseCondition
            ? api.healthGlucose(addDays(today, -(GLUCOSE_DAYS - 1)), today, tz)
            : Promise.resolve(null),
          glucoseCondition ? api.healthDayTimeline(date, tz) : Promise.resolve(null),
          api.listDoses(span.from, span.to),
          api.labTrends(),
          api.listHealthDocuments(),
        ])
        if (!isCurrent()) return
        this.conditionReport = report
        this.glucose = glucose
        this.timeline = timeline
        this.doses = doses
        this.labTrends = trends
        this.documents = documents
        return
      }
      case 'records': {
        const [documents, trends, labs] = await Promise.all([
          api.listHealthDocuments(),
          api.labTrends(),
          api.listLabs(),
        ])
        if (!isCurrent()) return
        this.documents = documents
        this.labTrends = trends
        this.labs = labs
        if (this.documentId && !documents.some((d) => d.id === this.documentId)) {
          this.documentId = null
        }
        return
      }
    }
  }
}

export const health = new HealthState()
