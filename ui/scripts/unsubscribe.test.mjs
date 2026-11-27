// Unsubscribing, as the interface says it and as the mock backend does it.
//
// `unsubscribe.ts` puts the service's outcomes into the sentences Mail's
// thread, Quick Cleanup and the Feed all show, so a wrong count or a page
// filed as a failure would be wrong in three places at once. `mock-mail.ts`
// is what those screens are built and tried against with no service behind
// them, so it keeps the service's rules: one outcome per sender of each
// account, one click before an email before a page, `askedAt` stamped on the
// message used -- and re-filing a sender moves their conversations, which is
// how a newsletter that is not one leaves the Newsletters tab.
//
// The mock's state is the module's own and carries from one section to the
// next, so they run in the order they are written.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

stubBrowser({ navigator: true })

const loaded = await load(['/src/lib/unsubscribe.ts', '/src/lib/mock-mail.ts'])
const [words, mock] = loaded.modules

const { askedLine, manyDetail, needsAttention, offerDetail, sortOutcomes, summarize } = words
const {
  mockGetThread,
  mockInboxSenders,
  mockListThreads,
  mockSetSenderCategory,
  mockUnsubscribe,
  mockUnsubscribeOffer,
} = mock

// ── the words ─────────────────────────────────────────────────────────────

function asked(email, name = '') {
  return {
    account: 'acct-google',
    email,
    name,
    via: 'oneClick',
    done: true,
  }
}
function page(email, name = '') {
  return {
    account: 'acct-google',
    email,
    name,
    via: 'web',
    done: false,
    url: `https://${email.split('@')[1]}/unsubscribe`,
  }
}
function unreached(email, name = '') {
  return {
    account: 'acct-google',
    email,
    name,
    done: false,
    error: 'their mail names no way to unsubscribe',
  }
}

{
  const one = summarize([asked('news@weekly.example.com', 'The Weekly')])
  assert.equal(one.title, 'Unsubscribed from The Weekly', 'one sender asked is named')
  assert.equal(one.body, undefined, 'and nothing else needs saying')
  assert.equal(one.ok, true)

  assert.equal(
    summarize([asked('news@weekly.example.com')]).title,
    'Unsubscribed from news@weekly.example.com',
    'a sender with no name is their address',
  )

  const mixed = summarize([
    asked('a@one.example.com'),
    asked('b@two.example.com'),
    page('c@three.example.com'),
    page('d@four.example.com'),
    unreached('e@five.example.com'),
  ])
  assert.equal(mixed.title, 'Unsubscribed from 2 senders', 'the senders asked are counted')
  assert.equal(
    mixed.body,
    "2 senders only unsubscribe on their website; couldn't unsubscribe from 1 sender.",
    'what is left over is said after',
  )
  assert.equal(mixed.ok, true, 'somebody was asked, so it went')

  const pages = summarize([page('c@three.example.com'), page('d@four.example.com')])
  assert.equal(pages.title, '2 senders only unsubscribe on their website.')
  assert.equal(pages.body, undefined)
  assert.equal(pages.ok, true, 'pages to open are not a failure')

  const none = summarize([])
  assert.equal(none.title, 'Nothing to unsubscribe from')
  assert.equal(none.ok, false)

  const failed = summarize([unreached('e@five.example.com')])
  assert.equal(failed.title, "Couldn't unsubscribe")
  assert.equal(failed.body, "Couldn't unsubscribe from 1 sender.")
  assert.equal(failed.ok, false)
}

{
  // A one-click request the sender refused, with only its page to fall back
  // on, is a page to open -- not a failure.
  const refused = { ...page('r@six.example.com'), via: 'oneClick' }
  const sorted = sortOutcomes([
    asked('a@one.example.com'),
    page('c@three.example.com'),
    unreached('e@five.example.com'),
    refused,
  ])
  assert.deepEqual(
    sorted.done.map((o) => o.email),
    ['a@one.example.com'],
  )
  assert.deepEqual(
    sorted.pages.map((o) => o.email),
    ['c@three.example.com', 'r@six.example.com'],
  )
  assert.deepEqual(
    sorted.failed.map((o) => o.email),
    ['e@five.example.com'],
  )

  assert.equal(needsAttention([]), false, 'nothing asked, nothing left over')
  assert.equal(needsAttention([asked('a@one.example.com')]), false, 'everyone asked')
  assert.equal(needsAttention([asked('a@one.example.com'), page('c@three.example.com')]), true)
  assert.equal(needsAttention([unreached('e@five.example.com')]), true)
}

{
  const offer = {
    message: 'msg-1',
    account: 'acct-google',
    email: 'newsletter@economist.example.com',
    name: 'The Economist',
    via: 'oneClick',
    to: 'economist.example.com',
  }
  const oneClick = offerDetail(offer)
  assert.match(oneClick, /economist\.example\.com/, 'says where the request goes')
  assert.match(oneClick, /Nothing opens/, 'and that nothing opens')

  const mail = offerDetail({ ...offer, via: 'mail', to: 'leave@lists.example.com' })
  assert.match(mail, /leave@lists\.example\.com/, 'says who the email goes to')
  assert.match(mail, /from your address/, 'and that it goes from you')

  const web = offerDetail({ ...offer, via: 'web', to: 'northwind.example.com' })
  assert.match(web, /northwind\.example\.com/)
  assert.match(web, /browser/, 'a page opens in the browser')

  const nameless = offerDetail({ ...offer, name: '', via: 'web', to: '' })
  assert.match(nameless, /newsletter@economist\.example\.com/, 'no name: the address')
  assert.match(nameless, /their page/, 'no host: still a page')

  assert.match(manyDetail(1), /^The sender is/)
  assert.match(manyDetail(3), /Each of the 3 senders/)
}

{
  assert.equal(askedLine(null), null)
  assert.equal(askedLine(undefined), null)
  assert.equal(askedLine('not a date'), null, 'a stamp that does not read is not said')
  const now = new Date('2026-11-26T12:00:00Z')
  const at = '2026-03-14T12:00:00Z'
  const day = new Date(at).toLocaleDateString(undefined, { day: 'numeric', month: 'short' })
  assert.equal(askedLine(at, now), `You asked to unsubscribe on ${day}`, 'this year: no year')
  assert.match(askedLine('2025-03-14T12:00:00Z', now), /2025/, 'another year says which')
}

// ── the mock: what the Newsletters tab offers ───────────────────────────

const newsletters = () => mockInboxSenders({ days: null, category: 'newsletter', limit: 100 })
const senderIn = (rows, email) => rows.find((s) => s.email === email)

const ECONOMIST = 'newsletter@economist.example.com'
const MORNING = 'hello@morning-edition.example.com'
const NORTHWIND = 'offers@northwind.example.com'
const AMAZON = 'shipment-tracking@amazon.example.com'
const GITHUB = 'notifications@github.com'

{
  const rows = newsletters()
  assert.equal(senderIn(rows, ECONOMIST)?.unsubscribe, 'oneClick', 'one click, when offered')
  assert.equal(senderIn(rows, MORNING)?.unsubscribe, 'mail', 'an email, when that is all')
  assert.equal(senderIn(rows, NORTHWIND)?.unsubscribe, 'web', 'a page, when that is all')
  const amazon = senderIn(rows, AMAZON)
  assert.ok(amazon, 'shipping notices filed as a newsletter, to move out again')
  assert.equal(amazon.unsubscribe, undefined, 'and no way to unsubscribe from them')
  for (const s of rows) {
    assert.equal(s.unsubscribedAt, undefined, `nobody asked yet: ${s.email}`)
  }
}

// ── the mock: unsubscribing ─────────────────────────────────────────────

{
  const economist = senderIn(newsletters(), ECONOMIST)
  const offer = mockUnsubscribeOffer(economist.threads[0])
  assert.deepEqual(offer, {
    message: economist.newest,
    account: 'acct-google',
    email: ECONOMIST,
    name: 'The Economist',
    via: 'oneClick',
    to: 'economist.example.com',
  })

  const outcomes = mockUnsubscribe(economist.threads, [economist.email])
  assert.deepEqual(outcomes, [
    {
      account: 'acct-google',
      email: ECONOMIST,
      name: 'The Economist',
      via: 'oneClick',
      done: true,
    },
  ])

  const after = senderIn(newsletters(), ECONOMIST)
  assert.equal(typeof after.unsubscribedAt, 'string', 'the row says they were asked')
  assert.ok(askedLine(after.unsubscribedAt), 'in words, too')
  assert.equal(after.unsubscribe, 'oneClick', 'and can still be asked again')
  assert.equal(
    mockUnsubscribeOffer(economist.threads[0]).askedAt,
    after.unsubscribedAt,
    'the thread knows as well',
  )
}

{
  const northwind = senderIn(newsletters(), NORTHWIND)
  const [outcome, ...rest] = mockUnsubscribe(northwind.threads, [northwind.email])
  assert.equal(rest.length, 0)
  assert.deepEqual(outcome, {
    account: 'acct-google',
    email: NORTHWIND,
    name: 'Northwind Outfitters',
    via: 'web',
    done: false,
    url: 'https://northwind.example.com/email-preferences?u=8f2c41',
  })
  assert.equal(
    senderIn(newsletters(), NORTHWIND).unsubscribedAt,
    undefined,
    'a page handed over is not asking: nobody knows what happened there',
  )
}

{
  const rows = newsletters()
  const morning = senderIn(rows, MORNING)
  const amazon = senderIn(rows, AMAZON)
  assert.equal(
    mockUnsubscribeOffer(morning.threads[0]).to,
    'leave-8f2c41@lists.morning-edition.example.com',
  )
  assert.equal(mockUnsubscribeOffer(amazon.threads[0]), null, 'no header, no offer')

  const outcomes = mockUnsubscribe(
    [...morning.threads, ...amazon.threads],
    [morning.email, amazon.email],
  )
  assert.deepEqual(
    outcomes.map((o) => [o.email, o.via ?? null, o.done]),
    [
      [MORNING, 'mail', true],
      [AMAZON, null, false],
    ],
    'senders asked first, senders with no way out after',
  )
  assert.match(outcomes[1].error, /no way to unsubscribe/)
  assert.equal(outcomes[1].url, undefined, 'and nothing to open')
}

{
  // GitHub writes to both accounts, and only the Google account's mail
  // carries a header: one outcome per sender of each account.
  const github = senderIn(mockInboxSenders({ days: null, limit: 100 }), GITHUB)
  assert.deepEqual([...github.accounts].sort(), ['acct-fastmail', 'acct-google'])
  assert.equal(github.unsubscribe, 'oneClick')
  assert.ok(github.threads.length > 1, 'several conversations')
  const outcomes = mockUnsubscribe(github.threads, ['Notifications@GitHub.com'])
  assert.deepEqual(
    outcomes.map((o) => [o.account, o.done]),
    [
      ['acct-google', true],
      ['acct-fastmail', false],
    ],
    'asked once from Google however many threads, and Fastmail told it has no way out',
  )
}

{
  // Without senders, each conversation names its own: the newest message
  // from somebody else -- here a person, whose mail names no way out.
  const person = mockInboxSenders({ days: null, limit: 100 }).find(
    (s) => !s.unsubscribe && s.email.endsWith('@example.com'),
  )
  assert.ok(person, 'the seed has people writing too')
  const outcomes = mockUnsubscribe([person.threads[0]], null)
  assert.equal(outcomes.length, 1)
  assert.equal(outcomes[0].email, person.email)
  assert.equal(outcomes[0].done, false)
  assert.equal(mockUnsubscribeOffer(person.threads[0]), null)
}

// ── the mock: filing a sender under another kind of mail ────────────────

{
  const amazon = senderIn(newsletters(), AMAZON)
  const [thread] = amazon.threads
  const theirs = amazon.threads
    .flatMap((t) => mockGetThread(t).messages)
    .filter((m) => m.from.email === AMAZON).length
  assert.ok(theirs > 0)

  assert.equal(
    mockSetSenderCategory([{ account: 'acct-fastmail', email: AMAZON }], 'other'),
    0,
    'another account: nothing of theirs there',
  )
  assert.equal(mockGetThread(thread).thread.category, 'newsletter', 'so nothing moved')

  const refs = amazon.accounts.map((account) => ({ account, email: AMAZON }))
  assert.equal(mockSetSenderCategory(refs, 'notification'), theirs, 'every message of theirs')
  const moved = mockGetThread(thread).thread
  assert.equal(moved.category, 'notification')
  assert.equal(moved.sortedAs, null, 'a person filed it: the model label goes')

  assert.equal(senderIn(newsletters(), AMAZON), undefined, 'gone from the Newsletters list')
  assert.ok(
    senderIn(mockInboxSenders({ days: null, category: 'notification', limit: 100 }), AMAZON),
    'and found under Notifications',
  )
  const tab = mockListThreads('mb-g-inbox', { category: 'newsletter' }, null, 100).threads
  assert.ok(!tab.some((t) => t.id === thread), "out of Mail's Newsletters tab too")

  const shouted = [{ account: 'acct-google', email: 'Shipment-Tracking@Amazon.example.com' }]
  assert.equal(mockSetSenderCategory(shouted, 'other'), theirs, 'an address whatever its case')
  assert.equal(mockGetThread(thread).thread.category, 'other')
}

await loaded.close()
console.log('unsubscribe: all checks passed')
