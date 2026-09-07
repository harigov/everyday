// Application state.
//
// One store, using Svelte 5 runes. It owns every piece of vault state the
// interface reads, and it is the only place that calls `api`, so components
// stay declarative and the save/lock logic lives in one auditable spot.

import { api, isMock } from './api'
import type {
  Bootstrap, Entry, EntryId, EntrySummary, Journal, JournalId, SearchHit, VaultStatus,
} from './types'
import { VaultError } from './types'

/** Idle delay before an edited entry is written. */
const AUTOSAVE_MS = 700
/** How often the backend is asked whether the idle timeout has elapsed. */
const AUTOLOCK_POLL_MS = 5_000
/** Floor between "the user is still here" pings to the backend. */
const TOUCH_MS = 15_000
/** Floor between list refreshes triggered by an autosave. */
const LIST_REFRESH_MS = 4_000

export type Screen = 'loading' | 'setup' | 'locked' | 'main' | 'error'

/**
 * Which app the sidebar is showing.
 *
 * A vault holds more than a journal now. The section is the one piece of
 * chrome state that outlives a lock, so it is remembered locally -- coming
 * back to the app you were last in is what makes it feel like one program
 * rather than three bolted together.
 */
export const SECTIONS = ['journal', 'todo', 'calendar'] as const
export type Section = (typeof SECTIONS)[number]

function isLocked(e: unknown): boolean {
  return e instanceof VaultError && e.code === 'locked'
}

export function errorMessage(e: unknown): string {
  if (e instanceof VaultError) return e.message
  if (e instanceof Error) return e.message
  return String(e)
}

class AppState {
  screen = $state<Screen>('loading')
  boot = $state<Bootstrap | null>(null)
  status = $state<VaultStatus | null>(null)
  error = $state<string | null>(null)

  journals = $state<Journal[]>([])
  entries = $state<EntrySummary[]>([])
  /** `null` means "all journals". */
  selectedJournal = $state<JournalId | null>(null)
  selectedEntry = $state<EntryId | null>(null)
  entry = $state<Entry | null>(null)

  query = $state('')
  results = $state<SearchHit[]>([])
  searching = $state(false)

  showStarredOnly = $state(false)
  saving = $state(false)
  lastSaved = $state<string | null>(null)
  theme = $state<'light' | 'dark' | 'system'>('system')
  section = $state<Section>('journal')

  /**
   * Things to drop when the vault locks.
   *
   * The todo store registers one of these rather than being imported here.
   * A lock must clear *every* decrypted thing the interface is holding, and
   * the alternative -- this file reaching into each app's store -- is a
   * circular import and a list that is quietly wrong the first time someone
   * adds an app and forgets to extend it.
   */
  #resetHooks: (() => void)[] = []

  #saveTimer: ReturnType<typeof setTimeout> | null = null
  #searchTimer: ReturnType<typeof setTimeout> | null = null
  #lockTimer: ReturnType<typeof setInterval> | null = null
  #lastTouch = 0
  #locking = false
  #lastListRefresh = 0
  #listTimer: ReturnType<typeof setTimeout> | null = null

  /**
   * How to obtain the entry body, registered by the editor.
   *
   * The editor is the owner of the document while it is open. Pushing the
   * serialised JSON into `entry.body` on every keystroke meant walking and
   * copying the whole document per character, and -- because `entry` is deep
   * reactive state -- waking every effect that touches the open entry. The
   * body is pulled once, at save time, instead.
   */
  #bodySource: (() => Entry['body']) | null = null

  // ── lifecycle ────────────────────────────────────────────────────────

  async start() {
    // A mock build accepts `?theme=` so the interface can be reviewed in a
    // fixed theme without clicking through to Settings first.
    const forced = isMock ? new URLSearchParams(location.search).get('theme') : null
    this.theme =
      (forced as typeof this.theme | null) ??
      (localStorage.getItem('everyday.theme') as typeof this.theme) ??
      'system'
    this.applyTheme()
    // `?section=` alongside `?theme=`, and for the same reason: so the
    // interface can be opened straight to the app under review.
    const asked = isMock ? new URLSearchParams(location.search).get('section') : null
    const remembered = asked ?? localStorage.getItem('everyday.section')
    if (remembered === 'journal' || remembered === 'todo' || remembered === 'calendar') {
      this.section = remembered
    }
    try {
      const boot = await api.bootstrap()
      this.boot = boot
      this.status = boot.status
      this.error = null
      if (!boot.vaultExists) this.screen = 'setup'
      else if (boot.status?.unlocked) await this.enterMain()
      else this.screen = 'locked'
    } catch (e) {
      // Deliberately NOT the setup screen. "Create a journal" in response to
      // a backend failure invites the user to make a second vault while the
      // first one is sitting there intact but unreachable -- and it reads as
      // though their entries are gone. Say what actually happened instead.
      this.error = errorMessage(e)
      this.screen = 'error'
    }
  }

  /** Register state to be dropped when the vault locks. */
  onLock(reset: () => void) {
    this.#resetHooks.push(reset)
  }

  /** Does this vault's backend carry the task domain? */
  get supportsTasks(): boolean {
    return this.status?.capabilities?.tasks === true
  }

  /**
   * Can this vault run the calendar app?
   *
   * *Both* domains, not just the calendar one. The grid draws time blocks --
   * which live in the task domain and always have -- underneath events from
   * subscribed calendars. A backend with one and not the other could not
   * draw a calendar worth the name, so the app is offered only for the pair.
   */
  get supportsCalendar(): boolean {
    return this.supportsTasks && this.status?.capabilities?.calendars === true
  }

  /** Is this section available on the vault that is open? */
  canShow(section: Section): boolean {
    if (section === 'todo') return this.supportsTasks
    if (section === 'calendar') return this.supportsCalendar
    return true
  }

  setSection(section: Section) {
    if (!this.canShow(section)) return
    this.section = section
    localStorage.setItem('everyday.section', section)
  }

  /**
   * Move to the next app the open vault can offer. What Ctrl/Cmd J does.
   *
   * A cycle rather than a toggle, now that there are three, and it skips
   * what the backend does not carry -- so on a Markdown vault the shortcut
   * is a no-op rather than a way to reach a screen that cannot work.
   */
  nextSection() {
    const available = SECTIONS.filter((s) => this.canShow(s))
    if (available.length < 2) return
    const at = available.indexOf(this.section)
    this.setSection(available[(at + 1) % available.length]!)
  }

  /** Retry the initial handshake after a failure. */
  async retry() {
    this.screen = 'loading'
    this.error = null
    await this.start()
  }

  applyTheme() {
    const el = document.documentElement
    if (this.theme === 'system') el.removeAttribute('data-theme')
    else el.setAttribute('data-theme', this.theme)
    localStorage.setItem('everyday.theme', this.theme)
  }

  setTheme(t: typeof this.theme) {
    this.theme = t
    this.applyTheme()
  }

  async createVault(opts: { path: string; name: string; backend: string; password: string | null }) {
    this.error = null
    try {
      this.status = await api.createVault(opts)
      await this.enterMain()
    } catch (e) {
      this.error = errorMessage(e)
      throw e
    }
  }

  async unlock(password: string) {
    this.error = null
    try {
      this.status = await api.unlock(password)
      await this.enterMain()
    } catch (e) {
      this.error = errorMessage(e)
      throw e
    }
  }

  async lock() {
    // Re-entrant: `flush` below routes a "locked" failure back here, and the
    // auto-lock poll can arrive while a manual lock is already in flight.
    if (this.#locking) return
    this.#locking = true
    try {
      await this.#lock()
    } finally {
      this.#locking = false
    }
  }

  async #lock() {
    // Write before dropping the entry, or a lock taken within the autosave
    // window throws away whatever was typed in it.
    await this.flush()
    // Drop decrypted content from the interface at the same moment the core
    // drops it from memory; a locked app must not leave the last entry
    // sitting behind the lock screen.
    this.stopTimers()
    for (const reset of this.#resetHooks) reset()
    this.entry = null
    this.entries = []
    this.journals = []
    this.results = []
    this.query = ''
    this.selectedEntry = null
    this.status = await api.lock()
    this.screen = 'locked'
  }

  private async enterMain() {
    this.screen = 'main'
    // A vault opened on a backend that stores journals only cannot show the
    // section the last one left us in.
    if (!this.canShow(this.section)) this.section = 'journal'
    await this.refreshJournals()
    await this.refreshEntries()
    this.startAutoLockPolling()
  }

  private startAutoLockPolling() {
    this.stopTimers()
    this.#lockTimer = setInterval(async () => {
      try {
        if (await api.pollAutoLock()) await this.lock()
      } catch {
        /* a transient failure here must never crash the interface */
      }
    }, AUTOLOCK_POLL_MS)
  }

  stopTimers() {
    if (this.#lockTimer) clearInterval(this.#lockTimer)
    this.#lockTimer = null
    if (this.#listTimer) clearTimeout(this.#listTimer)
    this.#listTimer = null
  }

  /** Called from real user interaction to defer the idle auto-lock. */
  touch() {
    // The backend needs to know the user is alive, not how fast they type.
    // Unthrottled this was one IPC round trip per keystroke.
    const now = Date.now()
    if (now - this.#lastTouch < TOUCH_MS) return
    this.#lastTouch = now
    void api.touch().catch(() => {})
  }

  // ── the open document ────────────────────────────────────────────────

  /** Register (or with `null`, retire) the editor's document getter. */
  bindBody(fn: (() => Entry['body']) | null) {
    this.#bodySource = fn
  }

  /** Pull the editor's current document into state. Cheap enough per save. */
  syncBody() {
    if (this.entry && this.#bodySource) this.entry.body = this.#bodySource()
  }

  // ── journals ─────────────────────────────────────────────────────────

  async refreshJournals() {
    try {
      this.journals = await api.journals()
    } catch (e) {
      if (isLocked(e)) return void (await this.lock())
      this.error = errorMessage(e)
    }
  }

  async selectJournal(id: JournalId | null) {
    // Switching journals can leave nothing open, which discards the editor.
    // Anything typed in the last 700ms is still sitting on the autosave
    // timer at that point, so it has to go to disk first.
    await this.flush()
    this.selectedJournal = id
    this.query = ''
    this.results = []
    await this.refreshEntries()
  }

  async saveJournal(journal: Journal) {
    await api.saveJournal(journal)
    await this.refreshJournals()
  }

  /** Create a journal and select it. Returns false if it could not be made. */
  async newJournal(name: string, color: string, sortOrder: number): Promise<boolean> {
    try {
      const journal = await api.newJournal(name)
      journal.color = color
      journal.sortOrder = sortOrder
      await this.saveJournal(journal)
      // Only select it once the backend has confirmed it exists, so a
      // journal can never appear in the sidebar without being on disk.
      if (!this.journals.some((j) => j.id === journal.id)) {
        this.error = 'The journal could not be saved.'
        return false
      }
      await this.selectJournal(journal.id)
      return true
    } catch (e) {
      if (isLocked(e)) { await this.lock(); return false }
      this.error = errorMessage(e)
      return false
    }
  }

  async deleteJournal(id: JournalId) {
    await api.deleteJournal(id)
    if (this.selectedJournal === id) this.selectedJournal = null
    await this.refreshJournals()
    await this.refreshEntries()
  }

  // ── entries ──────────────────────────────────────────────────────────

  /**
   * Ask for a list refresh. Coalescing: repeated calls inside the window
   * collapse into the single trailing refresh, so a burst of autosaves costs
   * one re-render rather than one each.
   */
  private queueListRefresh() {
    if (this.#listTimer) return
    const wait = Math.max(0, LIST_REFRESH_MS - (Date.now() - this.#lastListRefresh))
    this.#listTimer = setTimeout(() => {
      this.#listTimer = null
      this.#lastListRefresh = Date.now()
      void this.refreshRows().catch(() => {})
    }, wait)
  }

  /** Run any queued list refresh now. */
  private async settleList() {
    if (!this.#listTimer) return
    clearTimeout(this.#listTimer)
    this.#listTimer = null
    this.#lastListRefresh = Date.now()
    await this.refreshRows()
  }

  /** Re-read the list rows for the current filter. */
  private async refreshRows() {
    this.entries = await api.entries({
      journalId: this.selectedJournal,
      starred: this.showStarredOnly ? true : null,
      sort: 'dateDesc',
      limit: 500,
    })
  }

  async refreshEntries() {
    try {
      if (this.#listTimer) { clearTimeout(this.#listTimer); this.#listTimer = null }
      this.#lastListRefresh = Date.now()
      await this.refreshRows()
      // Keep a selection if it is still in view; otherwise open the newest.
      if (!this.entries.some((e) => e.id === this.selectedEntry)) {
        const first = this.entries[0]
        if (first) await this.openEntry(first.id)
        else { this.selectedEntry = null; this.entry = null }
      }
    } catch (e) {
      if (isLocked(e)) return void (await this.lock())
      this.error = errorMessage(e)
    }
  }

  async openEntry(id: EntryId) {
    // Never let a pending autosave land after we have switched documents,
    // and do not leave the row we are switching away from showing the title
    // it had a few seconds ago.
    await this.flush()
    await this.settleList()
    this.selectedEntry = id
    try {
      this.entry = await api.entry(id)
    } catch (e) {
      if (isLocked(e)) return void (await this.lock())
      this.error = errorMessage(e)
    }
  }

  async newEntry() {
    const journalId = this.selectedJournal ?? this.journals[0]?.id
    if (!journalId) return
    await this.flush()
    const entry = await api.newEntry(journalId)
    await api.saveEntry(entry)
    this.entry = entry
    this.selectedEntry = entry.id
    await this.refreshEntries()
    this.selectedEntry = entry.id
  }

  /** Queue a save. Repeated edits collapse into one write. */
  scheduleSave() {
    if (this.#saveTimer) clearTimeout(this.#saveTimer)
    this.#saveTimer = setTimeout(() => void this.flush(), AUTOSAVE_MS)
  }

  /** Write any pending edit immediately. Safe to call when nothing is dirty. */
  async flush() {
    if (this.#saveTimer) {
      clearTimeout(this.#saveTimer)
      this.#saveTimer = null
    }
    const entry = this.entry
    if (!entry) return
    this.syncBody()
    this.saving = true
    try {
      entry.updatedAt = new Date().toISOString()
      await api.saveEntry($state.snapshot(entry) as Entry)
      this.lastSaved = entry.updatedAt
      // Pick up the new title and excerpt in the list, but not right now.
      //
      // This is on the autosave path: while someone is writing it fired every
      // 700ms, and each one replaced the whole `entries` array, re-rendering
      // every row in the list beside the text they were typing into. The list
      // is a secondary view of an entry the author is looking straight at, so
      // it is allowed to lag -- but the refresh is queued, never dropped, so
      // it always converges once the typing stops.
    } catch (e) {
      if (isLocked(e)) return void (await this.lock())
      this.error = errorMessage(e)
    } finally {
      this.saving = false
      // Not after a save that ended in a lock: the refresh would fire four
      // seconds later against a vault that is no longer open.
      if (this.screen === 'main') this.queueListRefresh()
    }
  }

  async deleteEntry(id: EntryId) {
    if (this.#saveTimer) { clearTimeout(this.#saveTimer); this.#saveTimer = null }
    if (this.selectedEntry === id) { this.entry = null; this.selectedEntry = null }
    await api.deleteEntry(id)
    await this.refreshEntries()
  }

  async toggleStar(id: EntryId) {
    const full = this.entry?.id === id ? this.entry : await api.entry(id)
    full.starred = !full.starred
    await api.saveEntry($state.snapshot(full) as Entry)
    if (this.entry?.id === id) this.entry.starred = full.starred
    await this.refreshEntries()
  }

  // ── search ───────────────────────────────────────────────────────────

  /** Debounced, so typing does not fire a query per keystroke. */
  setQuery(q: string) {
    this.query = q
    if (this.#searchTimer) clearTimeout(this.#searchTimer)
    if (!q.trim()) {
      this.results = []
      this.searching = false
      return
    }
    this.searching = true
    this.#searchTimer = setTimeout(async () => {
      try {
        this.results = await api.search(q, this.selectedJournal, 50)
      } catch (e) {
        if (isLocked(e)) return void (await this.lock())
        this.error = errorMessage(e)
      } finally {
        this.searching = false
      }
    }, 140)
  }

  clearSearch() {
    this.query = ''
    this.results = []
    this.searching = false
  }

  // ── derived ──────────────────────────────────────────────────────────

  get journal(): Journal | null {
    return this.journals.find((j) => j.id === this.selectedJournal) ?? null
  }

  get accent(): string {
    const j = this.entry
      ? this.journals.find((x) => x.id === this.entry!.journalId)
      : this.journal
    return j?.color ?? 'var(--accent)'
  }
}

export const app = new AppState()
