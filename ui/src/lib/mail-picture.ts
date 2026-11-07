// A picture pasted or dropped into a message, where it was put.
//
// Stored like any attachment -- `api.putBlob`, one of the draft's
// `attachments` -- and drawn from this app's own address for that blob. The
// `data-blob` attribute is what the backend looks for on the way out
// (`compose::inline_pictures` in `everyday-mail`): each such `<img>` is
// pointed at a `cid:` part carrying the same bytes, so the recipient sees the
// picture in the message rather than a file beside it.
//
// Only `img[data-blob]` is read back. Any other `<img>` in pasted HTML is a
// remote address, which this window would refuse to load and which would go
// out pointing at somebody else's server.

import { Node, mergeAttributes } from '@tiptap/core'
import type { Node as ProseMirrorNode } from '@tiptap/pm/model'
import { mediaUrl } from './api'

export interface PictureAttrs {
  blob: string
  mime: string
  filename: string
}

declare module '@tiptap/core' {
  interface Commands<ReturnType> {
    mailPicture: {
      insertPicture: (attrs: PictureAttrs, at?: number) => ReturnType
    }
  }
}

export const MailPicture = Node.create({
  name: 'mailPicture',
  inline: true,
  group: 'inline',
  atom: true,
  draggable: true,

  addAttributes() {
    return {
      blob: { default: '', parseHTML: (el) => el.getAttribute('data-blob') ?? '' },
      mime: { default: '', parseHTML: (el) => el.getAttribute('data-mime') ?? '' },
      filename: { default: '', parseHTML: (el) => el.getAttribute('alt') ?? '' },
    }
  },

  parseHTML() {
    return [{ tag: 'img[data-blob]' }]
  },

  renderHTML({ node }) {
    const { blob, mime, filename } = node.attrs as PictureAttrs
    return [
      'img',
      mergeAttributes({
        src: mediaUrl(blob),
        'data-blob': blob,
        'data-mime': mime,
        alt: filename,
        // Inline, so a mail client that drops a stylesheet still keeps a
        // wide screenshot inside the message.
        style: 'max-width: 100%; height: auto;',
      }),
    ]
  },

  addCommands() {
    return {
      insertPicture:
        (attrs, at) =>
        ({ chain }) => {
          const node = { type: this.name, attrs }
          return at === undefined
            ? chain().insertContent(node).run()
            : chain().insertContentAt(at, node).run()
        },
    }
  },
})

/** The pictures in `doc`, by blob, each once. */
export function picturesIn(doc: ProseMirrorNode): Map<string, PictureAttrs> {
  const found = new Map<string, PictureAttrs>()
  doc.descendants((node) => {
    if (node.type.name === 'mailPicture' && node.attrs.blob) {
      found.set(node.attrs.blob as string, node.attrs as PictureAttrs)
    }
  })
  return found
}
