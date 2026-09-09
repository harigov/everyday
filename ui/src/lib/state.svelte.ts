// Application state.
//
// One store, using Svelte 5 runes. It owns every piece of vault state the
// interface reads, and it is the only place that calls `api`, so components
// stay declarative and the save/lock logic lives in one auditable spot.

import { tick } from 'svelte'
import { api, isMock } from './api'
import { AUTOSAVE_MS } from './autosave'
import { notify } from './notify.svelte'
import { todayIso } from './time'
import { TRAY_ORDER, tray } from './tray.svelte'
import type {
  Bootstrap,
  Entry,
  EntryId,
  EntrySummary,
  Journal,
  JournalId,
  SearchHit,
  TrackerId,
  VaultStatus,
} from './types'
import { VaultError } from './types'

/** How often the backend is asked whether the idle timeout has elapsed. */
const AUTOLOCK_POLL_MS = 5_000
/** Floor between "the user is still here" pings to the backend. */
const TOUCH_MS = 15_000
/** Floor between list refreshes triggered by an autosave. */
const LIST_REFRESH_MS = 4_000

/**
 * Longest gap between retries of an entry save that keeps failing.
 *
 * Matches `Autosave`'s ceiling, for the same reason: keep trying, but stop
 * hammering a disk that is not going to accept the write this second.
 */
const MAX_SAVE_RETRY_MS = 30_000

export type Screen = 'loading' | 'setup' | 'locked' | 'main' | 'error'

/**
 * Which app the sidebar is showing.
 *
 * A vault holds more than a journal now. The section is the one piece of
 * chrome state that outlives a lock, so it is remembered locally -- coming
 * back to the app you were last in is what makes it feel like one program
 * rather than four bolted together.
 */
export const SECTIONS = ['journal', 'todo', 'calendar', 'library'] as const
export type Section = (typeof SECTIONS)[number]

/**
 * Was this the vault locking under us rather than a fault?
 *
 * Nearly every caller wants `handle` below instead. This is exported for
 * the handful that deliberately do something else with the distinction --
 * a background refresh that swallows everything, or a dialog that returns
 * its message instead of posting it over the window.
 */
export function isLocked(e: unknown): boolean {
  return e instanceof VaultError && e.code === 'locked'
}

/**
 * Was this save refused because the entry changed elsewhere?
 *
 * Distinct from a failure: nothing is wrong, two people (or two processes)
 * simply wrote the same entry, and the interface has to ask rather than
 * pick a winner.
 */
export function isConflict(e: unknown): boolean {
  return e instanceof VaultError && e.code === 'conflict'
}

export function errorMessage(e: unknown): string {
  if (e instanceof VaultError) return e.message
  if (e instanceof Error) return e.message
  return String(e)
}

/**
 * What every store does when a call into the vault fails.
 *
 * There is one policy and it is this: a vault that locked under us is not an
 * error to report, it is a screen to go to -- the auto-lock can fire in the
 * middle of any call, and telling somebody "locked" in red at the top of the
 * window they are about to be taken away from is noise. Anything else is
 * worth saying.
 *
 * It lives here, beside `app`, and not as a copy in each store. This idiom
 * was written out twenty-six times across the three stores, with three
 * separate definitions of `isLocked` and several methods that had simply
 * forgotten to check -- so a lock during those produced an unhandled
 * rejection instead of the lock screen. The next store to be added gets the
 * behaviour by calling this rather than by remembering to copy it.
 *
 * `revert` re-reads whatever the caller had already changed optimistically,
 * for the writes that update the screen before the disk.
 */
export async function handle(e: unknown, revert?: () => Promise<unknown>): Promise<void> {
  if (isLocked(e)) {
    await app.lock()
    return
  }
  app.error = errorMessage(e)
  if (revert) await revert()
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
  /**
   * Set when a save was refused because the entry changed elsewhere.
   *
   * The editor keeps showing what the author typed -- the whole point is not
   * to lose it -- and autosave stops until they choose. `keepMine` writes
   * over the other version; `takeTheirs` reloads and discards this window's
   * copy.
   */
  conflict = $state(false)
  theme = $state<'light' | 'dark' | 'system'>('system')
  section = $state<Section>('journal')

  /**
   * Things to drop when the vault locks.
   *
   * The todo and library stores register one of these rather than being
   * imported here.
   * A lock must clear *every* decrypted thing the interface is holding, and
   * the alternative -- this file reaching into each app's store -- is a
   * circular import and a list that is quietly wrong the first time someone
   * adds an app and forgets to extend it.
   */
  #resetHooks: (() => void)[] = []

  #saveTimer: ReturnType<typeof setTimeout> | null = null
  /** The tail of the chain of saves, so only one is ever in the air. */
  #queue: Promise<void> = Promise.resolve()
  /**
   * Current backoff for a save that is failing, or `null` when the last one
   * landed. Doubles per failure up to `MAX_SAVE_RETRY_MS`.
   */
  #retryDelay: number | null = null
  /**
   * The `updatedAt` of the open entry as it was last agreed with the vault:
   * what was loaded, or what the last successful save wrote.
   *
   * This is the version token for the optimistic-concurrency check, and it
   * cannot be read off `entry` because `flush` stamps a fresh `updatedAt`
   * before every save. `null` means the open entry is one this window created
   * and has never successfully written.
   */
  #baseVersion: string | null = null
  #searchTimer: ReturnType<typeof setTimeout> | null = null
  #lockTimer: ReturnType<typeof setInterval> | null = null
  #lastTouch = 0
  #locking = false
  #lastListRefresh = 0
  #listTimer: ReturnType<typeof setTimeout> | null = null
  /**
   * Set once a failing save has been told to the user, cleared when one
   * lands.
   *
   * `Notices` shows a failed save in the window, which is right and stays,
   * but it can only be seen by somebody looking at the window. This is the
   * case the notification service was added for: the retries have been going
   * for over a minute, the author has walked away, and what they typed is
   * still only in a webview. That has to follow them out of the app.
   */
  #saveAlarmed = false

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
    if (SECTIONS.includes(remembered as Section)) {
      this.section = remembered as Section
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

  /**
   * Does this vault's backend carry the library?
   *
   * One capability, not a pair as the calendar needs. Nothing in the library
   * reads a task or an event -- a shelf is its own three records -- so it is
   * offered on any backend that carries the domain.
   */
  get supportsLibrary(): boolean {
    return this.status?.capabilities?.library === true
  }

  /**
   * Does this vault hold readings?
   *
   * Unlike the calendar's, this is one capability rather than a pair: the
   * tracker *definitions* ride inside a journal and every backend can store
   * those, so the only question is whether their readings have somewhere to
   * go. It gates the chips under an entry, the tracking section of a
   * journal's settings, and the marks on the calendar.
   */
  get supportsTrackers(): boolean {
    return this.status?.capabilities?.trackers === true
  }

  /** Is this section available on the vault that is open? */
  canShow(section: Section): boolean {
    if (section === 'todo') return this.supportsTasks
    if (section === 'calendar') return this.supportsCalendar
    if (section === 'library') return this.supportsLibrary
    return true
  }

  setSection(section: Section) {
    if (!this.canShow(section)) return
    this.section = section
    localStorage.setItem('everyday.section', section)
  }

  /**
   * Switch to a section and wait until it is actually on screen.
   *
   * What a quick action from outside the window needs and a click on the
   * sidebar does not: an app's view is what starts its store and holds its
   * capture field, and both are gone until the next render. Returns false
   * if the open vault cannot show that app at all, so a caller can stop
   * rather than act on the wrong screen.
   */
  async goTo(section: Section): Promise<boolean> {
    if (this.screen !== 'main' || !this.canShow(section)) return false
    this.setSection(section)
    await tick()
    return true
  }

  /**
   * Move to the next app the open vault can offer. What Ctrl/Cmd J does.
   *
   * A cycle rather than a toggle, now that there are four, and it skips
   * what the backend does not carry -- so on a vault whose backend holds
   * journals only, the shortcut is a no-op rather than a way to reach a
   * screen that cannot work.
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

  async createVault(opts: {
    path: string
    name: string
    backend: string
    settings?: Record<string, string>
    password: string | null
  }) {
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
    // A retry scheduled by a failed save has nothing left to write once the
    // entry below is dropped, and would fire against a locked vault.
    if (this.#saveTimer) clearTimeout(this.#saveTimer)
    this.#saveTimer = null
    this.#retryDelay = null
    this.#baseVersion = null
    this.conflict = false
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
      await handle(e)
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
    try {
      await api.saveJournal(journal)
    } catch (e) {
      return void (await handle(e))
    }
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
      await handle(e)
      return false
    }
  }

  /**
   * Draw a tracker's readings on the calendar, or stop drawing them.
   *
   * A tracker is a field on its journal rather than a record of its own --
   * there is deliberately no `saveTracker` -- so a switch on one is a write
   * of the whole journal. `JournalSettings` does exactly this; the method is
   * here because two other places offer the same switch without owning that
   * dialog: the chip under the day, and a reading already on the grid.
   */
  async setTrackerOnCalendar(journalId: JournalId, trackerId: TrackerId, onCalendar: boolean) {
    const journal = this.journals.find((j) => j.id === journalId)
    if (!journal) return
    const next = $state.snapshot(journal)
    const tracker = next.trackers.find((t) => t.id === trackerId)
    if (!tracker || tracker.onCalendar === onCalendar) return
    tracker.onCalendar = onCalendar
    next.updatedAt = new Date().toISOString()
    await this.saveJournal(next)
  }

  async deleteJournal(id: JournalId) {
    try {
      await api.deleteJournal(id)
    } catch (e) {
      return void (await handle(e))
    }
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
      if (this.#listTimer) {
        clearTimeout(this.#listTimer)
        this.#listTimer = null
      }
      this.#lastListRefresh = Date.now()
      await this.refreshRows()
      // Keep a selection if it is still in view; otherwise open the newest.
      if (!this.entries.some((e) => e.id === this.selectedEntry)) {
        const first = this.entries[0]
        if (first) await this.openEntry(first.id)
        else {
          this.selectedEntry = null
          this.entry = null
        }
      }
    } catch (e) {
      await handle(e)
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
      this.#baseVersion = this.entry.updatedAt
      // A conflict belongs to the document that had one, not to the window.
      this.conflict = false
    } catch (e) {
      await handle(e)
    }
  }

  /**
   * Start today's entry, or open it if it has already been started.
   *
   * **One entry per journal per day.** A journal is a record of days, and a
   * day is one thing: two entries dated the same Tuesday in the same journal
   * are not two records, they are one record that has been split by an
   * accidental second press of Ctrl+N -- and once split, half of what you
   * wrote is behind a row you have to remember exists. The list groups by
   * day, so the two sit under one heading looking like a duplicate, and
   * nothing in the interface says which is the one you were writing in.
   *
   * Enforced by opening rather than by refusing, which is the part that
   * makes it a rule and not an error message: "New entry" always answers
   * with today's page, whether or not there was one a moment ago. That is
   * also exactly what the tray action, the shortcut and the sidebar's "New
   * entry here" should do, and all three go through here.
   *
   * The check is a query rather than a look at `entries`, because the list
   * on screen is filtered: today's entry can easily not be in it -- the
   * starred view, another journal selected, a search in progress -- and
   * concluding "there is no entry today" from a list that was never asked
   * about today is how the rule would fail in exactly the case somebody
   * notices.
   *
   * More than one journal is untouched by this. Keeping a work journal and
   * a personal one and writing in both on the same day is the arrangement
   * the app is for.
   */
  async newEntry() {
    const journalId = this.selectedJournal ?? this.journals[0]?.id
    if (!journalId) return
    await this.flush()

    const today = todayIso()
    let existing: EntrySummary | undefined
    try {
      existing = (await api.entries({ journalId, from: today, to: today, limit: 1 }))[0]
    } catch (e) {
      return void (await handle(e))
    }
    if (existing) {
      // The starred filter is dropped for the reason it is below: an entry
      // opened in the editor while the list beside it cannot show that row
      // is an editor nobody can tell is the right one. The *journal*
      // selection is deliberately left alone -- narrowing "All entries" to
      // one journal as a side effect of asking for today's page would be
      // answering a question nobody asked.
      if (this.showStarredOnly) {
        this.showStarredOnly = false
        await this.refreshEntries()
      }
      await this.openEntry(existing.id)
      notify.info("You have already started today's entry", {
        body: 'A journal keeps one entry a day, so this is that one — carry on writing in it.',
        key: 'entry-today',
      })
      return
    }

    let entry: Entry
    try {
      entry = await api.newEntry(journalId)
      await api.saveEntry(entry, null)
    } catch (e) {
      return void (await handle(e))
    }
    this.#baseVersion = entry.updatedAt
    this.conflict = false
    this.entry = entry
    this.selectedEntry = entry.id
    // A new entry is not starred, so the Starred list is not a list it can
    // appear in. Leaving the filter up put the row out of sight while the
    // refresh below quietly opened some *other* entry in the editor -- so
    // "New entry" answered with somebody else's writing. Show the list the
    // new entry is actually in.
    this.showStarredOnly = false
    await this.refreshEntries()
  }

  /**
   * Queue a save. Repeated edits collapse into one write.
   *
   * The journal keeps its own timer rather than an `Autosave` like the other
   * two stores, and the difference is real: there is one open document, and
   * the editor -- not this store -- holds the truth about it while it is
   * open. `flush` therefore pulls the body and writes unconditionally
   * instead of consulting a set of dirty ids that could never see a
   * keystroke. Only the delay is shared, so all three agree on it.
   */
  scheduleSave() {
    if (this.#saveTimer) clearTimeout(this.#saveTimer)
    this.#saveTimer = setTimeout(() => void this.flush(), AUTOSAVE_MS)
  }

  /**
   * Write any pending edit immediately. Safe to call when nothing is dirty.
   *
   * Serialised through `#queue`, like `Autosave`'s and for a sharper reason:
   * two saves of the same entry in the air at once would both be sent with
   * the *same* `#baseVersion`, because the first has not returned to update
   * it. The second is then refused as a conflict against a version this
   * window wrote itself a moment earlier -- a "changed elsewhere" dialog
   * over an edit nobody else touched. Ctrl+S landing on top of an autosave,
   * or the close handshake landing on top of either, is all it took.
   */
  flush(): Promise<void> {
    if (this.#saveTimer) {
      clearTimeout(this.#saveTimer)
      this.#saveTimer = null
    }
    const next = this.#queue.then(() => this.#write())
    this.#queue = next.catch(() => {})
    return next
  }

  async #write() {
    const entry = this.entry
    if (!entry) return
    // Nothing is written while a conflict is unresolved. Retrying would
    // only be refused again, and the author has a choice in front of them.
    if (this.conflict) return
    this.syncBody()
    this.saving = true
    try {
      entry.updatedAt = new Date().toISOString()
      await api.saveEntry($state.snapshot(entry), this.#baseVersion)
      this.#baseVersion = entry.updatedAt
      this.lastSaved = entry.updatedAt
      // Pick up the new title and excerpt in the list, but not right now.
      //
      // This is on the autosave path: while someone is writing it fired every
      // 700ms, and each one replaced the whole `entries` array, re-rendering
      // every row in the list beside the text they were typing into. The list
      // is a secondary view of an entry the author is looking straight at, so
      // it is allowed to lag -- but the refresh is queued, never dropped, so
      // it always converges once the typing stops.
      this.#retryDelay = null
      if (this.#saveAlarmed) {
        this.#saveAlarmed = false
        // Worth saying out loud, and worth it reaching as far as the alarm
        // did. Being told your writing is not on disk and then never being
        // told that it is leaves you checking.
        notify.success('Your journal is saved again', {
          body: 'Everything written while saving was failing has been written.',
          reach: 'user',
          key: 'save-failing',
        })
      }
    } catch (e) {
      // A conflict is not a failure to retry. Somebody else's version is in
      // the vault and ours is on screen; both are real, and which one wins is
      // not a decision this code gets to make on a timer.
      if (isConflict(e)) {
        this.conflict = true
        this.saving = false
        return
      }
      await handle(e)
      // The entry is still only in the editor. Unlike the other two stores
      // there is no dirty set to put anything back into -- `flush` always
      // writes whatever the editor currently holds -- so retrying is a
      // matter of putting the timer back, with a widening gap so a vault on
      // a full disk is not written to forty times a minute.
      //
      // Not after a lock: the entry has been dropped from memory and the
      // vault is not open, so there is nothing to write and nowhere to
      // write it.
      if (!isLocked(e)) {
        this.#retryDelay = Math.min(
          this.#retryDelay === null ? AUTOSAVE_MS : this.#retryDelay * 2,
          MAX_SAVE_RETRY_MS,
        )
        this.#saveTimer = setTimeout(() => void this.flush(), this.#retryDelay)
        // Not on the first failure. A save refused once and accepted 700ms
        // later is a disk that was busy, and interrupting somebody mid-word
        // to tell them about it is how a notification service earns the
        // reputation that gets it muted. The backoff reaching its ceiling
        // means the retries have been failing for over a minute, which is no
        // longer a hiccup.
        if (this.#retryDelay === MAX_SAVE_RETRY_MS && !this.#saveAlarmed) {
          this.#saveAlarmed = true
          notify.error('Your journal is not being saved', {
            body: 'Every attempt for the last minute has failed. What you have written is still in the editor and has not been lost.',
            reach: 'user',
            key: 'save-failing',
          })
        }
      }
    } finally {
      this.saving = false
      // Not after a save that ended in a lock: the refresh would fire four
      // seconds later against a vault that is no longer open.
      if (this.screen === 'main') this.queueListRefresh()
    }
  }

  /** True while a failed save is still being retried. */
  get saveFailing(): boolean {
    return this.#retryDelay !== null
  }

  /**
   * Resolve a conflict by keeping what is on screen.
   *
   * An unconditional write, which is the one place the interface asks for
   * one. The other version is overwritten because the author looked at the
   * choice and said so.
   *
   * Queued behind any save still in the air, like `flush`: it is the write
   * whose version token every later save is measured against, so it must not
   * be the one that lands first.
   */
  keepMine(): Promise<void> {
    const next = this.#queue.then(() => this.#forceWrite())
    this.#queue = next.catch(() => {})
    return next
  }

  async #forceWrite() {
    const entry = this.entry
    if (!entry || !this.conflict) return
    this.syncBody()
    this.saving = true
    try {
      entry.updatedAt = new Date().toISOString()
      await api.saveEntryForce($state.snapshot(entry))
      this.#baseVersion = entry.updatedAt
      this.lastSaved = entry.updatedAt
      this.conflict = false
      this.#retryDelay = null
    } catch (e) {
      await handle(e)
    } finally {
      this.saving = false
      if (this.screen === 'main') this.queueListRefresh()
    }
  }

  /**
   * Resolve a conflict by discarding this window's copy and reloading.
   *
   * Destructive, so it is the second of the two actions and never the
   * default. `openEntry` re-reads and resets the version token.
   */
  async takeTheirs() {
    const id = this.entry?.id
    if (!id || !this.conflict) return
    this.conflict = false
    this.entry = null
    await this.openEntry(id)
    await this.refreshEntries()
  }

  async deleteEntry(id: EntryId) {
    if (this.#saveTimer) {
      clearTimeout(this.#saveTimer)
      this.#saveTimer = null
    }
    if (this.selectedEntry === id) {
      this.entry = null
      this.selectedEntry = null
    }
    try {
      await api.deleteEntry(id)
    } catch (e) {
      return void (await handle(e))
    }
    await this.refreshEntries()
  }

  /**
   * Change one field of an entry that may or may not be the open one, and
   * write it.
   *
   * Every row action in the list has this same two-case shape: the open
   * entry, whose agreed version this window is already tracking, or any
   * other row, which has to be read before it can be written -- the list
   * holds summaries, and writing one back would drop the body.
   */
  async #editEntry(id: EntryId, change: (entry: Entry) => void) {
    try {
      // The same object, when it is the open one: mutating it is what puts
      // the star on the entry behind the list without a second read.
      const open = this.entry && this.entry.id === id ? this.entry : null
      const full = open ?? (await api.entry(id))
      // For the open entry this window already tracks the agreed version;
      // for any other row, what we just read is it.
      const base = open ? this.#baseVersion : full.updatedAt
      change(full)
      full.updatedAt = new Date().toISOString()
      await api.saveEntry($state.snapshot(full), base)
      // Re-checked after the await, and by identity: starring one entry and
      // clicking another while the write is in flight would otherwise stamp
      // the first one's version token onto the second, and the next autosave
      // of *that* entry would be refused as a conflict it was never in.
      if (open && this.entry === open) this.#baseVersion = full.updatedAt
    } catch (e) {
      return void (await handle(e))
    }
    await this.refreshEntries()
  }

  async toggleStar(id: EntryId) {
    await this.#editEntry(id, (entry) => (entry.starred = !entry.starred))
  }

  /** Float an entry to the top of the list, or let it fall back into date order. */
  async togglePin(id: EntryId) {
    await this.#editEntry(id, (entry) => (entry.pinned = !entry.pinned))
  }

  /** File an entry under a different journal. */
  async moveEntry(id: EntryId, journalId: JournalId) {
    await this.#editEntry(id, (entry) => (entry.journalId = journalId))
  }

  /**
   * The days in a month that have an entry in them, for the mini calendar.
   *
   * A query rather than a read of `entries`, and not cached here: the list
   * holds at most 500 rows in whatever order the filter chose, so a month
   * three years back is very often simply not in it -- and a calendar that
   * is silently wrong about which days you wrote on is worse than no
   * calendar. Cheap: dates are a clear index column, so nothing is
   * decrypted to answer it.
   *
   * Returns a map from date to the entry to open for it. Where a journal
   * holds two entries on one day -- which `newEntry` no longer creates, but
   * which an older vault may well contain -- the first in the list wins,
   * and that is the one a click on the day opens.
   */
  async entriesInRange(from: string, to: string): Promise<Map<string, EntryId>> {
    const out = new Map<string, EntryId>()
    try {
      const rows = await api.entries({
        journalId: this.selectedJournal,
        from,
        to,
        sort: 'dateAsc',
        limit: 500,
      })
      for (const row of rows) if (!out.has(row.localDate)) out.set(row.localDate, row.id)
    } catch (e) {
      // Not worth a message over the window: the calendar is an aid beside
      // the list, and the list is the thing that has to be right.
      if (isLocked(e)) await app.lock()
    }
    return out
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
        await handle(e)
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
    const j = this.entry ? this.journals.find((x) => x.id === this.entry!.journalId) : this.journal
    return j?.color ?? 'var(--accent)'
  }
}

export const app = new AppState()

// ── Quick actions ──────────────────────────────────────────────────────
//
// The journal's, and the vault's own. Registered at module scope beside the
// store whose state they read, which is the convention the other two follow
// -- see `lib/tray.svelte.ts` for what the registration means and
// `lib/todo.svelte.ts` for the shortest example of adding one.
//
// Every group starts by checking the screen. A tray menu is drawn from state
// that the window is not showing, so "are we past the lock screen" is not
// implied by anything else here the way it is inside a component.

tray.register('journal', TRAY_ORDER.journal, () => {
  if (app.screen !== 'main') return []
  return [
    {
      id: 'journal:new-entry',
      label: 'New journal entry',
      // A vault with no journal in it has nowhere to put an entry. Greyed
      // rather than hidden: the action is the app's, not the moment's.
      enabled: app.journals.length > 0,
      run: async () => {
        if (await app.goTo('journal')) await app.newEntry()
      },
    },
  ]
})

tray.register('vault', TRAY_ORDER.vault, () => {
  // Nothing to lock on a vault with no password on it.
  if (app.screen !== 'main' || !app.status?.encrypted) return []
  return [
    {
      id: 'vault:lock',
      label: 'Lock now',
      // The one action here that is *about* not coming back to the window.
      raise: false,
      run: () => app.lock(),
    },
  ]
})
