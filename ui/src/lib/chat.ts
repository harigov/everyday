// The rules behind Settings → Chat, kept apart from the store and the panel so
// they are checkable without a browser -- see `scripts/chat.test.mjs`. What
// lives here: reading a link's status as a word, a tone and a sentence;
// checking a Matrix link's fields before they are sent anywhere, with the
// same words `MatrixLink::validate` uses in Rust; deciding whether the bot's
// password is still needed; turning the stored "quiet for this many minutes"
// into the hours the form asks for and back; and the few words the rest of
// the interface uses for a link (its name in a list, the tooltip on the
// assistant history's "Matrix" marker).
//
// Everything that touches the network lives in `chat.svelte.ts` instead.

import type { ChatLink, ChatLinkView, ChatState, ChatStatus, MatrixLink, Via } from './types'

// ── Where a link stands ──────────────────────────────────────────────────

/** How Settings → Chat draws a status. */
export interface StatusPill {
  label: string
  /** What colours the pill. `wait` is a state that is expected to pass by
   *  itself, so it is drawn neutral rather than as a warning. */
  tone: 'ok' | 'wait' | 'warn' | 'error' | 'off'
  /** One sentence to put beside it: what the connector said, or -- when it
   *  said nothing -- what the state means. */
  detail: string
}

const STATES: Record<ChatState, { label: string; tone: StatusPill['tone']; fallback: string }> = {
  online: { label: 'Online', tone: 'ok', fallback: 'Listening.' },
  connecting: { label: 'Connecting', tone: 'wait', fallback: 'Signing in…' },
  needsSignIn: {
    label: 'Needs sign-in',
    tone: 'warn',
    fallback: "Give the bot's password to sign it in.",
  },
  failing: {
    label: 'Failing',
    tone: 'error',
    fallback: 'Something keeps going wrong. It is being tried again.',
  },
  off: { label: 'Off', tone: 'off', fallback: 'Not running.' },
}

/**
 * A `ChatStatus` as a word, a tone and a sentence.
 *
 * The detail is the connector's own sentence, verbatim -- "Signed in as
 * @everyday:example.org. Talking to @me:example.org." -- and only when it
 * has none (a link that is off, a connector that has not reported yet) does
 * this say what the state means instead.
 */
export function statusPill(status: ChatStatus): StatusPill {
  const state = STATES[status.state] ?? STATES.off
  return { label: state.label, tone: state.tone, detail: status.detail.trim() || state.fallback }
}

// ── Checking a link before it is sent ────────────────────────────────────

/**
 * Is this Matrix link fit to save? `null` means yes, otherwise the reason.
 *
 * The same checks, in the same order and in the same words, as
 * `MatrixLink::validate` in `crates/everyday-core/src/chat.rs`: a person
 * gets the answer before a round trip rather than after one, and the two
 * sides do not give different reasons for the same mistake. Whitespace round
 * the fields is the caller's business -- the form trims before it asks --
 * because Rust, which does not trim ids, would refuse them.
 */
export function validateMatrixLink(m: MatrixLink): string | null {
  const home = m.homeserver.trim()
  const rest = home.startsWith('https://')
    ? home.slice('https://'.length)
    : home.startsWith('http://')
      ? home.slice('http://'.length)
      : null
  if (rest === null) return 'the homeserver address must start with https:// or http://'
  if (rest.replace(/^\/+|\/+$/g, '') === '') return 'the homeserver address has no host'
  const bad = badUserId(m.userId, "the bot's Matrix ID") ?? badUserId(m.owner, 'your Matrix ID')
  if (bad) return bad
  if (m.userId === m.owner) return 'the bot needs an account of its own, not yours'
  return null
}

/** `@localpart:server`, loosely: enough to catch a display name or a bare
 *  username typed into the wrong box, not a full grammar. */
function badUserId(id: string, what: string): string | null {
  const colon = id.indexOf(':')
  const ok =
    id.startsWith('@') &&
    colon > 1 &&
    colon < id.length - 1 &&
    // The same test Rust makes with `char::is_whitespace`.
    !/\s/u.test(id)
  return ok ? null : `${what} should look like @name:server, not "${id}"`
}

/**
 * The link with its text fields trimmed -- what the form sends. A Matrix ID
 * pasted from a chat client often arrives with a trailing space or newline.
 */
export function tidyLink(link: ChatLink): ChatLink {
  return {
    ...link,
    service: {
      ...link.service,
      homeserver: link.service.homeserver.trim(),
      userId: link.service.userId.trim(),
      owner: link.service.owner.trim(),
    },
  }
}

/** Validate whichever service a link is on. */
export function validateLink(link: ChatLink): string | null {
  switch (link.service.kind) {
    case 'matrix':
      return validateMatrixLink(link.service)
  }
}

/**
 * A reason, as a sentence to show: its first letter capitalised and a full
 * stop on the end. The Rust messages are written to be joined into other
 * sentences ("the homeserver address has no host"), so they arrive in lower
 * case without one.
 */
export function sentence(message: string): string {
  const text = message.trim()
  if (!text) return text
  const capital = text[0]!.toUpperCase() + text.slice(1)
  return /[.!?]$/.test(capital) ? capital : `${capital}.`
}

// ── Is the password still needed? ────────────────────────────────────────

/**
 * Does `draft` sign in to the same account `before` did?
 *
 * Mirrors `ChatLink::same_account`: the same server -- a trailing slash does
 * not make a different one -- and the same bot. Everything else (who the bot
 * answers, how long a chat may be quiet) can change without signing in
 * again, which is exactly why the saved sign-in is only kept for this.
 */
export function sameAccount(before: ChatLink, draft: ChatLink): boolean {
  const a = before.service
  const b = draft.service
  return (
    a.kind === b.kind &&
    a.homeserver.replace(/\/+$/, '') === b.homeserver.replace(/\/+$/, '') &&
    a.userId === b.userId
  )
}

/** What the form knows about the sign-in the saved link already has: `null`
 *  for a link that has never been saved. */
export type SavedSignIn = Pick<ChatLinkView, 'link' | 'signedIn' | 'hasPassword'> | null

/**
 * Does saving this draft need a password typed with it?
 *
 * Yes for a link that is switched on and has nothing to sign in with: no
 * session, no stored password, and no password in the box. A link switched
 * off can be saved without one -- nothing will try to sign in. And a link
 * whose server or bot has changed starts from nothing, however signed in it
 * was before: the service drops the old session, so the old sign-in does not
 * count.
 */
export function needsPassword(saved: SavedSignIn, draft: ChatLink, typed: string): boolean {
  if (!draft.enabled || typed !== '') return false
  const carried = saved !== null && sameAccount(saved.link, draft)
  return !(carried && (saved.signedIn || saved.hasPassword))
}

/** What the password is for, said the same way whichever case follows. */
const PASSWORD_USE = 'Used once to sign in and set up encryption, then forgotten.'

/** The sentence under the password field. */
export function passwordHint(saved: SavedSignIn, draft: ChatLink): string {
  if (saved === null) return PASSWORD_USE
  // An existing link pointed at a different server or bot: the service
  // drops the old sign-in, so say why the field has gone from "optional" to
  // "needed" rather than leaving that to the error after Save.
  if (!sameAccount(saved.link, draft)) {
    return `That is a different account from the one signed in, so it signs in again from scratch. ${PASSWORD_USE}`
  }
  if (saved.signedIn) return 'Signed in already, so this can stay blank.'
  if (saved.hasPassword) {
    return 'A password is saved and will be used the next time it signs in. Leave this blank to keep it.'
  }
  return PASSWORD_USE
}

/** The reason a draft cannot be saved for want of a password. */
export const PASSWORD_NEEDED = "the bot's password is needed to sign it in"

// ── How long a chat may be quiet ─────────────────────────────────────────

/** Minutes as the hours the form asks for. 90 minutes is 1.5 hours; nothing
 *  is rounded further than the minute it is stored in. */
export function minutesToHours(minutes: number): number {
  return Math.round((minutes / 60) * 100) / 100
}

/**
 * The hours typed into the form as the minutes stored. `null` when what is
 * there is not a number of hours -- empty, negative, not finite -- so the
 * form can say so rather than quietly saving "never" for a field somebody
 * cleared.
 */
export function hoursToMinutes(hours: number | null | undefined): number | null {
  if (hours === null || hours === undefined || !Number.isFinite(hours) || hours < 0) return null
  const minutes = Math.round(hours * 60)
  // `u32` on the Rust side; nobody means a century of quiet.
  return minutes <= 0xffff_ffff ? minutes : null
}

/** The reason `hoursToMinutes` refused. */
export const HOURS_INVALID = 'the quiet time should be a number of hours, zero or more'

/**
 * What the stored minutes mean, as one sentence for under the field.
 *
 * Zero is the unusual one -- never -- and the one somebody most needs told
 * what it does: the chat is then one long conversation until `!new`.
 */
export function quietNote(minutes: number): string {
  if (minutes <= 0) {
    return 'Never: each chat is one conversation until you type !new.'
  }
  const hours = minutesToHours(minutes)
  const span =
    minutes < 60
      ? `${minutes} ${minutes === 1 ? 'minute' : 'minutes'}`
      : `${hours} ${hours === 1 ? 'hour' : 'hours'}`
  return `A message after ${span} of quiet starts a new conversation. Type !new to start one sooner.`
}

// ── Naming a link ────────────────────────────────────────────────────────

/** "@everyday:example.org → @me:example.org": the bot, and who it answers. */
export function linkTitle(link: ChatLink): string {
  return `${link.service.userId || 'New bot'} → ${link.service.owner || '…'}`
}

/** What the history's "Matrix" marker says when hovered: where the thread
 *  began. The marker itself is just `via.service`. */
export function viaTitle(via: Via): string {
  return `Started in ${via.service}`
}
