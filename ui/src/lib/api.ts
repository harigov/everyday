// The single boundary between the interface and the Rust core.
//
// Everything the UI can do to a vault goes through here, which means the
// mock backend below is a complete substitute: `EVERYDAY_MOCK=1 npm run dev`
// runs the entire interface in a browser with no Rust, no vault and no
// native dependencies. That is what makes the design workable on its own.

import type {
  AgentEvent,
  AgentSettings,
  BlockId,
  BlockKind,
  BlockQuery,
  BlockSubject,
  Bootstrap,
  Calendar,
  CalendarId,
  ChangeEvent,
  Connected,
  Connection,
  ConversationId,
  Entry,
  EntryId,
  EntryQuery,
  EventId,
  EventQuery,
  Goal,
  GoalId,
  GoalQuery,
  HotkeyStatus,
  ImportMode,
  Item,
  ItemId,
  ItemQuery,
  ItemStatus,
  Journal,
  JournalId,
  Kind,
  KindId,
  LogEntry,
  LogEvent,
  LogId,
  LogQuery,
  McpStatus,
  Memory,
  MemoryId,
  Note,
  NoteId,
  NoteQuery,
  PickedFile,
  Profile,
  Project,
  ProjectId,
  Reading,
  ReadingId,
  ReadingQuery,
  Role,
  RoleId,
  Routine,
  RoutineId,
  RoutineRunId,
  RunQuery,
  SearchKind,
  SearchRequest,
  SearchResult,
  ShareStatus,
  ShellNotification,
  Task,
  TaskId,
  TaskQuery,
  TaskStatus,
  TimeBlock,
  Tracker,
  TrackerId,
  TrackerKind,
  TrayMenuItem,
  VaultStatus,
} from './types'
import { VaultError } from './types'
import { COMMAND_NAMES, SERVICE_COMMANDS } from './generated/commands'
import type { Commands } from './generated/commands'

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

type Invoke = <T>(cmd: string, args?: InvokeArgs, requestId?: string) => Promise<T>

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
 * Register the handler for a write that landed somewhere else.
 *
 * On a local vault, "somewhere else" is another command in this window, and the
 * store that made it ignores its own by origin. Under server mode it is another
 * machine, and this is how a list learns to reload. See `lib/live.svelte.ts`.
 */
export let onChange: (handler: (change: ChangeEvent) => void) => void = () => {}

/**
 * Register the handler for the vault locking or unlocking.
 *
 * Replaces polling for a remote client, where `pollAutoLock` every few seconds
 * would be a round trip every few seconds. The local window still polls,
 * because there the poll is an integer comparison.
 */
export let onLockState: (handler: (locked: boolean) => void) => void = () => {}

/**
 * Register the handler for the OS-wide hotkey.
 *
 * The shell raises the window and emits this; the palette opens here. Outside
 * Tauri there is no desktop to claim a key from, so this is a no-op.
 */
export let onPalette: (handler: () => void) => void = () => {}

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
  onChange = (handler) => {
    void listen<ChangeEvent>('everyday://changed', (e) => handler(e.payload))
  }
  onLockState = (handler) => {
    void listen<boolean>('everyday://lock-state', (e) => handler(e.payload))
  }
  onPalette = (handler) => {
    void listen('everyday://palette', () => handler())
  }

  const mod = await import('@tauri-apps/api/core')
  sendMessage = async (conversationId, prompt, context, onEvent) => {
    const channel = new mod.Channel<AgentEvent>()
    channel.onmessage = onEvent
    // By name, not through `call`: a turn answers with a stream rather than a
    // value, so the shell has a command of its own for it. The generated
    // `SERVICE_COMMANDS` leaves the streaming ones out for the same reason.
    await mod.invoke<void>('send_message', { conversationId, prompt, context, channel })
  }
  invoke = async <T>(cmd: string, args?: InvokeArgs, requestId?: string): Promise<T> => {
    try {
      // Two kinds of call, and the split is not arbitrary. Almost everything
      // is a *service* command -- one entry in a table the Rust owns -- and
      // goes through the shell's single `call`, which either runs it here or
      // forwards it to whichever machine holds the vault. What is left is the
      // handful the shell alone can do: opening a vault, the tray, the raw
      // attachment body. Those are called by name.
      //
      // The set is generated from the Rust table, so the shell's half is
      // whatever is left over and cannot drift.
      if (!SERVICE_COMMANDS.has(cmd)) return await mod.invoke<T>(cmd, args)
      return await mod.invoke<T>('call', {
        name: cmd,
        args: args ?? {},
        requestId: requestId ?? null,
      })
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

/**
 * An id for one *logical* write, so a retry of it is answered from the first
 * attempt rather than applied twice.
 *
 * The caller mints it, and that is the whole of the contract: an id must be
 * reused across retries of the same write and must not be reused for a
 * different one. Nothing here can decide that, which is why this is not done
 * automatically.
 *
 * It was, briefly, and the mistake is worth recording. Minting one per
 * `invoke` call gave every retry a *fresh* id, so the layer never engaged --
 * protection that looked present and was not. Deriving one by hashing the
 * arguments looks better and is worse: two identical writes are sometimes two
 * writes on purpose -- a second dose recorded at the same minute, the same
 * book added to a shelf twice -- and deduplicating those loses data silently.
 *
 * So: no id unless a caller has reasoned about it. Today that is the entry
 * autosave, which is the one retry loop in the interface whose write is
 * conditional and therefore the one that this failure mode actually bites.
 *
 * A counter behind a random prefix, rather than `crypto.randomUUID`, which
 * needs a secure context the packaged webview does not always provide -- the
 * same reason ids for records are minted in Rust. Uniqueness only has to hold
 * within one connection, and a prefix plus a counter gives that.
 */
const REQUEST_PREFIX = Math.random().toString(36).slice(2, 10)
let requestCounter = 0
export function newRequestId(): string {
  requestCounter += 1
  return `${REQUEST_PREFIX}-${requestCounter}`
}

export const isMock = MOCK

/**
 * Call a command from the generated surface, typed against it.
 *
 * `gen-api.mjs` exists so that renaming an argument in Rust is a type error
 * here rather than a call that silently sends `undefined`, but every method
 * below used to write its own `invoke<T>('snake_name', {...})` by hand --
 * which meant the generated `Commands` interface had no reader at all, and a
 * rename that changed a field name or a return type compiled anyway, on both
 * sides, until somebody noticed the field come back as `undefined` at run
 * time. `K` is the camelCase key `Commands` and `COMMAND_NAMES` share, so a
 * method reads as `call('saveEntry', ...)` and the wire spelling is resolved
 * for it rather than repeated by hand at every call site.
 *
 * `requestId` is passed straight through to `invoke`, which is the same
 * "nobody attaches one unless they have reasoned about it" contract
 * `newRequestId` documents -- this does not consult `WRITE_COMMANDS` to
 * decide whether to mint or forward one, because today only `saveEntry` and
 * `saveNote` ever pass one at all, and every other write below is content to
 * retry as a fresh call if it is retried.
 */
function call<K extends keyof Commands>(
  name: K,
  args: Commands[K]['args'],
  requestId?: string,
): Promise<Commands[K]['result']> {
  return invoke<Commands[K]['result']>(COMMAND_NAMES[name], args, requestId)
}

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

  // ── Another computer's vault ───────────────────────────────────────
  //
  // A remote client is this same application, with its own window, its own
  // tray and its own keyboard; only the machine the commands run on differs.
  // Everything below `api` is unchanged when one of these is in effect --
  // which is the whole claim, and why there is nothing here but connecting.

  /** Every server this copy has paired with. */
  remotes: () => invoke<Connection[]>('list_remotes'),

  /**
   * Pair from a link somebody copied, and connect.
   *
   * One call rather than pair-then-connect: a pairing that succeeded and a
   * connection that then failed would leave a row in the picker whose token
   * there is no way to know is good.
   */
  connectRemote: (link: string) => invoke<Connected>('connect_remote', { link }),

  /** Reconnect to one already paired with. */
  reconnectRemote: (id: string) => invoke<Connected>('reconnect_remote', { id }),

  /** Look at a vault in this process again. The pairing survives. */
  disconnectRemote: () => invoke<void>('disconnect_remote'),

  /** Forget a pairing: the connection and its token. */
  forgetRemote: (id: string) => invoke<void>('forget_remote', { id }),

  // ── Sharing this vault ─────────────────────────────────────────────
  //
  // The other half: this window's vault, served to other machines. Every one
  // of these answers with the whole pane's state, because every one of them
  // changes more than the thing it was asked about -- starting the server
  // fixes the address, pairing adds a device, revoking removes one.

  shareStatus: () => invoke<ShareStatus>('share_status'),
  shareStart: (opts: { address?: string | null; port?: number | null }) =>
    invoke<ShareStatus>('share_start', opts),
  /**
   * Whether a connected computer may unlock this vault.
   *
   * Its own call rather than an argument to `shareStart`, because it is not a
   * reason to restart the server -- and restarting rebinds the port, which can
   * fail and leave sharing off because somebody moved a switch.
   */
  setRemoteUnlock: (allow: boolean) => invoke<ShareStatus>('set_remote_unlock', { allow }),
  shareStop: () => invoke<ShareStatus>('share_stop'),
  /** Offer to pair, for the next five minutes. */
  newPairingCode: () => invoke<ShareStatus>('new_pairing_code'),
  cancelPairing: () => invoke<ShareStatus>('cancel_pairing'),
  /** Take a computer's access away. It has to pair again to get it back. */
  revokeDevice: (id: string) => invoke<ShareStatus>('revoke_device', { id }),

  // ── Letting an MCP client use this vault ────────────────────────────
  //
  // Claude Code, Claude Desktop or anything else that speaks the Model
  // Context Protocol, reading this vault's tools over `POST /mcp`. Off by
  // default and loopback by default; see `McpPanel.svelte`. Revoking an
  // issued token is `revokeDevice` above -- it is a row in the same list a
  // paired computer is.

  mcpStatus: () => invoke<McpStatus>('mcp_status'),
  mcpStart: (opts: { address?: string | null; port?: number | null }) =>
    invoke<McpStatus>('mcp_start', opts),
  mcpStop: () => invoke<McpStatus>('mcp_stop'),
  /** Whether a destructive tool -- one that deletes something -- is offered
   * at all. Off by default; see `docs/plans/mcp.md`'s "Destructive tools
   * are absent, not refused". */
  mcpSetDestructive: (allow: boolean) => invoke<McpStatus>('mcp_set_destructive', { allow }),
  /**
   * Mint a token scoped to `scopes` and hand it back once. There is no
   * second call that shows it again -- copy it now, or issue a new one.
   *
   * Never empty: `mcp_issue_token` refuses that outright rather than
   * quietly widening it to everything, so `McpPanel` disables the button
   * before it can ask.
   */
  mcpIssueToken: (scopes: string[]) => invoke<string>('mcp_issue_token', { scopes }),

  // ── The OS-wide hotkey ─────────────────────────────────────────────

  /**
   * Whether the desktop granted the key that raises the palette.
   *
   * False is an ordinary answer, not an error: Wayland has no protocol for an
   * application to claim a global key without the desktop's portal, and not
   * every compositor implements one. The settings pane says so and offers the
   * tray instead.
   */
  hotkeyStatus: () => invoke<HotkeyStatus>('hotkey_status'),
  setHotkey: (on: boolean) => invoke<HotkeyStatus>('set_hotkey', { on }),
  unlock: (password: string) => call('unlock', { password }),
  /**
   * Check a password without opening or closing anything.
   *
   * What a window asks when its own screen was locked and the vault behind it
   * never was. Costs the same Argon2 derivation an unlock costs and counts
   * against the same lockout, deliberately.
   */
  verifyPassword: (password: string) => call('verifyPassword', { password }),
  lock: () => call('lock', {}),
  status: () => call('status', {}),
  changePassword: (current: string, next: string) => call('changePassword', { current, next }),
  /** How long before a client hides what it is showing. */
  setAutoLock: (seconds: number) => call('setAutoLock', { seconds }),
  /** How long before the machine holding the vault drops its key. 0 is never. */
  setForgetKey: (seconds: number) => call('setForgetKey', { seconds }),
  /**
   * Keep this vault's key in this machine's keychain, or take it out again.
   *
   * The password is asked for on the way *on*, and only then: this is the one
   * switch whose whole effect is that the password stops being needed, so
   * pressing it should cost the password once from somebody who knows it,
   * rather than being available to anybody who wandered past an unlocked
   * screen. Answers with what the setting now is.
   */
  setOpensItself: (on: boolean, password: string | null) =>
    invoke<boolean>('set_opens_itself', { on, password }),
  /** Defers the moment the key is dropped; called on real user interaction. */
  touch: () => call('touch', {}),
  /** Returns true if the vault has just dropped its key. Polled on a timer. */
  pollAutoLock: () => call('pollAutoLock', {}),

  journals: () => call('listJournals', {}),
  newJournal: (name: string) => call('newJournal', { name }),
  saveJournal: (journal: Journal) => call('saveJournal', { journal }),
  deleteJournal: (id: JournalId) => call('deleteJournal', { id }),

  entries: (query: EntryQuery) => call('listEntries', { query }),
  entry: (id: EntryId) => call('getEntry', { id }),
  newEntry: (journalId: JournalId) => call('newEntry', { journalId }),
  /**
   * Save an entry, refusing to overwrite a change made since it was loaded.
   *
   * `expect` is the `updatedAt` this window last read for the entry, or
   * `null` for one it has just created. A mismatch rejects with code
   * `conflict` and writes nothing.
   *
   * `requestId` identifies one logical write. Pass the *same* one when
   * retrying, so a save that landed and whose answer was lost is answered from
   * the record rather than refused as a conflict against itself. See
   * `newRequestId`.
   */
  saveEntry: (entry: Entry, expect: string | null, requestId?: string) =>
    call('saveEntry', { entry, expect }, requestId),

  /** Save regardless of what is stored. The "keep mine" on a conflict. */
  saveEntryForce: (entry: Entry) => call('saveEntryForce', { entry }),
  deleteEntry: (id: EntryId) => call('deleteEntry', { id }),

  /**
   * Search entries and notes.
   *
   * `kind` narrows to one of them; naming a journal narrows to entries
   * whatever `kind` says, because a note is in no journal. Both absent means
   * both kinds, which is what the palette wants.
   */
  search: (
    query: string,
    journalId: JournalId | null,
    limit: number,
    kind: SearchKind | null = null,
  ) => call('search', { query, journalId, kind, limit }),

  /**
   * Who the vault belongs to.
   *
   * Read into every prompt the assistant sends. Written only from Settings:
   * there is no tool for it, deliberately, because a fact that changes is a
   * memory and this is for the ones that do not.
   */
  profile: () => call('profile', {}),
  saveProfile: (profile: Profile) => call('saveProfile', { profile }),

  /** The assistant's standing work, with when each next runs. */
  routines: () => call('listRoutines', {}),
  /** A blank routine with an id. The core allocates it; see `newEntry`. */
  newRoutine: () => call('newRoutine', {}),
  saveRoutine: (routine: Routine) => call('saveRoutine', { routine }),
  deleteRoutine: (id: RoutineId) => call('deleteRoutine', { id }),
  /**
   * Ask for a routine to run now.
   *
   * Queued rather than run: the scheduler carries it out on its next tick, so
   * that runs stay serial however many times the button is pressed. Pressing it
   * twice returns the same queued run rather than paying for two model calls.
   */
  runRoutine: (id: RoutineId) => call('runRoutine', { id }),
  runs: (query: RunQuery = {}) => call('listRuns', { query }),
  run: (id: RoutineRunId) => call('getRun', { id }),
  deleteRun: (id: RoutineRunId) => call('deleteRun', { id }),
  /** Mark runs as looked at. An empty list means all of them. */
  markRunsSeen: (ids: RoutineRunId[] = []) => call('markRunsSeen', { ids }),
  /** How many runs nobody has looked at. The number on the app bar. */
  unseenRuns: () => call('unseenRuns', {}),
  routineTemplates: () => call('routineTemplates', {}),

  notes: (query: NoteQuery = {}) => call('listNotes', { query }),
  note: (id: NoteId) => call('getNote', { id }),
  newNote: () => call('newNote', {}),
  /**
   * Save a note, refusing to overwrite an edit made since `expect` was read.
   *
   * The same contract `saveEntry` has, for the same reason: a note is typed
   * into and autosaved, so two windows on one vault find each other -- and
   * `requestId` is the caller's for the same reason again. This minted its own
   * with `newRequestId()` here, which is the one thing that function's own
   * doc comment says not to do: a fresh id per attempt is an id the retry
   * cannot be recognised by.
   */
  saveNote: (note: Note, expect: string | null, requestId?: string) =>
    call('saveNote', { note, expect }, requestId),
  saveNoteForce: (note: Note) => call('saveNoteForce', { note }),
  deleteNote: (id: NoteId) => call('deleteNote', { id }),
  noteTags: () => call('noteTags', {}),

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
  tags: () => call('listTags', {}),

  // ── The task domain ────────────────────────────────────────────────
  //
  // Available only when `status.capabilities.tasks` is true; a
  // vault stores journals and nothing else, and the interface hides the
  // todo app rather than letting these fail at click time.

  projects: () => call('listProjects', {}),
  newProject: (name: string) => call('newProject', { name }),
  saveProject: (project: Project) => call('saveProject', { project }),
  /** Deletes the project, its tasks and their time blocks. */
  deleteProject: (id: ProjectId) => call('deleteProject', { id }),

  tasks: (query: TaskQuery) => call('listTasks', { query }),
  task: (id: TaskId) => call('getTask', { id }),
  /** Mints an unsaved task; fill it in and pass it to `saveTask`. */
  newTask: (opts: {
    projectId: ProjectId | null
    parentId: TaskId | null
    status: TaskStatus | null
  }) => call('newTask', opts),
  saveTask: (task: Task) => call('saveTask', { task }),
  /** One write for many tasks: what a board reorder is. */
  saveTasks: (tasks: Task[]) => call('saveTasks', { tasks }),
  /** Deletes the task, its subtasks and their time blocks. */
  deleteTask: (id: TaskId) => call('deleteTask', { id }),

  blocks: (query: BlockQuery) => call('listBlocks', { query }),
  /** Mints an unsaved block, with the machine's own time zone resolved. */
  newBlock: (opts: {
    subject: BlockSubject
    start: string
    minutes: number
    kind: BlockKind | null
  }) => call('newBlock', opts),
  saveBlock: (block: TimeBlock) => call('saveBlock', { block }),
  deleteBlock: (id: BlockId) => call('deleteBlock', { id }),

  /** Every tag in the task domain with its usage count, most used first. */
  taskTags: () => call('taskTags', {}),
  taskStats: () => call('taskStats', {}),

  // ── The calendar domain ────────────────────────────────────────────
  //
  // Subscribed calendars and their events. Time *you* schedule is a time
  // block and goes through the task commands above -- there is deliberately
  // no second way to store an appointment.

  calendars: () => call('listCalendars', {}),
  saveCalendar: (calendar: Calendar) => call('saveCalendar', { calendar }),
  /** Unsubscribe: the calendar and every event that came from it. */
  deleteCalendar: (id: CalendarId) => call('deleteCalendar', { id }),

  /**
   * Subscribe to a feed and fetch it once.
   *
   * One call rather than save-then-fetch, because the two are not
   * independent: if the address turns out not to be a calendar, the honest
   * outcome is that nothing was added.
   */
  subscribeCalendar: (opts: { name: string; url: string; color: string }) =>
    call('subscribeCalendar', opts),

  /** Add a calendar from a `.ics` file the browser read for us. */
  importCalendar: (opts: { name: string; label: string; color: string; ics: string }) =>
    call('importCalendar', opts),

  /** Refetch one feed and replace its events with what comes back. */
  syncCalendar: (id: CalendarId) => call('syncCalendar', { id }),
  /** Refetch every feed whose interval has elapsed. `force` ignores it. */
  syncDueCalendars: (force: boolean) => call('syncDueCalendars', { force }),

  events: (query: EventQuery) => call('listEvents', { query }),
  event: (id: EventId) => call('getEvent', { id }),

  /** The providers the add sheet offers, with where to find each address. */
  calendarProviders: () => call('calendarProviders', {}),

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
  kinds: () => call('listKinds', {}),
  /** Mints an unsaved shelf, with a slug derived from the name. */
  newKind: (name: string, singular: string) => call('newKind', { name, singular }),
  saveKind: (kind: Kind) => call('saveKind', { kind }),
  /** Deletes the shelf, everything on it, and those items' log rows. */
  deleteKind: (id: KindId) => call('deleteKind', { id }),

  items: (query: ItemQuery) => call('listItems', { query }),
  item: (id: ItemId) => call('getItem', { id }),

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
    call('addItem', { kindId, title, lookup }),
  saveItem: (item: Item) => call('saveItem', { item }),
  /** One write for many items: what a re-ordered shelf is. */
  saveItems: (items: Item[]) => call('saveItems', { items }),
  /** Deletes the item and its whole log. */
  deleteItem: (id: ItemId) => call('deleteItem', { id }),

  /**
   * Move an item to a status, dating it and logging it in one act.
   *
   * The dates and the log row are coupled in the backend on purpose: marking
   * a book read is the moment "finished on" is known *and* the moment the
   * log gains the row that makes "what did I read this year" answerable.
   */
  setItemStatus: (id: ItemId, status: ItemStatus, log: boolean) =>
    call('setItemStatus', { id, status, log }),
  /** Record where you have got to. Starts the item if it was only wished for. */
  setItemProgress: (id: ItemId, position: number, total: number | null, log: boolean) =>
    call('setItemProgress', { id, position, total, log }),

  logs: (query: LogQuery) => call('listLogs', { query }),
  /** Mints an unsaved log row dated today on the machine's own calendar. */
  newLog: (itemId: ItemId, event: LogEvent) => call('newLog', { itemId, event }),
  saveLog: (log: LogEntry) => call('saveLog', { log }),
  deleteLog: (id: LogId) => call('deleteLog', { id }),

  libraryStats: () => call('libraryStats', {}),

  // ── Web search ─────────────────────────────────────────────────────
  //
  // A facility rather than a feature of one app. `lib/websearch.ts` wraps
  // these with debouncing, cancellation and a cache; prefer that over
  // calling them directly.

  /** Search the web. The general entry point; anything may call it. */
  webSearch: (request: SearchRequest) => call('webSearch', { request }),
  /** The sources a search can be run against, for the picker. */
  searchSources: () => call('searchSources', {}),
  /** Look a title up using whatever source a shelf prefers. */
  lookupMetadata: (kindId: KindId, query: string, limit?: number) =>
    call('lookupMetadata', { kindId, query, limit: limit ?? null }),
  /** Apply a chosen result to an item, downloading its cover on the way. */
  applyMetadata: (id: ItemId, result: SearchResult, overwrite: boolean) =>
    call('applyMetadata', { id, result, overwrite }),
  /**
   * Download a picture into the vault and return its blob id.
   *
   * Nothing in the interface ever loads a remote image directly: the content
   * security policy allows images from `'self'` and `everyday:` and nowhere
   * else, so a cover has to come home before it can be drawn.
   */
  fetchImage: (url: string) => call('fetchImage', { url }),

  // ── Roles and goals ────────────────────────────────────────────────
  //
  // Available only when `status.capabilities.purpose` is true. The two records
  // are small; the interesting call is `balance`, which is the whole reason
  // the purpose pointer exists.

  roles: () => call('listRoles', {}),

  /** Mints an unsaved role; fill it in and pass it to `saveRole`. */
  newRole: (name: string) => call('newRole', { name }),
  saveRole: (role: Role) => call('saveRole', { role }),

  /**
   * Delete a role. Refused, with a message naming the count, while goals
   * still point at it — unlike a project, which takes its tasks with it.
   */
  deleteRole: (id: RoleId) => call('deleteRole', { id }),

  /**
   * Offer a starting set of roles, and answer 0 if there are any already.
   *
   * Never called on unlock, unlike the library's shelves: a list of what a
   * life is made of is a claim, and writing one unasked would be this
   * application telling somebody who they are.
   */
  seedRoles: () => call('seedRoles', {}),

  goals: (query: GoalQuery = {}) => call('listGoals', { query }),
  goal: (id: GoalId) => call('getGoal', { id }),
  newGoal: (roleId: RoleId, title: string) => call('newGoal', { roleId, title }),
  saveGoal: (goal: Goal) => call('saveGoal', { goal }),
  saveGoals: (goals: Goal[]) => call('saveGoals', { goals }),
  deleteGoal: (id: GoalId) => call('deleteGoal', { id }),

  /**
   * Minutes per purpose over a window, and the meetings somebody else
   * booked, in one call — the Overview draws them together, and two round
   * trips would let one arrive without the other.
   */
  balance: (from: string, to: string) => call('timeByPurpose', { from, to }),

  goalActivity: (id: GoalId) => call('goalActivity', { id }),

  // ── The tracking domain ────────────────────────────────────────────
  //
  // Available only when `status.capabilities.trackers` is true. Both halves
  // are records: a definition is its own row, and which journals draw its
  // chip is a list of ids on each journal.

  /** Mints an unsaved tracker; fill it in and pass it to `saveTracker`. */
  newTracker: (name: string, kind: TrackerKind) => call('newTracker', { name, kind }),

  /**
   * Every tracker in the vault.
   *
   * Also where a vault written before trackers became records has its old
   * definitions moved out of its journals — once, on the first call.
   */
  trackers: () => call('listTrackers', {}),
  saveTracker: (tracker: Tracker) => call('saveTracker', { tracker }),

  /**
   * Fold one tracker into another, keeping both histories, and answer how
   * many readings moved. The tidy-up for a name typed two ways.
   */
  mergeTrackers: (from: TrackerId, into: TrackerId) => call('mergeTrackers', { from, into }),

  readings: (query: ReadingQuery) => call('listReadings', { query }),

  /** One row per tracker per day: the aggregate a chart is built from. */
  trackerDays: (query: ReadingQuery) => call('trackerDays', { query }),

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
    trackerId: TrackerId
    value: number
    date: string
    at?: string | null
    /** Where it was ticked. Both absent for a reading logged from anywhere else. */
    journalId?: JournalId | null
    entryId?: EntryId | null
  }) => call('logReading', opts),

  /** Update a reading that exists: a corrected dose, a note, a time. */
  saveReading: (reading: Reading) => call('saveReading', { reading }),
  deleteReading: (id: ReadingId) => call('deleteReading', { id }),

  /**
   * Delete a tracker *and* every reading it ever made, returning how many
   * went. Archiving — a flag on the definition — is the non-destructive
   * half of this pair, and the usual answer.
   */
  deleteTracker: (id: TrackerId) => call('deleteTracker', { id }),

  // ── The assistant ──────────────────────────────────────────────────
  //
  // Available only when `status.capabilities.agent` is true. Small, because
  // almost everything the assistant can do it does through its *tools*,
  // which live in the Rust core and never cross this boundary. What is here
  // is configuring it, reading its threads back, and the two halves of one
  // exchange: `sendMessage` (exported separately, because it streams) and
  // the confirmation that answers it.

  agentSettings: () => call('agentSettings', {}),

  /** Returns what was actually stored: `hasKey` is derived, not echoed. */
  saveAgentSettings: (settings: AgentSettings) => call('saveAgentSettings', { settings }),

  /** Store the API key. There is deliberately no call that reads one back. */
  setAgentKey: (key: string) => call('setAgentKey', { key }),
  clearAgentKey: () => call('clearAgentKey', {}),

  conversations: (limit?: number) => call('listConversations', { limit }),

  /** Mints an unsaved thread; the first message is what saves it. */
  newConversation: () => call('newConversation', {}),

  conversationMessages: (id: ConversationId) => call('conversationMessages', { id }),

  deleteConversation: (id: ConversationId) => call('deleteConversation', { id }),

  /**
   * Answer a confirmation the assistant is waiting on. False means nothing
   * was waiting any more -- a turn cancelled between the question and the
   * click -- which the panel treats as a dismissal rather than an error.
   */
  confirmToolCall: (callId: string, approved: boolean) =>
    call('confirmToolCall', { callId, approved }),

  memories: () => call('listMemories', {}),

  /** A blank memory with an id, pinned. The core allocates it. */
  newMemory: () => call('newMemory', {}),

  /**
   * Saving by hand also pins: a fact somebody typed is not one the
   * assistant's own housekeeping may drop. Returns what it evicted.
   */
  saveMemory: (memory: Memory) => call('saveMemory', { memory }),
  deleteMemory: (id: MemoryId) => call('deleteMemory', { id }),

  // ── The quick model ────────────────────────────────────────────────
  //
  // One call per flow that has a use for a small, fast model. Every one is a
  // read: nothing here writes a record. What comes back is a *proposal* that
  // some component draws beside a field, and it is the ordinary `updateTask`,
  // `saveItem` or `addReading` that applies it when somebody taps.
  //
  // Components should not call these directly -- go through `quick.svelte.ts`,
  // which owns the switch check, the cancellation and the rule that a failure
  // here is silence rather than an error.

  quickJobs: () => call('quickJobs', {}),
  setQuickJob: (args: { name: string; on: boolean }) => call('setQuickJob', args),

  /** The shelf's own declared fields, out of what a search found. */
  quickItemFields: (itemId: ItemId) => call('quickItemFields', { itemId }),

  /** Which result is the thing, or null when none of them is. */
  quickPickResult: (args: { kindId: KindId; query: string; results: SearchResult[] }) =>
    call('quickPickResult', args),

  /** A whole shelf, drafted from its name. */
  quickKindDraft: (name: string) => call('quickKindDraft', { name }),

  quickImportColumns: (args: { kindId: KindId; columns: string[]; sample?: string[] }) =>
    call('quickImportColumns', args),

  quickTaskLabels: (title: string) => call('quickTaskLabels', { title }),
  quickTaskFromLine: (line: string) => call('quickTaskFromLine', { line }),
  quickSubtasks: (taskId: TaskId) => call('quickSubtasks', { taskId }),
  quickEstimate: (taskId: TaskId) => call('quickEstimate', { taskId }),

  quickEventFromLine: (line: string) => call('quickEventFromLine', { line }),
  /** Display only: the feed's own record is never rewritten. */
  quickEventTitle: (title: string) => call('quickEventTitle', { title }),

  quickEntryReadings: (entryId: EntryId) => call('quickEntryReadings', { entryId }),
  quickEntryTitle: (entryId: EntryId) => call('quickEntryTitle', { entryId }),
  quickEntryLabels: (entryId: EntryId) => call('quickEntryLabels', { entryId }),

  quickNoteTitle: (noteId: NoteId) => call('quickNoteTitle', { noteId }),
  quickNoteTasks: (noteId: NoteId) => call('quickNoteTasks', { noteId }),
  quickNoteLabels: (noteId: NoteId) => call('quickNoteLabels', { noteId }),

  quickReadingFromLine: (line: string) => call('quickReadingFromLine', { line }),
  quickTrackerDraft: (args: { name: string; line?: string }) => call('quickTrackerDraft', args),

  quickGoalWording: (args: { title: string; roleId: RoleId }) => call('quickGoalWording', args),
  quickGoalBackfill: (goalId: GoalId) => call('quickGoalBackfill', { goalId }),

  quickWeekNote: (args: { thisWeek: string; lastWeek?: string }) => call('quickWeekNote', args),

  quickFrontMatter: (args: { ours: string[]; theirs: string[] }) => call('quickFrontMatter', args),

  // ── Taking your data out, and putting it back ────────────────────────
  //
  // Nine calls rather than two because an archive does not fit in a reply:
  // both directions are a handle and a series of chunks, exactly as an
  // attachment is. `lib/transfer.svelte.ts` owns both loops; nothing else in
  // the interface should be calling these by hand.

  /** What this vault can hand over, and how much of it there is. */
  exportableParts: () => call('listParts', {}),

  /** Build the archive. The bytes stay on the vault's machine until read. */
  startExport: (parts: string[], media: boolean) => call('startExport', { parts, media }),
  readExport: (handle: string, offset: number) => call('readExport', { handle, offset }),
  /** For a download that was abandoned; a finished one drops itself. */
  endExport: (handle: string) => call('endExport', { handle }),

  startImport: (name: string, bytes: number) => call('startImport', { name, bytes }),
  writeImport: (handle: string, offset: number, data: string) =>
    call('writeImport', { handle, offset, data }),
  /** What is in it, changing nothing. The dry run the dialog shows. */
  readImport: (handle: string) => call('readImport', { handle }),
  runImport: (handle: string, parts: string[], mode: ImportMode) =>
    call('runImport', { handle, parts, mode }),
  endImport: (handle: string) => call('endImport', { handle }),

  /**
   * Ask the shell to save an already-built export where the user picks.
   *
   * Desktop only, and the reason it exists is that the bytes then never come
   * through here at all: the shell pulls the chunks from the session straight
   * into the file. A browser attached to a paired vault assembles them itself
   * instead -- see `transfer.svelte.ts`. Answers false when the dialog was
   * dismissed, which is not an error.
   */
  saveExport: (handle: string, name: string) => invoke<boolean>('save_export', { handle, name }),

  /**
   * Ask the shell for a file to import, and hand it to the vault.
   *
   * Nothing has been read into the vault when this returns: what comes back
   * is the handle to inspect and then, separately, to import with. `null`
   * means the picker was dismissed.
   */
  openImport: () => invoke<PickedFile | null>('open_import'),
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
