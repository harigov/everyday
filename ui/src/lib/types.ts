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
  createdAt: string
  updatedAt: string
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

/** What the app knows before any vault is opened. */
export interface Bootstrap {
  vaultExists: boolean
  defaultPath: string
  backends: { id: string; description: string }[]
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
