// Where a notification goes, and what the stack does when they pile up.
//
// This half of the notification service is deliberately pure: no runes, no
// DOM, no Tauri. Everything here is a decision -- toast or notification
// centre, replace or append, which one to drop when there is no room -- and
// decisions are the part worth testing and the part a later change could
// silently reverse. `notify.svelte.ts` holds the state and the plumbing and
// calls into this; `scripts/notify.test.mjs` exercises it directly.

/**
 * How loud a notification is.
 *
 * Not a severity scale for its own sake: the level picks the colour, the
 * icon, the default dwell time and the politeness of the live region, so
 * choosing it correctly is what makes a screen reader announce a failed save
 * immediately and a saved one whenever it gets round to it.
 */
export type NotifyLevel = 'info' | 'success' | 'warning' | 'error'

/**
 * How far a notification has to travel to have done its job.
 *
 * `app` -- the default -- means it is about something the user is looking
 * at. A tick that says the calendar refreshed is meaningless in the
 * notification centre an hour later, and putting it there trains people to
 * turn the whole thing off.
 *
 * `user` means it must reach a person who may not be looking at this window
 * at all: their writing is not on disk, a subscribed calendar has stopped
 * answering. These are the ones allowed out to the operating system.
 */
export type NotifyReach = 'app' | 'user'

/** What a caller passes to `notify`. Only `title` is required. */
export interface NotifySpec {
  title: string
  /** A sentence of detail. The title alone should still be worth reading. */
  body?: string
  level?: NotifyLevel
  reach?: NotifyReach
  /**
   * Milliseconds on screen, or `null` to stay until dismissed.
   *
   * Defaults come from the level (see `DWELL`), which is nearly always the
   * right answer; pass this when the specific message needs longer.
   */
  timeout?: number | null
  /**
   * An identity for a notification that can recur.
   *
   * A save that keeps failing, a feed that is still down: two of the same
   * thing is not twice the information. A new notification with a `key`
   * replaces the one already on screen rather than stacking beneath it.
   */
  key?: string
  /** One button on the toast. Kept singular on purpose: a toast is not a dialog. */
  action?: NotifyAction
}

export interface NotifyAction {
  label: string
  run: () => void
}

/** A notification as the toast stack holds it. */
export interface Toast {
  /** Monotonic, and fresh even when a keyed toast replaces another, so the
   *  dwell timer restarts and Svelte re-keys the element. */
  id: number
  level: NotifyLevel
  title: string
  body: string | null
  timeout: number | null
  key: string | null
  action: NotifyAction | null
}

/**
 * How long each level sits on screen by default.
 *
 * Errors do not leave on their own. Everything else here is a remark; an
 * error is the app telling you something did not happen, and a message that
 * important disappearing after four seconds because you looked away is the
 * failure mode this service exists to avoid. It stays until dismissed.
 */
export const DWELL: Record<NotifyLevel, number | null> = {
  success: 4_000,
  info: 5_000,
  warning: 8_000,
  error: null,
}

/**
 * How many toasts may be on screen at once.
 *
 * Four is about where a stack stops reading as a list of messages and starts
 * reading as an app that has gone wrong.
 */
export const MAX_TOASTS = 4

/** Where a notification is delivered. */
export type Channel = 'toast' | 'os' | 'both'

/** What the router needs to know about the world outside the notification. */
export interface Surroundings {
  /** Is this window actually in front of the user right now? */
  focused: boolean
  /** Can an OS notification be delivered -- plugin present, permission granted? */
  osReady: boolean
}

/**
 * Decide where one notification goes.
 *
 * The rules, in order, and the reasoning for each:
 *
 *  1. `reach: 'app'` never leaves the window. It is the default, so the
 *     quiet failure mode of forgetting to think about this is a toast --
 *     never an unexpected banner over whatever else the machine is doing.
 *  2. A focused window gets a toast even when the reach is `user`. The
 *     person is already looking at the app; an OS banner covering the app it
 *     came from is worse than useless, and on macOS it is what makes people
 *     revoke the permission.
 *  3. Otherwise, out to the operating system -- if it can take it. When it
 *     cannot (permission refused, no plugin, a browser with notifications
 *     switched off) the toast is not a consolation prize: it is waiting in
 *     the window when they come back, which is where they were going to look
 *     anyway.
 *  4. Two kinds of notification go to both places. One with an `action`,
 *     because the button is the point and an OS banner here has nowhere to
 *     put it; and one that was meant to stay until dismissed, because a
 *     notification centre will quietly retire it on its own schedule. In
 *     both cases the OS copy is the summons and the toast is the thing
 *     itself.
 */
export function channelFor(spec: NotifySpec, where: Surroundings): Channel {
  const reach = spec.reach ?? 'app'
  if (reach === 'app') return 'toast'
  if (where.focused) return 'toast'
  if (!where.osReady) return 'toast'

  const timeout = spec.timeout === undefined ? DWELL[spec.level ?? 'info'] : spec.timeout
  if (spec.action || timeout === null) return 'both'
  return 'os'
}

/** Fill a caller's spec out into the toast the stack stores. */
export function toToast(spec: NotifySpec, id: number): Toast {
  const level = spec.level ?? 'info'
  return {
    id,
    level,
    title: spec.title,
    body: spec.body ?? null,
    timeout: spec.timeout === undefined ? DWELL[level] : spec.timeout,
    key: spec.key ?? null,
    action: spec.action ?? null,
  }
}

/**
 * Add `toast` to `stack`, returning the new stack.
 *
 * Pure, and returns a fresh array rather than mutating, so the caller's
 * reactive assignment is the only thing that moves the screen.
 *
 * A keyed toast replaces the one it matches *in place*. Removing it and
 * appending would make an error that recurs every few seconds crawl down the
 * stack and shuffle everything above it, which reads as several different
 * problems rather than one that has not gone away.
 */
export function push(stack: readonly Toast[], toast: Toast): Toast[] {
  const at = toast.key === null ? -1 : stack.findIndex((t) => t.key === toast.key)
  const next = at >= 0 ? stack.map((t, i) => (i === at ? toast : t)) : [...stack, toast]
  return trim(next, toast.id)
}

/**
 * Bring an over-full stack back to `MAX_TOASTS`.
 *
 * The oldest *dismissible* toast goes first, not simply the oldest. A sticky
 * toast is one somebody decided must not vanish on its own -- an unsaved
 * journal, a vault that cannot be written -- and letting a run of chatter
 * underneath it push it off the screen would lose exactly the message that
 * mattered. Only when every toast on screen is sticky does the oldest go,
 * because at that point something has to.
 *
 * `arriving` is never dropped, whatever else is true. A stack already full
 * of sticky errors would otherwise pick the toast that had just been posted
 * as its "oldest dismissible" one and drop it before it had been drawn --
 * silently, and specifically for the user who is having the worst time. The
 * newest message is the one being reacted to; if something has to go, it is
 * something already read.
 */
function trim(stack: Toast[], arriving: number): Toast[] {
  const out = [...stack]
  while (out.length > MAX_TOASTS) {
    const dismissible = out.findIndex((t) => t.timeout !== null && t.id !== arriving)
    const oldest = out.findIndex((t) => t.id !== arriving)
    out.splice(dismissible >= 0 ? dismissible : oldest, 1)
  }
  return out
}
