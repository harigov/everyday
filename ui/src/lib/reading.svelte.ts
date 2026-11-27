// The Feed's reading settings, as this device keeps them: see `reading.ts`
// for what each one does to an article.
//
// Kept locally, the way the Feed's chip and kind are: how somebody likes to
// read on this screen is not a fact about the vault, and a phone and a wide
// desktop rarely want the same size.

import { pref } from './prefs'
import { DEFAULT_READING, normaliseReading, parseReading, type ReadingSettings } from './reading'

const readingPref = pref<ReadingSettings>(
  'jatayu.feed.reading',
  parseReading,
  { ...DEFAULT_READING },
  (value) => JSON.stringify(value),
)

class Reading {
  /** Replaced whole on every change, so reading it is enough to follow it. */
  settings = $state<ReadingSettings>(readingPref.get())

  change(patch: Partial<ReadingSettings>): void {
    this.settings = normaliseReading({ ...this.settings, ...patch })
    readingPref.set(this.settings)
  }

  reset(): void {
    this.change(DEFAULT_READING)
  }
}

export const reading = new Reading()
