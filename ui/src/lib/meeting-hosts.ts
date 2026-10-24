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

/**
 * The first meeting-host link in the event's url, location or description.
 *
 * Read the way `detect.rs` reads it, which is the way a browser does: the
 * link this returns is opened as written by "Join & take notes", so a host
 * that merely *appears* in it is not enough. `https://evil.example?@meet.google.com/x`
 * goes to evil.example, and so would the button.
 */
export function joinLink(
  event: Pick<CalendarEvent, 'url' | 'location' | 'description'>,
): string | null {
  for (const field of [event.url, event.location, event.description]) {
    const link = field ? firstMeetingLink(field) : null
    if (link) return link
  }
  return null
}

/** What ends a link besides whitespace -- `detect.rs`'s `TOKEN_STOP`. */
const TOKEN_STOP = /[\s<>"']/
/** What a sentence leaves stuck to a link's end -- `TRAILING_PUNCTUATION`. */
const TRAILING_PUNCTUATION = /[.,;:)\]}>"'!]+$/

/**
 * Scan left to right for the first link -- schemed, or bare at a word
 * boundary as Google Meet's own invitations write it -- whose host matches
 * `MEETING_HOSTS`. A bare one is returned as written; `joinHref` gives it
 * its scheme.
 */
function firstMeetingLink(text: string): string | null {
  const lower = text.toLowerCase()
  for (let i = 0; i < text.length; i++) {
    if (text.startsWith('https://', i) || text.startsWith('http://', i)) {
      const token = takeToken(text.slice(i))
      const parts = hostAndPath(token.slice(token.indexOf('://') + 3))
      if (parts && matchesMeetingHost(parts.host, parts.path)) return token
      continue
    }
    const before = i === 0 ? '' : text[i - 1]!
    if (before && /[\p{L}\p{N}.\-@/]/u.test(before)) continue
    for (const host of MEETING_HOSTS) {
      if (host.includes('/') || !lower.startsWith(host, i)) continue
      const after = text[i + host.length]
      if (after === undefined || /[\s/?#]/.test(after) || TOKEN_STOP.test(after)) {
        return takeToken(text.slice(i))
      }
    }
  }
  return null
}

function takeToken(s: string): string {
  const end = s.search(TOKEN_STOP)
  return (end === -1 ? s : s.slice(0, end)).replace(TRAILING_PUNCTUATION, '')
}

/**
 * The host a browser would visit (lower-cased) and the rest of the link, or
 * `null` for one whose host this will not vouch for: the host ends at the
 * first `/`, `?`, `#` or `\`, a `user@` in front of it is refused rather than
 * stripped, and so is anything but plain letters, digits, dots and hyphens.
 */
function hostAndPath(noScheme: string): { host: string; path: string } | null {
  const end = noScheme.search(/[/?#\\]/)
  const authority = end === -1 ? noScheme : noScheme.slice(0, end)
  if (authority.includes('@')) return null
  const colon = authority.indexOf(':')
  const host = colon === -1 ? authority : authority.slice(0, colon)
  const port = colon === -1 ? '' : authority.slice(colon + 1)
  if (!/^[A-Za-z0-9.-]+$/.test(host) || !/^\d*$/.test(port)) return null
  return { host: host.toLowerCase(), path: end === -1 ? '' : noScheme.slice(end) }
}

/** A host matches an entry, or is a subdomain of it; an entry with a path needs that path too. */
function matchesMeetingHost(host: string, path: string): boolean {
  return MEETING_HOSTS.some((entry) => {
    const slash = entry.indexOf('/')
    const entryHost = slash === -1 ? entry : entry.slice(0, slash)
    const hostOk = host === entryHost || host.endsWith(`.${entryHost}`)
    return slash === -1
      ? hostOk
      : hostOk && path.toLowerCase().startsWith(entry.slice(slash).toLowerCase())
  })
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
