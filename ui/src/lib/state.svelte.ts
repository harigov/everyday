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

export type Screen = 'loading' | 'setup' | 'locked' | 'main' | 'error'

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

  #saveTimer: ReturnType<typeof setTimeout> | null = null
  #searchTimer: ReturnType<typeof setTimeout> | null = null
  #lockTimer: ReturnType<typeof setInterval> | null = null

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
    // Drop decrypted content from the interface at the same moment the core
    // drops it from memory; a locked app must not leave the last entry
    // sitting behind the lock screen.
    this.stopTimers()
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
  }

  /** Called from real user interaction to defer the idle auto-lock. */
  touch() {
    void api.touch().catch(() => {})
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
    this.selectedJournal = id
    this.query = ''
    this.results = []
    await this.refreshEntries()
  }

  async saveJournal(journal: Journal) {
    await api.saveJournal(journal)
    await this.refreshJournals()
  }

  async deleteJournal(id: JournalId) {
    await api.deleteJournal(id)
    if (this.selectedJournal === id) this.selectedJournal = null
    await this.refreshJournals()
    await this.refreshEntries()
  }

  // ── entries ──────────────────────────────────────────────────────────

  async refreshEntries() {
    try {
      this.entries = await api.entries({
        journalId: this.selectedJournal,
        starred: this.showStarredOnly ? true : null,
        sort: 'dateDesc',
        limit: 500,
      })
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
    // Never let a pending autosave land after we have switched documents.
    await this.flush()
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
    this.saving = true
    try {
      entry.updatedAt = new Date().toISOString()
      await api.saveEntry($state.snapshot(entry) as Entry)
      this.lastSaved = entry.updatedAt
      // Refresh the row in place so the list reflects the new title and
      // excerpt without the flicker of a full reload.
      const rows = await api.entries({
        journalId: this.selectedJournal,
        starred: this.showStarredOnly ? true : null,
        sort: 'dateDesc',
        limit: 500,
      })
      this.entries = rows
    } catch (e) {
      if (isLocked(e)) return void (await this.lock())
      this.error = errorMessage(e)
    } finally {
      this.saving = false
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
