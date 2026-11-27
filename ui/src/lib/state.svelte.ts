// Application state.
//
// One store, using Svelte 5 runes. It owns the vault's lock, the open vault's
// settings and the other state every app shares, and the save/lock logic
// lives here in one auditable spot. Each app's own state sits in a store of
// its own beside it (`journal.svelte.ts`, `mail.svelte.ts`, ...), and a
// component may call `api` directly for what only it needs.

import { tick } from 'svelte'
import { api, callCommand, hasShell, isMock, isWeb, mobileShell, newRequestId } from './api'
import { details, tellShellTheme } from './phone.svelte'
import { signOut } from './web'
import { APP_ORDER, APPS, type Section } from './apps'
import { pinned } from './appwindow'
import { AUTOSAVE_MS } from './autosave'
import { errorMessage, handle, isConflict, isLocked, quietly, setPolicy } from './errors'
import { notify } from './notify.svelte'
import { seen, type Showing } from './onscreen'
import { panels } from './panels.svelte'
import { pref } from './prefs'
import {
  parseThemeChoice,
  resolveTheme,
  showTheme,
  themeInfo,
  watchSystemScheme,
  type Frame,
  type ThemeChoice,
  type ThemeId,
} from './themes'
import { windowControls } from './window.svelte'
import { views, whenIdle } from './views.svelte'
import { debounce } from './store/debounce'
import { DocBinding } from './store/doc-binding'
import { mergeTags } from './tags'
import { todayIso } from './time'
import type {
  Bootstrap,
  Connected,
  Connection,
  Entry,
  EntryId,
  EntrySummary,
  Journal,
  JournalId,
  Purpose,
  EntryHit,
  StoreStats,
  VaultStatus,
} from './types'

// `errors.ts` holds the actual definitions now -- see it for why. Re-exported
// so every store that already writes `import { handle, isLocked } from
// './state.svelte'` keeps working unchanged.
export { errorMessage, handle, isConflict, isLocked, quietly } from './errors'

/**
 * How often the machine holding the vault is asked whether it has given up
 * its key for idleness. Local windows only; a remote client is told instead.
 */
const AUTOLOCK_POLL_MS = 5_000
/** How often this window checks its own keyboard against the screen timeout. */
const IDLE_POLL_MS = 5_000
/** Floor between "the user is still here" pings to the backend. */
const TOUCH_MS = 15_000

/**
 * When somebody last used any window of this app on this machine, kept where
 * every window can read it.
 *
 * The screen timer is each window's own, and a window of one app
 * (`appwindow.ts`) is somebody at this keyboard all the same: an hour spent in
 * the todo window must not send the main window beside it to its lock screen.
 * Written at the pace `touch` pings the backend -- so a window's view of the
 * others is at most `TOUCH_MS` old, against a timeout counted in minutes --
 * and only by this machine's windows, so a phone looking at the same vault
 * defers nothing here.
 */
const ACTIVE_AT = 'jatayu.activeAt'

/** The computer a phone was last looking at, by its connection's id. */
const LAST_REMOTE = 'jatayu.lastRemote'

function activeElsewhere(): number {
  try {
    return Number(localStorage.getItem(ACTIVE_AT)) || 0
  } catch {
    return 0
  }
}
/** Floor between list refreshes asked for by a change made elsewhere. */
const LIST_REFRESH_MS = 4_000
/**
 * How long the journal's list waits after an autosave before it catches up:
 * a pause in the typing, rather than mid-sentence.
 */
const LIST_IDLE_MS = 1_500
/** ...but no longer than this after the first save it is catching up with. */
const LIST_MAX_WAIT_MS = 20_000

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
 * rather than a handful bolted together.
 *
 * `Section` and its order now live in `apps.ts`, alongside the label, icon,
 * accent, capability check, create action and chat context every app in it
 * carries -- see that file's own header for why it, rather than this one,
 * owns them. Re-exported here because most of the application already
 * imports `Section` (and, for `nextSection` below, the order) from
 * `state.svelte`.
 */
export { type Section, APP_ORDER as SECTIONS } from './apps'

/** The theme the person chose. See `themes.ts` for the list and the contract. */
export type Theme = ThemeChoice

const themePref = pref<Theme>('jatayu.theme', parseThemeChoice, 'system')

/** Named because `start` also runs a `?section=` override through it. */
function parseSection(raw: string | null): Section | null {
  return APP_ORDER.includes(raw as Section) ? (raw as Section) : null
}
const sectionPref = pref<Section | null>('jatayu.section', parseSection, null)

class AppState {
  screen = $state<Screen>('loading')
  boot = $state<Bootstrap | null>(null)
  status = $state<VaultStatus | null>(null)
  error = $state<string | null>(null)

  /**
   * The vault on another computer that this window is looking at.
   *
   * `null` means the vault is in this process, which is the ordinary case.
   * When it is set, three things follow: the settings that are about *this*
   * machine's storage are hidden, because they belong to the other one; a
   * banner says which computer is being read from; and there is no offline
   * mode, so losing the connection is a condition rather than an error to
   * dismiss.
   */
  remote = $state<Connection | null>(null)
  /**
   * Set while this window is unlocking the vault itself.
   *
   * The backend raises a lock-state event on any unlock, including this
   * window's own, and the live router would otherwise reload everything from
   * scratch alongside the load the unlock is already doing. See
   * `live.svelte.ts`.
   */
  unlocking = $state(false)
  /**
   * True when the lock screen is this window's own, over a vault that is
   * still open.
   *
   * It decides which question the lock screen asks on the way back: prove who
   * you are, or open the vault. See `unlock`.
   */
  screenOnly = $state(false)
  /**
   * Whether this machine holds the key, so the vault opens without a password
   * when the process starts. Off unless somebody turned it on.
   *
   * Also what `startLockTimers` reads to suspend both idle clocks: once
   * signing in to the computer counts as unlocking, idling the screen or
   * the key for a few quiet minutes has nothing left to protect.
   */
  opensItself = $state(false)
  /** Servers this copy has paired with, for the picker. */
  remotes = $state<Connection[]>([])
  /** Disconnected on purpose: a phone does not go straight back. */
  private stayAway = false

  journals = $state<Journal[]>([])

  /**
   * The assistant's own journal, if it keeps one: notes it leaves itself,
   * filed under the day they are about. What reads the person's own writing
   * -- "All entries", the week's word count, the calendar's "you wrote" dots
   * -- leaves it out by this id. See `Journal.assistant`.
   */
  get assistantJournalId(): JournalId | null {
    return this.journals.find((j) => j.assistant)?.id ?? null
  }

  /** Their own journals, without the assistant's. */
  get ownJournals(): Journal[] {
    return this.journals.filter((j) => !j.assistant)
  }
  /** Replaced whole by every read, never changed in place. */
  entries = $state.raw<EntrySummary[]>([])
  /** `null` means "all journals". */
  selectedJournal = $state<JournalId | null>(null)
  selectedEntry = $state<EntryId | null>(null)
  entry = $state<Entry | null>(null)

  query = $state('')
  results = $state<EntryHit[]>([])
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
  theme = $state<Theme>('system')
  /** The theme on screen: `theme`, or Paper or Ink when that is to follow the
   *  desktop. Kept apart because only this one changes when the desktop does. */
  shown = $state<ThemeId>('light')
  section = $state<Section>('journal')

  /**
   * Things to drop when the vault locks.
   *
   * Every app's store registers one of these from its constructor rather than
   * being imported here. A lock must clear *every* decrypted thing the
   * interface is holding, and the alternative -- this file reaching into each
   * store -- is a circular import and a list that is quietly wrong the first
   * time someone adds an app and forgets to extend it.
   */
  #resetHooks: (() => void)[] = []
  /**
   * Everything with a debounced write, to be emptied before the interface
   * gives up what it is holding.
   *
   * The same arrangement as `#resetHooks` above, and added for the same reason
   * the list of resets is a registry rather than a list of imports: the four
   * places that had to write everything out kept the roster by hand, and by
   * the time the notes app arrived only one of the four had heard of it. A
   * store that registers a flush here cannot be left out of a lock, a Ctrl+S
   * or a window close, because none of them names it.
   */
  #flushHooks: (() => Promise<unknown>)[] = []

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
  /**
   * The open entry, when it was started from a template and the vault does
   * not have it yet.
   *
   * A day started from a template holds the template and nothing of the
   * person's. Writing that would file a day nobody wrote -- a row in the
   * list, a page insights and dreams read as theirs -- so it stays in the
   * editor until something is typed into it, and one left untouched is
   * simply dropped. A blank page is still saved the moment it is made, as
   * it always was.
   *
   * Two markers, because "nothing typed yet" ends on the first keystroke and
   * "not in the vault" only when that keystroke's save lands: `#untouched`
   * says whether a write should happen at all, `#unstored` that the first
   * one is a create -- and that until it lands, the list cannot show the
   * page and must not be read as having lost it.
   */
  #untouched = $state<EntryId | null>(null)
  #unstored = $state<EntryId | null>(null)
  /**
   * The `updatedAt` and request id of the write currently being attempted.
   *
   * Held across retries so a retry *is* the same write rather than a second
   * one; see `#write`. `null` when there is nothing in flight, and set back to
   * `null` whenever the text changes.
   */
  #writeStamp: { updatedAt: string; requestId: string } | null = null
  /** The journal search box's debounce. See `setQuery`. */
  #search = debounce((q: string) => void this.#runSearch(q), 140)
  #lockTimer: ReturnType<typeof setInterval> | null = null
  #screenTimer: ReturnType<typeof setInterval> | null = null
  /** When this window last saw a person. Drives the screen timeout. */
  #idleSince = Date.now()
  #lastTouch = 0
  #locking = false
  #lastListRefresh = 0
  #listTimer: ReturnType<typeof setTimeout> | null = null
  /** When the first autosave the queued list refresh is catching up with landed. */
  #listSince: number | null = null
  /** Each list read is numbered, so one answered late cannot replace a newer one. */
  #rowsAsked = 0
  /**
   * Whether the list has been read since the vault was opened. It is read
   * when the journal is first shown, not at the unlock: see `ensureEntries`.
   */
  #entriesLoaded = $state(false)
  #entriesLoading: Promise<void> | null = null
  /**
   * Edits to the open entry, counted, and how many of them the last write
   * that landed carried. Equal means the vault has what is on screen, and
   * moving to another entry, putting the editor away or locking writes
   * nothing.
   */
  #edits = 0
  #savedEdits = 0
  /**
   * The document the last `syncBody` pulled from the editor, kept as it came
   * so the save sends it as it is rather than copying it back out of the
   * reactive `entry`. Used once, by the next write of that same entry.
   */
  #pulled: { of: Entry; body: Entry['body'] } | null = null
  /** Each `openEntry` is numbered, so a slower earlier one cannot land last. */
  #opening = 0
  /** Each choice of app is numbered too, so a `goTo` still waiting for its
   *  view knows if another app was chosen, or the vault locked, meanwhile. */
  #choosing = 0
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
   * body is pulled once, at save time, instead. See `DocBinding`; the notes
   * app has its own copy of this same slot, for its own editor.
   */
  #body = new DocBinding<Entry['body']>()

  // ── lifecycle ────────────────────────────────────────────────────────

  async start() {
    // A mock build accepts `?theme=` so the interface can be reviewed in a
    // fixed theme without clicking through to Settings first.
    const forced = isMock ? new URLSearchParams(location.search).get('theme') : null
    this.theme = parseThemeChoice(forced ?? localStorage.getItem('jatayu.theme'))
    this.applyTheme()
    // `?section=` alongside `?theme=`, and for the same reason: so the
    // interface can be opened straight to the app under review.
    const asked = isMock ? new URLSearchParams(location.search).get('section') : null
    // A window of one app shows that app, whatever the main window was last
    // left on. See `appwindow.ts`.
    const section = pinned ?? parseSection(asked ?? localStorage.getItem('jatayu.section'))
    if (section) this.section = section
    try {
      const boot = await api.bootstrap()
      this.boot = boot
      this.status = boot.status
      this.remote = boot.remote
      this.remotes = boot.remotes
      this.opensItself = boot.opensItself
      this.error = null
      // A phone is always looking at a computer's vault (`mobile.rs`), and
      // goes back to the one it last looked at, as a desktop window opens
      // the vault it last had open -- unless it was just told to stop, when
      // it shows the computers it knows instead. Not reached: the error
      // screen, which offers to try again or to pair with another.
      const lastId = localStorage.getItem(LAST_REMOTE)
      const last = boot.remotes.find((r) => r.id === lastId) ?? boot.remotes.at(-1)
      if (mobileShell && !boot.remote && last && !this.stayAway) {
        await this.reconnectRemote(last.id)
        return
      }
      if (!boot.vaultExists && !boot.remote) this.screen = 'setup'
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

  /**
   * Connect to a vault on another computer, from a pairing link.
   *
   * The link is what somebody copied from that machine's settings, or scanned.
   * It carries the address, a one-time code, and the certificate to pin -- so
   * the check that this is the right computer happens before anything secret
   * is sent. See `jatayu_server::client::RemoteClient::pair`.
   */
  async connectRemote(link: string) {
    this.screen = 'loading'
    try {
      await this.afterConnect(await api.connectRemote(link))
    } catch (e) {
      this.error = errorMessage(e)
      this.screen = 'error'
    }
  }

  /** Reconnect to one this copy has paired with before. */
  async reconnectRemote(id: string) {
    this.screen = 'loading'
    try {
      await this.afterConnect(await api.reconnectRemote(id))
    } catch (e) {
      this.error = errorMessage(e)
      this.screen = 'error'
    }
  }

  private async afterConnect({ status, connection }: Connected) {
    this.status = status
    // The connection the backend actually attached, by identity. Matching it
    // out of the list by the vault's *name* was wrong for anybody with two
    // machines each holding a vault called "Journal", which is the default.
    this.remote = connection
    this.remotes = await api.remotes()
    this.stayAway = false
    localStorage.setItem(LAST_REMOTE, connection.id)
    this.error = null
    if (status.unlocked) await this.enterMain()
    else this.screen = 'locked'
  }

  /** Stop looking at another computer's vault. The pairing survives. */
  async disconnectRemote() {
    await api.disconnectRemote()
    this.remote = null
    this.stayAway = true
    await this.start()
  }

  /** Forget a pairing: the connection and the token in the keychain. */
  async forgetRemote(id: string) {
    await api.forgetRemote(id)
    this.remotes = await api.remotes()
    if (this.remote?.id === id) {
      this.remote = null
      this.stayAway = true
      await this.start()
    }
  }

  /**
   * Re-read the vault's status word without reloading anything else.
   *
   * What a settings change on another machine costs: capabilities, the
   * auto-lock, whether the vault is still writable.
   */
  async refreshStatus() {
    try {
      this.status = await api.status()
    } catch {
      // A status that could not be read changes nothing on screen. Whatever
      // made it fail will be reported by the next call that matters.
    }
  }

  /**
   * How much room the vault's pictures and other attachments take.
   *
   * Not in `status`, which is read at every unlock and every settings change
   * and so no longer counts it: the count walks every file in the media
   * folder. Asked for by the two pages that show it, when they are shown.
   * `null` when it cannot be told.
   */
  async mediaBytes(): Promise<number | null> {
    try {
      return (await callCommand<StoreStats>('vault_stats')).blobBytes
    } catch {
      // A backend too old to answer, or the review build's stand-in: what
      // the status still says, if it says anything.
      return this.status?.stats?.blobBytes || null
    }
  }

  /**
   * The vault locked, and not because anybody here asked.
   *
   * Under server mode the machine holding the vault decides: its idle timer,
   * or somebody locking it there. Every window looking at it has to leave, and
   * the local `lock()` path is wrong for this -- it would try to lock a vault
   * that is already locked and, worse, would do it over the wire.
   */
  async lockedElsewhere() {
    this.#teardown()
    this.screenOnly = false
    this.screen = 'locked'
  }

  /** Register state to be dropped when the vault locks. */
  onLock(reset: () => void) {
    this.#resetHooks.push(reset)
  }

  /**
   * Register a debounced writer to be emptied before anything is given up.
   *
   * Called from a store's constructor, beside its `onLock`. See `flushAll`.
   */
  onFlush(flush: () => Promise<unknown>) {
    this.#flushHooks.push(flush)
  }

  /**
   * Write out everything anybody has typed, everywhere.
   *
   * What a lock, an explicit save and the close handshake all need. `flush`
   * above is the *journal entry* alone, which is the right thing when the
   * journal is switching entries and the wrong thing at every moment the
   * interface is about to stop holding what it has.
   *
   * `allSettled`, because one store failing to write must not stop the others
   * from trying: the alternative is a save that gave up on four apps because
   * the first one it reached had a conflict on screen.
   */
  async flushAll(): Promise<void> {
    await Promise.allSettled([this.flush(), ...this.#flushHooks.map((f) => f())])
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
   * Does this vault hold trackers and their readings?
   *
   * One capability rather than a pair: both halves of the domain — the
   * definitions and the numbers — are answered by the same store. It gates
   * the chips under an entry, the tracking section of a journal's settings,
   * the marks on the calendar, and the Overview's habits.
   */
  get supportsTrackers(): boolean {
    return this.status?.capabilities?.trackers === true
  }

  /**
   * Can this vault run the Overview?
   *
   * A pair, as the calendar needs. The app is a view over what the other
   * apps store — where the week went, which goals have gone quiet, which
   * habits are holding — and the two halves of that come from the purpose
   * domain and the tracking one. A backend with areas and no readings could
   * draw half of it, and half of a balance report is a report that is
   * wrong rather than short.
   */
  get supportsOverview(): boolean {
    return this.status?.capabilities?.purpose === true && this.supportsTrackers
  }

  /**
   * Does this vault hold notes?
   *
   * Independent of everything else: nothing in the notes app reads a task or
   * an entry, so it is offered on any backend that carries the domain.
   */
  get supportsNotes(): boolean {
    return this.status?.capabilities?.notes === true
  }

  /**
   * Does this vault hold the assistant's standing work?
   *
   * Separate from `supportsAssistant`: a backend could hold conversations and
   * not routines, and the rail would still work. The Assistant app is offered
   * on the first; its routines pane is drawn on the second.
   */
  get supportsRoutines(): boolean {
    return this.status?.capabilities?.routines === true
  }

  /**
   * Can the assistant here take work on in the background?
   *
   * Its own flag rather than `supportsRoutines`, though both are the
   * assistant's standing work: the Markdown backend stores no jobs, and a
   * vault that has routines may still not have this table. Without it
   * `start_research` is not offered, so there is nothing to draw. See
   * `docs/plans/jobs.md`.
   */
  get supportsJobs(): boolean {
    return this.status?.capabilities?.jobs === true
  }

  /** Backend carries proposals -- see `docs/plans/dreaming.md`. */
  get supportsProposals(): boolean {
    return this.status?.capabilities?.proposals === true
  }

  /**
   * Does this vault hold the assistant's own records at all?
   *
   * The same flag the rail reads. The Assistant app needs nothing more: even
   * without routines it has a memory list and a thread history.
   */
  get supportsAssistant(): boolean {
    return this.status?.capabilities?.agent === true
  }

  /**
   * Can this vault run the Mail app?
   *
   * `capabilities.mail` is a backend implementing the mail domain, and per
   * its own doc a backend that carries it carries `accounts` too -- there is
   * nowhere to keep the credential a synced mailbox needs otherwise. So this
   * reads as "accounts exist or accounts are supported": once the backend
   * can hold a mailbox at all, the app is offered, empty inbox and all, so
   * the first thing anybody sees there can be "add an account".
   */
  get supportsMail(): boolean {
    return this.status?.capabilities?.mail === true
  }

  /**
   * Can this vault run the Feed?
   *
   * One capability. The app needs nothing from another domain to be useful:
   * a feed on a web page, or Hacker News, is read with no account anywhere.
   * Newsletters are the one thing that does -- they are mail -- and are
   * offered on `supportsMail` as well, in the Feed's own store, not here.
   */
  get supportsNews(): boolean {
    return this.status?.capabilities?.news === true
  }

  /**
   * Can this vault run Money?
   *
   * One capability, as the Feed needs. Nothing in it reads another domain:
   * an account kept by hand and filled from a bank's file works with no
   * Plaid keys and no account anywhere, so the app is offered on any backend
   * that can hold the records, and linking a bank is something it offers
   * from inside.
   */
  get supportsMoney(): boolean {
    return this.status?.capabilities?.money === true
  }

  /**
   * Can this vault keep screen time?
   *
   * One capability, as Money's. Whether *this computer* records any is a
   * separate question the shell answers (`screentime_status`): a vault can
   * show what a laptop recorded to a window that records nothing itself.
   */
  get supportsScreenTime(): boolean {
    return this.status?.capabilities?.screentime === true
  }

  /**
   * Can this vault keep files?
   *
   * One capability, as Money's: the catalog and the files folder, which every
   * app that keeps a file writes through (`docs/plans/files.md`). Whether the
   * folder is set up, and whether this window may pick files by path, are
   * questions `files_status` answers.
   */
  get supportsFiles(): boolean {
    return this.status?.capabilities?.files === true
  }

  /**
   * Can this vault run Health?
   *
   * One capability, as Money's and Screen time's. Nothing in it needs a
   * device: what a watch or an export brings is added from inside, and a
   * vault with none is a place to keep medications, results and meals by
   * hand.
   */
  get supportsHealth(): boolean {
    return this.status?.capabilities?.health === true
  }

  /**
   * Is this section available on the vault that is open?
   *
   * Reads `apps.ts`'s registry rather than a switch of its own: that used to
   * be a chain of `if`s with a `true` at the end, which answered "yes" for
   * anything it had not been told about, so an app added without a line here
   * was offered on a backend that cannot carry it and failed at the first
   * query. `APPS` does not compile until the new section says what it needs.
   */
  canShow(section: Section): boolean {
    return APPS[section].supported(this)
  }

  setSection(section: Section) {
    if (!this.canShow(section)) return
    this.#choosing++
    // A window of one app shows no other: the main window comes forward to
    // show it instead. Nor does it remember its own app as the last one
    // open -- that is the main window's to come back to.
    if (pinned) {
      if (section !== pinned) void api.showInMain({ section }).catch(() => {})
      return
    }
    this.section = section
    sectionPref.set(section)
    // Choosing an app is how Settings is left, the same as the close button
    // or Escape -- see `SettingsView.svelte`'s header comment. Every way of
    // picking a section funnels through here (the app bar, every `g`-chord,
    // the palette, "the next app", a widget's own "open this"), so this is
    // the one place that has to say it rather than each of those repeating
    // it. Harmless when Settings was already closed.
    panels.closeSettings()
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
    // In a window of one app, another app is the main window's to show --
    // and whatever the caller meant to do there next has no store here to
    // do it in, so it is told to stop.
    if (pinned && section !== pinned) {
      this.setSection(section)
      return false
    }
    // An app's view is loaded the first time it is shown (`views.svelte.ts`):
    // it has to have arrived for the tick below to put it on screen. Which
    // takes a moment, and whatever happened in it wins over this older ask:
    // another app chosen, or the vault locked.
    const asked = ++this.#choosing
    await views.ensure(section)
    if (asked !== this.#choosing || this.screen !== 'main') return false
    this.setSection(section)
    // A view that could not be loaded: the stage says so, and the caller
    // has nothing there to act on.
    if (!views.view(section)) return false
    await tick()
    return true
  }

  /**
   * Move to the next app the open vault can offer. What Ctrl/Cmd J does.
   *
   * A cycle rather than a toggle, now that there are more than two, and it
   * skips what the backend does not carry -- so on a vault whose backend
   * holds journals only, the shortcut is a no-op rather than a way to reach
   * a screen that cannot work.
   */
  nextSection() {
    if (pinned) return
    const available = APP_ORDER.filter((s) => this.canShow(s))
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

  /** From the error screen on a phone: pair with a computer, or pick another
   *  one it already knows, rather than trying the last one again. */
  chooseComputer() {
    this.error = null
    this.screen = 'setup'
  }

  /** Whether the window is drawing a dark theme: what a mail body or a feed
   *  story asks before it is rendered to match. */
  get dark(): boolean {
    return themeInfo(this.shown).scheme === 'dark'
  }

  /** The colours a framed document -- a mail body, a feed story -- is drawn
   *  in for the theme on screen. Reactive, so a renderer that reads it is
   *  redrawn by a change between two light themes as well as to dark. */
  get frame(): Frame {
    return themeInfo(this.shown).frame
  }

  applyTheme() {
    this.shown = resolveTheme(this.theme)
    showTheme(this.shown)
    themePref.set(this.theme)
    tellShellTheme(this.dark)
    // The panel in the corner of the screen draws its cards in this theme.
    // The main window's to say, on a desktop: a window of one app follows
    // this one's choice already, and a phone or a browser has no panel.
    if (hasShell && !mobileShell && !pinned) void api.setPanelTheme(this.shown).catch(() => {})
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

  /**
   * Come back in.
   *
   * Two different questions wearing one screen. If the vault behind this
   * window is still open -- because only *this* window's screen went dark,
   * and the machine holding the vault carried on serving it to the phone in
   * the other room and to the assistant's own scheduler -- then all that is
   * needed is proof of the person, and `verify_password` gives it without
   * opening or closing anything. If the vault is genuinely locked, this is an
   * unlock. Both cost the same Argon2 derivation and both count against the
   * same lockout, so neither is the cheap way in.
   */
  async unlock(password: string) {
    this.error = null
    // Held across the whole thing, including `enterMain`. The backend raises a
    // lock-state event for this unlock, and the live router must not answer it
    // by starting a second load of the same lists beside this one.
    this.unlocking = true
    try {
      if (this.screenOnly) {
        await api.verifyPassword(password)
        this.status = await api.status()
      } else {
        this.status = await api.unlock(password)
      }
      this.screenOnly = false
      await this.enterMain()
    } catch (e) {
      this.error = errorMessage(e)
      throw e
    } finally {
      this.unlocking = false
    }
  }

  /**
   * Hide what this window is showing, and leave the vault open.
   *
   * What `Ctrl/Cmd L` and the idle timer do. The teardown is the same one
   * `#lock` does -- decrypted content must not sit behind a lock screen -- but
   * no `lock` is sent, because the vault is not this window's to close. Other
   * windows keep working and the assistant keeps its appointments.
   */
  async lockScreen() {
    if (this.#locking || this.screen !== 'main') return
    this.#locking = true
    try {
      await this.flushAll()
      this.#teardown()
      // In a browser on web access, a dark screen is a signed-out session
      // too: the cookie that could have carried on reading is ended on the
      // server, so coming back needs the password whoever is holding the
      // phone. `verify_password` then opens a fresh one -- see `web.ts`.
      if (isWeb) await signOut()
      this.screenOnly = true
      this.screen = 'locked'
    } finally {
      this.#locking = false
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
    // Write before dropping anything, or a lock taken within the autosave
    // window throws away whatever was typed in it. *Everything*, not just the
    // open entry: `#teardown` below blanks every app's store, so a note or a
    // task half-written a moment ago has nowhere left to be written from.
    await this.flushAll()
    this.#teardown()
    this.status = await api.lock()
    this.screenOnly = false
    this.screen = 'locked'
  }

  /**
   * Drop decrypted content from the interface.
   *
   * Shared by all three ways out of the main screen -- this window locking its
   * screen, this window locking the vault, and the vault being locked
   * somewhere else -- because forgetting one of the lists would be a leak
   * behind a lock screen, and three copies of the sweep is three chances to.
   */
  #teardown() {
    this.stopTimers()
    // A retry scheduled by a failed save has nothing left to write once the
    // entry below is dropped, and would fire against a locked vault.
    if (this.#saveTimer) clearTimeout(this.#saveTimer)
    this.#saveTimer = null
    this.#retryDelay = null
    this.#baseVersion = null
    this.#untouched = null
    this.#unstored = null
    this.#savedEdits = this.#edits
    this.#pulled = null
    // A list or an entry still being read lands on nothing: its answer is
    // vault contents, and the screen is about to be the lock screen.
    this.#opening++
    this.#choosing++
    this.#rowsAsked++
    this.#entriesLoading = null
    this.conflict = false
    for (const reset of this.#resetHooks) reset()
    this.entry = null
    this.entries = []
    this.#entriesLoaded = false
    this.journals = []
    this.results = []
    this.query = ''
    this.selectedEntry = null
  }

  private async enterMain() {
    this.screen = 'main'
    // A vault opened on a backend that stores journals only cannot show the
    // section the last one left us in.
    if (!this.canShow(this.section)) {
      // A window of one app has nothing else to show: it closes, through
      // the same save as any other close.
      if (pinned) {
        windowControls.close()
        return
      }
      this.section = 'journal'
    }
    await this.refreshJournals()
    this.startLockTimers()
    // The journal's list and its newest page: read now if the journal is
    // what is showing, and otherwise once the window is idle, so it is
    // still instant when somebody switches to it. A window of another app
    // alone never shows the journal, and never reads them.
    if (pinned && pinned !== 'journal') return
    if (this.section === 'journal') await this.ensureEntries()
    else
      whenIdle(() => {
        if (this.screen === 'main') void this.ensureEntries()
      }, 2_000)
  }

  /**
   * The two clocks that can send this window back to the lock screen.
   *
   * The screen timer is *local*: idleness at this keyboard is a fact about
   * this window, so it is measured here and costs no round trip at all. That
   * is also what makes it right for a remote session, where the old
   * five-second poll was a request over TLS to ask whether somebody had
   * touched a keyboard on this side of the wire.
   *
   * The key timer belongs to the machine holding the vault, so a window in
   * that process asks it. A remote client does not: it is told by the
   * `lockState` event, which it has to act on anyway.
   *
   * Neither clock can lock anything while `opensItself` is on. The key is
   * kept in this machine's keychain, so signing in to the computer already
   * is unlocking it -- hiding the screen on a timer would only ask, a few
   * minutes later, for a password the OS login already stood in for. The
   * screen timer below is told directly; the key's own timer is the
   * backend's to stop, on the same condition, so `#lockTimer`'s poll needs
   * no change here -- it keeps polling, and keeps hearing that the key is
   * not going anywhere. Always false for a remote session -- the keychain in
   * play there is the other machine's, not this one's -- so this changes
   * nothing about a remote window's own screen timer.
   */
  private startLockTimers() {
    this.stopTimers()
    this.#idleSince = Date.now()
    this.#screenTimer = setInterval(() => {
      const seconds = this.status?.autoLockSeconds ?? 0
      if (seconds <= 0 || this.opensItself || this.screen !== 'main') return
      const since = Math.max(this.#idleSince, activeElsewhere())
      if (Date.now() - since >= seconds * 1000) void this.lockScreen()
    }, IDLE_POLL_MS)
    if (this.remote) return
    this.#lockTimer = setInterval(async () => {
      try {
        if (await api.pollAutoLock()) await this.lockedElsewhere()
      } catch {
        /* a transient failure here must never crash the interface */
      }
    }, AUTOLOCK_POLL_MS)
  }

  stopTimers() {
    if (this.#lockTimer) clearInterval(this.#lockTimer)
    this.#lockTimer = null
    if (this.#screenTimer) clearInterval(this.#screenTimer)
    this.#screenTimer = null
    this.#cancelList()
  }

  /**
   * Called from real user interaction. Defers both lock clocks.
   *
   * Wired to the window in `App.svelte` -- a key, a press, a wheel -- and not
   * only to the editors. It used to be reached from the two rich-text
   * `onUpdate` handlers and nowhere else, which meant the screen clock was
   * deferred by *typing prose* and by nothing else at all: an hour spent
   * dragging blocks around the calendar, working a task board or talking to
   * the assistant looked exactly like an empty chair, and the window dropped
   * to the lock screen mid-use. Reading a long entry did the same.
   *
   * Deliberately not `pointermove`. A screen lock is meant to fire while
   * somebody is away from the keyboard, and a mouse nudged by a passing lorry
   * is not somebody being there.
   */
  touch() {
    // Past the lock screen only. A key pressed at the password box is not the
    // vault being used, and must not defer the timer that forgets its key.
    if (this.screen !== 'main') return
    this.#idleSince = Date.now()
    // The backend needs to know the user is alive, not how fast they type.
    // Unthrottled this was one IPC round trip per keystroke.
    const now = Date.now()
    if (now - this.#lastTouch < TOUCH_MS) return
    this.#lastTouch = now
    void api.touch().catch(() => {})
    try {
      localStorage.setItem(ACTIVE_AT, String(now))
    } catch {
      // Storage refused (full, or a stand-in under test): each window then
      // keeps its own clock, as every window did before there were two.
    }
  }

  /**
   * Whether a turn of the wheel is worth hearing now, as `touch`: a lock
   * timer is running and half of it has gone by since anything in any window
   * of this app here deferred it. Only then does `App.svelte` listen for the
   * wheel at all -- see `lib/wheel.ts` for what a wheel listener costs every
   * scroll -- and half is early enough that reading with the wheel alone, a
   * turn every so often, still never reaches the lock or the key's own
   * clock, which `touch` pings at most `TOUCH_MS` apart.
   */
  wheelWanted(): boolean {
    const seconds = this.status?.autoLockSeconds ?? 0
    if (seconds <= 0 || this.opensItself || this.screen !== 'main') return false
    return Date.now() - Math.max(this.#idleSince, activeElsewhere()) >= (seconds * 1000) / 2
  }

  // ── the open document ────────────────────────────────────────────────

  /** Register (or with `null`, retire) the editor's document getter. */
  bindBody(fn: (() => Entry['body']) | null) {
    this.#body.bind(fn)
  }

  /** Pull the editor's current document into state. Cheap enough per save. */
  syncBody() {
    const body = this.#body.read()
    if (!this.entry || body === undefined) return
    this.entry.body = body
    this.#pulled = { of: this.entry, body }
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
   * Ask for a list refresh, for a change made somewhere else. Coalescing:
   * repeated calls inside the window collapse into the single trailing
   * refresh, so a burst of changes costs one re-render rather than one each.
   * Nothing at all while the list has not been read, or begun to be: the
   * first read will have it. (One under way may have been answered before
   * the change.)
   */
  queueListRefresh() {
    if ((!this.#entriesLoaded && !this.#entriesLoading) || this.#listTimer) return
    this.#armList(Math.max(0, LIST_REFRESH_MS - (Date.now() - this.#lastListRefresh)))
  }

  /**
   * The same after an autosave: once the typing pauses for `LIST_IDLE_MS`,
   * or `LIST_MAX_WAIT_MS` after the first save it covers if it never does.
   * Each save moves it on, so a paragraph typed in one go is one refresh
   * when it is finished rather than one every few seconds while it is being
   * written.
   */
  #listAfterSave() {
    const now = Date.now()
    this.#listSince ??= now
    this.#armList(Math.min(LIST_IDLE_MS, Math.max(0, this.#listSince + LIST_MAX_WAIT_MS - now)))
  }

  #armList(wait: number) {
    if (this.#listTimer) clearTimeout(this.#listTimer)
    this.#listTimer = setTimeout(() => {
      this.#listTimer = null
      this.#listSince = null
      this.#lastListRefresh = Date.now()
      void this.refreshRows().catch(() => {})
    }, wait)
  }

  /** Drop a queued list refresh; the caller reads the list itself. */
  #cancelList() {
    if (this.#listTimer) clearTimeout(this.#listTimer)
    this.#listTimer = null
    this.#listSince = null
  }

  /** Run any queued list refresh now. */
  private async settleList() {
    if (!this.#listTimer) return
    this.#cancelList()
    this.#lastListRefresh = Date.now()
    await this.refreshRows()
  }

  /** Re-read the list rows for the current filter. */
  private async refreshRows() {
    const asked = ++this.#rowsAsked
    const rows = await api.entries({
      journalId: this.selectedJournal,
      // "All entries" is theirs: the assistant's notes are in its journal.
      notJournal: this.selectedJournal ? null : this.assistantJournalId,
      starred: this.showStarredOnly ? true : null,
      sort: 'dateDesc',
      limit: 500,
    })
    // A read asked for after this one -- another journal chosen meanwhile --
    // is the one to show.
    if (asked !== this.#rowsAsked) return
    this.entries = rows
    this.#entriesLoaded = true
  }

  /** The list has been read since the vault was opened. Before it has, an
   *  empty list is not yet "no entries". */
  get entriesLoaded(): boolean {
    return this.#entriesLoaded
  }

  /**
   * Read the list, and open its newest entry, if that has not happened since
   * the vault was opened. What showing the journal calls (`JournalView`),
   * rather than the unlock: most windows open on another app. Shares a read
   * already under way.
   */
  ensureEntries(): Promise<void> {
    if (this.#entriesLoaded) return Promise.resolve()
    if (this.#entriesLoading) return this.#entriesLoading
    const loading: Promise<void> = this.#readEntries().finally(() => {
      // A lock meanwhile let go of this one already, and the next unlock's
      // may be under way.
      if (this.#entriesLoading === loading) this.#entriesLoading = null
    })
    this.#entriesLoading = loading
    return loading
  }

  async #readEntries() {
    try {
      this.#cancelList()
      this.#lastListRefresh = Date.now()
      await this.refreshRows()
      // The newest -- unless something was opened while the list was being
      // read, a link from another app to one entry, say.
      const first = this.entries[0]
      if (first && this.selectedEntry === null && this.entry === null) {
        await this.openEntry(first.id)
      }
    } catch (e) {
      await handle(e)
    }
  }

  async refreshEntries() {
    try {
      this.#cancelList()
      this.#lastListRefresh = Date.now()
      await this.refreshRows()
      // Keep a selection if it is still in view; otherwise open the newest.
      // A page started from a template that the vault does not have yet is
      // never in the list, and is kept all the same -- while the list is
      // still one it belongs in. Picking another journal leaves it.
      if (!this.entries.some((e) => e.id === this.selectedEntry) && !this.#heldPageInView()) {
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

  /** The unsaved templated page is open and the list on screen is one it
   *  would be in: its own journal or all of them, not starred-only. */
  #heldPageInView(): boolean {
    const entry = this.entry
    if (!entry || this.#unstored !== entry.id || this.selectedEntry !== entry.id) return false
    return (
      !this.showStarredOnly &&
      (this.selectedJournal === null || this.selectedJournal === entry.journalId)
    )
  }

  /** Open an entry to be read -- from a link in another app, say -- which on
   *  a phone means its page over the list (`details`), not the list. */
  async showEntry(id: EntryId) {
    details.open('journal')
    await this.openEntry(id)
  }

  async openEntry(id: EntryId) {
    const ticket = ++this.#opening
    // Chosen at once: the row shows it, and a held j or k steps on from
    // here rather than asking for the same entry again.
    this.selectedEntry = id
    // Read while the page being left is written: they are different
    // entries. The same entry is read after its write, or the read could
    // miss it -- and so is any entry after a page the vault does not have
    // yet, whose first write can add it to another (`#mergeInto`).
    const left = this.entry
    const reading = left && (left.id === id || this.#unstored === left.id) ? null : api.entry(id)
    void reading?.catch(() => {})
    // Never let a pending autosave land after we have switched documents.
    await this.flush()
    // The row being left catches up with its title, without holding up the
    // page.
    void this.settleList().catch(() => {})
    if (ticket !== this.#opening) return
    // Leaving a page started from a template that nothing was typed into
    // drops it; it was never written. (One that was typed into was written
    // by the flush above.) See `#untouched`.
    if (this.#untouched !== id) this.#untouched = null
    if (this.#unstored !== id) this.#unstored = null
    this.selectedEntry = id
    try {
      const entry = await (reading ?? api.entry(id))
      // Another entry was asked for meanwhile, and is the one to show.
      if (ticket !== this.#opening) return
      this.entry = entry
      this.#baseVersion = entry.updatedAt
      this.#savedEdits = this.#edits
      this.#pulled = null
      // A conflict belongs to the document that had one, not to the window.
      this.conflict = false
    } catch (e) {
      if (ticket === this.#opening) await handle(e)
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
    // One of theirs when none is chosen -- never the assistant's own, which
    // takes an entry only when somebody picks it to leave it a note.
    const journalId = this.selectedJournal ?? this.ownJournals[0]?.id
    if (!journalId) return
    // Today's page is what was asked for, wherever from: on a phone, over
    // the list.
    details.open('journal')
    await this.flush()

    const today = todayIso()
    let existing: EntrySummary | undefined
    try {
      existing = (await api.entries({ journalId, from: today, to: today, limit: 1 }))[0]
    } catch (e) {
      return void (await handle(e))
    }
    if (existing) {
      await this.showTheList()
      await this.openEntry(existing.id)
      notify.info("You have already started today's entry", {
        body: 'A journal keeps one entry a day, so this is that one — carry on writing in it.',
        key: 'entry-today',
      })
      return
    }

    let entry: Entry
    let templated = false
    try {
      entry = await api.newEntry(journalId)
      // The backend fills in the journal's template for the day; a page that
      // came back with anything on it is held unsaved -- see `#untouched`.
      templated = !!entry.title.trim() || !isBlankDoc(entry.body)
      if (!templated) await api.saveEntry(entry, null)
    } catch (e) {
      return void (await handle(e))
    }
    this.#untouched = templated ? entry.id : null
    this.#unstored = templated ? entry.id : null
    // Nothing stored yet means the first write is a create.
    this.#baseVersion = templated ? null : entry.updatedAt
    this.conflict = false
    // Over any entry still being opened, which would otherwise land after it.
    this.#opening++
    this.entry = entry
    this.#savedEdits = this.#edits
    this.#pulled = null
    this.selectedEntry = entry.id
    await this.showTheList()
  }

  /**
   * Put the list back to something today's entry can appear in.
   *
   * Two filters can hide it and both had to be cleared, though only one of
   * them was. A new entry is not starred, so the Starred view is not a list
   * it can be in; and while a search is running the panel draws `results`
   * rather than `entries`, so the row is not merely filtered out, it is not
   * the thing being drawn at all. Either way the editor opens an entry the
   * list beside it cannot show, which reads as the entry having gone
   * somewhere else.
   *
   * The *journal* selection is deliberately left alone: narrowing "All
   * entries" to one journal as a side effect of asking for today's page
   * would be answering a question nobody asked.
   */
  private async showTheList() {
    this.showStarredOnly = false
    this.clearSearch()
    await this.refreshEntries()
  }

  /**
   * Queue a save. Repeated edits collapse into one write.
   *
   * The journal keeps its own timer rather than an `Autosave` like the other
   * two stores, and the difference is real: there is one open document, and
   * the editor -- not this store -- holds the truth about it while it is
   * open. So it counts edits rather than keeping a set of dirty ids: every
   * change to the open entry -- the editor's, the title's, a tag -- comes
   * through here, and `flush` pulls the body and writes only when something
   * has. Only the delay is shared, so all three agree on it.
   */
  scheduleSave() {
    this.#edits++
    // Something was typed, so a page started from a template is somebody's
    // writing now and is saved like any other.
    if (this.#untouched === this.entry?.id) this.#untouched = null
    // The text has moved, so any write still being retried is superseded: it
    // must not be answered from the record, because the record holds the
    // *older* text. A new stamp is minted by the next attempt.
    this.#writeStamp = null
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
  /**
   * Add a page that was never written to the day's entry that turned up in
   * the meantime, and open that one: its text after theirs, its tags and
   * pictures added, its title only if theirs has none. Nothing typed is
   * lost, and the day stays one entry.
   */
  async #mergeInto(id: EntryId, page: Entry) {
    const day = await api.entry(id)
    const theirs = day.body.content ?? []
    const mine = $state.snapshot(page.body).content ?? []
    day.body = { ...day.body, content: [...theirs, ...mine] }
    if (!day.title.trim()) day.title = page.title
    day.tags = mergeTags(day.tags, page.tags)
    day.attachments = [...day.attachments, ...page.attachments]
    const expect = day.updatedAt
    day.updatedAt = new Date().toISOString()
    await api.saveEntry(day, expect)
    this.#untouched = null
    this.#unstored = null
    if (this.entry?.id === page.id) {
      this.entry = day
      this.selectedEntry = day.id
      this.#baseVersion = day.updatedAt
      this.#pulled = null
    }
    notify.info("Today's entry had already been started", {
      body: 'What you wrote here was added to the end of it.',
      key: 'entry-today',
    })
  }

  /**
   * Write the open entry now, even if it is a page started from a template
   * that nothing has been typed into -- for what reads it back from the vault
   * ("Read the day back", "Suggest a title"), which has nothing to read
   * otherwise.
   */
  commit(): Promise<void> {
    if (this.#untouched === this.entry?.id) this.#untouched = null
    return this.flush()
  }

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
    // Nothing typed since the last write that landed: the vault already has
    // it. Moving through the list used to write every entry it left. A page
    // the vault does not have yet is the exception, once it has been typed
    // into (`#untouched`, below).
    if (this.#edits === this.#savedEdits && this.#unstored !== entry.id) return
    const edits = this.#edits
    this.syncBody()
    // A page that still holds only its template is not written -- see
    // `#untouched`.
    if (this.#untouched === entry.id) return
    this.saving = true
    try {
      // One stamp per *logical* write, reused by every retry of it.
      //
      // Both halves matter and they have to move together. A fresh
      // `updatedAt` per attempt is what made a retry a different write; a
      // fresh request id per attempt is what stopped the backend recognising
      // it as the same one. So a save that landed and whose answer was lost
      // to a dropped connection came back as a conflict against its own
      // earlier write, and the editor offered "keep mine" for text that was
      // already on disk. Cleared on success, and by `scheduleSave` when the
      // text moves under it.
      this.#writeStamp ??= { updatedAt: new Date().toISOString(), requestId: newRequestId() }
      const stamp = this.#writeStamp
      // The first write of a page held unsaved is a create, and the day may
      // have been started meanwhile -- by another window, or the assistant
      // -- since nothing of this one was there for them to find. One entry
      // per journal per day holds by adding this page to that one rather
      // than filing a second.
      if (this.#unstored === entry.id) {
        const twin = (
          await api.entries({
            journalId: entry.journalId,
            from: entry.localDate,
            to: entry.localDate,
            limit: 1,
          })
        )[0]
        if (twin && twin.id !== entry.id) {
          this.#writeStamp = null
          await this.#mergeInto(twin.id, entry)
          this.#savedEdits = edits
          return
        }
      }
      entry.updatedAt = stamp.updatedAt
      // The document as the editor handed it over, when that is what is in
      // `entry` -- copying it back out of reactive state walks every node of
      // it. The rest of the entry is small.
      const pulled = this.#pulled?.of === entry ? this.#pulled.body : null
      this.#pulled = null
      const { body, ...rest } = entry
      const payload: Entry = { ...$state.snapshot(rest), body: pulled ?? $state.snapshot(body) }
      await api.saveEntry(payload, this.#baseVersion, stamp.requestId)
      if (this.#unstored === entry.id) this.#unstored = null
      this.#writeStamp = null
      this.#savedEdits = edits
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
      // The entry is still only in the editor, and still counted as edited
      // -- nothing above marked it written -- so retrying is a matter of
      // putting the timer back, with a widening gap so a vault on a full
      // disk is not written to forty times a minute.
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
      // Not after a save that ended in a lock: the refresh would fire a
      // moment later against a vault that is no longer open.
      if (this.screen === 'main') this.#listAfterSave()
    }
  }

  /** The open entry was started from a template and nothing has been typed
   *  into it, so it has not been written. See `#untouched`. */
  get untouched(): boolean {
    return this.#untouched !== null && this.#untouched === this.entry?.id
  }

  /**
   * The open entry's id, if the vault has it -- what a reading, the
   * assistant or anything else that stores a pointer to "this entry" should
   * use. `null` for a page that has not been written yet: a pointer to it
   * would name nothing once the page was left.
   */
  get storedEntryId(): EntryId | null {
    const entry = this.entry
    if (!entry || this.#unstored === entry.id) return null
    return entry.id
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
    const edits = this.#edits
    this.syncBody()
    this.#pulled = null
    this.saving = true
    try {
      entry.updatedAt = new Date().toISOString()
      await api.saveEntryForce($state.snapshot(entry))
      this.#savedEdits = edits
      this.#baseVersion = entry.updatedAt
      this.lastSaved = entry.updatedAt
      this.conflict = false
      this.#retryDelay = null
    } catch (e) {
      await handle(e)
    } finally {
      this.saving = false
      if (this.screen === 'main') this.#listAfterSave()
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
    if (this.#unstored === id) {
      // Never written, so there is nothing to delete but the page itself.
      this.#untouched = null
      this.#unstored = null
      return void (await this.refreshEntries())
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
    // The open page has not been written yet: change it and let the write
    // path create it, which is the one place the day is checked for a twin.
    if (this.entry?.id === id && this.#unstored === id) {
      change(this.entry)
      this.#untouched = null
      await this.flush()
      await this.refreshEntries()
      return
    }
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

  /**
   * Say what a day's writing was for.
   *
   * Set on very few entries, and that is the expected shape — a journal is
   * not a work log. It is here so the fortnight you wrote every evening
   * about learning to sail is evidence the goal was alive, which is a thing
   * no task and no block records.
   */
  async setEntryPurpose(id: EntryId, purpose: Purpose | null) {
    await this.#editEntry(id, (entry) => (entry.purpose = purpose))
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
        notJournal: this.selectedJournal ? null : this.assistantJournalId,
        from,
        to,
        sort: 'dateAsc',
        limit: 500,
      })
      for (const row of rows) if (!out.has(row.localDate)) out.set(row.localDate, row.id)
    } catch (e) {
      // Not worth a message over the window: the calendar is an aid beside
      // the list, and the list is the thing that has to be right.
      await quietly(e)
    }
    return out
  }

  // ── search ───────────────────────────────────────────────────────────

  /** Debounced, so typing does not fire a query per keystroke. */
  setQuery(q: string) {
    this.query = q
    if (!q.trim()) {
      this.#search.cancel()
      this.results = []
      this.searching = false
      return
    }
    this.searching = true
    this.#search.call(q)
  }

  async #runSearch(q: string) {
    try {
      // Entries only. This box is the journal's, and quietly mixing notes
      // into a list whose rows carry a journal colour and a date would be
      // answering a question nobody asked. Narrowed here rather than cast
      // at the drawing end, so the rows genuinely have the fields they use.
      const hits = await api.search(q, this.selectedJournal, 50, 'entry')
      this.results = hits.filter((h): h is EntryHit => h.type === 'entry')
    } catch (e) {
      await handle(e)
    } finally {
      this.searching = false
    }
  }

  clearSearch() {
    // The pending query goes too. Without this a search typed a moment ago,
    // still waiting on `#search`'s timer, lands afterwards and refills a
    // list that had been emptied on purpose -- results for a query no
    // longer in the box.
    this.#search.cancel()
    this.query = ''
    this.results = []
    this.searching = false
  }

  // ── derived ──────────────────────────────────────────────────────────

  get journal(): Journal | null {
    return this.journals.find((j) => j.id === this.selectedJournal) ?? null
  }

  /**
   * What the journal has on screen, for the assistant -- see `onscreen.ts`.
   *
   * `journalShowing` rather than `showing`: this store is the window's as
   * well as the journal's, and every other app's store answers the same
   * question under the plain name.
   */
  get journalShowing(): Showing {
    const entry = this.entry
    return {
      view: this.showStarredOnly ? 'starred entries' : null,
      within: seen('journal', this.selectedJournal, this.journal?.name ?? ''),
      // Not a page that has not been written: the assistant would be handed
      // an id no tool can find.
      open: seen('entry', this.storedEntryId, entry?.title ?? ''),
      query: this.query,
    }
  }

  get accent(): string {
    const j = this.entry ? this.journals.find((x) => x.id === this.entry!.journalId) : this.journal
    return j?.color ?? 'var(--accent)'
  }
}

export const app = new AppState()

// The theme, when it is changed in another window: a window of one app has no
// Settings of its own, and should not stay light after the main window went
// dark. `storage` fires in every other window of this origin, never the one
// that wrote.
if (typeof window !== 'undefined' && typeof window.addEventListener === 'function') {
  window.addEventListener('storage', (e) => {
    if (e.key !== 'jatayu.theme') return
    app.theme = parseThemeChoice(e.newValue)
    app.applyTheme()
  })
  // And when the desktop goes dark, if the choice was to follow it.
  watchSystemScheme(() => {
    if (app.theme === 'system') app.applyTheme()
  })
}

// `errors.ts` states the policy; this is the line that gives it the session to
// apply it to. Here rather than in that file so it depends on nothing, which
// is what lets `autosave.ts` be tested without a window. See its header.
setPolicy({ lock: () => app.lock(), report: (message) => (app.error = message) })

/** Whether a document is the empty page a new entry starts as: nothing, or one
 *  empty paragraph. Anything else came from a template. */
function isBlankDoc(doc: Entry['body']): boolean {
  const blocks = (doc.content ?? []) as { type?: string; content?: unknown[] }[]
  return blocks.every((b) => b.type === 'paragraph' && !b.content?.length)
}
