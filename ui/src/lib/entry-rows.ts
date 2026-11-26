// Flattening the journal's grouped entry list into rows a `VirtualList` can
// draw.
//
// The list groups entries under a date heading -- "Today", "Yesterday",
// "March" -- and a virtual list wants one flat array with a stable key per
// row, not a list of groups each holding a list of its own. So a heading
// becomes a row of its own kind, sitting in the flat array between the
// entries it heads, exactly where `EntryList.svelte` used to nest it inside
// a `{#each group.rows}` of its own.

import { groupLabel } from './format'
import type { EntryId, EntrySummary } from './types'

export type EntryRow =
  | { kind: 'header'; key: string; label: string }
  | { kind: 'entry'; key: EntryId; entry: EntrySummary }

/**
 * `entries`, already sorted by the backend, split at every change of
 * `groupLabel` and interleaved with a header row for each run.
 *
 * A function rather than a `$derived` inside the component, so the grouping
 * rule -- the part worth getting right -- is testable without a store, a
 * component, or a rendered list.
 *
 * The header's key is the id of the entry that opens its group rather than
 * the label itself: two runs of entries can share a label -- "Later" if a
 * future-dated entry and an even-later one are not adjacent -- and a key
 * that repeats is a row Svelte's keyed `{#each}` (and `VirtualList`, which
 * uses the same key to remember scroll and measured height) would conflate.
 */
export function toRows(entries: EntrySummary[]): EntryRow[] {
  const rows: EntryRow[] = []
  let lastLabel: string | null = null
  for (const entry of entries) {
    const label = groupLabel(entry.localDate)
    if (label !== lastLabel) {
      rows.push({ kind: 'header', key: `header:${entry.id}`, label })
      lastLabel = label
    }
    rows.push({ kind: 'entry', key: entry.id, entry })
  }
  return rows
}

/**
 * Split a search snippet on its highlight ranges so the matches can be
 * marked. Shared by the journal's results and the notes' -- the notes list
 * printed the snippet plain, so a hit never said which word it was.
 *
 * The ranges are UTF-8 byte offsets, the way the search in Rust counts
 * (`search.rs`'s `snippet_for`), and a JavaScript string is indexed in UTF-16
 * units. Sliced as they came, the mark landed two letters late after the
 * leading "…" (three bytes, one unit) and further after any accented letter,
 * or cut a character in half. So each offset is turned into an index first.
 */
export function pieces(text: string, ranges: [number, number][]) {
  const at = indexOfByte(text)
  const out: { text: string; hit: boolean }[] = []
  let done = 0
  for (const [from, to] of ranges) {
    const s = Math.max(at(from), done)
    const e = Math.max(at(to), s)
    if (s > done) out.push({ text: text.slice(done, s), hit: false })
    if (e > s) out.push({ text: text.slice(s, e), hit: true })
    done = e
  }
  if (done < text.length) out.push({ text: text.slice(done), hit: false })
  return out
}

/** Where in `text`, in UTF-16 units, a UTF-8 byte offset falls: at the start
 *  of the character it lands in, and at the end for anything past it. */
function indexOfByte(text: string): (byte: number) => number {
  const starts: [number, number][] = []
  let byte = 0
  let unit = 0
  for (const ch of text) {
    starts.push([byte, unit])
    const cp = ch.codePointAt(0) ?? 0
    byte += cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4
    unit += ch.length
  }
  return (wanted) => {
    if (wanted >= byte) return text.length
    let found = 0
    for (const [b, u] of starts) {
      if (b > wanted) break
      found = u
    }
    return found
  }
}
