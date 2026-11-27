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
// `crates/jatayu-service/src/domains/mail.rs`; `Draft` is the real,
// tagged-union-shaped record from `types.ts`, not the flat stand-in this
// file used to carry. Two contracts nobody has shipped in Rust yet are
// mocked here too, each marked with the same `TODO(i)`/`TODO(p)` this
// build's other stand-ins carry -- see `mail-api.ts`.

import type {
  ThreadDraft,
  AccountId,
  CategoryCount,
  Draft,
  DraftId,
  FollowUpKind,
  FollowUpList,
  InboxSender,
  InboxSendersArgs,
  ListUnsubscribe,
  MailActionByOrigin,
  MailAddress,
  MailAgentOriginKind,
  MailAttachment,
  MailActivity,
  MailCategory,
  MailCorrespondent,
  MailInvite,
  MailSender,
  MailSorting,
  MailWriting,
  Mailbox,
  MailboxId,
  MailboxRole,
  MailMessage,
  MailMessageDetail,
  MailMessageId,
  Op,
  OpKind,
  OlderSortView,
  RecentAction,
  RemoteImageSettings,
  Thread,
  ThreadDetail,
  ThreadFilter,
  ThreadId,
  ThreadPage,
  OutboxItem,
  ScheduledSend,
  SenderRef,
  UnsubscribeOffer,
  UnsubscribeOutcome,
  UnsubscribeVia,
} from './types'
import { VaultError } from './types'
import { senderMatches } from './mail'
import { mockRaiseChange } from './mock-jobs'

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
      pseudo: 'starred',
      uidvalidity: 1,
      uidnext: 1000,
      highestModseq: 1,
    },
    {
      id: `mb-${prefix}-snoozed`,
      accountId: account,
      remoteName: 'Snoozed',
      role: 'other',
      pseudo: 'snoozed',
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

/**
 * Who sends the seed's newsletters, and what their `List-Unsubscribe`
 * header offers: one of each way out `jatayu_core::mail::unsubscribe`
 * chooses between -- one click (with an email beside it, as most lists
 * have), an email only, a page only -- and a shop's shipping notices with
 * no header at all, filed under Newsletters by mistake, which is what Quick
 * Cleanup's "Move to…" is for. So the thread's Unsubscribe and every row of
 * Quick Cleanup's Newsletters have something different to say.
 */
const NEWSLETTERS: { from: MailAddress; subject: string; list: ListUnsubscribe | null }[] = [
  {
    from: addr('The Economist', 'newsletter@economist.example.com'),
    subject: 'The week in five stories',
    list: {
      links: [
        'mailto:unsubscribe@economist.example.com?subject=unsubscribe',
        'https://economist.example.com/unsubscribe?u=8f2c41',
      ],
      oneClick: true,
    },
  },
  {
    from: addr('The Morning Edition', 'hello@morning-edition.example.com'),
    subject: 'Your weekday briefing',
    list: { links: ['mailto:leave-8f2c41@lists.morning-edition.example.com?subject=unsubscribe'] },
  },
  {
    from: addr('Northwind Outfitters', 'offers@northwind.example.com'),
    subject: 'Up to 40% off, this weekend only',
    list: { links: ['https://northwind.example.com/email-preferences?u=8f2c41'] },
  },
  {
    from: addr('Amazon', 'shipment-tracking@amazon.example.com'),
    subject: 'Your order has shipped',
    list: null,
  },
]

/** Which of `NEWSLETTERS` sends each newsletter thread, in the order the
 *  generator makes them (every eighth thread from the fifth). The Inbox gets
 *  one from each of the four, and a second from The Economist that is
 *  snoozed; the rest land in Spam and Sent. */
const NEWSLETTER_RUN = [0, 1, 2, 0, 1, 3, 0]

/** GitHub's header, on every other notification thread: one click, and an
 *  email beside it. The rest carry none, so GitHub -- who writes to both
 *  accounts -- can be asked from one and not the other. */
const GITHUB_LIST: ListUnsubscribe = {
  links: [
    'mailto:unsub-8f2c41@reply.github.com',
    'https://github.com/notifications/unsubscribe-auth/8f2c41',
  ],
  oneClick: true,
}

/** A message's own copy of a sender's header, so stamping one message's
 *  `askedAt` leaves every other message's alone. */
function listHeader(list: ListUnsubscribe): ListUnsubscribe {
  return { ...list, links: [...(list.links ?? [])] }
}

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

/**
 * A reply with what it replied over still under it, the way each of the big
 * clients writes one -- `mailbody.ts` is what folds that away and takes the
 * black off it, and it has nothing to do in a preview where every message is
 * only the sender's own words.
 */
function replyHtmlFor(from: MailAddress, subject: string, seed: number, quotes: boolean): string {
  const own = bodyHtmlFor(from, subject, seed)
  if (!quotes) return own
  const earlier = pick(SNIPPETS, seed + 3)
  switch (seed % 3) {
    case 0:
      // Outlook on the desktop: black on every span, in a Word style sheet.
      return (
        `<style>p.MsoNormal{margin:0in;font-size:11.0pt;font-family:"Calibri",sans-serif}` +
        `a:link,span.MsoHyperlink{color:#0563C1}</style><div class="WordSection1">` +
        `<p class="MsoNormal"><span style="color:black">${pick(SNIPPETS, seed)} next week.</span></p>` +
        `<p class="MsoNormal"><span style="color:#1F497D">${from.name.split(' ')[0]}</span></p>` +
        `<p class="MsoNormal">&nbsp;</p>` +
        `<div style="border:none;border-top:solid #E1E1E1 1.0pt;padding:3.0pt 0in 0in 0in">` +
        `<p class="MsoNormal"><b>From: </b>${ME.name} &lt;${ME.email}&gt;<br><b>Sent: </b>` +
        `Friday, October 30, 2026 4:14 PM<br><b>To: </b>${from.name} &lt;${from.email}&gt;<br>` +
        `<b>Subject: </b>${subject}</p></div>` +
        `<p class="MsoNormal"><span style="color:black">${earlier} then.</span></p></div>`
      )
    case 1:
      return (
        `<div dir="ltr">${own}</div><br><div class="gmail_quote"><div dir="ltr" class="gmail_attr">` +
        `On Thu, Oct 29, 2026 at 9:00 AM ${ME.name} &lt;${ME.email}&gt; wrote:<br></div>` +
        `<blockquote class="gmail_quote" style="margin:0 0 0 .8ex;border-left:1px solid #ccc;` +
        `padding-left:1ex"><div dir="ltr">${earlier} then.</div></blockquote></div>`
      )
    default:
      return (
        `${own}<div id="appendonsend"></div><hr style="display:inline-block;width:98%">` +
        `<div id="divRplyFwdMsg" dir="ltr"><b>From:</b> ${ME.name}<br><b>Sent:</b> Monday<br>` +
        `<b>To:</b> ${from.name}<br><b>Subject:</b> ${subject}</div>` +
        `<div style="color:rgb(0,0,0);background-color:rgb(255,255,255)">${earlier} then.</div>`
      )
  }
}

/**
 * A newsletter as one really arrives: laid out in tables and set in the
 * dark grey its sender chose for a white page, with no page colour of its
 * own -- the shape that vanished into the dark theme before
 * `mailview.ts`'s `applyBodyTheme` gave such mail a white page.
 */
function newsletterHtmlFor(from: MailAddress, subject: string, seed: number): string {
  const ink = 'color:#363737'
  return (
    `<table role="presentation" width="100%" style="max-width:560px;margin:0 auto"><tr><td>` +
    `<h1 style="${ink};font-size:30px;line-height:1.2;margin:0 0 12px">${subject}</h1>` +
    `<table role="presentation" width="100%"><tr>` +
    `<td style="color:#757575;font-size:13px">${from.name.toUpperCase()}</td>` +
    `<td align="right"><a href="https://example.com/app" style="${ink};font-size:13px;` +
    `border:1px solid #e0e0e0;border-radius:4px;padding:8px 14px;text-decoration:none">` +
    `READ IN APP</a></td></tr></table>` +
    `<img alt="" width="300" style="display:block;height:60px;margin:0 0 12px;background:#3aa0d8" ` +
    // A fragment is all that marks this as a picture waiting on permission:
    // `mailview.ts` looks for the address a real one would have.
    `src="data:image/gif;base64,R0lGODlhAQABAAAAACH5BAEKAAEALAAAAAABAAEAAAICTAEAOw==#/mail/img/">` +
    `<p style="${ink};font-size:17px;line-height:1.6">${pick(SNIPPETS, seed)} next week. ` +
    `${pick(SNIPPETS, seed + 1)} then.</p>` +
    `<blockquote style="border-left:4px solid #3aa0d8;margin:0;padding-left:20px;` +
    `${ink};font-style:italic">${pick(SNIPPETS, seed + 2)} the end of the month.</blockquote>` +
    `<p style="${ink};font-size:17px;line-height:1.6">${pick(SNIPPETS, seed + 4)} soon.</p>` +
    `</td></tr></table>`
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
 *  "Download" path and `mockFetchAttachment` have something to demonstrate.
 *  Every fourth, the pages and the file that only saves; every seventh, a
 *  recording and a film, which the mock has no bytes for. */
function attachmentsFor(seed: number): MailAttachment[] {
  const unavailable = seed % 5 === 0
  const out: MailAttachment[] = []
  const add = (
    filename: string,
    mimeType: string,
    size: number,
    preview: MailAttachment['preview'],
  ) =>
    out.push({
      index: out.length,
      filename,
      mimeType,
      size,
      contentId: null,
      inline: false,
      available: !unavailable,
      ...(preview ? { preview } : {}),
      ...(preview === 'video' || preview === 'audio' ? { previewType: mimeType } : {}),
    })
  add(`photo-${1 + (seed % 12)}.jpg`, 'image/jpeg', 180_000 + seed * 1_200, 'image')
  if (seed % 3 === 0) add('report.pdf', 'application/pdf', 640_000 + seed * 3_000, 'pdf')
  if (seed % 4 === 1) {
    add('notes.md', 'text/markdown', 2_400, 'page')
    add('figures.csv', 'text/csv', 1_100, 'page')
    add(
      'budget.xlsx',
      'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
      48_000,
      null,
    )
  }
  if (seed % 7 === 2) {
    add('voice-memo.m4a', 'audio/mp4', 820_000, 'audio')
    add('clip.mp4', 'video/mp4', 4_200_000, 'video')
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
    // Which newsletter, or notification, this is among its own kind.
    const nth = Math.floor(i / 8)
    const letter = category === 'newsletter' ? NEWSLETTERS[pick(NEWSLETTER_RUN, nth)] : undefined
    const from = letter
      ? letter.from
      : automated
        ? pick(AUTOMATED, i)
        : role === 'sent'
          ? ME
          : pick(PEOPLE, i)
    const subject = letter
      ? letter.subject
      : automated
        ? pick(AUTOMATED_SUBJECTS, i)
        : i % 3 === 0
          ? pick(PERSONAL_SUBJECTS, i)
          : pick(WORK_SUBJECTS, i)
    // What every message of the thread says about leaving the list: the
    // newsletter's own header, GitHub's on every other notification, and
    // nothing from a person.
    const list = letter
      ? letter.list
      : category === 'notification' && nth % 2 === 0
        ? GITHUB_LIST
        : null

    const threadId: ThreadId = nextId('th')
    const messageCount = 1 + (i % 3)
    const msgIds: MailMessageId[] = []
    const participants: MailAddress[] = [from]
    for (let m = 0; m < messageCount; m++) {
      hoursAgo += 2 + (i % 5)
      const msgId: MailMessageId = nextId('msg')
      // Nobody writes back to a newsletter: an automated thread is all theirs.
      const sender = automated || m === 0 ? from : m % 2 === 0 ? from : ME
      if (!participants.some((p) => p.email === sender.email)) participants.push(sender)
      const flagged = automated ? false : i % 11 === 0
      const message: MailMessage = {
        id: msgId,
        accountId: account,
        threadId,
        messageIdHeader: `${msgId}@example.com`,
        date: iso(hoursAgo),
        from: sender,
        // What you sent went to the other side of the thread -- or, in a
        // thread you started, to somebody -- so the Overview's "who you
        // write to" has somebody to count.
        to: [
          sender.email === ME.email ? (from.email === ME.email ? pick(PEOPLE, i + 3) : from) : ME,
        ],
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
        ...(list ? { unsubscribe: listHeader(list) } : {}),
      }
      messages.set(msgId, message)
      bodies.set(
        msgId,
        category === 'newsletter'
          ? newsletterHtmlFor(sender, message.subject, i + m)
          : replyHtmlFor(sender, message.subject, i + m, m < messageCount - 1),
      )
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
      // Half the inbox has been through the decision model, so the label a
      // sorted row carries is there to look at.
      sortedAs:
        role === 'inbox' && i % 2 === 0
          ? { category: mockSortedCategoryFor(category, i), message: last.id }
          : null,
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

// ── Priority: a VIP-flagged handful ────────────────────────────────────
//
// Picked after the generator runs rather than folded into its loop: a
// handful of ordinary inbox threads from a person -- never an automated
// sender, which `category === 'important'` already rules out -- each
// rewritten to ask the reader something concrete. That is the shape a real
// "priority" thread actually has, rather than a random subject promoted
// into the tab with nothing in it worth the promotion.
function seedPriorityThreads(s: Seeded): void {
  const asks: [string, string][] = [
    [
      'Can you confirm the Saturday booking by tonight?',
      'Just need a yes or no so I can tell the venue —',
    ],
    [
      'Need your sign-off on the renewal before Friday',
      'Legal are waiting on this one, sorry to chase —',
    ],
    [
      'Are you free to look at this before the call?',
      "Won't take long, but I'd rather not wing it —",
    ],
  ]
  let n = 0
  for (const t of s.threads) {
    if (n >= asks.length) break
    if (t.category !== 'important') continue
    if (!(s.threadMailboxes.get(t.id) ?? []).some((id) => id.endsWith('-inbox'))) continue
    const [subject, snippet] = asks[n]!
    n += 1
    t.category = 'priority'
    t.subject = subject
    t.snippet = snippet
    const last = [...s.messages.values()]
      .filter((m) => m.threadId === t.id)
      .sort((a, b) => a.date.localeCompare(b.date))
      .at(-1)
    if (last) {
      last.subject = subject
      last.snippet = snippet
      s.bodies.set(last.id, bodyHtmlFor(last.from, subject, n))
    }
  }
}
seedPriorityThreads(seed)

// ── Follow-ups: two waiting on a reply, one to nudge ───────────────────
//
// Marked on the seed the way the follow-up pass marks a thread
// (`Thread.followUp`, naming the newest message and when it was sent), so
// `mail_follow_ups` below has the backend's own shape to read back. Picked by
// what they are rather than by index, so a change to the generator above
// moves which threads they are without leaving them pointing at nothing:
//
//   - waiting on a reply: the first ordinary conversation in each account's
//     Inbox -- past the newest few, which the store's own tests take as the
//     top of the list;
//   - to nudge: the first message Google sent to a person, at least three
//     days ago, that nobody has answered.
function seedFollowUps(s: Seeded): void {
  const filedIn = (t: Thread, role: MailboxRole) =>
    (s.threadMailboxes.get(t.id) ?? []).includes(mailboxId(t.accountId, role))
  const newest = (t: Thread) =>
    [...s.messages.values()]
      .filter((m) => m.threadId === t.id)
      .sort((a, b) => a.date.localeCompare(b.date))
      .at(-1)
  const mark = (t: Thread | undefined, kind: FollowUpKind) => {
    const last = t && newest(t)
    if (t && last) t.followUp = { kind, message: last.id, since: last.date }
  }
  for (const account of [GOOGLE, FASTMAIL]) {
    mark(
      s.threads.find(
        (t) =>
          t.accountId === account &&
          t.category === 'important' &&
          !t.snoozedUntil &&
          filedIn(t, 'inbox') &&
          newest(t)?.from.email !== ME.email,
      ),
      'reply',
    )
  }
  const threeDaysAgo = new Date(Date.now() - 3 * 86_400_000).toISOString()
  mark(
    s.threads.find((t) => {
      const last = newest(t)
      return (
        t.accountId === GOOGLE &&
        !t.snoozedUntil &&
        filedIn(t, 'sent') &&
        last?.from.email === ME.email &&
        PEOPLE.some((p) => last.to.some((r) => r.email === p.email)) &&
        last.date <= threeDaysAgo
      )
    }),
    'nudge',
  )
}
seedFollowUps(seed)

// ── The Overview's mail cards ────────────────────────────────────────

/** Every address the mock's accounts send as. */
const OWN = new Set(['me@gmail.com', 'me@fastmail.com'])

/**
 * `mail_activity`, over the seed: the rules `jatayu_core::insights`
 * counts by, close enough for a page to be looked at -- received and sent
 * told apart by address, spam and drafts left out, days in local time.
 */
export function mockMailActivity(from: string, to: string): MailActivity {
  const local = (iso: string) => {
    const d = new Date(iso)
    const pad = (n: number) => String(n).padStart(2, '0')
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
  }
  const days = new Map<string, { received: number; sent: number }>()
  for (
    const d = new Date(`${from}T00:00:00`);
    local(d.toISOString()) <= to;
    d.setDate(d.getDate() + 1)
  ) {
    days.set(local(d.toISOString()), { received: 0, sent: 0 })
  }
  const spam = new Set(mailboxes.filter((m) => m.role === 'spam').map((m) => m.id))
  const kinds = new Map<MailCategory | null, number>()
  const people = new Map<
    string,
    {
      name: string
      received: number
      unread: number
      sent: number
      last: string
      kinds: Map<MailCategory | null, number>
    }
  >()
  const tally = (email: string, name: string, at: string) => {
    const key = email.toLowerCase()
    const p = people.get(key) ?? {
      name: '',
      received: 0,
      unread: 0,
      sent: 0,
      last: at,
      kinds: new Map(),
    }
    // The latest name wins, the way the Rust side keeps it.
    if (name && (at >= p.last || !p.name)) p.name = name
    if (at > p.last) p.last = at
    people.set(key, p)
    return p
  }

  let received = 0
  let sent = 0
  let unread = 0
  for (const m of seed.messages.values()) {
    const day = days.get(local(m.date))
    if (!day || m.flags.draft) continue
    if ((seed.threadMailboxes.get(m.threadId) ?? []).some((id) => spam.has(id))) continue
    if (OWN.has(m.from.email.toLowerCase())) {
      sent += 1
      day.sent += 1
      for (const r of [...m.to, ...m.cc, ...m.bcc]) {
        if (!OWN.has(r.email.toLowerCase())) tally(r.email, r.name, m.date).sent += 1
      }
      continue
    }
    received += 1
    day.received += 1
    const category = m.category ?? null
    kinds.set(category, (kinds.get(category) ?? 0) + 1)
    const p = tally(m.from.email, m.from.name, m.date)
    p.received += 1
    p.kinds.set(category, (p.kinds.get(category) ?? 0) + 1)
    if (!m.flags.seen) {
      unread += 1
      p.unread += 1
    }
  }

  const order: (MailCategory | null)[] = ['important', 'other', 'newsletter', 'notification', null]
  const mostly = (k: Map<MailCategory | null, number>) =>
    order.reduce((best, c) => ((k.get(c) ?? 0) > (k.get(best) ?? 0) ? c : best), order[0]!)
  const senders: MailSender[] = [...people]
    .filter(([, p]) => p.received > 0)
    .map(([email, p]) => ({
      email,
      name: p.name,
      messages: p.received,
      unread: p.unread,
      category: mostly(p.kinds),
    }))
    .sort(
      (a, b) => b.messages - a.messages || b.unread - a.unread || a.email.localeCompare(b.email),
    )
    .slice(0, 30)
  const correspondents: MailCorrespondent[] = [...people]
    .filter(
      ([, p]) => p.sent > 0 || !['newsletter', 'notification'].includes(mostly(p.kinds) ?? ''),
    )
    .map(([email, p]) => ({
      email,
      name: p.name,
      received: p.received,
      sent: p.sent,
      last: p.last,
    }))
    .sort((a, b) => b.received + b.sent - (a.received + a.sent) || b.sent - a.sent)
    .slice(0, 100)

  return {
    received,
    sent,
    unread,
    days: [...days].map(([date, d]) => ({ date, ...d })),
    categories: order.map((category) => ({ category, messages: kinds.get(category) ?? 0 })),
    senders,
    correspondents,
  }
}

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
 *  `jatayu_store_sql::mail::write::recompute_thread`, called after
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
  if (filter?.starred != null) rows = rows.filter((t) => t.starred === filter.starred)
  if (filter?.hasAttachments != null)
    rows = rows.filter((t) => t.hasAttachments === filter.hasAttachments)
  rows = rows.slice().sort((a, b) => b.lastDate.localeCompare(a.lastDate))

  const start = cursor ? Number(cursor) : 0
  const take = limit ?? PAGE
  const page = rows.slice(start, start + take)
  const nextCursor = start + take < rows.length ? String(start + take) : null
  return { threads: page, nextCursor }
}

/** `threads_listed`: which of `threads` `mockListThreadsAcross` would find,
 *  on whichever page. */
export function mockThreadsListed(
  mailboxIds: MailboxId[],
  filter: ThreadFilter | undefined,
  threads: ThreadId[],
): ThreadId[] {
  const wanted = new Set(threads)
  return mockListThreadsAcross(mailboxIds, filter, null, seed.threads.length)
    .threads.map((t) => t.id)
    .filter((id) => wanted.has(id))
}

/** `category_counts`: `mailbox`'s threads by category, snoozed ones left
 *  out -- the same rows `mockListThreads` would list with `snoozed: false`. */
export function mockCategoryCounts(mailbox: MailboxId): CategoryCount[] {
  const rows = mockListThreads(mailbox, { snoozed: false }, null, Number.MAX_SAFE_INTEGER).threads
  const counts = new Map<MailCategory | null, CategoryCount>()
  for (const t of rows) {
    const key = t.category ?? null
    const row = counts.get(key) ?? { category: key, threads: 0, unread: 0 }
    row.threads += 1
    if (t.unreadCount > 0) row.unread += 1
    counts.set(key, row)
  }
  return [...counts.values()]
}

/**
 * `mail_follow_ups`: every thread the seed marks as waiting, `account`'s or
 * every account's -- reply ones from an Inbox, nudge ones from Sent, none
 * snoozed, reply first and newest first within each kind, as
 * `jatayu_service::mailfollowup::follow_ups` answers. A thread archived or
 * answered since has left the mailbox it was waiting in, and so the list.
 * Nothing at all with follow-ups switched off.
 */
export function mockFollowUps(account: AccountId | null): FollowUpList {
  if (mockSorting.followUp?.on === false) return { threads: [] }
  const from = (t: Thread, role: MailboxRole) =>
    (seed.threadMailboxes.get(t.id) ?? []).includes(mailboxId(t.accountId, role))
  const waiting = seed.threads.filter((t) => {
    if (!t.followUp || t.snoozedUntil != null) return false
    if (account && t.accountId !== account) return false
    return from(t, t.followUp.kind === 'reply' ? 'inbox' : 'sent')
  })
  const order = (kind: FollowUpKind) =>
    waiting
      .filter((t) => t.followUp?.kind === kind)
      .sort((a, b) => b.lastDate.localeCompare(a.lastDate))
  return { threads: [...order('reply'), ...order('nudge')].map((t) => ({ ...t })) }
}

/** `dismiss_follow_up`: gone until something new arrives in the thread. */
export function mockDismissFollowUp(id: ThreadId): void {
  const at = seed.threads.findIndex((t) => t.id === id)
  const t = seed.threads[at]
  if (!t) throw new VaultError('notFound', 'no such thread')
  // A new row, as `mockDeliverToThread` makes, so the list sees a change.
  seed.threads[at] = { ...t, followUp: null }
}

/**
 * `list_threads_across`: what `mockListThreads` answers for each of
 * `mailboxIds`, as one list, newest first, paged by the same offset cursor.
 * Deduped within the mock, which the real command does not promise across
 * pages -- harmless, since the store dedupes what it appends either way.
 */
export function mockListThreadsAcross(
  mailboxIds: MailboxId[],
  filter: ThreadFilter | undefined,
  cursor: string | null | undefined,
  limit: number | null | undefined,
): ThreadPage {
  const byId = new Map<ThreadId, Thread>()
  for (const id of mailboxIds) {
    // Every row of each, not one page: the union is what gets paged.
    for (const t of mockListThreads(id, filter, null, seed.threads.length).threads) {
      byId.set(t.id, t)
    }
  }
  const rows = [...byId.values()].sort((a, b) => b.lastDate.localeCompare(a.lastDate))
  const start = cursor ? Number(cursor) : 0
  const take = limit ?? PAGE
  const nextCursor = start + take < rows.length ? String(start + take) : null
  return { threads: rows.slice(start, start + take), nextCursor }
}

/**
 * `inbox_senders`, over the seed: every message from someone else in the
 * last `days`, in a thread still in an Inbox, not snoozed and of the kind
 * asked for, grouped by the sender's lower-cased address -- most messages
 * first, then most unread -- and narrowed to the senders `query` matches
 * before `limit` cuts it. The seed's automated senders (The Economist,
 * GitHub) and its regular correspondents each have several such messages,
 * so every period has somebody worth cleaning out.
 *
 * `unsubscribe` is how the newest counted message that kept a header would
 * be used, and `unsubscribedAt` the latest `askedAt` on any counted message
 * -- what `jatayu_core::mail::cleanup::inbox_senders` reads them from.
 */
export function mockInboxSenders({
  days,
  accounts,
  limit,
  category,
  sortedAs,
  query,
  olderThanDays,
}: InboxSendersArgs): InboxSender[] {
  const take = Math.min(20_000, Math.max(1, limit ?? 20))
  // `null` is all time, as it is to the real command.
  const since =
    days === null
      ? -Infinity
      : Date.now() - Math.min(365, Math.max(1, Math.round(days))) * 86_400_000
  // A conversation is old only if nothing in it is newer than this.
  const before = olderThanDays == null ? Infinity : Date.now() - olderThanDays * 86_400_000
  const inboxes = new Set(
    mailboxes
      .filter((m) => m.role === 'inbox' && (!accounts || accounts.includes(m.accountId)))
      .map((m) => m.id),
  )
  interface Tally {
    name: string
    messages: number
    unread: number
    latest: string
    newest: MailMessageId
    /** Thread id to its `lastDate`, for the newest-first order at the end. */
    threads: Map<ThreadId, string>
    accounts: Set<AccountId>
    /** How the newest message with a usable header would be used, and its
     *  date, so a newer one can take its place. */
    unsubscribe: UnsubscribeVia | null
    headerDate: string
    unsubscribedAt: string | null
  }
  const bySender = new Map<string, Tally>()
  for (const t of seed.threads) {
    if (t.snoozedUntil != null) continue
    if (!(seed.threadMailboxes.get(t.id) ?? []).some((id) => inboxes.has(id))) continue
    if (category && t.category !== category) continue
    if (sortedAs && t.sortedAs?.category !== sortedAs) continue
    if (new Date(t.lastDate).getTime() >= before) continue
    for (const m of messagesOf(t.id)) {
      if (new Date(m.date).getTime() < since) continue
      const email = m.from.email.toLowerCase()
      if (OWN.has(email)) continue
      const tally: Tally = bySender.get(email) ?? {
        name: '',
        messages: 0,
        unread: 0,
        latest: m.date,
        newest: m.id,
        threads: new Map(),
        accounts: new Set(),
        unsubscribe: null,
        headerDate: '',
        unsubscribedAt: null,
      }
      tally.messages += 1
      if (!m.flags.seen) tally.unread += 1
      // The newest name wins, the way `mockMailActivity` keeps it.
      if (m.from.name && (m.date >= tally.latest || !tally.name)) tally.name = m.from.name
      if (m.date > tally.latest) {
        tally.latest = m.date
        tally.newest = m.id
      }
      const way = unsubscribeWay(m.unsubscribe)
      if (way && m.date > tally.headerDate) {
        tally.unsubscribe = way.via
        tally.headerDate = m.date
      }
      const asked = m.unsubscribe?.askedAt
      if (asked && (!tally.unsubscribedAt || asked > tally.unsubscribedAt)) {
        tally.unsubscribedAt = asked
      }
      tally.threads.set(t.id, t.lastDate)
      tally.accounts.add(t.accountId)
      bySender.set(email, tally)
    }
  }
  return [...bySender]
    .map(([email, tally]): InboxSender => ({
      email,
      name: tally.name,
      messages: tally.messages,
      unread: tally.unread,
      threads: [...tally.threads].sort((a, b) => b[1].localeCompare(a[1])).map(([id]) => id),
      accounts: [...tally.accounts],
      latest: tally.latest,
      newest: tally.newest,
      // Left out when there is nothing to say, as the real answer leaves them.
      ...(tally.unsubscribe ? { unsubscribe: tally.unsubscribe } : {}),
      ...(tally.unsubscribedAt ? { unsubscribedAt: tally.unsubscribedAt } : {}),
    }))
    .sort(
      (a, b) => b.messages - a.messages || b.unread - a.unread || a.email.localeCompare(b.email),
    )
    .filter((s) => senderMatches(s, query ?? ''))
    .slice(0, take)
}

export function mockGetThread(id: ThreadId): ThreadDetail {
  const thread = seed.threads.find((t) => t.id === id)
  if (!thread) throw new VaultError('not_found', 'no such thread')
  const all: MailMessageDetail[] = messagesOf(id).map((message) => ({
    ...message,
    attachments: seed.attachments.get(message.id) ?? [],
  }))
  // As the real command: a draft's copies on the server are a card, not mail.
  const messages = all.filter((m) => !m.flags.draft)
  const copies = all.filter((m) => m.flags.draft)
  const ids = new Set(messages.map((m) => m.id))
  const threadDrafts: ThreadDraft[] = drafts
    .filter((d) => d.state.type === 'editing' && d.inReplyTo && ids.has(d.inReplyTo))
    .map((draft) => ({ draft, copies: [], ownCopy: null }))
  if (copies.length > 0) threadDrafts.push({ draft: null, copies, ownCopy: null })
  // As `sending_replies`: on its way, or sent and its copy not synced yet.
  const sending = drafts.filter(
    (d) =>
      d.inReplyTo &&
      ids.has(d.inReplyTo) &&
      ((d.state.type === 'queued' && !sendTimers.get(d.id)?.scheduled) ||
        (d.state.type === 'sent' && undelivered.has(d.id))),
  )
  return {
    thread,
    messages,
    recentActions: seed.recentActions.get(id) ?? [],
    drafts: threadDrafts,
    sending,
  }
}

/** The mock keeps no server copies, so there is never one to adopt. */
export function mockEditThreadDraft(): Draft {
  throw new VaultError('notFound', 'that draft has been deleted')
}

export function mockDeleteThreadDraft(draft: DraftId | null): void {
  if (draft) mockDiscardDraft(draft)
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

/** `preview_attachment`, mocked: a small document per kind of page, close
 *  enough to what `mailpreview::page` draws to work on the viewer with. */
export function mockPreviewAttachment(messageId: MailMessageId, index: number): { html: string } {
  const part = seed.attachments.get(messageId)?.[index]
  if (!part) throw new VaultError('notFound', 'no such part of this message')
  if (part.preview !== 'page') {
    throw new VaultError('invalid', 'this attachment cannot be shown as a page')
  }
  const inner = part.filename?.endsWith('.csv')
    ? '<table class="sheet"><thead><tr><th>Month</th><th>Visits</th><th>Note</th></tr></thead>' +
      '<tbody><tr><td>January</td><td>1,204</td><td>New site</td></tr>' +
      '<tr><td>February</td><td>1,388</td><td>Newsletter, "spring"</td></tr>' +
      '<tr><td>March</td><td>1,921</td><td></td></tr></tbody></table>'
    : '<div class="doc"><h1>Meeting notes</h1><p>Agreed to move the launch to <strong>May</strong>.</p>' +
      '<ul><li>Ann drafts the announcement</li><li>Bob checks the budget</li></ul>' +
      '<pre><code>launch: 2026-05-04</code></pre></div>'
  const style =
    'body{margin:0;padding:16px;font-family:sans-serif;font-size:15px;line-height:1.6}' +
    '.doc{max-width:46em;margin:0 auto}table{border-collapse:collapse;font-size:13px}' +
    'th,td{border:1px solid rgba(128,128,128,.35);padding:4px 8px;text-align:left}' +
    'th{background:rgba(128,128,128,.14)}pre{background:rgba(128,128,128,.14);padding:10px 12px}'
  return {
    html:
      `<!DOCTYPE html><html><head><meta charset="utf-8">` +
      `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'">` +
      `<base target="_blank"><style>${style}</style></head><body>${inner}</body></html>`,
  }
}

export function mockMessageBodyHtml(messageId: MailMessageId): string {
  return seed.bodies.get(messageId) ?? '<p><em>No body.</em></p>'
}

// ── Actions ──────────────────────────────────────────────────────────
//
// Every real write command takes `threads: ThreadId[]` and answers `Op[]` --
// see `crates/jatayu-service/src/domains/mail.rs`'s `batch_op`. Mocked
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

/** `mailbox`, when its mail may be deleted for good -- what the real
 *  backend refuses everything else with. */
function emptiableMailbox(mailbox: MailboxId): Mailbox {
  const box = mailboxes.find((m) => m.id === mailbox)
  if (!box || (box.role !== 'trash' && box.role !== 'spam')) {
    throw new VaultError(
      'invalid',
      `mail in ${box?.role ?? 'that mailbox'} cannot be deleted forever`,
    )
  }
  return box
}

/** Out of each of `from` the thread is filed in -- `delete_forever`. */
export function mockDeleteForever(ids: ThreadId[], from: MailboxId[]): Op[] {
  for (const id of from) emptiableMailbox(id)
  const ops: Op[] = []
  for (const id of ids) {
    for (const box of from) {
      const filed = seed.threadMailboxes.get(id) ?? []
      if (!filed.includes(box)) continue
      seed.threadMailboxes.set(
        id,
        filed.filter((m) => m !== box),
      )
      ops.push(...opsFor([id], { type: 'deleteForever', from: box, messages: [] }))
    }
  }
  return ops
}

/** Every thread filed in `mailbox` -- `empty_mailbox`. */
export function mockEmptyMailbox(mailbox: MailboxId): number {
  emptiableMailbox(mailbox)
  let count = 0
  for (const [id, filed] of seed.threadMailboxes) {
    if (!filed.includes(mailbox)) continue
    seed.threadMailboxes.set(
      id,
      filed.filter((m) => m !== mailbox),
    )
    count++
  }
  return count
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
  // A person filed it: the model's label is not theirs, as the store drops it.
  for (const id of ids) Object.assign(thread(id), { category, sortedAs: null })
}

/** `set_thread_priority` -- this thread only, never the standing per-sender
 *  rule `mockSetThreadCategory(ids, 'priority')` already is. Unflagging
 *  always lands on `important` rather than whatever it was before: the same
 *  simplification `mail.setPriority`'s own optimistic patch makes, so the
 *  mock and the store never disagree about what the toggle's other side is. */
export function mockSetThreadPriority(ids: ThreadId[], priority: boolean): void {
  for (const id of ids) {
    Object.assign(thread(id), { category: priority ? 'priority' : 'important', sortedAs: null })
  }
}

// ── Mail writing (what writing help is told) ───────────────────────────

/** `MailWriting::default()`: learning from sent replies is on from the start. */
let mockWriting: MailWriting = { instructions: '', useCalendar: false, learn: true }

export function mockMailWriting(): MailWriting {
  return structuredClone(mockWriting)
}

export function mockSaveMailWriting(writing: MailWriting): void {
  // The check `MailWriting::validate` makes, so the pane's error is real.
  if (writing.instructions.length > 4_000) {
    throw new VaultError(
      'invalid',
      'that is too long to go with every reply; keep it under 4000 characters',
    )
  }
  // And its checks before sending: at most 12, each one short line.
  const checks = writing.sendChecks ?? []
  if (checks.length > 12) {
    throw new VaultError('invalid', 'there can be 12 checks before sending at most')
  }
  for (const check of checks) {
    const text = check.text.trim()
    if (!text) throw new VaultError('invalid', 'a check before sending needs some words')
    if (/[\r\n]/.test(text)) throw new VaultError('invalid', 'a check before sending is one line')
    if ([...text].length > 200) {
      throw new VaultError(
        'invalid',
        'a check before sending is a short line; keep it under 200 characters',
      )
    }
  }
  mockWriting = structuredClone(writing)
}

// ── Mail sorting (a decision model's categories) ───────────────────────

/** Which of `MOCK_SORTING`'s categories a seeded thread was "sorted" into. */
function mockSortedCategoryFor(category: MailCategory, i: number): string {
  switch (category) {
    case 'priority':
      return 'needs-you'
    case 'important':
      return i % 4 === 2 ? 'needs-you' : 'people'
    case 'newsletter':
      return 'newsletters'
    case 'notification':
      return i % 3 === 0 ? 'receipts' : 'notifications'
    default:
      return 'other'
  }
}

/** `MailSorting::default()` in `crates/jatayu-core/src/mail/sorting.rs`. */
export function mockDefaultSorting(): MailSorting {
  const c = (id: string, name: string, description: string, treatAs: MailCategory) => ({
    id,
    name,
    description,
    treatAs,
  })
  return {
    categories: [
      c(
        'needs-you',
        'Needs you',
        "A real person directly asks you to do, answer or decide something; a deadline or a time-sensitive matter involves you; or someone is waiting on you. Not: FYI updates, anything automated, threads you are only cc'd on, or a pitch from somebody you have never written to, however personal or urgent it sounds.",
        'priority',
      ),
      c(
        'people',
        'People',
        'Written by a real person, to you, and worth reading: conversations, replies, introductions and updates from people you know or work with, that do not need anything from you soon. Not: sales, recruiting or partnership pitches from people you have never written to.',
        'important',
      ),
      c(
        'newsletters',
        'Newsletters',
        'Mail you subscribed to or were added to a list for: newsletters, digests, blog posts, announcements, marketing and promotions.',
        'newsletter',
      ),
      c(
        'receipts',
        'Receipts',
        'Orders, receipts, invoices, payment confirmations, bills and shipping or delivery updates.',
        'notification',
      ),
      c(
        'notifications',
        'Notifications',
        'Automated messages from apps and services: alerts, account and security notices, sign-in codes, calendar notices, comments and mentions.',
        'notification',
      ),
      c(
        'pitches',
        'Pitches',
        'Unsolicited mail from somebody you have never written to who wants something from you: sales, partnerships, investment or advertising offers, recruiting, link building, PR, podcast or event pitches, requests for a demo or a quick call, and their "just following up" emails.',
        'other',
      ),
      c('other', 'Other', 'Anything that fits none of the other categories.', 'other'),
    ],
    windowDays: 30,
    followUp: { on: true, windowDays: 14, nudgeAfterDays: 3 },
  }
}

let mockSorting: MailSorting = mockDefaultSorting()
const mockOlderSorts = new Map<AccountId, OlderSortView>()

export function mockMailSorting(): MailSorting {
  return structuredClone(mockSorting)
}

export function mockSaveMailSorting(sorting: MailSorting): void {
  // The checks `MailSorting::validate` makes, so the pane's errors are real.
  if (sorting.categories.length === 0) {
    throw new VaultError('invalid', 'mail sorting needs at least one category')
  }
  const names = new Set<string>()
  for (const [i, c] of sorting.categories.entries()) {
    const name = c.name.trim()
    if (!name) throw new VaultError('invalid', `category ${i + 1} needs a name`)
    if (names.has(name.toLowerCase())) {
      throw new VaultError('invalid', `there are two categories called ${name}`)
    }
    names.add(name.toLowerCase())
  }
  if (sorting.windowDays < 1 || sorting.windowDays > 90) {
    throw new VaultError('invalid', 'mail is sorted unasked for between 1 and 90 days back')
  }
  // `FollowUpSettings::validate`; absent reads as the defaults, as serde has it.
  const followUp = { on: true, windowDays: 14, nudgeAfterDays: 3, ...sorting.followUp }
  const { windowDays, nudgeAfterDays } = followUp
  if (!Number.isInteger(windowDays) || windowDays < 1 || windowDays > 60) {
    throw new VaultError('invalid', 'follow-ups look back between 1 and 60 days')
  }
  if (!Number.isInteger(nudgeAfterDays) || nudgeAfterDays < 1 || nudgeAfterDays >= windowDays) {
    throw new VaultError(
      'invalid',
      `a nudge waits at least a day, and less than the ${windowDays} days follow-ups look back`,
    )
  }
  mockSorting = structuredClone({ ...sorting, followUp })
}

export function mockOlderSortViews(): OlderSortView[] {
  return [...mockOlderSorts.values()].map((v) => ({ ...v }))
}

export function mockSortOlderMail(
  accounts: AccountId[],
  days: number | null,
  again: boolean,
): void {
  const since = days == null ? null : new Date(Date.now() - days * 86_400_000).toISOString()
  for (const accountId of accounts) {
    mockOlderSorts.set(accountId, {
      accountId,
      since,
      again,
      asked: 0,
      started: new Date().toISOString(),
    })
  }
}

export function mockStopSortingOlderMail(account?: AccountId | null): void {
  if (account) mockOlderSorts.delete(account)
  else mockOlderSorts.clear()
}

/** `scheduled_sends` -- every draft `mockSendDraft` queued far enough out
 *  to be a "send later" rather than an ordinary send's own brief undo
 *  window (`QueuedSend.scheduled`), and that has not since been undone,
 *  sent or discarded -- soonest first, filtered by account when given. */
export function mockScheduledSends(account?: AccountId | null): ScheduledSend[] {
  const out: ScheduledSend[] = []
  for (const [draftId, queued] of sendTimers) {
    if (!queued.scheduled) continue
    const d = drafts.find((x) => x.id === draftId)
    if (!d || d.state.type !== 'queued') continue
    if (account && d.accountId !== account) continue
    out.push({ draft: d, sendAt: queued.at })
  }
  return out.sort((a, b) => a.sendAt.localeCompare(b.sendAt))
}

/** `outbox` -- the mock never retries a send, so its Outbox is what a server
 *  refused (`mockSendDraft`'s `refused.example`), newest first. */
export function mockOutbox(account?: AccountId | null): OutboxItem[] {
  return drafts
    .filter(
      (d) => d.state.type === 'editing' && d.sendError && (!account || d.accountId === account),
    )
    .sort((a, b) => b.updatedAt.localeCompare(a.updatedAt))
    .map((draft) => ({ draft, attempts: 0 }))
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
  /** The account's signature, which `mock.ts` is the one holding. */
  signatureHtml?: string
}): Draft {
  const now = new Date().toISOString()
  // The shape `compose::blank_with_signature` writes.
  const signature = opts.signatureHtml?.replace(/<[^>]*>/g, '').trim()
    ? `<div data-signature="">${opts.signatureHtml}</div>`
    : ''
  const source = opts.inReplyTo ?? opts.forwardOf
  const original = source ? seed.messages.get(source) : undefined
  // Only a reply knows who it is to, as `new_draft` has it: a forward starts
  // with nobody.
  const to = original && opts.inReplyTo ? [original.from] : []
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
    // The shape `quote_html` in `jatayu-mail/src/compose.rs` writes, so
    // `mailwrite.ts`'s `splitQuoted` finds the quote here as it does there.
    bodyHtml: original
      ? `<p></p>${signature}<p>On ${new Date(original.date).toDateString()}, ${original.from.name || original.from.email} wrote:</p>` +
        `<blockquote type="cite">${mockMessageBodyHtml(original.id)}</blockquote>`
      : `<p></p>${signature}`,
    attachments: [],
    origin: { type: 'person' },
    state: { type: 'editing' },
    createdAt: now,
    updatedAt: now,
  }
  // Handed to the composer and not kept, as `new_draft` does: there is no row
  // until the first save, which is the case a blank draft being thrown away
  // has to get right.
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

export function mockRestoreDraft(id: DraftId): void {
  const d = drafts.find((x) => x.id === id)
  if (d?.state.type === 'discarded') d.state = { type: 'editing' }
}

/**
 * One draft `mockSendDraft` is waiting to actually send.
 *
 * `at` is reconstructed from `delaySeconds` rather than carried over from
 * whatever `sendAt` the caller first had in mind: `mock.ts`'s own
 * `send_draft` case already collapses the two into one delay before calling
 * this, the same precedence the real `send_draft` gives them, so there is no
 * `sendAt` left here to read back out. Computed at (almost) the same instant
 * the caller's own delay was, so `at` lands within the same rounded second
 * `delaySeconds` already did -- accurate enough for a list that only ever
 * shows a moment to the nearest minute.
 *
 * `scheduled` is what tells `mockScheduledSends` apart from the ordinary
 * few-second undo window every send -- "now" or "later" alike -- opens: a
 * delay past the backend's own 30s undo-window clamp (`undo_send_delay`
 * in `mail.rs`) is a "send later", never merely the pause before an
 * ordinary send goes out.
 */
interface QueuedSend {
  /** `undefined` for the one seeded at load -- see `seedScheduledSend`. */
  timer: ReturnType<typeof setTimeout> | undefined
  at: string
  scheduled: boolean
}

/** Past this many seconds, a queued send is a "send later", not the
 *  ordinary undo window -- see `QueuedSend.scheduled`. */
const UNDO_WINDOW_MAX_S = 30

const sendTimers = new Map<DraftId, QueuedSend>()

/**
 * A message landing in a thread, as a sync pass stores one -- and, as a sync
 * pass does, saying so with a thread change that names no thread. What sending
 * a reply comes to, a moment later; exported for a test to deliver one itself.
 */
export function mockDeliverToThread(
  threadId: ThreadId,
  from: MailAddress,
  html: string,
  seen: boolean,
): MailMessage {
  const at = seed.threads.findIndex((t) => t.id === threadId)
  const thread = seed.threads[at]
  if (!thread) throw new VaultError('notFound', 'no such thread')
  const last = messagesOf(threadId).at(-1)!
  const id: MailMessageId = nextId('msg')
  const now = new Date().toISOString()
  const message: MailMessage = {
    ...last,
    id,
    messageIdHeader: `${id}@example.com`,
    date: now,
    from,
    to: last.from.email === from.email ? last.to : [last.from],
    subject: thread.subject,
    snippet: html
      .replace(/<[^>]*>/g, ' ')
      .trim()
      .slice(0, 120),
    flags: { ...last.flags, seen, draft: false },
    hasAttachments: false,
    invite: null,
  }
  seed.messages.set(id, message)
  seed.bodies.set(id, html)
  // A new row, not this one changed: the real backend answers with a fresh
  // object every time, and the list only redraws a row it has not seen.
  seed.threads[at] = {
    ...thread,
    messageCount: thread.messageCount + 1,
    lastDate: now,
    snippet: message.snippet,
    unreadCount: thread.unreadCount + (seen ? 0 : 1),
    // Its newest message is another now -- answered, or answered again -- so
    // what it was waiting on is over, as `recompute_thread` has it.
    followUp: null,
  }
  mockRaiseChange({ kind: 'thread', op: 'updated', origin: 'mock-sync' })
  return message
}

/** A delayed message: one that arrives now and is dated long ago. For a test. */
export function mockBackdate(id: MailMessageId, date: string): void {
  const message = seed.messages.get(id)
  if (message) seed.messages.set(id, { ...message, date })
}

/** The server's copy of a draft, as autosave uploads one. For a test. */
export function mockMarkDraft(id: MailMessageId): void {
  const message = seed.messages.get(id)
  if (message) seed.messages.set(id, { ...message, flags: { ...message.flags, draft: true } })
}

/** Sent drafts whose copy has not "synced" yet -- see `mockSendDraft`. */
const undelivered = new Set<DraftId>()
/** How long the mock's server takes to hand a sent message back. */
const SENT_COPY_MS = 4000

function mockDeliverSent(draft: Draft): void {
  const parent = draft.inReplyTo ? seed.messages.get(draft.inReplyTo) : undefined
  if (!parent) return
  mockDeliverToThread(parent.threadId, { name: 'Me', email: draft.identity }, draft.bodyHtml, true)
}

/** `sendAt`, when given, is kept verbatim as the send's moment, the way
 *  `send_draft` in Rust uses it -- not re-derived from `delaySeconds`,
 *  which the mock's own latency would otherwise shift by a second. */
export function mockSendDraft(id: DraftId, delaySeconds: number, sendAt?: string): Draft {
  const d = drafts.find((x) => x.id === id)
  if (!d) throw new VaultError('notFound', 'no such draft')
  const op = nextId('op')
  d.state = { type: 'queued', op }
  d.sendError = null
  const at = sendAt ?? new Date(Date.now() + delaySeconds * 1000).toISOString()
  const timer = setTimeout(() => {
    sendTimers.delete(id)
    if (d.state.type !== 'queued') return
    // A recipient at `refused.example` stands in for a server saying no: the
    // draft comes back, saying why, as the outbox's permanent failure does.
    const refused = [...d.to, ...d.cc, ...d.bcc].find((a) => a.email.endsWith('@refused.example'))
    if (refused) {
      d.state = { type: 'editing' }
      d.sendError = `550 5.1.1 <${refused.email}>: Recipient address rejected: User unknown`
      mockRaiseChange({ kind: 'draft', op: 'updated', id: d.id, origin: 'mock-outbox' })
      return
    }
    d.state = { type: 'sent' }
    d.sendError = null
    // Sent now, its copy a sync later -- the gap the thread fills with the
    // draft itself.
    undelivered.add(d.id)
    mockRaiseChange({ kind: 'draft', op: 'updated', id: d.id, origin: 'mock-outbox' })
    setTimeout(() => {
      undelivered.delete(d.id)
      mockDeliverSent(d)
    }, SENT_COPY_MS)
  }, delaySeconds * 1000)
  sendTimers.set(id, { timer, at, scheduled: delaySeconds > UNDO_WINDOW_MAX_S })
  return d
}

/** `move_draft`: a new draft on `account`, with what was written; the old one
 *  thrown away. `identity` is already one of `account`'s addresses. */
export function mockMoveDraft(edited: Draft, account: AccountId, identity: string): Draft {
  // Never saved yet: only what was written moves, as `Vault::move_draft` has it.
  const old = drafts.find((x) => x.id === edited.id) ?? { ...edited }
  if (old.state.type !== 'editing') {
    throw new VaultError('invalid', 'a draft that is not being written cannot move')
  }
  const now = new Date().toISOString()
  const moved: Draft = {
    ...edited,
    id: nextId('draft'),
    accountId: account,
    identity,
    state: { type: 'editing' },
    sendError: null,
    createdAt: old.createdAt,
    updatedAt: now,
  }
  old.state = { type: 'discarded' }
  drafts.push(moved)
  return moved
}

export function mockUndoSend(draftId: DraftId): Draft {
  const queued = sendTimers.get(draftId)
  if (!queued) throw new VaultError('invalid', 'too late to undo this send')
  clearTimeout(queued.timer)
  sendTimers.delete(draftId)
  const d = drafts.find((x) => x.id === draftId)
  if (!d) throw new VaultError('notFound', 'no such draft')
  d.state = { type: 'editing' }
  return d
}

// Seeds one scheduled send -- a draft queued for tomorrow morning -- so
// `scheduled_sends` and the Scheduled nav row have something real to show
// the moment the mock starts, the same reason `drafts` above seeds two
// editing drafts rather than none. No timer: one started at module load
// keeps Node's event loop alive, and every `ui/scripts` test that imports
// this file then never exits. A send due tomorrow morning would not fire
// during a session anyway.
function seedScheduledSend(): void {
  const draft: Draft = {
    id: nextId('draft'),
    accountId: GOOGLE,
    identity: 'me@gmail.com',
    inReplyTo: null,
    to: [addr('Marcus Webb', 'marcus.webb@example.com')],
    cc: [],
    bcc: [],
    subject: 'Notes from the client call',
    bodyHtml: '<p>Sending this over first thing — a few follow-ups inside.</p>',
    attachments: [],
    origin: { type: 'person' },
    state: { type: 'queued', op: nextId('op') },
    createdAt: iso(2),
    updatedAt: iso(2),
  }
  drafts.push(draft)
  const at = new Date()
  at.setDate(at.getDate() + 1)
  at.setHours(8, 0, 0, 0)
  sendTimers.set(draft.id, { timer: undefined, at: at.toISOString(), scheduled: true })
}
seedScheduledSend()

// ── Sync status ──────────────────────────────────────────────────────

export function mockSyncAccount(_id: AccountId): void {
  // A no-op in the mock: every message is already "synced" from the moment
  // the seed builds. Real syncing is `mailsync::wiring`'s task.
}

// ── Search and address autocomplete ─────────────────────────────────

/** A Gmail-style query, split into its operators and what is left over as
 *  plain words -- the same syntax the field's own hint offers
 *  (`MailView.svelte`'s operator hint) and the real `search_mail` is meant
 *  to answer for real once `jatayu-mailindex` lands. */
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
): { threads: Thread[]; places: Record<ThreadId, MailboxRole[]>; next?: string | null } {
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
  const inbox: MailboxRole[] = ['inbox']
  const places = Object.fromEntries(threads.map((t) => [t.id, inbox]))
  return { threads, places, next: start + 20 < matches.length ? String(start + 20) : null }
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

// ── Unsubscribing and re-filing senders ──────────────────────────────
//
// The rules `jatayu_core::mail::unsubscribe` and the service's
// `mail::unsubscribe` keep, over the seed's headers (`NEWSLETTERS`,
// `GITHUB_LIST`): one click when offered with an https link, then an email,
// then a page left for the person to open. Nothing is sent anywhere; a
// sender "asked" has `askedAt` stamped on the message used, as the real
// command stamps it, so Quick Cleanup and the thread say so afterwards.

/** One way out of a list, as `ListUnsubscribe::method` would take it: `to`
 *  is where it goes, for a person to read, and `url` the page or POST
 *  address when there is one. */
interface Way {
  via: UnsubscribeVia
  to: string
  url?: string
}

/** The host a link is on, or `''` for one that does not parse. */
function hostOf(link: string): string {
  try {
    return new URL(link).hostname
  } catch {
    return ''
  }
}

/** The first https link, or with `plain` the first http one when there is
 *  no https -- `web_link` in `jatayu_core::mail::unsubscribe`. */
function webLinkOf(header: ListUnsubscribe, plain: boolean): string | undefined {
  const links = header.links ?? []
  const secure = links.find((l) => l.startsWith('https://'))
  return secure ?? (plain ? links.find((l) => l.startsWith('http://')) : undefined)
}

/** The one address a header's first usable `mailto:` link writes to. Any
 *  `to`, `cc` or `bcc` in the link is ignored, as the real reading ignores
 *  them. */
function mailtoOf(header: ListUnsubscribe): string | undefined {
  for (const link of header.links ?? []) {
    if (!link.startsWith('mailto:')) continue
    let to = ''
    try {
      to = decodeURIComponent(link.slice('mailto:'.length).split('?')[0] ?? '')
    } catch {
      continue
    }
    const first = to.split(',')[0]?.trim().toLowerCase()
    if (first && first.includes('@')) return first
  }
  return undefined
}

/** How `header` would be used, best first, or `null` when it names no way
 *  out at all. One click needs an https link to POST to; a page may be
 *  plain http. */
function unsubscribeWay(header: ListUnsubscribe | null | undefined): Way | null {
  if (!header) return null
  const secure = webLinkOf(header, false)
  if (header.oneClick && secure) return { via: 'oneClick', to: hostOf(secure), url: secure }
  const address = mailtoOf(header)
  if (address) return { via: 'mail', to: address }
  const page = webLinkOf(header, true)
  if (page) return { via: 'web', to: hostOf(page), url: page }
  return null
}

/** A message whose header offers a way out, with both to hand. */
interface Usable {
  message: MailMessage
  header: ListUnsubscribe
  way: Way
}

/** `m`, when its header offers a way out. */
function withWay(m: MailMessage): Usable | null {
  const header = m.unsubscribe
  const way = unsubscribeWay(header)
  return header && way ? { message: m, header, way } : null
}

/** `threadId`'s messages from anybody but the person, drafts left out,
 *  newest first: the ones an unsubscribe can be about. */
function othersNewestFirst(threadId: ThreadId): MailMessage[] {
  return messagesOf(threadId)
    .filter((m) => !m.flags.draft && !OWN.has(m.from.email.toLowerCase()))
    .reverse()
}

/** `unsubscribe_offer {thread}`: what unsubscribing from the thread's
 *  sender would do -- the newest message in it, not a draft and not the
 *  person's own, whose header offers a way out -- or `null` when none
 *  does. */
export function mockUnsubscribeOffer(id: ThreadId): UnsubscribeOffer | null {
  // An unknown thread is an error, as it is to every other thread command.
  thread(id)
  for (const m of othersNewestFirst(id)) {
    const found = withWay(m)
    if (!found) continue
    const email = m.from.email.toLowerCase()
    return {
      message: m.id,
      account: m.accountId,
      email,
      name: m.from.name || email,
      via: found.way.via,
      to: found.way.to,
      ...(found.header.askedAt ? { askedAt: found.header.askedAt } : {}),
    }
  }
  return null
}

/**
 * `unsubscribe {threads, senders}`: one outcome per (account, sender), as
 * the service's `targets_in_threads` groups them. With `senders`, only
 * their messages count, each one sender however many threads it wrote in;
 * without, each thread names one sender -- the newest message, not a draft
 * and not the person's own, that offers a way out, or failing that the
 * newest such message at all, which is then told it named none.
 *
 * One click and an email always "work" here, and stamp `askedAt` on the
 * message used; a page is handed back to open, unasked. Senders asked come
 * first and senders who named no way out after, each by name, in the real
 * command's order.
 * No change is raised: the real one is this window's own write, which its
 * live router ignores.
 */
export function mockUnsubscribe(
  threads: ThreadId[],
  senders: string[] | null,
): UnsubscribeOutcome[] {
  const only = senders ? new Set(senders.map((s) => s.trim().toLowerCase())) : null
  /** By account and sender, the messages that might say how to leave. */
  const groups = new Map<string, MailMessage[]>()
  const add = (m: MailMessage) => {
    const key = `${m.accountId}\n${m.from.email.toLowerCase()}`
    groups.set(key, [...(groups.get(key) ?? []), m])
  }
  for (const id of new Set(threads)) {
    const others = othersNewestFirst(id)
    if (only) {
      for (const m of others) if (only.has(m.from.email.toLowerCase())) add(m)
    } else {
      const named = others.find((m) => withWay(m)) ?? others[0]
      if (named) add(named)
    }
  }

  const now = new Date().toISOString()
  const asked: UnsubscribeOutcome[] = []
  const unoffered: UnsubscribeOutcome[] = []
  for (const list of groups.values()) {
    list.sort((a, b) => b.date.localeCompare(a.date))
    const newest = list[0]
    if (!newest) continue
    const email = newest.from.email.toLowerCase()
    const found = list.map(withWay).find((w) => w !== null)
    if (!found) {
      unoffered.push({
        account: newest.accountId,
        email,
        name: newest.from.name || email,
        done: false,
        error: 'their mail names no way to unsubscribe',
      })
      continue
    }
    const { message, header, way } = found
    const who = { account: message.accountId, email, name: message.from.name || email }
    if (way.via === 'web') {
      // Opening a page is not asking: what happens there is the person's.
      asked.push({ ...who, via: 'web', done: false, url: way.url ?? null })
      continue
    }
    message.unsubscribe = { ...header, askedAt: now }
    asked.push({ ...who, via: way.via, done: true })
  }
  const order = (a: UnsubscribeOutcome, b: UnsubscribeOutcome) =>
    a.name.toLowerCase().localeCompare(b.name.toLowerCase()) ||
    a.email.localeCompare(b.email) ||
    a.account.localeCompare(b.account)
  return [...asked.sort(order), ...unoffered.sort(order)]
}

/**
 * `set_sender_category {senders, category}`: every message from each
 * (account, sender) filed under `category`, and every thread whose newest
 * message from somebody else is theirs moved there with it -- as
 * `mockSetThreadCategory` moves one, the model's own label dropped, since a
 * person filed it. Answers how many messages are from them.
 */
export function mockSetSenderCategory(senders: SenderRef[], category: MailCategory): number {
  const named = new Set(senders.map((s) => `${s.account}\n${s.email.trim().toLowerCase()}`))
  const theirs = (m: MailMessage) => named.has(`${m.accountId}\n${m.from.email.toLowerCase()}`)
  let moved = 0
  for (const m of seed.messages.values()) {
    if (m.flags.draft || !theirs(m)) continue
    m.category = category
    moved += 1
  }
  for (const t of seed.threads) {
    const newest = othersNewestFirst(t.id)[0]
    if (newest && theirs(newest)) Object.assign(t, { category, sortedAs: null })
  }
  return moved
}
