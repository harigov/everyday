// The single boundary between the interface and the Rust core.
//
// Everything the UI can do to a vault goes through here, which means the
// mock backend below is a complete substitute: `EVERYDAY_MOCK=1 npm run dev`
// runs the entire interface in a browser with no Rust, no vault and no
// native dependencies. That is what makes the design workable on its own.

import type {
  AddedItem,
  BlockId,
  BlockKind,
  BlockQuery,
  BlockSubject,
  Bootstrap,
  Calendar,
  CalendarEvent,
  CalendarId,
  CalendarInfo,
  Entry,
  EntryId,
  EntryQuery,
  EntrySummary,
  EventId,
  EventQuery,
  Item,
  ItemId,
  ItemQuery,
  ItemStatus,
  Journal,
  JournalId,
  Kind,
  KindId,
  KindInfo,
  LibraryStats,
  LogEntry,
  LogEvent,
  LogId,
  LogQuery,
  Project,
  ProjectId,
  ProviderInfo,
  SearchHit,
  SearchRequest,
  SearchResult,
  SourceInfo,
  SyncReport,
  TagCount,
  Task,
  TaskId,
  TaskQuery,
  TaskStats,
  TaskStatus,
  TimeBlock,
  TrayMenuItem,
  VaultStatus,
} from './types'
import { VaultError } from './types'
import type { ShellNotification } from './types'

// Decided at BUILD time, not run time.
//
// `import.meta.env.DEV` is substituted with a literal by Vite, so a
// production bundle evaluates this to `false` and the mock module below is
// statically eliminated -- it is not merely unused, it is not shipped.
//
// The runtime check is deliberately *inside* the dev guard. A release build
// that cannot reach Tauri must fail loudly, not quietly serve a fake journal
// with sample entries in it: for a journal app, that failure mode is
// indistinguishable from having lost everything.
const MOCK = import.meta.env.DEV && !('__TAURI_INTERNALS__' in window)

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>

let invoke: Invoke = async () => {
  throw new VaultError(
    'unavailable',
    'Every Day could not reach its storage backend. Your journal has not been ' +
      'touched; this is a problem with the application, not with your data.',
  )
}

/**
 * Register the handler for the shell's save-before-close request.
 *
 * The window cancels its own close, asks here, and waits for
 * `api.readyToClose`. Outside Tauri there is no such handshake, so this is a
 * no-op and the mock interface closes the way a browser tab does.
 */
export let onSaveAndClose: (handler: () => void | Promise<void>) => void = () => {}

/**
 * Register the handler for notifications raised by the Rust shell.
 *
 * The shell does work the interface never asked for -- refreshing subscribed
 * calendars on a timer -- and this is how it says something about it. The
 * payload is a `NotifySpec` minus the parts only a component could supply,
 * so `notify.svelte.ts` can post it unchanged.
 *
 * Outside Tauri nothing ever emits, so this is a no-op.
 */
export let onShellNotification: (handler: (spec: ShellNotification) => void) => void = () => {}

/**
 * Register the handler for a chosen tray menu item.
 *
 * The payload is the id the interface gave the item in `set_tray_menu`; the
 * routing back to a function lives in `lib/tray.svelte.ts`. A no-op outside
 * Tauri, where there is no tray to choose anything from.
 */
export let onTrayAction: (handler: (id: string) => void) => void = () => {}

if (!MOCK) {
  const { listen } = await import('@tauri-apps/api/event')
  onSaveAndClose = (handler) => {
    void listen('everyday://save-and-close', () => void handler())
  }
  onShellNotification = (handler) => {
    void listen<ShellNotification>('everyday://notify', (event) => handler(event.payload))
  }
  onTrayAction = (handler) => {
    void listen<string>('everyday://tray-action', (e) => handler(e.payload))
  }

  const mod = await import('@tauri-apps/api/core')
  invoke = async <T>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
    try {
      return await mod.invoke<T>(cmd, args)
    } catch (raw) {
      // Commands reject with `{ code, message }`; anything else is a bug in
      // the bridge and should surface as-is rather than be swallowed.
      if (raw && typeof raw === 'object' && 'code' in raw && 'message' in raw) {
        throw new VaultError(String(raw.code), String(raw.message))
      }
      throw new VaultError('unknown', String(raw))
    }
  }
} else {
  const { mockInvoke } = await import('./mock')
  invoke = mockInvoke
}

export const isMock = MOCK

export const api = {
  bootstrap: () => invoke<Bootstrap>('bootstrap'),

  createVault: (opts: { path: string; name: string; backend: string; password: string | null }) =>
    invoke<VaultStatus>('create_vault', opts),

  openVault: (path: string) => invoke<VaultStatus>('open_vault', { path }),
  unlock: (password: string) => invoke<VaultStatus>('unlock', { password }),
  lock: () => invoke<VaultStatus>('lock'),
  status: () => invoke<VaultStatus>('status'),
  changePassword: (current: string, next: string) =>
    invoke<void>('change_password', { current, next }),
  setAutoLock: (seconds: number) => invoke<void>('set_auto_lock', { seconds }),
  /** Defers the idle auto-lock; called on real user interaction. */
  touch: () => invoke<void>('touch'),
  /** Returns true if the vault locked itself. Polled on a timer. */
  pollAutoLock: () => invoke<boolean>('poll_auto_lock'),

  journals: () => invoke<Journal[]>('list_journals'),
  newJournal: (name: string) => invoke<Journal>('new_journal', { name }),
  saveJournal: (journal: Journal) => invoke<void>('save_journal', { journal }),
  deleteJournal: (id: JournalId) => invoke<void>('delete_journal', { id }),

  entries: (query: EntryQuery) => invoke<EntrySummary[]>('list_entries', { query }),
  entry: (id: EntryId) => invoke<Entry>('get_entry', { id }),
  newEntry: (journalId: JournalId) => invoke<Entry>('new_entry', { journalId }),
  /**
   * Save an entry, refusing to overwrite a change made since it was loaded.
   *
   * `expect` is the `updatedAt` this window last read for the entry, or
   * `null` for one it has just created. A mismatch rejects with code
   * `conflict` and writes nothing.
   */
  saveEntry: (entry: Entry, expect: string | null) => invoke<void>('save_entry', { entry, expect }),

  /** Save regardless of what is stored. The "keep mine" on a conflict. */
  saveEntryForce: (entry: Entry) => invoke<void>('save_entry_force', { entry }),
  deleteEntry: (id: EntryId) => invoke<void>('delete_entry', { id }),

  search: (query: string, journalId: JournalId | null, limit: number) =>
    invoke<SearchHit[]>('search', { query, journalId, limit }),

  /** Import a file the user dropped or picked; returns its content address. */
  putBlob: (bytes: Uint8Array) => invoke<string>('put_blob', { bytes: Array.from(bytes) }),

  /** Tells the shell that pending writes have landed and it may close. */
  readyToClose: () => invoke<void>('ready_to_close'),

  /**
   * Put these items in the tray menu, raising the icon if it is not up.
   *
   * False means this desktop has nowhere to put one -- a Linux session with
   * no StatusNotifier host -- rather than that something went wrong.
   */
  setTrayMenu: (items: TrayMenuItem[]) => invoke<boolean>('set_tray_menu', { items }),
  /** Take the tray icon down. */
  hideTray: () => invoke<void>('hide_tray'),

  /** All tags in use, most frequent first. */
  tags: () => invoke<string[]>('list_tags'),

  // ── The task domain ────────────────────────────────────────────────
  //
  // Available only when `status.capabilities.tasks` is true; a Markdown
  // vault stores journals and nothing else, and the interface hides the
  // todo app rather than letting these fail at click time.

  projects: () => invoke<Project[]>('list_projects'),
  newProject: (name: string) => invoke<Project>('new_project', { name }),
  saveProject: (project: Project) => invoke<void>('save_project', { project }),
  /** Deletes the project, its tasks and their time blocks. */
  deleteProject: (id: ProjectId) => invoke<void>('delete_project', { id }),

  tasks: (query: TaskQuery) => invoke<Task[]>('list_tasks', { query }),
  task: (id: TaskId) => invoke<Task>('get_task', { id }),
  /** Mints an unsaved task; fill it in and pass it to `saveTask`. */
  newTask: (opts: {
    projectId: ProjectId | null
    parentId: TaskId | null
    status: TaskStatus | null
  }) => invoke<Task>('new_task', opts),
  saveTask: (task: Task) => invoke<void>('save_task', { task }),
  /** One write for many tasks: what a board reorder is. */
  saveTasks: (tasks: Task[]) => invoke<void>('save_tasks', { tasks }),
  /** Deletes the task, its subtasks and their time blocks. */
  deleteTask: (id: TaskId) => invoke<void>('delete_task', { id }),

  blocks: (query: BlockQuery) => invoke<TimeBlock[]>('list_blocks', { query }),
  /** Mints an unsaved block, with the machine's own time zone resolved. */
  newBlock: (opts: {
    subject: BlockSubject
    start: string
    minutes: number
    kind: BlockKind | null
  }) => invoke<TimeBlock>('new_block', opts),
  saveBlock: (block: TimeBlock) => invoke<void>('save_block', { block }),
  deleteBlock: (id: BlockId) => invoke<void>('delete_block', { id }),

  /** Every tag in the task domain with its usage count, most used first. */
  taskTags: () => invoke<TagCount[]>('task_tags'),
  taskStats: () => invoke<TaskStats>('task_stats'),

  // ── The calendar domain ────────────────────────────────────────────
  //
  // Subscribed calendars and their events. Time *you* schedule is a time
  // block and goes through the task commands above -- there is deliberately
  // no second way to store an appointment.

  calendars: () => invoke<CalendarInfo[]>('list_calendars'),
  saveCalendar: (calendar: Calendar) => invoke<void>('save_calendar', { calendar }),
  /** Unsubscribe: the calendar and every event that came from it. */
  deleteCalendar: (id: CalendarId) => invoke<void>('delete_calendar', { id }),

  /**
   * Subscribe to a feed and fetch it once.
   *
   * One call rather than save-then-fetch, because the two are not
   * independent: if the address turns out not to be a calendar, the honest
   * outcome is that nothing was added.
   */
  subscribeCalendar: (opts: { name: string; url: string; color: string }) =>
    invoke<CalendarInfo>('subscribe_calendar', opts),

  /** Add a calendar from a `.ics` file the browser read for us. */
  importCalendar: (opts: { name: string; label: string; color: string; ics: string }) =>
    invoke<CalendarInfo>('import_calendar', opts),

  /** Refetch one feed and replace its events with what comes back. */
  syncCalendar: (id: CalendarId) => invoke<SyncReport>('sync_calendar', { id }),
  /** Refetch every feed whose interval has elapsed. `force` ignores it. */
  syncDueCalendars: (force: boolean) => invoke<SyncReport[]>('sync_due_calendars', { force }),

  events: (query: EventQuery) => invoke<CalendarEvent[]>('list_events', { query }),
  event: (id: EventId) => invoke<CalendarEvent>('get_event', { id }),

  /** The providers the add sheet offers, with where to find each address. */
  calendarProviders: () => invoke<ProviderInfo[]>('calendar_providers'),

  // ── The library domain ─────────────────────────────────────────────
  //
  // Shelves, the things on them, and the log of what you did with them.
  // Available only when `status.capabilities.library` is true.

  /**
   * Every shelf, with its counts.
   *
   * Also what seeds the built-in shelves into an empty library, which is why
   * the library store calls this before anything else — see `list_kinds` in
   * the Rust shell for why the seeding hangs off a read.
   */
  kinds: () => invoke<KindInfo[]>('list_kinds'),
  /** Mints an unsaved shelf, with a slug derived from the name. */
  newKind: (name: string, singular: string) => invoke<Kind>('new_kind', { name, singular }),
  saveKind: (kind: Kind) => invoke<void>('save_kind', { kind }),
  /** Deletes the shelf, everything on it, and those items' log rows. */
  deleteKind: (id: KindId) => invoke<void>('delete_kind', { id }),

  items: (query: ItemQuery) => invoke<Item[]>('list_items', { query }),
  item: (id: ItemId) => invoke<Item>('get_item', { id }),

  /**
   * Add something to a shelf, and optionally go and find out what it is.
   *
   * One call rather than create-then-enrich: what the interface wants back is
   * the finished card. The lookup is best-effort — a network that is off
   * never stops something being added — and `lookedUp` says whether anything
   * was found, so the interface can offer to search again rather than
   * silently implying it tried.
   */
  addItem: (kindId: KindId, title: string, lookup: boolean) =>
    invoke<AddedItem>('add_item', { kindId, title, lookup }),
  saveItem: (item: Item) => invoke<void>('save_item', { item }),
  /** One write for many items: what a re-ordered shelf is. */
  saveItems: (items: Item[]) => invoke<void>('save_items', { items }),
  /** Deletes the item and its whole log. */
  deleteItem: (id: ItemId) => invoke<void>('delete_item', { id }),

  /**
   * Move an item to a status, dating it and logging it in one act.
   *
   * The dates and the log row are coupled in the backend on purpose: marking
   * a book read is the moment "finished on" is known *and* the moment the
   * log gains the row that makes "what did I read this year" answerable.
   */
  setItemStatus: (id: ItemId, status: ItemStatus, log: boolean) =>
    invoke<Item>('set_item_status', { id, status, log }),
  /** Record where you have got to. Starts the item if it was only wished for. */
  setItemProgress: (id: ItemId, position: number, total: number | null, log: boolean) =>
    invoke<Item>('set_item_progress', { id, position, total, log }),

  logs: (query: LogQuery) => invoke<LogEntry[]>('list_logs', { query }),
  /** Mints an unsaved log row dated today on the machine's own calendar. */
  newLog: (itemId: ItemId, event: LogEvent) => invoke<LogEntry>('new_log', { itemId, event }),
  saveLog: (log: LogEntry) => invoke<void>('save_log', { log }),
  deleteLog: (id: LogId) => invoke<void>('delete_log', { id }),

  libraryStats: () => invoke<LibraryStats>('library_stats'),

  // ── Web search ─────────────────────────────────────────────────────
  //
  // A facility rather than a feature of one app. `lib/websearch.ts` wraps
  // these with debouncing, cancellation and a cache; prefer that over
  // calling them directly.

  /** Search the web. The general entry point; anything may call it. */
  webSearch: (request: SearchRequest) => invoke<SearchResult[]>('web_search', { request }),
  /** The sources a search can be run against, for the picker. */
  searchSources: () => invoke<SourceInfo[]>('search_sources'),
  /** Look a title up using whatever source a shelf prefers. */
  lookupMetadata: (kindId: KindId, query: string, limit?: number) =>
    invoke<SearchResult[]>('lookup_metadata', { kindId, query, limit: limit ?? null }),
  /** Apply a chosen result to an item, downloading its cover on the way. */
  applyMetadata: (id: ItemId, result: SearchResult, overwrite: boolean) =>
    invoke<Item>('apply_metadata', { id, result, overwrite }),
  /**
   * Download a picture into the vault and return its blob id.
   *
   * Nothing in the interface ever loads a remote image directly: the content
   * security policy allows images from `'self'` and `everyday:` and nowhere
   * else, so a cover has to come home before it can be drawn.
   */
  fetchImage: (url: string) => invoke<string>('fetch_image', { url }),
}

/**
 * URL for an attachment.
 *
 * Media is served by a custom protocol handler in the Rust shell rather than
 * as a data: URL, so a 400 MB video streams and seeks instead of being
 * base64-encoded into the document.
 */
export function mediaUrl(blob: string): string {
  if (MOCK) return mockMediaUrl(blob)
  // Tauri maps custom schemes to an http origin on Windows and Android.
  const base =
    navigator.userAgent.includes('Windows') || navigator.userAgent.includes('Android')
      ? 'http://everyday.localhost'
      : 'everyday://localhost'
  return `${base}/${blob}`
}

let mockMediaUrl: (blob: string) => string = () => ''
if (MOCK) {
  const { mockMediaUrl: f } = await import('./mock')
  mockMediaUrl = f
}
