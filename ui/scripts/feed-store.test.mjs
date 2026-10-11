// Behaviour checks for `feed.svelte.ts` itself -- the class, not the pure
// helpers `feed.test.mjs` covers.
//
// Driven through the real mock backend (`mock-feed.ts`), the same way
// `library-store.test.mjs` is: a fake `api` would be a second copy of what
// the mock already does, and what is worth checking here is the round trip
// -- the optimistic mark that has to agree with the vault afterwards, the
// open story that has to stay put through a reload, the folder that has to
// show nothing and not everything.
//
// Written against the mock's seed (four sources, sixteen stories, ten of
// them unread), so a change to the seed will need these numbers changing.

import assert from 'node:assert/strict'
import { load, stubBrowser } from './harness.mjs'

// A localStorage that remembers, for the one check that the chip is kept.
const stored = new Map()
const opened = []

stubBrowser({
  window: globalThis,
  localStorage: {
    getItem: (key) => stored.get(key) ?? null,
    setItem: (key, value) => stored.set(key, String(value)),
    removeItem: (key) => stored.delete(key),
  },
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
// What the mock's `openExternal` reaches for in a browser tab.
globalThis.open = (url) => {
  opened.push(url)
  return {}
}

const {
  modules: [feedModule, stateModule, apiModule, notifyModule],
  close,
} = await load(
  [
    '/src/lib/feed.svelte.ts',
    '/src/lib/state.svelte.ts',
    '/src/lib/api.ts',
    '/src/lib/notify.svelte.ts',
  ],
  { svelte: true },
)
const { feed } = feedModule
const { app } = stateModule
const { api } = apiModule
const { notify } = notifyModule

/** Wait for something that happens in the background, up to a second. */
async function until(what, condition) {
  for (let i = 0; i < 50; i++) {
    if (condition()) return
    await new Promise((r) => setTimeout(r, 20))
  }
  assert.fail(`never happened: ${what}`)
}

const ids = (list) => list.map((s) => s.id)
const sourceNamed = (title) => feed.sources.find((s) => s.source.title === title)

await app.start()
assert.equal(app.screen, 'main', 'the mock vault should open unlocked')
assert.equal(app.supportsNews, true, 'and carry the news domain')

// ── start ────────────────────────────────────────────────────────────

await feed.start()
assert.equal(feed.sources.length, 4, 'the mock follows four sources')
assert.equal(feed.loading, false, 'loading settles back to false once the load lands')
assert.equal(feed.filter, 'unread', 'Unread is the chip a feed opens on')
assert.deepEqual(
  ids(feed.stories),
  ['st-9', 'st-1', 'st-5', 'st-10', 'st-11', 'st-2', 'st-13', 'st-14', 'st-6', 'st-8'],
  'Unread shows only what is unread, newest first',
)
assert.equal(feed.next, null, 'ten stories is one page')
assert.equal(feed.unreadTotal, 10, 'the sidebar total is the sum of the sources')

assert.deepEqual(
  feed.folders.map((f) => [f.name, f.unread]),
  [
    ['', 7],
    ['Engineering', 3],
  ],
  'unfiled sources come first, then each folder with its unread total',
)
assert.ok(sourceNamed('The Old Radio Blog').lastError, 'the dead feed says why it is dead')
assert.equal(feed.mailAvailable, true, 'the mock has mail, so newsletters can be offered')
assert.equal(feed.accent, '#0369a1', 'the Feed has an accent of its own')

// ── the chip, and remembering it ─────────────────────────────────────

await feed.setFilter('all')
assert.equal(feed.stories.length, 16, 'All is every story')
assert.equal(
  globalThis.localStorage.getItem('everyday.feed.filter'),
  'all',
  'the chip is kept on this device',
)
await feed.setFilter('saved')
assert.deepEqual(ids(feed.stories), ['st-3'], 'Saved is only what was kept')
await feed.setFilter('all')

// ── scope ────────────────────────────────────────────────────────────

await feed.select({ kind: 'source', id: sourceNamed('Shipyard Notes').source.id })
assert.deepEqual(ids(feed.stories), ['st-5', 'st-6', 'st-7', 'st-8'], 'one source, its own stories')
assert.equal(feed.source?.source.title, 'Shipyard Notes')

await feed.select({ kind: 'folder', name: 'Engineering' })
assert.equal(feed.stories.length, 6, 'a folder is the stories of every source filed in it')

// The one that matters: sent as an empty list the service would read it as
// "every source", and a folder somebody has just emptied would show the lot.
await feed.select({ kind: 'folder', name: 'Nowhere' })
assert.equal(feed.stories.length, 0, 'a folder with nothing in it shows nothing, not everything')

await feed.select({ kind: 'all' })
assert.equal(feed.stories.length, 16)

// A stale answer never lands: two selections in the air, the second wins.
{
  const first = feed.select({ kind: 'source', id: sourceNamed('Longform Weekly').source.id })
  const second = feed.select({ kind: 'source', id: sourceNamed('Hacker News').source.id })
  await Promise.all([first, second])
  assert.ok(
    feed.stories.length > 0 && feed.stories.every((s) => s.sourceId === 'src-hn'),
    'only the newest selection lands',
  )
}

// ── the bar's search, and the order ──────────────────────────────────

assert.deepEqual(
  ids(feed.visible).slice(0, 3),
  ['st-9', 'st-10', 'st-11'],
  'newest first by default',
)
feed.setSort('points')
assert.deepEqual(ids(feed.visible).slice(0, 3), ['st-9', 'st-13', 'st-10'], 'points ranks by score')
feed.setSort('newest')
await feed.select({ kind: 'all' })
feed.setQuery('lighthouse')
assert.deepEqual(ids(feed.visible), ['st-1'], 'the bar narrows what is loaded')
feed.setQuery('')
assert.equal(feed.visible.length, 16)

// ── pages ────────────────────────────────────────────────────────────

{
  const total = feed.stories.length
  const page = await api.stories({ limit: 5 })
  assert.ok(page.next, 'a short page has a cursor')
  feed.stories = page.stories
  feed.next = page.next
  await feed.loadMore()
  assert.equal(feed.stories.length, total, 'the next page makes the rest')
  assert.equal(feed.next, null, 'and the last page ends the cursor')
  assert.equal(new Set(ids(feed.stories)).size, total, 'no story is on screen twice')
}

// ── opening, and staying open ────────────────────────────────────────

await feed.setFilter('unread')
const longform = () => sourceNamed('Longform Weekly')
assert.equal(longform().unread, 2)

await feed.open('st-1')
assert.equal(feed.selected, 'st-1')
assert.equal(feed.story?.id, 'st-1')
assert.equal(feed.story?.read, true, 'opening a story marks it read at once')
assert.equal(longform().unread, 1, 'and the sidebar count moves with it')
await feed.flush()
await until('the count settles on the vault’s own', () => longform().unread === 1)

// The point of keeping it: a reload with the Unread chip up would otherwise
// drop the row the pane is showing, and `j` would not know where "below" was.
await feed.reload()
assert.deepEqual(ids(feed.stories).slice(0, 3), ['st-9', 'st-1', 'st-5'], 'it stays in its place')
assert.equal(feed.story?.id, 'st-1', 'and the pane still has it')

feed.close()
assert.equal(feed.story, null)
await feed.reload()
assert.ok(!ids(feed.stories).includes('st-1'), 'once closed, the next load lets it go')

// A different place is a different list: what was open there is not.
await feed.open('st-5')
await feed.select({ kind: 'all' })
assert.equal(feed.selected, null, 'selecting a scope closes the story')

// A story that is not in the list at all -- opened from the Overview.
await feed.open('st-3')
assert.equal(feed.story?.id, 'st-3', 'a story outside the list is fetched for the pane')
feed.close()

// ── j and k ──────────────────────────────────────────────────────────

{
  await feed.select({ kind: 'all' })
  const rows = ids(feed.visible)
  await feed.move(1)
  assert.equal(feed.selected, rows[0], 'with nothing open, j lands on the first')
  await feed.move(1)
  assert.equal(feed.selected, rows[1])
  await feed.move(-1)
  assert.equal(feed.selected, rows[0])
  await feed.move(-1)
  assert.equal(feed.selected, rows[0], 'k at the top stays at the top')
  feed.close()
}

// ── read and saved ───────────────────────────────────────────────────

await feed.setFilter('all')
await feed.markRead(['st-2'], true)
assert.equal(feed.stories.find((s) => s.id === 'st-2').read, true)
await feed.toggleRead('st-2')
assert.equal(feed.stories.find((s) => s.id === 'st-2').read, false, 'toggling puts it back')
assert.equal((await api.story('st-2')).read, false, 'and the vault agrees')

await feed.toggleSaved('st-2')
assert.equal(feed.stories.find((s) => s.id === 'st-2').saved, true)
assert.equal((await api.story('st-2')).saved, true, 'saving reaches the vault')
await feed.toggleSaved('st-2')
assert.equal((await api.story('st-2')).saved, false)

// ── mark all read, and the way back ──────────────────────────────────

{
  await feed.setFilter('unread')
  await feed.refreshSources()
  const before = feed.unreadTotal
  assert.ok(before > 0)
  await feed.markAllRead()
  assert.equal(feed.unreadTotal, 0, 'everything in view is read')
  assert.equal(feed.stories.length, 0, 'and the Unread list is empty, which is the point')

  const toast = notify.toasts.find((t) => t.key === 'feed-marked-read')
  assert.ok(toast, 'it says so, with a way back')
  assert.equal(toast.title, `Marked ${before} stories read`)
  assert.equal(toast.action?.label, 'Undo')

  toast.action.run()
  await until('the undo lands', () => feed.unreadTotal === before)
  await until('and the list comes back', () => feed.stories.length === before)
  notify.dismiss(toast.id)
}

// A search narrows what "in view" means, and marks only that.
{
  feed.setQuery('sync')
  assert.deepEqual(ids(feed.visible), ['st-6'])
  await feed.markAllRead()
  assert.equal((await api.story('st-6')).read, true, 'the matching story is read')
  assert.equal((await api.story('st-8')).read, false, 'and the rest of its source is not')
  await feed.markRead(['st-6'], false)
  feed.setQuery('')
}

// One source, by source and date: the others are left alone.
{
  const shipyard = () => sourceNamed('Shipyard Notes')
  await feed.refreshSources()
  const owed = shipyard().unread
  const others = longform().unread
  assert.ok(owed > 0 && others > 0)
  await feed.select({ kind: 'source', id: shipyard().source.id })
  await feed.markAllRead()
  assert.equal(shipyard().unread, 0)
  assert.equal(longform().unread, others, 'another source keeps its unread')
  const toast = notify.toasts.find((t) => t.key === 'feed-marked-read')
  toast.action.run()
  await until('the undo lands', () => shipyard().unread === owed)
  notify.dismiss(toast.id)
  await feed.select({ kind: 'all' })
}

// ── refreshing ───────────────────────────────────────────────────────

{
  const before = feed.stories.length
  const reports = await feed.refresh()
  assert.equal(reports.length, 4, 'one report for every source')
  assert.ok(reports.find((r) => r.sourceId === 'src-radio').error, 'the dead feed reports why')
  assert.equal(feed.refreshing, false)
  assert.equal(feed.stories.length, before + 3, 'the three that answered each brought one')
  assert.match(feed.note, /3 new stories, and 1 source could not be reached/)

  // A folder's menu asks for each of its sources at once, and the spinner
  // has to last until the last of them, not the first.
  const slow = feed.refresh('src-hn')
  assert.equal(feed.refreshing, true)
  const both = await Promise.all([slow, feed.refresh('src-longform'), feed.refresh('src-shipyard')])
  assert.equal(feed.refreshing, false, 'the spinner ends with the last refresh')
  assert.deepEqual(
    both.map((r) => r.length),
    [1, 1, 1],
    'none of them was turned away',
  )
}

// ── following ────────────────────────────────────────────────────────

{
  assert.equal((await feed.discover('https://example.org/blog')).length, 2, 'a page offers feeds')
  assert.deepEqual(
    (await feed.discover('https://example.org/feed.xml')).map((f) => f.url),
    ['https://example.org/feed.xml'],
    'a feed is its own answer',
  )
  assert.deepEqual(await feed.discover('https://nofeed.example'), [])
  assert.deepEqual(await feed.discover('nonsense'), [], 'a failure is an empty list...')
  assert.equal(app.error, 'That does not look like a web address.', '...and is said')
  app.error = null

  const added = await feed.addSource({
    type: 'web',
    url: 'https://example.org/feed.xml',
    folder: 'News',
  })
  assert.ok(added, 'following returns the source')
  assert.deepEqual(feed.scope, { kind: 'source', id: added.id }, 'and selects it')
  assert.equal(added.folder, 'News')
  assert.ok(
    feed.folders.some((f) => f.name === 'News'),
    'it appears in its folder',
  )
  await until('a new source is fetched straight away', () => feed.stories.length === 3)

  assert.equal(await feed.addSource({ type: 'web', url: 'https://example.org/feed.xml' }), null)
  assert.equal(app.error, 'You already follow that feed.', 'following twice is refused, and said')
  app.error = null

  // Newsletters: the sender is followed, and then it shows as followed.
  const before = await feed.newsletterSenders(90)
  assert.equal(before.length, 3)
  assert.ok(before.every((s) => !s.followed))
  const letter = await feed.addSource({ type: 'mail', sender: before[0].email })
  assert.ok(letter, 'a newsletter sender can be followed')
  assert.equal(letter.title, before[0].name)
  assert.equal((await feed.newsletterSenders(90))[0].followed, true)
  await until('its first issues arrive', () => feed.stories.length === 3)
  assert.ok(
    feed.stories.every((s) => s.mailMessage),
    'each issue points at its message',
  )

  // A newsletter's read state is its message's, so the service's sweep by
  // date leaves it alone. Marking the source read has to reach the issues by
  // id as well, and the undo has to put them back with the rest.
  await until('the sidebar counts the issues', () => feed.source?.unread === 3)
  await feed.markAllRead()
  assert.equal(feed.source.unread, 0, 'a newsletter source can be marked read')
  const issues = notify.toasts.find((t) => t.key === 'feed-marked-read')
  assert.equal(issues.title, 'Marked 3 stories read')
  issues.action.run()
  await until('the undo reaches the issues too', () => feed.source.unread === 3)
  notify.dismiss(issues.id)

  // Unfollowing takes the stories with it, and a scope that no longer exists
  // goes back to everything.
  await feed.deleteSource(letter.id)
  assert.ok(!feed.sources.some((s) => s.source.id === letter.id))
  assert.deepEqual(feed.scope, { kind: 'all' }, 'the deleted source was the scope')
  assert.ok(!feed.stories.some((s) => s.sourceId === letter.id))
  await feed.deleteSource(added.id)
  // The first delete's reload can land after the second's optimistic removal.
  await until('the vault agrees', () => feed.sources.length === 4)

  // A rename reaches the vault and the sidebar.
  const original = sourceNamed('Shipyard Notes').source
  await feed.saveSource({ ...original, title: 'Shipyard Dispatches', folder: '' })
  assert.ok(sourceNamed('Shipyard Dispatches'), 'a saved source is renamed')
  await feed.saveSource({ ...original, title: 'Shipyard Notes', folder: 'Engineering' })
}

// ── full text, the Library, links ────────────────────────────────────

{
  await feed.setFilter('all')
  const summaryOnly = feed.stories.find((s) => s.id === 'st-4')
  assert.equal(summaryOnly.hasBody, false)
  const teaser = await feed.loadBody(summaryOnly)
  assert.ok(
    teaser.html.includes('Harbour masters'),
    'a story with no body is drawn from its summary',
  )
  await feed.fetchFullArticle('st-4')
  assert.equal(feed.extracting, false)
  assert.equal(feed.stories.find((s) => s.id === 'st-4').hasBody, true, 'the body is kept')

  await feed.fetchFullArticle('st-11')
  assert.match(
    feed.note,
    /no article to fetch/,
    'a story with no address says so beside the button',
  )

  const item = await feed.addToLibrary('st-1')
  assert.equal(typeof item, 'string', 'adding returns the item')
  assert.match(feed.note, /Library/)
  assert.ok(
    (await api.kinds()).some((k) => k.slug === 'article'),
    'on the Articles shelf',
  )

  await feed.openOriginal('st-1')
  assert.deepEqual(opened, ['https://longform.example/lighthouse-keepers'])
  await feed.openDiscussion('st-9')
  assert.equal(opened.at(-1), 'https://news.ycombinator.com/item?id=41200002')
  await feed.openOriginal('st-11')
  assert.equal(opened.length, 2, 'a story with no address opens nothing')
  assert.match(feed.note, /no web address/)
}

// ── bodies ───────────────────────────────────────────────────────────

{
  assert.equal(feed.bodyUrl({ id: 'st-1' }), 'everyday://news/body/st-1')
  const body = await feed.loadBody(await api.story('st-1'))
  assert.ok(body.html.startsWith('<!DOCTYPE html>'), 'a whole document for the frame')
  assert.ok(body.html.includes('For two centuries'), 'with the story in it')
  assert.ok(body.html.includes('color-scheme'), 'drawn for the theme')
  assert.equal(body.imagesHidden, false)

  // The dead feed has its pictures switched off.
  const held = await feed.loadBody(await api.story('st-15'))
  assert.equal(held.imagesHidden, true, 'a source with pictures off says they were held back')
  assert.ok(!held.html.includes('<img'), 'and none are in the document')
}

// ── OPML ─────────────────────────────────────────────────────────────

{
  const xml = await feed.exportOpml()
  assert.match(xml, /<opml version="2.0">/)
  assert.match(xml, /xmlUrl="https:\/\/longform.example\/feed.xml"/)
  assert.ok(!xml.includes('news.ycombinator'), 'only web feeds are in an OPML file')

  const again = await feed.importOpml(xml)
  assert.equal(again.added, 0, 'reading back what is followed adds nothing')
  assert.equal(again.skipped, 3)

  const report = await feed.importOpml(
    '<opml version="2.0"><body><outline text="Fold">' +
      '<outline type="rss" text="New One" xmlUrl="https://newone.example/rss"/>' +
      '</outline></body></opml>',
  )
  assert.deepEqual(report, { added: 1, skipped: 0 })
  assert.equal(sourceNamed('New One').source.folder, 'Fold', 'a folder in the file is a folder')
  await until('the refresh an import starts finishes', () => !feed.refreshing)
}

// ── locking ──────────────────────────────────────────────────────────

feed.setQuery('x')
await feed.open(feed.stories[0].id)
feed.reset()
assert.equal(feed.sources.length, 0, 'a lock drops what was loaded')
assert.equal(feed.stories.length, 0)
assert.equal(feed.selected, null)
assert.equal(feed.query, '')
assert.equal(feed.filter, 'all', 'but not the chip, which is chrome')
await feed.flush()

await close()
console.log('feed-store: all checks passed')

// Same reason `library-store.test.mjs` ends like this: `app.start()` arms the
// idle-vault poll, which would otherwise hold this process open for ever.
process.exit(0)
