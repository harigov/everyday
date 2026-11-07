// `- ` and `1. ` starting a list after a line break, as they already do at
// the start of a paragraph.
//
// StarterKit's own list rules only look from the start of the paragraph, so
// a line that begins after a `<br>` -- Shift+Enter, or any of the lines a
// model's draft or a quick reply arrives as (`text_to_html` joins single
// newlines with `<br>`) -- kept its hyphen as a hyphen. In the text an input
// rule reads, a hard break is the `\n` its `renderText` gives -- but a match
// may not span it (TipTap skips any whose text and document lengths
// disagree), so the rule matches the marker alone, after a `\n`, and the
// break before it is taken out by hand.

import { Extension, InputRule, type ChainedCommands, type Range } from '@tiptap/core'
import type { EditorState } from '@tiptap/pm/state'

/** Inside a list already: a marker typed there is the person's own text,
 *  not a second list to start. */
function inList(state: EditorState): boolean {
  const { $from } = state.selection
  for (let depth = $from.depth; depth > 0; depth--) {
    const name = $from.node(depth).type.name
    if (name === 'listItem' || name === 'taskItem') return true
  }
  return false
}

export const ListAfterBreak = Extension.create({
  name: 'listAfterBreak',

  addInputRules() {
    // The break and the marker go, the rest of the line becomes a paragraph
    // of its own, and that paragraph the list's first item.
    const start = (
      state: EditorState,
      range: Range,
      chain: () => ChainedCommands,
      list: 'bulletList' | 'orderedList',
      first = 1,
    ): void | null => {
      if (inList(state)) return null
      const c = chain()
        .deleteRange({ from: range.from - 1, to: range.to })
        .splitBlock()
      if (list === 'bulletList') c.toggleBulletList().run()
      else if (first === 1) c.toggleOrderedList().run()
      else c.toggleOrderedList().updateAttributes('orderedList', { start: first }).run()
    }
    /** The marker `pattern` matches straight after a `\n`, without it. */
    const afterBreak = (pattern: RegExp) => (text: string) => {
      const found = pattern.exec(text)
      if (!found) return null
      return { index: found.index + 1, text: found[0].slice(1), data: { n: found[1] } }
    }
    return [
      new InputRule({
        find: afterBreak(/\n\s*([-+*•])\s$/),
        handler: ({ state, range, chain }) => start(state, range, chain, 'bulletList'),
      }),
      new InputRule({
        find: afterBreak(/\n(\d+)\.\s$/),
        handler: ({ state, range, match, chain }) =>
          start(state, range, chain, 'orderedList', Number(match.data?.n ?? 1)),
      }),
    ]
  },
})
