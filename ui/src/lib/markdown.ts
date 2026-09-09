// Markdown, for the one place in the application that receives it.
//
// The assistant answers in Markdown -- every model does, whatever it is
// asked -- and the panel was drawing it as `white-space: pre-wrap`. So a
// reply came back reading `**Three things:**` with literal asterisks, its
// numbered list run together, and a snippet of a shell command sitting in
// the same face and colour as the sentence around it. The content was right
// and it was unreadable, which for an answer is the same as being wrong.
//
// # Why this rather than a library
//
// Because of what the input is. This text arrives from a model over the
// network, it is put into the document with `{@html}`, and the vault it is
// describing is the thing being protected. A renderer is therefore a piece
// of security-relevant code, and the property that matters is not "handles
// every corner of CommonMark" but **no path from the input to executable
// markup**. That property is provable here in a way it is not for a
// dependency: the source text is HTML-escaped *first*, before a single
// pattern runs, so no `<` survives to open a tag. Every tag in the output is
// written by this file. There is no sanitiser to keep in step with a parser,
// because there is nothing to sanitise.
//
// The rest of the argument is ordinary: this is a few hundred lines against
// a transitive dependency tree, in an application whose whole premise is
// that your journal never leaves the machine.
//
// # What it covers
//
// What a chat reply actually contains: headings, emphasis, inline code,
// fenced and indented code, ordered and unordered lists (nested), block
// quotes, rules, tables, links and bare URLs. What it deliberately does not:
// raw HTML (escaped, on purpose), reference links, footnotes, and setext
// headings. Anything unrecognised comes out as the text that was typed,
// which is the correct failure for a renderer nobody can debug from the
// other end of an API.
//
// # Streaming
//
// The panel calls this on every chunk of a reply as it arrives, so the input
// is usually a *truncated* document: a fence that has not closed, a list
// mid-item, half a link. Every block below therefore terminates at
// end-of-input rather than requiring its closing marker, and the output is
// always well-formed HTML even when the Markdown is not.

/** Turn Markdown into HTML that is safe to put in the document. */
export function renderMarkdown(source: string): string {
  return blocks(prepare(source))
}

/**
 * The escaping the safety of the whole module rests on.
 *
 * Runs before anything else looks at the text, so no pattern below can ever
 * be handed a `<`. `&` goes first or it would double-escape the entities the
 * later replacements introduce.
 */
export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;')
}

/** Normalise line endings, tabs and the sentinel this module reserves. */
function prepare(source: string): string[] {
  return (
    source
      .replace(/\r\n?/g, '\n')
      // Inline code is lifted out into `\u0000n\u0000` slots while emphasis
      // runs, so a NUL arriving in the text would be able to point at a slot
      // it does not own. There is no legitimate NUL in a chat reply.
      //
      // The rule below is right in general -- a control character in a
      // pattern is nearly always a typo -- and this is the reserved
      // sentinel, being stripped from the input so that it cannot be one.
      // eslint-disable-next-line no-control-regex
      .replace(/\u0000/g, '')
      .replace(/\t/g, '    ')
      .split('\n')
  )
}

// ── Block level ──────────────────────────────────────────────────────────

const FENCE = /^ {0,3}(`{3,}|~{3,})\s*([^`]*)$/
const HEADING = /^ {0,3}(#{1,6})\s+(.*?)\s*#*\s*$/
const RULE = /^ {0,3}([-*_])(?:\s*\1){2,}\s*$/
const QUOTE = /^ {0,3}> ?(.*)$/
const BULLET = /^(\s*)([-*+])\s+(.*)$/
const NUMBER = /^(\s*)(\d{1,9})[.)]\s+(.*)$/
const TABLE_RULE = /^ {0,3}\|?\s*:?-{1,}:?\s*(\|\s*:?-{1,}:?\s*)*\|?\s*$/

/**
 * Walk the lines, emitting one block at a time.
 *
 * A plain index-and-`while` loop rather than a recursive-descent parser,
 * because every construct here is decided by the first line of it. The one
 * thing that nests is a list, and that recurses through `listBlock`.
 */
function blocks(lines: string[]): string {
  const out: string[] = []
  let i = 0

  while (i < lines.length) {
    const line = lines[i]!

    if (!line.trim()) {
      i++
      continue
    }

    const fence = FENCE.exec(line)
    if (fence) {
      const marker = fence[1]![0]!
      const body: string[] = []
      i++
      // An unterminated fence runs to the end of the input rather than
      // being abandoned: mid-stream, that is every fence.
      while (i < lines.length && !new RegExp(`^ {0,3}${marker}{3,}\\s*$`).test(lines[i]!)) {
        body.push(lines[i]!)
        i++
      }
      if (i < lines.length) i++
      const language = fence[2]!.trim().split(/\s+/)[0] ?? ''
      const attribute = language ? ` class="language-${escapeHtml(language)}"` : ''
      out.push(`<pre><code${attribute}>${escapeHtml(body.join('\n'))}</code></pre>`)
      continue
    }

    const heading = HEADING.exec(line)
    if (heading) {
      const level = heading[1]!.length
      out.push(`<h${level}>${inline(heading[2]!)}</h${level}>`)
      i++
      continue
    }

    if (RULE.test(line)) {
      out.push('<hr>')
      i++
      continue
    }

    if (QUOTE.test(line)) {
      const body: string[] = []
      while (i < lines.length && (QUOTE.test(lines[i]!) || bodyOfQuoteContinues(lines, i))) {
        const match = QUOTE.exec(lines[i]!)
        body.push(match ? match[1]! : lines[i]!)
        i++
      }
      out.push(`<blockquote>${blocks(body)}</blockquote>`)
      continue
    }

    if (BULLET.test(line) || NUMBER.test(line)) {
      const [html, next] = listBlock(lines, i)
      out.push(html)
      i = next
      continue
    }

    // Four spaces of indent is a code block, but only where a list is not
    // already the reason for the indent -- which the branch above has
    // already taken.
    if (/^ {4}\S/.test(line)) {
      const body: string[] = []
      while (i < lines.length && (/^ {4}/.test(lines[i]!) || !lines[i]!.trim())) {
        body.push(lines[i]!.slice(4))
        i++
      }
      while (body.length && !body[body.length - 1]!.trim()) body.pop()
      out.push(`<pre><code>${escapeHtml(body.join('\n'))}</code></pre>`)
      continue
    }

    if (i + 1 < lines.length && line.includes('|') && TABLE_RULE.test(lines[i + 1]!)) {
      const [html, next] = tableBlock(lines, i)
      out.push(html)
      i = next
      continue
    }

    // Everything else is a paragraph, running to the next blank line or the
    // start of a block that outranks it.
    const body: string[] = []
    while (i < lines.length && lines[i]!.trim() && !startsBlock(lines, i)) {
      body.push(lines[i]!.trim())
      i++
    }
    out.push(`<p>${inline(body.join('\n'))}</p>`)
  }

  return out.join('')
}

/** Would this line begin a block that interrupts a paragraph? */
function startsBlock(lines: string[], at: number): boolean {
  const line = lines[at]!
  return (
    FENCE.test(line) ||
    HEADING.test(line) ||
    RULE.test(line) ||
    QUOTE.test(line) ||
    BULLET.test(line) ||
    NUMBER.test(line) ||
    (at + 1 < lines.length && line.includes('|') && TABLE_RULE.test(lines[at + 1]!))
  )
}

/**
 * A quote's lazy continuation: an unmarked line directly under a quoted one.
 *
 * Markdown allows the `>` to be dropped after the first line of a paragraph
 * inside a quote, and models drop it constantly.
 */
function bodyOfQuoteContinues(lines: string[], at: number): boolean {
  if (at === 0 || !lines[at]!.trim()) return false
  return QUOTE.test(lines[at - 1]!) && !startsBlock(lines, at)
}

interface Item {
  content: string[]
  indent: number
}

/**
 * One list, with whatever nests inside it.
 *
 * Returns the HTML and the line to carry on from. Nesting is by indent: a
 * line indented past the item it follows belongs to that item, and is handed
 * back to `blocks` with the indent removed -- which is what makes a nested
 * list, a paragraph or a fenced block inside an item all work without this
 * function knowing about any of them.
 */
function listBlock(lines: string[], start: number): [string, number] {
  const first = BULLET.exec(lines[start]!) ?? NUMBER.exec(lines[start]!)!
  const ordered = !BULLET.test(lines[start]!)
  const baseIndent = first[1]!.length
  const items: Item[] = []
  let i = start
  let loose = false
  let blanks = 0

  while (i < lines.length) {
    const line = lines[i]!
    if (!line.trim()) {
      blanks++
      i++
      continue
    }
    const bullet = BULLET.exec(line)
    const numbered = NUMBER.exec(line)
    const match = bullet ?? numbered
    const indent = match ? match[1]!.length : leadingSpaces(line)

    if (match && indent <= baseIndent) {
      // A different marker at the same level starts a different list.
      if (indent === baseIndent && ordered === !!bullet) break
      if (items.length && blanks) loose = true
      items.push({ content: [match[3]!], indent: baseIndent })
      blanks = 0
      i++
      continue
    }

    if (!items.length) break
    // Anything indented past the marker continues the item it follows. So
    // does a lazy continuation -- an unindented line right under one -- which
    // is how models write a wrapped bullet.
    if (indent > baseIndent || (!match && !blanks && !startsBlock(lines, i))) {
      if (blanks) {
        loose = true
        items[items.length - 1]!.content.push('')
      }
      items[items.length - 1]!.content.push(line.slice(Math.min(indent, baseIndent + 2)))
      blanks = 0
      i++
      continue
    }
    break
  }

  const rendered = items.map((item) => {
    const inner = blocks(item.content)
    // A tight list is `<li>text</li>`; a loose one keeps its paragraphs,
    // which is what puts the air between items that a blank line asked for.
    // Only the item's *first* paragraph is unwrapped, so a bullet with a
    // nested list under it still reads as one line with a list beneath.
    const lead = loose ? null : /^<p>([^]*?)<\/p>/.exec(inner)
    return `<li>${lead ? lead[1]! + inner.slice(lead[0].length) : inner}</li>`
  })

  const tag = ordered ? 'ol' : 'ul'
  const startAt = ordered && first[2] !== '1' ? ` start="${Number(first[2])}"` : ''
  return [`<${tag}${startAt}>${rendered.join('')}</${tag}>`, i]
}

function leadingSpaces(line: string): number {
  return /^ */.exec(line)![0].length
}

/** A pipe table: a header row, an alignment rule, and the body. */
function tableBlock(lines: string[], start: number): [string, number] {
  const cells = (line: string) =>
    line
      .trim()
      .replace(/^\|/, '')
      .replace(/\|$/, '')
      .split('|')
      .map((cell) => cell.trim())

  const alignments = cells(lines[start + 1]!).map((spec) =>
    spec.startsWith(':') && spec.endsWith(':')
      ? ' style="text-align:center"'
      : spec.endsWith(':')
        ? ' style="text-align:right"'
        : '',
  )
  const head = cells(lines[start]!)
    .map((cell, n) => `<th${alignments[n] ?? ''}>${inline(cell)}</th>`)
    .join('')

  const body: string[] = []
  let i = start + 2
  while (i < lines.length && lines[i]!.trim() && lines[i]!.includes('|')) {
    const row = cells(lines[i]!)
      .map((cell, n) => `<td${alignments[n] ?? ''}>${inline(cell)}</td>`)
      .join('')
    body.push(`<tr>${row}</tr>`)
    i++
  }

  return [`<table><thead><tr>${head}</tr></thead><tbody>${body.join('')}</tbody></table>`, i]
}

// ── Inline level ─────────────────────────────────────────────────────────

/** Schemes a link is allowed to have. Everything else is drawn as text. */
const SAFE_SCHEME = /^(https?:\/\/|mailto:)/i

/**
 * Emphasis, code, links and the rest, in the one order that works.
 *
 * Everything that is *markup* -- a code span's contents, a link's tags -- is
 * lifted out into numbered slots before the emphasis patterns run, and put
 * back after. Two bugs are avoided by that, and both had to be met before it
 * was obvious:
 *
 *   - `**` inside a backtick pair is two asterisks and not a bold marker. No
 *     ordering of the emphasis patterns fixes that; the contents have to be
 *     out of their reach.
 *   - `target="_blank"` on one link and the same on the next is a pair of
 *     underscores with words between them, so the italic pattern matched
 *     across two anchors and wrote an `<em>` into the middle of the markup.
 *
 * A link's *label* stays in the text between its two slots, so emphasis
 * inside it still renders. A bare URL goes into one slot whole, because
 * there is no Markdown inside a URL and plenty of underscores.
 */
function inline(text: string): string {
  const slots: string[] = []
  /** Park a fragment of finished HTML where no later pattern can see it. */
  const park = (html: string) => {
    slots.push(html)
    return `\u0000${slots.length - 1}\u0000`
  }

  let out = escapeHtml(text).replace(/(`+)([^]*?)\1/g, (_all, _ticks: string, body: string) =>
    park(`<code>${body.trim()}</code>`),
  )

  out = out.replace(/\[([^\]]*)\]\(([^\s)]+)(?:\s+&quot;[^)]*&quot;)?\)/g, (all, label, href) => {
    const url = decodeEntities(String(href))
    if (!SAFE_SCHEME.test(url)) return all
    return `${park(anchor(url))}${String(label)}${park('</a>')}`
  })

  // A bare URL. Anything already inside an anchor is parked and therefore
  // invisible here, which is what stops a link being wrapped twice.
  out = out.replace(
    /(^|[\s(])(https?:\/\/[^\s<>&"')\]]+)/g,
    (_all, before: string, url: string) => {
      // Trailing punctuation belongs to the sentence, not to the address.
      const trimmed = url.replace(/[.,;:!?]+$/, '')
      return `${before}${park(`${anchor(trimmed)}${trimmed}</a>`)}${url.slice(trimmed.length)}`
    },
  )

  out = out
    .replace(/(\*\*\*|___)(?=\S)([^]*?\S)\1/g, '<strong><em>$2</em></strong>')
    .replace(/(\*\*|__)(?=\S)([^]*?\S)\1/g, '<strong>$2</strong>')
    .replace(/(?<![\w*])\*(?=\S)([^*\n]*?\S)\*(?![\w*])/g, '<em>$1</em>')
    .replace(/(?<![\w_])_(?=\S)([^_\n]*?\S)_(?![\w_])/g, '<em>$1</em>')
    .replace(/~~(?=\S)([^]*?\S)~~/g, '<del>$1</del>')

  // Two trailing spaces, or a bare newline inside a paragraph: both are a
  // line the writer meant to break. Markdown says only the first is, and
  // nobody typing into a chat box believes that.
  out = out.replace(/ {2,}\n/g, '<br>').replace(/\n/g, '<br>')

  // The other half of the sentinel; see `prepare` for why the rule is off.
  // eslint-disable-next-line no-control-regex
  return out.replace(/\u0000(\d+)\u0000/g, (_all, n: string) => slots[Number(n)] ?? '')
}

/** The opening tag of a link. External by definition -- see `SAFE_SCHEME`. */
function anchor(url: string): string {
  return `<a href="${escapeHtml(url)}" target="_blank" rel="noreferrer noopener">`
}

/** Undo the escaping for a URL, which is compared against a scheme list. */
function decodeEntities(text: string): string {
  return text
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, '&')
}
