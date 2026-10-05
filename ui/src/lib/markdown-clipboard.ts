// Markdown on the clipboard, both ways, for every editor that has text in it.
//
// The journal and notes draw `RichText.svelte`; mail compose builds its own
// small editor (see the top of `MailCompose.svelte` for why). All three want
// the same clipboard: a copy leaves Markdown as its plain text
// (`markdown-copy.ts`), and a paste of Markdown arrives formatted
// (`markdown-paste.ts`). An extension is what lets the three share that
// rather than each keeping a copy of the wiring that drifts.
//
// What differs between them is what their schemas can hold. Mail has no
// headings and no checklists -- a sent message has nowhere to keep either --
// so a heading pasted into it becomes a bold line and a ticked box becomes
// `[x]`, whether it came as Markdown or as HTML copied out of a note.

import { Extension } from '@tiptap/core'
import { Plugin, PluginKey } from '@tiptap/pm/state'
import { markdownToPaste } from './markdown-paste'
import { sliceToMarkdown, type PMNode } from './markdown-copy'

export interface MarkdownClipboardOptions {
  /** Whether the editor has checklists. Without them, a box stays `[x]` text. */
  taskLists: boolean
  /** The deepest heading the editor has; 0 for none, when a heading is a bold line. */
  headings: number
}

export const MarkdownClipboard = Extension.create<MarkdownClipboardOptions>({
  name: 'markdownClipboard',
  // Ahead of TipTap's own plugins: its core installs a plain-text
  // serializer of its own, and the first one asked is the one that answers.
  priority: 1000,

  addOptions() {
    return { taskLists: true, headings: 6 }
  },

  addProseMirrorPlugins() {
    const { taskLists, headings } = this.options
    /** Whether the paste on its way was asked for as plain text. */
    let plainPaste = false

    return [
      new Plugin({
        key: new PluginKey('markdownClipboard'),
        props: {
          // Ctrl+Shift+V (Cmd on a Mac) asks for the text as typed. Re-read
          // on every key, so a Shift+V typed into a word earlier cannot turn
          // a later paste from the Edit menu into a plain one.
          handleKeyDown: (_view, event) => {
            plainPaste =
              event.shiftKey && (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'v'
            return false
          },
          handleDOMEvents: {
            // Ahead of ProseMirror's own paste rather than inside
            // `handlePaste`, which runs after the clipboard has already been
            // parsed -- a parse that would be thrown away.
            paste: (view, event) => {
              const plain = plainPaste
              plainPaste = false
              const data = event.clipboardData
              // Files are the editor's own business. Inside a code block
              // Markdown is what is being written, not formatted.
              if (!data || plain || data.files.length > 0) return false
              if (view.state.selection.$from.parent.type.spec.code) return false
              const html = markdownToPaste(data.getData('text/plain'), data.getData('text/html'), {
                taskLists,
                maxHeading: headings,
              })
              if (html === null) return false
              event.preventDefault()
              // Through ProseMirror's own paste, so a table lands the way a
              // pasted `<table>` would -- fitted around the caret -- and the
              // paste is one step to undo.
              view.pasteHTML(html)
              return true
            },
          },
          transformPastedHTML: (html) => fitToSchema(html, taskLists, headings),
          // The plain text that a copy, a cut or a drag puts beside the HTML.
          // ProseMirror's own is the bare characters, with every heading,
          // mark and checkbox gone.
          clipboardTextSerializer: (slice) => {
            const content = (slice.content.toJSON() as PMNode[] | null) ?? []
            return sliceToMarkdown(content, slice.openStart, slice.openEnd)
          },
        },
      }),
    ]
  },
})

/**
 * Pasted HTML, with what the schema cannot hold made into what it can.
 *
 * Left to ProseMirror, a heading deeper than the editor has arrives as a
 * plain paragraph, and a checklist item in an editor without checklists
 * loses its tick. Both would happen silently, so both are rewritten first:
 * the heading to the deepest level there is (or a bold line, where there is
 * none), the box to the `[x]` it is written as in Markdown.
 */
function fitToSchema(html: string, taskLists: boolean, headings: number): string {
  const deep = headings < 6 && /<h[1-6][\s>]/i.test(html)
  const boxes = !taskLists && html.includes('taskItem')
  if (!deep && !boxes) return html

  const dom = new DOMParser().parseFromString(html, 'text/html')
  for (const heading of dom.body.querySelectorAll('h1, h2, h3, h4, h5, h6')) {
    if (Number(heading.tagName[1]) <= headings) continue
    const fitted = dom.createElement(headings ? `h${headings}` : 'p')
    for (const { name, value } of heading.attributes) fitted.setAttribute(name, value)
    const body = headings ? fitted : fitted.appendChild(dom.createElement('strong'))
    body.append(...heading.childNodes)
    heading.replaceWith(fitted)
  }
  if (!taskLists) {
    for (const item of dom.body.querySelectorAll<HTMLElement>('li[data-type="taskItem"]')) {
      item.querySelector(':scope > label')?.remove()
      ;(item.querySelector('p') ?? item).prepend(item.dataset.checked === 'true' ? '[x] ' : '[ ] ')
    }
  }
  return dom.body.innerHTML
}
