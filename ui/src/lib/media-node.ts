// The `media` node: images, video, audio and files embedded in an entry.
//
// The node stores only a content address. Bytes are fetched from the Rust
// shell over a custom protocol, so a large video streams and seeks rather
// than being inlined into the document as base64 -- which would bloat every
// save, every decrypt and every keystroke's undo step.

import { Node, mergeAttributes } from '@tiptap/core'
import { mediaUrl } from './api'
import type { MediaKind } from './types'

export interface MediaAttrs {
  blob: string
  kind: MediaKind
  mime: string
  filename: string
  caption: string
  width: number | null
  height: number | null
}

declare module '@tiptap/core' {
  interface Commands<ReturnType> {
    media: {
      insertMedia: (attrs: Partial<MediaAttrs> & { blob: string }) => ReturnType
    }
  }
}

export const Media = Node.create({
  name: 'media',
  group: 'block',
  atom: true,
  draggable: true,
  selectable: true,

  addAttributes() {
    return {
      blob: { default: '' },
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
      insertMedia:
        (attrs) =>
        ({ commands }) =>
          commands.insertContent({ type: this.name, attrs }),
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

      const url = mediaUrl(attrs.blob)

      if (attrs.kind === 'image') {
        const img = document.createElement('img')
        img.src = url
        img.alt = attrs.caption || attrs.filename
        img.loading = 'lazy'
        img.draggable = false
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
        const link = document.createElement('a')
        link.href = url
        link.className = 'ed-file'
        link.textContent = attrs.filename || 'Attachment'
        link.download = attrs.filename
        frame.append(link)
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
        if (e.key === 'Enter') { e.preventDefault(); caption.blur() }
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
