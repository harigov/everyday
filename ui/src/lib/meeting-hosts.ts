// Which calendar events are meetings worth notes, and where their call is, in
// TypeScript.
//
// A duplicate of `crates/everyday-core/src/meeting/detect.rs`'s own list and
// logic, kept here because the event popover has to answer "is this a
// meeting, and is there a link to join?" without a round trip to the backend -- it is deciding
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

/** How long before an event starts "Take notes" is offered on it. */
export const TAKE_NOTES_LEAD_MS = 5 * 60_000

/**
 * Is this an event worth taking notes on right now?
 *
 * Any timed, busy event with somebody besides its organizer invited, from
 * five minutes before it starts until it ends -- not only an online call, as
 * it used to be: a meeting in a room is as worth a transcript as one on a
 * link. This is the interface's half of the backend's `detect::is_meeting`,
 * which compares attendees against the owner's own addresses; this side has
 * no list of those, so it compares them against the organizer instead,
 * which is what tells a solo event (the owner inviting only themselves)
 * apart from a meeting. All-day events are left out because they are not
 * meetings (a birthday, a holiday), and so are cancelled and free ones.
 * Whether anything is already being recorded, and whether meeting notes
 * are set up, are the caller's to check; this only answers about the event
 * and the clock.
 */
export function takeNotesWindow(
  event: Pick<
    CalendarEvent,
    'start' | 'end' | 'allDay' | 'attendees' | 'status' | 'busy' | 'organizer'
  >,
  now: Date = new Date(),
): boolean {
  if (event.allDay || event.status === 'cancelled' || !event.busy) return false
  if (!hasSomebodyElse(event.attendees ?? [], event.organizer ?? '')) return false
  const start = new Date(event.start).getTime()
  const end = new Date(event.end).getTime()
  const t = now.getTime()
  return t >= start - TAKE_NOTES_LEAD_MS && t < end
}

/** The address in "Name <addr>" or a bare "addr"; `null` for a bare name. */
function addressOf(raw: string): string | null {
  const bracketed = /<([^<>]+@[^<>]+)>/.exec(raw)
  if (bracketed) return bracketed[1]!.trim().toLowerCase()
  const bare = raw.trim()
  return bare.includes('@') ? bare.toLowerCase() : null
}

/** Is anybody on `attendees` not the organizer? A bare name always is. */
function hasSomebodyElse(attendees: string[], organizer: string): boolean {
  const own = addressOf(organizer)
  return attendees.some((raw) => {
    const address = addressOf(raw)
    return address === null || address !== own
  })
}

/**
 * A join link as something a browser can be sent to. `joinLink` widens to
 * the whole token a host sits in, which can be a bare `zoom.us/j/1` out of a
 * description; the system opener only takes `https://` addresses.
 */
export function joinHref(link: string): string {
  return /^[a-z][a-z0-9+.-]*:\/\//i.test(link) ? link : `https://${link}`
}
