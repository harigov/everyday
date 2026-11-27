// The panel page's decisions, kept apart from the drawing so
// `scripts/panel.test.mjs` can check them without a window: which cards are
// drawn and which of their blocks, how tall those may grow before they
// scroll, which cards are held, what a press switches off and for how long,
// how an action item or a box shows the press before the shell has answered,
// what a box to type in sends and when it puts the text back, how the level
// meter moves, how a clock and an expiry are drawn, whose face is which
// colour, which theme to draw in, and what height the shell is told.
//
// Nothing here knows about the shell or the document. It takes the cards
// `panel.ts` describes and the time it is told, and answers.
import { initialsOf } from '../lib/avatar'
import type { IconName } from '../lib/icons'
import type { SymbolName } from '../lib/symbols'
import { parseThemeChoice, type ThemeId } from '../lib/themes'
import type {
  Decision,
  Panel,
  PanelAction,
  PanelBlock,
  PanelCheck,
  PanelExpiry,
  PanelInput,
  PanelKind,
  PanelMode,
  PanelTone,
  Person,
  Proposal,
  Rsvp,
  ToolIcon,
  Turn,
} from './panel'

/** The most cards drawn at once: `MAX_SHOWN` in `crates/jatayu-app/src/panel/mod.rs`. */
export const MAX_SHOWN = 3

/** How many turns of a conversation are drawn: the newest. */
export const MAX_TURNS = 8

/** How many people a list of them shows before the card is opened. */
export const PEOPLE_SHOWN = 3

/** How many faces an event shows in its row before "+N". */
export const FACES_SHOWN = 6

/** How long a press keeps its card's buttons off when the card does not change first. */
export const LOCK_MS = 4000

// ── which cards ───────────────────────────────────────────────────────────

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

/** A count -- "12s", "1:05", "5 min" -- as the number and what it is a number of. */
export function splitCount(count: string): { value: string; unit: string } {
  const found = /^\s*(\d[\d:.,]*)\s*(.*?)\s*$/.exec(count)
  if (!found) return { value: count.trim(), unit: '' }
  const [, value = count, unit = ''] = found
  return { value, unit }
}

/**
 * Whether `text` says what `title` already says -- an event block under a
 * meeting card whose title is the event's, a note under "Notes from …" --
 * so the block can leave it out rather than print the same words twice.
 */
export function repeats(text: string | undefined, title: string): boolean {
  if (text === undefined) return false
  const plain = (s: string) => s.trim().replace(/\s+/g, ' ').toLocaleLowerCase()
  return plain(text) !== '' && plain(text) === plain(title)
}

// ── which blocks ──────────────────────────────────────────────────────────

/**
 * The turns of a conversation that are drawn: the last few, newest at the
 * bottom. A turn with nothing in it is only worth drawing while it is still
 * coming -- voice typing starts with one, and its caret is how the card says
 * it is waiting for words; the assistant starts with one, and its dots are
 * how the card says an answer is on its way.
 */
export function lastTurns(turns: readonly Turn[] | undefined, max = MAX_TURNS): Turn[] {
  return (turns ?? []).filter((turn) => turn.pending || turn.text.trim() !== '').slice(-max)
}

/**
 * Whether the turn at `i` carries its speaker's name: the first of a run by
 * the same speaker does, the rest of the run do not, so a long answer in
 * several turns reads as one voice.
 */
export function showsName(turns: readonly Turn[], i: number): boolean {
  const turn = turns[i]
  const before = turns[i - 1]
  if (!turn) return false
  return !before || before.who !== turn.who || before.name !== turn.name
}

/** A block with nothing to draw: an empty list, a conversation with no words yet to show. */
export function isEmpty(block: PanelBlock): boolean {
  switch (block.type) {
    case 'conversation':
      return lastTurns(block.turns).length === 0
    case 'people':
      return block.people.length === 0
    case 'list':
    case 'proposals':
      return block.items.length === 0
    case 'choices':
      return block.choices.length === 0
    case 'text':
    case 'quote':
      return block.text.trim() === ''
    default:
      return false
  }
}

/** A block kept under "More" until the card is opened. */
export function isFolded(block: PanelBlock): boolean {
  return (block.type === 'people' || block.type === 'text' || block.type === 'list') && block.folded
}

/** The blocks a card draws: those with something in them, and the folded ones only once opened. */
export function drawnBlocks(
  blocks: readonly PanelBlock[] | undefined,
  open: boolean,
): PanelBlock[] {
  return (blocks ?? []).filter((block) => !isEmpty(block) && (open || !isFolded(block)))
}

/**
 * Whether opening the card would show more: a block folded away, or a list of
 * people longer than it shows closed. That is what earns it a "More".
 */
export function hasFolded(blocks: readonly PanelBlock[] | undefined): boolean {
  return (blocks ?? []).some(
    (block) =>
      !isEmpty(block) &&
      (isFolded(block) || (block.type === 'people' && block.people.length > PEOPLE_SHOWN)),
  )
}

/** The people a list shows: the first few, or everyone once the card is opened. */
export function peopleShown(people: readonly Person[], open: boolean): Person[] {
  return open ? [...people] : people.slice(0, PEOPLE_SHOWN)
}

/** A block with a key that survives the blocks around it changing. */
export interface Keyed {
  key: string
  block: PanelBlock
}

/**
 * A card's blocks in the three places they are drawn:
 *
 * - `top`, under the head and never scrolled away: what is happening now
 *   (`activity`) and for how long (`clock`) -- drawn on one line together;
 * - `body`, everything else, which scrolls when a long brief would push the
 *   buttons off the screen;
 * - `pinned`, under the body and over the buttons: the answers to pick
 *   (`choices`) and the box to type in (`input`), which must stay in reach
 *   however long what is above them grows -- and which sit where a reply
 *   goes, under what it replies to.
 *
 * Each keeps the order the shell gave. A box is keyed by its id, so what is
 * typed in it is not lost when the shell adds a block above it; the rest by
 * their type and how many of that type came before, so a new activity line
 * does not make every list below it start again.
 */
export function splitBlocks(blocks: readonly PanelBlock[]): {
  top: Keyed[]
  body: Keyed[]
  pinned: Keyed[]
} {
  const top: Keyed[] = []
  const body: Keyed[] = []
  const pinned: Keyed[] = []
  const seen: Record<string, number> = {}
  for (const block of blocks) {
    const n = seen[block.type] ?? 0
    seen[block.type] = n + 1
    const key = block.type === 'input' ? `input:${block.id}` : `${block.type}:${n}`
    const keyed = { key, block }
    if (block.type === 'activity' || block.type === 'clock') top.push(keyed)
    else if (block.type === 'input' || block.type === 'choices') pinned.push(keyed)
    else body.push(keyed)
  }
  return { top, body, pinned }
}

/**
 * Whether a card's scrolling part keeps to its foot as it grows: when it ends
 * in a conversation, whose newest words are at the bottom.
 */
export function sticksToBottom(body: readonly Keyed[]): boolean {
  return body.at(-1)?.block.type === 'conversation'
}

/**
 * The most of the screen's work area -- `screen.availHeight` to the page --
 * the window is made: `MOST_OF_AREA` in
 * `crates/jatayu-app/src/panel/window.rs`. A stack taller than that
 * scrolls, as a whole, inside the window.
 */
export const MOST_OF_AREA = 0.8

/**
 * How tall one card's blocks may grow before they scroll, so a long brief on a
 * short screen never pushes the buttons and the box below the edge of the
 * window. Each card is given an equal share of the most the window can be
 * (`MOST_OF_AREA` of the screen) after the stack's padding and gaps, less the
 * rest of that card -- its head, its box and its buttons, `chrome`
 * (`cardChrome`). Never so little that a block cannot be read; when even that
 * is too much, the stack scrolls.
 */
export function blocksBudget(screenHeight: number, cards: number, chrome: number): number {
  const MOST = 440
  const LEAST = 96
  /** The stack's padding, above and below: `.cards.up` in `PanelStack.svelte`. */
  const PADDING = 32
  const GAP = 10
  // A webview that will not say how tall the screen is gets the roomiest answer.
  if (!(screenHeight > 0)) return MOST
  const n = Math.max(1, cards)
  const share = (screenHeight * MOST_OF_AREA - PADDING - GAP * (n - 1)) / n
  const rest = Number.isFinite(chrome) ? Math.max(0, chrome) : 0
  return Math.max(LEAST, Math.min(MOST, Math.floor(share - rest)))
}

/**
 * About how tall a card is without its scrolling blocks, in pixels: its
 * padding and head, the line of what is happening now, what is pinned over
 * its buttons (a box to type in, with its ways to use it, its ready-made
 * replies and its error; answers to pick), and the buttons -- with the gaps
 * between them. Read from what the card holds rather than measured, so it is
 * the same before the card is drawn as after; a little generous, as the
 * stack scrolls rather than cut a card off when a guess is short. The sizes
 * are those `PanelCard.svelte` and `blocks/` draw.
 */
export function cardChrome(card: Pick<Panel, 'body' | 'blocks' | 'actions'>): number {
  /** Between the parts of a card: `--sp-3`. */
  const GAP = 12
  // The card's padding (12 above, 14 below), the mast (20) and the gap under
  // it, and the title beside its symbol -- with two lines of body under it,
  // where there is a body.
  let height = 26 + 28 + (card.body ? 64 : 40)
  const { top, body, pinned } = splitBlocks(drawnBlocks(card.blocks, true))
  if (top.length > 0) {
    // The activity pill (and the tool at work under it), or a clock's two lines.
    const tool = top.some(({ block }) => block.type === 'activity' && block.tool !== undefined)
    height += GAP + (tool ? 56 : 36)
  }
  if (body.length > 0) height += GAP
  for (const { block } of pinned) {
    height += GAP
    if (block.type === 'input') {
      height += 40
      if ((block.modes?.length ?? 0) > 1) height += 36
      if ((block.suggestions?.length ?? 0) > 0) height += 44
      if (block.error) height += 26
    } else if (block.type === 'choices') {
      // Two chips to a row, as most answers are short.
      const rows = Math.ceil(block.choices.length / 2)
      height += (block.prompt ? 26 : 0) + rows * 36
    }
  }
  if ((card.actions?.length ?? 0) > 0 || hasFolded(card.blocks)) height += GAP + 32
  return height
}

// ── how a card looks ──────────────────────────────────────────────────────

/** The app a card's kind belongs to: the symbol in its head and the colour it is drawn in. */
export interface Look {
  symbol: SymbolName
  /** A CSS colour: that app's `--app-*`. */
  colour: string
}

/**
 * A card's kind as the app it comes from, so a meeting reads as the
 * calendar's, a message as mail's, the assistant as the assistant's -- the
 * same symbol and colour as that app's button in the main window.
 */
export function kindLook(kind: PanelKind): Look {
  switch (kind) {
    case 'meeting':
    case 'invite':
      return { symbol: 'calendar', colour: 'var(--app-calendar)' }
    case 'mail':
      return { symbol: 'mail', colour: 'var(--app-mail)' }
    case 'notes':
      return { symbol: 'note', colour: 'var(--app-notes)' }
    case 'assistant':
      return { symbol: 'wing', colour: 'var(--app-assistant)' }
    case 'voiceTyping':
      return { symbol: 'mic', colour: 'var(--app-assistant)' }
    default:
      return { symbol: 'compass', colour: 'var(--app-overview)' }
  }
}

/**
 * The colour a tone is drawn in: the kicker, the ring of a card that needs the
 * person. A card going about its business is drawn in its own app's colour
 * (`--kind`, set on the card from `kindLook`).
 */
export function toneColour(tone: PanelTone): string {
  switch (tone) {
    case 'trouble':
      return 'var(--danger)'
    case 'success':
      return 'var(--success)'
    case 'attention':
      return 'var(--accent)'
    case 'neutral':
      return 'var(--fg-muted)'
    default:
      return 'var(--kind)'
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

/** The marks the page draws itself, for the few things the app's line icons have no drawing of. */
export type GlyphName = IconName | 'open' | 'join' | 'send'

const ACTION_GLYPHS: Record<string, GlyphName> = {
  reply: 'reply',
  archive: 'archive',
  read: 'eye',
  open: 'open',
  close: 'close',
  check: 'tick',
  x: 'close',
  copy: 'copy',
  join: 'join',
  mic: 'mic',
  micOff: 'mic-off',
  stop: 'stop',
  send: 'send',
  sparkle: 'sparkle',
  later: 'clock',
}

/** A button's mark, from the fixed set `PanelAction.icon` names; none for one it does not know. */
export function actionGlyph(icon: string | undefined): GlyphName | null {
  if (icon === undefined || !Object.hasOwn(ACTION_GLYPHS, icon)) return null
  return ACTION_GLYPHS[icon] ?? null
}

/** A tool at work as the app it touches: its symbol and colour. */
export function toolLook(icon: ToolIcon): Look {
  switch (icon) {
    case 'mail':
      return { symbol: 'mail', colour: 'var(--app-mail)' }
    case 'calendar':
      return { symbol: 'calendar', colour: 'var(--app-calendar)' }
    case 'tasks':
      return { symbol: 'check', colour: 'var(--app-todo)' }
    case 'notes':
      return { symbol: 'note', colour: 'var(--app-notes)' }
    case 'journal':
      return { symbol: 'notebook', colour: 'var(--app-journal)' }
    case 'web':
      return { symbol: 'globe', colour: 'var(--info)' }
    case 'files':
      return { symbol: 'folder', colour: 'var(--app-files)' }
    case 'image':
      return { symbol: 'image', colour: 'var(--app-library)' }
    case 'typing':
      return { symbol: 'pencil', colour: 'var(--app-assistant)' }
    case 'memory':
      return { symbol: 'layers', colour: 'var(--app-assistant)' }
    case 'money':
      return { symbol: 'wallet', colour: 'var(--app-money)' }
    case 'health':
      return { symbol: 'heart-pulse', colour: 'var(--app-health)' }
    case 'news':
      return { symbol: 'rss', colour: 'var(--app-feed)' }
    default:
      return { symbol: 'sparkle', colour: 'var(--app-assistant)' }
  }
}

/**
 * How an activity's state is shown beside its word: bars that move with a
 * voice, dots that wait on a thought, a wave while speaking, the red dot of a
 * recording, a slow pulse while waiting for the person -- or a still mark.
 */
export type Motion = 'bars' | 'dots' | 'wave' | 'rec' | 'pulse' | 'none'

export interface ModeLook {
  /** What the mode is called when the shell gives no label of its own. */
  word: string
  motion: Motion
  /** A still mark, for a mode that does not move. */
  glyph: GlyphName | null
  /** A CSS colour. `var(--kind)` is the card's own app colour. */
  colour: string
}

export function modeLook(mode: PanelMode): ModeLook {
  const look = (word: string, motion: Motion, colour = 'var(--kind)'): ModeLook => ({
    word,
    motion,
    glyph: null,
    colour,
  })
  switch (mode) {
    case 'starting':
      return look('Starting', 'dots')
    case 'listening':
      return look('Listening', 'bars')
    case 'hearing':
      return look('Hearing you', 'bars')
    case 'thinking':
      return look('Thinking', 'dots')
    case 'working':
      return look('Working', 'dots')
    case 'speaking':
      return look('Speaking', 'wave')
    case 'dictating':
      return look('Typing what you say', 'bars')
    case 'muted':
      return { word: 'Muted', motion: 'none', glyph: 'mic-off', colour: 'var(--fg-muted)' }
    case 'waiting':
      return look('Waiting for you', 'pulse', 'var(--accent)')
    case 'recording':
      return look('Recording', 'rec', 'var(--danger)')
    case 'sending':
      return look('Sending', 'dots')
    case 'done':
      return { word: 'Done', motion: 'none', glyph: 'tick', colour: 'var(--success)' }
    case 'failed':
      return { word: 'Didn’t work', motion: 'none', glyph: 'alert', colour: 'var(--danger)' }
    default:
      return look('Ready', 'none', 'var(--fg-muted)')
  }
}

// ── people ────────────────────────────────────────────────────────────────

/**
 * The colours a face can be: the apps' own, which every theme draws at one
 * lightness and saturation, so a face is never louder than the app symbols
 * beside it and always reads in that theme. Health's rose is left out -- on a
 * face it would read as a warning. Ordered so neighbours differ in hue.
 */
export const FACE_HUES = [
  'var(--app-mail)',
  'var(--app-todo)',
  'var(--app-journal)',
  'var(--app-screentime)',
  'var(--app-calendar)',
  'var(--app-files)',
  'var(--app-assistant)',
  'var(--app-money)',
  'var(--app-feed)',
  'var(--app-notes)',
  'var(--app-library)',
  'var(--app-overview)',
] as const

/** FNV-1a, 32-bit, as `lib/avatar.ts` uses for the same job: stable across machines. */
function hash(text: string): number {
  let h = 0x811c9dc5
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i)
    h = Math.imul(h, 0x01000193)
  }
  return h >>> 0
}

/** The colour `key` -- an address, or a name where there is none -- is always drawn in. */
export function faceHue(key: string): string {
  return FACE_HUES[hash(key.trim().toLowerCase()) % FACE_HUES.length] ?? FACE_HUES[0]
}

/** A person's face: their initials on their colour, the same for an address everywhere. */
export function face(person: Pick<Person, 'name' | 'email'>): { initials: string; hue: string } {
  const email = person.email ?? ''
  return { initials: initialsOf(person.name, email), hue: faceHue(email || person.name) }
}

/** The faces an event shows -- the organiser first, then as given -- and how many more. */
export function faces(
  people: readonly Person[],
  max = FACES_SHOWN,
): { shown: Person[]; more: number } {
  const ordered = [...people.filter((p) => p.organizer), ...people.filter((p) => !p.organizer)]
  return { shown: ordered.slice(0, max), more: Math.max(0, ordered.length - max) }
}

/** How an answer to an invitation is marked: a tick, a cross, a question, or still to come. */
export interface RsvpLook {
  mark: 'yes' | 'no' | 'maybe' | 'waiting'
  word: string
}

export function rsvpLook(status: Rsvp | undefined): RsvpLook | null {
  switch (status) {
    case 'accepted':
      return { mark: 'yes', word: 'Going' }
    case 'declined':
      return { mark: 'no', word: 'Not going' }
    case 'tentative':
      return { mark: 'maybe', word: 'Maybe' }
    case 'needsAction':
      return { mark: 'waiting', word: 'No answer yet' }
    default:
      return null
  }
}

/**
 * Who has answered, in a line under the faces: "3 going · 1 maybe · 2 no
 * answer". The person's own answer is left out -- it is shown on its own,
 * as theirs. Null when nobody's answer is known.
 */
export function rsvpSummary(people: readonly Person[]): string | null {
  const count = (status: Rsvp) => people.filter((p) => !p.you && p.status === status).length
  const parts = [
    [count('accepted'), 'going'],
    [count('tentative'), 'maybe'],
    [count('declined'), 'not going'],
    [count('needsAction'), 'no answer'],
  ] as const
  const said = parts.filter(([n]) => n > 0).map(([n, word]) => `${n} ${word}`)
  return said.length > 0 ? said.join(' · ') : null
}

/** The person's own answer to an invitation, said to them. */
export function yourAnswer(rsvp: Rsvp): { mark: RsvpLook['mark']; words: string } {
  switch (rsvp) {
    case 'accepted':
      return { mark: 'yes', words: 'You’re going' }
    case 'declined':
      return { mark: 'no', words: 'You declined' }
    case 'tentative':
      return { mark: 'maybe', words: 'You said maybe' }
    default:
      return { mark: 'waiting', words: 'You haven’t answered' }
  }
}

/** "1 attachment", "3 attachments". */
export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`
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

/** What a decision on an action item remembers: where it stood, where it was sent, and when. */
export interface DecisionPress {
  from: Decision
  to: Decision
  at: number
}

/**
 * A card's buttons in the order they are drawn, left to right: as listed,
 * except that the primary one goes last, rightmost -- where a desktop's dialogs
 * put the button that goes ahead. The shell lists the primary first, as the most
 * important; `drawn_order` in `crates/jatayu-app/src/panel/mod.rs` is the
 * same rule.
 */
export function drawnOrder(actions: readonly PanelAction[] | undefined): PanelAction[] {
  const list = actions ?? []
  return [...list.filter((a) => !a.primary), ...list.filter((a) => a.primary)]
}

/**
 * The card's identity as its buttons see it: its id and what it offers to
 * press -- the buttons along its foot and the answers in its choices.
 */
export function signature(card: Pick<Panel, 'id' | 'actions' | 'blocks'>): string {
  const actions = (card.actions ?? []).map((a) => `${a.id}\u0000${a.label}`)
  const choices = (card.blocks ?? []).flatMap((block) =>
    block.type === 'choices' ? block.choices.map((a) => `choice\u0002${a.id}\u0000${a.label}`) : [],
  )
  return [card.id, ...actions, ...choices].join('\u0001')
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
  card: Pick<Panel, 'id' | 'actions' | 'blocks'>,
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

/** Where something kept by card and by a name within it is kept: a box's press, a box's text. */
export function checkKey(id: string, action: string): string {
  return `${id}\u0001${action}`
}

/** The boxes a card can tick: in its lists, and on its tasks. */
export function checksOf(card: Pick<Panel, 'blocks'>): PanelCheck[] {
  return (card.blocks ?? []).flatMap((block): PanelCheck[] => {
    if (block.type === 'list') {
      return block.items.flatMap((item) => (item.check ? [item.check] : []))
    }
    if (block.type === 'task' && block.check !== undefined) {
      return [{ action: block.check, done: block.done }]
    }
    return []
  })
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
    for (const check of checksOf(card)) {
      const key = checkKey(card.id, check.action)
      const press = checks[key]
      if (press && checkView(check, press, now).locked) kept[key] = press
    }
  }
  return Object.keys(kept).length === Object.keys(checks).length ? checks : kept
}

/** Where a decision on one action item is kept. */
export function decisionKey(id: string, proposal: string): string {
  return checkKey(id, `proposal:${proposal}`)
}

/** What pressing accept or reject on an action item sends: `accept:{id}`, `reject:{id}`. */
export function decisionAction(proposal: string, to: 'accepted' | 'rejected'): string {
  return `${to === 'accepted' ? 'accept' : 'reject'}:${proposal}`
}

/**
 * How an action item is drawn. Decided, it shows the decision at once,
 * switched off, until the shell has answered by changing it -- or `LOCK_MS`
 * has passed and it must be taken that nothing happened.
 */
export function proposalView(
  item: Proposal,
  press: DecisionPress | undefined,
  now: number,
): { state: Decision; locked: boolean } {
  if (press && press.from === item.state && now - press.at < LOCK_MS) {
    return { state: press.to, locked: true }
  }
  return { state: item.state, locked: false }
}

/** The decisions still in force for the stack as it is. The same object when none has ended. */
export function settleDecisions(
  decisions: Record<string, DecisionPress>,
  stack: readonly Panel[],
  now: number,
): Record<string, DecisionPress> {
  const kept: Record<string, DecisionPress> = {}
  for (const card of stack) {
    for (const block of card.blocks ?? []) {
      if (block.type !== 'proposals') continue
      for (const item of block.items) {
        const key = decisionKey(card.id, item.id)
        const press = decisions[key]
        if (press && proposalView(item, press, now).locked) kept[key] = press
      }
    }
  }
  return Object.keys(kept).length === Object.keys(decisions).length ? decisions : kept
}

/** An action-item list's heading: the shell's, or how many there are. */
export function proposalsTitle(heading: string | undefined, count: number): string {
  if (heading !== undefined && heading.trim() !== '') return heading
  return `${count} action ${count === 1 ? 'item' : 'items'}`
}

/** How far the deciding has got, beside the heading. Null before anything is decided. */
export function proposalsTally(states: readonly Decision[]): string | null {
  const decided = states.filter((s) => s !== 'pending').length
  if (decided === 0) return null
  if (decided === states.length) return 'All decided'
  return `${decided} of ${states.length} decided`
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

// ── a box to type in ──────────────────────────────────────────────────────

/** Where a box's text, its mode and what was last sent from it are kept. */
export function inputKey(id: string, input: string): string {
  return checkKey(id, `input:${input}`)
}

/**
 * What the box is for right now: the mode picked above it (the first, before
 * any is), or the box itself where it has no modes. `action` is what a submit
 * sends.
 */
export function inputMode(
  input: Pick<PanelInput, 'id' | 'placeholder' | 'submit' | 'modes'>,
  picked: string | undefined,
): { action: string; placeholder: string; submit: string; mode: string | null } {
  const modes = input.modes ?? []
  const mode = modes.find((m) => m.id === picked) ?? modes[0]
  if (mode) {
    return { action: mode.id, placeholder: mode.placeholder, submit: mode.submit, mode: mode.id }
  }
  return { action: input.id, placeholder: input.placeholder, submit: input.submit, mode: null }
}

/**
 * What a key does in the box. Enter sends -- Shift+Enter is a new line where
 * the box takes several -- and Escape lets the keyboard go back to whatever
 * had it. A key that is part of composing a character with an input method
 * does nothing here: its Enter picks the character.
 */
export function keyAction(
  key: { key: string; shiftKey: boolean; isComposing: boolean },
  multiline: boolean,
): 'send' | 'newline' | 'blur' | null {
  if (key.isComposing) return null
  if (key.key === 'Enter') return key.shiftKey && multiline ? 'newline' : 'send'
  if (key.key === 'Escape') return 'blur'
  return null
}

/** Typed or pasted text as the box takes it: a one-line box has no line breaks. */
export function cleanText(text: string, multiline: boolean): string {
  return multiline ? text : text.replace(/\s*[\r\n]+\s*/g, ' ')
}

/** What a submit remembers, so the text can be put back if the shell says it failed. */
export interface Sent {
  text: string
  at: number
  /** The error the box showed when it was sent: an older failure, not this one's. */
  error?: string
  /** The shell has said it is dealing with it (the box went busy). */
  seen: boolean
  /**
   * The shell has taken it: the owner's `on_panel_action` has returned. An
   * owner that does its work at once -- a note added to a recording -- never
   * makes the box busy, and its box must not wait out `LOCK_MS` before it
   * takes the next; one whose work takes longer has made the box busy by
   * then. Kept until `LOCK_MS` all the same, so a failure said late still
   * puts the words back.
   */
  acked?: boolean
}

/**
 * What became of a submit, given the box as the shell now sends it.
 *
 * - The box is busy: the shell has it. Kept, and any error from before no
 *   longer counts as old.
 * - It shows an error it did not show when the text was sent: that failed,
 *   and the text goes back in the box.
 * - It was busy and is not now, with no new error: it went.
 * - No word either way for `LOCK_MS`: taken as gone. An owner whose work
 *   takes longer says so by making the box busy. (Taken by the shell --
 *   `acked` -- the box is free sooner, but the words can still come back
 *   until then.)
 * - The box is gone: there is nowhere to put the text back.
 */
export function sentAfter(
  sent: Sent,
  input: Pick<PanelInput, 'busy' | 'error'> | undefined,
  now: number,
): { sent: Sent | null; restore: string | null } {
  if (!input) return { sent: null, restore: null }
  if (input.busy) return { sent: { text: sent.text, at: sent.at, seen: true }, restore: null }
  if (input.error !== undefined && input.error !== sent.error) {
    return { sent: null, restore: sent.text }
  }
  if (sent.seen || now - sent.at >= LOCK_MS) return { sent: null, restore: null }
  return { sent, restore: null }
}

/**
 * Whether the box's send is off: the shell is busy with the last, or has not
 * yet taken it -- for `LOCK_MS` at most.
 */
export function inputLocked(
  sent: Sent | undefined,
  input: Pick<PanelInput, 'busy'>,
  now: number,
): boolean {
  if (input.busy) return true
  return sent !== undefined && !sent.seen && !sent.acked && now - sent.at < LOCK_MS
}

/**
 * The submits the page must look at again when their `LOCK_MS` is up -- not
 * yet seen busy, and younger than that: to switch the box's send back on, or
 * to stop being ready to put the words back.
 */
export function waitingSends(sent: Record<string, Sent>, now: number): Record<string, Sent> {
  return Object.fromEntries(Object.entries(sent).filter(([, s]) => !s.seen && now - s.at < LOCK_MS))
}

/** Every box on the cards, by `inputKey`. */
export function inputsOf(stack: readonly Panel[]): Map<string, PanelInput> {
  const found = new Map<string, PanelInput>()
  for (const card of stack) {
    for (const block of card.blocks ?? []) {
      if (block.type === 'input') found.set(inputKey(card.id, block.id), block)
    }
  }
  return found
}

// ── holding a card ────────────────────────────────────────────────────────

/**
 * How long the pointer holds a card after the last sign of it, where the
 * webview has not shown that it follows the pointer (`followsPointer`): long
 * enough to read what a press changed, short enough that a card pressed and
 * left goes by itself as it should.
 */
export const HOVER_LAPSE_MS = 3000

/** A pointer event, as far as holding a card goes. */
export interface PointerSign {
  type: string
  buttons: number
  clientX: number
  clientY: number
}

/**
 * Whether a pointer event shows that the webview follows the pointer over the
 * window -- a move with no button down, to somewhere the pointer was not --
 * and so will hear it leave, and a card the pointer is on can stay held until
 * it does. `last` is where the pointer was last seen.
 *
 * Until one has, a hold by the pointer lapses `HOVER_LAPSE_MS` after the last
 * sign of it. A macOS panel that is not the key window hears nothing of the
 * pointer passing over it -- WebKit follows the pointer only in the key
 * window, where the system's scroll bars are the overlay kind, as they are
 * with a trackpad -- but a press still reaches it (`accept_first_mouse`), and
 * WebKit opens a press with a `pointerover`. No move and no leaving follow, so
 * a card held from a press would never be let go. A move with a button down
 * is a press dragged; one to where the pointer already was is the webview's
 * own, sent after the layout changed under a pointer that did not move.
 * Neither says the pointer is followed.
 */
export function followsPointer(event: PointerSign, last: { x: number; y: number } | null): boolean {
  return (
    event.type === 'pointermove' &&
    event.buttons === 0 &&
    (last === null || last.x !== event.clientX || last.y !== event.clientY)
  )
}

/**
 * The cards with words typed in a box and not yet sent, from the stack's
 * drafts (keyed by `inputKey`). Each is held: a card that went by itself
 * while the person looked something up in another window would take what
 * they had typed with it. Sent or cleared, the card can go again.
 */
export function writingIn(drafts: Record<string, string>): string[] {
  const ids = new Set<string>()
  for (const [key, text] of Object.entries(drafts)) {
    if (text.trim() !== '') ids.add(cardOfKey(key))
  }
  return [...ids]
}

/**
 * What to tell the shell about which cards are held -- the one under the
 * pointer, the one being typed in, those with words waiting in a box -- given
 * what it was last told: each card newly held, and each no longer held that
 * is still up (one that went is forgotten without a word). `held` is what the
 * shell knows afterwards.
 */
export function holdChanges(
  before: readonly string[],
  want: readonly (string | null)[],
  present: readonly string[],
): { held: string[]; changes: Array<[string, boolean]> } {
  const up = new Set(present)
  const held = [...new Set(want.filter((id): id is string => id !== null && up.has(id)))]
  const changes: Array<[string, boolean]> = []
  for (const id of held) if (!before.includes(id)) changes.push([id, true])
  for (const id of before) if (!held.includes(id) && up.has(id)) changes.push([id, false])
  return { held, changes }
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

/** The card a `checkKey` belongs to. */
function cardOfKey(key: string): string {
  return key.split('\u0001')[0] ?? ''
}

/** As `keepCards`, for a record keyed by `checkKey`: a card's id, then a name within it. */
export function keepByCard<T>(
  record: Record<string, T>,
  stack: readonly Pick<Panel, 'id'>[],
): Record<string, T> {
  const ids = new Set(stack.map((p) => p.id))
  const kept = Object.entries(record).filter(([key]) => ids.has(cardOfKey(key)))
  return kept.length === Object.keys(record).length ? record : Object.fromEntries(kept)
}

/** `record` without `key`. */
export function without<T>(record: Record<string, T>, key: string): Record<string, T> {
  return Object.fromEntries(Object.entries(record).filter(([k]) => k !== key))
}

// ── time ──────────────────────────────────────────────────────────────────

/** A running clock from `sinceMs` to `nowMs`: "4:05", or "1:02:03" past the hour. */
export function clockText(sinceMs: number, nowMs: number): string {
  const elapsed = Number.isFinite(sinceMs) && Number.isFinite(nowMs) ? nowMs - sinceMs : 0
  const total = Math.max(0, Math.floor(elapsed / 1000))
  const hours = Math.floor(total / 3600)
  const minutes = Math.floor((total % 3600) / 60)
  const seconds = total % 60
  const two = (n: number) => String(n).padStart(2, '0')
  return hours > 0 ? `${hours}:${two(minutes)}:${two(seconds)}` : `${minutes}:${two(seconds)}`
}

/**
 * How a card's expiry bar is drawn: a CSS animation from full to empty over
 * the whole of `afterMs`, started part of the way through (a negative delay)
 * so it begins at what the shell says is left. `held`: stopped where it is.
 * Null for a card that does not go by itself.
 */
export function expiryView(
  expires: PanelExpiry | undefined,
): { durationMs: number; delayMs: number; fraction: number; held: boolean } | null {
  if (!expires || !(expires.afterMs > 0)) return null
  const told = Number.isFinite(expires.remainingMs) ? expires.remainingMs : expires.afterMs
  const remaining = Math.max(0, Math.min(expires.afterMs, told))
  return {
    durationMs: expires.afterMs,
    delayMs: remaining - expires.afterMs,
    fraction: remaining / expires.afterMs,
    held: expires.held,
  }
}

/**
 * What the expiry bar is started again for: a stack whose copy of the time
 * left is new. A card re-sent while held says the same, and the bar is left
 * where it stopped.
 */
export function expiryKey(expires: PanelExpiry | undefined): string {
  if (!expires) return ''
  return `${expires.afterMs}:${expires.remainingMs}:${expires.held}:${expires.action}`
}

// ── the theme ─────────────────────────────────────────────────────────────

/**
 * The theme to draw in: one forced for a review in a browser (`?theme=`),
 * else the main window's (the shell's, which a child process with no storage
 * of its own can still be told), else the one stored in this origin, else
 * Paper or Ink by the desktop. Anything that is not a theme -- one since
 * removed, `system` -- is passed over.
 */
export function pickTheme(
  forced: string | null,
  shell: string | null,
  stored: string | null,
  dark: boolean,
): ThemeId {
  for (const raw of [forced, shell, stored]) {
    const choice = parseThemeChoice(raw)
    if (choice !== 'system') return choice
  }
  return dark ? 'dark' : 'light'
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
