// Seed mail data for the mock backend, and the pure query and mutation
// helpers `mock.ts` calls into for every mail command.
//
// Kept in its own file, as `docs/plans/mail.md` asks: if agent (a) lands a
// real `mock-mail.ts` of their own first, this file is expected to be
// replaced by theirs wholesale rather than merged line by line, and
// `mock.ts`'s "── Mail ──" section is written to call only the handful of
// functions exported here, so that swap costs one import line.
//
// About sixty threads, across the two accounts `mock.ts` already seeds
// (`acct-google`, `acct-fastmail`), spread over every mailbox role a
// provider offers plus two labels and a snoozed handful -- generated from a
// small set of realistic senders and subjects rather than typed out sixty
// times by hand. The volume is the point here, which is the one place in
// the mock data where a generator earns its keep over prose.

import type {
  AccountId,
  MailAddress,
  Mailbox,
  MailboxId,
  MailboxRole,
  MailCategory,
  MailMessage,
  MailMessageId,
  Thread,
  ThreadDetail,
  ThreadFilter,
  ThreadId,
  ThreadPage,
} from './types'
import { VaultError } from './types'
import type { Draft, SyncStatus } from './mail-api'

const GOOGLE: AccountId = 'acct-google'
const FASTMAIL: AccountId = 'acct-fastmail'

let seq = 0
function nextId(prefix: string): string {
  seq += 1
  return `${prefix}-${seq}`
}

function iso(hoursAgo: number): string {
  return new Date(Date.now() - hoursAgo * 3_600_000).toISOString()
}

function addr(name: string, email: string): MailAddress {
  return { name, email }
}

// ── Mailboxes ──────────────────────────────────────────────────────────
//
// One row per role IMAP gives every account, plus two labels per account so
// `MailNav` has folders to draw under the fixed rows, and a "Starred" and
// "Snoozed" pseudo-mailbox each. Those two are not a real IMAP mailbox --
// nothing in `docs/plans/mail.md`'s data model gives a thread a
// mailbox-independent "starred" view -- so they are a mock-only affordance
// that lets `MailNav` and `mail.svelte.ts` reach them through the ordinary
// `listMailboxes`/`listThreads` pair rather than a third code path. `role`
// is `'other'`, matching what a real cross-folder saved search would be.

function mailboxSet(account: AccountId, prefix: string): Mailbox[] {
  const fixed: [MailboxRole, string][] = [
    ['inbox', 'Inbox'],
    ['sent', 'Sent'],
    ['drafts', 'Drafts'],
    ['archive', 'Archive'],
    ['spam', 'Spam'],
    ['trash', 'Trash'],
  ]
  const boxes: Mailbox[] = fixed.map(([role, name]) => ({
    id: `mb-${prefix}-${role}`,
    accountId: account,
    remoteName: name,
    role,
    uidvalidity: 1,
    uidnext: 1000,
    highestModseq: 1,
  }))
  const folders = prefix === 'g' ? ['Receipts', 'Team'] : ['Boat club']
  for (const name of folders) {
    boxes.push({
      id: `mb-${prefix}-${name.toLowerCase().replace(/\s+/g, '-')}`,
      accountId: account,
      remoteName: name,
      role: 'other',
      uidvalidity: 1,
      uidnext: 1000,
      highestModseq: 1,
    })
  }
  // The two pseudo-mailboxes -- see the module doc above.
  boxes.push(
    {
      id: `mb-${prefix}-starred`,
      accountId: account,
      remoteName: 'Starred',
      role: 'other',
      uidvalidity: 1,
      uidnext: 1000,
      highestModseq: 1,
    },
    {
      id: `mb-${prefix}-snoozed`,
      accountId: account,
      remoteName: 'Snoozed',
      role: 'other',
      uidvalidity: 1,
      uidnext: 1000,
      highestModseq: 1,
    },
  )
  return boxes
}

const mailboxes: Mailbox[] = [...mailboxSet(GOOGLE, 'g'), ...mailboxSet(FASTMAIL, 'f')]

function mailboxId(account: AccountId, role: MailboxRole): MailboxId {
  const prefix = account === GOOGLE ? 'g' : 'f'
  return `mb-${prefix}-${role}`
}

/** True for the two mailboxes that read across every real one. */
function isPseudo(id: MailboxId): 'starred' | 'snoozed' | null {
  if (id.endsWith('-starred')) return 'starred'
  if (id.endsWith('-snoozed')) return 'snoozed'
  return null
}

// ── Senders, subjects and bodies ──────────────────────────────────────

const PEOPLE: MailAddress[] = [
  addr('Priya Raman', 'priya@example.com'),
  addr('Marcus Webb', 'marcus.webb@example.com'),
  addr('Sofia Alonso', 'sofia@example.com'),
  addr('Tom Fenwick', 'tom.fenwick@example.com'),
  addr('Nadia Osei', 'nadia@example.com'),
  addr('Ben Carrow', 'ben@example.com'),
  addr('Yuki Tanaka', 'yuki.tanaka@example.com'),
  addr('Dad', 'dad@example.com'),
  addr('Ana Ferreira', 'ana@example.com'),
  addr('Harold at the boat club', 'harold@boatclub.example.com'),
]

const AUTOMATED: MailAddress[] = [
  addr('GitHub', 'notifications@github.com'),
  addr('Figma', 'notify@figma.com'),
  addr('Amazon', 'shipment-tracking@amazon.example.com'),
  addr('Stripe', 'receipts@stripe.example.com'),
  addr('The Economist', 'newsletter@economist.example.com'),
  addr('Linear', 'notify@linear.app'),
  addr('National Rail', 'noreply@nationalrail.example.com'),
  addr('Companies House', 'no-reply@companieshouse.example.com'),
]

const WORK_SUBJECTS = [
  'Re: Q3 numbers — one more pass',
  'Design review moved to Thursday',
  'Can you look at the deploy before I do',
  'Notes from the client call',
  'Re: the migration plan',
  'Quick one before you go',
  'Onboarding doc needs a second pair of eyes',
  'Re: renewal — a few questions',
  'Interview debrief: the 2pm candidate',
]

const PERSONAL_SUBJECTS = [
  'Re: Saturday',
  'Photos from the weekend',
  'Flights for the wedding',
  'Re: are you free next week',
  'Boat club AGM — Thursday 8pm',
  'Recipe you asked about',
  "Ana's birthday — thoughts?",
  'Re: the leak in the utility room',
]

const AUTOMATED_SUBJECTS = [
  'Your weekly digest',
  'A new pull request needs your review',
  'Your order has shipped',
  'Receipt for your payment',
  'The week in five stories',
  'Issue assigned to you',
  'Your train is on time',
  'Confirmation statement due',
]

const SNIPPETS = [
  'Thanks for sending this over — a couple of thoughts before we finalise it,',
  'Just circling back on this, did you get a chance to',
  'Attaching the file we talked about. Let me know if',
  'Quick update: this is now scheduled for',
  'No rush on this, but wanted to flag it before',
  "Here's what I've got so far. Happy to walk through",
]

function pick<T>(arr: T[], i: number): T {
  return arr[i % arr.length]!
}

function paragraph(seed: number): string {
  return `<p>${pick(SNIPPETS, seed)} next week. ${pick(SNIPPETS, seed + 3)} then.</p>`
}

function bodyHtmlFor(from: MailAddress, subject: string, seed: number): string {
  return (
    `<p>Hi,</p>` +
    paragraph(seed) +
    `<p>Best,<br>${from.name.split(' ')[0]}</p>` +
    `<blockquote>On ${new Date(iso(seed * 5 + 48)).toDateString()}, someone wrote:<br>` +
    `&gt; ${subject}</blockquote>`
  )
}

// ── The generator ────────────────────────────────────────────────────

interface Seeded {
  threads: Thread[]
  messages: Map<MailMessageId, MailMessage>
  bodies: Map<MailMessageId, string>
  /** Which mailbox(es) each thread sits in -- `message_mailboxes` stands in
   *  for the many-to-many a Gmail label needs. */
  threadMailboxes: Map<ThreadId, MailboxId[]>
}

function buildSeed(): Seeded {
  const threads: Thread[] = []
  const messages = new Map<MailMessageId, MailMessage>()
  const bodies = new Map<MailMessageId, string>()
  const threadMailboxes = new Map<ThreadId, MailboxId[]>()

  let hoursAgo = 1
  const N = 60
  for (let i = 0; i < N; i++) {
    const account = i % 3 === 0 ? FASTMAIL : GOOGLE
    // Roughly: half inbox, then a spread across the other mailboxes so every
    // one of them has something in it worth looking at.
    const bucket = i % 10
    const role: MailboxRole =
      bucket < 5
        ? 'inbox'
        : bucket < 7
          ? 'sent'
          : bucket === 7
            ? 'archive'
            : bucket === 8
              ? 'spam'
              : 'trash'
    const automated = i % 4 === 0
    const category: MailCategory = automated
      ? i % 8 === 0
        ? 'notification'
        : 'newsletter'
      : i % 5 === 0
        ? 'other'
        : 'important'
    const from = automated
      ? pick(AUTOMATED, i)
      : role === 'sent'
        ? addr('Me', 'me@example.com')
        : pick(PEOPLE, i)
    const subject = automated
      ? pick(AUTOMATED_SUBJECTS, i)
      : i % 3 === 0
        ? pick(PERSONAL_SUBJECTS, i)
        : pick(WORK_SUBJECTS, i)

    const threadId: ThreadId = nextId('th')
    const messageCount = 1 + (i % 3)
    const msgIds: MailMessageId[] = []
    const participants: MailAddress[] = [from]
    for (let m = 0; m < messageCount; m++) {
      hoursAgo += 2 + (i % 5)
      const msgId: MailMessageId = nextId('msg')
      const sender = m === 0 ? from : m % 2 === 0 ? from : addr('Me', 'me@example.com')
      if (!participants.some((p) => p.email === sender.email)) participants.push(sender)
      const flagged = automated ? false : i % 11 === 0
      const message: MailMessage = {
        id: msgId,
        accountId: account,
        threadId,
        messageIdHeader: `${msgId}@example.com`,
        date: iso(hoursAgo),
        from: sender,
        to: [addr('Me', 'me@example.com')],
        cc: [],
        bcc: [],
        replyTo: [],
        subject: m === 0 ? subject : `Re: ${subject.replace(/^Re: /, '')}`,
        snippet: pick(SNIPPETS, i + m).slice(0, 120),
        flags: {
          seen: role === 'sent' || i % 3 !== 0 || m < messageCount - 1,
          answered: m > 0,
          flagged,
          draft: false,
          deleted: role === 'trash',
        },
        labels: [],
        hasAttachments: i % 9 === 0 && m === messageCount - 1,
        size: 4200 + i * 37,
        category,
        pack: { account: account, pack: `pack-${account}`, offset: 0, len: 0 },
      }
      messages.set(msgId, message)
      bodies.set(msgId, bodyHtmlFor(sender, message.subject, i + m))
      msgIds.push(msgId)
    }
    const last = messages.get(msgIds[msgIds.length - 1]!)!
    const unreadCount = msgIds.filter((id) => !messages.get(id)!.flags.seen).length
    const starred = msgIds.some((id) => messages.get(id)!.flags.flagged)
    const snoozedUntil = i % 13 === 0 ? iso(-24) : null // a small handful, due back tomorrow
    const thread: Thread = {
      id: threadId,
      accountId: account,
      subject,
      participants,
      lastDate: last.date,
      messageCount,
      unreadCount,
      category,
      snoozedUntil,
      starred,
      hasAttachments: msgIds.some((id) => messages.get(id)!.hasAttachments),
      draftedByAssistant: i % 17 === 0,
      snippet: last.snippet,
    }
    threads.push(thread)
    threadMailboxes.set(threadId, [mailboxId(account, role)])
  }

  return { threads, messages, bodies, threadMailboxes }
}

const seed = buildSeed()

// ── Drafts ───────────────────────────────────────────────────────────

const drafts: Draft[] = [
  {
    id: 'draft-1',
    accountId: GOOGLE,
    identity: null,
    inReplyTo: null,
    forwardOf: null,
    to: [addr('Priya Raman', 'priya@example.com')],
    cc: [],
    bcc: [],
    subject: 'Re: Q3 numbers — one more pass',
    bodyHtml: '<p>Looks right to me, one small note on the second table —</p>',
    attachments: [],
    origin: 'person',
    state: 'editing',
    createdAt: iso(6),
    updatedAt: iso(1),
  },
  {
    id: 'draft-2',
    accountId: GOOGLE,
    identity: null,
    inReplyTo: null,
    forwardOf: null,
    to: [addr('Ana Ferreira', 'ana@example.com')],
    cc: [],
    bcc: [],
    subject: "Re: Ana's birthday — thoughts?",
    bodyHtml:
      '<p>Drafted a reply — the bookshop on the corner does gift wrapping, might be easiest.</p>',
    attachments: [],
    origin: 'assistant',
    state: 'editing',
    createdAt: iso(3),
    updatedAt: iso(3),
  },
]

// ── Queries ──────────────────────────────────────────────────────────

export function mockListMailboxes(account: AccountId): Mailbox[] {
  return mailboxes.filter((m) => m.accountId === account)
}

const PAGE = 50

export function mockListThreads(
  mailbox: MailboxId,
  filter: ThreadFilter | undefined,
  cursor: string | null | undefined,
  limit: number | null | undefined,
): ThreadPage {
  const pseudo = isPseudo(mailbox)
  const account = mailboxes.find((m) => m.id === mailbox)?.accountId
  let rows = seed.threads.filter((t) => {
    if (account && t.accountId !== account) return false
    if (pseudo === 'starred') return t.starred === true
    if (pseudo === 'snoozed') return t.snoozedUntil != null
    return (seed.threadMailboxes.get(t.id) ?? []).includes(mailbox)
  })
  if (filter?.unread != null) rows = rows.filter((t) => t.unreadCount > 0 === filter.unread)
  if (filter?.category != null) rows = rows.filter((t) => t.category === filter.category)
  if (filter?.snoozed != null)
    rows = rows.filter((t) => (t.snoozedUntil != null) === filter.snoozed)
  rows = rows.slice().sort((a, b) => b.lastDate.localeCompare(a.lastDate))

  const start = cursor ? Number(cursor) : 0
  const take = limit ?? PAGE
  const page = rows.slice(start, start + take)
  const nextCursor = start + take < rows.length ? String(start + take) : null
  return { threads: page, nextCursor }
}

export function mockGetThread(id: ThreadId): ThreadDetail {
  const thread = seed.threads.find((t) => t.id === id)
  if (!thread) throw new VaultError('notFound', 'no such thread')
  const messages = [...seed.messages.values()]
    .filter((m) => m.threadId === id)
    .sort((a, b) => a.date.localeCompare(b.date))
  return { thread, messages }
}

export function mockMessageBodyHtml(messageId: MailMessageId): string {
  return seed.bodies.get(messageId) ?? '<p><em>No body.</em></p>'
}

// ── Actions ──────────────────────────────────────────────────────────
//
// Each mutates the thread in place and returns it, the way `mock.ts`'s other
// domains answer a write with the row as it now stands.

function thread(id: ThreadId): Thread {
  const t = seed.threads.find((x) => x.id === id)
  if (!t) throw new VaultError('notFound', 'no such thread')
  return t
}

export function mockMarkRead(id: ThreadId): void {
  const t = thread(id)
  t.unreadCount = 0
  for (const m of seed.messages.values()) if (m.threadId === id) m.flags.seen = true
}

export function mockMarkUnread(id: ThreadId): void {
  const t = thread(id)
  t.unreadCount = Math.max(1, t.unreadCount)
  const last = [...seed.messages.values()].filter((m) => m.threadId === id).at(-1)
  if (last) last.flags.seen = false
}

export function mockStar(id: ThreadId): void {
  thread(id).starred = true
}
export function mockUnstar(id: ThreadId): void {
  thread(id).starred = false
}

export function mockArchive(id: ThreadId): void {
  const t = thread(id)
  seed.threadMailboxes.set(t.id, [mailboxId(t.accountId, 'archive')])
}

export function mockTrash(id: ThreadId): void {
  const t = thread(id)
  seed.threadMailboxes.set(t.id, [mailboxId(t.accountId, 'trash')])
}

export function mockMoveToMailbox(id: ThreadId, mailbox: MailboxId): void {
  seed.threadMailboxes.set(id, [mailbox])
}

const threadLabels = new Map<ThreadId, Set<string>>()
export function mockLabel(id: ThreadId, label: string): void {
  const set = threadLabels.get(id) ?? new Set<string>()
  set.add(label)
  threadLabels.set(id, set)
}
export function mockUnlabel(id: ThreadId, label: string): void {
  threadLabels.get(id)?.delete(label)
}

export function mockSnooze(id: ThreadId, until: string): void {
  thread(id).snoozedUntil = until
}
export function mockUnsnooze(id: ThreadId): void {
  thread(id).snoozedUntil = null
}

// ── Drafts and sending ───────────────────────────────────────────────

export function mockListDrafts(): Draft[] {
  return drafts.filter((d) => d.state !== 'discarded' && d.state !== 'sent')
}

export function mockNewDraft(opts: {
  account: AccountId
  inReplyTo?: MailMessageId
  forwardOf?: MailMessageId
  replyAll?: boolean
}): Draft {
  const now = new Date().toISOString()
  const source = opts.inReplyTo ?? opts.forwardOf
  const original = source ? seed.messages.get(source) : undefined
  const to = original ? [original.from] : []
  const cc =
    original && opts.replyAll ? original.to.filter((a) => a.email !== 'me@example.com') : []
  const subjectPrefix = opts.forwardOf ? 'Fwd: ' : opts.inReplyTo ? 'Re: ' : ''
  const draft: Draft = {
    id: nextId('draft'),
    accountId: opts.account,
    identity: null,
    inReplyTo: opts.inReplyTo ?? null,
    forwardOf: opts.forwardOf ?? null,
    to,
    cc,
    bcc: [],
    subject: original ? `${subjectPrefix}${original.subject.replace(/^(Re|Fwd): /, '')}` : '',
    bodyHtml: original
      ? `<p></p><blockquote>${mockMessageBodyHtml(original.id)}</blockquote>`
      : '<p></p>',
    attachments: [],
    origin: 'person',
    state: 'editing',
    createdAt: now,
    updatedAt: now,
  }
  drafts.push(draft)
  return draft
}

export function mockSaveDraft(draft: Draft): void {
  const at = drafts.findIndex((d) => d.id === draft.id)
  const next = { ...draft, updatedAt: new Date().toISOString() }
  if (at >= 0) drafts[at] = next
  else drafts.push(next)
}

export function mockDiscardDraft(id: string): void {
  const d = drafts.find((x) => x.id === id)
  if (d) d.state = 'discarded'
}

const sendTimers = new Map<string, ReturnType<typeof setTimeout>>()

export function mockSendDraft(id: string, delaySeconds: number): void {
  const d = drafts.find((x) => x.id === id)
  if (!d) throw new VaultError('notFound', 'no such draft')
  d.state = 'queued'
  const timer = setTimeout(() => {
    sendTimers.delete(id)
    if (d.state === 'queued') d.state = 'sent'
  }, delaySeconds * 1000)
  sendTimers.set(id, timer)
}

export function mockUndoSend(draftId: string): void {
  const timer = sendTimers.get(draftId)
  if (!timer) throw new VaultError('invalid', 'too late to undo this send')
  clearTimeout(timer)
  sendTimers.delete(draftId)
  const d = drafts.find((x) => x.id === draftId)
  if (d) d.state = 'editing'
}

// ── Sync status ──────────────────────────────────────────────────────

export function mockSyncStatus(): SyncStatus[] {
  return [
    { accountId: GOOGLE, progress: null, error: null },
    { accountId: FASTMAIL, progress: null, error: null },
  ]
}

export function mockSyncAccount(_id: AccountId): void {
  // A no-op in the mock: every message is already "synced" from the moment
  // the seed builds. Real syncing is (s)'s task.
}

// ── Search and address autocomplete ─────────────────────────────────

/**
 * `search_mail`'s answer is the same `Thread` shape `list_threads` and
 * `get_thread` use -- see `SearchMailResult` in `./types` -- not a
 * search-specific hit shape, so this returns `seed.threads` entries
 * directly rather than projecting them into something narrower.
 */
export function mockSearchMail(
  query: string,
  cursor: string | null | undefined,
): { threads: Thread[]; next?: string | null } {
  const needle = query.trim().toLowerCase()
  const matches = needle
    ? seed.threads.filter((t) => {
        const hay = [t.subject, ...t.participants.map((p) => `${p.name} ${p.email}`)]
          .join(' ')
          .toLowerCase()
        return hay.includes(needle)
      })
    : []
  const start = cursor ? Number(cursor) : 0
  const threads = matches.slice(start, start + 20)
  return { threads, next: start + 20 < matches.length ? String(start + 20) : null }
}

export function mockSuggestAddresses(prefix: string): MailAddress[] {
  const needle = prefix.trim().toLowerCase()
  if (!needle) return []
  const counts = new Map<string, { addr: MailAddress; count: number }>()
  for (const m of seed.messages.values()) {
    for (const a of [m.from, ...m.to, ...m.cc]) {
      if (!a.name.toLowerCase().startsWith(needle) && !a.email.toLowerCase().startsWith(needle))
        continue
      const entry = counts.get(a.email)
      if (entry) entry.count += 1
      else counts.set(a.email, { addr: a, count: 1 })
    }
  }
  return [...counts.values()]
    .sort((a, b) => b.count - a.count)
    .slice(0, 8)
    .map((e) => e.addr)
}

export function mockAllowRemoteImages(_messageId: MailMessageId, _forever: boolean): void {
  // A no-op in the mock: nothing here ever fetches a remote image in the
  // first place, so there is no cache to warm.
}
