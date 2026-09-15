// Seed mail data for the mock backend, and the pure query and mutation
// helpers `mock.ts` calls into for every mail command.
//
// About sixty threads, across the two accounts `mock.ts` already seeds
// (`acct-google`, `acct-fastmail`), spread over every mailbox role a
// provider offers plus two labels and a snoozed handful -- generated from a
// small set of realistic senders and subjects rather than typed out sixty
// times by hand. The volume is the point here, which is the one place in
// the mock data where a generator earns its keep over prose.
//
// Reconciled against the real command surface: every write below takes the
// same `threads: ThreadId[]` a batch op does and answers `Op[]`, matching
// `crates/everyday-service/src/domains/mail.rs`; `Draft` is the real,
// tagged-union-shaped record from `types.ts`, not the flat stand-in this
// file used to carry. Two contracts nobody has shipped in Rust yet are
// mocked here too, each marked with the same `TODO(i)`/`TODO(p)` this
// build's other stand-ins carry -- see `mail-api.ts`.

import type {
  AccountId,
  Draft,
  DraftId,
  MailActionByOrigin,
  MailAddress,
  MailAgentOriginKind,
  MailAttachment,
  MailCategory,
  MailInvite,
  Mailbox,
  MailboxId,
  MailboxRole,
  MailMessage,
  MailMessageDetail,
  MailMessageId,
  Op,
  OpKind,
  RecentAction,
  RemoteImageSettings,
  Thread,
  ThreadDetail,
  ThreadFilter,
  ThreadId,
  ThreadPage,
} from './types'
import { VaultError } from './types'

const GOOGLE: AccountId = 'acct-google'
const FASTMAIL: AccountId = 'acct-fastmail'
const ME: MailAddress = { name: 'Me', email: 'me@gmail.com' }

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

/** (i) TODO: a plausible calendar invitation for every seventh message
 *  from a person (never from an automated sender) -- see `types.ts`'s
 *  `MailInvite` for the contract this stands in for until the calendars
 *  agent lands `respond_to_invite` for real. */
function inviteFor(seed: number, organizer: MailAddress): MailInvite | null {
  if (seed % 7 !== 0) return null
  const startHoursFromNow = -((seed % 5) - 2) * 24
  const start = new Date(Date.now() + startHoursFromNow * 3_600_000)
  start.setHours(14, 0, 0, 0)
  const end = new Date(start.getTime() + 45 * 60_000)
  const method = seed % 21 === 0 ? 'cancel' : 'request'
  return {
    uid: `invite-${seed}`,
    method,
    summary: pick(
      ['Design review', 'Boat club committee', '1:1', 'Client call', 'Interview debrief'],
      seed,
    ),
    start: start.toISOString(),
    end: end.toISOString(),
    allDay: false,
    location: seed % 2 === 0 ? 'Meeting room 2' : null,
    organizer,
    attendees: [
      { address: organizer, response: 'accepted' },
      { address: ME, response: 'needsAction' },
    ],
    myResponse: null,
    recurrence: null,
  }
}

// ── The generator ────────────────────────────────────────────────────

interface Seeded {
  threads: Thread[]
  messages: Map<MailMessageId, MailMessage>
  bodies: Map<MailMessageId, string>
  /** Which mailbox(es) each thread sits in -- `message_mailboxes` stands in
   *  for the many-to-many a Gmail label needs. */
  threadMailboxes: Map<ThreadId, MailboxId[]>
  /** Every message's own `Body.parts`, turned into the chips
   *  `MailThread.svelte` draws -- see `attachmentsFor`. */
  attachments: Map<MailMessageId, MailAttachment[]>
  /** A thread's own outbox history, for the "recent actions" line -- see
   *  `seedRecentActions`. */
  recentActions: Map<ThreadId, RecentAction[]>
}

/** A handful of attachments for a message flagged `hasAttachments` -- an
 *  image (for the thumbnail) and, every third time, a PDF too (for the
 *  preview), one of them left `available: false` every fifth time so the
 *  "Download" path and `mockFetchAttachment` have something to demonstrate. */
function attachmentsFor(seed: number): MailAttachment[] {
  const unavailable = seed % 5 === 0
  const out: MailAttachment[] = [
    {
      index: 0,
      filename: `photo-${1 + (seed % 12)}.jpg`,
      mimeType: 'image/jpeg',
      size: 180_000 + seed * 1_200,
      contentId: null,
      inline: false,
      available: !unavailable,
    },
  ]
  if (seed % 3 === 0) {
    out.push({
      index: 1,
      filename: 'report.pdf',
      mimeType: 'application/pdf',
      size: 640_000 + seed * 3_000,
      contentId: null,
      inline: false,
      available: !unavailable,
    })
  }
  return out
}

function buildSeed(): Seeded {
  const threads: Thread[] = []
  const messages = new Map<MailMessageId, MailMessage>()
  const bodies = new Map<MailMessageId, string>()
  const threadMailboxes = new Map<ThreadId, MailboxId[]>()
  const attachments = new Map<MailMessageId, MailAttachment[]>()
  const recentActions = new Map<ThreadId, RecentAction[]>()

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
    const from = automated ? pick(AUTOMATED, i) : role === 'sent' ? ME : pick(PEOPLE, i)
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
      const sender = m === 0 ? from : m % 2 === 0 ? from : ME
      if (!participants.some((p) => p.email === sender.email)) participants.push(sender)
      const flagged = automated ? false : i % 11 === 0
      const message: MailMessage = {
        id: msgId,
        accountId: account,
        threadId,
        messageIdHeader: `${msgId}@example.com`,
        date: iso(hoursAgo),
        from: sender,
        to: [ME],
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
        // `m === 0` here, not `messageCount - 1`: this generator's `hoursAgo`
        // only ever grows, so the *first* message pushed for a thread is
        // its most recent -- the one `messagesOf`'s ascending sort puts
        // last, and the one a thread opens with expanded. An invite on any
        // other message would be real, but collapsed and unseen until
        // clicked open, which is a poor thing for seed data whose whole job
        // is to make the feature visible.
        invite: !automated && m === 0 ? inviteFor(i, sender) : null,
      }
      messages.set(msgId, message)
      bodies.set(msgId, bodyHtmlFor(sender, message.subject, i + m))
      if (message.hasAttachments) attachments.set(msgId, attachmentsFor(i))
      msgIds.push(msgId)
    }
    const last = messages.get(msgIds[msgIds.length - 1]!)!
    const unreadCount = msgIds.filter((id) => !messages.get(id)!.flags.seen).length
    const snoozedUntil = i % 13 === 0 ? iso(-24) : null // a small handful, due back tomorrow
    const starred = msgIds.some((id) => messages.get(id)!.flags.flagged)
    const hasAttachments = msgIds.some((id) => messages.get(id)!.hasAttachments)
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
      snippet: last.snippet,
      starred,
      hasAttachments,
    }
    threads.push(thread)
    threadMailboxes.set(threadId, [mailboxId(account, role)])

    // A small, deterministic slice of threads get a "recent actions" line,
    // to demonstrate every mark the plan's phase 5 asks a thread to be able
    // to show -- an assistant's own action, an MCP client's, and a failure.
    if (i % 15 === 1) {
      recentActions.set(threadId, [
        {
          kind: { type: 'archive' },
          origin: { type: 'assistant', conversation: 'demo-conversation' },
          at: iso(2),
          state: { type: 'done' },
        },
      ])
    } else if (i % 15 === 6) {
      recentActions.set(threadId, [
        {
          kind: { type: 'move', to: mailboxId(account, 'archive') },
          origin: { type: 'mcp', client: 'Claude Desktop' },
          at: iso(5),
          state: { type: 'done' },
        },
      ])
    } else if (i % 15 === 11) {
      recentActions.set(threadId, [
        {
          kind: { type: 'archive' },
          origin: { type: 'assistant', conversation: 'demo-conversation' },
          at: iso(1),
          state: {
            type: 'failed',
            permanent: true,
            message: 'the server refused: over quota',
          },
        },
      ])
    }
  }

  return { threads, messages, bodies, threadMailboxes, attachments, recentActions }
}

const seed = buildSeed()

/** Every message in `threadId`, oldest first -- the shape `mockGetThread`
 *  already builds; pulled out because several write handlers below need it
 *  too (starring and marking read/unread touch every message in a thread,
 *  not just the thread's own aggregate row). */
function messagesOf(threadId: ThreadId): MailMessage[] {
  return [...seed.messages.values()]
    .filter((m) => m.threadId === threadId)
    .sort((a, b) => a.date.localeCompare(b.date))
}

/** Recomputes `thread(id)`'s own `starred`/`hasAttachments` aggregates from
 *  its current messages -- the mock's stand-in for
 *  `everyday_store_sql::mail::write::recompute_thread`, called after
 *  anything that could have changed either (starring, unstarring). */
function recomputeThreadAggregates(id: ThreadId): void {
  const t = thread(id)
  const msgs = messagesOf(id)
  t.starred = msgs.some((m) => m.flags.flagged)
  t.hasAttachments = msgs.some((m) => m.hasAttachments)
}

// ── Drafts ───────────────────────────────────────────────────────────

const drafts: Draft[] = [
  {
    id: 'draft-1',
    accountId: GOOGLE,
    identity: 'me@gmail.com',
    inReplyTo: null,
    to: [addr('Priya Raman', 'priya@example.com')],
    cc: [],
    bcc: [],
    subject: 'Re: Q3 numbers — one more pass',
    bodyHtml: '<p>Looks right to me, one small note on the second table —</p>',
    attachments: [],
    origin: { type: 'person' },
    state: { type: 'editing' },
    createdAt: iso(6),
    updatedAt: iso(1),
  },
  {
    id: 'draft-2',
    accountId: GOOGLE,
    identity: 'me@gmail.com',
    // TODO(p): an auto-draft, in reply to the first message of thread 6 --
    // see `mail.svelte.ts`'s `#openAutoDraftIfAny`. `th-6`/`msg-…` are the
    // ids `buildSeed` mints deterministically from `nextId`'s counter, which
    // this file is the only writer of, so the reference below holds as long
    // as `buildSeed`'s loop shape does not change above it.
    inReplyTo: messagesOf('th-6')[0]?.id ?? null,
    to: [addr('Ana Ferreira', 'ana@example.com')],
    cc: [],
    bcc: [],
    subject: "Re: Ana's birthday — thoughts?",
    bodyHtml:
      '<p>Drafted a reply — the bookshop on the corner does gift wrapping, might be easiest.</p>',
    attachments: [],
    origin: { type: 'assistant', conversation: 'demo-conversation' },
    state: { type: 'editing' },
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
    if (pseudo === 'starred') return t.starred
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
  const messages: MailMessageDetail[] = messagesOf(id).map((message) => ({
    ...message,
    attachments: seed.attachments.get(message.id) ?? [],
  }))
  return { thread, messages, recentActions: seed.recentActions.get(id) ?? [] }
}

/** Fetches one part left `available: false` -- the mock's stand-in for
 *  extracting bytes from the raw message on demand. Flips `available` on
 *  the seeded attachment and hands it back, same as the real command. */
export function mockFetchAttachment(messageId: MailMessageId, index: number): MailAttachment {
  const list = seed.attachments.get(messageId)
  const part = list?.[index]
  if (!part) throw new VaultError('notFound', 'no such part of this message')
  part.available = true
  return part
}

export function mockMessageBodyHtml(messageId: MailMessageId): string {
  return seed.bodies.get(messageId) ?? '<p><em>No body.</em></p>'
}

// ── Actions ──────────────────────────────────────────────────────────
//
// Every real write command takes `threads: ThreadId[]` and answers `Op[]` --
// see `crates/everyday-service/src/domains/mail.rs`'s `batch_op`. Mocked
// the same shape here, one op per thread, already `done`: nothing in this
// build reads the returned ops back, but the shape is worth keeping honest.

function thread(id: ThreadId): Thread {
  const t = seed.threads.find((x) => x.id === id)
  if (!t) throw new VaultError('notFound', 'no such thread')
  return t
}

function opsFor(ids: ThreadId[], kind: OpKind): Op[] {
  const now = new Date().toISOString()
  return ids.map((id) => ({
    id: nextId('op'),
    accountId: thread(id).accountId,
    kind,
    target: { type: 'thread', id },
    notBefore: now,
    attempts: 0,
    lastError: null,
    state: { type: 'done' },
    origin: { type: 'person' },
    createdAt: now,
    updatedAt: now,
  }))
}

export function mockMarkRead(ids: ThreadId[]): Op[] {
  for (const id of ids) {
    thread(id).unreadCount = 0
    for (const m of messagesOf(id)) m.flags.seen = true
  }
  return opsFor(ids, { type: 'markRead' })
}

export function mockMarkUnread(ids: ThreadId[]): Op[] {
  for (const id of ids) {
    const t = thread(id)
    t.unreadCount = Math.max(1, t.unreadCount)
    const last = messagesOf(id).at(-1)
    if (last) last.flags.seen = false
  }
  return opsFor(ids, { type: 'markUnread' })
}

export function mockStar(ids: ThreadId[]): Op[] {
  for (const id of ids) {
    for (const m of messagesOf(id)) m.flags.flagged = true
    recomputeThreadAggregates(id)
  }
  return opsFor(ids, { type: 'star' })
}
export function mockUnstar(ids: ThreadId[]): Op[] {
  for (const id of ids) {
    for (const m of messagesOf(id)) m.flags.flagged = false
    recomputeThreadAggregates(id)
  }
  return opsFor(ids, { type: 'unstar' })
}

export function mockArchive(ids: ThreadId[]): Op[] {
  for (const id of ids) {
    const t = thread(id)
    seed.threadMailboxes.set(t.id, [mailboxId(t.accountId, 'archive')])
  }
  return opsFor(ids, { type: 'archive' })
}

export function mockTrash(ids: ThreadId[]): Op[] {
  for (const id of ids) {
    const t = thread(id)
    seed.threadMailboxes.set(t.id, [mailboxId(t.accountId, 'trash')])
  }
  return opsFor(ids, { type: 'trash' })
}

export function mockMoveToMailbox(ids: ThreadId[], mailbox: MailboxId): Op[] {
  for (const id of ids) seed.threadMailboxes.set(id, [mailbox])
  return opsFor(ids, { type: 'move', to: mailbox })
}

const threadLabels = new Map<ThreadId, Set<string>>()
export function mockLabel(ids: ThreadId[], label: string): Op[] {
  for (const id of ids) {
    const set = threadLabels.get(id) ?? new Set<string>()
    set.add(label)
    threadLabels.set(id, set)
  }
  return opsFor(ids, { type: 'label', label })
}
export function mockUnlabel(ids: ThreadId[], label: string): Op[] {
  for (const id of ids) threadLabels.get(id)?.delete(label)
  return opsFor(ids, { type: 'unlabel', label })
}

export function mockSnooze(ids: ThreadId[], until: string): Op[] {
  for (const id of ids) thread(id).snoozedUntil = until
  return opsFor(ids, { type: 'snooze', until })
}
export function mockUnsnooze(ids: ThreadId[]): void {
  for (const id of ids) thread(id).snoozedUntil = null
}

// ── The split inbox's category rules and summaries (phase 7) ───────────

export function mockSetThreadCategory(ids: ThreadId[], category: MailCategory): void {
  for (const id of ids) thread(id).category = category
}

export function mockRecategorizeMail(account?: AccountId | null): { changed: number } {
  let changed = 0
  for (const t of seed.threads) {
    if (account && t.accountId !== account) continue
    if (t.category == null) {
      t.category = 'other'
      changed++
    }
  }
  return { changed }
}

export function mockSummarizeThread(id: ThreadId): { summary: string } {
  const messages = messagesOf(id)
  const t = thread(id)
  return {
    summary:
      `${messages.length} message${messages.length === 1 ? '' : 's'} between ` +
      `${[...new Set(t.participants.map((p) => p.name || p.email))].join(', ')}. ` +
      `${messages[0]?.snippet ?? ''}`.trim(),
  }
}

// ── Invitations ─────────────────────────────────────────────────────

export function mockRespondToInvite(
  messageId: MailMessageId,
  response: string,
  _comment?: string,
): void {
  if (response !== 'accepted' && response !== 'tentative' && response !== 'declined') {
    throw new VaultError('invalid', 'answer accepted, tentative or declined')
  }
  const message = seed.messages.get(messageId)
  if (!message?.invite) throw new VaultError('notFound', 'no invitation on that message')
  message.invite = {
    ...message.invite,
    myResponse: response,
    attendees: message.invite.attendees.map((a) =>
      a.address.email === ME.email ? { ...a, response } : a,
    ),
  }
}

// ── What an agent did with mail ─────────────────────────────────────
//
// `mail_actions_by_origin`'s mock: a handful of the seeded threads above,
// replayed as if one kind of caller had acted on them, so Settings →
// Sharing's own list and the assistant's settings have real threads to name
// and open rather than a second, disconnected set of ids.

const ACCOUNT_ADDRESS: Record<AccountId, string> = {
  [GOOGLE]: 'me@gmail.com',
  [FASTMAIL]: 'me@fastmail.com',
}

/** Two named MCP clients, so the mock's own "grouped by client" list has
 *  more than one group to draw -- a settings panel built against only one
 *  would not catch the grouping breaking. */
const MCP_CLIENTS = ['Claude Code', 'Claude Desktop']

export function mockMailActionsByOrigin(
  kind: MailAgentOriginKind,
  limit?: number | null,
  cursor?: string | null,
): MailActionByOrigin[] {
  const take = limit ?? 50
  const states = ['done', 'done', 'done', 'failed']
  const rows: MailActionByOrigin[] = seed.threads.slice(0, 10).map((thread, i) => {
    const state = states[i % states.length]!
    const row: MailActionByOrigin = {
      opId: `op-${kind}-${thread.id}`,
      kind,
      state,
      at: iso(i * 3),
      account: ACCOUNT_ADDRESS[thread.accountId] ?? thread.accountId,
      threadId: thread.id,
      subject: thread.subject,
      lastError: state === 'failed' ? 'the server refused the request' : null,
    }
    if (kind === 'mcp') row.client = MCP_CLIENTS[i % MCP_CLIENTS.length]
    else if (kind === 'assistant') row.conversation = 'conv-mail-demo'
    else row.run = 'run-morning-brief'
    return row
  })

  const after = cursor ? rows.findIndex((r) => r.opId === cursor) + 1 : 0
  return rows.slice(after, after + take)
}

// ── Drafts and sending ───────────────────────────────────────────────

export function mockListDrafts(account: AccountId): Draft[] {
  return drafts.filter(
    (d) => d.accountId === account && d.state.type !== 'discarded' && d.state.type !== 'sent',
  )
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
  const cc = original && opts.replyAll ? original.to.filter((a) => a.email !== ME.email) : []
  const subjectPrefix = opts.forwardOf ? 'Fwd: ' : opts.inReplyTo ? 'Re: ' : ''
  const draft: Draft = {
    id: nextId('draft'),
    accountId: opts.account,
    identity: opts.account === GOOGLE ? 'me@gmail.com' : 'me@fastmail.com',
    inReplyTo: opts.inReplyTo ?? null,
    to,
    cc,
    bcc: [],
    subject: original ? `${subjectPrefix}${original.subject.replace(/^(Re|Fwd): /, '')}` : '',
    bodyHtml: original
      ? `<p></p><blockquote>${mockMessageBodyHtml(original.id)}</blockquote>`
      : '<p></p>',
    attachments: [],
    origin: { type: 'person' },
    state: { type: 'editing' },
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

export function mockDiscardDraft(id: DraftId): void {
  const d = drafts.find((x) => x.id === id)
  if (d) d.state = { type: 'discarded' }
}

const sendTimers = new Map<string, ReturnType<typeof setTimeout>>()

export function mockSendDraft(id: DraftId, delaySeconds: number): Draft {
  const d = drafts.find((x) => x.id === id)
  if (!d) throw new VaultError('notFound', 'no such draft')
  const op = nextId('op')
  d.state = { type: 'queued', op }
  const timer = setTimeout(() => {
    sendTimers.delete(id)
    if (d.state.type === 'queued') d.state = { type: 'sent' }
  }, delaySeconds * 1000)
  sendTimers.set(id, timer)
  return d
}

export function mockUndoSend(draftId: DraftId): Draft {
  const timer = sendTimers.get(draftId)
  if (!timer) throw new VaultError('invalid', 'too late to undo this send')
  clearTimeout(timer)
  sendTimers.delete(draftId)
  const d = drafts.find((x) => x.id === draftId)
  if (!d) throw new VaultError('notFound', 'no such draft')
  d.state = { type: 'editing' }
  return d
}

// ── Sync status ──────────────────────────────────────────────────────

export function mockSyncAccount(_id: AccountId): void {
  // A no-op in the mock: every message is already "synced" from the moment
  // the seed builds. Real syncing is `mailsync::wiring`'s task.
}

// ── Search and address autocomplete ─────────────────────────────────

/** A Gmail-style query, split into its operators and what is left over as
 *  plain words -- the same syntax the field's own hint offers
 *  (`MailView.svelte`'s operator hint) and the real `search_mail` is meant
 *  to answer for real once `everyday-mailindex` lands. */
interface ParsedSearchQuery {
  terms: string[]
  from?: string
  to?: string
  subject?: string
  isUnread?: boolean
  hasAttachment?: boolean
}

function parseSearchQuery(query: string): ParsedSearchQuery {
  const parsed: ParsedSearchQuery = { terms: [] }
  for (const token of query.trim().split(/\s+/).filter(Boolean)) {
    const m = /^(\w+):(.+)$/.exec(token)
    if (!m) {
      parsed.terms.push(token.toLowerCase())
      continue
    }
    const key = m[1]!.toLowerCase()
    const value = m[2]!.toLowerCase()
    if (key === 'from') parsed.from = value
    else if (key === 'to') parsed.to = value
    else if (key === 'subject') parsed.subject = value
    else if (key === 'is' && value === 'unread') parsed.isUnread = true
    else if (key === 'has' && value === 'attachment') parsed.hasAttachment = true
    else parsed.terms.push(token.toLowerCase())
  }
  return parsed
}

function addressMatches(list: MailAddress[], needle: string): boolean {
  return list.some((a) => `${a.name} ${a.email}`.toLowerCase().includes(needle))
}

export function mockSearchMail(
  query: string,
  cursor: string | null | undefined,
): { threads: Thread[]; next?: string | null } {
  const q = parseSearchQuery(query)
  const empty =
    q.terms.length === 0 && !q.from && !q.to && !q.subject && !q.isUnread && !q.hasAttachment
  const matches = empty
    ? []
    : seed.threads.filter((t) => {
        const msgs = messagesOf(t.id)
        if (q.from && !msgs.some((m) => addressMatches([m.from], q.from!))) return false
        if (q.to && !msgs.some((m) => addressMatches(m.to, q.to!))) return false
        if (q.subject && !t.subject.toLowerCase().includes(q.subject)) return false
        if (q.isUnread && t.unreadCount === 0) return false
        if (q.hasAttachment && !msgs.some((m) => m.hasAttachments)) return false
        if (q.terms.length > 0) {
          const hay = [t.subject, ...t.participants.map((p) => `${p.name} ${p.email}`)]
            .join(' ')
            .toLowerCase()
          if (!q.terms.every((term) => hay.includes(term))) return false
        }
        return true
      })
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

// ── Remote images ────────────────────────────────────────────────────

/** The one-off, in-memory grants `allow_remote_images {messageId}` makes --
 *  the standing sender/domain allow-list is `mock.ts`'s own
 *  `mockRemoteImageSettings`, since that is where `list_`/`revoke_` already
 *  lived before this file was reconciled against the real command shape. */
const oneOffImageGrants = new Set<MailMessageId>()

export function mockAllowRemoteImagesOnce(messageId: MailMessageId): void {
  oneOffImageGrants.add(messageId)
}

export function mockRemoteImagesAllowedOnce(messageId: MailMessageId): boolean {
  return oneOffImageGrants.has(messageId)
}

export type { RemoteImageSettings }
