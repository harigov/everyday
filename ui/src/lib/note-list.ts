// What the notes list draws, worked out apart from the component that draws
// it: the sections it is cut into, which tags earn a chip of their own, and
// what each kind of note is called.
//
// The list was one undivided column -- every note alike, pinned ones first,
// forty tags drawn above it with nothing to say which mattered. These are the
// three rules that replaced that, and they are here rather than inside
// `NotesNav.svelte` so they can be tested without a store or a rendered list,
// the way `entry-rows.ts` is for the journal.

import { daysBetween, friendlyDate, groupLabel, rowDate } from './format'
import type { IconName } from './icons'
import { isoDate, startOfDay } from './time'
import type { NoteId, NoteOrigin, NoteSort, NoteSummary, NoteTag } from './types'

export type NoteRow =
  { kind: 'header'; key: string; label: string } | { kind: 'note'; key: NoteId; note: NoteSummary }

/**
 * The moment a row is filed under: when it was made for "Newest", when it
 * was last changed otherwise -- the same moment the list is ordered by, so a
 * section heading never disagrees with the order beneath it.
 */
export function sortedAt(note: NoteSummary, sort: NoteSort): string {
  return sort === 'createdDesc' ? note.createdAt : note.updatedAt
}

/**
 * Order `notes` in place exactly as the backend's `sort_notes` does: pinned
 * first whatever the sort, then the sort's own key, title ties broken by id.
 * For a list patched one row at a time from a change event, which must land
 * where a full reload would have put it -- a row left where it was, or put
 * at the front, splits a section in two around it.
 */
export function sortNotes(notes: NoteSummary[], sort: NoteSort): NoteSummary[] {
  const key = (n: NoteSummary) => n.title.toLowerCase()
  return notes.sort((a, b) => {
    if (a.pinned !== b.pinned) return a.pinned ? -1 : 1
    if (sort === 'titleAsc') {
      if (key(a) !== key(b)) return key(a) < key(b) ? -1 : 1
      return a.id < b.id ? -1 : a.id > b.id ? 1 : 0
    }
    // ISO instants from one serializer, so text order is time order.
    const at = (n: NoteSummary) => sortedAt(n, sort)
    return at(a) < at(b) ? 1 : at(a) > at(b) ? -1 : 0
  })
}

/**
 * `notes`, in `sortNotes`'s order (pinned first), cut into sections
 * with a heading row before each: Pinned, then Today, Yesterday, Earlier
 * this week and the months -- `groupLabel`, which the journal's list uses
 * too. Sorted by title there are no dates to head anything with, so the
 * unpinned rest is one section, called Notes, and has no heading at all
 * when nothing is pinned above it to tell it apart from.
 *
 * A header's key is the id of the note that opens it rather than its label,
 * for the reason `entry-rows.ts` gives: two runs can share a label, and a
 * repeated key is one Svelte's keyed `{#each}` would conflate.
 */
export function toNoteRows(notes: NoteSummary[], sort: NoteSort): NoteRow[] {
  const rows: NoteRow[] = []
  let last: string | null = null
  for (const note of notes) {
    const label = note.pinned
      ? 'Pinned'
      : sort === 'titleAsc'
        ? 'Notes'
        : groupLabel(isoDate(new Date(sortedAt(note, sort))))
    if (label !== last) {
      rows.push({ kind: 'header', key: `header:${note.id}`, label })
      last = label
    }
    rows.push({ kind: 'note', key: note.id, note })
  }
  if (rows[0]?.kind === 'header' && rows[0].label === 'Notes') rows.shift()
  return rows
}

/**
 * The date at the end of a row. Under a date heading it is `rowDate`'s --
 * the time for today and yesterday, since the heading already said which.
 * Pinned rows and a list sorted by title have no such heading, so there
 * yesterday has to say so rather than pass for today.
 */
export function rowWhen(note: NoteSummary, sort: NoteSort, now = new Date()): string {
  const at = sortedAt(note, sort)
  if (!note.pinned && sort !== 'titleAsc') return rowDate(at, now)
  const day = isoDate(new Date(at))
  return daysBetween(startOfDay(day), now) === 1 ? friendlyDate(day, now) : rowDate(at, now)
}

/** How many tags get a chip of their own before the rest go behind "More". */
export const TOP_TAGS = 6

/**
 * Which tags are drawn as chips over the list, and how many are left for
 * the "More" menu.
 *
 * `tags` arrives most used first (`note_tags` orders it). A short list is
 * drawn whole. A long one is cut to the most used few, and only tags on more
 * than one note make that cut: a tag carried by a single note is a way to
 * that note, not a way to narrow a list, and a real vault had thirty-one of
 * its forty tags on one note each. The tag being filtered by is always
 * drawn, wherever it ranks, so the filter in force can be seen and undone.
 */
export function shortlistTags(
  tags: NoteTag[],
  selected: string | null,
  top = TOP_TAGS,
): { shown: NoteTag[]; rest: number } {
  const shown = tags.length <= top + 1 ? [...tags] : tags.slice(0, top).filter((t) => t.count > 1)
  if (selected && !shown.some((t) => sameTag(t.tag, selected))) {
    // Under its own spelling when no note carries it any more -- the last
    // note with it untagged or deleted under the filter -- or the filter in
    // force would have nothing on screen to undo it.
    shown.push(tags.find((t) => sameTag(t.tag, selected)) ?? { tag: selected, count: 0 })
  }
  return { shown, rest: tags.length - shown.filter((t) => t.count > 0).length }
}

/** One tag to a person, case aside -- as the backend filters and counts. */
export function sameTag(a: string, b: string | null): boolean {
  return b !== null && a.toLowerCase() === b.toLowerCase()
}

/** Each kind of note, in the order the filter offers them. */
export const ORIGINS: NoteOrigin[] = ['own', 'assistant', 'meeting']

/** Each kind's icon: the mark in a row's gutter and over the open note,
 *  and its chip in the kind filter. Only the filter draws `own`'s -- a row
 *  of the person's own carries no mark, since that is what a note is, and a
 *  mark on every one of them would only be noise for the other two to stand
 *  out from. */
export const ORIGIN_ICONS: Record<NoteOrigin, IconName> = {
  own: 'pencil',
  assistant: 'wing',
  meeting: 'mic',
}

/**
 * What the filter calls a kind of note. The assistant goes by whatever its
 * owner named it -- `agent.displayName` -- so a note "Sakha" wrote is filed
 * under Sakha, not under a word the owner never chose.
 */
export function originLabel(origin: NoteOrigin, assistant: string): string {
  if (origin === 'assistant') return assistant
  return origin === 'meeting' ? 'Meetings' : 'Mine'
}

/** The line over an open note saying where it came from. Nothing for the
 *  person's own, for the reason `ORIGIN_ICONS` gives. */
export function originLine(origin: NoteOrigin, assistant: string): string | null {
  if (origin === 'assistant') return `Written by ${assistant}`
  return origin === 'meeting' ? 'Notes from a call' : null
}

/**
 * Who started `note`, read from its tags when it is older than the field --
 * the same reading `NoteOrigin::guess` makes in the core, which the vault
 * applies to every list row but never to a note it hands out whole, so that
 * a guess saved back is never kept. Only the two writers that are not the
 * person tag what they make: the meeting pipeline `meeting`, a research job
 * `research`.
 */
export function noteOrigin(note: { origin?: NoteOrigin | null; tags: string[] }): NoteOrigin {
  if (note.origin) return note.origin
  if (note.tags.some((t) => sameTag(t, 'meeting'))) return 'meeting'
  return note.tags.some((t) => sameTag(t, 'research')) ? 'assistant' : 'own'
}

/** How many of `notes` are of each kind. */
export function countOrigins(notes: NoteSummary[]): Record<NoteOrigin, number> {
  const counts: Record<NoteOrigin, number> = { own: 0, assistant: 0, meeting: 0 }
  for (const n of notes) counts[n.origin] += 1
  return counts
}
