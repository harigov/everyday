// The `media` node: images, video, audio and files embedded in an entry.
//
// The node stores only an address. Bytes are fetched from the Rust shell
// over a custom protocol, so a large video streams and seeks rather than
// being inlined into the document as base64 -- which would bloat every save,
// every decrypt and every keystroke's undo step.
//
// There are two kinds of address. `file` names a file in the files catalog
// (`docs/plans/files.md`): the way every picture, film and file is kept in a
// vault that has one, and the file stays where it is -- in the person's own
// folder -- rather than being copied into the document's record. `blob` is a
// content address in the vault's blob store, which is how it was done before
// and how documents already written still refer to theirs. A node has one or
// the other.

import { Node, mergeAttributes } from '@tiptap/core'
import { NodeSelection, TextSelection } from '@tiptap/pm/state'
import { fileUrl, mediaUrl } from './api'
import type { FileId, MediaKind } from './types'

export interface MediaAttrs {
  /** A content address in the vault's blob store; empty for a catalogued file. */
  blob: string
  /** A file in the files catalog; empty for a blob. */
  file: FileId
  kind: MediaKind
  mime: string
  filename: string
  caption: string
  width: number | null
  height: number | null
}

export interface MediaOptions {
  /** A file that is not a picture, film or recording was pressed: show it.
   *  Supplied by the editor, which owns the viewer. */
  openFile: (id: FileId) => void
}

declare module '@tiptap/core' {
  interface Commands<ReturnType> {
    media: {
      insertMedia: (
        attrs: Partial<MediaAttrs> & ({ blob: string } | { file: FileId }),
      ) => ReturnType
    }
  }
}

export const Media = Node.create<MediaOptions>({
  name: 'media',
  group: 'block',
  atom: true,
  draggable: true,
  selectable: true,

  addOptions() {
    return { openFile: () => {} }
  },

  addAttributes() {
    return {
      blob: { default: '' },
      file: { default: '' },
      kind: { default: 'image' as MediaKind },
      mime: { default: '' },
      filename: { default: '' },
      caption: { default: '' },
      width: { default: null },
      height: { default: null },
    }
  },

  parseHTML() {
    return [{ tag: 'figure[data-media]' }]
  },

  renderHTML({ HTMLAttributes }) {
    return ['figure', mergeAttributes({ 'data-media': '' }, HTMLAttributes)]
  },

  addCommands() {
    return {
      // Inserted with the caret left on the line after it, not on the node.
      // ProseMirror selects an atom block it has just put in, and a selected
      // picture is what the next insertion replaces -- so pasting a second
      // screenshot threw the first away, and a drop of several photographs
      // kept only the last.
      insertMedia:
        (attrs) =>
        ({ commands }) =>
          commands.insertContent({ type: this.name, attrs }) &&
          commands.command(({ tr, dispatch }) => {
            const picked = tr.selection
            if (!(picked instanceof NodeSelection) || picked.node.type.name !== this.name) {
              return true
            }
            const after = picked.to
            const next = TextSelection.findFrom(tr.doc.resolve(after), 1, true)
            if (dispatch) {
              if (next) {
                tr.setSelection(next)
              } else {
                // The picture ended the document: give the caret a line to
                // stand on below it.
                tr.insert(after, tr.doc.type.schema.nodes.paragraph!.create())
                tr.setSelection(TextSelection.create(tr.doc, after + 1))
              }
            }
            return true
          }),
    }
  },

  // A custom node view rather than `renderHTML`, so the caption can be an
  // editable field and the aspect ratio can be reserved before the media
  // loads -- without that, inserting a photo shoves the text the writer is
  // looking at down the page.
  addNodeView() {
    return ({ node, editor, getPos }) => {
      const attrs = node.attrs as MediaAttrs
      const figure = document.createElement('figure')
      figure.className = 'ed-media'
      figure.dataset.kind = attrs.kind
      figure.contentEditable = 'false'

      const frame = document.createElement('div')
      frame.className = 'ed-media-frame'
      if (attrs.width && attrs.height) {
        frame.style.aspectRatio = `${attrs.width} / ${attrs.height}`
      }

      // A catalogued file is read where the catalog says it is, with ranges, so
      // a film seeks; a blob is read from the vault's blob store.
      const url = attrs.file && !attrs.blob ? fileUrl(attrs.file) : mediaUrl(attrs.blob)

      // What stands for a file that cannot be drawn in place -- anything that
      // is not a picture, film or recording, or a picture the webview cannot
      // decode (a camera's RAW, say). A catalogued one opens in the viewer,
      // which can show what the file is far better than a link can; an old
      // blob is saved.
      const chip = () => {
        if (attrs.file && !attrs.blob) {
          const button = document.createElement('button')
          button.type = 'button'
          button.className = 'ed-file'
          button.textContent = attrs.filename || 'File'
          button.title = 'Open'
          button.addEventListener('click', () => this.options.openFile(attrs.file))
          return button
        }
        const link = document.createElement('a')
        link.href = url
        link.className = 'ed-file'
        link.textContent = attrs.filename || 'Attachment'
        link.download = attrs.filename
        return link
      }

      if (attrs.kind === 'image') {
        const img = document.createElement('img')
        img.src = url
        img.alt = attrs.caption || attrs.filename
        img.loading = 'lazy'
        img.draggable = false
        if (attrs.file && !attrs.blob) {
          img.addEventListener('error', () => {
            frame.style.aspectRatio = ''
            frame.replaceChildren(chip())
          })
        }
        frame.append(img)
      } else if (attrs.kind === 'video') {
        const video = document.createElement('video')
        video.src = url
        video.controls = true
        video.preload = 'metadata'
        video.playsInline = true
        frame.append(video)
      } else if (attrs.kind === 'audio') {
        const audio = document.createElement('audio')
        audio.src = url
        audio.controls = true
        audio.preload = 'metadata'
        frame.append(audio)
      } else {
        frame.append(chip())
      }

      const caption = document.createElement('figcaption')
      caption.className = 'ed-caption'
      caption.contentEditable = String(editor.isEditable)
      caption.dataset.placeholder = 'Add a caption…'
      caption.textContent = attrs.caption
      caption.addEventListener('blur', () => {
        const pos = typeof getPos === 'function' ? getPos() : null
        if (pos === null || pos === undefined) return
        const text = caption.textContent ?? ''
        if (text === node.attrs.caption) return
        editor.view.dispatch(
          editor.view.state.tr.setNodeMarkup(pos, undefined, {
            ...node.attrs,
            caption: text,
          }),
        )
      })
      // Enter should end the caption, not insert a newline inside it.
      caption.addEventListener('keydown', (e) => {
        if (e.key === 'Enter') {
          e.preventDefault()
          caption.blur()
        }
      })

      figure.append(frame, caption)
      return {
        dom: figure,
        // The caption is the only editable region; let ProseMirror ignore
        // mutations inside the media frame itself.
        ignoreMutation: (m) => !figure.contains(m.target) || m.target === caption,
        update: (updated) => updated.type.name === this.name,
      }
    }
  },
})
