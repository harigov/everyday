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
  import type { EditorView } from '@tiptap/pm/view'
  import StarterKit from '@tiptap/starter-kit'
  import Placeholder from '@tiptap/extension-placeholder'
  import Link from '@tiptap/extension-link'
  import Underline from '@tiptap/extension-underline'
  import Highlight from '@tiptap/extension-highlight'
  import Typography from '@tiptap/extension-typography'
  import TaskList from '@tiptap/extension-task-list'
  import TaskItem from '@tiptap/extension-task-item'
  import CharacterCount from '@tiptap/extension-character-count'
  import { TableKit } from '@tiptap/extension-table'
  import { clipboardPicture, mayBePicture } from '../lib/clipboard-picture'
  import { Media } from '../lib/media-node'
  import { MarkdownClipboard } from '../lib/markdown-clipboard'
  import { app } from '../lib/state.svelte'
  import { api } from '../lib/api'
  import { lookupFiles, whyNotAdded } from '../lib/file-lookup'
  import { addFiles } from '../lib/files'
  import { notify } from '../lib/notify.svelte'
  import type { Attachment, FileEntry, FileId, MediaKind, RichDoc } from '../lib/types'
  import FileViewer from './FileViewer.svelte'

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
     *
     * Only for a file stored as a blob. One kept in Files is a node that
     * names it, and the record's references to it are worked out from the
     * document whenever it is saved: there is nothing for the caller to add.
     */
    onattach: (attachment: Attachment) => void
    /**
     * Which app's files these are, for Files: where a file dropped here is
     * put, and what it says it came from. The journal's unless told.
     */
    origin?: 'journal' | 'notes'
    /** Words in the document, on a lull rather than on every keypress. */
    onwords?: (words: number) => void
    /** The file currently being written to the vault, for a status bar. */
    onstoring?: (filename: string | null) => void
    /**
     * The live editor, or `null` when there is none.
     *
     * Handed back so the caller can put the toolbar where it belongs. It is
     * *not* rendered here: the toolbar is a fixed bar at the top of the pane,
     * above the scroll, and a copy inside this component would be inside the
     * page column -- scrolling out of view on a long entry and sitting under
     * the date and the title on a short one.
     */
    oneditor?: (editor: Editor | null) => void
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
    oneditor,
    origin = 'journal',
  }: Props = $props()

  let host = $state<HTMLDivElement>()
  let editor = $state<Editor | null>(null)
  /** Guards against the programmatic `setContent` echoing back as an edit. */
  let loading = false
  /** Which record the ProseMirror document currently holds. */
  let loadedId: string | null = null
  let wordTimer: ReturnType<typeof setTimeout> | null = null
  const HEADING_LEVELS: (1 | 2 | 3)[] = [1, 2, 3]

  export function insertDroppedFiles(files: File[]): boolean {
    return insertFiles(files)
  }
  function build(el: HTMLDivElement) {
    const ed = new Editor({
      element: el,
      extensions: [
        StarterKit.configure({
          heading: { levels: HEADING_LEVELS },
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
        // Not resizable: a column width dragged in here would be stored in
        // the document and mean nothing to the Markdown it is exported as.
        // The wrapper is what scrolls a table wider than the page, rather
        // than the table pushing the whole column sideways.
        TableKit.configure({ table: { resizable: false, renderWrapper: true } }),
        Media.configure({ openFile: (id) => void showFile(id) }),
        // Markdown out on a copy, and in on a paste.
        MarkdownClipboard.configure({ taskLists: true, headings: Math.max(...HEADING_LEVELS) }),
      ],
      editorProps: {
        attributes: { class: 'ed-content', spellcheck: 'true' },
        // Files first, as a browser and the webviews on macOS and Windows
        // bring a pasted picture. WebKitGTK does not: see `pastePicture`.
        handlePaste: (view, event) =>
          insertFiles(Array.from(event.clipboardData?.files ?? [])) || pastePicture(view, event),
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
   * A paste that brought no files but may still have been a picture, which
   * WebKitGTK keeps from the page: a screenshot alone on the clipboard
   * reaches it as a paste with nothing in it, and a browser's "Copy image" as
   * the HTML of an `<img>` -- which the schema has no node for, so it would be
   * dropped without a word. The desktop shell can read the clipboard
   * (`clipboardPicture`), so it is asked, and what it finds goes in as if it
   * had been pasted as a file. Whether a paste is worth asking about is
   * `mayBePicture`'s to say, and it says no to anything with words in it, so
   * a paste of text, or of a note's own Markdown, never gets here.
   *
   * Answers true at once, for ProseMirror to stop pasting what it has: the
   * shell's answer arrives after the event is over. Two things follow from
   * that.
   *
   * The clipboard's HTML is read now, because the event's data is emptied
   * when it ends. It is what is pasted if the shell finds no picture, so that
   * nothing which used to paste has stopped. It goes in through `pasteHTML`,
   * which runs this handler again with an event of its own making --
   * `MarkdownClipboard` pastes through it as well -- and `mayBePicture` says
   * no to any event that is not trusted, whatever data it has, so it cannot
   * ask twice or swallow a paste it made itself.
   *
   * And the record and editor open now are remembered, as `insertFiles` does
   * for the files it stores, because they may not be the ones open when the
   * shell answers; the paste is dropped if they are not, and goes nowhere
   * else.
   */
  function pastePicture(view: EditorView, event: ClipboardEvent): boolean {
    if (!mayBePicture(event)) return false
    const html = event.clipboardData?.getData('text/html') ?? ''
    const target = docId
    const ed = editor
    void (async () => {
      const picture = await clipboardPicture()
      if (docId !== target || ed !== editor) return
      if (picture && insertFiles([picture])) return
      if (html) view.pasteHTML(html)
    })()
    return true
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
    if (app.supportsFiles) return insertCatalogued(files, target, ed)
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

  /**
   * The same, in a vault that has a files catalog: a picture, a film or any
   * file goes into Files, as it does from every app (`addFiles`: in chunks,
   * into the person's own folder, never through this window as one buffer),
   * and the document refers to it by id. There is no size limit here beyond
   * the disk's -- that belonged to the blob store.
   *
   * Nothing is handed to `onattach`: a node that names a file has no blob for
   * the record to keep, and the vault finds the file in the document when it
   * is saved. The same care about a record navigated away from applies: the
   * file is in Files either way, and is left there rather than put in a
   * document it was not dropped on.
   */
  function insertCatalogued(files: File[], target: string, ed: Editor): boolean {
    void (async () => {
      for (const file of files) {
        try {
          onstoring?.(file.name)
          const [entry] = await addFiles([file], { origin })
          if (!entry) throw new Error(whyNotAdded(file))
          const kind: MediaKind =
            entry.kind === 'image' || entry.kind === 'video' || entry.kind === 'audio'
              ? entry.kind
              : 'file'
          // The size the webview will draw it at, which is not always what
          // the file says: a photograph turned by its orientation, say.
          const size =
            kind === 'image'
              ? ((await imageSize(file)) ??
                (entry.media.width && entry.media.height
                  ? { width: entry.media.width, height: entry.media.height }
                  : null))
              : null
          if (docId !== target || ed !== editor) continue
          ed.chain()
            .focus()
            .insertMedia({
              file: entry.id,
              kind,
              mime: entry.mime || file.type || 'application/octet-stream',
              filename: entry.name,
              caption: '',
              width: size?.width ?? null,
              height: size?.height ?? null,
            })
            .run()
        } catch (e) {
          app.error = e instanceof Error ? e.message : String(e)
        } finally {
          onstoring?.(null)
        }
      }
    })()
    return true
  }

  /** A file in the document that is not drawn in place was pressed: look it
   *  up as it is now, and open it in the viewer. */
  async function showFile(id: FileId) {
    try {
      const [file] = await lookupFiles([id], { fresh: true })
      if (file) viewing = file
      else notify.warn('That file is no longer in Files.')
    } catch (e) {
      app.error = e instanceof Error ? e.message : String(e)
    }
  }

  /** The file open in the viewer, if any. */
  let viewing = $state<FileEntry | null>(null)

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
    oneditor?.(ed)
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
      if (editor === ed) {
        editor = null
        oneditor?.(null)
      }
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

<div class="prose" bind:this={host}></div>

{#if viewing}
  <FileViewer files={[viewing]} index={0} onclose={() => (viewing = null)} />
{/if}

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
     line height rather than being a fixed number of pixels. A third of a
     line, as note apps space their blocks: a new paragraph is still seen
     before it is read, and a run of short ones does not look double-spaced. */
  .prose :global(.ed-content > * + *) {
    margin-top: 0.5em;
  }
  /* A heading starts a section, so it stands further from what is above it
     than from what it heads. Through the same selector as the gap above, or
     that one, more specific than a bare `h2`, wins. */
  .prose :global(.ed-content > * + :is(h1, h2, h3)) {
    margin-top: 1.2em;
  }

  .prose :global(h1),
  .prose :global(h2),
  .prose :global(h3) {
    font-weight: 650;
    line-height: var(--leading-tight);
    letter-spacing: -0.012em;
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
    background: color-mix(in oklab, var(--highlight) 42%, transparent);
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
  /* Items sit a line apart, as in any notes app; the sliver between them is
     only so that two wrapped items do not read as one. */
  .prose :global(li + li) {
    margin-top: 0.15em;
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
    margin-top: 0.32em;
    accent-color: var(--journal-accent, var(--accent));
  }

  /* ── Tables ─────────────────────────────────────────────────────────── */

  .prose :global(.tableWrapper) {
    overflow-x: auto;
  }
  .prose :global(table) {
    border-collapse: collapse;
    font-size: 0.92em;
    line-height: var(--leading-normal);
    font-variant-numeric: tabular-nums lining-nums;
  }
  .prose :global(th),
  .prose :global(td) {
    position: relative;
    min-width: 4em;
    padding: 0.4em 0.75em;
    border: 1px solid var(--border);
    text-align: left;
    vertical-align: top;
  }
  .prose :global(th) {
    background: var(--bg-sunken);
    font-weight: 650;
  }
  .prose :global(th p),
  .prose :global(td p) {
    margin: 0;
  }
  /* A dragged selection across cells, which the browser has no native
     highlight for because it is not a text selection. */
  .prose :global(.selectedCell::after) {
    content: '';
    position: absolute;
    inset: 0;
    background: color-mix(in oklab, var(--journal-accent, var(--accent)) 14%, transparent);
    pointer-events: none;
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
  /* A file kept in Files opens in the viewer rather than being downloaded, so
     it is a button wearing a link's colour. */
  .prose :global(button.ed-file) {
    width: 100%;
    border: 0;
    background: none;
    color: var(--journal-accent, var(--accent));
    text-align: left;
    cursor: pointer;
  }
  .prose :global(button.ed-file:hover) {
    text-decoration: underline;
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
