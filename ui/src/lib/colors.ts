/** Journal accents. Chosen to stay legible on both themes at 6px. */
export const DEFAULT_COLORS = [
  '#c2410c',
  '#0f766e',
  '#4338ca',
  '#a21caf',
  '#b45309',
  '#15803d',
  '#0369a1',
  '#be123c',
] as const

/**
 * What to call each of them.
 *
 * Only the context menus need this: a palette laid out as swatches is picked
 * by looking, but a menu row is read as well as seen, and "#0f766e" is not a
 * colour anybody recognises.
 */
const NAMES: Record<string, string> = {
  '#c2410c': 'Rust',
  '#0f766e': 'Teal',
  '#4338ca': 'Indigo',
  '#a21caf': 'Magenta',
  '#b45309': 'Amber',
  '#15803d': 'Green',
  '#0369a1': 'Blue',
  '#be123c': 'Crimson',
}

/** The name of a palette colour, or the hex itself for anything else. */
export function colorName(hex: string): string {
  return NAMES[hex.toLowerCase()] ?? hex
}
