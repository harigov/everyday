// When pasted text is read as Markdown, and what it becomes.
//
// The editor pasted Markdown as the text it was, so a table copied out of a
// model's reply landed as a column of `| a | b |` lines. `markdown-paste.ts`
// decides which pastes are Markdown and renders them; both halves fail
// quietly -- too eager and a pasted poem loses its line breaks to a
// paragraph, too shy and the table is still pipes -- so they are pinned here.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module, close } = await load('/src/lib/markdown-paste.ts')
const { looksLikeMarkdown, markdownToPaste } = module

const TABLE = '| Day | Miles |\n|---|---:|\n| Mon | 3 |\n| Tue | 5 |'

// ── What counts as Markdown ───────────────────────────────────────────

for (const text of [
  TABLE,
  'Intro\n\n## Heading\n\nBody',
  '# Title',
  '- one\n- two',
  '1. first\n2. second',
  '> quoted',
  '```\ncode\n```',
  // A table without its outer pipes is still a table.
  'a | b\n--|--\n1 | 2',
  // A link has no paste rule of TipTap's to catch it, so it counts alone.
  'See [the plan](https://example.com/plan).',
  'See <https://example.com/plan>.',
]) {
  assert.ok(looksLikeMarkdown(text), `should read as Markdown:\n${text}`)
}

for (const text of [
  'Just a sentence.',
  'Roses are red\nViolets are blue',
  // Inline marks are left to the editor's own paste rules.
  'It **rained** all day.',
  // A tag is not a heading.
  '#gratitude for the rain',
  // A pipe in prose, with no rule under it, is not a table -- and nor is
  // one over a rule without a column for each of its cells.
  'either this | or that',
  'Options: a | b\n---',
  // A signature's rule is not Markdown on its own.
  'Thanks,\n---\nHari',
  // Source code, which is what `# comment` and indentation mean in it.
  '# helpers\nimport os\n\ndef run(__name__, **kw):\n    return os.path.join(__file__, __name__)',
  '# install it\napt install ripgrep',
  'def f():\n    # not a heading\n\n    - not a list',
  // Brackets in prose are not a link.
  'See note [1] (below).',
  'Tuple [a, b] (x, y)',
]) {
  assert.ok(!looksLikeMarkdown(text), `should stay plain text:\n${text}`)
}

// ── What it becomes ───────────────────────────────────────────────────

{
  const html = markdownToPaste(TABLE, '')
  assert.ok(html?.includes('<table>'), `a pipe table becomes a table: ${html}`)
  assert.ok(html.includes('<th>Day</th>'), `its first row is the header: ${html}`)
  assert.ok(html.includes('<td style="text-align:right">5</td>'), `with its cells: ${html}`)
  assert.ok(!html.includes('---'), `and the rule under the header is gone: ${html}`)
}
{
  // Left is said out loud too: the editor keeps it, and copies it back out.
  const html = markdownToPaste('| a | b | c |\n| :--- | :---: | --- |\n| 1 | 2 | 3 |', '')
  assert.ok(html?.includes('<td style="text-align:left">1</td>'), `left: ${html}`)
  assert.ok(html.includes('<td style="text-align:center">2</td>'), `centre: ${html}`)
  assert.ok(html.includes('<td>3</td>'), `and none: ${html}`)
}
{
  // What exporting a cell with a pipe in it writes, read back.
  const html = markdownToPaste('| Note |\n| --- |\n| this \\| that |', '')
  assert.ok(html?.includes('<td>this | that</td>'), `an escaped pipe stays in its cell: ${html}`)
}

const EDITOR = { taskLists: true, maxHeading: 3 }

assert.equal(
  markdownToPaste('- [ ] milk\n- [x] bread', ''),
  '<ul><li>[ ] milk</li><li>[x] bread</li></ul>',
  'boxes stay text for an editor without checklists',
)
assert.equal(
  markdownToPaste('- [ ] milk\n- [x] bread', '', EDITOR),
  '<ul data-type="taskList"><li data-type="taskItem" data-checked="false">milk</li>' +
    '<li data-type="taskItem" data-checked="true">bread</li></ul>',
  'a checklist becomes the editor’s checklist, ticks and all',
)
assert.equal(
  markdownToPaste('- [ ] milk\n\n- [x] bread', '', EDITOR),
  '<ul data-type="taskList"><li data-type="taskItem" data-checked="false"><p>milk</p></li>' +
    '<li data-type="taskItem" data-checked="true"><p>bread</p></li></ul>',
  'so does a loose one',
)
for (const mixed of ['- milk\n- [ ] bread', '- [ ] milk\n- bread']) {
  const html = markdownToPaste(mixed, '', EDITOR)
  assert.ok(
    !html.includes('data-type'),
    `a list is a checklist only when every item has a box, or the editor has no shape for it: ${html}`,
  )
}
assert.equal(
  markdownToPaste('- [ ] pack\n  - [x] socks\n  - boots', '', EDITOR),
  '<ul data-type="taskList"><li data-type="taskItem" data-checked="false">pack' +
    '<ul><li>[x] socks</li><li>boots</li></ul></li></ul>',
  'each list is decided on its own items, nested ones included',
)
assert.equal(
  markdownToPaste('#### Step 1\n\nDo it.', '', EDITOR),
  '<h3>Step 1</h3><p>Do it.</p>',
  'a heading deeper than the editor has is drawn at its deepest, not lost',
)
assert.equal(
  markdownToPaste('## Summary\n\nAll done.', '', { maxHeading: 0 }),
  '<p><strong>Summary</strong></p><p>All done.</p>',
  'in an editor with no headings -- mail -- a heading is a bold line',
)
assert.equal(
  markdownToPaste('See [the plan](https://example.com/plan).', ''),
  '<p>See <a href="https://example.com/plan" target="_blank" rel="noreferrer noopener">the plan</a>.</p>',
  'a link pasted alone arrives as a link',
)
assert.ok(
  markdownToPaste('| x | y |\n|---|---|\n| 1 | 2 |\n- note | aside', '')?.endsWith(
    '</table><ul><li>note | aside</li></ul>',
  ),
  'a list item under a table is a list item, pipe or not',
)

// ── When the clipboard's HTML wins ────────────────────────────────────

assert.equal(
  markdownToPaste(TABLE, '<table><tr><td>Day</td></tr></table>'),
  null,
  'HTML with structure is the better copy -- a table off a web page',
)
assert.equal(
  markdownToPaste(
    'Agenda\n- budget, see the sheet',
    '<div><p>Agenda</p><div>- budget, see <a href="https://example.com/s">the sheet</a></div></div>',
  ),
  null,
  'so is HTML with a link in it, which the plain text has lost the address of',
)
assert.equal(
  markdownToPaste('- one\n- two', '<ul data-pm-slice="1 1 []"><li><p>one</p></li></ul>'),
  null,
  'anything copied out of the editor itself is pasted as it was',
)
assert.ok(
  markdownToPaste(
    TABLE,
    '<div style="color: #ccc"><div><span>| Day | Miles |</span></div></div>',
  )?.includes('<table>'),
  'a code editor’s HTML is the source with colours on, so the Markdown wins',
)
assert.ok(
  markdownToPaste(
    TABLE,
    '<html><body><!--StartFragment--><div><span>| Day |</span><br></div><!--EndFragment--></body></html>',
  )?.includes('<table>'),
  'including the fragment comments and page tags Windows wraps it in',
)
assert.equal(markdownToPaste('Just a sentence.', ''), null, 'plain text is left alone')
assert.equal(markdownToPaste('   ', ''), null, 'and so is nothing')

// ── Safety ────────────────────────────────────────────────────────────
//
// The HTML goes into the editor's paste, which parses it into the document.
// Nothing in it may be a tag the renderer did not write.

{
  const html = markdownToPaste('- <img src=x onerror=alert(1)>\n- [ ] <script>x</script>', '')
  assert.ok(!/<(img|script)/i.test(html), `raw tags stay text: ${html}`)
}

await close()
console.log('markdown-paste: all checks passed')
