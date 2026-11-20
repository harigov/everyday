// Behaviour checks for "safe websites" and "always allow this website":
// `agent.svelte.ts`'s `confirm(…, 'always')`, `trustSite` and `untrustSite`,
// and the mock backend that stands in for the service while the interface is
// run in a browser.
//
// What is pinned: that a website is kept as the host it names and only once,
// that what is not a website is refused with a sentence to show, that saving
// the other settings never carries the list, that "always" is sent as such
// -- and reaches the settings the Settings pane draws -- and that a card
// only offers it when the question named a site.
//
// Written against the mock, whose own `trust_site` is a copy of the core's
// `sites::normalize_site`: if the two drift, `sites.rs`'s tests say what the
// service does and this file says what the interface was built against.

import { load, makeCheck, stubBrowser } from './harness.mjs'

stubBrowser({
  window: globalThis,
  localStorage: true,
  matchMedia: true,
  document: {
    querySelector: () => null,
    documentElement: {
      style: { setProperty: () => {} },
      classList: { toggle: () => {} },
      setAttribute: () => {},
      removeAttribute: () => {},
    },
    addEventListener: () => {},
  },
  navigator: { userAgent: 'node', language: 'en-GB' },
  location: new URL('http://localhost/?unlocked=1'),
  createObjectURL: () => 'blob:stub',
})

const {
  modules: [storeModule, agentLib, stateModule, apiModule],
  close,
} = await load(
  ['/src/lib/agent.svelte.ts', '/src/lib/agent.ts', '/src/lib/state.svelte.ts', '/src/lib/api.ts'],
  { svelte: true },
)
const { agent } = storeModule
const { applyEvent, liveTurn, sitesCover } = agentLib
const { app } = stateModule
const { api } = apiModule

const { check, ok, finish } = makeCheck()

/** Wait for `condition`, or give up after `ms` and let the check fail. */
async function until(condition, ms = 8000) {
  const stop = Date.now() + ms
  while (!condition() && Date.now() < stop) await new Promise((r) => setTimeout(r, 20))
  return condition()
}

/** What a call said when it was refused, or null when it was not. */
async function refusal(run) {
  try {
    await run()
    return null
  } catch (e) {
    return e instanceof Error ? e.message : String(e)
  }
}

const TWO = ['wikipedia.org', 'example.org']
const THREE = ['wikipedia.org', 'docs.rs', 'example.org']

/** The plain settings as the mock holds them, to build a pane's copy from. */
async function stored() {
  return JSON.parse(JSON.stringify(await api.agentSettings()))
}

await app.start()
check('the mock vault opens unlocked', app.screen, 'main')
// What `load` does for the pane, less the threads and the models: the store
// holds the settings, and a conversation is open to send into.
agent.settings = await stored()
agent.conversationId = 'conv-safe-sites'
ok('the vault starts with no safe websites', !agent.settings.safeSites?.length)

// ── trust_site: kept as a host, once ──────────────────────────────────────

{
  let s = await api.trustSite('https://www.Wikipedia.org/wiki/Lisbon?x=1#top')
  check('an address becomes its host', s.safeSites, ['wikipedia.org'])
  s = await api.trustSite('wikipedia.org')
  check('the same site twice is once', s.safeSites, ['wikipedia.org'])
  s = await api.trustSite('WWW.WIKIPEDIA.ORG.')
  check('whatever the case, www. or dot', s.safeSites, ['wikipedia.org'])
  s = await api.trustSite('en.wikipedia.org')
  check('a name under one already there is covered by it', s.safeSites, ['wikipedia.org'])
  s = await api.trustSite('  *.Docs.RS ')
  check('a pattern is its host, in the order added', s.safeSites, ['wikipedia.org', 'docs.rs'])
  s = await api.trustSite('http://user:pw@example.org:8080/x')
  check('credentials and a port are not part of it', s.safeSites, THREE)
  check('and the settings that come back are the stored ones', s.web, false)
  check('the same list is what is read afterwards', (await api.agentSettings()).safeSites, THREE)
}

// What is not a website is refused with a sentence the pane can show, and
// the list is as it was.
const NOT_SITES = ['', '   ', 'localhost', 'exa mple.com', '-bad.com', 'a..b', 'https://', '[::1]']
for (const bad of NOT_SITES) {
  const message = await refusal(() => api.trustSite(bad))
  ok(`${JSON.stringify(bad)} is refused`, message !== null)
  ok(
    `and says what it is not: ${message}`,
    /not a website's address/.test(message ?? '') && message.length < 120,
  )
}
check('nothing refused was kept', (await api.agentSettings()).safeSites, THREE)

// ── untrust_site ──────────────────────────────────────────────────────────

{
  let s = await api.untrustSite('https://www.docs.rs/std')
  check('an address takes its host off', s.safeSites, TWO)
  s = await api.untrustSite('docs.rs')
  check('one that is not there is not an error', s.safeSites, TWO)
  s = await api.untrustSite('en.wikipedia.org')
  check('a name under a site is not the site: only what was added goes', s.safeSites, TWO)
  const message = await refusal(() => api.untrustSite('nonsense'))
  ok('and what is not a website is refused here too', /not a website's address/.test(message ?? ''))
}

// ── saving the other settings never carries the list ──────────────────────

{
  // A pane opened before a site was added, saved afterwards: its copy has
  // the old list, and the backend must keep the stored one.
  const pane = await stored()
  await api.trustSite('news.example')
  await agent.saveSettings({ ...pane, safeSites: ['evil.example'], web: true })
  check('a save keeps the stored list, not the one it carried', agent.settings.safeSites, [
    ...TWO,
    'news.example',
  ])
  check('and still saves the rest', agent.settings.web, true)

  // Nor can it put sites on an empty list.
  for (const site of ['wikipedia.org', 'example.org', 'news.example']) await api.untrustSite(site)
  const empty = await stored()
  ok('an emptied list is not stored as anything', !empty.safeSites?.length)
  await agent.saveSettings({ ...empty, safeSites: ['evil.example'] })
  ok('and a save cannot fill it', !agent.settings.safeSites?.length)
  check('though what else it carried was saved', agent.settings.web, true)

  // The quick asks have a command of their own, and are kept the same way.
  await agent.saveSettings({ ...empty, quickAsks: { denied: ['somebody-elses'] } })
  check('a save keeps the stored quick asks', agent.settings.quickAsks, {})
}

// ── the store's own trustSite / untrustSite ───────────────────────────────

{
  await agent.trustSite('https://www.Docs.rs/std')
  check('the store takes the settings the backend answered', agent.settings.safeSites, ['docs.rs'])
  await agent.trustSite('blog.example.com')
  check('and adds to them', agent.settings.safeSites, ['docs.rs', 'blog.example.com'])
  const message = await refusal(() => agent.trustSite('localhost'))
  ok('a refusal reaches the caller to show', /not a website's address/.test(message ?? ''))
  check('and changes nothing', agent.settings.safeSites, ['docs.rs', 'blog.example.com'])
  await agent.untrustSite('docs.rs')
  check('removing is the same', agent.settings.safeSites, ['blog.example.com'])
  await agent.untrustSite('blog.example.com')
  ok('the last one off leaves none', !agent.settings.safeSites?.length)
}

// ── a card offers "always allow" only when the question named a site ──────

const realConfirm = api.confirmToolCall
const realSettings = api.agentSettings
const sent = []

/** `agent.turns` holding one question, as the stream would have raised it. */
function ask(callId, site) {
  const turn = liveTurn(`turn-${callId}`)
  applyEvent(turn, {
    type: 'confirmationRequired',
    callId,
    name: 'read_web_page',
    subject: 'https://en.wikipedia.org/wiki/Lisbon',
    arguments: {},
    kind: 'fetch',
    canPark: false,
    ...(site === undefined ? {} : { site }),
  })
  agent.turns = [turn]
  return () => agent.turns[0].cards[0]
}

{
  const none = ask('c0')
  check('a question with no site offers none', none().site ?? null, null)
  const named = ask('c1', 'en.wikipedia.org')
  check('one that named a site carries it', named().site, 'en.wikipedia.org')
  const unset = ask('c2', null)
  check('and null is none', unset().site, null)
}

// ── confirm sends what was pressed ────────────────────────────────────────

api.confirmToolCall = async (...args) => {
  sent.push(args)
  return true
}

{
  const card = ask('a1', 'en.wikipedia.org')
  await agent.confirm('a1', 'always')
  check('always is a yes that says so', sent.at(-1), ['a1', true, false, true])
  check('the card runs', card().state, 'running')
  check('and remembers what it added', card().trusted, 'en.wikipedia.org')
}
{
  const card = ask('a2', 'en.wikipedia.org')
  await agent.confirm('a2', 'confirm')
  check('yes is a yes and no more', sent.at(-1), ['a2', true, false, false])
  check('adding nothing', [card().state, card().trusted ?? null], ['running', null])
}
{
  const card = ask('a3')
  await agent.confirm('a3', 'later')
  check('later parks it', [sent.at(-1), card().state], [['a3', false, true, false], 'later'])
}
{
  const card = ask('a4', 'en.wikipedia.org')
  await agent.confirm('a4', 'decline')
  check('no is no', [sent.at(-1), card().state], [['a4', false, false, false], 'declined'])
  const before = sent.length
  await agent.confirm('a4', 'always')
  check('a card already answered cannot be answered again', sent.length, before)
}
{
  // Nothing was waiting any more: the turn was cancelled between the
  // question and the click.
  api.confirmToolCall = async () => false
  const card = ask('a5', 'en.wikipedia.org')
  await agent.confirm('a5', 'always')
  check('a withdrawn question fails the card', card().state, 'failed')
  check('and nothing is claimed to have been added', card().trusted ?? null, null)
  api.confirmToolCall = async (...args) => {
    sent.push(args)
    return true
  }
}

// ── and the settings follow it -- once the call has finished ───────────────

{
  // The answer itself reads nothing: the service may not have saved the
  // site yet, and a read that raced it would show the old list.
  agent.settings = await stored()
  await api.trustSite('news.example')
  let reads = 0
  api.agentSettings = async () => {
    reads += 1
    return realSettings()
  }
  const card = ask('b1', 'news.example')
  await agent.confirm('b1', 'always')
  check('always reads no settings of its own', reads, 0)
  ok('so the store has not heard yet', !agent.settings.safeSites?.includes('news.example'))
  check('the card asked for it, which is all it knows', card().trusted, 'news.example')
  api.agentSettings = realSettings
}
api.confirmToolCall = realConfirm

// ── the whole of it, through the mock's scripted question ─────────────────
//
// "strange" is the turn that opens an address nobody gave it: the mock stops
// to ask, naming the site, and honours "always" as the service does.

{
  await api.untrustSite('collector.example')
  await agent.saveSettings({ ...(await stored()), web: true })
  agent.turns = []

  const first = agent.send('Try the strange one', null)
  ok(
    'the turn stops to ask',
    await until(() => agent.turns.some((t) => t.cards.some((c) => c.state === 'waiting'))),
  )
  const card = agent.turns.flatMap((t) => t.cards).find((c) => c.state === 'waiting')
  check(
    'and offers its site',
    [card.name, card.confirmKind, card.site],
    ['read_web_page', 'fetch', 'collector.example'],
  )
  await agent.confirm(card.callId, 'always')
  await first
  check('the page was opened', card.state, 'done')
  check('and the card asked for the site', card.trusted, 'collector.example')
  ok(
    'the store read the settings when the call finished, and the site is in them',
    await until(() => sitesCover(agent.settings.safeSites ?? [], 'collector.example')),
  )
  const vault = await api.agentSettings()
  check('and in the vault', vault.safeSites?.includes('collector.example'), true)

  // Asked again, it no longer offers it: the site is on the list already.
  agent.turns = []
  const second = agent.send('Try the strange one again', null)
  ok(
    'asked again, it stops again',
    await until(() => agent.turns.some((t) => t.cards.some((c) => c.state === 'waiting'))),
  )
  const again = agent.turns.flatMap((t) => t.cards).find((c) => c.state === 'waiting')
  check('with nothing left to allow', again.site ?? null, null)
  await agent.confirm(again.callId, 'decline')
  await second
  check('declined, it is not opened', again.state, 'declined')
}

// A plain yes through the same question adds nothing.
{
  await api.untrustSite('collector.example')
  agent.turns = []
  const run = agent.send('Try the strange one once more', null)
  ok(
    'a third time it asks',
    await until(() => agent.turns.some((t) => t.cards.some((c) => c.state === 'waiting'))),
  )
  const card = agent.turns.flatMap((t) => t.cards).find((c) => c.state === 'waiting')
  check('offering its site again', card.site, 'collector.example')
  await agent.confirm(card.callId, 'confirm')
  await run
  ok(
    'a plain yes opens it and adds nothing',
    card.state === 'done' && !(await api.agentSettings()).safeSites?.includes('collector.example'),
  )
}

// A read that fails when the call has finished leaves the settings as they
// were -- so the card, which says "added" only when they cover the site, says
// nothing -- and does not touch the page that was opened.
{
  await api.untrustSite('collector.example')
  agent.settings = await stored()
  agent.turns = []
  api.agentSettings = async () => {
    throw new Error('the vault went away')
  }
  const run = agent.send('Try the strange one a fourth time', null)
  ok(
    'a fourth time it asks',
    await until(() => agent.turns.some((t) => t.cards.some((c) => c.state === 'waiting'))),
  )
  const card = agent.turns.flatMap((t) => t.cards).find((c) => c.state === 'waiting')
  await agent.confirm(card.callId, 'always')
  await run
  api.agentSettings = realSettings
  check('the page was still opened', [card.state, agent.error], ['done', null])
  check('and the card asked for the site', card.trusted, 'collector.example')
  ok(
    'but the store does not cover it, so the card says nothing of it',
    !sitesCover(agent.settings.safeSites ?? [], card.trusted),
  )
}

finish('safe-sites')
await close()

// `app.start()` arms the idle-vault poll, which would otherwise hold this
// process open for ever.
process.exit(process.exitCode ?? 0)
