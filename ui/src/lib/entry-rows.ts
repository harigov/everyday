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
