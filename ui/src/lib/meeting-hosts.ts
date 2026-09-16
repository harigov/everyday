// Which calendar events are online calls, in TypeScript.
//
// A duplicate of `crates/everyday-core/src/meeting/detect.rs`'s own list and
// logic, kept here because the event popover has to answer "does this look
// like a call?" without a round trip to the backend -- it is deciding
// whether to draw a button at all. `MEETING_HOSTS` must be kept in step with
// the Rust list by hand; there is no code generation between the two crates
// and the interface, so a host added on one side and not the other is a
// button that shows up here and never records, or the reverse.

import type { CalendarEvent } from './types'

/** Mirrors `everyday_core::meeting::detect::MEETING_HOSTS`. */
export const MEETING_HOSTS: readonly string[] = [
  'meet.google.com',
  'zoom.us',
  'zoomgov.com',
  'teams.microsoft.com',
  'teams.live.com',
  'webex.com',
  'whereby.com',
  'chime.aws',
  'gotomeeting.com',
  'meet.goto.com',
  'around.co',
  'app.slack.com/huddle',
  'discord.gg',
  'meet.jit.si',
]

/** The first meeting-host link in the event's url, location or description. */
export function joinLink(
  event: Pick<CalendarEvent, 'url' | 'location' | 'description'>,
): string | null {
  for (const field of [event.url, event.location, event.description]) {
    if (!field) continue
    for (const host of MEETING_HOSTS) {
      const at = field.indexOf(host)
      if (at === -1) continue
      // Widen to the whole token the host sits in, so what is offered is a
      // followable link rather than a bare "zoom.us" clipped out of a
      // sentence around it.
      let start = at
      while (start > 0 && !/\s/.test(field[start - 1]!)) start -= 1
      let end = at + host.length
      while (end < field.length && !/\s/.test(field[end]!)) end += 1
      return field.slice(start, end)
    }
  }
  return null
}

/**
 * Is this event an online call worth offering to record?
 *
 * Busy, not cancelled, and a meeting host somewhere in its url, location or
 * description. The full backend rule also checks attendees against the
 * owner's own addresses, which this side does not have; this is
 * deliberately the looser half -- good enough to decide whether a "Take
 * notes" button is worth drawing, not the final word on whether a recording
 * is offered automatically.
 */
export function looksLikeOnlineCall(
  event: Pick<CalendarEvent, 'url' | 'location' | 'description' | 'status' | 'busy'>,
): boolean {
  if (event.status === 'cancelled') return false
  if (!event.busy) return false
  return joinLink(event) !== null
}

/** Is `now` between the event's start and end -- "in progress"? */
export function eventInProgress(
  event: Pick<CalendarEvent, 'start' | 'end'>,
  now: Date = new Date(),
): boolean {
  const start = new Date(event.start).getTime()
  const end = new Date(event.end).getTime()
  const t = now.getTime()
  return t >= start && t < end
}
