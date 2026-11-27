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
  FEED_KINDS,
  KIND_LABELS,
  KIND_NONE,
  NOT_NEWSLETTER_DETAIL,
  alsoLine,
  alsoOn,
  bodyAboutOf,
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
  isOfKind,
  isWebUrl,
  kindOf,
  listedSources,
  looksCutOff,
  muteText,
  parseMute,
  plainText,
  pointsLine,
  scopeSources,
  scopeTitle,
  scopeUnread,
  searchNav,
  siteOf,
  sortStories,
  sourceLabel,
  storyText,
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
  sourceLabel(
    source('m', { title: '', kind: { type: 'mail', sender: 'a@b.example', name: ' TLDR AI ' } }),
  ),
  'TLDR AI',
  'else the name a newsletter goes by, before the address it is sent from',
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
  // The store hands over each story's words lowercased once, for every key
  // typed; the answers are the same as working them out each time.
  const texts = new Map(list.map((s) => [s.id, storyText(s)]))
  for (const query of ['async', 'NIKO', 'hacker', 'rust blog', 'async niko', 'zebra', '']) {
    assert.deepEqual(
      ids(filterStories(list, query, sources, texts)),
      ids(filterStories(list, query, sources)),
      `the same with the words worked out already: "${query}"`,
    )
  }
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

// ── the kinds ────────────────────────────────────────────────────────────

const show = { author: 'A' }
const mail = (sender, name) => ({ type: 'mail', sender, ...(name ? { name } : {}) })

assert.deepEqual(
  FEED_KINDS.map((k) => KIND_LABELS[k]),
  ['All', 'Newsletters', 'Feeds', 'Podcasts'],
  'the chips, in the order they are drawn',
)
assert.deepEqual(Object.keys(KIND_NONE).sort(), ['feed', 'newsletter', 'podcast'])

{
  assert.equal(kindOf(source('blog')), 'feed', 'a web feed')
  assert.equal(
    kindOf(source('hn', { kind: { type: 'hackerNews', list: 'front', minPoints: 0 } })),
    'feed',
    'Hacker News is a feed',
  )
  assert.equal(kindOf(source('pod', { podcast: show })), 'podcast', 'a show')
  assert.equal(kindOf(source('pod', { podcast: null })), 'feed', 'no settings, no show')
  assert.equal(kindOf(source('m', { kind: mail('a@b.example') })), 'newsletter', 'a sender')
  assert.equal(
    kindOf(source('m', { kind: mail('a@b.example', 'The Download'), found: true })),
    'newsletter',
    'a found newsletter, which is a sender and a name',
  )
  assert.equal(kindOf(source('n', { kind: { type: 'newsletters' } })), 'newsletter', 'all of Mail')
  assert.equal(
    kindOf(source('both', { kind: mail('a@b.example'), podcast: show })),
    'podcast',
    'a show is a show, whatever it is read from: the order the service decides in',
  )

  assert.equal(isOfKind(source('blog'), 'all'), true, 'everything is under All')
  assert.equal(isOfKind(source('blog'), 'feed'), true)
  assert.equal(isOfKind(source('blog'), 'podcast'), false)
}

// ── what the sidebar lists, and what a place in it covers ────────────────

{
  const letters = { ...summary('news', 0, { kind: { type: 'newsletters' } }), total: 0 }
  const letterStories = { ...letters, total: 4 }
  const all = [
    summary('blog', 2),
    summary('pod', 5, { podcast: show }),
    summary('dl', 1, { kind: mail('dl@mit.example', 'The Download'), found: true }),
    letters,
  ]
  const named = (list) => list.map((s) => s.source.id)

  assert.deepEqual(
    named(listedSources(all)),
    ['blog', 'pod', 'dl'],
    'the Newsletters source is not listed while nothing is under it',
  )
  assert.deepEqual(
    named(listedSources([...all.slice(0, 3), letterStories])),
    ['blog', 'pod', 'dl', 'news'],
    'and is once something is',
  )
  assert.deepEqual(named(listedSources(all, 'podcast')), ['pod'], 'the Podcasts chip')
  assert.deepEqual(named(listedSources(all, 'feed')), ['blog'], 'the Feeds chip')
  assert.deepEqual(named(listedSources(all, 'newsletter')), ['dl'], 'the Newsletters chip')
  assert.deepEqual(
    named(listedSources([...all.slice(0, 3), letterStories], 'newsletter')),
    ['dl', 'news'],
    'a Newsletters source with issues is a newsletter',
  )
  assert.deepEqual(named(listedSources([], 'podcast')), [], 'nothing followed, nothing listed')
  assert.deepEqual(
    named(listedSources(all, 'podcast', 'blog')),
    ['blog', 'pod'],
    'the source on screen keeps its row, in its place, whatever the chip says',
  )
  assert.deepEqual(
    named(listedSources(all, 'all', 'news')),
    ['blog', 'pod', 'dl', 'news'],
    'even the Newsletters source with nothing under it, while it is the one open',
  )

  // What a place covers: the same sources the list, the count and "mark all
  // read" are made from.
  const filed = [
    summary('blog', 2, { folder: 'Mix' }),
    summary('pod', 5, { folder: 'Mix', podcast: show }),
    summary('dl', 1, { kind: mail('dl@mit.example', 'The Download') }),
  ]
  assert.deepEqual(named(scopeSources({ kind: 'all' }, filed)), ['blog', 'pod', 'dl'])
  assert.deepEqual(named(scopeSources({ kind: 'all' }, filed, 'podcast')), ['pod'])
  assert.deepEqual(named(scopeSources({ kind: 'all' }, filed, 'newsletter')), ['dl'])
  assert.deepEqual(
    named(scopeSources({ kind: 'all' }, [summary('blog')], 'podcast')),
    [],
    'a kind with nothing followed covers nothing, which is not the same as everything',
  )
  assert.deepEqual(named(scopeSources({ kind: 'folder', name: 'Mix' }, filed)), ['blog', 'pod'])
  assert.deepEqual(
    named(scopeSources({ kind: 'folder', name: ' Mix ' }, filed, 'podcast')),
    ['pod'],
    'a folder is what the chip leaves of it, its name compared trimmed',
  )
  assert.deepEqual(named(scopeSources({ kind: 'folder', name: 'Mix' }, filed, 'newsletter')), [])
  assert.deepEqual(
    named(scopeSources({ kind: 'source', id: 'blog' }, filed, 'podcast')),
    ['blog'],
    'one source is itself, whatever the chip says',
  )
  assert.deepEqual(named(scopeSources({ kind: 'upnext' }, filed)), [], 'Up Next is not sources')
  assert.deepEqual(named(scopeSources({ kind: 'history' }, filed)), [])

  assert.equal(scopeUnread({ kind: 'all' }, filed), 8)
  assert.equal(scopeUnread({ kind: 'all' }, filed, 'podcast'), 5, 'All, under a chip, is its kind')
  assert.equal(scopeUnread({ kind: 'all' }, filed, 'newsletter'), 1)
  assert.equal(scopeUnread({ kind: 'all' }, filed, 'feed'), 2)
  assert.equal(scopeUnread({ kind: 'folder', name: 'Mix' }, filed), 7, 'a show is in its folder')
  assert.equal(scopeUnread({ kind: 'folder', name: 'Mix' }, filed, 'feed'), 2)
  assert.equal(scopeUnread({ kind: 'source', id: 'pod' }, filed, 'feed'), 5)
  assert.equal(scopeUnread({ kind: 'history' }, filed), 0)

  assert.equal(scopeTitle({ kind: 'all' }, filed, 'all'), 'All')
  assert.equal(scopeTitle({ kind: 'all' }, filed, 'newsletter'), 'All newsletters')
  assert.equal(scopeTitle({ kind: 'all' }, filed, 'podcast'), 'All podcasts')
  assert.equal(scopeTitle({ kind: 'folder', name: 'Mix' }, filed, 'podcast'), 'Mix')
  assert.equal(scopeTitle({ kind: 'source', id: 'pod' }, filed, 'feed'), 'pod')

  // Shows are rows in their folder like any other source, and a folder of
  // nothing but shows is a folder.
  const folders = groupFolders([
    summary('blog', 2, { folder: 'news' }),
    summary('pod', 5, { folder: 'news', podcast: show }),
    summary('only', 3, { folder: 'shows', podcast: show }),
    summary('loose', 1, { podcast: show }),
  ])
  assert.deepEqual(
    folders.map((f) => f.name),
    ['', 'news', 'shows'],
  )
  assert.deepEqual(
    folders.map((f) => named(f.sources)),
    [['loose'], ['blog', 'pod'], ['only']],
    'a show is in its folder, or unfiled',
  )
  assert.deepEqual(
    folders.map((f) => f.unread),
    [1, 7, 3],
  )
  assert.deepEqual(folderNames(folders.flatMap((f) => f.sources)), ['news', 'shows'])
}

// ── finding a row in the sidebar ─────────────────────────────────────────

{
  const folders = groupFolders([
    summary('dl', 1, { title: 'The Download', folder: 'Tech' }),
    summary('algo', 2, { title: 'The Algorithm', folder: 'Tech' }),
    summary('long', 0, { title: 'Longform Weekly', folder: 'Reading' }),
    summary('radar', 3, { title: 'Tech Radar', folder: 'Reading' }),
    summary('slow', 0, { title: 'The Slow Hour', podcast: show }),
  ])
  // Folders: unfiled, Reading, Tech.
  const found = (query) =>
    searchNav(folders, query).map((f) => [f.folder.name, f.rows.map((s) => s.source.id)])

  assert.deepEqual(
    found(''),
    [
      ['', ['slow']],
      ['Reading', ['long', 'radar']],
      ['Tech', ['dl', 'algo']],
    ],
    'nothing typed is everything',
  )
  assert.deepEqual(found('   '), found(''), 'and so is a blank')
  assert.deepEqual(found('download'), [['Tech', ['dl']]], 'a source by its name')
  assert.deepEqual(found('DOWNLOAD'), [['Tech', ['dl']]], 'in any case')
  assert.deepEqual(found('slow'), [['', ['slow']]], 'an unfiled source')
  assert.deepEqual(found('the algo'), [['Tech', ['algo']]], 'every word, in the name, in any order')
  assert.deepEqual(found('algo the'), [['Tech', ['algo']]])
  assert.deepEqual(
    found('reading'),
    [['Reading', ['long', 'radar']]],
    'a folder by its own name keeps every row it has',
  )
  assert.deepEqual(
    found('tech'),
    [
      ['Reading', ['radar']],
      ['Tech', ['dl', 'algo']],
    ],
    'a name that matches in one place and a folder that matches in another',
  )
  assert.deepEqual(found('unfiled'), [], 'the unfiled sources have no folder name to find')
  assert.deepEqual(found('zzz'), [], 'nothing matches')
  assert.deepEqual(found('download zzz'), [], 'every word has to')
  assert.equal(
    searchNav(folders, 'download')[0].folder,
    folders[2],
    'the folder is the grouped one, with the unread it had, for its menu',
  )
}

assert.equal(
  NOT_NEWSLETTER_DETAIL,
  'It goes from the Feed and will not be found again. Its emails stay in Mail.',
)

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

  // In a story's own type: larger and looser is taller, a wider padding a
  // narrower column.
  const type = (sizePx, leading, letterEm = 0.5) => ({
    sizePx,
    leading,
    letterEm,
    padX: (width) => Math.min(28, Math.max(16, width * 0.04)),
    padY: 48,
  })
  const article = doc(para.repeat(20))
  const reader = estimateArticleHeight(article, 700, type(18, 1.6))
  assert.ok(reader > long / 2, 'twenty paragraphs at 18px are no shorter than forty at 15px halved')
  assert.ok(estimateArticleHeight(article, 700, type(24, 1.6)) > reader, 'larger type is taller')
  assert.ok(estimateArticleHeight(article, 700, type(18, 1.8)) > reader, 'looser lines are taller')
  const essay = doc(`<p>${'word '.repeat(300)}</p>`.repeat(10))
  assert.ok(
    estimateArticleHeight(essay, 700, type(18, 1.6, 0.52)) >
      estimateArticleHeight(essay, 700, type(18, 1.6)),
    'a serif, which runs wider, is taller',
  )
  assert.ok(
    estimateArticleHeight(article, 1000, type(18, 1.6)) < reader,
    'a wider column is shorter',
  )
}

// ── bodyAboutOf ──────────────────────────────────────────────────────────

{
  const head = (metas) =>
    `<!DOCTYPE html><html><head><meta charset="utf-8">${metas}</head><body><p>x</p></body></html>`
  assert.deepEqual(
    bodyAboutOf(
      head(
        '<meta name="news-byline" content="Ada &quot;Countess&quot; &lt;L&gt; &amp; Co &#39;26">\n' +
          '<meta name="news-site" content="The Gazette">',
      ),
    ),
    { byline: 'Ada "Countess" <L> & Co \'26', site: 'The Gazette' },
    "both read, with the service's escapes undone",
  )
  assert.deepEqual(
    bodyAboutOf(head('')),
    { byline: null, site: null },
    'a document that does not say',
  )
  assert.deepEqual(
    bodyAboutOf('<p><meta name="news-byline" content="In the body"></p>'),
    { byline: null, site: null },
    'only the head counts: a publisher cannot claim to be somebody else',
  )
}

// "Not newsletters" says what happened in Mail, not what was asked.
{
  const note = feed.notNewslettersNote
  assert.equal(
    note(['The Download'], 'other', { moved: 4, shared: 0 }).body,
    'Its emails are now under Other in Mail.',
  )
  assert.equal(
    note(['The Download'], 'other', { moved: 0, shared: 1 }).body,
    'Its emails stay in Newsletters in Mail: another newsletter in the Feed comes from the same address.',
    'a sibling newsletter from the same address keeps the mail where it is',
  )
  assert.match(
    note(['A', 'B'], 'important', { moved: 3, shared: 1 }).body,
    /^Moved 3 emails to Important in Mail\. Mail from an address another newsletter/,
  )
  assert.equal(
    note(['A', 'B'], 'other', { moved: 0, shared: 0 }).body,
    'They had no emails in Mail to move.',
  )
  assert.equal(note(['A'], null).body, 'Its emails stay where they were in Mail.')
  assert.equal(note(['A', 'B'], null).title, 'Took 2 sources out of the Feed')
}

await close()
console.log('feed: all checks passed')
