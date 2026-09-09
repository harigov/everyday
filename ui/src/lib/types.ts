// Mirrors the serde representations in `everyday-core`. Kept hand-written
// rather than generated: the surface is small, and a hand-written type is a
// place to document what the field *means* to the interface.

export type JournalId = string
export type EntryId = string
export type BlobId = string
export type ProjectId = string
export type TaskId = string
export type BlockId = string
export type CalendarId = string
export type EventId = string
export type TrackerId = string
export type ReadingId = string
export type ConversationId = string
export type MessageId = string
export type MemoryId = string

/** A ProseMirror document. Opaque to everything but the editor. */
export type RichDoc = { type: 'doc'; content?: unknown[] }

export interface Journal {
  id: JournalId
  name: string
  /** `#rrggbb`; drives the journal's accent throughout the interface. */
  color: string
  icon: string
  description: string
  sortOrder: number
  /** What this journal records beside its entries. See `Tracker`. */
  trackers: Tracker[]
  createdAt: string
  updatedAt: string
}

// ── Tracking ─────────────────────────────────────────────────────────────
//
// Four kinds of thing, one stored shape: a reading is a tracker, an instant
// and a number. The kind decides how the interface *collects* that number
// and how a chart should *aggregate* it, and nothing else.

export type TrackerKind =
  /** Done or not done. `value` is 1 or 0; no reading at all means unrecorded. */
  | 'check'
  /** Something taken, in a dose. One reading per dose, so a day is a sum. */
  | 'dose'
  /** Something felt, `0..=scaleMax`. Several a day is normal. */
  | 'scale'
  /** A quantity: minutes, pages, glasses. */
  | 'amount'

export const TRACKER_KINDS: TrackerKind[] = ['check', 'dose', 'scale', 'amount']

/** How readings combine over a day or a week. Fixed per kind. */
export type Aggregate = 'count' | 'sum' | 'mean'

export function aggregateOf(kind: TrackerKind): Aggregate {
  if (kind === 'check') return 'count'
  if (kind === 'scale') return 'mean'
  return 'sum'
}

/** A thing you have decided to record. Lives in the journal's settings. */
export interface Tracker {
  id: TrackerId
  name: string
  kind: TrackerKind
  /** A name from `tracker-icons.ts`; unknown names fall back to a dot. */
  icon: string
  /** `#rrggbb`. A tracker's own identity in a row of chips. */
  color: string
  /** Shown after the value: `mg`, `min`, `pages`. Empty for a check. */
  unit: string
  /** What the input prefills and the step its buttons take. */
  defaultValue: number
  /** A daily goal, if there is one. Drives the ring on the chip. */
  target?: number | null
  /** Top of a scale's range; the bottom is always 0. */
  scaleMax: number
  /** Draw this tracker's readings on the calendar. */
  onCalendar: boolean
  /** Retired: keeps its history, leaves the day's chips. */
  archived: boolean
  sortOrder: number
  createdAt: string
  updatedAt: string
}

/** One recorded value. */
export interface Reading {
  id: ReadingId
  journalId: JournalId
  trackerId: TrackerId
  /** The entry it was recorded beside, if there was one. */
  entryId?: EntryId | null
  /** `YYYY-MM-DD`. Always known. */
  localDate: string
  /**
   * When it happened, RFC 3339 — or absent, meaning "that day, time
   * unknown". Ticking something on a past page records the day and no
   * minute, because there was no minute to record.
   */
  at?: string | null
  tz: string
  value: number
  note: string
  createdAt: string
  updatedAt: string
}

/** One tracker's day, rolled up. What a chart is built from. */
export interface TrackerDay {
  trackerId: TrackerId
  date: string
  count: number
  sum: number
  max: number
  firstAt?: string | null
  lastAt?: string | null
}

export interface ReadingQuery {
  journalId?: JournalId | null
  trackerIds?: TrackerId[]
  entryId?: EntryId | null
  from?: string | null
  to?: string | null
  /** Only readings that know their time of day. */
  timedOnly?: boolean
  limit?: number | null
}

export type MediaKind = 'image' | 'video' | 'audio' | 'file'

export interface Attachment {
  blob: BlobId
  kind: MediaKind
  mime: string
  filename: string
  byteLen: number
  width?: number
  height?: number
  durationMs?: number
  caption: string
}

export interface Location {
  latitude: number
  longitude: number
  placeName?: string
  locality?: string
  country?: string
}

export interface Entry {
  id: EntryId
  journalId: JournalId
  title: string
  body: RichDoc
  /** `YYYY-MM-DD`, in the author's local time. What the UI groups by. */
  localDate: string
  tz: string
  createdAt: string
  updatedAt: string
  tags: string[]
  starred: boolean
  pinned: boolean
  location?: Location
  attachments: Attachment[]
}

/** The condensed form the list view renders; never carries a full body. */
export interface EntrySummary {
  id: EntryId
  journalId: JournalId
  title: string
  excerpt: string
  localDate: string
  createdAt: string
  updatedAt: string
  tags: string[]
  starred: boolean
  pinned: boolean
  wordCount: number
  attachmentCount: number
  cover?: BlobId
  place?: string
}

export type SortOrder = 'dateDesc' | 'dateAsc' | 'updatedDesc' | 'createdDesc' | 'titleAsc'

export interface EntryQuery {
  journalId?: JournalId | null
  from?: string | null
  to?: string | null
  tags?: string[]
  starred?: boolean | null
  sort?: SortOrder
  offset?: number
  limit?: number | null
}

export interface SearchHit {
  id: EntryId
  journalId: JournalId
  title: string
  localDate: string
  score: number
  snippet: string
  /** Byte ranges within `snippet` that matched. */
  highlights: [number, number][]
}

export interface StoreStats {
  journals: number
  entries: number
  blobs: number
  blobBytes: number
}

export interface Capabilities {
  blobs: boolean
  transactional: boolean
  humanReadable: boolean
  maxBlobBytes?: number | null
  /** Backend carries the task domain, so the todo app can be offered. */
  tasks: boolean
  /**
   * Backend carries the calendar domain.
   *
   * The calendar app needs *both*: it draws time blocks, which belong to the
   * task domain, over events, which belong to this one. `app.supportsCalendar`
   * is the pair, and is what the sidebar reads.
   */
  calendars: boolean
  /**
   * Backend carries the library domain, so the library app can be offered.
   *
   * Independent of the other two, unlike the calendar: nothing in the library
   * reads a task or an event, so it is offered on any backend that carries
   * this alone.
   */
  library: boolean
  /**
   * Backend carries the tracking domain, so readings have somewhere to live.
   *
   * False hides tracking entirely, settings included: offering to configure
   * what cannot then be recorded is worse than not offering it.
   */
  trackers: boolean
  /**
   * Backend carries the assistant's own domain, so its settings, threads and
   * memory have somewhere to live.
   *
   * False hides the chat panel entirely rather than offering one whose
   * conversation vanishes when the window closes.
   */
  agent: boolean
}

export interface VaultStatus {
  name: string
  backend: string
  unlocked: boolean
  encrypted: boolean
  autoLockSeconds: number
  path: string
  /**
   * False when another process holds this vault's write lock — a second copy
   * of the app, or the CLI. Reads work; every write is refused with
   * `vault_in_use`.
   */
  writable: boolean
  stats?: StoreStats
  capabilities?: Capabilities
}

/**
 * One field a backend must be told before it can be opened.
 *
 * The setup screen renders whatever the backend declares rather than knowing
 * which backends exist — so a backend that later needs two fields, or none,
 * changes in Rust and nowhere here.
 */
export interface SettingSpec {
  key: string
  label: string
  /** Shown greyed in the empty field. Never a real credential. */
  placeholder: string
  required: boolean
  /** Masked on entry, and never sent back to the interface afterwards. */
  secret: boolean
}

/** A storage backend as the vault-creation screen sees it. */
export interface BackendInfo {
  id: string
  /** Short human name, e.g. "On this computer". */
  name: string
  description: string
  /** Empty for a backend that needs nothing but a folder. */
  settings: SettingSpec[]
}

/** What the app knows before any vault is opened. */
export interface Bootstrap {
  vaultExists: boolean
  defaultPath: string
  backends: BackendInfo[]
  status: VaultStatus | null
}

/** A backend error, carrying the stable machine-readable code. */
export class VaultError extends Error {
  constructor(
    readonly code: string,
    message: string,
  ) {
    super(message)
    this.name = 'VaultError'
  }
}

// ── The task domain ──────────────────────────────────────────────────────
//
// Projects, tasks and blocks of time. Mirrors `everyday-core`'s `task`
// module; see its docs for why subtasks are just tasks and why scheduled
// time is a record rather than a field.

/** Board columns, in the order they are drawn. */
export const TASK_STATUSES = ['backlog', 'todo', 'doing', 'blocked', 'done', 'cancelled'] as const
export type TaskStatus = (typeof TASK_STATUSES)[number]

/** Neither done nor cancelled: there is still work in it. */
export function isOpen(status: TaskStatus): boolean {
  return status !== 'done' && status !== 'cancelled'
}

export const PRIORITIES = ['none', 'low', 'medium', 'high', 'urgent'] as const
export type Priority = (typeof PRIORITIES)[number]

/** Sort key. Matches `Priority::rank` in the core. */
export function priorityRank(p: Priority): number {
  return PRIORITIES.indexOf(p)
}

export type ProjectStatus = 'active' | 'paused' | 'done' | 'archived'

export interface Project {
  id: ProjectId
  name: string
  /** Plain-text description. */
  notes: string
  /** `#rrggbb`; drives the project's accent throughout the interface. */
  color: string
  icon: string
  status: ProjectStatus
  priority: Priority
  /** `YYYY-MM-DD`. */
  startDate?: string | null
  dueDate?: string | null
  estimateMinutes?: number | null
  tags: string[]
  sortOrder: number
  createdAt: string
  updatedAt: string
  completedAt?: string | null
}

export interface Task {
  id: TaskId
  /** Absent means the inbox: captured, not filed. */
  projectId?: ProjectId | null
  /** Absent means top level. Set, and this is a subtask of that task. */
  parentId?: TaskId | null
  title: string
  /** Plain-text description. */
  notes: string
  status: TaskStatus
  priority: Priority
  /** `YYYY-MM-DD`. */
  startDate?: string | null
  dueDate?: string | null
  /** `HH:MM:SS`. Meaningless without `dueDate`. */
  dueTime?: string | null
  estimateMinutes?: number | null
  tags: string[]
  /** Position within its board column or list section. */
  sortOrder: number
  createdAt: string
  updatedAt: string
  completedAt?: string | null
}

/** What a block of time was spent on. */
export type BlockSubject =
  { type: 'task'; id: TaskId } | { type: 'project'; id: ProjectId } | { type: 'adhoc' }

/** Intention or record. The pair is what makes "where did my time go" answerable. */
export type BlockKind = 'planned' | 'actual'

export interface TimeBlock {
  id: BlockId
  subject: BlockSubject
  /** Empty means "use the subject's own name". */
  title: string
  /** RFC 3339 instants. */
  start: string
  end: string
  /** `YYYY-MM-DD`: the day it is filed under, in `tz`. */
  localDate: string
  tz: string
  allDay: boolean
  kind: BlockKind
  notes: string
  tags: string[]
  createdAt: string
  updatedAt: string
}

/** Which project's tasks to look at. "Inbox" is tasks with no project. */
export type ProjectScope =
  { scope: 'any' } | { scope: 'inbox' } | { scope: 'project'; id: ProjectId }

/** Which level of the task tree to look at. */
export type ParentScope = { scope: 'any' } | { scope: 'topLevel' } | { scope: 'of'; id: TaskId }

export type TaskSort =
  | 'manual'
  | 'dueAsc'
  | 'priorityDesc'
  | 'createdDesc'
  | 'updatedDesc'
  | 'completedDesc'
  | 'titleAsc'

export interface TaskQuery {
  project?: ProjectScope
  parent?: ParentScope
  /** Empty means any status. */
  statuses?: TaskStatus[]
  tags?: string[]
  priorityAtLeast?: Priority | null
  dueFrom?: string | null
  dueTo?: string | null
  hasDue?: boolean | null
  text?: string
  sort?: TaskSort
  offset?: number
  limit?: number | null
}

export interface BlockQuery {
  from?: string | null
  to?: string | null
  taskId?: TaskId | null
  projectId?: ProjectId | null
  kind?: BlockKind | null
  limit?: number | null
}

/** Outstanding work in one project. Absent `projectId` means the inbox. */
export interface ProjectTaskCount {
  projectId?: ProjectId | null
  open: number
}

export interface TaskStats {
  projects: number
  activeProjects: number
  tasks: number
  openTasks: number
  doneTasks: number
  blocks: number
  loggedMinutes: number
  plannedMinutes: number
  /** Open tasks due on or before today, overdue ones included. */
  dueToday: number
  /** Open tasks whose deadline has already passed. A subset of `dueToday`. */
  overdue: number
  /**
   * Open tasks per project, counted by the backend over the whole vault.
   *
   * Not derived in the interface, which only ever holds what is on screen:
   * a sidebar counting its own rows would say "1" for a project with forty
   * tasks in it, because one of them happened to be due today. Projects with
   * nothing open are omitted rather than listed as zero.
   */
  openByProject: ProjectTaskCount[]
}

export interface TagCount {
  tag: string
  count: number
}

// ── The calendar domain ──────────────────────────────────────────────────
//
// Mirrors `everyday-core`'s `calendar` module. Two records only, because
// most of a calendar already existed: time you schedule for yourself is a
// `TimeBlock` (above), and what the calendar adds is other people's
// calendars — a subscription, and the events read out of it.

export type CalendarProvider = 'google' | 'outlook' | 'apple' | 'other'

/** Where a calendar's events come from. */
export type CalendarOrigin = { type: 'url'; url: string } | { type: 'file'; label: string }

export interface Calendar {
  id: CalendarId
  name: string
  /** `#rrggbb`; every event on this calendar is drawn in it. */
  color: string
  origin: CalendarOrigin
  provider: CalendarProvider
  /** Drawn on the grid. Hiding is a view setting, not an unsubscribe. */
  visible: boolean
  /** Minutes between refetches; 0 means manual only. */
  refreshMinutes: number
  lastSyncedAt?: string | null
  /** Why the last refresh failed. Shown beside the calendar, never as a dialog. */
  lastError?: string | null
  createdAt: string
  updatedAt: string
}

/** A calendar plus how many events are held for it. */
export interface CalendarInfo extends Calendar {
  events: number
}

export type EventStatus = 'confirmed' | 'tentative' | 'cancelled'

/**
 * One occurrence on a subscribed calendar. Read-only, always: nothing in
 * this application writes back to the server an event came from.
 */
export interface CalendarEvent {
  id: EventId
  calendarId: CalendarId
  /** The publisher's UID, plus this occurrence's start. */
  uid: string
  title: string
  description: string
  location: string
  /** RFC 3339 instants. */
  start: string
  end: string
  /** First and last day covered, `YYYY-MM-DD`, in `tz`. */
  localDate: string
  endDate: string
  tz: string
  allDay: boolean
  status: EventStatus
  organizer: string
  url: string
  /** False for a "free" or cancelled event, which should not read as a clash. */
  busy: boolean
  updatedAt: string
}

export interface EventQuery {
  /** Any day covered on or after this. An overlap test, not a start bound. */
  from?: string | null
  to?: string | null
  calendarId?: CalendarId | null
  /** Only calendars left ticked in the sidebar. */
  visibleOnly?: boolean
  text?: string
  limit?: number | null
}

/** What one refresh did. */
export interface SyncReport {
  calendarId?: CalendarId | null
  events: number
  /** Occurrences the feed described that fell outside the synced window. */
  skipped: number
  /** The name the publisher gives the calendar, if it offered one. */
  feedName?: string | null
}

/** A provider, and where in that product the secret address is found. */
export interface ProviderInfo {
  id: CalendarProvider
  label: string
  hint: string
}

/**
 * A notification raised by the Rust shell, off `everyday://notify`.
 *
 * Mirrors `everyday_app::notify::Notification`, and is deliberately a subset
 * of the interface's own `NotifySpec`: the shell can say what happened, how
 * serious it is and how far it needs to reach, but it cannot hand across a
 * button, because the thing a button does lives on this side of the bridge.
 */
export interface ShellNotification {
  level: 'info' | 'success' | 'warning' | 'error'
  reach: 'app' | 'user'
  title: string
  body?: string | null
  /** Identity for a condition that recurs; replaces rather than stacks. */
  key?: string | null
}

// ── The tray ───────────────────────────────────────────────────────────
//
// The wire form of a tray menu, as `set_tray_menu` takes it. What the
// interface actually writes is a `TrayEntry` in `lib/tray.svelte.ts`, which
// carries a handler as well; this is what is left after the handler has been
// put aside and kept on this side of the bridge.

export type TrayMenuItem =
  | {
      kind: 'action'
      id: string
      label: string
      enabled: boolean
      /** A checkbox rather than a plain item; `null` for a plain one. */
      checked: boolean | null
      /** Bring the window forward before the handler runs. */
      raise: boolean
    }
  | { kind: 'separator' }
  | { kind: 'submenu'; label: string; enabled: boolean; items: TrayMenuItem[] }

// ── The library domain ───────────────────────────────────────────────────
//
// Mirrors `everyday-core`'s `library` module. Three records, and the middle
// one is the point:
//
//   Kind ────── Item ────── LogEntry
//  (Books)     (Dune)      started 3 Mar, finished 2 Apr, ★★★★½
//
// A *kind* is data rather than a variant, so "Board games" is something you
// add rather than something we ship; a *log entry* is a record rather than a
// field, so reading something twice does not overwrite the first time. See
// the core module's docs for both arguments in full.

export type KindId = string
export type ItemId = string
export type LogId = string

/** How a shelf's extra fields are written. Presentation, never validation. */
export type FieldType = 'text' | 'multiline' | 'number' | 'date' | 'url'

export interface FieldDef {
  /** Stable key into `Item.facts`. Renaming the label never moves this. */
  key: string
  label: string
  fieldType: FieldType
  placeholder: string
}

/**
 * What a shelf calls the states an item can be in.
 *
 * A person *reads* a book, *watches* a series and *plays* a game, and an app
 * that insists on "in progress" for all three reads like a form.
 */
export interface Verbs {
  /** "To read", "To watch". */
  wishlist: string
  /** "Reading", "Watching", "Playing". */
  active: string
  /** "Read", "Watched", "Played". */
  done: string
  /** Lower case, for mid-sentence: "read on 4 March". */
  log: string
}

/** Where a shelf's metadata is looked up. Matches `websearch::Source`. */
export type SearchSource = 'web' | 'wikipedia' | 'openLibrary' | 'itunes' | 'nominatim'

/** A category of thing you keep track of. Data, not a variant. */
export interface Kind {
  id: KindId
  /** Stable machine name — `book`, `film`. What lookups key on. */
  slug: string
  /** Plural: it names a shelf. */
  name: string
  /** Singular: the buttons say "Add a book". */
  singular: string
  icon: string
  /** `#rrggbb`; tints the shelf and every card on it. */
  color: string
  verbs: Verbs
  fields: FieldDef[]
  /** A `SearchSource` slug. Anything unrecognised means a plain web search. */
  source: string
  /** "page", "episode", "hour". Empty means no notion of being part-way. */
  progressUnit: string
  sortOrder: number
  /** Seeded by the app rather than added by hand. */
  builtin: boolean
  visible: boolean
  createdAt: string
  updatedAt: string
}

/** A shelf plus how much is on it. */
export interface KindInfo extends Kind {
  items: number
  /** Wishlist, active and paused together: everything still ahead of you. */
  open: number
}

export const ITEM_STATUSES = ['wishlist', 'active', 'paused', 'done', 'abandoned'] as const
export type ItemStatus = (typeof ITEM_STATUSES)[number]

/** Still something you intend to get to. `abandoned` is not. */
export function isAhead(status: ItemStatus): boolean {
  return status === 'wishlist' || status === 'active' || status === 'paused'
}

/** Somebody else's score, normalised to 0–100 so two sources can be compared. */
export interface ExternalRating {
  source: string
  score: number
  count?: number | null
  url: string
}

export interface Link {
  label: string
  url: string
}

/** How far through you are. `total` is absent when there is no finish line. */
export interface Progress {
  position: number
  total?: number | null
  unit: string
}

export interface Item {
  id: ItemId
  kindId: KindId
  title: string
  subtitle: string
  /** Author, director, artist, developer, chef. */
  creator: string
  year?: number | null
  status: ItemStatus
  /** Yours, 0–100. `stars()` in `lib/rating.ts` is the display conversion. */
  rating?: number | null
  external: ExternalRating[]
  /** A blob id. Covers are downloaded into the vault, never hot-linked. */
  cover?: BlobId | null
  /** Where the cover came from, kept so it can be fetched again. */
  coverUrl: string
  /** The blurb, in the source's words. A re-fetch replaces it. */
  summary: string
  /** Yours. Nothing a lookup does can touch this. */
  notes: string
  tags: string[]
  /** The shelf's extra fields, keyed by `FieldDef.key`. */
  facts: Record<string, string>
  links: Link[]
  progress?: Progress | null
  favourite: boolean
  /** `YYYY-MM-DD`. Denormalised from the log for the card and the sort. */
  startedOn?: string | null
  finishedOn?: string | null
  /** Which source filled this in; empty for something typed by hand. */
  source: string
  sortOrder: number
  createdAt: string
  updatedAt: string
}

/** What sort of thing happened, on a day. */
export const LOG_EVENTS = [
  'started',
  'progress',
  'finished',
  'revisited',
  'note',
  'stopped',
] as const
export type LogEvent = (typeof LOG_EVENTS)[number]

/** Both ways of getting to the end of something. */
export function isCompletion(event: LogEvent): boolean {
  return event === 'finished' || event === 'revisited'
}

/** One occasion on which you did something about an item. */
export interface LogEntry {
  id: LogId
  itemId: ItemId
  event: LogEvent
  /** `YYYY-MM-DD`, in `tz`. A day, because a day is what a person knows. */
  date: string
  tz: string
  note: string
  /** What you thought *at the time*, 0–100. */
  rating?: number | null
  position?: number | null
  minutes?: number | null
  createdAt: string
  updatedAt: string
}

export type ItemSort =
  | 'addedDesc'
  | 'addedAsc'
  | 'updatedDesc'
  | 'titleAsc'
  | 'ratingDesc'
  | 'finishedDesc'
  | 'yearAsc'
  | 'yearDesc'
  | 'manual'

export interface ItemQuery {
  kindId?: KindId | null
  /** Empty means any status. */
  statuses?: ItemStatus[]
  tags?: string[]
  text?: string
  favourite?: boolean | null
  /** 0–100. An unrated item never passes a lower bound. */
  ratingAtLeast?: number | null
  finishedFrom?: string | null
  finishedTo?: string | null
  sort?: ItemSort
  offset?: number
  limit?: number | null
}

export interface LogQuery {
  itemId?: ItemId | null
  from?: string | null
  to?: string | null
  /** Empty means any event. */
  events?: LogEvent[]
  limit?: number | null
}

/** What is on one shelf. Counted by the backend over the whole vault. */
export interface KindCount {
  kindId: KindId
  items: number
  open: number
  active: number
}

export interface LibraryStats {
  kinds: number
  items: number
  wishlist: number
  active: number
  done: number
  /** Log rows meaning "got to the end of it" dated in the current year. */
  finishedThisYear: number
  rated: number
  /** Mean of your ratings on the 0–100 scale; absent if you have rated none. */
  meanRating?: number | null
  byKind: KindCount[]
}

/** What `addItem` hands back: the item, and whether the web knew it. */
export interface AddedItem {
  item: Item
  lookedUp: boolean
}

// ── Web search ───────────────────────────────────────────────────────────
//
// A facility rather than a feature of the library: `lib/websearch.ts` is the
// class any component can reach for. Mirrors `everyday-core`'s `websearch`.

export interface SearchRequest {
  query: string
  source: SearchSource
  /** The `Kind.slug` this is for, when there is one. */
  hint: string
  limit: number
}

/** One answer, from any source, in one shape. Everything is optional. */
export interface SearchResult {
  title: string
  subtitle: string
  creator: string
  summary: string
  year?: number | null
  url: string
  imageUrl: string
  /** Their score, normalised to 0–100. */
  rating?: number | null
  ratingCount?: number | null
  /** Already keyed to match the field keys the seeded shelves use. */
  facts: Record<string, string>
  source: string
}

/** A source, for the picker. */
export interface SourceInfo {
  id: SearchSource
  label: string
  hasImages: boolean
}

// ── The assistant ────────────────────────────────────────────────────────
//
// Mirrors `everyday_core::agent`. Note what is *not* here: the API key. It
// has no field on `AgentSettings` on the Rust side either, and there is no
// command that reads one back -- see the module docs there for why that is
// structural rather than a convention.

/** Which family of API the model is spoken to over. */
export type Provider = 'openAi'

export interface ModelConfig {
  provider: Provider
  /** As the endpoint spells it: `gpt-5.1`, `qwen3:32b`. */
  model: string
  /** Overrides the provider default. What points this at a local model. */
  baseUrl: string | null
  temperature: number | null
  maxTokens: number | null
}

export interface AgentSettings {
  /** Off until somebody turns it on, and off in a new vault. */
  enabled: boolean
  model: ModelConfig
  /** The person's own standing instructions. */
  instructions: string
  /** Whether a destructive tool call stops and asks first. */
  confirmDestructive: boolean
  /** Model turns one request may take before the loop gives up. */
  maxSteps: number
  /** Whether the assistant may write memories. */
  remember: boolean
  /** Whether a key is stored. Never the key. */
  hasKey: boolean
}

export type Role = 'user' | 'assistant' | 'tool' | 'system'

export interface ToolCall {
  id: string
  name: string
  arguments: unknown
}

export interface AgentMessage {
  id: MessageId
  conversationId: ConversationId
  role: Role
  content: string
  toolCalls: ToolCall[]
  toolCallId: string | null
  /** Set on a tool turn whose tool failed, so it can be drawn as one
   *  without the interface parsing its prose. */
  failed: boolean
  createdAt: string
}

export interface Conversation {
  id: ConversationId
  title: string
  createdAt: string
  updatedAt: string
}

/** A thread in the history list, with how long it is. */
export interface ConversationSummary extends Conversation {
  messages: number
}

export interface Memory {
  id: MemoryId
  text: string
  sourceId: ConversationId | null
  /** Written or edited by hand, so the assistant's own trimming leaves it. */
  pinned: boolean
  createdAt: string
  updatedAt: string
}

/**
 * One thing that happened during a turn.
 *
 * Arrives over a channel as the turn runs rather than all at once at the end
 * -- see `send_message` in the Rust shell. `finished` and `failed` are the
 * two terminal ones, and exactly one of them always arrives.
 */
export type AgentEvent =
  | { type: 'started'; messageId: MessageId }
  | { type: 'delta'; text: string }
  | { type: 'toolStarted'; callId: string; name: string; arguments: unknown }
  | { type: 'toolFinished'; callId: string; name: string; ok: boolean; summary: string }
  | {
      type: 'confirmationRequired'
      callId: string
      name: string
      subject: string
      arguments: unknown
    }
  | { type: 'finished'; messageId: MessageId }
  | { type: 'failed'; message: string }
