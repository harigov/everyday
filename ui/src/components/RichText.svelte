<script lang="ts">
  // The rich text canvas: a toolbar, a ProseMirror document, and the media
  // pipeline that puts a dropped photograph into it.
  //
  // Lifted out of `Editor.svelte` when notes arrived, and lifted rather than
  // copied for the obvious reason: a second editor would be a second place
  // for the caret bugs to be fixed. What stayed behind in each caller is the
  // page around this -- a journal entry has a date, a tracker strip and a
  // Delete button; a note has a tag row and nothing else -- and what came
  // here is everything that is about editing text.
  //
  // The two effects below carry most of the scar tissue. Read their comments
  // before touching either: one is why the pane comes back with a working
  // caret after being unmounted, and the other is why typing does not throw
  // the caret away on every autosave.
  import { onDestroy, untrack } from 'svelte'
  import { Editor } from '@tiptap/core'
  import StarterKit from '@tiptap/starter-kit'
  import Placeholder from '@tiptap/extension-placeholder'
  import Link from '@tiptap/extension-link'
  import Underline from '@tiptap/extension-underline'
  import Highlight from '@tiptap/extension-highlight'
  import Typography from '@tiptap/extension-typography'
  import TaskList from '@tiptap/extension-task-list'
  import TaskItem from '@tiptap/extension-task-item'
  import CharacterCount from '@tiptap/extension-character-count'
  import { Media } from '../lib/media-node'
  import { app } from '../lib/state.svelte'
  import { api } from '../lib/api'
  import Toolbar from './Toolbar.svelte'
  import type { Attachment, MediaKind, RichDoc } from '../lib/types'

  interface Props {
    /**
     * Which record the document belongs to.
     *
     * The document is swapped when *this* changes and at no other time. See
     * the second effect below for what depending on the record itself cost.
     */
    docId: string | null
    /** The document to draw. Read untracked, on a swap. */
    doc: () => RichDoc | undefined
    placeholder?: string
    /**
     * Register a getter for the live document, or `null` on teardown.
     *
     * Pulling the body out of ProseMirror once per save rather than once per
     * keystroke is most of the typing latency on a long document.
     */
    bindBody: (get: (() => RichDoc) | null) => void
    /** Capture the live document into the record. Called before teardown. */
    syncBody: () => void
    /** Something was typed. The caller schedules its own save. */
    onedit: () => void
    /**
     * A file was stored and inserted, and the record has to remember it.
     *
     * The caller appends to its own `attachments` and saves; this does not
     * reach into the record, because it does not know what kind of record it
     * is drawing.
     */
    onattach: (attachment: Attachment) => void
    /** Words in the document, on a lull rather than on every keypress. */
    onwords?: (words: number) => void
    /** The file currently being written to the vault, for a status bar. */
    onstoring?: (filename: string | null) => void
  }

  let {
    docId,
    doc,
    placeholder = 'Write…',
    bindBody,
    syncBody,
    onedit,
    onattach,
    onwords,
    onstoring,
  }: Props = $props()

  let host = $state<HTMLDivElement>()
  let editor = $state<Editor | null>(null)
  /** Guards against the programmatic `setContent` echoing back as an edit. */
  let loading = false
  /** Which record the ProseMirror document currently holds. */
  let loadedId: string | null = null
  let wordTimer: ReturnType<typeof setTimeout> | null = null

  export function insertDroppedFiles(files: File[]): boolean {
    return insertFiles(files)
  }
  function build(el: HTMLDivElement) {
    const ed = new Editor({
      element: el,
      extensions: [
        StarterKit.configure({
          heading: { levels: [1, 2, 3] },
          // Supplied separately below so they can be configured.
          link: false,
          underline: false,
        }),
        Placeholder.configure({
          placeholder: ({ node }) => (node.type.name === 'heading' ? 'Heading' : placeholder),
        }),
        Link.configure({
          openOnClick: true,
          autolink: true,
          protocols: ['http', 'https', 'mailto'],
        }),
        Underline,
        Highlight,
        Typography,
        TaskList,
        TaskItem.configure({ nested: true }),
        CharacterCount,
        Media,
      ],
      editorProps: {
        attributes: { class: 'ed-content', spellcheck: 'true' },
        handlePaste: (_view, event) => insertFiles(Array.from(event.clipboardData?.files ?? [])),
        handleDrop: (_view, event) => {
          const dt = event.dataTransfer
          return insertFiles(Array.from(dt?.files ?? []))
        },
      },
      // Deliberately does not serialise the document into app state. The
      // body is pulled from `getJSON()` once per save (see `bindBody`); doing
      // it per keystroke walked the whole document every character and was
      // most of the typing latency on a long entry.
      onUpdate: () => {
        if (loading || docId === null) return
        onedit()
        app.touch()
        scheduleWordCount()
      },
    })
    return ed
  }

  /** Word counting walks the whole document, so it runs on a lull, not on
   *  every keypress. The reader cannot follow a counter faster than this. */
  function scheduleWordCount() {
    if (wordTimer) return
    wordTimer = setTimeout(() => {
      wordTimer = null
      onwords?.(editor?.storage.characterCount.words() ?? 0)
    }, 400)
  }

  /**
   * Store dropped or pasted files as blobs and insert media nodes.
   *
   * Two things here are about the gap between dropping a file and seeing it.
   * A phone video is hundreds of megabytes, so reading it, sealing it and
   * writing it out is seconds of work with nothing on screen -- hence
   * `onstoring`, which puts the filename in the status bar for as long as it
   * takes rather than leaving a drop that appears to have done nothing.
   *
   * And the record open when the drop happened is remembered, because it may
   * not be the record open when the write finishes. Switching entries during
   * an upload used to file the attachment against whichever entry had since
   * been opened and insert the video into *its* document -- so a clip landed
   * in a day it had nothing to do with, and the entry it was dropped on had
   * no record of it at all.
   */
  function insertFiles(files: File[]): boolean {
    const target = docId
    if (!files.length || !editor || target === null) return false
    const ed = editor
    void (async () => {
      for (const file of files) {
        try {
          onstoring?.(file.name)
          const bytes = new Uint8Array(await file.arrayBuffer())
          const blob = await api.putBlob(bytes)
          const kind: MediaKind = file.type.startsWith('image/')
            ? 'image'
            : file.type.startsWith('video/')
              ? 'video'
              : file.type.startsWith('audio/')
                ? 'audio'
                : 'file'
          const size = kind === 'image' ? await imageSize(file) : null
          const media = {
            blob,
            kind,
            mime: file.type || 'application/octet-stream',
            filename: file.name,
            caption: '',
            width: size?.width ?? null,
            height: size?.height ?? null,
          }
          // The bytes are in the vault either way -- the garbage collector
          // reclaims a blob nothing references -- so a drop whose record has
          // been navigated away from is dropped quietly rather than being
          // put somewhere it does not belong.
          if (docId !== target || ed !== editor) continue
          ed.chain().focus().insertMedia(media).run()
          onattach({
            ...media,
            byteLen: file.size,
            width: size?.width,
            height: size?.height,
          })
        } catch (e) {
          app.error = e instanceof Error ? e.message : String(e)
        } finally {
          onstoring?.(null)
        }
      }
    })()
    return true
  }

  /** Read intrinsic dimensions so the layout can reserve space up front. */
  function imageSize(file: File): Promise<{ width: number; height: number } | null> {
    return new Promise((resolve) => {
      const url = URL.createObjectURL(file)
      const img = new Image()
      img.onload = () => {
        resolve({ width: img.naturalWidth, height: img.naturalHeight })
        URL.revokeObjectURL(url)
      }
      img.onerror = () => {
        resolve(null)
        URL.revokeObjectURL(url)
      }
      img.src = url
    })
  }

  // Own the editor's lifetime, keyed on the element it lives in.
  //
  // A caller unmounts this whole pane whenever nothing is open -- switching
  // to an empty journal, locking, deleting the last record. The old guard was
  // `if (host && !editor)`, so after such a round trip `host` was a brand new
  // <div> but `editor` still held a ProseMirror view attached to the
  // discarded one. Nothing rebuilt, and the pane came back with no editable
  // element in it at all: no caret, no typing, no way to tell anything was
  // wrong. Rebuilding when `host` changes is the fix, and the teardown stops
  // the old view leaking with it.
  $effect(() => {
    const el = host
    if (!el) return
    const ed = build(el)
    editor = ed
    loadedId = null
    bindBody(() => ed.getJSON() as RichDoc)
    return () => {
      // Capture the document while the view is still alive. If the pane is
      // going away because nothing is open, `syncBody` is a no-op; it can
      // never write this document into a different record, because switching
      // records does not unmount the pane.
      syncBody()
      bindBody(null)
      ed.destroy()
      if (editor === ed) editor = null
    }
  })

  // Swap the document when the open record changes -- and *only* then.
  //
  // This used to depend on the whole record object, which meant every write
  // to any of its fields re-ran it: the autosave stamping `updatedAt`, a
  // keystroke in the title, adding a tag. Each re-run replaced the entire
  // ProseMirror document, which threw away the caret and fired transactions
  // from inside an effect -- and that last part crashed Svelte with
  // `effect_update_depth_exceeded`, after which every button in the app
  // stopped responding. Reading only the id is what keeps it to real swaps.
  $effect(() => {
    const id = docId
    if (!editor) return
    if (id === null) {
      loadedId = null
      return
    }
    if (id === loadedId) return
    loadedId = id
    // Untracked: reading the document must not subscribe this effect to
    // every node in it.
    const body = untrack(() => doc())
    loading = true
    editor.commands.setContent(body as never, { emitUpdate: false })
    onwords?.(editor.storage.characterCount.words())
    loading = false
  })

  onDestroy(() => {
    if (wordTimer) clearTimeout(wordTimer)
    // The effect teardown above captures the document; the caller's own
    // `onDestroy` makes sure it reaches disk.
    syncBody()
  })
</script>

<Toolbar {editor} />

<div class="prose" bind:this={host}></div>

<style>
  .prose :global(.ed-content) {
    font-family: var(--font-read);
    font-size: var(--text-prose);
    line-height: var(--leading-prose);
    color: var(--fg);
    user-select: text;
    /* Proportional, but lining: old-style figures are a serif mannerism and
       look like a mistake in a sans. */
    font-variant-numeric: proportional-nums lining-nums;
    outline: none;
    -webkit-user-modify: read-write-plaintext-only;
  }
  /* The gap between blocks, in ems of the prose size, so it tracks the
     line height rather than being a fixed number of pixels that stops
     looking like a paragraph break when the ratio changes. A shade over one
     line: enough that a new paragraph is seen before it is read, and not so
     much that a page of short ones reads as a list. */
  .prose :global(.ed-content > * + *) {
    margin-top: 1.15em;
  }

  .prose :global(h1),
  .prose :global(h2),
  .prose :global(h3) {
    font-weight: 650;
    line-height: var(--leading-tight);
    letter-spacing: -0.012em;
    margin-top: 1.7em;
  }
  .prose :global(h1) {
    font-size: 1.5em;
  }
  .prose :global(h2) {
    font-size: 1.28em;
  }
  .prose :global(h3) {
    font-size: 1.1em;
  }

  .prose :global(strong) {
    font-weight: 700;
  }
  .prose :global(em) {
    font-style: italic;
  }
  .prose :global(mark) {
    background: color-mix(in oklab, #f5d90a 42%, transparent);
    color: inherit;
    border-radius: 2px;
    padding: 0 2px;
  }
  .prose :global(a) {
    color: var(--journal-accent, var(--accent));
  }

  .prose :global(blockquote) {
    margin-left: 0;
    padding-left: 1.1em;
    border-left: 2px solid var(--journal-accent, var(--accent));
    color: var(--fg-muted);
    font-style: italic;
  }

  .prose :global(code) {
    font-family: var(--font-mono);
    font-size: 0.86em;
    background: var(--bg-sunken);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.1em 0.35em;
    font-variant-numeric: lining-nums;
  }
  .prose :global(pre) {
    background: var(--bg-sunken);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: var(--sp-3) var(--sp-4);
    overflow-x: auto;
    font-size: var(--text-base);
    line-height: var(--leading-normal);
  }
  .prose :global(pre code) {
    background: none;
    border: none;
    padding: 0;
    font-size: inherit;
  }

  .prose :global(ul),
  .prose :global(ol) {
    padding-left: 1.3em;
  }
  .prose :global(li + li) {
    margin-top: 0.35em;
  }
  .prose :global(li p) {
    margin: 0;
  }

  .prose :global(ul[data-type='taskList']) {
    list-style: none;
    padding-left: 0;
  }
  .prose :global(ul[data-type='taskList'] li) {
    display: flex;
    gap: 0.6em;
    align-items: flex-start;
  }
  .prose :global(ul[data-type='taskList'] input) {
    margin-top: 0.45em;
    accent-color: var(--journal-accent, var(--accent));
  }

  .prose :global(hr) {
    border: none;
    border-top: 1px solid var(--border);
    margin: 2em 0;
  }

  /* Placeholder for the first empty block. */
  .prose :global(p.is-editor-empty:first-child::before) {
    content: attr(data-placeholder);
    float: left;
    height: 0;
    pointer-events: none;
    color: var(--fg-faint);
  }

  /* ── Embedded media ───────────────────────────────────────────────── */

  .prose :global(.ed-media) {
    margin: 1.6em 0;
  }
  .prose :global(.ed-media-frame) {
    overflow: hidden;
    border-radius: var(--radius-lg);
    background: var(--bg-sunken);
    box-shadow: var(--shadow-sm);
    line-height: 0;
  }
  .prose :global(.ed-media img),
  .prose :global(.ed-media video) {
    width: 100%;
    height: auto;
    display: block;
  }
  .prose :global(.ed-media audio) {
    width: 100%;
    display: block;
    line-height: normal;
  }
  .prose :global(.ed-file) {
    display: block;
    padding: var(--sp-4);
    font-family: var(--font-ui);
    font-size: var(--text-base);
    line-height: normal;
  }
  .prose :global(.ed-caption) {
    margin-top: 0.55em;
    font-family: var(--font-ui);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    text-align: center;
    outline: none;
  }
  .prose :global(.ed-caption:empty::before) {
    content: attr(data-placeholder);
    color: var(--fg-faint);
  }
  .prose :global(.ed-media.ProseMirror-selectednode .ed-media-frame) {
    outline: 2px solid var(--journal-accent, var(--accent));
    outline-offset: 2px;
  }
</style>
