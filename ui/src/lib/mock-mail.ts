// Mail's seed data and in-memory behaviour for `mock.ts`.
//
// Kept apart from the rest of the mock, per the plan's own instruction: the
// mail UI's own agent needs this seed and will likely grow it, and a mailbox
// of sixty threads has no business living inside the four-thousand-line file
// that mocks every other app. `mock.ts` imports the handful of functions
// below and wires them into its one big `switch`; nothing here reaches back
// into that file, so the boundary stays one-directional.
//
// Two accounts, matching the ids `mock.ts`'s own `accounts` array already
// seeds (`acct-google`, `acct-fastmail`) so a thread's `accountId` resolves
// to a real row in Settings → Accounts. Mailboxes, threads and messages are
// generated rather than hand-written — sixty of them by hand is sixty
// chances to typo a date out of order — with one thread pinned to a real
// reply chain and a few pinned to carry attachments, per the plan's own
// words for what this seed needs.

import type {
  Draft,
  DraftState,
  Mailbox,
  MailAddress,
  MailCategory,
  MailMessage,
  MailOrigin,
  Op,
  OpKind,
  OpTarget,
  Thread,
  ThreadFilter,
  ThreadPage,
} from './types'

// ── ids and dates ───────────────────────────────────────────────────────

let seq = 0
function nextId(prefix: string): string {
  seq += 1
  return `${prefix}-${seq.toString(36).padStart(4, '0')}`
}

function iso(daysAgo: number, hour = 9, minute = 0): string {
  const d = new Date()
  d.setDate(d.getDate() - daysAgo)
  d.setHours(hour, minute, 0, 0)
  return d.toISOString()
}

// ── accounts and mailboxes ───────────────────────────────────────────────

interface AccountSeed {
  account: string
  ownAddress: string
  domain: string
}

const SEED_ACCOUNTS: AccountSeed[] = [
  { account: 'acct-google', ownAddress: 'me@gmail.com', domain: 'gmail.com' },
  { account: 'acct-fastmail', ownAddress: 'me@fastmail.com', domain: 'fastmail.com' },
]

function ownAddressOf(account: string): string {
  return SEED_ACCOUNTS.find((a) => a.account === account)?.ownAddress ?? 'me@example.com'
}

function mailbox(account: string, role: Mailbox['role'], remoteName: string): Mailbox {
  return {
    id: `mb-${account}-${role}`,
    accountId: account,
    remoteName,
    role,
    uidvalidity: 1,
    uidnext: 5000,
    highestModseq: 128,
  }
}

export const MOCK_MAILBOXES: Mailbox[] = SEED_ACCOUNTS.flatMap(({ account }) => [
  mailbox(account, 'inbox', 'INBOX'),
  mailbox(account, 'sent', 'Sent'),
  mailbox(account, 'drafts', 'Drafts'),
  mailbox(account, 'archive', 'Archive'),
  mailbox(account, 'trash', 'Trash'),
])

function mailboxId(account: string, role: Mailbox['role']): string {
  return `mb-${account}-${role}`
}

// ── threads and messages ─────────────────────────────────────────────────

interface ThreadRecord {
  thread: Thread
  /** Which mailbox this thread is currently filed under -- a
   *  single-owner simplification of the real `thread_mailboxes` table,
   *  which allows a Gmail thread to be filed under several at once. */
  mailboxId: string
  messages: MailMessage[]
}

const THREADS = new Map<string, ThreadRecord>()

const SENDERS = [
  { name: 'Priya Nair', local: 'priya' },
  { name: 'Tom Baxter', local: 'tom' },
  { name: 'Newsletter', local: 'digest' },
  { name: 'Sam Okafor', local: 'sam' },
  { name: 'Billing', local: 'billing' },
  { name: 'Wren Sato', local: 'wren' },
]

const SUBJECTS = [
  'Re: quarterly numbers',
  'Your receipt',
  'Weekend plans',
  'Design review notes',
  'This week in review',
  'Can you take a look?',
  'Flight confirmation',
  'Meeting moved to Thursday',
  'Draft agenda',
  'Photos from the trip',
]

const CATEGORIES: (MailCategory | undefined)[] = [
  undefined,
  undefined,
  'important',
  'newsletter',
  'notification',
]

function address(seed: (typeof SENDERS)[number], domain: string): MailAddress {
  return { name: seed.name, email: `${seed.local}@${domain}` }
}

function message(
  account: string,
  threadId: string,
  from: MailAddress,
  to: MailAddress[],
  subject: string,
  daysAgo: number,
  opts: { seen?: boolean; flagged?: boolean; hasAttachments?: boolean; snippet?: string } = {},
): MailMessage {
  const id = nextId('msg')
  return {
    id,
    accountId: account,
    threadId,
    messageIdHeader: `<${id}@example.com>`,
    date: iso(daysAgo, 8 + (daysAgo % 10)),
    from,
    to,
    cc: [],
    bcc: [],
    replyTo: [],
    subject,
    snippet: opts.snippet ?? 'A short preview of the message body, enough for a list row.',
    flags: {
      seen: opts.seen ?? true,
      answered: false,
      flagged: opts.flagged ?? false,
      draft: false,
      deleted: false,
    },
    labels: [],
    hasAttachments: opts.hasAttachments ?? false,
    size: 4_096,
    category: undefined,
    pack: { account, pack: `pack-${account}`, offset: 0, len: 4_096 },
    gmail: null,
  }
}

function addThread(
  account: string,
  role: Mailbox['role'],
  messages: MailMessage[],
  category?: MailCategory,
) {
  const threadId = messages[0]!.threadId
  const last = messages[messages.length - 1]!
  const unreadCount = messages.filter((m) => !m.flags.seen).length
  const participants = new Map<string, MailAddress>()
  for (const m of messages) {
    participants.set(m.from.email, m.from)
    for (const t of m.to) participants.set(t.email, t)
  }
  const thread: Thread = {
    id: threadId,
    accountId: account,
    subject: last.subject,
    participants: [...participants.values()],
    lastDate: last.date,
    messageCount: messages.length,
    unreadCount,
    category,
    snoozedUntil: null,
  }
  THREADS.set(threadId, { thread, mailboxId: mailboxId(account, role), messages })
}

function generateSeed() {
  for (const { account, ownAddress, domain } of SEED_ACCOUNTS) {
    const me: MailAddress = { name: 'Me', email: ownAddress }

    // Inbox: about eighteen threads, mostly single messages, a handful
    // unread, a couple starred, a couple with an attachment.
    for (let i = 0; i < 18; i++) {
      const threadId = nextId('thread')
      const sender = SENDERS[i % SENDERS.length]!
      const subject = SUBJECTS[i % SUBJECTS.length]!
      const daysAgo = i
      const m = message(account, threadId, address(sender, domain), [me], subject, daysAgo, {
        seen: i % 4 !== 0,
        flagged: i % 7 === 0,
        hasAttachments: i % 5 === 0,
      })
      addThread(account, 'inbox', [m], CATEGORIES[i % CATEGORIES.length])
    }

    // Sent: about eight, all read, all from the account's own address.
    for (let i = 0; i < 8; i++) {
      const threadId = nextId('thread')
      const recipient = SENDERS[(i + 2) % SENDERS.length]!
      const subject = SUBJECTS[(i + 3) % SUBJECTS.length]!
      const m = message(account, threadId, me, [address(recipient, domain)], subject, i + 1, {
        seen: true,
      })
      addThread(account, 'sent', [m])
    }

    // Archive: about four, all read.
    for (let i = 0; i < 4; i++) {
      const threadId = nextId('thread')
      const sender = SENDERS[(i + 1) % SENDERS.length]!
      const subject = SUBJECTS[(i + 5) % SUBJECTS.length]!
      const m = message(account, threadId, address(sender, domain), [me], subject, i + 20, {
        seen: true,
      })
      addThread(account, 'archive', [m])
    }

    // A real reply chain in the inbox: four messages, alternating sender,
    // the kind quoting and threading actually has to handle.
    const chainId = nextId('thread')
    const chain = [
      message(account, chainId, address(SENDERS[0]!, domain), [me], 'Re: quarterly numbers', 6, {
        seen: true,
        snippet: 'Here are the numbers we discussed on the call.',
      }),
      message(account, chainId, me, [address(SENDERS[0]!, domain)], 'Re: quarterly numbers', 5, {
        seen: true,
        snippet: 'Thanks -- one question about the Q3 column.',
      }),
      message(account, chainId, address(SENDERS[0]!, domain), [me], 'Re: quarterly numbers', 4, {
        seen: true,
        snippet: 'Good catch, that was a formula error. Fixed version attached.',
        hasAttachments: true,
      }),
      message(account, chainId, me, [address(SENDERS[0]!, domain)], 'Re: quarterly numbers', 0, {
        seen: false,
        snippet: 'Perfect, this matches what finance has too.',
      }),
    ]
    addThread(account, 'inbox', chain, 'important')
  }
}

generateSeed()

// ── reading ───────────────────────────────────────────────────────────

export function listMailboxes(account: string): Mailbox[] {
  return MOCK_MAILBOXES.filter((m) => m.accountId === account)
}

export function listThreads(
  mailbox: string,
  filter: ThreadFilter | undefined,
  limit: number,
): ThreadPage {
  let list = [...THREADS.values()].filter((r) => r.mailboxId === mailbox)
  if (filter?.unread === true) list = list.filter((r) => r.thread.unreadCount > 0)
  if (filter?.unread === false) list = list.filter((r) => r.thread.unreadCount === 0)
  if (filter?.category) list = list.filter((r) => r.thread.category === filter.category)
  if (filter?.snoozed === true) list = list.filter((r) => r.thread.snoozedUntil)
  if (filter?.snoozed === false) list = list.filter((r) => !r.thread.snoozedUntil)
  list.sort((a, b) => (a.thread.lastDate < b.thread.lastDate ? 1 : -1))
  return { threads: list.slice(0, limit).map((r) => r.thread), nextCursor: null }
}

export function getThread(id: string): { thread: Thread; messages: MailMessage[] } {
  const record = THREADS.get(id)
  if (!record) throw new Error(`no such thread: ${id}`)
  return { thread: record.thread, messages: record.messages }
}

function findMessage(id: string): { message: MailMessage; record: ThreadRecord } | undefined {
  for (const record of THREADS.values()) {
    const message = record.messages.find((m) => m.id === id)
    if (message) return { message, record }
  }
  return undefined
}

// ── the outbox, faked ────────────────────────────────────────────────────

function op(accountId: string, kind: OpKind, target: OpTarget): Op {
  const now = new Date().toISOString()
  return {
    id: nextId('op'),
    accountId,
    kind,
    target,
    notBefore: now,
    attempts: 0,
    // A real backend answers `pending`: nothing has drained this yet.
    // There is no sync agent in a browser tab to ever change that, which
    // is honest rather than a gap -- the mock's whole job is the local
    // write, and the local write already happened by the time this
    // returns.
    state: { type: 'pending' },
    origin: { type: 'person' },
    createdAt: now,
    updatedAt: now,
  }
}

function forThreads(threads: string[], kind: OpKind, apply: (r: ThreadRecord) => void): Op[] {
  const ops: Op[] = []
  for (const id of threads) {
    const record = THREADS.get(id)
    if (!record) continue
    apply(record)
    ops.push(op(record.thread.accountId, kind, { type: 'thread', id }))
  }
  return ops
}

export function markRead(threads: string[]): Op[] {
  return forThreads(threads, { type: 'markRead' }, (r) => {
    r.messages.forEach((m) => (m.flags.seen = true))
    r.thread.unreadCount = 0
  })
}

export function markUnread(threads: string[]): Op[] {
  return forThreads(threads, { type: 'markUnread' }, (r) => {
    r.messages.forEach((m) => (m.flags.seen = false))
    r.thread.unreadCount = r.messages.length
  })
}

export function star(threads: string[]): Op[] {
  return forThreads(threads, { type: 'star' }, (r) =>
    r.messages.forEach((m) => (m.flags.flagged = true)),
  )
}

export function unstar(threads: string[]): Op[] {
  return forThreads(threads, { type: 'unstar' }, (r) =>
    r.messages.forEach((m) => (m.flags.flagged = false)),
  )
}

export function archive(threads: string[]): Op[] {
  return forThreads(threads, { type: 'archive' }, (r) => {
    r.mailboxId = mailboxId(r.thread.accountId, 'archive')
  })
}

export function trash(threads: string[]): Op[] {
  return forThreads(threads, { type: 'trash' }, (r) => {
    r.mailboxId = mailboxId(r.thread.accountId, 'trash')
  })
}

export function moveToMailbox(threads: string[], to: string): Op[] {
  return forThreads(threads, { type: 'move', to }, (r) => {
    r.mailboxId = to
  })
}

export function label(threads: string[], name: string): Op[] {
  return forThreads(threads, { type: 'label', label: name }, (r) =>
    r.messages.forEach((m) => {
      if (!m.labels.includes(name)) m.labels.push(name)
    }),
  )
}

export function unlabel(threads: string[], name: string): Op[] {
  return forThreads(threads, { type: 'unlabel', label: name }, (r) =>
    r.messages.forEach((m) => {
      m.labels = m.labels.filter((l) => l !== name)
    }),
  )
}

export function snooze(threads: string[], until: string): Op[] {
  return forThreads(threads, { type: 'snooze', until }, (r) => {
    r.thread.snoozedUntil = until
  })
}

export function unsnooze(threads: string[]): void {
  for (const id of threads) {
    const record = THREADS.get(id)
    if (record) record.thread.snoozedUntil = null
  }
}

// ── drafts ───────────────────────────────────────────────────────────────

const DRAFTS = new Map<string, Draft>()

const UNDO_SEND_DEFAULT_SECONDS = 10
const UNDO_SEND_MIN_SECONDS = 5
const UNDO_SEND_MAX_SECONDS = 30

function clampUndoSeconds(seconds: number | null | undefined): number {
  const value = seconds ?? UNDO_SEND_DEFAULT_SECONDS
  return Math.min(UNDO_SEND_MAX_SECONDS, Math.max(UNDO_SEND_MIN_SECONDS, value))
}

export function newDraft(
  account: string,
  inReplyTo: string | null | undefined,
  forwardOf: string | null | undefined,
  replyAll: boolean | null | undefined,
): Draft {
  const now = new Date().toISOString()
  const draft: Draft = {
    id: nextId('draft'),
    accountId: account,
    identity: ownAddressOf(account),
    inReplyTo: null,
    to: [],
    cc: [],
    bcc: [],
    subject: '',
    bodyHtml: '',
    attachments: [],
    origin: { type: 'person' } satisfies MailOrigin,
    state: { type: 'editing' } satisfies DraftState,
    createdAt: now,
    updatedAt: now,
  }

  const parentId = inReplyTo ?? forwardOf
  const found = parentId ? findMessage(parentId) : undefined
  if (found) {
    const { message: parent } = found
    const own = ownAddressOf(account).toLowerCase()
    const quoted = `<p>On ${parent.date}, ${parent.from.name || parent.from.email} wrote:</p><blockquote>${parent.snippet}</blockquote>`
    if (inReplyTo) {
      draft.inReplyTo = parent.id
      draft.subject = /^re:/i.test(parent.subject) ? parent.subject : `Re: ${parent.subject}`
      draft.to = [parent.from]
      if (replyAll) {
        for (const addr of [...parent.to, ...parent.cc]) {
          if (addr.email.toLowerCase() === own) continue
          if (draft.to.some((a) => a.email.toLowerCase() === addr.email.toLowerCase())) continue
          draft.cc.push(addr)
        }
      }
    } else {
      draft.subject = /^fwd:/i.test(parent.subject) ? parent.subject : `Fwd: ${parent.subject}`
    }
    draft.bodyHtml = quoted
  }

  return draft
}

export function saveDraft(draft: Draft): void {
  DRAFTS.set(draft.id, { ...draft, updatedAt: new Date().toISOString() })
}

export function discardDraft(id: string): void {
  const draft = DRAFTS.get(id)
  if (draft) draft.state = { type: 'discarded' }
}

export function sendDraft(
  id: string,
  delaySeconds: number | null | undefined,
  sendAt: string | null | undefined,
): Draft {
  const draft = DRAFTS.get(id)
  if (!draft) throw new Error(`no such draft: ${id}`)
  if (draft.to.length === 0 && draft.cc.length === 0 && draft.bcc.length === 0) {
    throw new Error('a message needs at least one recipient')
  }
  const notBefore =
    sendAt ?? new Date(Date.now() + clampUndoSeconds(delaySeconds) * 1000).toISOString()
  const sendOp = op(draft.accountId, { type: 'send' }, { type: 'draft', id })
  sendOp.notBefore = notBefore
  draft.state = { type: 'queued', op: sendOp.id }
  draft.updatedAt = new Date().toISOString()
  return draft
}

export function undoSend(draftId: string): Draft {
  const draft = DRAFTS.get(draftId)
  if (!draft || draft.state.type !== 'queued') {
    throw new Error('this draft is not queued to send')
  }
  draft.state = { type: 'editing' }
  draft.updatedAt = new Date().toISOString()
  return draft
}

export function listDrafts(account: string): Draft[] {
  return [...DRAFTS.values()].filter((d) => d.accountId === account)
}
