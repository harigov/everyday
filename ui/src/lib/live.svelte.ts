// A write that landed somewhere else, and the list that has to notice.
//
// One window's save is another window's stale list. On a local vault the two
// windows are the same one and it already knows; under server mode they are two
// machines, and this is how the second finds out.
//
// The backend raises a `changed` event after any command that writes, carrying
// what it touched and who did it. The origin is compared on the *server* side
// for a remote client and here for a local one, so a window never reloads under
// its own cursor mid-scroll.
//
// # Why this is a router and not a subscription per store
//
// Because the granularity of the event is deliberately coarse -- an app, not a
// table -- and the thing that reloads is a store's own `refresh`, which each
// store already has for its own reasons. A subscription per store would mean
// every store learning about the event system; one router means adding an app
// is adding a line here.

import {
  api,
  onChange,
  onLockState,
  onOwnWrite,
  onPalette,
  onShowInMain,
  onTalk,
  type OpenItem,
  type ShowInMain,
} from './api'
import type { Section } from './apps'
import { ownOrigin, pinned } from './appwindow'
import { quietly } from './errors'
import { isoDate } from './time'
import { onWebStream } from './web'
import { accounts } from './accounts.svelte'
import { calendar } from './calendar.svelte'
import { feed } from './feed.svelte'
import { files } from './files.svelte'
import { health } from './health.svelte'
import { jobs } from './jobs.svelte'
import { library } from './library.svelte'
import { mail } from './mail.svelte'
import { assistant } from './assistant.svelte'
import { briefs } from './brief.svelte'
import { meetings } from './meetings.svelte'
import { money } from './money.svelte'
import { applierFor, registerApply, type ChangeWithIds } from './live-apply'
import { notes } from './notes.svelte'
import { overview } from './overview.svelte'
import { proposals } from './proposals.svelte'
import { purpose } from './purpose.svelte'
import { screentime } from './screentime.svelte'
import { isSettingsTab, panels } from './panels.svelte'
import { app } from './state.svelte'
import { templates } from './templates.svelte'
import { player } from './player.svelte'
import { todo } from './todo.svelte'
import { targets as goalTargets } from './targets.svelte'
import { tracking } from './tracking.svelte'
import { upnext } from './upnext.svelte'
import { voice } from './voice.svelte'
import type { ChangeEvent, ChangeKind } from './types'

/**
 * How long to gather changes before acting on them.
 *
 * A calendar sync writes a thousand events and a board reorder writes forty
 * tasks, each of which announces itself. Reloading once per row would be a
 * thousand round trips to draw one grid. The window is short enough that a
 * single edit on another machine still feels immediate.
 */
const COALESCE_MS = 250

/**
 * How long the stream has to stay down before the window says so.
 *
 * A phone changing from wifi to mobile data, or a laptop waking, drops the
 * stream for a second or two and picks it up again by itself; a notice that
 * came and went each time would be the flapping it is meant to prevent.
 */
const DOWN_MS = 3000

/**
 * Whether `section` is the app this window is showing to somebody: the one
 * on screen, in a window not hidden away in the tray.
 *
 * An app that is not is not read again for every change that lands. Its
 * store is marked stale instead, and reads in full on its own way back --
 * its `start`, when the view mounts, or `HELD` below when the window is
 * shown again.
 */
function onScreen(section: Section): boolean {
  return app.section === section && document.visibilityState !== 'hidden'
}

/** An app whose reloads wait while it is off screen. See `HELD`. */
interface Held {
  section: Section
  /** The reloads in `RELOAD` that are its. */
  targets: readonly ReloadTarget[]
  stale: () => boolean
  markStale: () => void
  start: () => Promise<unknown>
}

/**
 * The apps whose reloads wait while they are off screen (see `onScreen`),
 * and how each catches up: its own `start`, which reads in full once marked
 * stale and draws what it has otherwise.
 */
const HELD: readonly Held[] = [
  {
    section: 'feed',
    targets: ['feed'],
    stale: () => feed.stale,
    markStale: () => feed.markStale(),
    start: () => feed.start(),
  },
  {
    section: 'library',
    targets: ['library'],
    stale: () => library.stale,
    markStale: () => library.markStale(),
    start: () => library.start(),
  },
  {
    section: 'money',
    targets: ['money'],
    stale: () => money.stale,
    markStale: () => money.markStale(),
    start: () => money.start(),
  },
  {
    section: 'screentime',
    targets: ['screentime', 'screentimeSpans'],
    stale: () => screentime.stale,
    markStale: () => screentime.markStale(),
    start: () => screentime.start(),
  },
  {
    section: 'health',
    targets: ['health', 'healthSamples'],
    stale: () => health.stale,
    markStale: () => health.markStale(),
    start: () => health.start(),
  },
]

/**
 * The things that can be reloaded, by name.
 *
 * Named rather than referred to by function, and that is the whole reason this
 * indirection exists. A batch routinely carries several kinds that reload the
 * *same* store -- a task and a project, an item and a log row -- and
 * deduplicating by the function meant comparing two different arrow literals
 * that happened to call the same method. They never matched, so a calendar
 * sync reloaded the grid three times in a row.
 *
 * What actually runs for a target is not always this, though: a store that
 * has called `registerApply` (`live-apply.ts`) gets first refusal, and this
 * only runs when it declines. Kept here regardless, because a store can
 * always decline, and a target with nothing registered always does.
 */
export const RELOAD = {
  accounts: () => accounts.refresh(),
  journals: () => app.refreshJournals(),
  entries: () => app.queueListRefresh(),
  todo: () => todo.refresh(),
  calendar: () => calendar.refreshIfOpen(),
  notes: () => notes.refresh(),
  // The one list of templates, which Settings, a journal's settings and both
  // editors read. Only once something has read it since unlock.
  templates: () => templates.liveRefresh(),
  // The assistant's own store registered an `apply` for this target: off
  // screen only the count on the bar is read, and on screen the few runs a
  // batch names are read again one by one. This is for a wider batch on
  // screen, or a log nothing has read yet. The briefs, which are routines
  // too, have an `apply` of their own registered below -- a run finishing is
  // a brief arriving.
  assistant: () => assistant.refresh(),
  // Work the assistant is doing in the background: a job moving from queued
  // to running to done, a line of progress, a cancel from another window.
  // Reloads the list and the count together, since the app bar draws one
  // and Settings → "What it did" the other. `jobs.svelte.ts` registered an
  // `apply` that reads only the jobs a batch names, so this is for a batch
  // naming more.
  jobs: () => jobs.refresh(),
  // A fresh read, not a share of one already in the air: that may have left
  // before the write this change is about.
  proposals: () => proposals.refresh(true),
  shelves: () => library.refreshKinds(),
  // Only while the Library is on screen: it reads in full when it comes back.
  library: () => (onScreen('library') ? library.refresh() : library.markStale()),
  // `thread` and `draft` below route here for anything `mail.svelte.ts`'s
  // own `registerApply` declines -- a batch wider than one record.
  mail: () => mail.refresh(),
  // A folder or label a sync pass met for the first time -- see `targetsFor`
  // for why this one is not in `RELOADS`. Only where Mail has been opened,
  // and held while nobody can see it: see `mailboxesChanged`.
  mailboxes: () => mail.mailboxesChanged(),
  tracking: () => tracking.refresh(),
  overview: () => overview.liveRefresh(),
  // Areas and goals, which three surfaces now draw: the todo app's goals
  // pane, every purpose picker, and whatever the Overview has on it. One
  // reload for all of them, because there is one copy -- see `purpose`.
  purpose: () => purpose.load(true),
  // How each goal is doing against its targets: the meters on the todo
  // sidebar's goal rows and in a goal's Targets section. See `TARGET_FEEDING`.
  goalTargets: () => goalTargets.refresh(),
  // Settings that moved: the auto-lock, the assistant's configuration. What
  // draws them re-reads on open, so the useful thing is the status word.
  // Settings → Feed is one of them, read again only once it has been read
  // since the vault opened: nothing else draws it.
  status: () =>
    Promise.all([app.refreshStatus(), upnext.settings ? upnext.loadSettings() : undefined]),
  // A recording's stage moved, or a voiceprint was learned or forgotten --
  // another window's write, or this vault's own background pipeline
  // finishing or failing a call. One target for both kinds; see
  // `meetings.svelte.ts`'s own `liveRefresh`.
  meetings: () => meetings.liveRefresh(),
  // A source followed or changed, or a story arrived or was read elsewhere.
  // Both halves, since the sidebar's counts and the list move together; the
  // list reloads over what it has and keeps the open story in its place, so
  // a refresh landing mid-read does not take the pane's footing away.
  //
  // An episode finishing its download is a story change too, and while any
  // is under way the downloads are asked after again: that is what tells Up
  // Next a file has arrived. So is one that has been transcribed, or an
  // article written from it, and the list of background work is asked again.
  //
  // The two halves only while the Feed is on screen: a background pass lands
  // about once a minute, and a list of hundreds read again for a window
  // showing something else is wasted. It reads both when it comes back. Up
  // Next's half always, since the player draws it in every app.
  feed: () =>
    Promise.all([
      onScreen('feed') ? Promise.all([feed.refreshSources(), feed.reload()]) : feed.markStale(),
      upnext.storyChanged(),
    ]),
  // Up Next changed in another window, or by something in the background: a
  // show's new episode put in it, a top-up from Recommended. Read again at
  // once -- the player moves on to whatever is next in it.
  upnext: () => upnext.queueChanged(),
  // Something was read, heard or watched, here or on another device. A
  // device playing reports every fifteen seconds, so the store gathers a
  // burst into one read of Up Next and Recommended -- and of History, while
  // its page is open: see `consumptionChanged`.
  consumption: () => upnext.consumptionChanged(),
  // A bank synced in the background, or a transaction, category or rule
  // changed on another machine. One reload of everything Money shows --
  // the banks, the month's list and summary, and whichever report is on
  // screen -- and only once the app has been opened since the vault did:
  // a year of reports nobody has asked for is not worth reading for a
  // sync that lands while somebody is writing in the journal. It reads
  // fresh when it is opened. Nor while another app is on screen, for the
  // same reason: it is marked stale and reads in full when it comes back.
  money: () => (money.loaded && onScreen('money') ? money.reload() : money.markStale()),
  // A label or a category changed on another machine. Read again only
  // while Screen time is the app on screen, and only once it has been
  // opened since the vault did: otherwise it is marked stale and reads in
  // full when it is next opened, and nothing outside it draws a label but a
  // goal's measure picker, which asks for its own.
  screentime: () =>
    screentime.loaded && onScreen('screentime') ? screentime.reload() : screentime.markStale(),
  // Heartbeats arriving -- every half minute from every machine while
  // anything is in use. A target of its own so it can be held to far less
  // than a label is: only while today is on screen, and at most once a
  // minute. See `screentime.spansArrived`.
  screentimeSpans: () => screentime.spansArrived(),
  // A file arrived, changed, moved or went missing -- the reconciler finds
  // them as the folder changes under a sync tool, and every app that keeps a
  // file raises one -- or an album did. Read again only while Files is the
  // app on screen, and one at a time: see `files.changed` and `files.reload`.
  //
  // A recording that has just been transcribed is a file change too -- the
  // transcript is a new file -- so the Feed's list of background work is read
  // again with it, once something has asked for it.
  // The Feed's work list too, which reads only once it has been loaded: a
  // transcript or an audio version landing is a file change.
  files: () => {
    void upnext.workChanged()
    return files.changed()
  },
  // A medication, a condition, a lab result, a meal or a workout changed on
  // another machine, or a source was edited. Read again only while Health is
  // the app on screen, and only once it has been opened since the vault was:
  // otherwise it is marked stale and reads in full when it is next opened,
  // and what is on screen is the only thing it ever loads.
  health: () => (health.loaded && onScreen('health') ? health.reload() : health.markStale()),
  // Samples arriving -- every hour from a phone, and a batch at a time from
  // an import. A target of its own so it can be held to far less than a
  // record is: at most once a minute. See `health.samplesArrived`.
  healthSamples: () => health.samplesArrived(),
} satisfies Record<string, () => Promise<unknown> | void>

/**
 * The name of something in [`RELOAD`].
 *
 * A union of the actual keys rather than `string`, which is what the table was
 * annotated as. `Record<string, ...>` made `keyof typeof RELOAD` mean `string`,
 * so a misrouted kind -- `'liberry'` for `'library'` -- typechecked, and the
 * only thing standing between that and an app that silently stopped refreshing
 * was a runtime assertion in `live.test.mjs`.
 */
export type ReloadTarget = keyof typeof RELOAD

// The briefs are routines too, and a run finishing is a brief arriving: the
// Brief page and Settings → Briefs read again whenever a routine or a run
// moves, once something has read them, whatever the assistant's own `apply`
// on the same target does with the batch. Answering `true` leaves the
// decision about the assistant's lists to that `apply` alone.
registerApply(
  'assistant',
  () => {
    if (briefs.loaded) void briefs.refresh()
    return true
  },
  'briefs',
)

/**
 * The kinds that can carry a purpose, and so change what the reports say.
 *
 * A purpose is a pointer on a project, a task, a block, an entry, a shelf item
 * and a tracker, and it *inherits* -- a block's own, else its task's, else that
 * task's project's. So filing one task under a goal changes every chart the
 * Overview draws, while the change event says only "task".
 *
 * Reloading the Overview for all of these unconditionally would mean redrawing
 * a year of balance on every keystroke's autosave. So it is done only when the
 * Overview is the app on screen: a chart nobody is looking at does not need to
 * be right yet, and it re-reads when it is opened.
 */
const PURPOSE_BEARING: ReadonlySet<ChangeKind> = new Set([
  'project',
  'task',
  'block',
  'entry',
  'note',
  'item',
  'tracker',
  'reading',
  'calendar',
])

/**
 * The kinds a goal's target meters are read off.
 *
 * A target counts a tracker's days, and a derived tracker's days are worked
 * out from blocks (through the task and project they inherit a purpose
 * from), from the library's log and from Health's samples. So an hour logged
 * on another machine, a book finished there, or a day's steps arriving from
 * a phone moves a meter while the change event says only "block", "log" or
 * "healthSample" -- the same shape of problem `PURPOSE_BEARING` solves for
 * the Overview, and solved the same way: only while the todo app, which is
 * where the meters are drawn, is on screen. It re-reads them when opened.
 */
const TARGET_FEEDING: ReadonlySet<ChangeKind> = new Set([
  'block',
  'log',
  'item',
  'task',
  'project',
  'tracker',
  'reading',
  'goal',
  'area',
  'healthSample',
])

/**
 * Which app each kind of record `ShowInMain.open` can name opens in.
 *
 * The shell's words for them (`windows::open_item`): a card on the panel asks
 * the main window to open the thread it is about, the note its meeting
 * produced, the task or event it made. A kind not here has no way to be
 * opened by id in this window, and the section the shell sent with it is
 * shown instead.
 */
export const OPENS_IN: Readonly<Record<string, Section>> = {
  thread: 'mail',
  note: 'notes',
  task: 'todo',
  event: 'calendar',
}

/** The app `open` is opened in, or `null` for a kind this window cannot open
 *  by id. Own properties only, so `constructor` is not a kind. */
export function appForItem(open: OpenItem | null | undefined): Section | null {
  if (!open || !Object.hasOwn(OPENS_IN, open.kind)) return null
  return OPENS_IN[open.kind] ?? null
}

/** Which of those a change of each kind asks for. */
export const RELOADS: Record<ChangeKind, ReloadTarget | null> = {
  journal: 'journals',
  entry: 'entries',
  note: 'notes',
  template: 'templates',
  project: 'todo',
  task: 'todo',
  block: 'calendar',
  calendar: 'calendar',
  event: 'calendar',
  shelf: 'shelves',
  item: 'library',
  log: 'library',
  tracker: 'tracking',
  reading: 'tracking',
  // Areas and goals. They used to reload the Overview, because that is where
  // they were edited; they are read in three places now and stored in one, so
  // the reload is of the store rather than of an app. The Overview redraws
  // anyway when it is the app on screen -- see `targetsFor`.
  area: 'purpose',
  goal: 'purpose',
  // The assistant's standing work, and the log of what it did. The routines
  // pane redraws for either, and the count on the app bar is read from the
  // same reload.
  routine: 'assistant',
  routineRun: 'assistant',
  // A background job, which writes often while it runs -- every tool call is
  // a progress line. The 250 ms window above gathers those into one read of
  // the job they name (see `jobs.svelte.ts`'s `apply`).
  job: 'jobs',
  // Work the assistant prepared. Drawn in several apps from one list, which
  // also holds half the count on the app bar -- see `proposals.svelte.ts`.
  proposal: 'proposals',
  // The assistant's own thread. The panel reads it when it is opened, and a
  // reply arriving on another machine is not something to interrupt this one
  // with -- so nothing reloads, and the event exists for a future history
  // list. Its memory is the same argument: the list is read when it is shown.
  conversation: null,
  memory: null,
  // Skills too: Settings → Skills reads its list each time it is shown, and
  // the assistant reads them fresh on every turn.
  skill: null,
  settings: 'status',
  // A mailbox provider signed in to. Settings → Accounts registered an
  // `apply` for this target (see `accounts.svelte.ts`), so most changes are
  // patched in place; a batch wider than one record falls back to this.
  account: 'accounts',
  // A conversation of mail messages, and a message being written. The Mail
  // app's own store registered an `apply` for this target (`mail.svelte.ts`
  // -- see its `#applyChanges`), so a single thread or draft is patched in
  // place; a batch wider than one record falls back to this, which reloads
  // the currently visible page rather than the whole mailbox.
  thread: 'mail',
  draft: 'mail',
  // A call recorded, and the voice a transcript's speaker was matched or
  // named to. Both route to the one `meetings` target -- see `RELOAD`'s own
  // comment on why the two kinds are not told apart.
  recording: 'meetings',
  voiceprint: 'meetings',
  // What was said in a call. The note a call becomes exists as a draft from
  // the moment it ends, and its transcript is written, then its speakers
  // worked out, while it may be open: the same `meetings` target, whose
  // refresh tells the open `TranscriptPanel` to read it again.
  transcript: 'meetings',
  // What somebody follows, and what it brought. A background refresh raises
  // one `story` change with no id for the whole pass -- "reload the list", as
  // a mail sync does -- so there is nothing to patch in place and no applier.
  newsSource: 'feed',
  story: 'feed',
  // What Up Next holds, and what was read and heard. Their own store's, not
  // the Feed's list: the queue is drawn in the player and on its own page.
  queue: 'upnext',
  consumption: 'consumption',
  // Linked banks, their accounts, what happened in them, and the categories
  // and rules that sort it. A background sync raises `txn` changes for a
  // whole delta at once, so there is nothing to patch row by row and no
  // applier: the store reloads what is on screen.
  bankLink: 'money',
  bankAccount: 'money',
  txn: 'money',
  moneyCategory: 'money',
  moneyRule: 'money',
  // What somebody calls an app or a site, and the categories they sort
  // them into; and the spans heartbeats become, which never carry an id --
  // a batch from a collector is one change for the lot.
  screenApp: 'screentime',
  screenCategory: 'screentime',
  screenSpan: 'screentimeSpans',
  // What is in the files folder and the catalog, and the albums made of it.
  // One target for both: Files shows the two together.
  file: 'files',
  album: 'files',
  // What Health keeps: where its data comes from, the workouts and doses,
  // medicines, conditions, results, documents, allergies, foods and meals
  // somebody records. All one reload of what is on screen. Samples are the
  // exception, as screen time's spans are: they arrive by the thousand and
  // never with an id, so they are a target of their own.
  healthSource: 'health',
  workout: 'health',
  medication: 'health',
  dose: 'health',
  condition: 'health',
  labResult: 'health',
  healthDocument: 'health',
  allergy: 'health',
  food: 'health',
  meal: 'health',
  healthSample: 'healthSamples',
}

/**
 * Whether a change is the service's `mailbox`: a folder or label a sync pass
 * met for the first time. No command declares it, so it is not in the
 * generated `ChangeKind`, and it is compared as the plain string it arrives as.
 */
function isMailbox(kind: ChangeKind): boolean {
  const name: string = kind
  return name === 'mailbox'
}

/**
 * What one batch of changes should reload, once each.
 *
 * Its own function so the collapsing is testable without the stores: it is a
 * rule about a table, and the rule is the part that was wrong.
 */
export function targetsFor(
  kinds: ChangeKind[],
  overviewShowing = false,
  todoShowing = false,
): ReloadTarget[] {
  // `!= null`, not `!== null`: a kind the service raises that no command
  // declares -- a supervised task's `backgroundTask` status, a sync pass's
  // `mailbox` -- is not in the generated `ChangeKind`, so `RELOADS` has no
  // entry for it and answers `undefined`. Kept, that `undefined` became a
  // target whose reload was called as a function, which threw and took
  // every reload after it in the same batch down with it.
  const targets = new Set(
    kinds.map((kind) => RELOADS[kind]).filter((t): t is ReloadTarget => t != null),
  )
  // The one of those this window acts on: a sync pass that met a folder or
  // label for the first time. A list reload does not read the mailboxes
  // again, and the pass may never be seen mid-flight for `MailNav`'s falling
  // edge to catch it.
  if (kinds.some(isMailbox)) targets.add('mailboxes')
  // See `PURPOSE_BEARING`: these do not name the Overview and still change what
  // it says, but only matter while somebody is looking at it.
  if (overviewShowing && kinds.some((kind) => PURPOSE_BEARING.has(kind))) {
    targets.add('overview')
  }
  // An area or a goal moving changes every chart the Overview draws, and the
  // store reload above only refreshes the lists behind them.
  if (overviewShowing && kinds.some((kind) => kind === 'area' || kind === 'goal')) {
    targets.add('overview')
  }
  if (todoShowing && kinds.some((kind) => TARGET_FEEDING.has(kind))) {
    targets.add('goalTargets')
  }
  return [...targets]
}

/**
 * What one batch of changes should do to each target `targetsFor` names.
 *
 * A target whose store registered an `apply` (`live-apply.ts`) gets the
 * changes routed to it -- by kind, through `RELOADS` -- and is asked to
 * patch its own list; if it does (or there is nothing registered), the
 * target lands in `refreshed` for `RELOAD` to run the old way instead.
 *
 * Its own function for the reason `targetsFor` is: a rule about a batch,
 * testable without a store, a timer, or the backend they both talk to.
 */
export function planBatch(
  changes: ChangeWithIds[],
  overviewShowing = false,
  todoShowing = false,
): { applied: ReloadTarget[]; refreshed: ReloadTarget[] } {
  const kinds = changes.map((c) => c.kind)
  const applied: ReloadTarget[] = []
  const refreshed: ReloadTarget[] = []
  for (const target of targetsFor(kinds, overviewShowing, todoShowing)) {
    const relevant = changes.filter((c) => RELOADS[c.kind] === target)
    const apply = applierFor(target)
    if (apply && apply(relevant)) applied.push(target)
    else refreshed.push(target)
  }
  return { applied, refreshed }
}

class Live {
  /**
   * The changes gathered since the timer armed, in arrival order.
   *
   * Whole `ChangeEvent`s now, not just their kinds: `planBatch` has to hand a
   * store's `apply` the ids it asked for, not merely tell it that something
   * of its kind happened.
   */
  #pendingChanges: ChangeWithIds[] = []
  #timer: ReturnType<typeof setTimeout> | null = null
  #started = false
  #downTimer: ReturnType<typeof setTimeout> | null = null
  /** The stream has dropped since it was last up, so changes may have been missed. */
  #missed = false

  /**
   * Whether the computer holding the vault is being heard from. False only
   * once its event stream has been down for `DOWN_MS`; `Notices.svelte`
   * reads it. The stream's own loop keeps retrying, so this clears by itself
   * on the first reconnect.
   */
  reachable = $state(true)

  /** What the stream loop sees, however it is carried. Debounced down, not
   *  up: coming back is good news to say at once. */
  setStream(up: boolean) {
    if (up) {
      if (this.#downTimer !== null) clearTimeout(this.#downTimer)
      this.#downTimer = null
      this.reachable = true
      // Whatever changed while it was down was never announced, so nothing
      // held may draw what it has: each reads in full on its way back, and
      // the one on screen now.
      if (this.#missed) {
        this.#missed = false
        for (const held of HELD) held.markStale()
        this.#catchUp()
      }
      return
    }
    this.#missed = true
    if (this.#downTimer === null && this.reachable) {
      this.#downTimer = setTimeout(() => {
        this.#downTimer = null
        this.reachable = false
      }, DOWN_MS)
    }
  }

  /**
   * A write this window made itself. The app it belongs to has drawn it
   * already if that app is on screen; one that is not -- a reading logged
   * from the Overview, a story read from the Brief page -- reads in full on
   * its way back rather than drawing what it had, as it did when every
   * visit read everything.
   *
   * The briefs too, which a visit no longer reads on a desktop window: a run
   * removed from "What it did", or a brief switched on by this window's
   * assistant, is a routine change this window would otherwise never hear.
   * And a label this window's assistant made, which no list here has drawn.
   */
  #ownWrite(kind: ChangeKind) {
    if (isMailbox(kind)) RELOAD.mailboxes()
    const target = RELOADS[kind]
    if (target == null) return
    if (target === 'assistant' && briefs.loaded) void briefs.refresh()
    for (const held of HELD) {
      if (held.targets.includes(target) && !onScreen(held.section)) held.markStale()
    }
  }

  /** Read again the held app on screen, if anything has marked it stale. */
  #catchUp() {
    if (app.screen !== 'main') return
    for (const held of HELD) {
      if (onScreen(held.section) && held.stale()) void held.start().catch(() => {})
    }
  }

  /**
   * Begin listening. Called once, from the root component.
   *
   * Idempotent, because hot reloading in development mounts the root more than
   * once and two subscriptions would mean two reloads per change.
   */
  start() {
    if (this.#started) return
    this.#started = true

    onChange((change: ChangeEvent) => this.#note(change))
    // A server never echoes a client's own writes back to it, so on a phone
    // or a web tab this is how the Overview learns a card it drew went stale
    // (on the desktop the change arrives on the stream too; marking twice is
    // harmless).
    onOwnWrite((kind) => overview.noteChange(kind))
    // Only a browser on web access has a stream of its own to watch; the
    // desktop shell's remote client would feed `setStream` from its own.
    onWebStream((up) => this.setStream(up))
    // A window hidden in the tray holds the reloads of the app it shows, as
    // an app off screen does (see `onScreen`); being shown again is when
    // that app catches up.
    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState === 'visible') this.#catchUp()
    })
    // The main window's alone, both of these: the shell raises the main
    // window for either, and a window of one app opening its palette as well,
    // behind it, would be a second palette nobody can see.
    if (!pinned) {
      // The OS-wide hotkey. The shell has already raised the window by the
      // time this arrives; all that is left is to open the palette -- and only
      // past the lock screen, because a key pressed while the vault is sealed
      // should not put a list of its contents on screen.
      onPalette(() => {
        if (app.screen === 'main') panels.openPalette()
      })
      // A conversation from the talk key has the speakers while it lasts:
      // what the player was playing waits, as it does for voice mode here.
      // And what this window shows is what "that article" may mean, so the
      // conversation is told it -- only while the window is showing at all
      // and past the lock screen.
      onTalk((active) => {
        if (active) {
          player.pauseForVoice()
          if (app.screen === 'main' && document.visibilityState === 'visible') voice.shareScreen()
        } else player.resumeAfterVoice()
      })
      // A window of one app handing over what is not its app -- see
      // `appwindow.ts`. Past the lock screen only, for the palette's reason,
      // but kept until then rather than dropped: this window going to its
      // lock screen while somebody works in the other is the ordinary case,
      // and after the password it should show what was asked for, not
      // whatever it was last left on.
      onShowInMain((asked) => {
        if (app.screen === 'main') this.#show(asked)
        else this.#asked = asked
      })
      $effect.root(() => {
        $effect(() => {
          if (app.screen !== 'main' || !this.#asked) return
          const asked = this.#asked
          this.#asked = null
          this.#show(asked)
        })
      })
    }
    onLockState((locked: boolean) => {
      // Not a list to reload: every screen has to leave. A vault that locked
      // itself on the machine holding it locks for everybody looking at it.
      if (locked) {
        void app.lockedElsewhere()
        return
      }
      // An *unlock* only matters if this window is on the lock screen and did
      // not do it itself. The local `unlock` path raises this event too, and
      // starting up again from here ran `app.start()` concurrently with the
      // `enterMain()` that unlock was already running -- two loads of the same
      // lists, racing, with the loser's answer drawn.
      if (app.screen === 'locked' && !app.unlocking) void app.start()
    })
  }

  /** What a window of one app asked the main window to show while it was on
   *  its lock screen -- the latest only, which is the one still wanted. */
  #asked: ShowInMain | null = null

  #show({ section, settings, part, open }: ShowInMain) {
    if (open && appForItem(open)) void this.#open(open, section)
    else if (section) app.setSection(section)
    else if (isSettingsTab(settings)) panels.openSettings(settings, part ?? null)
  }

  /**
   * Open one record in its app -- the thread a mail card on the panel was
   * about, the note a notes card was -- the way a link to it inside this
   * window would: switch to the app, then that app's own way of opening one.
   * Anything that goes wrong leaves the app on screen (or `section`, if it
   * could not be shown), not an error about a card that has already gone.
   */
  async #open({ kind, id }: OpenItem, section: Section | null) {
    try {
      switch (kind) {
        case 'thread':
          await mail.openFromElsewhere(id)
          return
        case 'note':
          if (await app.goTo('notes')) await notes.openNote(id)
          return
        case 'task':
          // Every open task, since the card cannot say which list it is in;
          // the detail pane opens beside it either way.
          if (!(await app.goTo('todo'))) return
          await todo.setScope({ kind: 'all' })
          await todo.open(id)
          return
        case 'event': {
          if (!(await app.goTo('calendar'))) return
          // On the day it starts, picked out in the rail: an event the
          // grid has not loaded is not one the rail can draw.
          const event = await api.event(id)
          calendar.goto(event.allDay ? event.localDate : isoDate(new Date(event.start)))
          calendar.selection = { kind: 'event', id }
          return
        }
      }
    } catch (e) {
      await quietly(e)
      if (section && app.section !== section) app.setSection(section)
    }
  }

  #note(change: ChangeEvent) {
    if (app.screen !== 'main') return
    // A window never reloads for its own write. It already drew the result,
    // and reloading would move the list under the cursor that made the change.
    //
    // `local` is the main window in this process, whichever vault it is
    // looking at; a window of one app has an origin of its own (see
    // `appwindow.ts`), so the two reload for each other's writes. A remote
    // server filters by device id before it sends, so what arrives here from
    // another machine is already somebody else's; this covers the case where
    // the vault is in this process and the sink is talking to itself.
    // Overview keeps what it drew and refreshes only what a change made
    // stale, so it hears every change, this window's own writes included
    // (a task ticked in Todo must still move the Overview's card).
    overview.noteChange(change.kind)
    if (change.origin === ownOrigin) {
      this.#ownWrite(change.kind)
      return
    }
    this.#pendingChanges.push(change)
    if (this.#timer !== null) return
    this.#timer = setTimeout(() => {
      this.#timer = null
      const changes = this.#pendingChanges
      this.#pendingChanges = []
      // Deduplicated by *what would run*, not by the kind: a task and a
      // project both reload the todo app, and doing it twice is a wasted round
      // trip on a connection that may be a phone's. `applied` targets need
      // nothing further -- their store already patched itself.
      const { refreshed } = planBatch(changes, app.section === 'overview', app.section === 'todo')
      for (const target of refreshed) {
        void Promise.resolve(RELOAD[target]()).catch(() => {})
      }
    }, COALESCE_MS)
  }
}

export const live = new Live()
