// Unsubscribing, in words: what pressing Unsubscribe will do, said before
// anybody is asked, and how it went, said after.
//
// The service decides the way out (`jatayu_core::mail::unsubscribe`): an
// RFC 8058 one-click request when the sender offers one, then an email to
// the list from the person's own address, then a page for them to finish in
// the browser. These helpers only put that into sentences, so Mail's thread
// view, Quick Cleanup and the Feed all say it the same way.

import { plural } from './format'
import type { UnsubscribeOffer, UnsubscribeOutcome, UnsubscribeVia } from './types'

/** The button for one sender: their site is named when that is the only way. */
export function unsubscribeLabel(via: UnsubscribeVia | null | undefined): string {
  return via === 'web' ? 'Unsubscribe on their site' : 'Unsubscribe'
}

/** What unsubscribing from one sender will do, for the confirm. */
export function offerDetail(offer: UnsubscribeOffer): string {
  const who = offer.name || offer.email
  switch (offer.via) {
    case 'oneClick':
      return `${offer.to || who} is asked to take you off ${who}'s list. Nothing opens in your browser.`
    case 'mail':
      return `An unsubscribe email goes to ${offer.to} from your address.`
    case 'web':
      return `${who} only unsubscribes on their website, so ${offer.to || 'their page'} opens in your browser for you to finish there.`
  }
}

/** What unsubscribing from many senders will do, for the confirm. */
export function manyDetail(count: number): string {
  return `${count === 1 ? 'The sender is' : `Each of the ${count} senders is`} asked to take you off their list, by a one-click request or an email from your address, whichever their mail offers. Anyone who only unsubscribes on a web page is listed afterwards for you to open.`
}

/** "Asked on 26 Nov" for a sender already asked, or `null`. */
export function askedLine(askedAt: string | null | undefined, now = new Date()): string | null {
  if (!askedAt) return null
  const at = new Date(askedAt)
  if (Number.isNaN(at.getTime())) return null
  const sameYear = at.getFullYear() === now.getFullYear()
  const day = at.toLocaleDateString(undefined, {
    day: 'numeric',
    month: 'short',
    ...(sameYear ? {} : { year: 'numeric' }),
  })
  return `You asked to unsubscribe on ${day}`
}

/** Senders asked, pages left to open, and senders it could not reach. */
export function sortOutcomes(outcomes: readonly UnsubscribeOutcome[]): {
  done: UnsubscribeOutcome[]
  pages: UnsubscribeOutcome[]
  failed: UnsubscribeOutcome[]
} {
  return {
    done: outcomes.filter((o) => o.done),
    pages: outcomes.filter((o) => !o.done && !!o.url),
    failed: outcomes.filter((o) => !o.done && !o.url),
  }
}

/** Whether anything is left for the person: a page to open, or a failure to read. */
export function needsAttention(outcomes: readonly UnsubscribeOutcome[]): boolean {
  return outcomes.some((o) => !o.done)
}

/** A toast's title and body for a set of outcomes. */
export function summarize(outcomes: readonly UnsubscribeOutcome[]): {
  title: string
  body?: string
  ok: boolean
} {
  const { done, pages, failed } = sortOutcomes(outcomes)
  const rest: string[] = []
  if (pages.length)
    rest.push(
      `${plural(pages.length, 'sender only unsubscribes', 'senders only unsubscribe')} on their website`,
    )
  if (failed.length) rest.push(`couldn't unsubscribe from ${plural(failed.length, 'sender')}`)
  const body = rest.length ? sentence(rest.join('; ')) : undefined
  const [only] = done
  if (only) {
    const title =
      done.length === 1
        ? `Unsubscribed from ${only.name || only.email}`
        : `Unsubscribed from ${plural(done.length, 'sender')}`
    return { title, body, ok: true }
  }
  if (pages.length && !failed.length) {
    return { title: sentence(rest[0] ?? ''), ok: true }
  }
  if (!outcomes.length) return { title: 'Nothing to unsubscribe from', ok: false }
  return { title: "Couldn't unsubscribe", body, ok: false }
}

function sentence(s: string): string {
  return s ? s.charAt(0).toUpperCase() + s.slice(1) + '.' : s
}
