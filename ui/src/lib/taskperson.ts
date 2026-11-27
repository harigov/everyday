// Somebody a task is to be raised with, as the words said them.
//
// The same reading the core makes (`TaskPerson::said` over
// `meeting::identify::parse_attendee` in `jatayu-core`), so a person typed
// into a task's "Raise with" field and one the assistant wrote down from a
// sentence come out the same. It matters because the brief before a meeting
// matches by address as well as by name: "Josh Smith <josh@acme.com>" kept
// whole as a name would never be found among the guests, where the address
// on its own would.
//
// Pure, and in a file of its own so that a test can load it without the rest
// of the interface behind it (`scripts/meetingbrief.test.mjs`).

import type { TaskPerson } from './types'

/** Lower-case the letters a-z only, as Rust's `to_ascii_lowercase` does. */
function asciiLower(text: string): string {
  return text.replace(/[A-Z]/g, (c) => c.toLowerCase())
}

/**
 * Split an attendee string into a name and an address, either of which may be
 * absent. Handles a display name with the address in angle brackets (the name
 * quoted or not), a bare `mailto:` address, a bare address, and a bare name.
 * Addresses come back lower-case.
 */
export function parseAttendee(text: string): { name?: string; email?: string } {
  const s = text.trim()
  if (!s) return {}

  const open = s.indexOf('<')
  const close = open >= 0 ? s.indexOf('>', open) : -1
  if (open >= 0 && close >= 0) {
    const inside = s.slice(open + 1, close).trim()
    const name = s
      .slice(0, open)
      .trim()
      .replace(/^"+|"+$/g, '')
      .trim()
    const email = (inside.startsWith('mailto:') ? inside.slice('mailto:'.length) : inside).trim()
    return {
      ...(name ? { name } : {}),
      ...(email ? { email: asciiLower(email) } : {}),
    }
  }

  if (s.startsWith('mailto:')) {
    const email = s.slice('mailto:'.length).trim()
    return email ? { email: asciiLower(email) } : {}
  }

  if (s.includes('@') && !/\s/.test(s)) return { email: asciiLower(s) }

  return { name: s }
}

/**
 * A person as typed into "Raise with": `Josh`, `josh@acme.com` or `Josh Smith
 * <josh@acme.com>`. A bare address is its own name until a card gives a better
 * one. Nothing is looked up -- finding their card is the service's, which has
 * the vault. The name is empty only when nothing usable was typed.
 */
export function personSaid(text: string): TaskPerson {
  const { name, email } = parseAttendee(text)
  return { name: name ?? email ?? '', ...(email ? { email } : {}) }
}
