// The mock answers for writing help -- `suggest_replies`, `draft_with_ai`
// and `improve_writing` -- so the interface can be driven in a browser
// without a model behind it. Each keeps the shape of a short, plain email
// rather than anything that reads as generated: a greeting, a line or two,
// a sign-off, the same as `mock-mail.ts`'s own seeded messages do. And what
// writing help has learned from the replies sent -- `voice_learning` and the
// two commands that change it.

import { mockGetThread, mockMailWriting } from './mock-mail'
import { VaultError } from './types'
import type {
  ImproveMode,
  MailMessageDetail,
  NeedsReply,
  ReplySuggestion,
  ReplySuggestions,
  ThreadId,
  VoiceLearningView,
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

/** `needs_reply`: the backend's free rules (`mailfollowup::waits_on_reply`),
 *  then what the follow-up pass decided, then a stand-in for the decision
 *  model -- a question in the newest message -- after about as long as a
 *  provider takes to answer one. */
export async function mockNeedsReply(id: ThreadId): Promise<NeedsReply> {
  const { thread, messages } = mockGetThread(id)
  const newest = messages.at(-1)
  if (!newest || isOwnAddress(newest.from.email)) return { needed: false }
  const filed = newest.category ?? thread.category
  const addressed = [...newest.to, ...newest.cc].some((a) => isOwnAddress(a.email))
  if (
    filed === 'newsletter' ||
    filed === 'notification' ||
    /no-?reply|notifications?@|alerts?@/i.test(newest.from.email) ||
    !addressed
  ) {
    return { needed: false }
  }
  if (thread.followUp?.message === newest.id) return { needed: thread.followUp.kind === 'reply' }
  await delay(300)
  return { needed: /\?/.test(`${newest.snippet} ${thread.subject}`) }
}

export async function mockSuggestReplies(id: ThreadId): Promise<ReplySuggestions> {
  // Long enough to see the shimmering placeholders this is answering for.
  // As the real command: nothing for a thread waiting on no reply.
  if (!(await mockNeedsReply(id)).needed) return { suggestions: [] }
  await delay(700)
  const { messages } = mockGetThread(id)
  const newest = messages.at(-1)
  if (!newest) return { suggestions: [] }
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
  spoken?: boolean
}): Promise<WrittenText> {
  await delay(500)
  // Words dictated arrive with the fillers a person speaks with; the real
  // model is told to ignore them, so the stand-in drops the plainest ones.
  const instruction = opts.spoken
    ? opts.instruction.replace(/\b(um+|uh+|er+|you know)\b,?\s*/gi, '')
    : opts.instruction
  const lines = ['Hi,', ...sentencesFromInstruction(instruction), 'Best,']
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

// ── What it has learned from the replies sent ──────────────────────────

/** `MAX_NOTES_CHARS` in `crates/karmayoga-core/src/mail/learning.rs`. */
const MAX_NOTES_CHARS = 2_000

/** Nothing learned yet, as a vault starts. */
let learning: VoiceLearningView = { notes: '', learnedAt: null, repliesRead: 0, edited: false }

export function mockVoiceLearning(): VoiceLearningView {
  return { ...learning }
}

/** `save_voice_learning`: the notes as the person left them, or -- empty --
 *  everything learned forgotten, the count with it. */
export function mockSaveVoiceLearning(notes: string): VoiceLearningView {
  const kept = notes.trim()
  if (kept.length > MAX_NOTES_CHARS) {
    throw new VaultError(
      'invalid',
      `that is too long to go with every reply; keep it under ${MAX_NOTES_CHARS} characters`,
    )
  }
  if (!kept) {
    learning = { notes: '', learnedAt: null, repliesRead: 0, edited: false }
  } else if (kept !== learning.notes) {
    learning = { ...learning, notes: kept, edited: true, learnedAt: new Date().toISOString() }
  }
  return mockVoiceLearning()
}

/** What a pass "learns": the sort of rules a model writes about how somebody
 *  replies, a line each, the newest pass rewriting the whole set. */
const LEARNED_NOTES = [
  'Replies are short: two or three sentences, rarely more.',
  'Opens with the first name and no greeting word ("Priya —"), never "Hope you are well".',
  'Says yes or no in the first sentence, then the one reason.',
  'Signs off "Thanks" with colleagues and with nothing at all with family.',
  'Turns down meetings before 10 and offers a time instead of asking for one.',
]

/** `learn_from_replies`: refused, saying why, as the real pass is when
 *  learning is off; otherwise the notes rewritten, and a few more replies
 *  read. Takes a moment, the way a model does. */
export async function mockLearnFromReplies(): Promise<VoiceLearningView> {
  if (mockMailWriting().learn === false) {
    throw new VaultError(
      'invalid',
      'learning from your replies is switched off under Settings → Mail writing',
    )
  }
  await delay(900)
  const read = learning.repliesRead + 4 + (learning.repliesRead % 3)
  learning = {
    notes: LEARNED_NOTES.slice(0, Math.min(LEARNED_NOTES.length, 3 + (read % 3))).join('\n'),
    learnedAt: new Date().toISOString(),
    repliesRead: read,
    edited: false,
  }
  return mockVoiceLearning()
}
