// The mechanics of a keyboard shortcut. Not the shortcuts themselves.
//
// Those are in `shortcuts.ts`, which reads the stores; this file knows
// nothing about journals or tasks and is therefore the part that can be
// tested without a backend. The split is the same one `menu.ts` makes
// against `menu.svelte.ts`.
//
// # What a shortcut looks like
//
// A binding's `keys` is a space-separated *sequence* of chords, and a chord
// is modifiers and a key joined by `+`:
//
//     'mod+n'      one chord, with the platform's command key
//     'g j'        two chords in a row -- press g, then j
//     '?'          a bare key
//
// The sequence is the Superhuman idea and the reason this module exists. A
// desktop application has four apps to switch between, a create action in
// each, a search in each and a panel to open, and there are not enough
// modifier combinations left that some other program has not already claimed.
// `g` then `j` costs one more keystroke and reads as a sentence -- *go to
// journal* -- which is what makes twenty of them learnable.
//
// # `mod`
//
// Always written `mod` and never `ctrl` or `meta`: it is Command on a Mac and
// Control everywhere else, and a binding table that spelled that out twice
// would be two tables to keep in step.

/** A binding, as `shortcuts.ts` declares it. */
export interface Binding {
  /** The sequence. See the module comment for the spelling. */
  keys: string
  /** What it does, in the imperative, for the help sheet. */
  label: string
  /** The heading it appears under in the help sheet. */
  group: string
  /**
   * Whether it applies right now.
   *
   * A shortcut that is not applicable is not merely inert: it is absent, so
   * the key falls through to whatever else wants it, and it is left out of
   * the help sheet. That is what lets the calendar have `d`, `w` and `m` at
   * all -- they are the calendar's, and only while the calendar is open.
   */
  when?: () => boolean
  run: () => unknown
  /**
   * Fire even while the caret is in a text field.
   *
   * Off by default and correctly so: the whole point of a bare-letter
   * shortcut is that it is a letter, and a letter typed into a search box is
   * a letter. Only the chords with a modifier in them set this.
   */
  whileTyping?: boolean
}

/**
 * One chord in the form everything here compares: modifiers in a fixed
 * order, then the key, lowercased.
 *
 * Fixed order because `shift+mod+k` and `mod+shift+k` are the same chord and
 * a table that treated them as two is a bug nobody finds by reading.
 */
export function chord(parts: {
  key: string
  mod?: boolean
  shift?: boolean
  alt?: boolean
}): string {
  const out: string[] = []
  if (parts.mod) out.push('mod')
  if (parts.alt) out.push('alt')
  if (parts.shift) out.push('shift')
  out.push(parts.key.length === 1 ? parts.key.toLowerCase() : parts.key)
  return out.join('+')
}

/**
 * The chord a key event is.
 *
 * `null` for a press that is only a modifier: those arrive as events of
 * their own and must not clear a sequence in progress, or `g` followed by
 * Shift for a capital would abandon the `g`.
 *
 * Shift is deliberately *not* recorded for a printable character. The key
 * `?` is already Shift+/ on most layouts and Shift+, on some; recording the
 * modifier as well would mean a binding table that only works on one
 * keyboard. What is typed is what is matched.
 */
export function chordOf(event: {
  key: string
  ctrlKey: boolean
  metaKey: boolean
  shiftKey: boolean
  altKey: boolean
}): string | null {
  const key = event.key
  if (key === 'Shift' || key === 'Control' || key === 'Meta' || key === 'Alt') return null
  const printable = key.length === 1
  return chord({
    key,
    mod: event.ctrlKey || event.metaKey,
    alt: event.altKey,
    shift: event.shiftKey && !printable,
  })
}

/** Split a binding's `keys` into its chords, canonicalised. */
export function sequence(keys: string): string[] {
  return keys
    .trim()
    .split(/\s+/)
    .map((step) => {
      const parts = step.split('+')
      const key = parts.pop() ?? ''
      const has = (name: string) => parts.some((p) => p.toLowerCase() === name)
      return chord({ key, mod: has('mod'), alt: has('alt'), shift: has('shift') })
    })
}

/** What matching a run of chords against the table came to. */
export interface Match {
  /** The binding this run completes, if it completes one. */
  hit: Binding | null
  /** Whether some binding is still waiting for more. `g`, having pressed g. */
  pending: boolean
}

/**
 * Match the chords pressed so far against the applicable bindings.
 *
 * Returns both answers because the caller needs both: a hit is run, and a
 * *pending* prefix is what makes the next keystroke part of this sequence
 * rather than a fresh one — and, just as importantly, is what tells the
 * caller to swallow the `g` instead of letting it reach the page.
 */
export function match(bindings: Binding[], pressed: string[], typing = false): Match {
  let hit: Binding | null = null
  let pending = false
  for (const binding of bindings) {
    if (typing && !binding.whileTyping) continue
    if (binding.when && !binding.when()) continue
    const steps = sequence(binding.keys)
    if (steps.length < pressed.length) continue
    if (!pressed.every((chord, i) => steps[i] === chord)) continue
    if (steps.length === pressed.length) hit ??= binding
    else pending = true
  }
  return { hit, pending }
}

/**
 * Is the caret somewhere that a bare letter belongs to?
 *
 * A `<select>` counts: typing a letter in one jumps to the option starting
 * with it, which is a real feature and not something to take away.
 */
export function isTyping(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null
  if (!el) return false
  if (el.isContentEditable) return true
  return /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName)
}

/**
 * A chord as a reader should see it.
 *
 * Symbols on a Mac, because that is what its own menus show and what is
 * printed on the keys; words elsewhere, for the same reason.
 */
export function chordLabel(spec: string, mac: boolean): string {
  const parts = spec.split('+')
  const key = parts.pop() ?? ''
  const names: Record<string, string> = {
    ArrowLeft: '←',
    ArrowRight: '→',
    ArrowUp: '↑',
    ArrowDown: '↓',
    Escape: 'Esc',
    Enter: mac ? '↩' : 'Enter',
    ' ': 'Space',
  }
  const shown = names[key] ?? (key.length === 1 ? key.toUpperCase() : key)
  const mods = parts.map((p) =>
    p === 'mod' ? (mac ? '⌘' : 'Ctrl') : p === 'alt' ? (mac ? '⌥' : 'Alt') : mac ? '⇧' : 'Shift',
  )
  return mac ? mods.join('') + shown : [...mods, shown].join('+')
}

/** A whole sequence as a reader should see it: `G then J`. */
export function keysLabel(keys: string, mac: boolean): string {
  return sequence(keys)
    .map((step) => chordLabel(step, mac))
    .join(' then ')
}

/**
 * How long a half-finished sequence waits for its second key.
 *
 * Long enough that `g` and `j` need not be hurried, short enough that a `g`
 * pressed by accident does not silently eat a letter typed a moment later.
 */
export const SEQUENCE_MS = 1200
