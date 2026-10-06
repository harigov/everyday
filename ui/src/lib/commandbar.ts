// The rows the bar at the top of the window offers, and their order.
//
// The bar is the one place in the window to find something or do something.
// It replaced two things that each did half of that: a search field in every
// app's own corner -- five fields, five shapes, in the sidebar in two apps,
// in a header in two more, over the list in the journal -- and a palette that
// opened in a dialog over everything. See `CommandBar.svelte` for how the two
// halves share one field; this module is the part of it that needs no
// window, so the ordering rules can be tested without one.

import type { Binding } from './keys'
import type { IconName } from './icons'

/**
 * One thing the bar can do with what was typed.
 *
 * A single list rather than sections, because the arrow keys move through it
 * as one thing -- a list where the keyboard stops at a boundary is a list
 * people stop using.
 */
export type Row =
  /** Search the open app for the text. */
  | { kind: 'search'; label: string }
  /** An action from the one table in `shortcuts.svelte.ts`. */
  | { kind: 'action'; action: Binding; score: number }
  /** Keep the text as something: a task, a reading, an appointment. */
  | { kind: 'capture'; label: string; icon: IconName; hint: string; run: () => unknown }
  /** Hand the text to the assistant, along with what is on screen. */
  | { kind: 'ask'; label: string }

/**
 * How well `action` matches what has been typed.
 *
 * Tiers rather than a fuzzy distance, because the useful orderings here are
 * coarse and a scored edit distance puts surprising things first. A prefix
 * of the label beats a word inside it, which beats a keyword or the group --
 * so typing "lo" offers "Lock now" before "Library, covers or a list", which
 * is what somebody typing two letters meant. Nothing typed matches
 * everything equally, so the table's own order stands.
 */
export function score(action: Binding, needle: string): number {
  const n = needle.trim().toLowerCase()
  if (!n) return 1
  const label = action.label.toLowerCase()
  if (label.startsWith(n)) return 4
  if (label.split(/\s+/).some((word) => word.startsWith(n))) return 3.5
  if (label.includes(n)) return 3
  if (action.keywords?.some((k) => k.includes(n))) return 2
  if (action.group.toLowerCase().includes(n)) return 1
  return 0
}

/** The matching actions, best first, the table's order breaking ties. */
export function rankActions(actions: Binding[], needle: string, limit = Infinity): Row[] {
  return actions
    .map((action, i) => ({ action, score: score(action, needle), i }))
    .filter((r) => r.score > 0)
    .sort((a, b) => b.score - a.score || a.i - b.i)
    .slice(0, limit)
    .map(({ action, score }) => ({ kind: 'action' as const, action, score }))
}

/** The heading a row is filed under in the dropdown. */
export function headingOf(row: Row): string {
  switch (row.kind) {
    case 'search':
      return 'Search'
    case 'action':
      return row.action.group
    case 'capture':
      return 'Keep what you typed'
    case 'ask':
      return 'Ask'
  }
}

/** A key for `{#each}` that is stable while somebody types. */
export function keyOf(row: Row): string {
  switch (row.kind) {
    case 'action':
      return `a:${row.action.group}:${row.action.label}`
    case 'capture':
      return `c:${row.icon}:${row.label.split(':')[0]}`
    default:
      return row.kind
  }
}
