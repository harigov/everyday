// Who a message is from, drawn the way a face would be: one or two initials
// on a colour that belongs to the address.
//
// A list of names alone is a wall of text the eye has to read line by line;
// a column of coloured circles is something it can scan, and the same
// sender's circle is the same colour on every row, in the list and in the
// open thread alike, so a regular correspondent is recognised before their
// name is read. There are no photos: fetching one would tell a stranger's
// server when the mail was opened, which is exactly what this app's remote
// image rules exist to prevent.
//
// Pure, so the rules -- which letters, which colour -- are tested without a
// browser (`scripts/avatar.test.mjs`).

/**
 * Mid-tone colours white initials stay legible on, light theme and dark --
 * soft rather than saturated, so a screen of them reads as calm rather than
 * as a sweet jar. Ordered so neighbours differ in hue.
 */
export const AVATAR_COLORS = [
  '#7c6fd6',
  '#d9894a',
  '#3fa47f',
  '#4f8fd6',
  '#c96a9a',
  '#4f9fa8',
  '#b8873a',
  '#8b7cb8',
  '#d1695a',
  '#6e9150',
] as const

/**
 * The few accents an account's dot can be, in the order the accounts are
 * listed -- "which inbox is this from" on a unified list. Kept to a handful
 * of clearly different hues, since nobody tells eight inboxes apart by
 * colour anyway; a dot is 8px beside a 38px avatar, so sharing a hue with
 * one never makes the two read as the same thing.
 */
export const ACCOUNT_COLORS = [
  '#4f8fd6',
  '#3fa47f',
  '#d9894a',
  '#a77fd6',
  '#d1697f',
  '#4f9fa8',
] as const

/** Letters and digits, in any script. */
const WORD = /[\p{L}\p{N}]+/gu

/**
 * One or two initials: the first letter of the first and of the last word
 * of `name` -- "Prof. Kemi Adeyemi" is PA, "CP Comboios" CC -- or of the
 * address's own words when there is no name ("marcus.webb@…" is MW). A
 * single word gives a single letter rather than two from the same word,
 * which reads as an abbreviation nobody chose. `?` only when there is
 * nothing to take a letter from at all.
 */
export function initialsOf(name: string, email: string): string {
  const fromName = name.replace(/["'()<>[\]]/g, ' ').match(WORD) ?? []
  const words = fromName.length > 0 ? fromName : (email.split('@')[0]?.match(WORD) ?? [])
  const first = words[0]
  if (!first) return '?'
  const last = words.length > 1 ? words[words.length - 1] : undefined
  const letter = (word: string) => [...word][0]?.toLocaleUpperCase() ?? ''
  return last ? letter(first) + letter(last) : letter(first)
}

/** FNV-1a, 32-bit: stable across sessions and machines, which is all a
 *  colour choice needs -- nothing here is a secret or a key. */
function hash(text: string): number {
  let h = 0x811c9dc5
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i)
    h = Math.imul(h, 0x01000193)
  }
  return h >>> 0
}

/** The colour `key` -- an address, ordinarily -- is always drawn in,
 *  compared without case or surrounding space. */
export function avatarColor(key: string): string {
  const palette = AVATAR_COLORS
  return palette[hash(key.trim().toLowerCase()) % palette.length] ?? palette[0]
}

/** An account's dot: its place among `accountIds`, wrapping past the end
 *  of the palette. The first colour for an account no longer listed. */
export function accountColor(accountId: string, accountIds: readonly string[]): string {
  const at = Math.max(0, accountIds.indexOf(accountId))
  return ACCOUNT_COLORS[at % ACCOUNT_COLORS.length] ?? ACCOUNT_COLORS[0]
}
