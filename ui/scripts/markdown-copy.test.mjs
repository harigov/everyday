// What a copy out of the editor leaves on the clipboard as plain text.
//
// ProseMirror's own plain text is the document's characters with a blank
// line between blocks, so a note pasted anywhere but another rich editor
// lost its headings, bold, links, boxes and tables. `markdown-copy.ts`
// writes Markdown instead.
//
// The slices here are cut by real selections over the editor's real schema,
// rather than written out by hand, because the shape of a slice is the whole
// difficulty: a word in a list item is copied as a list holding an item
// holding a paragraph, and cells as bare rows with no table round them.

import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { load } from './harness.mjs'

const { modules, close } = await load([
  '/src/lib/markdown-copy.ts',
  '@tiptap/core',
  '@tiptap/starter-kit',
  '@tiptap/extension-link',
  '@tiptap/extension-underline',
  '@tiptap/extension-highlight',
  '@tiptap/extension-task-list',
  '@tiptap/extension-task-item',
  '@tiptap/extension-table',
  '@tiptap/pm/state',
  '@tiptap/pm/tables',
])
const [
  { sliceToMarkdown },
  { getSchema, Node },
  { default: StarterKit },
  { default: Link },
  { default: Underline },
  { default: Highlight },
  { default: TaskList },
  { default: TaskItem },
  { TableKit },
  { TextSelection, AllSelection },
  { CellSelection },
] = modules

// `media-node.ts` reaches for the backend to draw itself; only its name and
// attributes matter to a schema.
const Media = Node.create({
  name: 'media',
  group: 'block',
  atom: true,
  addAttributes: () =>
    Object.fromEntries(
      ['blob', 'kind', 'mime', 'filename', 'caption'].map((key) => [key, { default: '' }]),
    ),
})

// As `RichText.svelte` configures them.
const schema = getSchema([
  StarterKit.configure({ heading: { levels: [1, 2, 3] }, link: false, underline: false }),
  Link,
  Underline,
  Highlight,
  TaskList,
  TaskItem.configure({ nested: true }),
  TableKit.configure({ table: { resizable: false } }),
  Media,
])

// ── Building documents ────────────────────────────────────────────────

const text = (t, ...marks) => ({
  type: 'text',
  text: t,
  ...(marks.length ? { marks: marks.map((m) => (typeof m === 'string' ? { type: m } : m)) } : {}),
})
const p = (...content) => ({ type: 'paragraph', content: content.map(asNode) })
const h = (level, t) => ({ type: 'heading', attrs: { level }, content: [text(t)] })
const li = (...content) => ({ type: 'listItem', content })
const task = (checked, ...content) => ({ type: 'taskItem', attrs: { checked }, content })
const cell = (t, type = 'tableCell') => ({ type, content: [p(t)] })
const row = (...cells) => ({ type: 'tableRow', content: cells })
const asNode = (c) => (typeof c === 'string' ? text(c) : c)
const doc = (...content) => schema.nodeFromJSON({ type: 'doc', content })

/** What the clipboard's plain text is for this selection. */
function copied(selection) {
  const slice = selection.content()
  return sliceToMarkdown(slice.content.toJSON() ?? [], slice.openStart, slice.openEnd)
}

/** The position just before the first `needle` in the text at or after `from`. */
function at(d, needle, from = 0) {
  let found = null
  d.descendants((node, pos) => {
    if (found !== null || !node.isText) return
    const i = node.text.indexOf(needle, Math.max(0, from - pos))
    if (i !== -1) found = pos + i
  })
  assert.notEqual(found, null, `"${needle}" is in the document`)
  return found
}

const all = (d) => copied(new AllSelection(d))
/** From the start of `from` to the end of the first `to` after it. */
function between(d, from, to) {
  const start = at(d, from)
  return copied(TextSelection.create(d, start, at(d, to, start) + to.length))
}

// ── Whole documents ───────────────────────────────────────────────────
//
// The cases `richtext.rs` checks its export against: a note copied out of
// the editor and the same note exported are the same Markdown. Built through
// the schema here, so they carry every default attribute a real document
// has -- a link's `target` and `rel`, a list's `type` -- where the Rust test
// reads them as written.

const fixture = JSON.parse(
  readFileSync(new URL('../../crates/everyday-core/tests/fixtures/markdown.json', import.meta.url)),
)
for (const { name, doc: content, markdown } of fixture.cases) {
  assert.equal(all(schema.nodeFromJSON({ type: 'doc', content })), markdown, name)
}

// ── Selections ────────────────────────────────────────────────────────

{
  const d = doc(
    h(2, 'Plans for the week'),
    p('Some ', text('bold', 'bold'), ' words.'),
    { type: 'bulletList', content: [li(p('first item')), li(p('second item'))] },
    { type: 'taskList', content: [task(true, p('done thing')), task(false, p('open thing'))] },
    { type: 'codeBlock', content: [text('let x = 1\nlet y = 2')] },
  )

  assert.equal(between(d, 'for', 'week'), 'for the week', 'a word out of a heading has no `##`')
  assert.equal(
    between(d, 'Some', 'words.'),
    'Some **bold** words.',
    'text inside one paragraph keeps its marks',
  )
  assert.equal(
    between(d, 'second', 'item'),
    'second item',
    'a word out of a list item is the word, not a one-item list',
  )
  assert.equal(
    between(d, 'x = 1', 'let y'),
    'x = 1\nlet y',
    'a line out of a code block is the code, with no fence round it',
  )
  assert.equal(
    between(d, 'week', 'Some'),
    '## week\n\nSome',
    'across blocks, each is written as the block it is',
  )
  assert.equal(
    between(d, 'item', 'second'),
    '- item\n- second',
    'across list items, the list is kept',
  )
  assert.equal(
    between(d, 'thing', 'open'),
    '- [x] thing\n- [ ] open',
    'and so are a checklist’s boxes',
  )
}

{
  const d = doc({
    type: 'table',
    content: [
      row(cell('Day'), cell('Miles'), cell('Note')),
      row(cell('Mon'), cell('3'), cell('easy')),
    ],
  })
  const cellAt = (needle) => d.resolve(at(d, needle)).before(-1)
  assert.equal(
    copied(CellSelection.create(d, cellAt('Day'), cellAt('3'))),
    '| Day | Miles |\n| --- | --- |\n| Mon | 3 |',
    'cells copied without their table are still a table',
  )
  assert.equal(between(d, 'Mi', 'les'), 'Miles', 'and text inside one cell is the text')
}

await close()
console.log('markdown-copy: all checks passed')
