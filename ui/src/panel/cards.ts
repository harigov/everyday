// The panel page's decisions, kept apart from the drawing so
// `scripts/panel.test.mjs` can check them without a window: which cards are
// drawn, which of a card's lines, what a press switches off and for how long,
// how the level meter moves, and what height the shell is told.
//
// Nothing here knows about the shell. It takes the cards `panel.ts` describes
// and the time it is told, and answers.
import type { Panel, PanelAction, PanelCheck, PanelLine, PanelSection, PanelTone } from './panel'

/** The most cards drawn at once: `MAX_SHOWN` in `crates/karmayoga-app/src/panel/mod.rs`. */
export const MAX_SHOWN = 3

/** How many of a card's lines are drawn: the newest. */
export const MAX_LINES = 3

/** How long a press keeps its card's buttons off when the card does not change first. */
export const LOCK_MS = 4000

// ── which cards, which lines ──────────────────────────────────────────────

export interface Visible {
  /** The cards drawn, in the order the shell sent them. */
  shown: Panel[]
  /** How many more wait in the stack, undrawn, until one above them goes. */
  more: number
}

export function visible(stack: readonly Panel[], max = MAX_SHOWN): Visible {
  return { shown: stack.slice(0, max), more: Math.max(0, stack.length - max) }
}

/** The quiet line under the cards when some wait. */
export function moreLabel(more: number): string {
  return `+${more} more`
}

/**
 * The lines of a card that are drawn: the last few, newest at the bottom. A
 * line with nothing in it is only worth drawing while it is still coming --
 * voice typing starts with one, and its caret is how the card says it is
 * waiting for words.
 */
export function lastLines(lines: readonly PanelLine[] | undefined, max = MAX_LINES): PanelLine[] {
  return (lines ?? []).filter((line) => line.pending || line.text.trim() !== '').slice(-max)
}

/** A count -- "12s", "1:05", "5 min" -- as the number and what it is a number of. */
export function splitCount(count: string): { value: string; unit: string } {
  const found = /^\s*(\d[\d:.,]*)\s*(.*?)\s*$/.exec(count)
  if (!found) return { value: count.trim(), unit: '' }
  const [, value = count, unit = ''] = found
  return { value, unit }
}

/** The lists a card draws: those with something in them, and the folded ones only once opened. */
export function drawnSections(
  sections: readonly PanelSection[] | undefined,
  open: boolean,
): PanelSection[] {
  return (sections ?? []).filter((s) => s.items.length > 0 && (open || !s.folded))
}

/** Whether a card has lists folded away, which is what earns it a "More". */
export function hasFolded(sections: readonly PanelSection[] | undefined): boolean {
  return (sections ?? []).some((s) => s.folded && s.items.length > 0)
}

/**
 * How tall one card's lists may grow before they scroll, so a long brief on a
 * short screen never pushes the buttons below the edge of it. Each card is
 * given an equal share of what the screen has left after the window's margins,
 * the stack's padding and the rest of the card (its heading, lines and buttons,
 * about as tall as `CHROME`).
 */
export function listsBudget(screenHeight: number, cards: number): number {
  const MOST = 360
  const EDGES = 32
  const PADDING = 20
  const GAP = 8
  const CHROME = 140
  // A webview that will not say how tall the screen is gets the roomiest answer.
  if (!(screenHeight > 0)) return MOST
  const n = Math.max(1, cards)
  const share = (screenHeight - EDGES - PADDING - GAP * (n - 1)) / n
  return Math.max(96, Math.min(MOST, Math.floor(share - CHROME)))
}

// ── tone ──────────────────────────────────────────────────────────────────

/** The colour a tone is drawn in: the kicker, the mark beside it, the meter. */
export function toneColour(tone: PanelTone): string {
  switch (tone) {
    case 'trouble':
      return 'var(--danger)'
    case 'neutral':
      return 'var(--fg-muted)'
    default:
      return 'var(--accent)'
  }
}

/** The mark beside the kicker. */
export type ToneMark = 'none' | 'dot' | 'check' | 'alert'

export function toneMark(tone: PanelTone): ToneMark {
  switch (tone) {
    case 'neutral':
      return 'none'
    case 'success':
      return 'check'
    case 'trouble':
      return 'alert'
    default:
      return 'dot'
  }
}

// ── presses ───────────────────────────────────────────────────────────────

/** What a press of a card's button remembers: which buttons were there, and when. */
export interface Press {
  signature: string
  at: number
}

/** What a tick of a box remembers: the state it was in when pressed, and when. */
export interface CheckPress {
  done: boolean
  at: number
}

/**
 * A card's buttons in the order they are drawn, left to right: as listed,
 * except that the primary one goes last, rightmost -- where a desktop's dialogs
 * put the button that goes ahead. The shell lists the primary first, as the most
 * important; `drawn_order` in `crates/karmayoga-app/src/panel/mod.rs` is the
 * same rule for the GTK card.
 */
export function drawnOrder(actions: readonly PanelAction[] | undefined): PanelAction[] {
  const list = actions ?? []
  return [...list.filter((a) => !a.primary), ...list.filter((a) => a.primary)]
}

/** The card's identity as its buttons see it: its id and what it offers to press. */
export function signature(card: Pick<Panel, 'id' | 'actions'>): string {
  const actions = (card.actions ?? []).map((a) => `${a.id}\u0000${a.label}`)
  return [card.id, ...actions].join('\u0001')
}

/**
 * Whether a card's buttons are off: it was pressed lately and has not changed
 * since. A press is answered by the shell changing the card -- a different
 * title, different buttons, gone -- or not at all, and in that case the
 * buttons come back after `LOCK_MS`, so one that did nothing can be pressed
 * again. A card re-sent with the same buttons -- a countdown ticking -- is not
 * a change.
 */
export function isLocked(
  press: Press | undefined,
  card: Pick<Panel, 'id' | 'actions'>,
  now: number,
): boolean {
  return press !== undefined && now - press.at < LOCK_MS && press.signature === signature(card)
}

/** The presses still in force for the stack as it is. The same object when none has ended. */
export function settlePresses(
  presses: Record<string, Press>,
  stack: readonly Panel[],
  now: number,
): Record<string, Press> {
  const kept: Record<string, Press> = {}
  for (const card of stack) {
    const press = presses[card.id]
    if (press && isLocked(press, card, now)) kept[card.id] = press
  }
  return Object.keys(kept).length === Object.keys(presses).length ? presses : kept
}

/** Where a box's press is kept: the card, and the action the box sends. */
export function checkKey(id: string, action: string): string {
  return `${id}\u0001${action}`
}

/**
 * How a box is drawn. Ticked, it sends its action and shows the other state at
 * once, switched off, until the shell has answered by changing it -- or
 * `LOCK_MS` has passed and it must be taken that nothing happened.
 */
export function checkView(
  check: PanelCheck,
  press: CheckPress | undefined,
  now: number,
): { done: boolean; locked: boolean } {
  if (press && press.done === check.done && now - press.at < LOCK_MS) {
    return { done: !check.done, locked: true }
  }
  return { done: check.done, locked: false }
}

/** The box presses still in force for the stack as it is. The same object when none has ended. */
export function settleChecks(
  checks: Record<string, CheckPress>,
  stack: readonly Panel[],
  now: number,
): Record<string, CheckPress> {
  const kept: Record<string, CheckPress> = {}
  for (const card of stack) {
    for (const section of card.sections ?? []) {
      for (const item of section.items) {
        if (!item.check) continue
        const key = checkKey(card.id, item.check.action)
        const press = checks[key]
        if (press && checkView(item.check, press, now).locked) kept[key] = press
      }
    }
  }
  return Object.keys(kept).length === Object.keys(checks).length ? checks : kept
}

/** When the first press in any of these runs out, or null when none is waiting. */
export function nextExpiry(...records: Array<Record<string, { at: number }>>): number | null {
  let soonest: number | null = null
  for (const record of records) {
    for (const press of Object.values(record)) {
      const ends = press.at + LOCK_MS
      if (soonest === null || ends < soonest) soonest = ends
    }
  }
  return soonest
}

/**
 * Put `text` on the clipboard, and say whether it went. Only the page can know:
 * on Wayland the clipboard belongs to the window that was clicked, and a
 * button that says it copied must have. Asked synchronously from the click,
 * before anything is awaited, so the webview still counts it as the person's
 * doing.
 */
export async function tryCopy(
  text: string,
  clipboard: Pick<Clipboard, 'writeText'> | undefined,
): Promise<boolean> {
  if (!clipboard) return false
  try {
    await clipboard.writeText(text)
    return true
  } catch {
    return false
  }
}

/** Whether pressing `action` has the page copy something first. */
export function copies(action: PanelAction): action is PanelAction & { copy: string } {
  return typeof action.copy === 'string'
}

// ── what is kept by card ──────────────────────────────────────────────────

/**
 * `record` without the entries of cards that are not in the stack -- the
 * folded lists a person opened, the meter's history -- so a card that comes
 * back starts as new. The same object when nothing goes.
 */
export function keepCards<T>(
  record: Record<string, T>,
  stack: readonly Pick<Panel, 'id'>[],
): Record<string, T> {
  const ids = new Set(stack.map((p) => p.id))
  const kept = Object.entries(record).filter(([key]) => ids.has(key))
  return kept.length === Object.keys(record).length ? record : Object.fromEntries(kept)
}

/** `record` without `key`. */
export function without<T>(record: Record<string, T>, key: string): Record<string, T> {
  return Object.fromEntries(Object.entries(record).filter(([k]) => k !== key))
}

// ── the level meter ───────────────────────────────────────────────────────

/** How many bars the meter has. */
export const BARS = 9

/** The level history to start with: silence. */
export function silence(): number[] {
  return Array.from({ length: BARS }, () => 0)
}

/**
 * The meter after one more reading: the oldest bar drops off, and the new one
 * rises quickly and falls slowly, so speech reads as speech and not as flicker.
 */
export function pushLevel(history: readonly number[], level: number): number[] {
  const last = history[history.length - 1] ?? 0
  const clean = Number.isFinite(level) ? Math.max(0, Math.min(1, level)) : 0
  const next = clean > last ? last + (clean - last) * 0.7 : last + (clean - last) * 0.3
  return [...history.slice(-(BARS - 1)), next]
}

/** How tall a bar is, as a fraction of the meter: never nothing, and speech lifts it. */
export function barScale(level: number): number {
  const lifted = Math.sqrt(Math.max(0, Math.min(1, level)))
  return 0.16 + 0.84 * lifted
}

/** Nothing said lately: the meter breathes instead of sitting still. */
export function isQuiet(history: readonly number[]): boolean {
  return history.every((level) => level < 0.02)
}

// ── telling the shell how tall the cards are ──────────────────────────────

/** A measured height as the window's: whole pixels, rounded up so nothing is cut off. */
export function roundHeight(measured: number): number {
  if (!Number.isFinite(measured) || measured <= 0) return 0
  // Layout reports 99.99999 for a hundred pixels; that is a hundred.
  return Math.ceil(Math.round(measured * 100) / 100)
}

/**
 * What to tell the shell about a measurement, given what it was last told:
 * nothing when it is the same. A page with no cards says nothing and forgets
 * what it said -- the window is hidden then, and made its default size again
 * when it next opens, so the next stack must be reported even if it is as
 * tall as the last.
 */
export function reportHeight(
  last: number | null,
  measured: number,
): { last: number | null; send: number | null } {
  const height = roundHeight(measured)
  if (height === 0) return { last: null, send: null }
  if (height === last) return { last, send: null }
  return { last: height, send: height }
}
