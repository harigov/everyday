// Behaviour checks for the Markdown renderer.
//
// Same reasoning as the test files beside this one: the type checker covers
// most of the interface, and what it cannot cover is the modules made of
// rules. `markdown.ts` has two kinds of rule and both fail quietly.
//
// The first is safety. Its output goes into the document through `{@html}`,
// and the text comes from a model on somebody else's server describing the
// contents of an encrypted journal. A single unescaped `<` is a script tag
// in a window that has the vault open. Nothing about that fails to compile,
// and it would not be noticed in ordinary use, so it is checked here from
// every direction: raw tags, tags smuggled through a code fence, a link
// whose scheme is `javascript:`, and an `onerror` attribute.
//
// The second is fidelity, and its failure is the reason the module exists:
// a reply that is right and unreadable. A list that renders as one run-on
// paragraph, a `**` that survives into the text, a code block in the same
// face as the prose around it. Each of those is one regex away at all times.
//
// The streaming cases are third and are their own hazard. The panel renders
// a reply as it streams, so it renders half a document constantly -- a
// fence that has not closed, a list mid-item. Well-formed HTML out of
// malformed Markdown in is the contract.
//
// No test framework, deliberately: one dependency-free file, run by
// `npm run check`, with the TypeScript loaded through Vite so it compiles
// exactly as the application compiles it.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: markdown, close } = await load('/src/lib/markdown.ts')
const { renderMarkdown, escapeHtml } = markdown

/** Assert that `source` renders to exactly `want`. */
function renders(source, want, why) {
  assert.equal(renderMarkdown(source), want, why ?? source)
}

/** Assert that `source` renders to something containing `fragment`. */
function includes(source, fragment, why) {
  const html = renderMarkdown(source)
  assert.ok(html.includes(fragment), `${why ?? source}\n  wanted: ${fragment}\n  got:    ${html}`)
}

// ── Safety ────────────────────────────────────────────────────────────
//
// The property the whole module rests on: no path from the input to a tag
// this file did not write.

assert.equal(escapeHtml('<b>&"\'</b>'), '&lt;b&gt;&amp;&quot;&#39;&lt;/b&gt;')

/** Every tag this module is allowed to write. Nothing else may appear. */
const ALLOWED = new Set([
  'p',
  'br',
  'hr',
  'em',
  'strong',
  'del',
  'mark',
  'u',
  'code',
  'pre',
  'a',
  'blockquote',
  // Only ever `<button type="button" class="file-chip" data-file="<id>">`: a
  // link to a file in Files. See the checks below.
  'button',
  'ul',
  'ol',
  'li',
  'h1',
  'h2',
  'h3',
  'h4',
  'h5',
  'h6',
  'table',
  'thead',
  'tbody',
  'tr',
  'th',
  'td',
])

/** The tags in some HTML, and their attributes, for the checks below. */
function tagsIn(html) {
  return [...html.matchAll(/<\/?([a-zA-Z][\w-]*)((?:\s[^>]*)?)>/g)].map((m) => ({
    name: m[1].toLowerCase(),
    attributes: m[2] ?? '',
  }))
}

for (const attack of [
  '<script>alert(1)</script>',
  '<img src=x onerror=alert(1)>',
  '<iframe src="javascript:alert(1)"></iframe>',
  '<div onclick="steal()">hello</div>',
  '<svg/onload=alert(1)>',
  '<a href="javascript:alert(1)">x</a>',
  '`<script>alert(1)</script>`',
  '# <script>alert(1)</script>',
  '- <script>alert(1)</script>',
  '| <script>alert(1)</script> |\n| - |\n| x |',
  // The one tag Markdown is written with here, carrying something it should not.
  '<u onclick="steal()">x</u>',
  '<u><script>alert(1)</script></u>',
  '<javascript:alert(1)>',
]) {
  const html = renderMarkdown(attack)
  for (const tag of tagsIn(html)) {
    assert.ok(ALLOWED.has(tag.name), `a tag this module does not write survived: ${html}`)
    assert.ok(!/\son\w+=/i.test(tag.attributes), `an event handler survived: ${html}`)
  }
  assert.ok(html.includes('&lt;'), `the angle bracket was not escaped: ${html}`)
}

// Inside a code fence too, where a renderer that escapes late would let it
// through: the fence body is escaped by the same function as everything else.
includes('```\n<script>alert(1)</script>\n```', '&lt;script&gt;')

// A link's scheme is checked against a list, so the two that execute cannot
// be written. The text is still shown -- a reader should see what was said.
for (const scheme of ['javascript:alert(1)', 'data:text/html,<script>x</script>', 'vbscript:x']) {
  const html = renderMarkdown(`[click me](${scheme})`)
  assert.ok(!html.includes('<a '), `a dangerous scheme was linked: ${html}`)
  assert.ok(html.includes('click me'), `the label was lost: ${html}`)
}

// A link to a file in Files -- how the assistant names one -- is a button
// carrying the id, never an address: nothing that can be navigated to, and
// nothing in it but the id.
{
  const id = '0192f3a4-5b6c-7d8e-9f01-23456789abcd'
  includes(
    `[the plan](files:${id})`,
    `<button type="button" class="file-chip" data-file="${id}">the plan</button>`,
  )
  includes(`see [**the plan**](files:${id}) now`, '<strong>the plan</strong></button> now')
  includes(`[](files:${id})`, '>File</button>', 'a link with no words is called File')
  includes(`[the plan](files:${id.toUpperCase()})`, `data-file="${id}"`, 'the id is lower-cased')
  includes(`[the plan](files:${id.replaceAll('-', '')})`, `data-file="${id}"`, 'and hyphenated')
  assert.ok(!renderMarkdown(`[the plan](files:${id})`).includes('href'), 'it is not an address')
  // Anything but an id after the scheme is the text that was typed.
  for (const bad of [
    'files:',
    'files:abc',
    'files:../../etc/passwd',
    `files:${id}/extra`,
    `files:${id}"onclick="steal()`,
    'files:<script>alert(1)</script>',
  ]) {
    const html = renderMarkdown(`[the plan](${bad})`)
    assert.ok(!html.includes('<button'), `not an id, so not a chip: ${html}`)
    assert.ok(!html.includes('<script'), `a tag was written that should not be: ${html}`)
    assert.ok(html.includes('the plan'), `the label was lost: ${html}`)
  }
  for (const tag of tagsIn(renderMarkdown(`[a *b*](files:${id})`))) {
    assert.ok(ALLOWED.has(tag.name), `a tag this module does not write: ${tag.name}`)
    assert.ok(!/\son\w+=/i.test(tag.attributes), `an event handler on a chip: ${tag.attributes}`)
  }
}

// And the three that do not execute are linked, away from this window.
includes('[docs](https://example.org/x)', '<a href="https://example.org/x"')
includes('[docs](https://example.org/x)', 'rel="noreferrer noopener"')
includes('[mail](mailto:someone@example.org)', '<a href="mailto:someone@example.org"')

// ── Blocks ────────────────────────────────────────────────────────────

renders('Just a sentence.', '<p>Just a sentence.</p>')
renders('One.\n\nTwo.', '<p>One.</p><p>Two.</p>')
renders('# Title', '<h1>Title</h1>')
renders('### Third ###', '<h3>Third</h3>')
renders('---', '<hr>')
renders('> quoted', '<blockquote><p>quoted</p></blockquote>')

// A heading directly under a paragraph is a heading, not the paragraph's
// second line. Models write without blank lines constantly.
renders('Before\n## After', '<p>Before</p><h2>After</h2>')

// Lists. The failure this catches is the one that made the panel unreadable:
// three bullets rendered as one paragraph with hyphens in it.
renders('- one\n- two', '<ul><li>one</li><li>two</li></ul>')
renders('1. one\n2. two', '<ol><li>one</li><li>two</li></ol>')
renders('3. three\n4. four', '<ol start="3"><li>three</li><li>four</li></ol>')
renders('- outer\n  - inner', '<ul><li>outer<ul><li>inner</li></ul></li></ul>')
renders('Intro:\n- one\n- two', '<p>Intro:</p><ul><li>one</li><li>two</li></ul>')

// A blank line between items makes the list loose, and its items keep their
// paragraphs -- which is what puts air between them.
renders('- one\n\n- two', '<ul><li><p>one</p></li><li><p>two</p></li></ul>')

// A wrapped bullet is one item, not two.
renders('- one that\n  carries on', '<ul><li>one that<br>carries on</li></ul>')

// Code, fenced and indented, with the language kept for the class hook.
renders('```\nx = 1\n```', '<pre><code>x = 1</code></pre>')
renders('```js\nx = 1\n```', '<pre><code class="language-js">x = 1</code></pre>')
renders('    x = 1', '<pre><code>x = 1</code></pre>')

// A fence is literal all the way through: no list, no heading, no emphasis.
renders(
  '```\n- not a list\n# not a heading\n**not bold**\n```',
  '<pre><code>- not a list\n# not a heading\n**not bold**</code></pre>',
)

// Tables, because a model asked for a comparison answers with one.
includes('| a | b |\n| - | - |\n| 1 | 2 |', '<table>')
includes('| a | b |\n| - | - |\n| 1 | 2 |', '<td>1</td><td>2</td>')
includes('| a |\n| --: |\n| 1 |', 'style="text-align:right"')

// ── Inline ────────────────────────────────────────────────────────────

renders('**bold**', '<p><strong>bold</strong></p>')
renders('__bold__', '<p><strong>bold</strong></p>')
renders('*it*', '<p><em>it</em></p>')
renders('_it_', '<p><em>it</em></p>')
renders('***both***', '<p><strong><em>both</em></strong></p>')
renders('~~gone~~', '<p><del>gone</del></p>')
renders('`code`', '<p><code>code</code></p>')

// The reason code spans are lifted out before emphasis runs.
renders('`**not bold**`', '<p><code>**not bold**</code></p>')

// An underscore inside a word is an underscore. `snake_case_name` reading as
// `snake<em>case</em>name` is the classic way this goes wrong.
renders('snake_case_name', '<p>snake_case_name</p>')

// The bug that made link tags worth parking: two links in one line, each
// carrying `target="_blank"`, put a pair of underscores around the markup
// between them.
{
  const html = renderMarkdown('[a](https://x.test) and [b](https://y.test)')
  assert.equal(html.match(/<a /g).length, 2, html)
  assert.ok(!html.includes('<em>'), `emphasis leaked into the markup: ${html}`)
}

// Emphasis inside a link label still renders, which is why only the tags are
// parked and not the whole link.
includes('[**bold** link](https://x.test)', '<strong>bold</strong>')

// A bare address is a link, and the full stop that ends the sentence is not
// part of it.
includes('see https://example.org/a_b for more', 'href="https://example.org/a_b"')
{
  const html = renderMarkdown('see https://example.org.')
  assert.ok(html.includes('href="https://example.org"'), html)
  assert.ok(html.endsWith('.</p>'), `the sentence lost its full stop: ${html}`)
}

// A query string survives whole. This runs on escaped text, where `&` is
// five characters, and the first attempt stopped the address at it -- so a
// link with two parameters pointed at a page with one, and the rest of the
// query was left sitting in the prose beside it.
{
  const html = renderMarkdown('see https://example.org/s?a=1&b=2 for more')
  assert.ok(html.includes('href="https://example.org/s?a=1&amp;b=2"'), html)
  assert.ok(!html.includes('b=2 for more'), `the query was cut in half: ${html}`)
  assert.equal(html.match(/<a /g).length, 1, html)
}
// The same, written as a Markdown link.
includes('[q](https://example.org/s?a=1&b=2)', 'href="https://example.org/s?a=1&amp;b=2"')

// An entity that really does end an address still ends it: the closing
// quotation mark is `&quot;` by the time this runs, and allowing `&` through
// must not let the address swallow it.
{
  const html = renderMarkdown('he said "go to https://example.org/x" and left')
  assert.ok(html.includes('href="https://example.org/x"'), html)
  assert.ok(!html.includes('href="https://example.org/x&'), `it ate the quotation: ${html}`)
  assert.ok(html.includes('&quot; and left'), `the quotation was lost: ${html}`)
}
// A trailing semicolon is a sentence's, but the one inside `&amp;` is not.
{
  const html = renderMarkdown('https://example.org/s?a=1&b=2;')
  assert.ok(html.includes('href="https://example.org/s?a=1&amp;b=2"'), html)
}

// A single newline inside a paragraph is a line the writer meant to break.
renders('one\ntwo', '<p>one<br>two</p>')

// ── Streaming ─────────────────────────────────────────────────────────
//
// The panel renders on every chunk, so half a document is the normal input.
// Nothing below may throw, and everything must close its own tags.

for (const half of [
  '```js\nconst x =',
  '- one\n- tw',
  '**bol',
  '[label](https://exa',
  '| a | b |\n| - | - |\n| 1',
  '> quoted and cut',
  '#',
  '',
]) {
  const html = renderMarkdown(half)
  const opened = [...html.matchAll(/<(\w+)[^>]*>/g)].map((m) => m[1])
  const closed = [...html.matchAll(/<\/(\w+)>/g)].map((m) => m[1])
  for (const tag of new Set(opened)) {
    if (tag === 'br' || tag === 'hr') continue
    assert.equal(
      opened.filter((t) => t === tag).length,
      closed.filter((t) => t === tag).length,
      `<${tag}> was left open by "${half}": ${html}`,
    )
  }
}

// A NUL in the input cannot be made to point at a parked slot.
assert.ok(!renderMarkdown(' 0  `code`').includes('<code>code</code><code>'))

// ── What the editor's copy writes ─────────────────────────────────────
//
// A note copied out of the editor is Markdown with three things a chat reply
// rarely has; pasted back, or anywhere this renders, each is drawn.

renders('==marked==', '<p><mark>marked</mark></p>')
renders('<u>under</u>lined', '<p><u>under</u>lined</p>')
renders('<u>**both**</u>', '<p><u><strong>both</strong></u></p>')
includes('see <https://example.org/a_b?x=1&y=2>', '<a href="https://example.org/a_b?x=1&amp;y=2"')
includes('see <https://example.org/a_b>', '>https://example.org/a_b</a>')
includes('<mailto:me@example.org>', '<a href="mailto:me@example.org"')
renders('a == b == c', '<p>a == b == c</p>', 'a comparison is not a highlight')
renders('if a <u and b> c', '<p>if a &lt;u and b&gt; c</p>', 'nor is an angle bracket a tag')

// ── A whole reply ─────────────────────────────────────────────────────
//
// What the panel actually receives, end to end.

{
  const html = renderMarkdown(
    [
      "Here's what I found:",
      '',
      '1. **Three tasks** are overdue',
      '2. One is in `Home`',
      '',
      '> You asked about this on Tuesday.',
      '',
      '```sh',
      'jatayu search rain',
      '```',
    ].join('\n'),
  )
  assert.ok(html.startsWith('<p>Here&#39;s what I found:</p>'), html)
  assert.ok(html.includes('<ol><li><strong>Three tasks</strong> are overdue</li>'), html)
  assert.ok(html.includes('<code>Home</code>'), html)
  assert.ok(html.includes('<blockquote><p>You asked about this on Tuesday.</p></blockquote>'), html)
  assert.ok(html.includes('<pre><code class="language-sh">jatayu search rain</code></pre>'), html)
}

// ── A block at a time ─────────────────────────────────────────────────
//
// A streaming reply is drawn block by block, so finished paragraphs keep
// their nodes. Joined, the blocks must be exactly the whole render, at every
// length the stream passes through: a fence still open, a loose list, a
// table arriving a line at a time, a lazy quote. And `MarkdownStream`, which
// reads again only near the end of a growing text, must give exactly the
// blocks a fresh read does -- whatever size the pieces arrive in.

{
  const { MarkdownStream, renderMarkdownBlocks } = markdown
  const samples = [
    [
      "Here's what I found:",
      '',
      '1. **Three tasks** are overdue',
      '2. One is in `Home`',
      '',
      '> You asked about this on Tuesday.',
      'and again on Friday',
      '',
      '```sh',
      'jatayu search rain',
      '',
      'jatayu list',
      '```',
      '',
      '- first',
      '',
      '- second, after a gap',
      '  continued under it',
      '',
      '| Day | Rain |',
      '| --- | ---: |',
      '| Mon | 4 mm |',
      '| Tue | none |',
      '',
      '# Next',
      '---',
      '    indented code',
      '',
      '    more of it',
      'A [link](https://example.com/a?b=1&c=2), *some* **emphasis** and',
      'a [file](files:0e3f1c2a-1b2c-4d5e-8f90-123456789abc).',
    ].join('\n'),
    'foo\na | b\n|---|---\n| 1 | 2 |\nbar',
    '* one\n* two\n\n\n1. three\n2) four',
    // A list that turns loose items later, code that runs on past a blank
    // line, a quote with a lazy line, a table under a paragraph, and the
    // line endings and tabs `prepare` rewrites.
    [
      'Intro line',
      '',
      '- a',
      '- b',
      '',
      '',
      '- c, which makes it loose',
      '',
      'Between',
      '',
      '    code one',
      '',
      '',
      '    code two',
      'After the code',
      '> quoted',
      'lazily continued',
      '',
      'para',
      'x | y',
      '|---|---|',
      '| 1 | 2 |',
      '',
      '1. one',
      '   nested under it',
      '   - deeper',
      '2. two',
      'end',
    ].join('\n'),
    'one\r\ntwo\r\n\r\n- a\r\n\tindented by a tab\r\n\r\nthree',
  ]
  for (const sample of samples) {
    for (let n = 0; n <= sample.length; n++) {
      const prefix = sample.slice(0, n)
      const blocks = renderMarkdownBlocks(prefix)
      assert.equal(blocks.join(''), renderMarkdown(prefix), `prefix ${n} of ${sample.slice(0, 20)}`)
      assert.ok(
        blocks.every((html) => html.startsWith('<')),
        `every block is markup: ${blocks}`,
      )
    }
  }
  assert.deepEqual(renderMarkdownBlocks(''), [])
  assert.deepEqual(renderMarkdownBlocks('one\n\ntwo'), ['<p>one</p>', '<p>two</p>'])

  // A character at a time, and in pieces of any size: a small fixed-seed
  // generator, so a failure names a size that can be run again.
  let seed = 7
  const next = () => (seed = (seed * 1103515245 + 12345) % 2147483648)
  for (const sample of samples) {
    const one = new MarkdownStream()
    for (let n = 0; n <= sample.length; n++) {
      const prefix = sample.slice(0, n)
      assert.deepEqual(one.render(prefix), renderMarkdownBlocks(prefix), `streamed to ${n}`)
    }
    for (let round = 0; round < 20; round++) {
      const stream = new MarkdownStream()
      let n = 0
      while (n < sample.length) {
        n = Math.min(sample.length, n + 1 + (next() % 40))
        const prefix = sample.slice(0, n)
        assert.deepEqual(stream.render(prefix), renderMarkdownBlocks(prefix), `pieces to ${n}`)
      }
    }
  }

  // Text that is not the last with more on the end -- a retry starting over,
  // a reply cut to what was heard -- is read whole.
  const stream = new MarkdownStream()
  stream.render(samples[0])
  assert.deepEqual(stream.render('Again: one\n\ntwo'), ['<p>Again: one</p>', '<p>two</p>'])
  const head = samples[0].slice(0, 200)
  assert.deepEqual(stream.render(head), renderMarkdownBlocks(head))
  assert.deepEqual(stream.render(''), [])
  assert.deepEqual(stream.render('back'), ['<p>back</p>'])
}

await close()
console.log('markdown: all checks passed')
