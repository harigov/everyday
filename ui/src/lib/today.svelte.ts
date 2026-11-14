// Today's day of the month, kept current across midnight.
//
// For the calendar symbol, which shows the date: the app bar draws it once
// and keeps it for as long as the window is open -- days, with the keychain
// unlocking the vault and no auto-lock -- so a date read when it was drawn
// goes stale at the first midnight. One timer, set for just after the next
// midnight and set again each time it fires, started the first time anything
// asks. A timer held up by sleep fires late on waking, which is when the
// date is next looked at anyway.

let day = $state(new Date().getDate())
let timer: ReturnType<typeof setTimeout> | undefined

function schedule() {
  const now = new Date()
  const next = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1, 0, 0, 5)
  timer = setTimeout(() => {
    day = new Date().getDate()
    schedule()
  }, next.getTime() - now.getTime())
}

/** Today's date, 1-31, reactive: read inside `$derived` and it changes at midnight. */
export function dayOfMonth(): number {
  if (timer === undefined && typeof window !== 'undefined') schedule()
  return day
}
