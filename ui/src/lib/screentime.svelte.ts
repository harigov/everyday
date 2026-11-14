// State for Screen time.
//
// A sibling of `money.svelte.ts`. Everything here goes through `api` -- the
// mock backend (`mock-screentime.ts`) stands in for the vault and the shell
// in a browser.
//
// The public names below are the contract `components/ScreenTime*.svelte`
// are written against: each name and signature was laid down before the
// bodies were, and members added since are marked as such in their own
// comments.
//
// Two halves, kept apart because they live in different places:
//
//   - the vault's: reports, the timeline, labels, categories, settings --
//     commands, the same on every machine looking at this vault;
//   - this machine's: whether it tracks, what it is called, how it watches
//     focus, the GNOME extension, browser connections -- shell calls
//     (`screentime_*`), which a remote window answers for itself, not for
//     the vault's machine.
//
// What is loaded follows what is on screen: the summary pane reads a
// report, the timeline pane a day, the apps and categories panes the label
// list. Categories, settings and the devices are small and every pane needs
// one of them, so they are always read.
//
// Rules that shape it:
//
//   1. Every navigation reloads through a `latest()` generation, so when the
//      range is stepped three times quickly only the last answer lands.
//   2. A label edit shows at once and is put back if the vault refuses, as
//      Money's recategorisation is.
//   3. Deleting -- an item's history, a category, a range of time -- asks
//      first, through `question`, which `App.svelte` draws.
//   4. What somebody used is theirs and the sites' words. The assistant is
//      told the pane and the range, never an app's or a site's name.

import { api } from './api'
import { plural } from './format'
import { pref } from './prefs'
import {
  categoryColor,
  keyKind,
  localZone,
  rangeLabel as labelOfRange,
  rangeOf,
  stepAnchor,
  type RangeKind,
} from './screentime'
import type { Showing } from './onscreen'
import { notify } from './notify.svelte'
import { app, errorMessage, handle, isLocked, quietly } from './state.svelte'
import { latest } from './store/latest'
import { optimisticRemove } from './store/optimistic-remove'
import { guardedRefresh } from './store/refresh'
import { localeWeekStart, todayIso } from './time'
import type {
  BrowserConnection,
  ScreenApp,
  ScreenAppRow,
  ScreenCategory,
  ScreenCategoryId,
  ScreenDevice,
  ScreenQuery,
  ScreenReport,
  ScreenTimeSettings,
  ScreenTimeStatus,
  ScreenTimeline,
  SpanKind,
} from './types'

/** The panes of the app, as the sidebar lists them. */
export const SCREEN_VIEWS = ['summary', 'timeline', 'apps', 'categories'] as const
export type ScreenView = (typeof SCREEN_VIEWS)[number]

/** What a label edit may change. */
export type LabelPatch = Partial<
  Pick<ScreenApp, 'name' | 'category' | 'uncategorized' | 'ignored' | 'split'>
>

/** A destructive act waiting to hear yes or no -- shaped for `ChoiceDialog`,
 *  as Money's is. */
export interface ScreenQuestion {
  title: string
  detail: string
  options: { label: string; danger?: boolean; run: () => void }[]
  cancel: () => void
}

const RANGES: readonly RangeKind[] = ['day', 'week', 'month']

/** What Screen time tints the window with: the palette's indigo, apart from
 *  the Feed's blue and Money's green. */
const SCREEN_ACCENT = '#4338ca'

/**
 * At most one reload a minute for heartbeats arriving. A collector flushes
 * every half minute while anything is in use, so without this the summary
 * would be read again twice a minute for a number that moved by thirty
 * seconds.
 */
const LIVE_EVERY_MS = 60_000

/**
 * Which pane was last open, the range the summary covers, and the devices
 * it is narrowed to -- remembered on this device: chrome, not a fact about
 * the vault. Read in the constructor, so the app does not draw the summary
 * and visibly flip.
 */
const viewPref = pref<ScreenView>(
  'karmayoga.screentime.view',
  (raw) => SCREEN_VIEWS.find((v) => v === raw) ?? 'summary',
  'summary',
)
const rangePref = pref<RangeKind>(
  'karmayoga.screentime.range',
  (raw) => RANGES.find((r) => r === raw) ?? 'day',
  'day',
)
const devicesPref = pref<string[]>(
  'karmayoga.screentime.devices',
  (raw) => {
    const parsed: unknown = raw ? JSON.parse(raw) : []
    return Array.isArray(parsed) ? parsed.filter((d): d is string => typeof d === 'string') : []
  },
  [],
  (devices) => JSON.stringify(devices),
)

/**
 * A fresh id for a label or a category that has not been saved yet -- the
 * stopgap `money.svelte.ts`'s `mintId` documents: `save_*` is a plain
 * upsert, there is no `new_*` to mint one, and the app's origins are secure
 * contexts.
 */
function mintId(): string {
  return crypto.randomUUID()
}

class ScreenTimeState {
  /** Opened at least once since the vault did. */
  loaded = $state(false)
  view = $state<ScreenView>('summary')
  range = $state<RangeKind>('day')
  /** A day inside the range on screen, `YYYY-MM-DD`. */
  anchor = $state(todayIso())
  /** Devices the summary and timeline are narrowed to; empty is all. */
  devices = $state<string[]>([])
  /** The summary's category filter; `null` is all. */
  category = $state<ScreenCategoryId | null>(null)

  report = $state<ScreenReport | null>(null)
  /** The day the timeline pane shows, `YYYY-MM-DD`. */
  timelineDate = $state(todayIso())
  timeline = $state<ScreenTimeline | null>(null)
  apps = $state<ScreenAppRow[]>([])
  categories = $state<ScreenCategory[]>([])
  knownDevices = $state<ScreenDevice[]>([])
  settings = $state<ScreenTimeSettings | null>(null)
  /** This machine's collector, from the shell. */
  status = $state<ScreenTimeStatus | null>(null)

  /** A read or write is in flight. */
  busy = $state(false)
  question = $state.raw<ScreenQuestion | null>(null)

  // -- added after the skeleton ---------------------------------------------
  /**
   * Why this machine's status could not be read -- a shell without screen
   * time, a window with no shell at all -- or `null`. `status` is `null`
   * beside it; the Settings tab says this instead of drawing switches that
   * would do nothing.
   */
  statusError = $state<string | null>(null)
  /** "Start the next thing" (`c`) asked for a new category: the categories
   *  pane opens and its form takes this as the cue to focus. Cleared by
   *  whoever acts on it. */
  wantsNewCategory = $state(false)

  /** Which load is the current one, per thing loaded. */
  #basicsGen = latest()
  #paneGen = latest()
  #appsGen = latest()
  #statusGen = latest()
  /** Pane loads in flight, so `busy` lasts until the last answers. */
  #loads = 0
  /** When heartbeats last caused a reload. See `LIVE_EVERY_MS`. */
  #liveAt = 0
  /** Read once: `localeWeekStart` builds an `Intl.Locale`, and `from`/`to`
   *  are read on every render. */
  #weekStart = localeWeekStart()
  /** Writes sent and not yet answered, so the close handshake can wait. */
  #writes = new Set<Promise<unknown>>()

  constructor() {
    // Registered once, here, as Money's are: the view calls `start` on
    // every mount.
    app.onLock(() => this.reset())
    app.onFlush(() => this.flush())
    this.view = viewPref.get()
    this.range = rangePref.get()
    this.devices = devicesPref.get()
  }

  // ── what is on screen ────────────────────────────────────────────────

  /** First and last day of the range on screen. */
  get from(): string {
    return rangeOf(this.range, this.anchor, this.#weekStart).from
  }
  get to(): string {
    return rangeOf(this.range, this.anchor, this.#weekStart).to
  }
  /** "Today", "3–9 Nov", ... */
  get rangeLabel(): string {
    return labelOfRange(this.range, this.from, this.to, todayIso())
  }
  /** Is the range on screen the current one (nothing to step forward to)? */
  get atPresent(): boolean {
    return this.to >= todayIso()
  }

  /** Categories in their order. Added after the skeleton. */
  get sortedCategories(): ScreenCategory[] {
    return [...this.categories].sort(
      (a, b) => a.sortOrder - b.sortOrder || a.name.localeCompare(b.name),
    )
  }

  categoryOf(id: ScreenCategoryId | null | undefined): ScreenCategory | undefined {
    if (!id) return undefined
    return this.categories.find((c) => c.id === id)
  }
  /** The colour a category (or uncategorised, `null`) is drawn in. */
  colorOf(id: ScreenCategoryId | null | undefined): string {
    return categoryColor(this.categoryOf(id), this.categories)
  }

  /** The colour the window tints itself while Screen time is open. Added
   *  after the skeleton, for `apps.ts`. */
  get accent(): string {
    return SCREEN_ACCENT
  }

  /**
   * What is on screen, for the assistant -- see `onscreen.ts`. Added after
   * the skeleton.
   *
   * The pane and the range in the interface's own words. Never an app's or
   * a site's name, nor a device's or a category's: those are the person's
   * and the web's, and there is no record kind for the assistant to look
   * them up by.
   */
  get showing(): Showing {
    const range = this.rangeLabel
    const when = range === 'Today' || range === 'Yesterday' ? range.toLowerCase() : range
    const day = labelOfRange('day', this.timelineDate, this.timelineDate, todayIso())
    const on = day === 'Today' || day === 'Yesterday' ? day.toLowerCase() : day
    const where: Record<ScreenView, string> = {
      summary: `their screen time for ${when}`,
      timeline: `the timeline of their screen time for ${on}`,
      apps: 'the apps and websites they use, and how each is labelled',
      categories: 'the categories their screen time is sorted into',
    }
    const narrowed = this.view === 'summary' || this.view === 'timeline'
    const words = [
      where[this.view],
      narrowed && this.devices.length > 0
        ? `narrowed to ${plural(this.devices.length, 'device')}`
        : null,
      this.view === 'summary' && this.category ? 'narrowed to one category' : null,
    ].filter(Boolean)
    return { view: words.join(', ') }
  }

  // ── lifecycle ────────────────────────────────────────────────────────

  /** Called when the app is first opened. Loads what the pane needs, plus
   *  categories, settings and this machine's status. */
  async start(): Promise<void> {
    if (!app.supportsScreenTime) return
    await Promise.all([this.reload(), this.refreshStatus()])
    if (app.screen === 'main') this.loaded = true
  }

  /** Read again whatever is on screen. */
  async reload(): Promise<void> {
    if (!app.supportsScreenTime) return
    // An answer that arrives after a lock must not fill the screen back in.
    if (app.screen !== 'main') return
    await Promise.all([this.#loadBasics(false), this.#loadPane(false)])
  }

  /** Make sure `categories` and `apps` are loaded, for a picker outside
   *  the app (a goal's measure). Cheap when they already are. */
  async ensureLabels(): Promise<void> {
    if (!app.supportsScreenTime || app.screen !== 'main') return
    await Promise.all([
      this.categories.length === 0 ? this.#loadBasics(true) : Promise.resolve(),
      this.apps.length === 0 ? this.#loadApps(true) : Promise.resolve(),
    ])
  }

  /**
   * Forget everything: the vault locked or changed. Added after the skeleton.
   *
   * The pane, the range and the devices stay: they are chrome. What was
   * used and when is exactly what a lock exists to put away. This
   * machine's status goes too -- the next unlock asks again.
   */
  reset(): void {
    this.question?.cancel()
    this.question = null
    for (const gen of [this.#basicsGen, this.#paneGen, this.#appsGen, this.#statusGen]) gen.next()
    this.#loads = 0
    this.#liveAt = 0
    this.loaded = false
    this.anchor = todayIso()
    this.timelineDate = todayIso()
    this.category = null
    this.report = null
    this.timeline = null
    this.apps = []
    this.categories = []
    this.knownDevices = []
    this.settings = null
    this.status = null
    this.statusError = null
    this.busy = false
    this.wantsNewCategory = false
  }

  /** Wait for anything outstanding. Part of the close handshake. */
  async flush(): Promise<void> {
    await Promise.allSettled([...this.#writes])
  }

  /**
   * Heartbeats arrived (a `screenSpan` change). Added after the skeleton,
   * for `live.svelte.ts`.
   *
   * Read again only if this would show anything new: the app open, and a
   * summary that covers today or today's timeline on screen -- a week last
   * month does not move because somebody is typing now. And at most once a
   * minute, see `LIVE_EVERY_MS`.
   */
  spansArrived(): Promise<void> | undefined {
    if (!this.loaded || app.screen !== 'main' || app.section !== 'screentime') return undefined
    const today = todayIso()
    const showsToday =
      (this.view === 'summary' && this.from <= today && this.to >= today) ||
      (this.view === 'timeline' && this.timelineDate === today)
    if (!showsToday) return undefined
    const now = Date.now()
    if (now - this.#liveAt < LIVE_EVERY_MS) return undefined
    this.#liveAt = now
    return this.#loadPane(true)
  }

  /**
   * "Start the next thing" (`c`). Added after the skeleton.
   *
   * Nothing in this app is typed in but a category, so that: the
   * categories pane, with its form asked to take the caret.
   */
  create(): void {
    this.setView('categories')
    this.wantsNewCategory = true
  }

  // ── navigation ───────────────────────────────────────────────────────

  setView(view: ScreenView): void {
    if (view === this.view) return
    this.view = view
    viewPref.set(view)
    void this.#loadPane(true)
  }

  setRange(range: RangeKind): void {
    if (range === this.range) return
    this.range = range
    rangePref.set(range)
    // A range reaching past today is cut back to the one holding today.
    if (this.from > todayIso()) this.anchor = todayIso()
    if (this.view === 'summary') void this.#loadPane(true)
  }

  /** One range back (-1) or forward (+1). */
  step(delta: -1 | 1): void {
    if (delta > 0 && this.atPresent) return
    const next = stepAnchor(this.range, this.anchor, delta)
    const today = todayIso()
    // Forward into the range holding today lands on today, not on the same
    // weekday next week.
    this.anchor = next > today ? today : next
    if (this.view === 'summary') void this.#loadPane(true)
  }

  goToday(): void {
    const today = todayIso()
    this.anchor = today
    this.timelineDate = today
    if (this.view === 'summary' || this.view === 'timeline') void this.#loadPane(true)
  }

  setDevices(devices: string[]): void {
    this.devices = [...new Set(devices)]
    devicesPref.set(this.devices)
    if (this.view === 'summary' || this.view === 'timeline') void this.#loadPane(true)
  }

  setCategory(category: ScreenCategoryId | null): void {
    if (category === this.category) return
    this.category = category
    if (this.view === 'summary') void this.#loadPane(true)
  }

  /** Show one day on the timeline pane. */
  openTimeline(date: string): void {
    const today = todayIso()
    this.timelineDate = date > today ? today : date
    if (this.view !== 'timeline') {
      this.view = 'timeline'
      viewPref.set('timeline')
    }
    void this.#loadPane(true)
  }

  // ---- labels ---------------------------------------------------------

  /** Change a label, making one if the item has none. Shows at once; put
   *  back if the vault refuses. */
  async saveLabel(key: string, kind: SpanKind, patch: LabelPatch): Promise<void> {
    if (!app.supportsScreenTime) return
    // The label as it stands is on the apps list; the summary alone does
    // not carry it, so read the list first when it is not loaded.
    if (!this.apps.some((r) => r.key === key)) await this.#loadApps(true)
    const row = this.apps.find((r) => r.key === key)
    const current = row?.label ? $state.snapshot(row.label) : null
    const now = new Date().toISOString()
    const next: ScreenApp = {
      ...(current ?? {
        id: mintId(),
        key,
        kind,
        name: '',
        category: null,
        uncategorized: false,
        ignored: false,
        split: false,
        createdAt: now,
      }),
      ...patch,
      updatedAt: now,
    }
    next.name = (next.name ?? '').trim()
    // A category chosen is not "none on purpose", and "none on purpose"
    // has no category.
    if (patch.category) next.uncategorized = false
    if (next.uncategorized) next.category = null
    if (next.kind !== 'site') next.split = false

    const before = this.#remember()
    // On screen first: the row's label, and the item's name and colour in
    // the summary. What the item is called with no name of its own is the
    // row's default; its category with none chosen, the suggestion.
    if (row) row.label = next
    const category = next.uncategorized ? null : (next.category ?? row?.suggested ?? null)
    const suggested = !next.uncategorized && !next.category && category !== null
    for (const item of this.report?.items ?? []) {
      if (item.key !== key) continue
      item.name = next.name || row?.defaultName || item.name
      item.category = category
      item.suggested = suggested
      item.appId = next.id
    }
    try {
      await this.#track(api.saveScreenApp(next))
      // Totals by category, an item ignored or split: the report is the
      // vault's to add up again.
      await Promise.all([this.#loadApps(true), this.#loadPane(true)])
    } catch (e) {
      await handle(e, async () => {
        this.#restore(before)
        await this.#loadApps(true)
      })
    }
  }

  /** Drop a label: the item goes back to the collector's name and the
   *  suggested category. */
  async clearLabel(key: string): Promise<void> {
    if (!app.supportsScreenTime) return
    if (!this.apps.some((r) => r.key === key)) await this.#loadApps(true)
    const row = this.apps.find((r) => r.key === key)
    const label = row?.label
    if (!row || !label) return
    const before = this.#remember()
    row.label = null
    for (const item of this.report?.items ?? []) {
      if (item.key !== key) continue
      item.name = row.defaultName
      item.category = row.suggested
      item.suggested = row.suggested !== null
      item.appId = null
    }
    try {
      await this.#track(api.deleteScreenApp(label.id))
      await Promise.all([this.#loadApps(true), this.#loadPane(true)])
    } catch (e) {
      await handle(e, async () => {
        this.#restore(before)
        await this.#loadApps(true)
      })
    }
  }

  /** Delete every span of an item, after asking. */
  forgetItem(key: string, name: string): void {
    void this.#forget(key, name)
  }

  async #forget(key: string, name: string): Promise<void> {
    if (!app.supportsScreenTime) return
    const what = keyKind(key) === 'site' ? 'visit to it' : 'minute in it'
    const yes = await this.#ask(
      `Forget “${name}”?`,
      `Every ${what}, on every device, is deleted from this vault for good. Its label, if it has one, stays.`,
      'Forget',
    )
    if (!yes) return
    try {
      const gone = await this.#track(api.forgetScreenItem(key))
      notify.info(gone === 0 ? 'Nothing was recorded of it.' : `Forgot “${name}”.`, {
        key: 'screentime-forgot',
      })
    } catch (e) {
      await handle(e)
    }
    await this.#afterSpansGone()
  }

  // ---- categories -----------------------------------------------------

  async createCategory(name: string, color: string): Promise<ScreenCategory | null> {
    if (!app.supportsScreenTime) return null
    const trimmed = name.trim()
    if (!trimmed) return null
    const now = new Date().toISOString()
    const made: ScreenCategory = {
      id: mintId(),
      name: trimmed,
      color: color.trim(),
      builtin: '',
      // Last in the order.
      sortOrder: this.categories.reduce((n, c) => Math.max(n, c.sortOrder + 1), 0),
      createdAt: now,
      updatedAt: now,
    }
    this.categories = [...this.categories, made]
    try {
      await this.#track(api.saveScreenCategory(made))
      await this.#loadBasics(true)
      return this.categoryOf(made.id) ?? made
    } catch (e) {
      await handle(e, () => this.#loadBasics(true))
      return null
    }
  }

  async saveCategory(category: ScreenCategory): Promise<void> {
    if (!app.supportsScreenTime) return
    const plain = { ...$state.snapshot(category), updatedAt: new Date().toISOString() }
    plain.name = plain.name.trim()
    const at = this.categories.findIndex((c) => c.id === plain.id)
    if (at >= 0) this.categories[at] = plain
    else this.categories = [...this.categories, plain]
    try {
      await this.#track(api.saveScreenCategory(plain))
      await this.#loadBasics(true)
    } catch (e) {
      await handle(e, () => this.#loadBasics(true))
    }
  }

  /** Delete a category, after asking. */
  deleteCategory(category: ScreenCategory): void {
    void this.#deleteCategory(category)
  }

  async #deleteCategory(category: ScreenCategory): Promise<void> {
    if (!app.supportsScreenTime) return
    const yes = await this.#ask(
      `Delete “${category.name}”?`,
      'What was in it goes back to its suggested category, or to none. Nothing recorded is deleted.',
      'Delete',
    )
    if (!yes) return
    const id = category.id
    let done = false
    await optimisticRemove({
      optimistic: () => {
        this.categories = this.categories.filter((c) => c.id !== id)
        if (this.category === id) this.category = null
      },
      call: () => this.#track(api.deleteScreenCategory(id)),
      onSuccess: () => (done = true),
      rollback: () => this.#loadBasics(true),
    })
    if (done) {
      await Promise.all([this.#loadBasics(true), this.#loadApps(true), this.#loadPane(true)])
    }
  }

  /** Move a category one place up (-1) or down (+1). */
  async moveCategory(category: ScreenCategory, delta: -1 | 1): Promise<void> {
    if (!app.supportsScreenTime) return
    const order = this.sortedCategories
    const at = order.findIndex((c) => c.id === category.id)
    const other = order[at + delta]
    const self = order[at]
    if (at < 0 || !other || !self) return
    // Swap places. Two that share a number (an older vault, a hand edit)
    // are given their positions first, so a swap always moves something.
    const mine = self.sortOrder === other.sortOrder ? at : self.sortOrder
    const theirs = self.sortOrder === other.sortOrder ? at + delta : other.sortOrder
    const now = new Date().toISOString()
    const a: ScreenCategory = { ...$state.snapshot(self), sortOrder: theirs, updatedAt: now }
    const b: ScreenCategory = { ...$state.snapshot(other), sortOrder: mine, updatedAt: now }
    this.categories = this.categories.map((c) => (c.id === a.id ? a : c.id === b.id ? b : c))
    try {
      await this.#track(Promise.all([api.saveScreenCategory(a), api.saveScreenCategory(b)]))
      await this.#loadBasics(true)
    } catch (e) {
      await handle(e, () => this.#loadBasics(true))
    }
  }

  // ---- settings -------------------------------------------------------

  async saveSettings(settings: ScreenTimeSettings): Promise<void> {
    if (!app.supportsScreenTime) return
    const plain = $state.snapshot(settings)
    const keptFewer = (plain.titleDays || Infinity) < (this.settings?.titleDays || Infinity)
    const titlesOff = !plain.keepTitles && this.settings?.keepTitles === true
    this.settings = plain
    try {
      await this.#track(api.saveScreenTimeSettings(plain))
      await this.#loadBasics(true)
      // Keeping titles for fewer days, or not at all, changes the timeline.
      if (keptFewer || titlesOff) await this.#afterSpansGone()
    } catch (e) {
      await handle(e, () => this.#loadBasics(true))
    }
  }

  /** Delete everything recorded between two instants (ISO), after asking. */
  deleteRange(from: string, to: string, label: string): void {
    void this.#deleteRange(from, to, label)
  }

  async #deleteRange(from: string, to: string, label: string): Promise<void> {
    if (!app.supportsScreenTime) return
    const yes = await this.#ask(
      `Delete screen time for ${label}?`,
      'Everything recorded in that time, on every device, is deleted from this vault for good.',
      'Delete',
    )
    if (!yes) return
    try {
      const gone = await this.#track(api.deleteScreenTime(from, to))
      notify.info(gone === 0 ? 'Nothing was recorded then.' : 'Deleted.', {
        key: 'screentime-deleted',
      })
    } catch (e) {
      await handle(e)
    }
    await this.#afterSpansGone()
  }

  // ---- this machine ---------------------------------------------------

  /**
   * Ask the shell how this machine is doing. On `start`, and whenever the
   * Settings tab opens. A shell with no screen time -- an older build, a
   * window with no shell -- answers with an error, which is kept in
   * `statusError` rather than shown as a banner: it is an answer, not a
   * failure.
   */
  async refreshStatus(): Promise<void> {
    const token = this.#statusGen.next()
    try {
      const status = await api.screentimeStatus()
      if (!this.#statusGen.isCurrent(token)) return
      this.status = status
      this.statusError = null
    } catch (e) {
      if (!this.#statusGen.isCurrent(token)) return
      if (isLocked(e)) return quietly(e)
      this.status = null
      this.statusError = errorMessage(e)
    }
  }

  async setTracking(on: boolean): Promise<void> {
    await this.#shell(() => api.screentimeSetTracking(on))
  }

  async setDevice(name: string): Promise<void> {
    const trimmed = name.trim()
    if (!trimmed || trimmed === this.status?.device) return
    await this.#shell(() => api.screentimeSetDevice(trimmed))
    // The new name is what this machine's next heartbeats arrive under.
    void this.#loadBasics(true)
  }

  /** Copy the GNOME Shell extension into place and enable it. */
  async installGnomeExtension(): Promise<void> {
    await this.#shell(() => api.screentimeInstallGnomeExtension())
  }

  /** Mint a connection for a browser extension (`chrome`, `firefox`,
   *  `edge`, ...). Shown once. */
  async connectBrowser(browser: string): Promise<BrowserConnection | null> {
    this.busy = true
    try {
      return await api.screentimeConnectBrowser(browser)
    } catch (e) {
      await handle(e)
      return null
    } finally {
      this.busy = this.#loads > 0
      void this.refreshStatus()
    }
  }

  /** A shell call that answers the status as it is afterwards. */
  async #shell(call: () => Promise<ScreenTimeStatus>): Promise<void> {
    this.#statusGen.next()
    this.busy = true
    try {
      this.status = await call()
      this.statusError = null
    } catch (e) {
      await handle(e)
    } finally {
      this.busy = this.#loads > 0
    }
  }

  // ── asking ───────────────────────────────────────────────────────────

  /** Ask before something that cannot be taken back. One question at a
   *  time: a second answers the first "no". As Money's `#ask`. */
  #ask(title: string, detail: string, yes: string): Promise<boolean> {
    this.question?.cancel()
    return new Promise((resolve) => {
      const answer = (value: boolean) => () => {
        if (this.question === question) this.question = null
        resolve(value)
      }
      const question: ScreenQuestion = {
        title,
        detail,
        options: [{ label: yes, danger: true, run: answer(true) }],
        cancel: answer(false),
      }
      this.question = question
    })
  }

  // ── loading ──────────────────────────────────────────────────────────

  /** Keep a write in view until it settles. See `flush`. */
  #track<T>(write: Promise<T>): Promise<T> {
    this.#writes.add(write)
    const done = () => this.#writes.delete(write)
    void write.then(done, done)
    return write
  }

  /** The summary on screen as a query. */
  #query(): ScreenQuery {
    const query: ScreenQuery = {
      from: this.from,
      to: this.to,
      tz: localZone(),
      devices: [...this.devices],
    }
    if (this.category) query.category = this.category
    return query
  }

  /** Categories, settings and devices: small, and wanted by every pane. */
  async #loadBasics(quiet: boolean): Promise<void> {
    if (!app.supportsScreenTime) return
    const token = this.#basicsGen.next()
    try {
      const [categories, settings, devices] = await Promise.all([
        api.listScreenCategories(),
        api.screenTimeSettings(),
        api.listScreenDevices(),
      ])
      if (!this.#basicsGen.isCurrent(token)) return
      this.categories = categories
      this.settings = settings
      this.knownDevices = devices
      // Narrowed to a category deleted elsewhere: everything, rather than
      // an empty summary under a name the sidebar no longer has.
      if (this.category && !categories.some((c) => c.id === this.category)) {
        this.category = null
        if (this.view === 'summary') void this.#loadPane(true)
      }
    } catch (e) {
      if (quiet) await quietly(e)
      else await handle(e)
    }
  }

  async #loadApps(quiet: boolean): Promise<void> {
    if (!app.supportsScreenTime) return
    const token = this.#appsGen.next()
    try {
      const rows = await api.listScreenApps()
      if (this.#appsGen.isCurrent(token)) this.apps = rows
    } catch (e) {
      if (quiet) await quietly(e)
      else await handle(e)
    }
  }

  /**
   * What the pane on screen draws: a report, a day, or the label list.
   * One generation for all of them -- a pane switched while the last one's
   * answer is on its way has no use for it.
   */
  async #loadPane(quiet: boolean): Promise<void> {
    if (!app.supportsScreenTime) return
    const view = this.view
    if (view === 'apps' || view === 'categories') {
      // The label list has its own generation, since a label edit reads it
      // too; this one only has to retire a report on its way.
      this.#paneGen.next()
      await this.#loadApps(quiet)
      return
    }
    this.#loads += 1
    this.busy = true
    try {
      await guardedRefresh(
        this.#paneGen,
        async (isCurrent) => {
          if (view === 'summary') {
            const report = await api.screenTimeReport(this.#query())
            if (isCurrent()) this.report = report
          } else {
            const date = this.timelineDate || todayIso()
            const devices = this.devices.length > 0 ? [...this.devices] : undefined
            const timeline = await api.screenTimeTimeline(date, localZone(), devices)
            if (isCurrent()) this.timeline = timeline
          }
        },
        { onError: (e) => (quiet ? quietly(e) : handle(e)) },
      )
    } finally {
      this.#loads = Math.max(0, this.#loads - 1)
      this.busy = this.#loads > 0
    }
  }

  /** After spans were deleted: everything that adds them up. */
  async #afterSpansGone(): Promise<void> {
    await Promise.all([this.#loadBasics(true), this.#loadApps(true), this.#loadPane(true)])
  }

  /** What a label edit can change on screen, to put back on a refusal. */
  #remember(): { apps: ScreenAppRow[]; report: ScreenReport | null } {
    return { apps: $state.snapshot(this.apps), report: $state.snapshot(this.report) }
  }

  #restore(before: { apps: ScreenAppRow[]; report: ScreenReport | null }): void {
    this.apps = before.apps
    // Only if no newer report landed meanwhile for another range.
    if (this.report && before.report && this.report.from === before.report.from) {
      this.report = before.report
    }
  }
}

export const screentime = new ScreenTimeState()
