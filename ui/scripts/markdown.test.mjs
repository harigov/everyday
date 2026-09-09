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
// a reply on every chunk, so it renders half a document constantly -- a
// fence that has not closed, a list mid-item. Well-formed HTML out of
// malformed Markdown in is the contract.
//
// No test framework, deliberately: one dependency-free file, run by
// `npm run check`, with the TypeScript loaded through Vite so it compiles
// exactly as the application compiles it.

import assert from 'node:assert/strict'
import { createServer } from 'vite'

const server = await createServer({
  configFile: false,
  root: new URL('..', import.meta.url).pathname,
  // `watch: null` because a test loads a module once and exits. Vite's
  // watcher is on by default even in middleware mode, and a watcher is a
  // per-user resource: a suite that starts one server per file exhausts the
  // supply (`EMFILE`) on any machine that already has a dev server running.
  server: { middlewareMode: true, watch: null },
  appType: 'custom',
  logLevel: 'error',
})

const { renderMarkdown, escapeHtml } = await server.ssrLoadModule('/src/lib/markdown.ts')

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
  'code',
  'pre',
  'a',
  'blockquote',
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
      'everyday search rain',
      '```',
    ].join('\n'),
  )
  assert.ok(html.startsWith('<p>Here&#39;s what I found:</p>'), html)
  assert.ok(html.includes('<ol><li><strong>Three tasks</strong> are overdue</li>'), html)
  assert.ok(html.includes('<code>Home</code>'), html)
  assert.ok(html.includes('<blockquote><p>You asked about this on Tuesday.</p></blockquote>'), html)
  assert.ok(html.includes('<pre><code class="language-sh">everyday search rain</code></pre>'), html)
}

await server.close()
console.log('markdown: all checks passed')
