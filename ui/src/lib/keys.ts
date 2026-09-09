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

/**
 * One thing the application can do.
 *
 * Three surfaces read this same row and none of them owns it: the keyboard,
 * which dispatches the ones with `keys`; the tray, which sends the ones with
 * `tray` to the operating system; and the palette, which offers all of them.
 * Before this they were three registries, and the third one to learn about a
 * new action was always the one nobody remembered.
 *
 * An action that cannot happen right now says so by `when` answering false, and
 * is then *absent* rather than disabled -- absent from the palette, from the
 * help sheet, and from the tray menu. That is what lets two apps use the same
 * bare letter, and it is why `when` is a function rather than a flag: it is
 * re-read every time somebody looks.
 */
export interface Binding {
  /**
   * A stable name, for the surfaces that have to refer to one across a
   * boundary -- the tray sends it to Rust and gets it back when a menu item is
   * chosen. Prefixed with the app it belongs to: `todo:add`.
   *
   * Optional only because most rows are reached by key or by reading, and
   * inventing an id for each of those would be forty names nobody says.
   */
  id?: string
  /**
   * The sequence. See the module comment for the spelling.
   *
   * Absent for an action that has no shortcut, which is most of what a palette
   * offers: there are more things worth doing than there are comfortable keys.
   */
  keys?: string
  /** What it does, in the imperative, for the help sheet and the palette. */
  label: string
  /** The heading it appears under. */
  group: string
  /**
   * Extra words to match on in the palette, for the things people call by
   * another name than the label uses. "Lock" should be found by "sign out";
   * "Board" by "kanban".
   */
  keywords?: string[]
  /** Drawn beside the label in the palette. */
  icon?: string
  /**
   * Offer this in the system tray as well.
   *
   * The tray is a *filtered view* over this table rather than a registry of
   * its own, so an action reaches the menu bar by wearing this rather than by
   * being declared a second time somewhere else.
   */
  tray?: boolean
  /**
   * Bring the window forward before running, when chosen from the tray.
   * Default true, because almost every quick action ends with a cursor
   * somewhere. False for the ones that are precisely about *not* coming back:
   * locking, stopping a timer.
   */
  raise?: boolean
  /**
   * Renders a checkbox in the tray. For an action that is also a state -- a
   * timer that is or is not running -- where hiding the "off" version would
   * cost the reader the fact that it is off.
   */
  checked?: () => boolean
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
    // A row with no keys is a palette action and has nothing to match.
    if (!binding.keys) continue
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
