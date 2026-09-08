// Ratings, on the way from storage to the screen and back.
//
// Scores are stored out of a hundred (see `everyday_core::library`) and shown
// out of five, and these two functions are the only places that conversion
// happens. Keeping them here rather than inline in a component is what makes
// "four and a half stars, reloaded, is still four and a half stars" a
// property of the application rather than of whichever component was written
// most recently.

/** How many stars the control draws. */
export const MAX_STARS = 5

/**
 * The finest position the control offers.
 *
 * Ten positions, which is about as fine as an opinion of a film actually is.
 * Storage holds a hundred so that somebody else's 82% survives being
 * imported; the control does not pretend you can express 82% about a novel.
 */
export const STAR_STEP = 0.5

/** A stored 0-100 score as stars, rounded to the nearest half. */
export function stars(score: number): number {
  const raw = Math.min(Math.max(score, 0), 100) / 20
  return Math.round(raw * 2) / 2
}

/** A half-star position back to the stored scale. Exact for every step. */
export function fromStars(value: number): number {
  return Math.round(Math.min(Math.max(value, 0), MAX_STARS) * 20)
}

/**
 * A score as text: "4.5", or "4" rather than "4.0".
 *
 * `toFixed(1)` everywhere would print "4.0", which reads as a precision
 * nobody claimed.
 */
export function ratingLabel(score: number): string {
  const value = stars(score)
  return Number.isInteger(value) ? String(value) : value.toFixed(1)
}

/**
 * The same, spoken, for a screen reader and a tooltip.
 *
 * The noun agrees with the *scale*, not with the score: it is "1 out of 5
 * stars", never "1 out of 5 star". Pluralising on the score is the easy
 * mistake and a quiet one -- it shows at exactly one of the ten positions
 * the control offers -- so the scale being fixed at five is stated here
 * rather than left implied.
 */
export function ratingTitle(score: number | null | undefined): string {
  if (score === null || score === undefined) return 'Not rated'
  return `${ratingLabel(score)} out of ${MAX_STARS} stars`
}

/**
 * The score a row should draw: the position under the pointer, or the stored
 * value when there is no pointer.
 *
 * It exists because there are two scales in play and they look alike. A
 * hover position is stars (`0.5 .. 5`); a stored rating is a score
 * (`0 .. 100`). Reconciling them inline reads fine and was wrong in exactly
 * one direction -- `fromStars(84)` clamps to five, so every rated item drew
 * as five full stars while its number said 4.2. Naming the conversion is
 * what makes it a thing that can be tested rather than a thing that can be
 * glanced at.
 */
export function shownScore(value: number | null | undefined, hovered: number | null): number {
  if (hovered !== null) return fromStars(hovered)
  return value ?? 0
}

/**
 * How full the `n`th star is, in `0..=1`.
 *
 * A fraction rather than a boolean, so a half star is drawn as half a star
 * rather than as a different glyph -- which is what keeps the row looking
 * like one row instead of a row with a typo in it.
 */
export function starFill(score: number, n: number): number {
  return Math.min(Math.max(stars(score) - (n - 1), 0), 1)
}
