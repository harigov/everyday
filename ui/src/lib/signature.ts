// A draft's signature as one block in the compose sheet's editor.
//
// It has to survive being edited as what it is. `new_draft` writes it as
// `<div data-signature="">` (`compose::signature_block` in
// `karmayoga-core/src/mail/compose.rs`), and an editor that did not know that
// shape would flatten it into paragraphs like any others -- and then Improve
// would rewrite somebody's job title, and "Write with AI" would replace it,
// because nothing could say where their words stopped. It stays perfectly
// ordinary text otherwise: click into it and type, or select it and delete it.

import { Extension, Node, mergeAttributes } from '@tiptap/core'
import type { Node as ProseMirrorNode } from '@tiptap/pm/model'

export const Signature = Node.create({
  name: 'signature',
  group: 'block',
  content: 'block+',
  defining: true,
  parseHTML() {
    return [{ tag: 'div[data-signature]' }]
  },
  renderHTML({ HTMLAttributes }) {
    return ['div', mergeAttributes(HTMLAttributes, { 'data-signature': '' }), 0]
  },
})

/** Where the signature sits in a document, by the positions of its opening and
 *  closing, when it has one. */
export function signatureRange(doc: ProseMirrorNode): { from: number; to: number } | null {
  let range: { from: number; to: number } | null = null
  doc.forEach((node, offset) => {
    if (node.type.name === 'signature') range = { from: offset, to: offset + node.nodeSize }
  })
  return range
}

/**
 * Enter as a line break rather than a new paragraph, for writing a signature:
 * a mail client draws a gap between paragraphs, and a name, a title and a phone
 * number with a blank line between each do not read as one block.
 */
export const LineBreakOnEnter = Extension.create({
  name: 'lineBreakOnEnter',
  addKeyboardShortcuts() {
    return { Enter: () => this.editor.commands.setHardBreak() }
  },
})
