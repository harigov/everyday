<script lang="ts">
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
  import { longDate, plural, relativeTime } from '../lib/format'
  import Toolbar from './Toolbar.svelte'
  import EntryMeta from './EntryMeta.svelte'
  import TrackerStrip from './TrackerStrip.svelte'
  import Logo from './Logo.svelte'
  import Icon from './Icon.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import type { MediaKind } from '../lib/types'

  let host = $state<HTMLDivElement>()
  let editor = $state<Editor | null>(null)
  let words = $state(0)
  let dropping = $state(false)
  /** The file being written to the vault, if any. See `insertFiles`. */
  let storing = $state<string | null>(null)
  /** Guards against the programmatic `setContent` echoing back as an edit. */
  let loading = false
  /** Which entry the ProseMirror document currently holds. */
  let loadedId: string | null = null
  let wordTimer: ReturnType<typeof setTimeout> | null = null

  const entry = $derived(app.entry)

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
          placeholder: ({ node }) =>
            node.type.name === 'heading' ? 'Heading' : 'What happened today?',
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
      // body is pulled from `getJSON()` once per save (see app.bindBody);
      // doing it per keystroke walked the whole document every character and
      // was most of the typing latency on a long entry.
      onUpdate: () => {
        if (loading || !app.entry) return
        app.scheduleSave()
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
      words = editor?.storage.characterCount.words() ?? 0
    }, 400)
  }

  /**
   * Store dropped or pasted files as blobs and insert media nodes.
   *
   * Two things here are about the gap between dropping a file and seeing it.
   * A phone video is hundreds of megabytes, so reading it, sealing it and
   * writing it out is seconds of work with nothing on screen -- hence
   * `storing`, which puts the filename in the status bar for as long as it
   * takes rather than leaving a drop that appears to have done nothing.
   *
   * And the entry open when the drop happened is remembered, because it may
   * not be the entry open when the write finishes. Switching entries during
   * an upload used to file the attachment against whichever entry had since
   * been opened and insert the video into *its* document -- so a clip landed
   * in a day it had nothing to do with, and the entry it was dropped on had
   * no record of it at all.
   */
  function insertFiles(files: File[]): boolean {
    const target = app.entry
    if (!files.length || !editor || !target) return false
    const ed = editor
    void (async () => {
      for (const file of files) {
        try {
          storing = file.name
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
          // reclaims a blob nothing references -- so a drop whose entry has
          // been navigated away from is dropped quietly rather than being
          // put somewhere it does not belong.
          if (app.entry !== target || ed !== editor) continue
          ed.chain().focus().insertMedia(media).run()
          target.attachments = [
            ...target.attachments,
            { ...media, byteLen: file.size, width: size?.width, height: size?.height },
          ]
          app.scheduleSave()
        } catch (e) {
          app.error = e instanceof Error ? e.message : String(e)
        } finally {
          storing = null
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
  // The `{#if entry}` above unmounts this whole pane whenever nothing is
  // open -- switching to an empty journal, locking, deleting the last entry.
  // The old guard was `if (host && !editor)`, so after such a round trip
  // `host` was a brand new <div> but `editor` still held a ProseMirror view
  // attached to the discarded one. Nothing rebuilt, and the pane came back
  // with no editable element in it at all: no caret, no typing, no way to
  // tell anything was wrong. Rebuilding when `host` changes is the fix, and
  // the teardown stops the old view leaking with it.
  $effect(() => {
    const el = host
    if (!el) return
    const ed = build(el)
    editor = ed
    loadedId = null
    app.bindBody(() => ed.getJSON() as never)
    return () => {
      // Capture the document while the view is still alive. If the pane is
      // going away because nothing is open, `syncBody` is a no-op; it can
      // never write this document into a different entry, because switching
      // entries does not unmount the pane.
      app.syncBody()
      app.bindBody(null)
      ed.destroy()
      if (editor === ed) editor = null
    }
  })

  // Swap the document when the selected entry changes -- and *only* then.
  //
  // This used to depend on the whole `entry` object, which meant every write
  // to any of its fields re-ran it: the autosave stamping `updatedAt`, a
  // keystroke in the title, adding a tag. Each re-run replaced the entire
  // ProseMirror document, which threw away the caret and fired transactions
  // from inside an effect -- and that last part crashed Svelte with
  // `effect_update_depth_exceeded`, after which every button in the app
  // stopped responding. Reading only `id` is what keeps it to real swaps.
  $effect(() => {
    const id = entry?.id ?? null
    if (!editor) return
    if (!id) {
      loadedId = null
      return
    }
    if (id === loadedId) return
    loadedId = id
    // Untracked: reading the document must not subscribe this effect to
    // every node in it.
    const body = untrack(() => app.entry?.body)
    loading = true
    editor.commands.setContent(body as never, { emitUpdate: false })
    words = editor.storage.characterCount.words()
    loading = false
  })

  onDestroy(() => {
    if (wordTimer) clearTimeout(wordTimer)
    // The effect teardown above captures the document; this only has to make
    // sure it reaches disk.
    app.syncBody()
    void app.flush()
  })

  let confirmingDelete = $state(false)

  async function deleteEntry() {
    confirmingDelete = false
    const id = app.entry?.id
    if (id) await app.deleteEntry(id)
  }

  function onDragOver(e: DragEvent) {
    if (e.dataTransfer?.types.includes('Files')) {
      e.preventDefault()
      dropping = true
    }
  }
</script>

{#if !entry}
  <div class="empty">
    <div class="empty-inner">
      <div class="empty-mark"><Logo size={40} /></div>
      <h2>Nothing open</h2>
      <p>Choose an entry, or start a new one.</p>
      <button class="btn btn-primary" onclick={() => app.newEntry()}>New entry</button>
    </div>
  </div>
{:else}
  <div
    class="editor"
    class:dropping
    ondragover={onDragOver}
    ondragleave={() => (dropping = false)}
    ondrop={() => (dropping = false)}
    role="region"
    aria-label="Entry editor"
  >
    <Toolbar {editor} />

    <div class="scroll canvas">
      <div class="page">
        <header class="head">
          <div class="date">{longDate(entry.localDate)}</div>
          <input
            class="title"
            placeholder="Title"
            bind:value={entry.title}
            oninput={() => {
              app.scheduleSave()
              app.touch()
            }}
            spellcheck="false"
          />
          <EntryMeta {entry} />
          <!-- What the day recorded in numbers, under what it recorded in
               prose. Keyed on the journal and the date rather than on the
               entry, because a reading belongs to the day: it survives this
               entry being deleted, and it can be recorded on a day nothing
               was written at all. -->
          <TrackerStrip journalId={entry.journalId} date={entry.localDate} />
        </header>

        <div class="prose" bind:this={host}></div>
      </div>
    </div>

    <footer class="status">
      <span>{plural(words, 'word')}</span>
      <span class="dot">·</span>
      <span>
        {#if storing}Storing {storing}…
        {:else if app.saving}Saving…
        {:else if app.lastSaved}Saved {relativeTime(app.lastSaved)}
        {:else}Edited {relativeTime(entry.updatedAt)}{/if}
      </span>
      <span class="spacer"></span>
      <button class="delete" onclick={() => (confirmingDelete = true)} title="Delete this entry">
        <Icon name="trash" size={14} weight={1.5} />
        Delete
      </button>
    </footer>

    {#if dropping}
      <div class="dropzone"><span>Drop to add to this entry</span></div>
    {/if}
  </div>

  {#if confirmingDelete}
    <ConfirmDialog
      title="Delete this entry?"
      detail={'“' +
        (entry.title || 'Untitled entry') +
        '” and anything attached to it will be removed. This cannot be undone.'}
      confirmLabel="Delete entry"
      onconfirm={deleteEntry}
      oncancel={() => (confirmingDelete = false)}
    />
  {/if}
{/if}

<style>
  .editor {
    position: relative;
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg-raised);
  }
  .canvas {
    flex: 1;
  }

  /* --measure is the text column; the padding sits outside it. Setting it as
     the box width instead cost two thirds of an inch of line on every side. */
  .page {
    max-width: calc(var(--measure) + var(--sp-8) * 2);
    margin: 0 auto;
    padding: var(--sp-10) var(--sp-8) 30vh;
  }

  .head {
    margin-bottom: var(--sp-6);
  }

  .date {
    font-size: var(--text-sm);
    font-weight: 600;
    letter-spacing: 0.02em;
    color: var(--journal-accent, var(--accent));
    margin-bottom: var(--sp-2);
  }

  .title {
    width: 100%;
    border: none;
    background: none;
    padding: 0;
    font-family: var(--font-read);
    font-size: var(--text-3xl);
    font-weight: 650;
    line-height: var(--leading-tight);
    letter-spacing: -0.018em;
    color: var(--fg);
    user-select: text;
  }
  .title::placeholder {
    color: var(--fg-faint);
    font-weight: 500;
  }

  .status {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: 30px;
    padding: 0 var(--sp-4);
    border-top: 1px solid var(--border);
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  .dot {
    opacity: 0.5;
  }
  .status .spacer {
    flex: 1;
  }

  /* Quiet until wanted: deleting an entry should be findable, not inviting. */
  .delete {
    display: flex;
    align-items: center;
    gap: 5px;
    height: 22px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    font-size: var(--text-xs);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .delete:hover {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
  }

  .dropzone {
    position: absolute;
    inset: var(--sp-3);
    display: grid;
    place-items: center;
    border: 2px dashed var(--accent);
    border-radius: var(--radius-lg);
    background: color-mix(in oklab, var(--accent) 8%, transparent);
    backdrop-filter: blur(2px);
    font-weight: 600;
    color: var(--accent);
    pointer-events: none;
  }

  .empty {
    display: grid;
    place-items: center;
    height: 100%;
    background: var(--bg-raised);
  }
  .empty-inner {
    text-align: center;
    max-width: 26ch;
  }
  .empty-mark {
    display: flex;
    justify-content: center;
    color: var(--fg-faint);
    margin-bottom: var(--sp-4);
  }
  .empty h2 {
    font-family: var(--font-read);
    font-size: var(--text-xl);
    font-weight: 620;
    margin-bottom: var(--sp-2);
  }
  .empty p {
    color: var(--fg-subtle);
    margin-bottom: var(--sp-5);
    line-height: var(--leading-normal);
  }

  /* ── Prose ───────────────────────────────────────────────────────────
     Typography for the entry body. This is the part of the app people
     actually look at, so it gets the reading face, a generous measure and a
     line height chosen for continuous reading rather than for UI density.

     The face is a humanist sans rather than a serif. It still contrasts with
     the interface around it -- different drawing, different colour on the
     page -- without the serif's mismatch against a screen-first UI. */

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
  .prose :global(.ed-content > * + *) {
    margin-top: 0.95em;
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
    margin-top: 0.3em;
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
