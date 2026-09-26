// Markdown, pasted into the editor.
//
// Markdown reaches the clipboard as text: a model's reply copied with its
// Copy button, a README out of a code editor, a table out of a terminal.
// ProseMirror pastes text as text, one paragraph per line, so the only part
// of it that ever came out formatted was what TipTap's inline paste rules
// happen to catch -- `**bold**` did, and a pipe table arrived as a column of
// `| a | b |` lines with the `|---|` rule still in the middle.
//
// So text that has Markdown's block structure in it is rendered to HTML and
// pasted as that. The renderer is `markdown.ts`, the one the assistant panel
// already trusts with text off the network: it escapes before it parses, so
// the HTML handed to the editor holds no tag it did not write.
//
// No DOM in here, so `scripts/markdown-paste.test.mjs` can check the rules.

import {
  BULLET,
  FENCE,
  HEADING,
  NUMBER,
  QUOTE,
  isTableStart,
  renderMarkdown,
  type RenderOptions,
} from './markdown'

/** What the receiving editor can hold, so the HTML is fitted to it. */
export interface PasteOptions extends RenderOptions {
  /**
   * The deepest heading the editor has. A `####` is drawn at this level
   * rather than arriving as a paragraph, which is what a heading level the
   * schema has no rule for would otherwise become.
   */
  maxHeading?: number
}

/**
 * Does this text have Markdown's block structure anywhere in it?
 *
 * The blocks are `markdown.ts`'s own patterns, so what is recognised here is
 * exactly what the renderer will draw. Inline marks are deliberately not on
 * the list -- TipTap's paste rules already handle those, and `*` and `_`
 * turn up in plain prose. A rule is not either: `---` is how a signature
 * starts.
 *
 * A heading counts only with a blank line under it, the way Markdown is
 * written, because `# comment` over a line of code is how a shell script or
 * a Python file is written. And text indented like source code is not read
 * as Markdown at all -- see `looksLikeCode`.
 */
export function looksLikeMarkdown(text: string): boolean {
  const lines = text.replace(/\r\n?/g, '\n').split('\n')
  if (looksLikeCode(lines)) return false
  return lines.some(
    (line, i) =>
      FENCE.test(line) ||
      QUOTE.test(line) ||
      BULLET.test(line) ||
      NUMBER.test(line) ||
      isTableStart(lines, i) ||
      (HEADING.test(line) && !lines[i + 1]?.trim()),
  )
}

/**
 * A line indented by a tab or four spaces before any list has begun: the
 * body of a function, not a paragraph. In Markdown an indent that deep only
 * means something under a list item; outside fences and before any list,
 * it is source, and source pasted into a note should arrive as typed rather
 * than with its `__init__` turned bold.
 */
function looksLikeCode(lines: string[]): boolean {
  let fenced = false
  for (const line of lines) {
    if (FENCE.test(line)) fenced = !fenced
    if (fenced) continue
    if (BULLET.test(line) || NUMBER.test(line)) return false
    if (/^(\t| {4})\s*\S/.test(line)) return true
  }
  return false
}

/**
 * Tags a clipboard's HTML may be made of and still be nothing but text with
 * styling on. A code editor copies Markdown as exactly this -- one styled
 * `<div>` per line of source, colours and all -- so the Markdown reading of
 * the text is the better copy. Anything else in the HTML -- a link, a table,
 * a list, bold -- is something the plain text lost, and the HTML wins.
 */
const PLAIN_TAGS = new Set(['html', 'head', 'body', 'meta', 'style', 'div', 'span', 'p', 'br'])

/** Is this clipboard HTML only styled text, with nothing plain text lacks? */
function htmlIsOnlyText(html: string): boolean {
  // Anything copied out of this editor is already exactly what was selected.
  if (html.includes('data-pm-slice')) return false
  const body = html.replace(/<!--[^]*?-->/g, '').replace(/<style[^>]*>[^]*?<\/style>/gi, '')
  for (const [, tag] of body.matchAll(/<\/?([a-z][\w-]*)/gi)) {
    if (!PLAIN_TAGS.has(tag!.toLowerCase())) return false
  }
  return true
}

/**
 * The HTML to paste in place of what the clipboard holds, or `null` to let
 * the editor paste it as it would have.
 *
 * `html` is the clipboard's `text/html`, empty when there is none.
 */
export function markdownToPaste(
  text: string,
  html: string,
  options: PasteOptions = {},
): string | null {
  if (!text.trim() || !looksLikeMarkdown(text)) return null
  if (html && !htmlIsOnlyText(html)) return null
  const out = renderMarkdown(text, options)
  const max = options.maxHeading
  if (!max) return out
  // The renderer writes its heading tags bare, and escapes every `<` in the
  // text, so nothing but its own headings can match.
  return out.replace(/<(\/?)h([1-6])>/g, (all, close: string, level: string) =>
    Number(level) > max ? `<${close}h${max}>` : all,
  )
}
