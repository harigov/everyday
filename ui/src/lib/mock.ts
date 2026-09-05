// An in-memory stand-in for the Rust backend.
//
// This is not a toy: it implements every command the interface calls, so the
// whole app -- lock screen, journals, editor, search, media -- runs in a
// plain browser. It exists so the interface can be designed and reviewed
// without a native build, and so a broken Rust build never blocks UI work.

import type {
  Bootstrap,
  Entry,
  EntryQuery,
  EntrySummary,
  Journal,
  SearchHit,
  VaultStatus,
} from './types'
import { VaultError } from './types'

const PASSWORD = 'everyday'

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
    <text x="60" y="740" font-family="Georgia,serif" font-size="34"
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
            { type: 'listItem', content: para('Finished the sourdough, finally got the crumb right') },
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
    location: { latitude: 38.7223, longitude: -9.1393, placeName: 'Alfama', locality: 'Lisbon', country: 'Portugal' },
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

// Start unlocked when the URL says so. Only the mock honours this, and it
// exists so the interface can be opened straight to the main view when
// reviewing or screenshotting it.
let unlocked = new URLSearchParams(location.search).has('unlocked')
let nextId = 100

function plainText(node: unknown): string {
  if (!node || typeof node !== 'object') return ''
  const n = node as Record<string, unknown>
  if (n.type === 'text') return String(n.text ?? '')
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
      ? { blobs: true, transactional: true, humanReadable: false }
      : undefined,
  }
}

function requireUnlocked() {
  if (!unlocked) throw new VaultError('locked', 'vault is locked')
}

export const mockInvoke = async <T,>(
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
          { id: 'sqlite', description: 'SQLite database — fastest, best for large journals (recommended)' },
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
      journals.splice(journals.findIndex((j) => j.id === id), 1)
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
      entries.splice(entries.findIndex((e) => e.id === args.id), 1)
      return undefined as T
    }

    case 'search': {
      requireUnlocked()
      const q = String(args.query ?? '').trim().toLowerCase()
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

    default:
      throw new VaultError('unknown', `no mock for command ${cmd}`)
  }
}

export function mockMediaUrl(blob: string): string {
  return BLOBS[blob] ?? swatch('#8a8a8a', '#4a4a4a', 'missing media')
}
