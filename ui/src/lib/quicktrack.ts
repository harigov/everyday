// One line, one reading — and a tracker made on the way if there is not one.
//
// The grammar the todo app's capture line uses, aimed at a different domain:
//
//   swim 60min          60 minutes of swimming
//   mood 7/10           a severity of seven out of ten
//   floss               done
//   ibuprofen 400mg     400 mg
//   weight 78.4kg       a number with a unit
//
// # Why this exists at all
//
// A structured note is only worth making if making it is cheaper than
// writing the sentence. "Spent an hour in the pool" takes four seconds; a
// trip to a settings dialog to define a Swimming tracker before you can
// record the hour takes a minute and does not happen, which means the number
// never gets recorded and the question "did my mood improve after pool days"
// stays unanswerable forever. So the tracker is created *by* the act of
// recording, not before it.
//
// # What is guessed, and what is not
//
// The name is matched against the trackers that already exist first, case
// insensitively, so `swim` on Friday finds the Swimming made on Monday
// rather than making a second one. Only an unmatched name creates, and what
// it creates is a guess from the text:
//
//   a number with a time unit    an amount in minutes
//   a number with any unit       an amount in that unit
//   a bare number                an amount with no unit
//   `n/m`                        a scale out of m
//   nothing but a name           a check
//
// `Dose` is deliberately never guessed. A dose and an amount store the same
// number and differ only in how a day adds up, so guessing wrong is
// invisible until a chart is drawn — and `400mg` is an amount in mg, which
// is *true*, rather than a dose, which is an interpretation. Promoting one
// is a click in the Overview.
//
// # What it never does
//
// Throw, or silently swallow what somebody typed. Anything it cannot read
// comes back in `rest` for the caller to keep, exactly as an unrecognised
// quick-add token stays in a task's title.

import type { Tracker, TrackerKind } from './types'

/** What the hint under the field says. Kept beside the grammar it describes. */
export const QUICK_TRACK_HINT = 'swim 60min   mood 7/10   floss   ibuprofen 400mg'

/** A tracker that already exists, or the shape of one to make. */
export type Target =
  | { kind: 'existing'; tracker: Tracker }
  | { kind: 'new'; name: string; trackerKind: TrackerKind; unit: string; scaleMax: number }

/** What one quick-track line means. */
export interface QuickTrack {
  /** `null` when the line has no name in it at all. */
  target: Target | null
  /** The value to record. Always a number; a check is 1. */
  value: number
  /** Whatever could not be read, kept rather than dropped. */
  rest: string
}

/** Units that mean a length of time, and what one of them is in minutes. */
const TIME_UNITS: Record<string, number> = {
  min: 1,
  mins: 1,
  minute: 1,
  minutes: 1,
  m: 1,
  h: 60,
  hr: 60,
  hrs: 60,
  hour: 60,
  hours: 60,
}

/** Top of a scale when the line says `n/m` and nothing else. */
const DEFAULT_SCALE_MAX = 10

/**
 * Read one line.
 *
 * `existing` is every tracker in the vault, archived ones included: logging
 * against something you retired should find it rather than making a second
 * one with the same name.
 */
export function parseQuickTrack(line: string, existing: Tracker[] = []): QuickTrack {
  const words = line.trim().split(/\s+/).filter(Boolean)
  if (words.length === 0) return { target: null, value: 1, rest: '' }

  // The measurement is the last word that looks like one. Last rather than
  // first, because the name comes first and can be several words: "walk the
  // dog" and "resting heart rate 58" are both ordinary.
  const tail = words[words.length - 1]!
  const measured = readValue(tail)
  const nameWords = measured ? words.slice(0, -1) : words
  const name = nameWords.join(' ').trim()

  if (!name) {
    // A number with no name is not a reading of anything. Handed back whole,
    // so the field can say so rather than logging it against nothing.
    return { target: null, value: measured?.value ?? 1, rest: line.trim() }
  }

  const found = existing.find((t) => t.name.toLowerCase() === name.toLowerCase())
  if (found) {
    return {
      target: { kind: 'existing', tracker: found },
      // A check is one however it was written: "floss 3" is not three
      // flossings, it is a tick, and the vault would clamp it anyway.
      value: found.kind === 'check' ? 1 : (measured?.value ?? found.defaultValue),
      rest: '',
    }
  }

  return { target: newTarget(name, measured), value: measured?.value ?? 1, rest: '' }
}

/** A measurement read off one word. */
interface Measured {
  value: number
  unit: string
  /** Set when the word was `n/m`: a severity out of m. */
  outOf?: number
}

/**
 * Read a trailing word as a measurement.
 *
 * Returns `null` for anything that is not one, which is how a name made
 * entirely of words survives: `floss` has no measurement and becomes a
 * check rather than losing its last word.
 */
export function readValue(word: string): Measured | null {
  const w = word.toLowerCase()

  // `7/10` — a severity, and the only form that carries its own ceiling.
  const scale = /^(\d+(?:\.\d+)?)\/(\d+)$/.exec(w)
  if (scale) {
    const outOf = Number(scale[2])
    return outOf > 0 ? { value: Number(scale[1]), unit: '', outOf } : null
  }

  // `1h30` and `1h30m` — the same compound the todo app's estimate takes,
  // because somebody who has learned one should not have to learn two.
  const hm = /^(\d{1,3})h(\d{1,2})m?$/.exec(w)
  if (hm) return { value: Number(hm[1]) * 60 + Number(hm[2]), unit: 'min' }

  // `60min`, `400mg`, `78.4kg`, `20`, `2h`.
  const amount = /^(\d+(?:\.\d+)?)([a-z%]*)$/.exec(w)
  if (!amount) return null
  const raw = Number(amount[1])
  if (!Number.isFinite(raw)) return null
  const unit = amount[2] ?? ''

  // A time unit is normalised to minutes, which is what makes a 45-minute
  // run draw as a span on the calendar rather than as a dot.
  const factor = TIME_UNITS[unit]
  if (factor !== undefined) return { value: raw * factor, unit: 'min' }
  return { value: raw, unit }
}

function newTarget(name: string, measured: Measured | null): Target {
  if (measured?.outOf) {
    return {
      kind: 'new',
      name,
      trackerKind: 'scale',
      unit: '',
      scaleMax: measured.outOf,
    }
  }
  if (measured) {
    return {
      kind: 'new',
      name,
      trackerKind: 'amount',
      unit: measured.unit,
      scaleMax: DEFAULT_SCALE_MAX,
    }
  }
  // Nothing but a name: the commonest thing anybody tracks, and the one
  // that needs no number to mean something.
  return { kind: 'new', name, trackerKind: 'check', unit: '', scaleMax: DEFAULT_SCALE_MAX }
}

/**
 * The preview line under the field: what pressing Enter would do.
 *
 * This is the whole of how the grammar is taught. A hint says what is
 * possible; this says what *this* line means, which is the difference
 * between a syntax somebody learns and a syntax somebody guesses at.
 */
export function describeQuickTrack(parsed: QuickTrack): string {
  const { target, value } = parsed
  if (!target) return parsed.rest ? 'No tracker named in that' : ''

  if (target.kind === 'existing') {
    const t = target.tracker
    if (t.kind === 'check') return `${t.name} — done`
    const unit = t.unit ? ` ${t.unit}` : ''
    if (t.kind === 'scale') return `${t.name} — ${value} out of ${t.scaleMax}`
    return `${t.name} — ${format(value)}${unit}`
  }

  // A new tracker says so, and says what it will be: the one moment to
  // catch a wrong guess is before it is made.
  const what =
    target.trackerKind === 'check'
      ? 'a habit'
      : target.trackerKind === 'scale'
        ? `out of ${target.scaleMax}`
        : target.unit
          ? `in ${target.unit}`
          : 'a number'
  return `New tracker “${target.name}” (${what}) — ${
    target.trackerKind === 'check' ? 'done' : format(value)
  }`
}

/** A number without a pointless trailing zero. Mirrors `formatNumber`. */
function format(v: number): string {
  return Number.isInteger(v) ? String(v) : String(Number(v.toFixed(2)))
}
