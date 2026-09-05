<script lang="ts">
  import { onDestroy } from 'svelte'
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

  let host = $state<HTMLDivElement>()
  let editor = $state<Editor | null>(null)
  let words = $state(0)
  let dropping = $state(false)
  /** Guards against the programmatic `setContent` echoing back as an edit. */
  let loading = false

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
        Link.configure({ openOnClick: true, autolink: true, protocols: ['http', 'https', 'mailto'] }),
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
          const dt = (event as DragEvent).dataTransfer
          return insertFiles(Array.from(dt?.files ?? []))
        },
      },
      onUpdate: ({ editor: ed }) => {
        if (loading || !app.entry) return
        app.entry.body = ed.getJSON() as typeof app.entry.body
        words = ed.storage.characterCount.words()
        app.scheduleSave()
        app.touch()
      },
    })
    return ed
  }

  /** Store dropped or pasted files as blobs and insert media nodes. */
  function insertFiles(files: File[]): boolean {
    if (!files.length || !editor || !app.entry) return false
    void (async () => {
      for (const file of files) {
        try {
          const bytes = new Uint8Array(await file.arrayBuffer())
          const blob = await api.putBlob(bytes)
          const kind = file.type.startsWith('image/')
            ? 'image'
            : file.type.startsWith('video/')
              ? 'video'
              : file.type.startsWith('audio/')
                ? 'audio'
                : 'file'
          const size = kind === 'image' ? await imageSize(file) : null
          editor!.chain().focus().insertMedia({
            blob,
            kind,
            mime: file.type || 'application/octet-stream',
            filename: file.name,
            caption: '',
            width: size?.width ?? null,
            height: size?.height ?? null,
          }).run()
          app.entry!.attachments = [
            ...app.entry!.attachments,
            {
              blob, kind, mime: file.type || 'application/octet-stream',
              filename: file.name, byteLen: file.size,
              width: size?.width, height: size?.height, caption: '',
            },
          ]
          app.scheduleSave()
        } catch (e) {
          app.error = e instanceof Error ? e.message : String(e)
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
      img.onerror = () => { resolve(null); URL.revokeObjectURL(url) }
      img.src = url
    })
  }

  $effect(() => {
    if (host && !editor) editor = build(host)
  })

  // Swap the document when the selected entry changes.
  $effect(() => {
    const current = entry
    if (!editor || !current) return
    loading = true
    editor.commands.setContent(current.body as never, { emitUpdate: false })
    words = editor.storage.characterCount.words()
    loading = false
  })

  onDestroy(() => {
    void app.flush()
    editor?.destroy()
    editor = null
  })

  function onDragOver(e: DragEvent) {
    if (e.dataTransfer?.types.includes('Files')) { e.preventDefault(); dropping = true }
  }
</script>

{#if !entry}
  <div class="empty">
    <div class="empty-inner">
      <div class="empty-mark">✦</div>
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
            oninput={() => { app.scheduleSave(); app.touch() }}
            spellcheck="false"
          />
          <EntryMeta {entry} />
        </header>

        <div class="prose" bind:this={host}></div>
      </div>
    </div>

    <footer class="status">
      <span>{plural(words, 'word')}</span>
      <span class="dot">·</span>
      <span>
        {#if app.saving}Saving…
        {:else if app.lastSaved}Saved {relativeTime(app.lastSaved)}
        {:else}Edited {relativeTime(entry.updatedAt)}{/if}
      </span>
    </footer>

    {#if dropping}
      <div class="dropzone"><span>Drop to add to this entry</span></div>
    {/if}
  </div>
{/if}

<style>
  .editor { position: relative; display: flex; flex-direction: column; height: 100%; background: var(--bg-raised); }
  .canvas { flex: 1; }

  .page {
    max-width: var(--measure);
    margin: 0 auto;
    padding: var(--sp-12) var(--sp-8) 30vh;
  }

  .head { margin-bottom: var(--sp-6); }

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
    font-weight: 600;
    line-height: var(--leading-tight);
    letter-spacing: -0.015em;
    color: var(--fg);
    user-select: text;
  }
  .title::placeholder { color: var(--fg-faint); font-weight: 500; }

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
  .dot { opacity: 0.5; }

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

  .empty { display: grid; place-items: center; height: 100%; background: var(--bg-raised); }
  .empty-inner { text-align: center; max-width: 26ch; }
  .empty-mark { font-size: 30px; color: var(--fg-faint); margin-bottom: var(--sp-3); }
  .empty h2 { font-family: var(--font-read); font-size: var(--text-xl); font-weight: 600; margin-bottom: var(--sp-2); }
  .empty p { color: var(--fg-subtle); margin-bottom: var(--sp-5); line-height: var(--leading-normal); }

  /* ── Prose ───────────────────────────────────────────────────────────
     Typography for the entry body. This is the part of the app people
     actually look at, so it gets a serif face, a generous measure and a
     line height chosen for continuous reading rather than for UI density. */

  .prose :global(.ed-content) {
    font-family: var(--font-read);
    font-size: var(--text-lg);
    line-height: var(--leading-prose);
    color: var(--fg);
    user-select: text;
    /* Old-style figures sit better in running prose than lining ones. */
    font-variant-numeric: oldstyle-nums proportional-nums;
    outline: none;
    -webkit-user-modify: read-write-plaintext-only;
  }
  .prose :global(.ed-content > * + *) { margin-top: 1.1em; }
  .prose :global(p) { hanging-punctuation: first last; }

  .prose :global(h1),
  .prose :global(h2),
  .prose :global(h3) {
    font-weight: 600;
    line-height: var(--leading-tight);
    letter-spacing: -0.012em;
    margin-top: 1.7em;
  }
  .prose :global(h1) { font-size: 1.5em; }
  .prose :global(h2) { font-size: 1.28em; }
  .prose :global(h3) { font-size: 1.1em; }

  .prose :global(strong) { font-weight: 650; }
  .prose :global(em) { font-style: italic; }
  .prose :global(mark) {
    background: color-mix(in oklab, #f5d90a 42%, transparent);
    color: inherit;
    border-radius: 2px;
    padding: 0 2px;
  }
  .prose :global(a) { color: var(--journal-accent, var(--accent)); }

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
  .prose :global(pre code) { background: none; border: none; padding: 0; font-size: inherit; }

  .prose :global(ul),
  .prose :global(ol) { padding-left: 1.3em; }
  .prose :global(li + li) { margin-top: 0.3em; }
  .prose :global(li p) { margin: 0; }

  .prose :global(ul[data-type='taskList']) { list-style: none; padding-left: 0; }
  .prose :global(ul[data-type='taskList'] li) { display: flex; gap: 0.6em; align-items: flex-start; }
  .prose :global(ul[data-type='taskList'] input) { margin-top: 0.45em; accent-color: var(--journal-accent, var(--accent)); }

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

  .prose :global(.ed-media) { margin: 1.6em 0; }
  .prose :global(.ed-media-frame) {
    overflow: hidden;
    border-radius: var(--radius-lg);
    background: var(--bg-sunken);
    box-shadow: var(--shadow-sm);
    line-height: 0;
  }
  .prose :global(.ed-media img),
  .prose :global(.ed-media video) { width: 100%; height: auto; display: block; }
  .prose :global(.ed-media audio) { width: 100%; display: block; line-height: normal; }
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
