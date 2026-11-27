// Young Jatayu in the assistant's header: what it can look like, and what it
// is doing.
//
// One list and two rules, kept apart from the scene that draws them
// (`companion-scene.ts`) for the reason `agent.ts` sits beside its store:
// the drawing is checked by looking at it, and these are not. A list that
// drifted from what Settings offers, or a stored word that no longer names
// anything, fails quietly -- the header just draws a different bird than the
// one somebody chose.
//
// The first rule is reading a stored choice. The vault keeps one word (see
// `Companion` in `jatayu-core`'s `agent/records.rs`) and a build is free
// to meet one it has never heard of -- a plumage a later build added, on a
// vault that came from there, or what an earlier build stored when this was a
// dog with a breed and a coat. A word this list does not know is drawn as the
// default, rather than refused, so the worst case is a different colour and
// never no settings at all.
//
// The second is what to act out. The bird is not decoration: it is the one
// thing in the rail's header that shows, at a glance and from across the
// room, that a turn is still working and roughly what at -- head down while
// it looks things up, at a keyboard while it writes, head tilted while it
// thinks, a wing raised while it waits for an answer. Like `activity` in
// `agent.ts`, that is read from the turn as the stream has left it, and
// like `toolLabel` it is derived from a tool's name rather than a table of
// them, so a tool added to the catalogue next month already does something
// sensible.

import type { Turn } from './agent'
import type { Companion } from './types'

/**
 * The colour of its feathers: the one thing about young Jatayu that can be
 * changed. It is one character, not a kennel -- the shape, the crest and the
 * wings are the same bird whatever it is painted in.
 */
export type Plumage = 'gold' | 'ember' | 'bronze' | 'snow' | 'dusk'

/** One bird, fully decided: what the scene is handed. */
export interface Look {
  plumage: Plumage
}

interface Choice<T extends string> {
  id: T
  label: string
}

/**
 * The plumages, in the order Settings offers them. The first is the default.
 *
 * `base` is the down of the head and body, and what a swatch is painted in;
 * `pale` is the face and chest; `wing` is the stack of flight feathers, from
 * the one in front to the one at the back -- the same step of tones the
 * application's icon is drawn in; `deep` is the crest's tips and the tail.
 */
export const PLUMAGES: (Choice<Plumage> & {
  base: string
  pale: string
  wing: [string, string]
  deep: string
})[] = [
  {
    id: 'gold',
    label: 'Gold',
    base: '#f6bb45',
    pale: '#fff1d2',
    wing: ['#fff0bd', '#ff7a24'],
    deep: '#f08a1f',
  },
  {
    id: 'ember',
    label: 'Ember',
    base: '#f08a4a',
    pale: '#ffe9d2',
    wing: ['#ffd9a3', '#e2452f'],
    deep: '#d9482c',
  },
  {
    id: 'bronze',
    label: 'Bronze',
    base: '#9a6a44',
    pale: '#f3e1c6',
    wing: ['#edc58a', '#6a3f24'],
    deep: '#6f4226',
  },
  {
    id: 'snow',
    label: 'Snow',
    base: '#f7f4ee',
    pale: '#ffffff',
    wing: ['#ffffff', '#8fbdf0'],
    deep: '#a9cdf3',
  },
  {
    id: 'dusk',
    label: 'Dusk',
    base: '#7388b6',
    pale: '#e8eefa',
    wing: ['#d3dff6', '#3c4f8c'],
    deep: '#44578f',
  },
]

/** The bird nobody chose: gold, the colour of the icon. */
export const DEFAULT_LOOK: Look = { plumage: 'gold' }

function known<T extends string>(list: Choice<T>[], word: string | undefined, fallback: T): T {
  return list.find((c) => c.id === word)?.id ?? fallback
}

/**
 * The bird to draw for a stored choice, or `null` for none.
 *
 * `undefined` -- settings that have not loaded, or a backend older than the
 * field -- is the default bird, the same as the vault's own reading of a
 * record with no key. Only an explicit `null` is no bird.
 */
export function lookOf(companion: Companion | null | undefined): Look | null {
  if (companion === null) return null
  const c = companion ?? {}
  return { plumage: known(PLUMAGES, c.plumage, DEFAULT_LOOK.plumage) }
}

/** A look as the vault stores it. The word is written, default or not. */
export function companionOf(look: Look): Companion {
  return { plumage: look.plumage }
}

export function plumageOf(plumage: Plumage): (typeof PLUMAGES)[number] {
  return PLUMAGES.find((p) => p.id === plumage) ?? PLUMAGES[0]!
}

// ── What it is doing ──────────────────────────────────────────────────

/**
 * Everything the bird can act out.
 *
 * `sniff` is looking something up -- the vault or the web: head down, peering
 * and pecking about. (The word is from when this was a dog; it is kept
 * because it is only ever a name.) `type` is writing, whether that is the
 * reply or a record. `listen` is the person writing to it: crest up,
 * watching the box. `cheer` and `droop` are the
 * moment after a turn ends, well or badly; `sleep` is an assistant that is
 * not set up, and is also where a long `idle` drifts on its own (see the
 * scene).
 */
export type Act =
  'idle' | 'listen' | 'think' | 'sniff' | 'type' | 'ask' | 'cheer' | 'droop' | 'sleep'

/** How long a turn that answered is celebrated. */
export const CHEER_MS = 2600
/** How long a turn that failed is moped about. Longer: it is worth noticing. */
export const DROOP_MS = 5000

/**
 * Verbs that find things rather than make them, as a tool's name begins.
 *
 * English rather than the catalogue, for `toolLabel`'s reason: the tools
 * change every month and these words do not. Anything else -- create,
 * update, log, send, delete -- is writing something, and is drawn at the
 * keyboard.
 */
const LOOKING = new Set(['list', 'get', 'find', 'search', 'read', 'lookup', 'fetch', 'recall'])

/** Is a call to this tool looking something up? */
export function sniffs(tool: string): boolean {
  const [verb = ''] = tool.split('_')
  return LOOKING.has(verb) || tool.includes('search')
}

/**
 * What the bird should be doing for the newest turn in the thread, at `now`.
 *
 * Read from the turn's own phase and cards -- the same state `activity`
 * reads for the sentence under the turn -- so the bird and the sentence can
 * never disagree about whether something is still running.
 */
export function actOf(turn: Turn | undefined, now: number): Act {
  if (!turn || turn.role !== 'assistant') return 'idle'
  if (turn.phase !== 'done') {
    if (turn.cards.some((c) => c.state === 'waiting')) return 'ask'
    switch (turn.phase) {
      case 'tool': {
        const card = [...turn.cards].reverse().find((c) => c.state === 'running')
        return card ? (sniffs(card.name) ? 'sniff' : 'type') : 'think'
      }
      case 'writing':
        return 'type'
      default:
        // Thinking, and choosing a tool, which is thinking about one.
        return 'think'
    }
  }
  // A stored turn has no end time, so a thread opened from the history list
  // is never celebrated: only a turn this window watched finish.
  if (turn.endedAt === null) return 'idle'
  const since = now - turn.endedAt
  if (turn.error) return since < DROOP_MS ? 'droop' : 'idle'
  // Stopping it is not something to celebrate, or to be sad about.
  if (turn.stopped) return 'idle'
  return since < CHEER_MS ? 'cheer' : 'idle'
}

/**
 * What the bird in a header does: `actOf`, with the two things around a turn
 * that the turn itself cannot know.
 *
 * An assistant that is not set up has nothing to do, so it sleeps. And one
 * with nothing to do while somebody is typing to it listens -- but only
 * then: a question typed while a reply is still being written does not stop
 * the bird writing it.
 */
export function headerAct(
  state: { ready: boolean; composing: boolean; turn: Turn | undefined },
  now: number,
): Act {
  if (!state.ready) return 'sleep'
  const act = actOf(state.turn, now)
  return act === 'idle' && state.composing ? 'listen' : act
}

/**
 * How long until `actOf` gives a different answer by the clock alone, or
 * `null` if only a new event can change it.
 *
 * What lets the header schedule one timer for the end of a cheer instead of
 * re-reading the turn on every frame.
 */
export function actChangesIn(turn: Turn | undefined, now: number): number | null {
  if (!turn || turn.role !== 'assistant' || turn.phase !== 'done' || turn.endedAt === null) {
    return null
  }
  if (turn.stopped && !turn.error) return null
  const until = turn.endedAt + (turn.error ? DROOP_MS : CHEER_MS) - now
  return until > 0 ? until : null
}

// ── When it is drawn ──────────────────────────────────────────────────

/**
 * Is this act a turn at work -- or the moment just after one -- rather than
 * the bird with nothing to do? Those are the acts that say "still going" from
 * across the room, so they are drawn for as long as they last, watched or
 * not. The rest settle and are then held on one frame (see `parks`).
 */
export function working(act: Act): boolean {
  return act !== 'idle' && act !== 'sleep' && act !== 'listen'
}

/**
 * How long a bird with nothing to do keeps moving, in a window somebody is
 * at, before its picture is held: long enough to be seen settling into the
 * new act. After that, drawing a bird that is only sitting, or asleep, thirty
 * times a second all day is a core's worth of work for a picture that hardly
 * changes. In a window nobody is at, it is held as soon as it has come to
 * rest.
 */
export const SETTLE_S = 20

/**
 * Whether the scene can stop drawing and keep the frame it has: nothing to
 * do, the change into this act finished and settled, the bird at rest within
 * it -- eyes open, between two of idle's little fidgets -- and nothing laid
 * over it still moving: a pat, hearts, the head easing back from the pointer.
 */
export function parks(state: {
  /** What it is acting out. */
  act: Act
  /** How far through the change into it, 0 to 1. */
  blend: number
  /** Seconds since it last changed what it was acting out. */
  quietFor: number
  /** Whether anyone is likely to be looking: the window has the focus. */
  watched: boolean
  /** A pat, hearts, the head following the pointer, the crest wobbling. */
  busy: boolean
  /** At rest within the act. */
  resting: boolean
}): boolean {
  return (
    !working(state.act) &&
    state.blend >= 1 &&
    state.quietFor >= (state.watched ? SETTLE_S : 0) &&
    !state.busy &&
    state.resting
  )
}
