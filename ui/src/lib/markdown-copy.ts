// Markdown, copied out of the editor.
//
// A copy puts two things on the clipboard: HTML, which a rich editor pastes
// with its formatting, and plain text for everything else. ProseMirror's
// plain text is the document's characters with a blank line between blocks,
// so a note copied into a chat box, a code editor or a Markdown file arrived
// with its headings, bold, links, checkboxes and tables all gone -- a column
// of unmarked lines that only looked like the note it came from.
//
// So the plain text is written as Markdown -- the same Markdown, character
// for character, that `to_markdown` in `richtext.rs` exports a note as, and
// which the Markdown vault stores. The two are line-for-line mirrors, and
// `crates/everyday-core/tests/fixtures/markdown.json` is checked against
// both. Pasting back into the editor is untouched by any of this: it reads
// the HTML beside the text.
//
// No DOM in here, so `scripts/markdown-copy.test.mjs` can check it.

/** A ProseMirror node as JSON: what `getJSON()` and `Fragment.toJSON()` give. */
export interface PMNode {
  type: string
  attrs?: Record<string, unknown>
  content?: PMNode[]
  marks?: PMMark[]
  text?: string
}

interface PMMark {
  type: string
  attrs?: Record<string, unknown>
}

/** Blocks whose content is text, and a selection inside one copies text. */
const TEXTBLOCKS = new Set(['paragraph', 'heading', 'codeBlock'])
const LISTS = new Set(['bulletList', 'orderedList', 'taskList'])
const INLINE = new Set(['text', 'hardBreak'])

/**
 * Marks written as a pair of delimiters round their text, in the order they
 * are opened when several start together: a link outermost, so its label can
 * carry marks of its own, and bold before italic, so `***` opens as `**` then
 * `*` -- the reading `richtext.rs` parses it back with. A link's closer is
 * its address, written by `closeLink`.
 */
const DELIMITED: [type: string, open: string, close: string][] = [
  ['link', '[', ''],
  ['bold', '**', '**'],
  ['italic', '*', '*'],
  ['strike', '~~', '~~'],
  ['highlight', '==', '=='],
  ['underline', '<u>', '</u>'],
]
const RANK = new Map(DELIMITED.map(([type], rank) => [type, rank]))

const HARD_BREAK = '  \n'

/**
 * What a copied slice of the document reads as in Markdown.
 *
 * `content` is the slice's top-level nodes and `openStart`/`openEnd` how far
 * into them it was cut: ProseMirror's `Slice`, as JSON. A whole document is
 * its `content` with both at zero.
 */
export function sliceToMarkdown(content: PMNode[], openStart: number, openEnd: number): string {
  // A selection is copied with every node above it, down from the document,
  // so a word in a list item arrives as a list holding an item holding a
  // paragraph holding the word. Wrappers that hold nothing else are peeled
  // off, the same way ProseMirror peels them for the HTML.
  while (openStart > 1 && openEnd > 1 && content.length === 1) {
    const inner = content[0]!.content ?? []
    if (inner.length !== 1) break
    content = inner
    openStart--
    openEnd--
  }
  // Inside one block, what was selected is text: a word out of a heading
  // without the `##`, a line of a code block without the fence round it.
  const only = content.length === 1 ? content[0]! : null
  if (only && openStart > 0 && openEnd > 0 && TEXTBLOCKS.has(only.type)) {
    return only.type === 'codeBlock' ? plain(only) : inline(only.content ?? [])
  }
  return blocks(content)
}

// ── Blocks ───────────────────────────────────────────────────────────────

/**
 * Blocks one after another, with a blank line between them.
 *
 * Inside a list item, a list follows the line above it directly: that is
 * what keeps a nested list tight rather than spreading every item apart.
 */
function blocks(nodes: PMNode[], inItem = false): string {
  let out = ''
  for (const node of grouped(nodes)) {
    const md = block(node)
    if (!md.trim()) continue
    if (out) out += inItem && LISTS.has(node.type) ? '\n' : '\n\n'
    out += md
  }
  return out
}

/**
 * Nodes that only make sense inside something, put back inside it.
 *
 * Cells copied out of a table come as bare rows, and a list item selected
 * whole comes as a bare item, so a run of either is drawn as the table or
 * the list it was lifted from.
 */
function grouped(nodes: PMNode[]): PMNode[] {
  const out: PMNode[] = []
  for (let i = 0; i < nodes.length;) {
    const type = nodes[i]!.type
    const wrapper =
      type === 'tableRow'
        ? 'table'
        : type === 'taskItem'
          ? 'taskList'
          : type === 'listItem'
            ? 'bulletList'
            : INLINE.has(type)
              ? 'paragraph'
              : null
    if (!wrapper) {
      out.push(nodes[i++]!)
      continue
    }
    const run: PMNode[] = []
    const same = (n: PMNode) => (wrapper === 'paragraph' ? INLINE.has(n.type) : n.type === type)
    while (i < nodes.length && same(nodes[i]!)) run.push(nodes[i++]!)
    out.push({ type: wrapper, content: run })
  }
  return out
}

function block(node: PMNode): string {
  const children = node.content ?? []
  switch (node.type) {
    case 'paragraph':
      return inline(children).trimEnd()
    case 'heading': {
      const level = Math.min(Math.max(Number(node.attrs?.level) || 1, 1), 6)
      // A heading is one line in Markdown; a break inside it would end it.
      return `${'#'.repeat(level)} ${inline(children).replaceAll(HARD_BREAK, ' ').trimEnd()}`
    }
    case 'blockquote':
      return blocks(children)
        .split('\n')
        .map((line) => (line ? `> ${line}` : '>'))
        .join('\n')
    case 'codeBlock': {
      const body = plain(node)
      const fence = '`'.repeat(Math.max(3, longestRun(body) + 1))
      const language = typeof node.attrs?.language === 'string' ? node.attrs.language : ''
      return body ? `${fence}${language}\n${body}\n${fence}` : `${fence}${language}\n${fence}`
    }
    case 'bulletList':
    case 'orderedList':
    case 'taskList':
      return list(node)
    case 'table':
      return table(node)
    case 'horizontalRule':
      return '---'
    case 'media':
      return media(node)
    default:
      // A block this does not know keeps its text rather than vanishing.
      return blocks(children)
  }
}

function list(node: PMNode): string {
  const ordered = node.type === 'orderedList'
  const start = node.attrs?.start
  const first = Number.isInteger(start) && (start as number) >= 0 ? (start as number) : 1
  return (node.content ?? [])
    .map((item, n) => {
      const marker = ordered ? `${first + n}. ` : '- '
      const checked = item.attrs?.checked
      const box = typeof checked === 'boolean' ? (checked ? '[x] ' : '[ ] ') : ''
      // What follows the first line lines up under the item's text, which
      // is where Markdown looks for what belongs to it. Under the marker,
      // not the box: as far as Markdown knows, the box is part of the text.
      const pad = ' '.repeat(marker.length)
      const [lead = '', ...rest] = blocks(item.content ?? [], true).split('\n')
      return [
        lead ? `${marker}${box}${lead}` : `${marker}${box}`.trimEnd(),
        ...rest.map((line) => (line ? pad + line : '')),
      ].join('\n')
    })
    .join('\n')
}

/** The widest a merged cell is taken to be; `richtext.rs` caps it the same. */
const MAX_SPAN = 1000

/** The rule under a table's header, a column at a time, by its alignment. */
const RULE: Record<string, string> = { left: ' :--- |', center: ' :---: |', right: ' ---: |' }

/**
 * A pipe table, its first row the header.
 *
 * Markdown has no merged cells, so one spanning several keeps its text in
 * the first slot it covers and leaves the rest empty -- as the export does,
 * so every cell after it stays in its own column. And Markdown aligns
 * columns rather than cells, so the first cell in a column with an
 * alignment decides it.
 */
function table(node: PMNode): string {
  const rows = node.content ?? []
  const grid: (string | undefined)[][] = rows.map(() => [])
  const aligns: (string | undefined)[] = []
  rows.forEach((row, r) => {
    let c = 0
    for (const cell of row.content ?? []) {
      while (grid[r]![c] !== undefined) c++
      const span = (key: string) => Math.min(Math.max(Number(cell.attrs?.[key]) || 1, 1), MAX_SPAN)
      const across = span('colspan')
      const align = cell.attrs?.align
      if (aligns[c] === undefined && typeof align === 'string' && align in RULE) aligns[c] = align
      let text: string | undefined = tableCell(cell)
      for (const slots of grid.slice(r, r + span('rowspan'))) {
        for (let k = c; k < c + across; k++) {
          slots[k] = text ?? ''
          text = undefined
        }
      }
      c += across
    }
  })
  const width = Math.max(0, ...grid.map((row) => row.length))
  if (!width) return ''
  const line = (cells: (string | undefined)[]) =>
    `|${Array.from({ length: width }, (_, k) => ` ${cells[k] ?? ''} |`).join('')}`
  const [head = [], ...body] = grid
  const rule = Array.from({ length: width }, (_, k) => RULE[aligns[k] ?? ''] ?? ' --- |')
  return [line(head), `|${rule.join('')}`, ...body.map(line)].join('\n')
}

/** A cell on one line, which is all a row has room for, its pipes escaped. */
function tableCell(cell: PMNode): string {
  return blocks(cell.content ?? [])
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
    .join(' ')
    .replaceAll('|', '\\|')
}

/** A photograph as an image, anything else as a link to the file. */
function media(node: PMNode): string {
  const attr = (key: string) => (typeof node.attrs?.[key] === 'string' ? node.attrs[key] : '')
  const caption = attr('caption')
  const blob = attr('blob')
  if (attr('kind') === 'image') return `![${caption}](media/${blob})`
  return `[${caption || attr('filename') || blob}](media/${blob})`
}

/** A code block's text, as typed. */
function plain(node: PMNode): string {
  return (node.content ?? []).map((child) => child.text ?? '').join('')
}

function longestRun(text: string): number {
  return Math.max(0, ...(text.match(/`+/g) ?? []).map((run) => run.length))
}

// ── Inline ───────────────────────────────────────────────────────────────

/**
 * A paragraph's text with its marks written as delimiters.
 *
 * A mark is opened where it starts and closed where it ends, rather than
 * round each text node: a run of bold with an italic word in the middle is
 * three nodes, and wrapping each in its own marks writes `**a *****b***** c**`.
 */
function inline(nodes: PMNode[]): string {
  let out = ''
  /** The marks open at the end of `out`, outermost first, and where each began. */
  const open: { mark: PMMark; at: number }[] = []

  const closeTo = (depth: number) => {
    if (open.length <= depth) return
    // `**word **` is not bold: a delimiter after a space cannot close. The
    // space goes outside instead, where it reads the same.
    const trailing = /\s*$/.exec(out)![0]
    out = out.slice(0, out.length - trailing.length)
    while (open.length > depth) {
      const { mark, at } = open.pop()!
      out = mark.type === 'link' ? closeLink(out, mark, at) : out + delimiter(mark)[2]
    }
    out += trailing
  }

  for (const node of nodes) {
    // Every mark is closed before a break and opened again after it, so
    // each line reads on its own -- the way `richtext.rs` reads a paragraph
    // back, and most other Markdown readers too.
    if (node.type === 'hardBreak') {
      closeTo(0)
      out += HARD_BREAK
      continue
    }
    if (node.type !== 'text') {
      closeTo(0)
      out += inline(node.content ?? [])
      continue
    }
    const text = node.text ?? ''
    // Whitespace alone neither opens nor closes anything.
    if (!text.trim()) {
      out += text
      continue
    }
    const marks = (node.marks ?? [])
      .filter((m) => RANK.has(m.type))
      .sort((a, b) => RANK.get(a.type)! - RANK.get(b.type)!)
    let keep = 0
    while (keep < open.length && marks.some((m) => sameMark(m, open[keep]!.mark))) keep++
    closeTo(keep)

    const code = node.marks?.some((m) => m.type === 'code')
    const lead = code ? '' : /^\s*/.exec(text)![0]
    out += lead
    for (const mark of marks) {
      if (open.some((o) => sameMark(o.mark, mark))) continue
      open.push({ mark, at: out.length })
      out += delimiter(mark)[1]
    }
    out += code ? codeSpan(text) : text.slice(lead.length)
  }
  closeTo(0)
  // A break at the very end of a paragraph breaks nothing.
  return out.replace(/( {2}\n)+$/, '')
}

function delimiter(mark: PMMark): [string, string, string] {
  return DELIMITED[RANK.get(mark.type)!]!
}

/**
 * `[label](href)`, or `<href>` for an address that is its own label.
 *
 * The address is written with its spaces and parentheses escaped, because
 * either would end it early.
 */
function closeLink(out: string, mark: PMMark, at: number): string {
  const href = typeof mark.attrs?.href === 'string' ? mark.attrs.href : ''
  const label = out.slice(at + 1)
  if (!href) return out.slice(0, at) + label
  if (label === href && isAutolink(href)) return `${out.slice(0, at)}<${href}>`
  const escaped = href.replaceAll(' ', '%20').replaceAll('(', '%28').replaceAll(')', '%29')
  return `${out}](${escaped})`
}

/** Can `href` be written as `<href>` and read back as the same link? */
function isAutolink(href: string): boolean {
  return /^(https?:\/\/|mailto:)/.test(href) && !/[\s<>]/.test(href)
}

/**
 * Fenced with one backtick more than the longest run inside, so none ends
 * it, and padded with a space where Markdown would otherwise take one off or
 * run a backtick at the edge into the fence.
 */
function codeSpan(text: string): string {
  const ticks = '`'.repeat(longestRun(text) + 1)
  const spaced = text.startsWith(' ') && text.endsWith(' ') && text.trim() !== ''
  const pad = text.startsWith('`') || text.endsWith('`') || spaced ? ' ' : ''
  return `${ticks}${pad}${text}${pad}${ticks}`
}

function sameMark(a: PMMark, b: PMMark): boolean {
  return a.type === b.type && JSON.stringify(a.attrs ?? null) === JSON.stringify(b.attrs ?? null)
}
