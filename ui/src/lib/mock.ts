// An in-memory stand-in for the Rust backend.
//
// This is not a toy: it implements every command the interface calls, so the
// whole app -- lock screen, journals, editor, search, media -- runs in a
// plain browser. It exists so the interface can be designed and reviewed
// without a native build, and so a broken Rust build never blocks UI work.
//
// It is reachable only from a development build. `api.ts` gates the import
// on `import.meta.env.DEV`, so this module is eliminated at build time and
// never reaches a released application.

import type {
  BlockKind,
  BlockQuery,
  BlockSubject,
  Bootstrap,
  Calendar,
  CalendarEvent,
  CalendarInfo,
  EventQuery,
  ProviderInfo,
  SyncReport,
  Entry,
  EntryQuery,
  EntrySummary,
  Journal,
  Project,
  SearchHit,
  Task,
  TaskQuery,
  TaskStats,
  TaskStatus,
  TimeBlock,
  VaultStatus,
} from './types'
import { TASK_STATUSES, VaultError, isOpen, priorityRank } from './types'
import { DEFAULT_COLORS } from './colors'

const PASSWORD = 'everyday'

/**
 * A string argument off the command bridge.
 *
 * Arguments arrive as `unknown`, because this file stands in for a process
 * that receives JSON. `String(x)` on an object does not fail -- it yields
 * "[object Object]" -- which in a fake backend is a silent wrong answer
 * rather than a loud one. Narrow, and fall back when the shape is not what
 * was expected.
 */
function str(v: unknown, fallback = ''): string {
  return typeof v === 'string' ? v : fallback
}

function iso(daysAgo: number): string {
  const d = new Date()
  d.setDate(d.getDate() - daysAgo)
  return d.toISOString()
}

function day(daysAgo: number): string {
  // Local, not `toISOString()`: that is UTC, so an evening west of Greenwich
  // would stamp today's entry with tomorrow's date.
  const d = new Date()
  d.setDate(d.getDate() - daysAgo)
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`
}

function para(...lines: string[]) {
  return lines.map((text) => ({
    type: 'paragraph',
    content: text ? [{ type: 'text', text }] : [],
  }))
}

/** A soft gradient standing in for a photograph. */
function swatch(a: string, b: string, label: string): string {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="800">
    <defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0%" stop-color="${a}"/><stop offset="100%" stop-color="${b}"/>
    </linearGradient></defs>
    <rect width="1200" height="800" fill="url(#g)"/>
    <text x="60" y="740" font-family="system-ui,sans-serif" font-size="34"
          fill="rgba(255,255,255,.72)">${label}</text>
  </svg>`
  return `data:image/svg+xml;base64,${btoa(unescape(encodeURIComponent(svg)))}`
}

const BLOBS: Record<string, string> = {
  ['a'.repeat(64)]: swatch('#f0a35e', '#b8434f', 'the harbour at dusk'),
  ['b'.repeat(64)]: swatch('#4a7fb5', '#1c2f4a', 'the train window'),
  ['c'.repeat(64)]: swatch('#7ba05b', '#2f4a2f', 'first frost'),
}

const journals: Journal[] = [
  {
    id: 'j-daily',
    name: 'Daily',
    color: '#c2410c',
    icon: '\u{1f342}',
    description: 'The ordinary days',
    sortOrder: 0,
    createdAt: iso(400),
    updatedAt: iso(1),
  },
  {
    id: 'j-travel',
    name: 'Travel',
    color: '#0f766e',
    icon: '\u{2708}\u{fe0f}',
    description: 'Trips, trains and long walks',
    sortOrder: 1,
    createdAt: iso(300),
    updatedAt: iso(9),
  },
  {
    id: 'j-notes',
    name: 'Reading',
    color: '#4338ca',
    icon: '\u{1f4d6}',
    description: 'Books and the thoughts they caused',
    sortOrder: 2,
    createdAt: iso(200),
    updatedAt: iso(20),
  },
]

const entries: Entry[] = [
  {
    id: 'e-1',
    journalId: 'j-daily',
    title: 'First frost',
    body: {
      type: 'doc',
      content: [
        ...para(
          'The grass was stiff underfoot this morning and the light came in low and orange across the field, the way it only does for about three weeks a year.',
        ),
        {
          type: 'media',
          attrs: {
            blob: 'c'.repeat(64),
            kind: 'image',
            mime: 'image/svg+xml',
            filename: 'frost.jpg',
            caption: 'The field at half past seven',
          },
        },
        ...para(
          'I stood at the gate longer than I meant to. There is a particular silence to a cold morning that I never manage to describe properly, and every year I try again.',
        ),
        {
          type: 'blockquote',
          content: para('Note to self: buy proper gloves before this gets serious.'),
        },
      ],
    },
    localDate: day(0),
    tz: 'Europe/London',
    createdAt: iso(0),
    updatedAt: iso(0),
    tags: ['winter', 'walking'],
    starred: true,
    pinned: false,
    attachments: [
      {
        blob: 'c'.repeat(64),
        kind: 'image',
        mime: 'image/svg+xml',
        filename: 'frost.jpg',
        byteLen: 2_400_000,
        width: 1200,
        height: 800,
        caption: 'The field at half past seven',
      },
    ],
  },
  {
    id: 'e-2',
    journalId: 'j-daily',
    title: 'A slow Sunday',
    body: {
      type: 'doc',
      content: [
        ...para(
          'Bread, coffee, and most of a book. Nothing happened, which was the entire point of it.',
        ),
        {
          type: 'bulletList',
          content: [
            {
              type: 'listItem',
              content: para('Finished the sourdough, finally got the crumb right'),
            },
            { type: 'listItem', content: para('Walked to the bridge and back') },
            { type: 'listItem', content: para('Did not open the laptop once') },
          ],
        },
      ],
    },
    localDate: day(1),
    tz: 'Europe/London',
    createdAt: iso(1),
    updatedAt: iso(1),
    tags: ['rest'],
    starred: false,
    pinned: false,
    attachments: [],
  },
  {
    id: 'e-3',
    journalId: 'j-travel',
    title: 'Arriving in Lisbon',
    body: {
      type: 'doc',
      content: [
        ...para(
          'The taxi driver took the long way along the river so I could see the bridge lit up. Worth every extra euro, and he knew it.',
        ),
        {
          type: 'media',
          attrs: {
            blob: 'a'.repeat(64),
            kind: 'image',
            mime: 'image/svg+xml',
            filename: 'harbour.jpg',
            caption: 'From the taxi window, somewhere near Belém',
          },
        },
        ...para(
          'The flat smells of woodsmoke and the window opens onto a courtyard where somebody was practising scales until about ten.',
        ),
      ],
    },
    localDate: day(8),
    tz: 'Europe/Lisbon',
    createdAt: iso(8),
    updatedAt: iso(7),
    tags: ['portugal', 'travel'],
    starred: true,
    pinned: true,
    location: {
      latitude: 38.7223,
      longitude: -9.1393,
      placeName: 'Alfama',
      locality: 'Lisbon',
      country: 'Portugal',
    },
    attachments: [
      {
        blob: 'a'.repeat(64),
        kind: 'image',
        mime: 'image/svg+xml',
        filename: 'harbour.jpg',
        byteLen: 3_100_000,
        width: 1200,
        height: 800,
        caption: 'From the taxi window, somewhere near Belém',
      },
    ],
  },
  {
    id: 'e-4',
    journalId: 'j-travel',
    title: 'The train to Porto',
    body: {
      type: 'doc',
      content: [
        ...para('Three hours of eucalyptus and red roofs.'),
        {
          type: 'media',
          attrs: {
            blob: 'b'.repeat(64),
            kind: 'image',
            mime: 'image/svg+xml',
            filename: 'train.jpg',
            caption: 'Somewhere past Coimbra',
          },
        },
        ...para(
          'I read one page and looked out of the window for the rest, which felt like the correct allocation of a morning.',
        ),
      ],
    },
    localDate: day(9),
    tz: 'Europe/Lisbon',
    createdAt: iso(9),
    updatedAt: iso(9),
    tags: ['portugal', 'trains'],
    starred: false,
    pinned: false,
    attachments: [
      {
        blob: 'b'.repeat(64),
        kind: 'image',
        mime: 'image/svg+xml',
        filename: 'train.jpg',
        byteLen: 2_800_000,
        width: 1200,
        height: 800,
        caption: 'Somewhere past Coimbra',
      },
    ],
  },
  {
    id: 'e-5',
    journalId: 'j-notes',
    title: 'On rereading',
    body: {
      type: 'doc',
      content: [
        ...para(
          'Started the Berger again. I remember almost none of it, which is either a failure of attention or the entire argument for rereading.',
        ),
        {
          type: 'blockquote',
          content: para('We only see what we look at. To look is an act of choice.'),
        },
        ...para('Twelve years since the last time. Different book now.'),
      ],
    },
    localDate: day(21),
    tz: 'Europe/London',
    createdAt: iso(21),
    updatedAt: iso(21),
    tags: ['books'],
    starred: false,
    pinned: false,
    attachments: [],
  },
]

// ── The task domain ──────────────────────────────────────────────────────

function ahead(days: number): string {
  return day(-days)
}

const projects: Project[] = [
  {
    id: 'p-house',
    name: 'Move house',
    notes: 'Completion is the 12th. Everything hangs off that.',
    color: '#c2410c',
    icon: '\u{1f4e6}',
    status: 'active',
    priority: 'high',
    startDate: day(20),
    dueDate: ahead(12),
    estimateMinutes: 3_600,
    tags: ['home'],
    sortOrder: 0,
    createdAt: iso(40),
    updatedAt: iso(1),
  },
  {
    id: 'p-site',
    name: 'Rebuild the site',
    notes: 'Static, fast, and no analytics.',
    color: '#0f766e',
    icon: '\u{1f5a5}\u{fe0f}',
    status: 'active',
    priority: 'medium',
    dueDate: ahead(30),
    estimateMinutes: 1_800,
    tags: ['work'],
    sortOrder: 1,
    createdAt: iso(60),
    updatedAt: iso(3),
  },
  {
    id: 'p-garden',
    name: 'The garden',
    notes: '',
    color: '#15803d',
    icon: '\u{1f331}',
    status: 'paused',
    priority: 'low',
    tags: ['home'],
    sortOrder: 2,
    createdAt: iso(120),
    updatedAt: iso(30),
  },
]

let taskSeq = 0

function seedTask(t: Partial<Task> & { title: string }): Task {
  taskSeq += 1
  return {
    id: `t-${taskSeq}`,
    projectId: null,
    parentId: null,
    notes: '',
    status: 'todo',
    priority: 'none',
    tags: [],
    sortOrder: 0,
    createdAt: iso(10),
    updatedAt: iso(1),
    ...t,
  }
}

const tasks: Task[] = [
  // Move house: a board with something in most columns.
  seedTask({
    id: 't-pack',
    title: 'Pack the study',
    projectId: 'p-house',
    status: 'doing',
    priority: 'high',
    dueDate: ahead(2),
    estimateMinutes: 240,
    tags: ['moving'],
    notes: 'The books are the whole job. Everything else is an afternoon.',
    sortOrder: 0,
  }),
  seedTask({
    id: 't-books',
    title: 'Box up the books',
    projectId: 'p-house',
    parentId: 't-pack',
    status: 'done',
    completedAt: iso(1),
    estimateMinutes: 120,
    sortOrder: 0,
  }),
  seedTask({
    id: 't-shelves',
    title: 'Take the shelves down',
    projectId: 'p-house',
    parentId: 't-pack',
    dueDate: ahead(2),
    estimateMinutes: 60,
    sortOrder: 1,
  }),
  seedTask({
    id: 't-cables',
    title: 'Label the cables',
    projectId: 'p-house',
    parentId: 't-pack',
    priority: 'low',
    sortOrder: 2,
  }),
  seedTask({
    id: 't-van',
    title: 'Book the van',
    projectId: 'p-house',
    priority: 'urgent',
    dueDate: day(1),
    dueTime: '17:00:00',
    estimateMinutes: 30,
    tags: ['moving', 'money'],
    notes: 'Two quotes so far. The cheaper one has no tail lift.',
    sortOrder: 1,
  }),
  seedTask({
    id: 't-meter',
    title: 'Read the meters on the day',
    projectId: 'p-house',
    status: 'backlog',
    dueDate: ahead(12),
    sortOrder: 2,
  }),
  seedTask({
    id: 't-broadband',
    title: 'Chase the broadband transfer',
    projectId: 'p-house',
    status: 'blocked',
    priority: 'high',
    tags: ['admin'],
    notes: 'They said 5 working days on the 3rd. It has been nine.',
    sortOrder: 3,
  }),
  seedTask({
    id: 't-deposit',
    title: 'Get the deposit back',
    projectId: 'p-house',
    status: 'done',
    completedAt: iso(4),
    tags: ['money'],
    sortOrder: 4,
  }),

  // The site.
  seedTask({
    id: 't-type',
    title: 'Settle the type scale',
    projectId: 'p-site',
    status: 'doing',
    priority: 'medium',
    dueDate: ahead(1),
    estimateMinutes: 90,
    tags: ['design'],
    sortOrder: 0,
  }),
  seedTask({
    id: 't-build',
    title: 'Move the build to the new runner',
    projectId: 'p-site',
    dueDate: ahead(5),
    estimateMinutes: 180,
    tags: ['work'],
    sortOrder: 1,
  }),
  seedTask({
    id: 't-archive',
    title: 'Import the old archive',
    projectId: 'p-site',
    status: 'backlog',
    tags: ['work'],
    sortOrder: 2,
  }),

  // The inbox: captured, not filed. Including one that has gone past.
  seedTask({
    id: 't-dentist',
    title: 'Ring the dentist back',
    priority: 'high',
    dueDate: day(2),
    tags: ['admin'],
    sortOrder: 0,
  }),
  seedTask({ id: 't-boots', title: 'Resole the walking boots', sortOrder: 1 }),
  seedTask({
    id: 't-gift',
    title: 'Something for Ana\u{2019}s birthday',
    dueDate: ahead(6),
    tags: ['family'],
    sortOrder: 2,
  }),
]

function blockAt(
  id: string,
  taskId: string,
  daysAgo: number,
  hour: number,
  minutes: number,
  kind: BlockKind,
): TimeBlock {
  const start = new Date()
  start.setDate(start.getDate() - daysAgo)
  start.setHours(hour, 0, 0, 0)
  return {
    id,
    subject: { type: 'task', id: taskId },
    title: '',
    start: start.toISOString(),
    end: new Date(start.getTime() + minutes * 60_000).toISOString(),
    localDate: day(daysAgo),
    tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
    allDay: false,
    kind,
    notes: '',
    tags: [],
    createdAt: iso(daysAgo),
    updatedAt: iso(daysAgo),
  }
}

const blocks: TimeBlock[] = [
  blockAt('b-1', 't-books', 1, 10, 150, 'actual'),
  blockAt('b-2', 't-pack', 0, 9, 120, 'planned'),
  blockAt('b-3', 't-type', 2, 14, 75, 'actual'),
  blockAt('b-4', 't-van', -1, 11, 30, 'planned'),
]

// ── The calendar domain ──────────────────────────────────────────────────
//
// Two subscribed calendars and a week of plausible meetings, so the grid can
// be designed against something that looks like a real Tuesday rather than
// against an empty page. One of them is deliberately in a failed state: a
// feed that cannot be reached is a thing the sidebar has to say well, and it
// is hard to get right if you never see it.

const calendars: CalendarInfo[] = [
  {
    id: 'c-work',
    name: 'Priya — Work',
    color: '#0369a1',
    origin: { type: 'url', url: 'https://calendar.google.com/calendar/ical/…/basic.ics' },
    provider: 'google',
    visible: true,
    refreshMinutes: 60,
    lastSyncedAt: iso(0),
    createdAt: iso(30),
    updatedAt: iso(0),
    events: 0,
  },
  {
    id: 'c-holidays',
    name: 'Public holidays',
    color: '#15803d',
    origin: { type: 'url', url: 'https://example.com/holidays.ics' },
    provider: 'other',
    visible: true,
    refreshMinutes: 1440,
    lastSyncedAt: iso(3),
    lastError: 'there is no calendar at that address any more. It may have been revoked.',
    createdAt: iso(60),
    updatedAt: iso(0),
    events: 0,
  },
]

function eventAt(
  id: string,
  calendarId: string,
  daysAhead: number,
  hour: number,
  minutes: number,
  title: string,
  extra: Partial<CalendarEvent> = {},
): CalendarEvent {
  const start = new Date()
  start.setDate(start.getDate() + daysAhead)
  start.setHours(hour, 0, 0, 0)
  const localDate = day(-daysAhead)
  return {
    id,
    calendarId,
    uid: `${id}@example.com`,
    title,
    description: '',
    location: '',
    start: start.toISOString(),
    end: new Date(start.getTime() + minutes * 60_000).toISOString(),
    localDate,
    endDate: localDate,
    tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
    allDay: false,
    status: 'confirmed',
    organizer: '',
    url: '',
    busy: true,
    updatedAt: iso(1),
    ...extra,
  }
}

const events: CalendarEvent[] = [
  eventAt('ev-1', 'c-work', 0, 9, 15, 'Stand-up', { location: 'Meeting room 4' }),
  eventAt('ev-2', 'c-work', 0, 11, 60, 'Design review', {
    organizer: 'Priya Raman',
    description: 'Bring the two options and the numbers behind them.',
  }),
  eventAt('ev-3', 'c-work', 0, 15, 30, 'One-to-one', { status: 'tentative' }),
  eventAt('ev-4', 'c-work', 1, 9, 15, 'Stand-up', { location: 'Meeting room 4' }),
  eventAt('ev-5', 'c-work', 1, 13, 90, 'Quarterly planning'),
  eventAt('ev-6', 'c-work', 2, 9, 15, 'Stand-up', { location: 'Meeting room 4' }),
  eventAt('ev-7', 'c-work', 2, 16, 45, 'Retro', { status: 'cancelled', busy: false }),
  eventAt('ev-8', 'c-work', 3, 10, 120, 'Workshop', { location: 'Off site' }),
  eventAt('ev-9', 'c-holidays', 4, 0, 1440, 'Spring bank holiday', {
    allDay: true,
    busy: false,
  }),
]

/** Open tasks grouped by project; `null` is the inbox. Mirrors the SQL. */
function openPerProject(): Map<string | null, number> {
  const out = new Map<string | null, number>()
  for (const t of tasks) {
    if (!isOpen(t.status)) continue
    const key = t.projectId ?? null
    out.set(key, (out.get(key) ?? 0) + 1)
  }
  return out
}

function putTask(t: Task) {
  const i = tasks.findIndex((x) => x.id === t.id)
  if (i >= 0) tasks[i] = structuredClone(t)
  else tasks.push(structuredClone(t))
}

/** `roots` plus every task nested under them, at any depth. */
function subtreeOf(roots: string[]): Set<string> {
  const out = new Set(roots)
  let grew = true
  while (grew) {
    grew = false
    for (const t of tasks) {
      if (t.parentId && out.has(t.parentId) && !out.has(t.id)) {
        out.add(t.id)
        grew = true
      }
    }
  }
  return out
}

function dropTasks(doomed: Set<string>) {
  for (let i = tasks.length - 1; i >= 0; i--) {
    if (doomed.has(tasks[i]!.id)) tasks.splice(i, 1)
  }
  for (let i = blocks.length - 1; i >= 0; i--) {
    const s = blocks[i]!.subject
    if (s.type === 'task' && doomed.has(s.id)) blocks.splice(i, 1)
  }
}

/**
 * The JavaScript twin of `TaskQuery::matches` and `apply` in the core.
 *
 * It has to agree with the Rust, or the interface would be designed against
 * behaviour the real backend does not have -- which is the one way a mock
 * this thorough can still mislead.
 */
function applyTaskQuery(q: TaskQuery): Task[] {
  let rows = tasks.filter((t) => {
    const project = q.project ?? { scope: 'any' }
    if (project.scope === 'inbox' && t.projectId) return false
    if (project.scope === 'project' && t.projectId !== project.id) return false

    const parent = q.parent ?? { scope: 'any' }
    if (parent.scope === 'topLevel' && t.parentId) return false
    if (parent.scope === 'of' && t.parentId !== parent.id) return false

    if (q.statuses?.length && !q.statuses.includes(t.status)) return false
    if (q.priorityAtLeast && priorityRank(t.priority) < priorityRank(q.priorityAtLeast)) {
      return false
    }
    if (q.hasDue != null && !!t.dueDate !== q.hasDue) return false
    // An undated task is outside every date window, not inside all of them.
    if (q.dueFrom || q.dueTo) {
      if (!t.dueDate) return false
      if (q.dueFrom && t.dueDate < q.dueFrom) return false
      if (q.dueTo && t.dueDate > q.dueTo) return false
    }
    if (q.text?.trim()) {
      const hay = `${t.title}\n${t.notes}\n${t.tags.join(' ')}`.toLowerCase()
      if (!hay.includes(q.text.trim().toLowerCase())) return false
    }
    return (q.tags ?? []).every((want) =>
      t.tags.some((have) => have.toLowerCase() === want.toLowerCase()),
    )
  })

  const byOrder = (a: Task, b: Task) =>
    a.sortOrder - b.sortOrder || a.createdAt.localeCompare(b.createdAt)
  rows = [...rows].sort((a, b) => {
    switch (q.sort ?? 'manual') {
      case 'dueAsc':
        if (!a.dueDate && !b.dueDate) return byOrder(a, b)
        if (!a.dueDate) return 1
        if (!b.dueDate) return -1
        return a.dueDate.localeCompare(b.dueDate) || byOrder(a, b)
      case 'priorityDesc':
        return priorityRank(b.priority) - priorityRank(a.priority) || byOrder(a, b)
      case 'createdDesc':
        return b.createdAt.localeCompare(a.createdAt)
      case 'updatedDesc':
        return b.updatedAt.localeCompare(a.updatedAt)
      case 'completedDesc':
        return (b.completedAt ?? '').localeCompare(a.completedAt ?? '')
      case 'titleAsc':
        return a.title.localeCompare(b.title)
      default:
        return byOrder(a, b)
    }
  })

  const from = q.offset ?? 0
  return rows.slice(from, q.limit ? from + q.limit : undefined).map((t) => structuredClone(t))
}

// A reference to keep the status list honest against the core's ordering.
void TASK_STATUSES

// Start unlocked when the URL says so. Only the mock honours this, and it
// exists so the interface can be opened straight to the main view when
// reviewing or screenshotting it.
let unlocked = new URLSearchParams(location.search).has('unlocked')
let nextId = 100

function plainText(node: unknown): string {
  if (!node || typeof node !== 'object') return ''
  const n = node as Record<string, unknown>
  if (n.type === 'text') return str(n.text)
  const caption = (n.attrs as Record<string, unknown> | undefined)?.caption
  const own = n.type === 'media' && typeof caption === 'string' ? caption : ''
  const kids = Array.isArray(n.content) ? n.content.map(plainText).join(' ') : ''
  return [own, kids].filter(Boolean).join(' ')
}

function summarize(e: Entry): EntrySummary {
  const text = plainText(e.body)
  return {
    id: e.id,
    journalId: e.journalId,
    title: e.title || text.slice(0, 60) || 'Untitled entry',
    excerpt: text.slice(0, 240),
    localDate: e.localDate,
    createdAt: e.createdAt,
    updatedAt: e.updatedAt,
    tags: e.tags,
    starred: e.starred,
    pinned: e.pinned,
    wordCount: text.split(/\s+/).filter(Boolean).length,
    attachmentCount: e.attachments.length,
    cover: e.attachments.find((a) => a.kind === 'image')?.blob,
    place: e.location?.placeName ?? e.location?.locality,
  }
}

function status(): VaultStatus {
  return {
    name: 'My Journal',
    backend: 'sqlite',
    unlocked,
    encrypted: true,
    autoLockSeconds: 900,
    path: '/Users/you/Library/Application Support/EveryDay',
    stats: unlocked
      ? {
          journals: journals.length,
          entries: entries.length,
          blobs: Object.keys(BLOBS).length,
          blobBytes: 8_300_000,
        }
      : undefined,
    capabilities: unlocked
      ? { blobs: true, transactional: true, humanReadable: false, tasks: true, calendars: true }
      : undefined,
  }
}

function requireUnlocked() {
  if (!unlocked) throw new VaultError('locked', 'vault is locked')
}

export const mockInvoke = async <T>(
  cmd: string,
  args: Record<string, unknown> = {},
): Promise<T> => {
  // A touch of latency, so loading states are exercised rather than skipped.
  await new Promise((r) => setTimeout(r, 40))

  switch (cmd) {
    case 'bootstrap':
      return {
        vaultExists: true,
        defaultPath: '/Users/you/Library/Application Support/EveryDay',
        backends: [
          {
            id: 'sqlite',
            description: 'SQLite database — fastest, best for large journals (recommended)',
          },
          { id: 'markdown', description: 'Markdown files — readable and syncable with any tool' },
        ],
        status: status(),
      } satisfies Bootstrap as T

    case 'unlock':
      if (args.password !== PASSWORD) throw new VaultError('bad_password', 'incorrect password')
      unlocked = true
      return status() as T

    case 'create_vault':
      unlocked = true
      return status() as T

    case 'open_vault':
    case 'status':
      return status() as T

    case 'lock':
      unlocked = false
      return status() as T

    case 'change_password':
      if (args.current !== PASSWORD) throw new VaultError('bad_password', 'incorrect password')
      return undefined as T

    case 'set_auto_lock':
    case 'touch':
      return undefined as T

    case 'poll_auto_lock':
      return false as T

    case 'list_journals':
      requireUnlocked()
      return [...journals].sort((a, b) => a.sortOrder - b.sortOrder) as T

    case 'new_journal': {
      requireUnlocked()
      const now = new Date().toISOString()
      return {
        id: `j-${nextId++}`,
        name: args.name as string,
        color: DEFAULT_COLORS[journals.length % DEFAULT_COLORS.length]!,
        icon: '\u{1f4d3}',
        description: '',
        sortOrder: journals.length,
        createdAt: now,
        updatedAt: now,
      } as T
    }

    case 'save_journal': {
      requireUnlocked()
      const j = args.journal as Journal
      const i = journals.findIndex((x) => x.id === j.id)
      if (i >= 0) journals[i] = j
      else journals.push(j)
      return undefined as T
    }

    case 'delete_journal': {
      requireUnlocked()
      const id = args.id as string
      journals.splice(
        journals.findIndex((j) => j.id === id),
        1,
      )
      for (let i = entries.length - 1; i >= 0; i--) {
        if (entries[i]!.journalId === id) entries.splice(i, 1)
      }
      return undefined as T
    }

    case 'list_entries': {
      requireUnlocked()
      const q = (args.query ?? {}) as EntryQuery
      let rows = entries.map(summarize)
      if (q.journalId) rows = rows.filter((r) => r.journalId === q.journalId)
      if (q.starred) rows = rows.filter((r) => r.starred)
      if (q.tags?.length) {
        rows = rows.filter((r) =>
          q.tags!.every((t) => r.tags.some((x) => x.toLowerCase() === t.toLowerCase())),
        )
      }
      rows.sort((a, b) => {
        if (a.pinned !== b.pinned) return a.pinned ? -1 : 1
        if (q.sort === 'dateAsc') return a.localDate.localeCompare(b.localDate)
        if (q.sort === 'titleAsc') return a.title.localeCompare(b.title)
        if (q.sort === 'updatedDesc') return b.updatedAt.localeCompare(a.updatedAt)
        return b.localDate.localeCompare(a.localDate)
      })
      return rows.slice(q.offset ?? 0, q.limit ? (q.offset ?? 0) + q.limit : undefined) as T
    }

    case 'get_entry': {
      requireUnlocked()
      const e = entries.find((x) => x.id === args.id)
      if (!e) throw new VaultError('not_found', 'entry not found')
      return structuredClone(e) as T
    }

    case 'new_entry': {
      requireUnlocked()
      const now = new Date()
      return {
        id: `e-${nextId++}`,
        journalId: args.journalId as string,
        title: '',
        body: { type: 'doc', content: [{ type: 'paragraph' }] },
        localDate: day(0),
        tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
        createdAt: now.toISOString(),
        updatedAt: now.toISOString(),
        tags: [],
        starred: false,
        pinned: false,
        attachments: [],
      } satisfies Entry as T
    }

    case 'save_entry': {
      requireUnlocked()
      const e = args.entry as Entry
      const i = entries.findIndex((x) => x.id === e.id)
      if (i >= 0) entries[i] = structuredClone(e)
      else entries.unshift(structuredClone(e))
      return undefined as T
    }

    case 'delete_entry': {
      requireUnlocked()
      entries.splice(
        entries.findIndex((e) => e.id === args.id),
        1,
      )
      return undefined as T
    }

    case 'search': {
      requireUnlocked()
      const q = str(args.query).trim().toLowerCase()
      if (!q) return [] as T
      const hits: SearchHit[] = []
      for (const e of entries) {
        if (args.journalId && e.journalId !== args.journalId) continue
        const text = `${e.title}\n${plainText(e.body)}\n${e.tags.join(' ')}`
        const at = text.toLowerCase().indexOf(q)
        if (at < 0) continue
        const start = Math.max(0, at - 60)
        const snippet = (start > 0 ? '…' : '') + text.slice(start, at + 140)
        const rel = at - start + (start > 0 ? 1 : 0)
        hits.push({
          id: e.id,
          journalId: e.journalId,
          title: e.title,
          localDate: e.localDate,
          score: 1,
          snippet,
          highlights: [[rel, rel + q.length]],
        })
      }
      return hits.slice(0, Number(args.limit ?? 10)) as T
    }

    case 'put_blob':
      requireUnlocked()
      return 'd'.repeat(64) as T

    case 'list_tags':
      requireUnlocked()
      return [...new Set(entries.flatMap((e) => e.tags))].sort() as T

    // ── The task domain ──────────────────────────────────────────────

    case 'list_projects':
      requireUnlocked()
      return [...projects].sort((a, b) => a.sortOrder - b.sortOrder) as T

    case 'new_project': {
      requireUnlocked()
      const now = new Date().toISOString()
      return {
        id: `p-${nextId++}`,
        name: args.name as string,
        notes: '',
        color: DEFAULT_COLORS[projects.length % DEFAULT_COLORS.length]!,
        icon: '\u{1f5c2}\u{fe0f}',
        status: 'active',
        priority: 'none',
        tags: [],
        sortOrder: projects.length,
        createdAt: now,
        updatedAt: now,
      } satisfies Project as T
    }

    case 'save_project': {
      requireUnlocked()
      const p = args.project as Project
      const i = projects.findIndex((x) => x.id === p.id)
      if (i >= 0) projects[i] = structuredClone(p)
      else projects.push(structuredClone(p))
      return undefined as T
    }

    case 'delete_project': {
      requireUnlocked()
      const id = args.id as string
      const doomed = subtreeOf(tasks.filter((t) => t.projectId === id).map((t) => t.id))
      dropTasks(doomed)
      for (let i = blocks.length - 1; i >= 0; i--) {
        const subject = blocks[i]!.subject
        if (subject.type === 'project' && subject.id === id) blocks.splice(i, 1)
      }
      projects.splice(
        projects.findIndex((p) => p.id === id),
        1,
      )
      return undefined as T
    }

    case 'list_tasks': {
      requireUnlocked()
      return applyTaskQuery(args.query ?? {}) as T
    }

    case 'get_task': {
      requireUnlocked()
      const t = tasks.find((x) => x.id === args.id)
      if (!t) throw new VaultError('not_found', 'task not found')
      return structuredClone(t) as T
    }

    case 'new_task': {
      requireUnlocked()
      const now = new Date().toISOString()
      return {
        id: `t-${nextId++}`,
        projectId: (args.projectId as string | null) ?? null,
        parentId: (args.parentId as string | null) ?? null,
        title: '',
        notes: '',
        status: (args.status as TaskStatus | null) ?? 'todo',
        priority: 'none',
        tags: [],
        sortOrder: 0,
        createdAt: now,
        updatedAt: now,
      } satisfies Task as T
    }

    case 'save_task':
      requireUnlocked()
      putTask(args.task as Task)
      return undefined as T

    case 'save_tasks':
      requireUnlocked()
      for (const t of args.tasks as Task[]) putTask(t)
      return undefined as T

    case 'delete_task':
      requireUnlocked()
      dropTasks(subtreeOf([args.id as string]))
      return undefined as T

    case 'list_blocks': {
      requireUnlocked()
      const q = (args.query ?? {}) as BlockQuery
      return blocks
        .filter((b) => {
          if (q.from && b.localDate < q.from) return false
          if (q.to && b.localDate > q.to) return false
          if (q.kind && b.kind !== q.kind) return false
          if (q.taskId && !(b.subject.type === 'task' && b.subject.id === q.taskId)) return false
          if (q.projectId && !(b.subject.type === 'project' && b.subject.id === q.projectId)) {
            return false
          }
          return true
        })
        .sort((a, b) => a.start.localeCompare(b.start))
        .map((b) => structuredClone(b)) as T
    }

    case 'new_block': {
      requireUnlocked()
      const start = new Date(args.start as string)
      const minutes = args.minutes as number
      const now = new Date().toISOString()
      const p = (n: number) => String(n).padStart(2, '0')
      return {
        id: `b-${nextId++}`,
        subject: args.subject as BlockSubject,
        title: '',
        start: start.toISOString(),
        end: new Date(start.getTime() + minutes * 60_000).toISOString(),
        localDate: `${start.getFullYear()}-${p(start.getMonth() + 1)}-${p(start.getDate())}`,
        tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
        allDay: false,
        kind: (args.kind as BlockKind | null) ?? 'planned',
        notes: '',
        tags: [],
        createdAt: now,
        updatedAt: now,
      } satisfies TimeBlock as T
    }

    case 'save_block': {
      requireUnlocked()
      const b = args.block as TimeBlock
      const i = blocks.findIndex((x) => x.id === b.id)
      if (i >= 0) blocks[i] = structuredClone(b)
      else blocks.push(structuredClone(b))
      return undefined as T
    }

    case 'delete_block':
      requireUnlocked()
      blocks.splice(
        blocks.findIndex((b) => b.id === args.id),
        1,
      )
      return undefined as T

    case 'task_tags': {
      requireUnlocked()
      const counts = new Map<string, number>()
      for (const tags of [
        ...projects.map((p) => p.tags),
        ...tasks.map((t) => t.tags),
        ...blocks.map((b) => b.tags),
      ]) {
        for (const tag of tags) counts.set(tag, (counts.get(tag) ?? 0) + 1)
      }
      return [...counts]
        .map(([tag, count]) => ({ tag, count }))
        .sort((a, b) => b.count - a.count || a.tag.localeCompare(b.tag)) as T
    }

    case 'task_stats': {
      requireUnlocked()
      const minutes = (kind: BlockKind) =>
        blocks
          .filter((b) => b.kind === kind)
          .reduce(
            (sum, b) =>
              sum + Math.max(0, Math.round((Date.parse(b.end) - Date.parse(b.start)) / 60_000)),
            0,
          )
      return {
        projects: projects.length,
        activeProjects: projects.filter((p) => p.status === 'active' || p.status === 'paused')
          .length,
        tasks: tasks.length,
        openTasks: tasks.filter((t) => isOpen(t.status)).length,
        doneTasks: tasks.filter((t) => t.status === 'done').length,
        blocks: blocks.length,
        loggedMinutes: minutes('actual'),
        plannedMinutes: minutes('planned'),
        dueToday: tasks.filter((t) => isOpen(t.status) && t.dueDate && t.dueDate <= day(0)).length,
        overdue: tasks.filter((t) => isOpen(t.status) && t.dueDate && t.dueDate < day(0)).length,
        openByProject: [...openPerProject()].map(([projectId, open]) => ({
          projectId,
          open,
        })),
      } satisfies TaskStats as T
    }

    // ── The calendar domain ──────────────────────────────────────────

    case 'list_calendars':
      requireUnlocked()
      return calendars.map((c) => ({
        ...c,
        events: events.filter((e) => e.calendarId === c.id).length,
      })) as T

    case 'save_calendar': {
      requireUnlocked()
      const c = args.calendar as Calendar
      const i = calendars.findIndex((x) => x.id === c.id)
      const withCount = { ...c, events: events.filter((e) => e.calendarId === c.id).length }
      if (i >= 0) calendars[i] = withCount
      else calendars.push(withCount)
      return undefined as T
    }

    case 'delete_calendar': {
      requireUnlocked()
      const id = args.id as string
      const i = calendars.findIndex((c) => c.id === id)
      if (i >= 0) calendars.splice(i, 1)
      for (let j = events.length - 1; j >= 0; j--) {
        if (events[j]!.calendarId === id) events.splice(j, 1)
      }
      return undefined as T
    }

    case 'list_events': {
      requireUnlocked()
      const q = (args.query ?? {}) as EventQuery
      const visible = new Set(calendars.filter((c) => c.visible).map((c) => c.id))
      return events
        .filter((e) => {
          // The same overlap test the SQL does: any day the event covers.
          if (q.from && e.endDate < q.from) return false
          if (q.to && e.localDate > q.to) return false
          if (q.calendarId && e.calendarId !== q.calendarId) return false
          if (q.visibleOnly && !visible.has(e.calendarId)) return false
          if (q.text?.trim()) {
            const hay = `${e.title}\n${e.description}\n${e.location}`.toLowerCase()
            if (!hay.includes(q.text.trim().toLowerCase())) return false
          }
          return true
        })
        .sort((a, b) => Number(b.allDay) - Number(a.allDay) || a.start.localeCompare(b.start))
        .slice(0, q.limit ?? undefined) as T
    }

    case 'get_event': {
      requireUnlocked()
      const e = events.find((x) => x.id === args.id)
      if (!e) throw new VaultError('not_found', `event ${str(args.id)} not found`)
      return e as T
    }

    case 'subscribe_calendar':
    case 'import_calendar': {
      requireUnlocked()
      // No network here, obviously. What the mock does provide is the two
      // shapes the interface has to handle -- a subscription that works and
      // one that does not -- so both paths through the sheet are reachable
      // without a server to point at.
      const address = str(args.url) || str(args.label)
      if (/fail|nope|localhost/i.test(address)) {
        throw new VaultError(
          'network',
          'there is no calendar at that address any more. It may have been revoked.',
        )
      }
      const added: CalendarInfo = {
        id: `c-${Math.random().toString(36).slice(2, 8)}`,
        name: str(args.name).trim() || 'Subscribed calendar',
        color: str(args.color, '#4338ca'),
        origin:
          cmd === 'import_calendar'
            ? { type: 'file', label: str(args.label, 'calendar.ics') }
            : { type: 'url', url: address },
        provider: /google/i.test(address)
          ? 'google'
          : /outlook|office/i.test(address)
            ? 'outlook'
            : /icloud/i.test(address)
              ? 'apple'
              : 'other',
        visible: true,
        refreshMinutes: cmd === 'import_calendar' ? 0 : 60,
        lastSyncedAt: new Date().toISOString(),
        createdAt: new Date().toISOString(),
        updatedAt: new Date().toISOString(),
        events: 0,
      }
      calendars.push(added)
      return added as T
    }

    case 'sync_calendar':
    case 'sync_due_calendars': {
      requireUnlocked()
      const reports: SyncReport[] = calendars
        .filter((c) => c.origin.type === 'url' && !c.lastError)
        .map((c) => ({
          calendarId: c.id,
          events: events.filter((e) => e.calendarId === c.id).length,
          skipped: 0,
        }))
      return (cmd === 'sync_calendar' ? (reports[0] ?? { events: 0, skipped: 0 }) : reports) as T
    }

    case 'calendar_providers':
      requireUnlocked()
      return [
        {
          id: 'google',
          label: 'Google Calendar',
          hint: 'Settings \u2192 your calendar \u2192 Integrate calendar \u2192 Secret address in iCal format.',
        },
        {
          id: 'outlook',
          label: 'Outlook',
          hint: 'Settings \u2192 Calendar \u2192 Shared calendars \u2192 Publish a calendar, then copy the ICS link.',
        },
        {
          id: 'apple',
          label: 'Apple Calendar',
          hint: 'iCloud.com \u2192 Calendar \u2192 the share icon beside a calendar \u2192 Public Calendar, then copy the link.',
        },
        {
          id: 'other',
          label: 'Another calendar',
          hint: 'Any address publishing an iCalendar (.ics) feed \u2014 a team calendar, a fixture list, your country\u2019s public holidays.',
        },
      ] satisfies ProviderInfo[] as T

    default:
      throw new VaultError('unknown', `no mock for command ${cmd}`)
  }
}

export function mockMediaUrl(blob: string): string {
  return BLOBS[blob] ?? swatch('#8a8a8a', '#4a4a4a', 'missing media')
}
