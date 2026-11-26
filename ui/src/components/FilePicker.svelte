<script lang="ts">
  // Choosing documents from the files catalog: to attach to a library item
  // (several at once), or to make an item from (one). Only what the reader
  // can open is listed -- a PDF, an EPUB, Markdown, text or a web page -- and
  // nothing outside the catalog is ever offered: a file has to be in the
  // files folder first (see `docs/plans/reading.md`).
  //
  // The list is the newest documents, and a name typed into the field
  // narrows it by asking the catalog rather than by filtering what is held,
  // so a document from long ago is one word away however much is in the
  // folder. It is keyboard-first like a palette: the field keeps focus, the
  // arrows move through the rows, Enter chooses, Escape leaves.

  import { tick } from 'svelte'
  import { api } from '../lib/api'
  import { formatName, isReadable, placeOf } from '../lib/documents'
  import { formatSize } from '../lib/files'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { closesOnBack } from '../lib/phone.svelte'
  import { quietly } from '../lib/state.svelte'
  import { debounce } from '../lib/store/debounce'
  import { latest } from '../lib/store/latest'
  import type { FileEntry, FileId } from '../lib/types'
  import Icon from './Icon.svelte'

  interface Props {
    title: string
    /** One line under the title, saying what choosing does. */
    note?: string
    /** Tick several and choose them together, or choose one by clicking it. */
    multiple?: boolean
    /** What the confirming button says when several are ticked. */
    confirm?: string
    /** Files already on the item: left out, so nothing is offered twice. */
    taken?: FileId[]
    onchoose: (files: FileEntry[]) => void
    onclose: () => void
  }
  let {
    title,
    note = '',
    multiple = false,
    confirm = 'Choose',
    taken = [],
    onchoose,
    onclose,
  }: Props = $props()

  const uid = $props.id()

  /** The most the catalog is asked for at once: more than a screenful, and
   *  fewer than would make typing a name slower than scrolling. */
  const LIMIT = 200

  let query = $state('')
  let rows = $state.raw<FileEntry[]>([])
  /** How many more the catalog has than were asked for. */
  let more = $state(0)
  let loading = $state(true)
  let failed = $state(false)
  let cursor = $state(0)
  let ticked = $state<FileId[]>([])
  let list = $state<HTMLElement | null>(null)

  const shown = $derived(rows.filter((f) => !taken.includes(f.id)))
  const chosen = $derived(shown.filter((f) => ticked.includes(f.id)))

  const generation = latest()
  async function load(text: string) {
    const token = generation.next()
    loading = true
    try {
      // Documents and the unclassified, not pictures or films: an EPUB or a
      // Markdown file is as likely to be catalogued as "other" as "document",
      // and `isReadable` has the last word.
      const page = await api.listFiles({
        kinds: ['document', 'other'],
        status: 'present',
        text: text.trim() || null,
        sort: 'when',
        descending: true,
        limit: LIMIT,
      })
      if (!generation.isCurrent(token)) return
      rows = page.files.filter((f) => isReadable(f))
      more = Math.max(0, page.total - page.files.length)
      failed = false
      cursor = 0
    } catch (e) {
      if (!generation.isCurrent(token)) return
      rows = []
      failed = true
      await quietly(e)
    } finally {
      if (generation.isCurrent(token)) loading = false
    }
  }

  const later = debounce((text: string) => void load(text), 160)
  void load('')

  function typed(text: string) {
    query = text
    later.call(text)
  }

  function toggle(file: FileEntry) {
    const id = file.id
    ticked = ticked.includes(id) ? ticked.filter((t) => t !== id) : [...ticked, id]
  }

  function pick(file: FileEntry) {
    if (multiple) toggle(file)
    else onchoose([file])
  }

  /** Enter: the ticked files, or -- when none are -- the one under the
   *  cursor, so a single file is two keys from the field. */
  function submit() {
    if (multiple && chosen.length > 0) onchoose(chosen)
    else {
      const file = shown[cursor]
      if (file) onchoose([file])
    }
  }

  async function move(by: number) {
    if (shown.length === 0) return
    cursor = Math.min(shown.length - 1, Math.max(0, cursor + by))
    await tick()
    list?.querySelector(`[data-at="${cursor}"]`)?.scrollIntoView({ block: 'nearest' })
  }

  // Back puts it away, on a phone.
  closesOnBack(
    () => true,
    () => onclose(),
  )

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault()
      e.stopPropagation()
      onclose()
    } else if (e.key === 'ArrowDown') {
      e.preventDefault()
      void move(1)
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      void move(-1)
    } else if (e.key === 'Enter' && e.target instanceof HTMLInputElement) {
      // Only from the field: Enter on Cancel is a press of Cancel.
      e.preventDefault()
      submit()
    }
  }
</script>

<svelte:window {onkeydown} />

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={onclose}></div>
<div class="sheet picker" role="dialog" aria-modal="true" aria-label={title} use:trapFocus>
  <h2 class="title">{title}</h2>
  {#if note}<p class="hint">{note}</p>{/if}

  <input
    class="field"
    type="search"
    role="combobox"
    aria-expanded="true"
    aria-controls="{uid}-list"
    aria-activedescendant={shown[cursor] ? `${uid}-row-${cursor}` : undefined}
    aria-label="Find a document"
    placeholder="Find a document by name"
    autocomplete="off"
    spellcheck="false"
    value={query}
    use:focusOnMount
    oninput={(e) => typed(e.currentTarget.value)}
  />

  <div
    class="list scroll"
    id="{uid}-list"
    role="listbox"
    aria-multiselectable={multiple}
    bind:this={list}
  >
    {#each shown as file, i (file.id)}
      {@const on = ticked.includes(file.id)}
      <button
        id="{uid}-row-{i}"
        class="list-row row"
        class:sel={cursor === i}
        role="option"
        aria-selected={multiple ? on : cursor === i}
        data-at={i}
        tabindex="-1"
        onmousemove={() => (cursor = i)}
        onclick={() => pick(file)}
      >
        {#if multiple}
          <span class="tick" class:on aria-hidden="true">
            {#if on}<Icon name="tick" size={12} weight={2.2} />{/if}
          </span>
        {:else}
          <span class="kind" aria-hidden="true"><Icon name="file" size={16} /></span>
        {/if}
        <span class="text">
          <span class="name">{file.name}</span>
          <span class="where">{placeOf(file) || 'The top of the files folder'}</span>
        </span>
        <span class="facts">
          {[formatName(file), formatSize(file.size)].filter(Boolean).join(' · ')}
        </span>
      </button>
    {/each}
    {#if loading && shown.length === 0}
      <p class="status" role="status">Looking…</p>
    {:else if failed}
      <p class="status err">The documents could not be listed.</p>
    {:else if shown.length === 0}
      <p class="status">
        {#if query.trim()}
          No document is named like “{query.trim()}”.
        {:else}
          Nothing to choose from. A PDF, an EPUB, a Markdown or text file or a web page has to be in
          your files first.
        {/if}
      </p>
    {:else if more > 0}
      <p class="status">Showing the newest. Type a name to find an older one.</p>
    {/if}
  </div>

  <div class="sheet-row">
    {#if multiple && chosen.length > 0}
      <span class="count">{chosen.length} ticked</span>
    {/if}
    <span class="spacer"></span>
    <button class="btn" onclick={onclose}>Cancel</button>
    {#if multiple}
      <button class="btn btn-primary" disabled={chosen.length === 0} onclick={submit}>
        {confirm}
      </button>
    {/if}
  </div>
</div>

<style>
  /* Wider than a question: a file's name wants room, and a path under it. */
  .picker {
    top: 14%;
    width: min(560px, calc(100vw - var(--sp-8)));
  }
  .title {
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
  }
  .title + .hint {
    margin-top: var(--sp-1);
  }
  .field {
    margin-top: var(--sp-3);
  }

  .list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    max-height: min(46vh, 380px);
    margin: var(--sp-2) calc(var(--sp-2) * -1) 0;
    padding: 2px 0;
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
    flex: none;
  }
  .kind {
    flex: none;
    display: grid;
    place-items: center;
    width: 18px;
    color: var(--fg-faint);
  }
  .tick {
    flex: none;
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border: 1.5px solid var(--border-strong);
    border-radius: 5px;
    color: var(--fg-on-accent);
  }
  .tick.on {
    border-color: var(--accent);
    background: var(--accent);
  }
  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .name,
  .where {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .name {
    font-size: var(--text-base);
    font-weight: 550;
    color: var(--fg);
  }
  .where {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .facts {
    flex: none;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .status {
    margin: 0;
    padding: var(--sp-3);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }
  .status.err {
    color: var(--danger);
  }
  .count {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  /* ── A phone ──────────────────────────────────────────────────────────
     The sheet's own width and place (app.css), and rows a finger can aim at,
     with what sizes them left for the inspector to say. */
  :global([data-phone]) .picker {
    top: calc(var(--safe-top) + max(8dvh, var(--sp-4)));
    width: calc(100vw - var(--sp-6) - var(--safe-left) - var(--safe-right));
  }
  :global([data-phone]) .list {
    max-height: min(46dvh, 380px);
  }
  :global([data-phone]) .row {
    min-height: 52px;
  }
  :global([data-phone]) .facts {
    display: none;
  }
  :global([data-phone]) .tick {
    width: 22px;
    height: 22px;
  }
</style>
