// The mock answers for writing help -- `suggest_replies`, `draft_with_ai`
// and `improve_writing` -- so the interface can be driven in a browser
// without a model behind it. Each keeps the shape of a short, plain email
// rather than anything that reads as generated: a greeting, a line or two,
// a sign-off, the same as `mock-mail.ts`'s own seeded messages do.

import { mockGetThread } from './mock-mail'
import type {
  ImproveMode,
  MailMessageDetail,
  ReplySuggestion,
  ReplySuggestions,
  ThreadId,
  WrittenText,
} from './types'

/**
 * The two seeded accounts' own addresses (`mock.ts`'s `acct-google` and
 * `acct-fastmail`), named here rather than looked up: `mock.ts` already
 * imports the three functions below, so reaching back into it -- or into
 * `mock-mail.ts`'s own unexported `ME` -- for an account's address would be
 * this pair's one circular import. Two short-lived string literals cost
 * less than that.
 */
const MOCK_OWN_ADDRESSES = new Set(['me@gmail.com', 'me@fastmail.com'])

function isOwnAddress(email: string): boolean {
  return MOCK_OWN_ADDRESSES.has(email.trim().toLowerCase())
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

function firstName(message: MailMessageDetail): string {
  const name = message.from.name.trim()
  return name ? name.split(' ')[0]! : message.from.email.split('@')[0]!
}

/** Plain lines, each its own paragraph -- the same shape `quote_html`'s own
 *  `<p>` wrapping expects a draft's body in. */
function paragraphs(lines: string[]): string {
  return lines.map((l) => `<p>${l}</p>`).join('')
}

export async function mockSuggestReplies(id: ThreadId): Promise<ReplySuggestions> {
  // Long enough to see the shimmering placeholders this is answering for.
  await delay(700)
  const { messages } = mockGetThread(id)
  const newest = messages.at(-1)
  if (!newest || isOwnAddress(newest.from.email)) return { suggestions: [] }
  const name = firstName(newest)
  const suggestions: ReplySuggestion[] = [
    {
      label: 'Sounds good',
      bodyText: `Sounds good, ${name} — let's go with that.`,
      bodyHtml: paragraphs([`Sounds good, ${name} — let's go with that.`]),
    },
    {
      label: 'Not this time',
      bodyText: `Thanks for thinking of me, ${name}, but I'll have to pass this time.`,
      bodyHtml: paragraphs([
        `Thanks for thinking of me, ${name}, but I'll have to pass this time.`,
      ]),
    },
    {
      label: 'A quick question',
      bodyText: `Before I answer — could you say a bit more about the timing?`,
      bodyHtml: paragraphs([`Before I answer — could you say a bit more about the timing?`]),
    },
  ]
  return { suggestions }
}

/** Up to two short sentences out of a free-form instruction -- "yes to
 *  Thursday, ask for the agenda" becomes two; "just checking in" becomes
 *  one, the same as somebody actually would. */
function sentencesFromInstruction(instruction: string): string[] {
  const clauses = instruction
    .split(/,| and /i)
    .map((c) => c.trim())
    .filter(Boolean)
  if (clauses.length === 0) return ['Just following up on this.']
  return clauses.slice(0, 2).map((c) => {
    const bare = c.replace(/\.$/, '')
    return `${bare.charAt(0).toUpperCase()}${bare.slice(1)}.`
  })
}

export async function mockDraftWithAi(opts: {
  account: string
  inReplyTo?: string | null
  instruction: string
  currentText?: string | null
}): Promise<WrittenText> {
  await delay(500)
  const lines = ['Hi,', ...sentencesFromInstruction(opts.instruction), 'Best,']
  return { bodyText: lines.join('\n'), bodyHtml: paragraphs(lines) }
}

/** A small, visible rewrite per mode -- not a real model, but each mode
 *  still has to look like it did something different from the others. */
function transform(text: string, mode: ImproveMode): string {
  const trimmed = text.trim()
  switch (mode) {
    case 'polish':
      return trimmed.replace(/\s+/g, ' ').replace(/\s+([,.!?])/g, '$1')
    case 'shorter':
      return trimmed.split(/(?<=[.!?])\s+/)[0] || trimmed
    case 'longer':
      return (
        `${trimmed} I wanted to add a little more detail, ` +
        'so there is no doubt about what I mean.'
      )
    case 'friendlier':
      return `Hope you're doing well! ${trimmed} Thanks so much!`
    case 'formal':
      return trimmed
        .replace(/\bhi\b/gi, 'Dear')
        .replace(/\bhey\b/gi, 'Dear')
        .replace(/\bthanks\b/gi, 'Thank you')
        .replace(/\bcan't\b/gi, 'cannot')
        .replace(/\bdon't\b/gi, 'do not')
    case 'fix':
      return trimmed.replace(/(^|[.!?]\s+)i\b/g, (_m, p1: string) => `${p1}I`)
  }
}

export async function mockImproveWriting(opts: {
  account: string
  text: string
  mode: ImproveMode
  instruction?: string | null
}): Promise<WrittenText> {
  await delay(500)
  const text = transform(opts.text, opts.mode)
  return { bodyText: text, bodyHtml: paragraphs(text.split(/\n+/).filter(Boolean)) }
}
