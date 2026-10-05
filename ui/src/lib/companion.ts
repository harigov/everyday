// The dog in the assistant's header: which dogs there are, and what the dog
// is doing.
//
// Two lists and two rules, kept apart from the scene that draws them
// (`companion-scene.ts`) for the reason `agent.ts` sits beside its store:
// the drawing is checked by looking at it, and these are not. A breed list
// that drifted from what Settings offers, or a stored word that no longer
// names anything, fails quietly -- the header just draws a different dog
// than the one somebody chose.
//
// The first rule is reading a stored choice. The vault keeps three words (see
// `Companion` in `everyday-core`'s `agent.rs`) and a build is free to meet
// one it has never heard of -- a breed a later build added, on a vault that
// came from there. A word this list does not know is drawn as the default,
// rather than refused, so the worst case is a different dog and never no
// settings at all.
//
// The second is what to act out. The dog is not decoration: it is the one
// thing in the rail's header that shows, at a glance and from across the
// room, that a turn is still working and roughly what at -- nose down while
// it looks things up, at a keyboard while it writes, head tilted while it
// thinks, paw raised while it waits for an answer. Like `activity` in
// `agent.ts`, that is read from the turn as the stream has left it, and
// like `toolLabel` it is derived from a tool's name rather than a table of
// them, so a tool added to the catalogue next month already does something
// sensible.

import type { Turn } from './agent'
import type { Companion } from './types'

export type Breed = 'shiba' | 'corgi' | 'beagle' | 'pug' | 'poodle'
export type Coat = 'honey' | 'apricot' | 'cream' | 'cocoa' | 'charcoal' | 'silver' | 'snow'
export type Markings = 'solid' | 'socks' | 'spots' | 'patches' | 'mask'

/** One dog, fully decided: what the scene is handed. */
export interface Look {
  breed: Breed
  coat: Coat
  markings: Markings
}

interface Choice<T extends string> {
  id: T
  label: string
}

/** The shapes, in the order Settings offers them. The first is the default. */
export const BREEDS: Choice<Breed>[] = [
  { id: 'shiba', label: 'Shiba' },
  { id: 'corgi', label: 'Corgi' },
  { id: 'beagle', label: 'Beagle' },
  { id: 'pug', label: 'Pug' },
  { id: 'poodle', label: 'Poodle' },
]

/**
 * The coats. `base` is the fur; `mark` is what spots and patches are painted
 * in, chosen per coat so they show -- dark on a light coat, light on a dark
 * one, which is why a charcoal dog's spots are cream.
 */
export const COATS: (Choice<Coat> & { base: string; mark: string })[] = [
  { id: 'honey', label: 'Honey', base: '#dd9a5b', mark: '#5b3a26' },
  { id: 'apricot', label: 'Apricot', base: '#efc08e', mark: '#7a4f33' },
  { id: 'cream', label: 'Cream', base: '#f3e2c4', mark: '#8a6248' },
  { id: 'cocoa', label: 'Cocoa', base: '#7d5038', mark: '#f1e1c8' },
  { id: 'charcoal', label: 'Charcoal', base: '#3d3839', mark: '#efe4d2' },
  { id: 'silver', label: 'Silver', base: '#b4b2b6', mark: '#4a474d' },
  { id: 'snow', label: 'Snow', base: '#f7f4ef', mark: '#3a3536' },
]

/** What is painted on the coat. */
export const MARKINGS: Choice<Markings>[] = [
  { id: 'socks', label: 'Bib and socks' },
  { id: 'solid', label: 'Solid' },
  { id: 'spots', label: 'Spots' },
  { id: 'patches', label: 'Patches' },
  { id: 'mask', label: 'Mask' },
]

/** The dog nobody chose: a honey shiba with a white bib and socks. */
export const DEFAULT_LOOK: Look = { breed: 'shiba', coat: 'honey', markings: 'socks' }

function known<T extends string>(list: Choice<T>[], word: string | undefined, fallback: T): T {
  return list.find((c) => c.id === word)?.id ?? fallback
}

/**
 * The dog to draw for a stored choice, or `null` for none.
 *
 * `undefined` -- settings that have not loaded, or a backend older than the
 * field -- is the default dog, the same as the vault's own reading of a
 * record with no key. Only an explicit `null` is no dog.
 */
export function lookOf(companion: Companion | null | undefined): Look | null {
  if (companion === null) return null
  const c = companion ?? {}
  return {
    breed: known(BREEDS, c.breed, DEFAULT_LOOK.breed),
    coat: known(COATS, c.coat, DEFAULT_LOOK.coat),
    markings: known(MARKINGS, c.markings, DEFAULT_LOOK.markings),
  }
}

/** A look as the vault stores it. Every word is written, default or not. */
export function companionOf(look: Look): Companion {
  return { breed: look.breed, coat: look.coat, markings: look.markings }
}

export function coatOf(coat: Coat): (typeof COATS)[number] {
  return COATS.find((c) => c.id === coat) ?? COATS[0]!
}

// ── What it is doing ──────────────────────────────────────────────────

/**
 * Everything the dog can act out.
 *
 * `sniff` is looking something up -- the vault or the web -- and `type` is
 * writing, whether that is the reply or a record. `listen` is the person
 * writing to it: ears up, watching the box. `cheer` and `droop` are the
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
 * What the dog should be doing for the newest turn in the thread, at `now`.
 *
 * Read from the turn's own phase and cards -- the same state `activity`
 * reads for the sentence under the turn -- so the dog and the sentence can
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
 * What the dog in a header does: `actOf`, with the two things around a turn
 * that the turn itself cannot know.
 *
 * An assistant that is not set up has nothing to do, so it sleeps. And one
 * with nothing to do while somebody is typing to it listens -- but only
 * then: a question typed while a reply is still being written does not stop
 * the dog writing it.
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
