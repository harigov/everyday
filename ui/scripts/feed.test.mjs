// `lib/feed.ts`: the Feed's rules that need no store.
//
// The type checker covers the shape of all this; what it cannot cover is a
// rule whose failure is quiet. A folder in the wrong place, a search that
// misses the source's own name, a points sort that ranks a story nobody has
// scored above one with a hundred -- none of them throws, and all of them
// are a list somebody reads wrongly.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: feed, close } = await load('/src/lib/feed.ts')
const {
  alsoLine,
  alsoOn,
  bylineParts,
  clampDays,
  clampPoints,
  distinctAuthor,
  estimateArticleHeight,
  filterStories,
  folderNames,
  folderUnread,
  groupFolders,
  hostOf,
  isWebUrl,
  looksCutOff,
  muteText,
  parseMute,
  plainText,
  pointsLine,
  scopeTitle,
  scopeUnread,
  siteOf,
  sortStories,
  sourceLabel,
  topStories,
  unfollowDetail,
} = feed

// ── fixtures ─────────────────────────────────────────────────────────────

function source(id, over = {}) {
  return {
    id,
    kind: { type: 'web', url: `https://${id}.example/feed.xml` },
    title: id,
    folder: '',
    siteUrl: '',
    images: true,
    fullText: false,
    keepOutOfInbox: false,
    mute: [],
    paused: false,
    sortOrder: 0,
    createdAt: '2026-10-01T00:00:00Z',
    updatedAt: '2026-10-01T00:00:00Z',
    ...over,
  }
}

function summary(id, unread = 0, over = {}) {
  return { source: source(id, over), unread, total: unread }
}

function story(id, over = {}) {
  return {
    id,
    sourceId: 'blog',
    key: id,
    title: `Story ${id}`,
    url: `https://blog.example/${id}`,
    author: '',
    publishedAt: '2026-10-10T12:00:00Z',
    fetchedAt: '2026-10-10T12:05:00Z',
    summary: '',
    imageUrl: '',
    discussionUrl: '',
    also: [],
    read: false,
    saved: false,
    hasBody: true,
    expired: false,
    ...over,
  }
}

const ids = (list) => list.map((s) => s.id)

// ── folders ──────────────────────────────────────────────────────────────

{
  const folders = groupFolders([
    summary('a', 3, { folder: 'news' }),
    summary('b', 0),
    summary('c', 5, { folder: 'Art' }),
    summary('d', 2, { folder: 'news' }),
    summary('e', 1, { folder: 'Zed' }),
    summary('f', 4, { folder: '  ' }),
  ])
  assert.deepEqual(
    folders.map((f) => f.name),
    ['', 'Art', 'news', 'Zed'],
    'unfiled first, then folders alphabetically ignoring case',
  )
  assert.deepEqual(
    folders.map((f) => f.unread),
    [4, 5, 5, 1],
    'each folder carries its unread total',
  )
  assert.deepEqual(
    folders[2].sources.map((s) => s.source.id),
    ['a', 'd'],
    'sources keep the order they were given in',
  )
  assert.deepEqual(
    folders[0].sources.map((s) => s.source.id),
    ['b', 'f'],
    'a blank folder name is no folder',
  )
  assert.deepEqual(groupFolders([]), [], 'no sources, no folders')
  assert.equal(
    groupFolders([summary('x', 1, { folder: 'Tech' })])[0].name,
    'Tech',
    'there is no empty unfiled group to fill the space',
  )
  assert.deepEqual(
    groupFolders([summary('x', 0, { folder: 'b' }), summary('y', 0, { folder: 'B' })]).map(
      (f) => f.name,
    ),
    ['B', 'b'],
    'names that differ only by case are two folders, in a settled order',
  )
  assert.deepEqual(
    folderNames([summary('x', 0, { folder: 'b' }), summary('y'), summary('z', 0, { folder: 'a' })]),
    ['a', 'b'],
    'the datalist offers real folders only',
  )
}

// ── what a source is called ──────────────────────────────────────────────

assert.equal(sourceLabel(source('a', { title: ' Atom ' })), 'Atom', 'its title, trimmed')
assert.equal(sourceLabel(source('a', { title: '' })), 'a.example', 'else the host it comes from')
assert.equal(
  sourceLabel(source('h', { title: '', kind: { type: 'hackerNews', list: 'show', minPoints: 0 } })),
  'Show HN',
  'else the list it follows',
)
assert.equal(
  sourceLabel(source('m', { title: '', kind: { type: 'mail', sender: 'a@b.example' } })),
  'a@b.example',
  'else the sender',
)

assert.equal(
  siteOf(source('a', { siteUrl: 'https://a.example/' })),
  'https://a.example/',
  "a source's own site",
)
assert.equal(siteOf(source('a')), 'https://a.example', 'else the origin of its feed')
assert.equal(
  siteOf(source('h', { kind: { type: 'hackerNews', list: 'front', minPoints: 0 } })),
  'https://news.ycombinator.com',
)
assert.equal(
  siteOf(source('m', { kind: { type: 'mail', sender: 'a@b.example' } })),
  '',
  'mail has no site to open',
)
assert.equal(
  siteOf(source('x', { siteUrl: 'javascript:alert(1)', kind: { type: 'web', url: 'nonsense' } })),
  '',
  'nothing that is not a web address',
)

assert.match(unfollowDetail(source('a')), /Library stays there/, 'what survives an unfollow')
assert.match(
  unfollowDetail(source('m', { kind: { type: 'mail', sender: 'a@b.example' } })),
  /stay in Mail/,
  'a newsletter was never copied out of Mail',
)

// ── addresses ────────────────────────────────────────────────────────────

assert.equal(hostOf('https://www.example.com/a?b=1'), 'example.com', 'www is dropped')
assert.equal(hostOf('https://Blog.Example.COM/x'), 'blog.example.com', 'lower-cased')
assert.equal(hostOf('http://example.com:8080/x'), 'example.com:8080', 'a port is part of a host')
assert.equal(hostOf('example.com/no-scheme'), '', 'not an address')
assert.equal(hostOf(''), '')
assert.equal(hostOf('mailto:a@b.example'), '', 'no host, no host')
assert.equal(isWebUrl('https://a.example'), true)
assert.equal(isWebUrl('  http://a.example/x'), true, 'leading space is forgiven')
assert.equal(isWebUrl('javascript:alert(1)'), false)
assert.equal(isWebUrl('ftp://a.example'), false)
assert.equal(isWebUrl(''), false)

// ── ordering ─────────────────────────────────────────────────────────────

{
  const list = [
    story('old', { publishedAt: '2026-10-01T00:00:00Z', score: 900 }),
    story('new', { publishedAt: '2026-10-09T00:00:00Z', score: 3 }),
    story('mid', { publishedAt: '2026-10-05T00:00:00Z', score: 40 }),
    story('blog', { publishedAt: '2026-10-08T00:00:00Z' }),
    story('zero', { publishedAt: '2026-10-07T00:00:00Z', score: 0 }),
  ]
  const before = ids(list)
  assert.deepEqual(ids(sortStories(list, 'newest')), ['new', 'blog', 'zero', 'mid', 'old'])
  assert.deepEqual(
    ids(sortStories(list, 'points')),
    ['old', 'mid', 'new', 'zero', 'blog'],
    'points, and a story nobody has scored below one with a score of nothing',
  )
  assert.deepEqual(ids(list), before, 'sorting never reorders what the store holds')

  const tied = [
    story('a', { score: 5, publishedAt: '2026-10-01T00:00:00Z' }),
    story('b', { score: 5, publishedAt: '2026-10-02T00:00:00Z' }),
  ]
  assert.deepEqual(ids(sortStories(tied, 'points')), ['b', 'a'], 'level on points, newest first')
  const same = [story('a'), story('b')]
  assert.deepEqual(ids(sortStories(same, 'newest')), ['b', 'a'], 'same minute: id, descending')
  assert.deepEqual(ids(sortStories([], 'points')), [])
}

// ── search ───────────────────────────────────────────────────────────────

{
  const sources = [
    summary('blog', 0, { title: 'The Rust Blog' }),
    summary('hn', 0, { title: 'Hacker News' }),
  ]
  const list = [
    story('1', { title: 'Async closures land', author: 'Niko', summary: 'Stable in 1.99' }),
    story('2', { sourceId: 'hn', title: 'A tale of two kernels', author: 'dang' }),
    story('3', { title: 'Release notes', summary: 'Nothing about ASYNC here' }),
  ]
  assert.deepEqual(ids(filterStories(list, '', sources)), ['1', '2', '3'], 'empty is everything')
  assert.deepEqual(ids(filterStories(list, '   ', sources)), ['1', '2', '3'], 'so is blank')
  assert.deepEqual(ids(filterStories(list, 'async', sources)), ['1', '3'], 'title and summary')
  assert.deepEqual(ids(filterStories(list, 'NIKO', sources)), ['1'], 'author, any case')
  assert.deepEqual(ids(filterStories(list, 'hacker', sources)), ['2'], "the source's name")
  assert.deepEqual(
    ids(filterStories(list, 'rust blog', sources)),
    ['1', '3'],
    'two words of a name',
  )
  assert.deepEqual(ids(filterStories(list, 'async niko', sources)), ['1'], 'every word, anywhere')
  assert.deepEqual(ids(filterStories(list, 'zebra', sources)), [], 'no match')
  assert.deepEqual(
    ids(filterStories(list, 'rust', [])),
    [],
    'a source that is not known contributes no name',
  )
}

// ── the sidebar's headings ───────────────────────────────────────────────

{
  const sources = [
    summary('a', 2, { folder: 'News' }),
    summary('b', 3, { folder: 'News' }),
    summary('c', 4),
  ]
  assert.equal(scopeTitle({ kind: 'all' }, sources), 'All')
  assert.equal(scopeTitle({ kind: 'folder', name: 'News' }, sources), 'News')
  assert.equal(scopeTitle({ kind: 'folder', name: '' }, sources), 'Unfiled')
  assert.equal(scopeTitle({ kind: 'source', id: 'c' }, sources), 'c')
  assert.equal(scopeTitle({ kind: 'source', id: 'gone' }, sources), 'Feed', 'a source that left')
  assert.equal(scopeUnread({ kind: 'all' }, sources), 9)
  assert.equal(scopeUnread({ kind: 'folder', name: 'News' }, sources), 5)
  assert.equal(scopeUnread({ kind: 'folder', name: '' }, sources), 4, 'unfiled is a folder too')
  assert.equal(scopeUnread({ kind: 'source', id: 'b' }, sources), 3)
  assert.equal(scopeUnread({ kind: 'source', id: 'gone' }, sources), 0)
}

// ── what a row says ──────────────────────────────────────────────────────

assert.equal(pointsLine({ score: 412, comments: 180 }), '▲ 412 · 180 comments')
assert.equal(pointsLine({ score: 1, comments: 1 }), '▲ 1 · 1 comment')
assert.equal(
  pointsLine({ score: 0, comments: 0 }),
  '▲ 0 · 0 comments',
  'nothing yet is still a score',
)
assert.equal(pointsLine({ score: 7, comments: null }), '▲ 7')
assert.equal(pointsLine({ comments: 3 }), '3 comments')
assert.equal(pointsLine({}), '', 'a blog post has neither')
assert.equal(pointsLine({ score: null, comments: null }), '')
assert.equal(pointsLine({ score: 1234 }), `▲ ${(1234).toLocaleString()}`, 'in the reader locale')

assert.deepEqual(bylineParts({ author: 'Ann' }, 'Blog'), ['Blog', 'Ann'])
assert.deepEqual(bylineParts({ author: 'blog' }, 'Blog'), ['Blog'], 'not said twice')
assert.deepEqual(bylineParts({ author: '' }, 'Blog'), ['Blog'])
assert.deepEqual(bylineParts({ author: 'Ann' }, ' '), ['Ann'], 'no source name to lead with')
assert.equal(distinctAuthor({ author: ' Ann ' }, 'Blog'), 'Ann')
assert.equal(distinctAuthor({ author: 'BLOG' }, 'Blog'), '', 'the source again is nobody')
assert.equal(distinctAuthor({ author: '' }, ''), '', 'nothing from nothing')

{
  const sources = [
    summary('hn', 0, { title: 'Front', kind: { type: 'hackerNews', list: 'front', minPoints: 0 } }),
  ]
  const also = [
    {
      sourceId: 'hn',
      score: 412,
      comments: 180,
      discussionUrl: 'https://news.ycombinator.com/item?id=1',
    },
  ]
  assert.deepEqual(alsoOn({ also }, sources), [
    {
      sourceId: 'hn',
      name: 'Hacker News',
      detail: '412 points · 180 comments',
      url: 'https://news.ycombinator.com/item?id=1',
    },
  ])
  assert.equal(
    alsoLine({ also }, sources),
    'Also on Hacker News · 412 points · 180 comments',
    'one sentence for a tooltip',
  )
  assert.equal(alsoLine({ also: [] }, sources), '', 'nothing to say')
  const odd = alsoOn(
    {
      also: [{ sourceId: 'gone', score: 1, comments: null, discussionUrl: 'javascript:alert(1)' }],
    },
    sources,
  )
  assert.deepEqual(
    odd.map((p) => [p.name, p.detail, p.url]),
    [['another source', '1 point', '']],
    'a source that left, and an address no browser is sent to',
  )
}

// ── a body that stops short ──────────────────────────────────────────────

assert.equal(
  plainText('<html><head><style>p{}</style></head><body><p>One <b>two</b></p></body></html>'),
  'One two',
  'the document around the words is dropped',
)
assert.equal(
  plainText('<header>Top</header><p>Body</p>'),
  'Top Body',
  'a <header> is not the document head',
)
assert.equal(looksCutOff('<p>The first paragraph and then it just stops…</p>'), true)
assert.equal(looksCutOff('<p>It trails off [&hellip;]</p>'), true, 'an entity is still an ellipsis')
assert.equal(looksCutOff('<p>It trails off [...]</p>'), true)
assert.equal(looksCutOff('<p>Something short. Read more</p>'), true)
assert.equal(looksCutOff('<p>Something short. <a href="/x">Continue reading →</a></p>'), true)
assert.equal(
  looksCutOff('<p>Text.</p><p>The post A thing appeared first on Some Blog.</p>'),
  true,
  "WordPress's own footer",
)
assert.equal(looksCutOff('<p>A whole article, ending as articles do.</p>'), false)
assert.equal(
  looksCutOff('<p>Sure... and then a lot more text follows after it, in full.</p>'),
  false,
  'an ellipsis mid-way is only punctuation',
)
assert.equal(
  looksCutOff('<p>你好... 世界。这是一篇完整的文章。</p>'),
  false,
  'not every non-ASCII letter is a gap',
)
assert.equal(looksCutOff(''), false)

// ── a source's settings ──────────────────────────────────────────────────

assert.deepEqual(
  parseMute('Crypto, NFT\nweb3 ,, crypto '),
  ['Crypto', 'NFT', 'web3'],
  'deduplicated',
)
assert.deepEqual(parseMute('  '), [])
assert.equal(muteText(['a', 'b']), 'a, b')
assert.deepEqual(
  parseMute(muteText(['a b', 'c'])),
  ['a b', 'c'],
  'a phrase survives the round trip',
)

assert.equal(clampDays(30, 90), 30)
assert.equal(clampDays(0, 90), 1, 'a day at the least')
assert.equal(clampDays(9999, 90), 365, 'a year at the most')
assert.equal(clampDays(null, 90), 90, 'an empty field is the default')
assert.equal(clampDays(Number.NaN, 90), 90)
assert.equal(clampDays(12.6, 90), 13)
assert.equal(clampPoints(-5), 0)
assert.equal(clampPoints(null), 0)
assert.equal(clampPoints(150.4), 150)

// ── the Overview's card ──────────────────────────────────────────────────

{
  const sources = [
    summary('a', 2, { folder: 'News' }),
    summary('b', 9, { folder: 'Tech' }),
    summary('c', 0, { folder: 'Quiet' }),
    summary('d', 2),
  ]
  assert.deepEqual(
    folderUnread(sources).map((f) => [f.label, f.unread]),
    [
      ['Tech', 9],
      ['News', 2],
      ['Unfiled', 2],
    ],
    'most unread first, a quiet folder left out, ties by name',
  )
  assert.deepEqual(folderUnread([]), [])
}

{
  const sources = [
    summary('hn', 0, { kind: { type: 'hackerNews', list: 'front', minPoints: 0 } }),
    summary('blog'),
  ]
  const list = [
    story('b1', { publishedAt: '2026-10-10T10:00:00Z' }),
    story('b2', { publishedAt: '2026-10-10T09:00:00Z' }),
    story('b3', { publishedAt: '2026-10-10T08:00:00Z' }),
    story('h1', { sourceId: 'hn', score: 50, publishedAt: '2026-10-10T07:00:00Z' }),
    story('h2', { sourceId: 'hn', score: 600, publishedAt: '2026-10-10T06:00:00Z' }),
  ]
  assert.deepEqual(
    ids(topStories(list, sources)),
    ['b1', 'h2', 'b2'],
    'newest blog, best of Hacker News, then the next blog',
  )
  assert.deepEqual(
    ids(topStories(list, sources, 5)),
    ['b1', 'h2', 'b2', 'h1', 'b3'],
    'dealt in turn',
  )
  assert.deepEqual(
    ids(
      topStories(
        list.filter((s) => s.sourceId === 'hn'),
        sources,
      ),
    ),
    ['h2', 'h1'],
    'Hacker News alone is by points',
  )
  assert.deepEqual(
    ids(
      topStories(
        list.filter((s) => s.sourceId !== 'hn'),
        sources,
      ),
    ),
    ['b1', 'b2', 'b3'],
    'everything else alone is newest first',
  )
  const hnFirst = [
    story('h3', { sourceId: 'hn', score: 700, publishedAt: '2026-10-10T11:00:00Z' }),
    ...list,
  ]
  assert.equal(topStories(hnFirst, sources)[0].id, 'h3', 'the pool with the newer story goes first')
  assert.deepEqual(topStories([], sources), [])
}

// ── estimateArticleHeight ────────────────────────────────────────────────

{
  const doc = (inner) =>
    `<!DOCTYPE html><html><head><style>body{margin:0}</style></head><body>${inner}</body></html>`
  const para = `<p>${'word '.repeat(80)}</p>`
  const short = estimateArticleHeight(doc(para), 700)
  const long = estimateArticleHeight(doc(para.repeat(40)), 700)
  assert.ok(long > short * 20, 'forty paragraphs are far taller than one')
  assert.ok(long > 2400, 'an article is not capped at a message-sized height')

  const pictured = estimateArticleHeight(doc(`${para}<img src="x" width="1400" height="700">`), 700)
  // A 1400x700 picture in a 668px column draws at about 334px.
  assert.ok(pictured - short > 300 && pictured - short < 380, 'a picture is scaled to the column')
  const unsized = estimateArticleHeight(doc(`${para}<img src="x">`), 700)
  assert.ok(unsized - short > 200, 'a picture with no size is still given room')

  const code = estimateArticleHeight(doc(`<pre>${'line\n'.repeat(30)}</pre>`), 700)
  assert.ok(code > 30 * 20, 'code is counted line by line')
  assert.ok(
    estimateArticleHeight(doc('<p>hi</p>'), 700) >= 160,
    'even a one-liner has a frame worth drawing',
  )
}

await close()
console.log('feed: all checks passed')
