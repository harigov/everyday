// The single boundary between the interface and the Rust core.
//
// Everything the UI can do to a vault goes through here, which means the
// mock backend below is a complete substitute: `EVERYDAY_MOCK=1 npm run dev`
// runs the entire interface in a browser with no Rust, no vault and no
// native dependencies. That is what makes the design workable on its own.

import type {
  AddedItem,
  AgentEvent,
  AgentMessage,
  AgentSettings,
  BlockId,
  BlockKind,
  BlockQuery,
  BlockSubject,
  BalanceReport,
  Bootstrap,
  Conversation,
  ConversationId,
  ConversationSummary,
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
  Goal,
  GoalActivity,
  GoalId,
  GoalQuery,
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
  Memory,
  MemoryId,
  Project,
  ProjectId,
  ProviderInfo,
  Reading,
  ReadingId,
  ReadingQuery,
  Role,
  RoleId,
  RoleInfo,
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
  Tracker,
  TrackerDay,
  TrackerId,
  TrackerKind,
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

/**
 * `args` is a bag of named arguments for every command but one. `put_blob`
 * passes a bare `Uint8Array`, which Tauri sends as a raw body rather than as
 * JSON -- see `putBlob` for why that distinction is worth the wider type.
 */
type InvokeArgs = Record<string, unknown> | Uint8Array

type Invoke = <T>(cmd: string, args?: InvokeArgs) => Promise<T>

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

/**
 * Say something to the assistant, streaming what it says back.
 *
 * Separate from the `api` object below because it is the one call that is not
 * request/response: a turn takes seconds and runs tools while it does, so the
 * reply arrives on a channel and this resolves only when the turn is over.
 * Rejects with the same `VaultError` every other command does.
 *
 * Replaced with the mock implementation in dev-without-Tauri, so the panel is
 * as usable in a browser as the rest of the interface.
 */
export let sendMessage: (
  conversationId: ConversationId,
  prompt: string,
  context: string | null,
  onEvent: (event: AgentEvent) => void,
) => Promise<void> = async () => {
  throw new VaultError('unavailable', 'The assistant could not reach its backend.')
}

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
  sendMessage = async (conversationId, prompt, context, onEvent) => {
    const channel = new mod.Channel<AgentEvent>()
    channel.onmessage = onEvent
    await invoke<void>('send_message', { conversationId, prompt, context, channel })
  }
  invoke = async <T>(cmd: string, args?: InvokeArgs): Promise<T> => {
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

  /**
   * `settings` is whatever the chosen backend asked for in its spec — a
   * connection URL, a schema name — and is omitted for a backend that needs
   * only a folder. It is sealed under the vault password on the way in, so
   * a database credential does not end up readable in the vault header.
   */
  createVault: (opts: {
    path: string
    name: string
    backend: string
    settings?: Record<string, string>
    password: string | null
  }) => invoke<VaultStatus>('create_vault', opts),

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

  /**
   * Import a file the user dropped or picked; returns its content address.
   *
   * The buffer is the whole payload rather than a field inside one, which
   * looks like a slip and is not: Tauri sends a top-level `ArrayBuffer` as
   * raw bytes and anything else as JSON. Wrapped in an object, a 100 MB
   * video became a hundred million JSON numbers -- hundreds of megabytes of
   * text to build, post and parse -- and froze the window while it did.
   * `put_blob` reads the request body to match.
   */
  putBlob: (bytes: Uint8Array) => invoke<string>('put_blob', bytes),

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
  // Available only when `status.capabilities.tasks` is true; a
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

  // ── Roles and goals ────────────────────────────────────────────────
  //
  // Available only when `status.capabilities.goals` is true. The two records
  // are small; the interesting call is `balance`, which is the whole reason
  // the purpose pointer exists.

  roles: () => invoke<RoleInfo[]>('list_roles'),

  /** Mints an unsaved role; fill it in and pass it to `saveRole`. */
  newRole: (name: string) => invoke<Role>('new_role', { name }),
  saveRole: (role: Role) => invoke<void>('save_role', { role }),

  /**
   * Delete a role. Refused, with a message naming the count, while goals
   * still point at it — unlike a project, which takes its tasks with it.
   */
  deleteRole: (id: RoleId) => invoke<void>('delete_role', { id }),

  /**
   * Offer a starting set of roles, and answer 0 if there are any already.
   *
   * Never called on unlock, unlike the library's shelves: a list of what a
   * life is made of is a claim, and writing one unasked would be this
   * application telling somebody who they are.
   */
  seedRoles: () => invoke<number>('seed_roles'),

  goals: (query: GoalQuery = {}) => invoke<Goal[]>('list_goals', { query }),
  goal: (id: GoalId) => invoke<Goal>('get_goal', { id }),
  newGoal: (roleId: RoleId, title: string) => invoke<Goal>('new_goal', { roleId, title }),
  saveGoal: (goal: Goal) => invoke<void>('save_goal', { goal }),
  saveGoals: (goals: Goal[]) => invoke<void>('save_goals', { goals }),
  deleteGoal: (id: GoalId) => invoke<void>('delete_goal', { id }),

  /**
   * Minutes per purpose over a window, and the meetings somebody else
   * booked, in one call — the Overview draws them together, and two round
   * trips would let one arrive without the other.
   */
  balance: (from: string, to: string) => invoke<BalanceReport>('time_by_purpose', { from, to }),

  goalActivity: (id: GoalId) => invoke<GoalActivity>('goal_activity', { id }),

  // ── The tracking domain ────────────────────────────────────────────
  //
  // Available only when `status.capabilities.trackers` is true. Note the
  // split: a tracker's *definition* is a field on its journal and is saved
  // with `saveJournal`, so there is no `saveTracker` here. What is here is
  // minting one and everything to do with the readings it produces.

  /** Mints an unsaved tracker; fill it in and save the journal holding it. */
  newTracker: (name: string, kind: TrackerKind) => invoke<Tracker>('new_tracker', { name, kind }),

  readings: (query: ReadingQuery) => invoke<Reading[]>('list_readings', { query }),

  /** One row per tracker per day: the aggregate a chart is built from. */
  trackerDays: (query: ReadingQuery) => invoke<TrackerDay[]>('tracker_days', { query }),

  /**
   * Record one value, and let the backend decide what "when" means.
   *
   * Pass `at` to state the time outright. Otherwise a reading on today's
   * date takes the current minute, and one on a past date takes no time at
   * all — writing up Tuesday on Thursday says nothing about 23:04, and a
   * defaulted timestamp would put a mark on the calendar at an hour nothing
   * happened.
   */
  logReading: (opts: {
    journalId: JournalId
    trackerId: TrackerId
    value: number
    date: string
    at?: string | null
    entryId?: EntryId | null
  }) => invoke<Reading>('log_reading', opts),

  /** Update a reading that exists: a corrected dose, a note, a time. */
  saveReading: (reading: Reading) => invoke<void>('save_reading', { reading }),
  deleteReading: (id: ReadingId) => invoke<void>('delete_reading', { id }),

  /**
   * Remove a tracker from its journal *and* every reading it ever made,
   * returning how many went. Archiving — a flag on the definition, saved
   * with the journal — is the non-destructive half of this pair.
   */
  deleteTracker: (journalId: JournalId, trackerId: TrackerId) =>
    invoke<number>('delete_tracker', { journalId, trackerId }),

  // ── The assistant ──────────────────────────────────────────────────
  //
  // Available only when `status.capabilities.agent` is true. Small, because
  // almost everything the assistant can do it does through its *tools*,
  // which live in the Rust core and never cross this boundary. What is here
  // is configuring it, reading its threads back, and the two halves of one
  // exchange: `sendMessage` (exported separately, because it streams) and
  // the confirmation that answers it.

  agentSettings: () => invoke<AgentSettings>('agent_settings'),

  /** Returns what was actually stored: `hasKey` is derived, not echoed. */
  saveAgentSettings: (settings: AgentSettings) =>
    invoke<AgentSettings>('save_agent_settings', { settings }),

  /** Store the API key. There is deliberately no call that reads one back. */
  setAgentKey: (key: string) => invoke<void>('set_agent_key', { key }),
  clearAgentKey: () => invoke<void>('clear_agent_key'),

  conversations: (limit?: number) => invoke<ConversationSummary[]>('list_conversations', { limit }),

  /** Mints an unsaved thread; the first message is what saves it. */
  newConversation: () => invoke<Conversation>('new_conversation'),

  conversationMessages: (id: ConversationId) =>
    invoke<AgentMessage[]>('conversation_messages', { id }),

  deleteConversation: (id: ConversationId) => invoke<void>('delete_conversation', { id }),

  /**
   * Answer a confirmation the assistant is waiting on. False means nothing
   * was waiting any more -- a turn cancelled between the question and the
   * click -- which the panel treats as a dismissal rather than an error.
   */
  confirmToolCall: (callId: string, approved: boolean) =>
    invoke<boolean>('confirm_tool_call', { callId, approved }),

  memories: () => invoke<Memory[]>('list_memories'),

  /** Saving by hand also pins: a fact somebody typed is not one the
   *  assistant's own housekeeping may drop. Returns what it evicted. */
  saveMemory: (memory: Memory) => invoke<Memory[]>('save_memory', { memory }),
  deleteMemory: (id: MemoryId) => invoke<void>('delete_memory', { id }),
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
  const { mockMediaUrl: f, mockSendMessage } = await import('./mock')
  mockMediaUrl = f
  sendMessage = mockSendMessage
}
