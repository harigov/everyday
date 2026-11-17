// Today, kept current across midnight.
//
// For the calendar symbol, which shows the date, and the Overview, whose
// Karma card counts the week and the streak up to today: each is drawn once
// and keeps it for as long as the window is open -- days, with the keychain
// unlocking the vault and no auto-lock -- so a date read when it was drawn
// goes stale at the first midnight. One timer, set for just after the next
// midnight and set again each time it fires, started the first time anything
// asks. A timer held up by sleep fires late on waking, which is when the
// date is next looked at anyway.

import { todayIso } from './time'

let iso = $state(todayIso())
let timer: ReturnType<typeof setTimeout> | undefined

function schedule() {
  const now = new Date()
  const next = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1, 0, 0, 5)
  timer = setTimeout(() => {
    iso = todayIso()
    schedule()
  }, next.getTime() - now.getTime())
}

/** Today, `YYYY-MM-DD`, reactive: read inside `$derived` and it changes at midnight. */
export function currentDay(): string {
  if (timer === undefined && typeof window !== 'undefined') schedule()
  return iso
}

/** Today's date, 1-31, reactive like `currentDay`. */
export function dayOfMonth(): number {
  return Number(currentDay().slice(8))
}
