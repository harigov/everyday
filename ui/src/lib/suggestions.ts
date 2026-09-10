// Which suggestion chips are still on offer, and what accepting one does.
//
// Pure, and separate from `Suggestions.svelte` for the reason `habits.ts` and
// `balance.ts` are separate from what draws them: these are rules, and the way
// they fail is quiet.
//
// The rule that earns the file is the *round*. The component stays mounted
// across rounds -- the journal's tag row lives in the page header for every
// entry it draws -- so a plain "already taken" set leaks: a `#travel` accepted
// on Monday silently filters `#travel` out of every later suggestion, and a
// reading keyed by its position hides position nought for good. Nothing
// throws, nothing fails to compile, and what somebody sees is the model
// quietly getting worse at its job, which is the hardest kind of bug to
// report. So the keys are stored beside the list they were taken from, and
// tested next door in `scripts/suggestions.test.mjs`.

/** One chip: a stable key, and what it says. */
export interface Suggestion {
  key: string
  label: string
}

/** Accepted keys, and the list they were accepted from. */
export interface Taken {
  round: string
  keys: Set<string>
}

export const NOTHING_TAKEN: Taken = { round: '', keys: new Set() }

/**
 * Identity of a round of suggestions: *which record*, and what was offered
 * about it.
 *
 * `scope` is the parent's answer to "what are these about" -- an entry id, a
 * note id, a task id. It is the load-bearing half and it cannot be derived,
 * which is why it is a prop rather than something this file works out.
 *
 * The chips alone are not enough, and the way that fails is subtle. Half the
 * callers key by position, because a reading has no id until it is written,
 * so Monday's list and Tuesday's are both `0, 1`. Adding the labels fixes
 * that case and still misses the plainest one of all: two entries whose
 * suggested tags happen to be the same list. Accept `#travel` on Monday and
 * `#travel` is silently missing from Tuesday, which reads as the model
 * getting worse rather than as a bug.
 *
 * The labels stay in it as well, so that pressing the button again on a
 * record that has not changed keeps a declined chip declined, while a genuinely
 * different answer about the same record is a fresh offer.
 *
 * Newline as the separator: it occurs in none of a tag, a chip's label, a
 * stringified index or an id, so two rounds cannot collide by one field
 * ending where the next begins.
 */
export function roundOf(items: Suggestion[], scope = ''): string {
  return [scope, ...items.map((i) => `${i.key}\n${i.label}`)].join('\n')
}

/** What is still on offer. */
export function remaining(items: Suggestion[], taken: Taken, scope = ''): Suggestion[] {
  if (taken.round !== roundOf(items, scope)) return items
  return items.filter((i) => !taken.keys.has(i.key))
}

/** Take one, answering the new record. Never mutates what it was given. */
export function take(items: Suggestion[], taken: Taken, key: string, scope = ''): Taken {
  const round = roundOf(items, scope)
  const keys = taken.round === round ? new Set(taken.keys) : new Set<string>()
  keys.add(key)
  return { round, keys }
}

/**
 * Whether taking that one emptied the row.
 *
 * The last chip taken closes it: there is nothing left to look at, and an
 * empty row with a close button on it is furniture.
 */
export function exhausted(items: Suggestion[], taken: Taken, scope = ''): boolean {
  return remaining(items, taken, scope).length === 0
}
