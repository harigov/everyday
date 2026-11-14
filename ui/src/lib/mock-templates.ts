// The mock backend's templates: one list for the journal, notes and meeting
// notes, standing in for `karmayoga_core::template` and the
// `domains::templates` commands. Placeholders, rules and lint follow the Rust
// closely enough that the interface can be driven against them; the Rust is
// the authority, and its tests are where the edge cases are pinned.

import type {
  Entry,
  Journal,
  RenderedTemplate,
  RichDoc,
  Template,
  TemplateId,
  TemplateUse,
  When,
} from './types'

export const MEETING_DEFAULT_BODY = `## Summary
Two to four sentences: what this meeting was for, what was settled and what happens next.

## Decisions
Bullets. Only what was actually agreed.

## Action items
A table, one row per action. Owner is "Unassigned" when nobody took it on.

| Owner | Action | Due |
|---|---|---|

## Open questions
Bullets. What was raised and left unsettled.
`

export const MEETING_BRIEF_BODY = `## Summary
Two sentences.

## Action items
- [ ] Owner — what — by when, only if a date was said.
`

const DAILY_BODY = `## {{weekday}}
What today is for, in a sentence.

## On my mind

## Grateful for
`

const WEEKLY_BODY = `## What went well this week

## What did not

## Next week, week {{week+1w}}
`

const stamp = '2026-09-01T09:00:00.000Z'

let templates: Template[] = [
  {
    id: 'tpl-default',
    name: 'Meeting notes',
    uses: ['meetings'],
    title: '',
    body: MEETING_DEFAULT_BODY,
    tags: [],
    createdAt: stamp,
    updatedAt: stamp,
  },
  {
    id: 'tpl-brief',
    name: 'Brief',
    uses: ['meetings'],
    title: '',
    body: MEETING_BRIEF_BODY,
    tags: [],
    createdAt: '2026-09-01T09:00:01.000Z',
    updatedAt: stamp,
  },
  {
    id: 'tpl-daily',
    name: 'Daily page',
    uses: ['journal', 'notes'],
    title: '',
    body: DAILY_BODY,
    tags: [],
    createdAt: '2026-09-01T09:00:02.000Z',
    updatedAt: stamp,
  },
  {
    id: 'tpl-weekly',
    name: 'Weekly review',
    uses: ['journal', 'notes'],
    title: 'Week {{week}} review',
    body: WEEKLY_BODY,
    tags: ['review'],
    createdAt: '2026-09-01T09:00:03.000Z',
    updatedAt: stamp,
  },
]

let next = 1

export function mockTemplates(): Template[] {
  return templates.map((t) => structuredClone(t))
}

export function mockTemplate(id: TemplateId): Template | undefined {
  return templates.find((t) => t.id === id)
}

export function mockNewTemplate(uses: TemplateUse[]): Template {
  const now = new Date().toISOString()
  return {
    id: `tpl-new-${next++}`,
    name: 'New template',
    uses,
    title: '',
    body: uses.includes('meetings')
      ? MEETING_DEFAULT_BODY
      : '## Today\nWhat today is for, in a sentence.\n\n## On my mind\n\n## Grateful for\n',
    tags: [],
    createdAt: now,
    updatedAt: now,
  }
}

export function mockSaveTemplate(template: Template): void {
  if (!template.name.trim()) throw new Error('a template needs a name')
  const at = templates.findIndex((t) => t.id === template.id)
  if (at === -1) templates = [...templates, structuredClone(template)]
  else templates = templates.map((t, i) => (i === at ? structuredClone(template) : t))
}

export function mockDeleteTemplate(id: TemplateId): void {
  templates = templates.filter((t) => t.id !== id)
}

// ── Placeholders ─────────────────────────────────────────────────────────

const DATE_KEYS = ['date', 'weekday', 'day', 'month', 'year', 'week', 'quarter']
const MEETING_KEYS = [
  'title',
  'start',
  'end',
  'duration',
  'organizer',
  'attendees',
  'present',
  'calendar',
]
const MONTHS = [
  'January',
  'February',
  'March',
  'April',
  'May',
  'June',
  'July',
  'August',
  'September',
  'October',
  'November',
  'December',
]
const DAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday']

function parseIso(iso: string): Date {
  const [y, m, d] = iso.split('-').map(Number)
  return new Date(y!, m! - 1, d)
}

function isoOf(date: Date): string {
  const p = (n: number) => String(n).padStart(2, '0')
  return `${date.getFullYear()}-${p(date.getMonth() + 1)}-${p(date.getDate())}`
}

function isoWeek(date: Date): number {
  const d = new Date(Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()))
  const day = d.getUTCDay() || 7
  d.setUTCDate(d.getUTCDate() + 4 - day)
  const yearStart = new Date(Date.UTC(d.getUTCFullYear(), 0, 1))
  return Math.ceil(((d.getTime() - yearStart.getTime()) / 86400000 + 1) / 7)
}

interface Parsed {
  name: string
  offset: { n: number; unit: string } | null
  format: string | null
}

function parsePlaceholder(raw: string): Parsed | null {
  const m = /^\s*([a-zA-Z]+)\s*(?:([+-])\s*(\d+)\s*([dwmyDWMY]))?\s*(?::(.*))?$/.exec(raw)
  if (!m) return null
  return {
    name: m[1]!.toLowerCase(),
    offset: m[2] ? { n: Number(m[3]) * (m[2] === '-' ? -1 : 1), unit: m[4]!.toLowerCase() } : null,
    format: m[5] ?? null,
  }
}

function dateValue(p: Parsed, iso: string): string | null {
  if (!DATE_KEYS.includes(p.name)) return null
  if (p.format != null && p.name !== 'date') return null
  const d = parseIso(iso)
  if (p.offset) {
    const { n, unit } = p.offset
    if (unit === 'd') d.setDate(d.getDate() + n)
    if (unit === 'w') d.setDate(d.getDate() + 7 * n)
    if (unit === 'm') d.setMonth(d.getMonth() + n)
    if (unit === 'y') d.setFullYear(d.getFullYear() + n)
  }
  // A format is the real thing's strftime; the mock shows the long date.
  switch (p.name) {
    case 'date':
      return `${DAYS[d.getDay()]} ${d.getDate()} ${MONTHS[d.getMonth()]} ${d.getFullYear()}`
    case 'weekday':
      return DAYS[d.getDay()]!
    case 'day':
      return String(d.getDate())
    case 'month':
      return MONTHS[d.getMonth()]!
    case 'year':
      return String(d.getFullYear())
    case 'week':
      return String(isoWeek(d))
    case 'quarter':
      return `Q${Math.floor(d.getMonth() / 3) + 1}`
  }
  return null
}

function fill(text: string, iso: string, journal: string | null): string {
  return text.replace(/\{\{([^}]*)\}\}/g, (whole, raw: string) => {
    const p = parsePlaceholder(raw)
    if (!p) return whole
    if (p.name === 'journal' && !p.offset && p.format == null) return journal ?? whole
    return dateValue(p, iso) ?? whole
  })
}

/** Markdown to the editor's document, for the shapes a template has. */
function toDoc(markdown: string): RichDoc {
  const content: unknown[] = []
  let bullets: unknown[] | null = null
  const text = (t: string) => (t ? [{ type: 'text', text: t }] : undefined)
  for (const line of markdown.split('\n')) {
    const heading = /^(#{1,6})\s+(.*)$/.exec(line)
    const bullet = /^\s*[-*]\s+(?:\[[ x]\]\s+)?(.*)$/.exec(line)
    if (bullet) {
      bullets ??= []
      bullets.push({
        type: 'listItem',
        content: [{ type: 'paragraph', content: text(bullet[1]!) }],
      })
      continue
    }
    if (bullets) {
      content.push({ type: 'bulletList', content: bullets })
      bullets = null
    }
    if (heading) {
      content.push({
        type: 'heading',
        attrs: { level: Math.min(3, heading[1]!.length) },
        content: text(heading[2]!),
      })
    } else if (line.trim()) {
      content.push({ type: 'paragraph', content: text(line) })
    }
  }
  if (bullets) content.push({ type: 'bulletList', content: bullets })
  if (!content.length) content.push({ type: 'paragraph' })
  return { type: 'doc', content }
}

export function mockRenderTemplate(
  template: Template,
  date: string,
  journal: string | null,
): RenderedTemplate {
  return {
    title: fill(template.title, date, journal).trim(),
    body: toDoc(fill(template.body, date, journal)),
    tags: [...template.tags],
  }
}

// ── Rules ────────────────────────────────────────────────────────────────

function mondayOf(d: Date): Date {
  const m = new Date(d)
  m.setDate(m.getDate() - ((d.getDay() + 6) % 7))
  return m
}

export function mockMatches(when: When, iso: string): boolean {
  const d = parseIso(iso)
  const weekday = d.getDay() || 7
  const last = new Date(d.getFullYear(), d.getMonth() + 1, 0).getDate()
  switch (when.kind) {
    case 'weekdays':
      return when.days.includes(weekday)
    case 'everyWeeks': {
      const weeks = Math.round(
        (mondayOf(d).getTime() - mondayOf(parseIso(when.from)).getTime()) / (7 * 86400000),
      )
      const every = Math.max(1, when.every)
      return (
        ((weeks % every) + every) % every === 0 &&
        (when.days.length === 0 || when.days.includes(weekday))
      )
    }
    case 'monthDay':
      return when.day === -1 ? d.getDate() === last : d.getDate() === when.day
    case 'nthWeekday':
      return (
        when.day === weekday &&
        (when.nth === -1
          ? d.getDate() + 7 > last
          : Math.floor((d.getDate() - 1) / 7) + 1 === when.nth)
      )
    case 'yearDay':
      return d.getMonth() + 1 === when.month && d.getDate() === when.day
  }
}

/** Fill a new entry from its journal's template for its date, as `new_entry` does. */
export function mockStartEntry(entry: Entry, journal: Journal | undefined): Entry {
  const jt = journal?.templates
  if (!journal || !jt) return entry
  const find = (id: TemplateId) => templates.find((t) => t.id === id)
  const chosen =
    (jt.rules ?? [])
      .filter((r) => mockMatches(r.when, entry.localDate))
      .map((r) => find(r.template))
      .find(Boolean) ?? (jt.default ? find(jt.default) : undefined)
  if (!chosen) return entry
  const r = mockRenderTemplate(chosen, entry.localDate, journal.name)
  return { ...entry, title: r.title, body: r.body, tags: r.tags }
}

// ── Lint ─────────────────────────────────────────────────────────────────

const PLACE: Record<TemplateUse, string> = {
  journal: 'a journal entry',
  notes: 'a note',
  meetings: 'meeting notes',
}

export function mockLintTemplate(template: Template): string[] {
  const issues: string[] = []
  const both = `${template.title}\n${template.body}`
  if ((both.match(/\{\{/g) ?? []).length !== (both.match(/\}\}/g) ?? []).length) {
    issues.push('Unbalanced braces: {{ and }} do not match.')
  }
  const seen = new Set<string>()
  for (const m of both.matchAll(/\{\{([^}]*)\}\}/g)) {
    const raw = m[1]!
    if (seen.has(raw.trim())) continue
    seen.add(raw.trim())
    const p = parsePlaceholder(raw)
    const shown = `{{${raw}}}`
    if (!p) {
      issues.push(`Unknown placeholder ${shown}`)
      continue
    }
    const only: TemplateUse | null = MEETING_KEYS.includes(p.name)
      ? 'meetings'
      : p.name === 'journal'
        ? 'journal'
        : null
    if (!only && !DATE_KEYS.includes(p.name)) {
      issues.push(`Unknown placeholder ${shown}`)
      continue
    }
    if (p.format != null && p.name !== 'date') {
      issues.push(`${shown}: only {{date}} takes a format`)
      continue
    }
    if (only) {
      const elsewhere = template.uses.filter((u) => u !== only).map((u) => PLACE[u])
      if (elsewhere.length) {
        issues.push(
          `${shown} is only filled in ${PLACE[only]}, so ${elsewhere.join(' or ')} would show it as typed.`,
        )
      }
    }
  }
  if (template.uses.includes('meetings') && !/^\s{0,3}#{1,6}\s/m.test(template.body)) {
    issues.push(
      'A template for meeting notes needs headings: they are what the model keeps, and the text under each tells it what to write there.',
    )
  }
  if (template.uses.length === 0) {
    issues.push('This template is not offered anywhere yet. Tick where it should be used.')
  }
  return issues
}

export { isoOf as mockIsoOf }
