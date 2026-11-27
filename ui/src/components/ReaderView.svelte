<script lang="ts" module>
  /** The page a PDF was at when the reader was last taken down, and the jump
   *  it had obeyed: Settings is drawn in the reader's place, and closing it
   *  builds the reader again, which should open where the person was and not
   *  at the last place they were sent. */
  let resume: { file: string; seq: number; page: number } | null = null

  /** The files whose warnings have been shown: said once, not on every visit. */
  const told = new Set<string>()
</script>

<script lang="ts">
  // The reader, on the stage: a document in the files folder open in front of
  // the person, its contents or a search beside it, and the assistant's rail
  // (drawn by the window, not here) on the right. It stands in the place of
  // whatever app is open; the sidebar stays.
  //
  // This is the frame and the bar. What is drawn is one of two things: a PDF,
  // by `PdfView` (pdf.js, the pages as the file made them), or anything else
  // -- an EPUB, Markdown, text, a web page -- by `ReaderFlow`, block by block
  // from what the service parsed. Either way what they report (the page and
  // block in view, the selection) goes to the store, which is what the
  // assistant is told, and what the store asks for (`reader.target`) they
  // obey. Nothing here knows where in the document anything is.
  //
  // Escape closes it, unless the caret is in a field (then it leaves the
  // field first) or something is over it. The window's own handler also has
  // Ctrl+F and a few bare keys, for the app behind this; the keys that are the
  // reader's are taken before it hears them, in the capture phase.

  import { tick, untrack } from 'svelte'
  import { appleKeyboard } from '../lib/platform'
  import { agent } from '../lib/agent.svelte'
  import { pinned } from '../lib/appwindow'
  import { fetchWhole, fileUrl, fileVersion } from '../lib/api'
  import { previewOf } from '../lib/files'
  import { isTyping, keysLabel } from '../lib/keys'
  import { menu } from '../lib/menu.svelte'
  import { closesOnBack, phone } from '../lib/phone.svelte'
  import { reader } from '../lib/reader.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import Loading from './Loading.svelte'
  import { PDF_ZOOMS as ZOOMS } from '../lib/documents'
  import PdfView from './PdfView.svelte'
  import PhoneBack from './PhoneBack.svelte'
  import ReaderFlow from './ReaderFlow.svelte'
  import ReaderOutline from './ReaderOutline.svelte'
  import ReaderSearch from './ReaderSearch.svelte'

  const mac = appleKeyboard
  const findKeys = keysLabel('mod+f', mac)

  const file = $derived(reader.file)
  /** What the service read of the open file; not what it read of the one
   *  before, while this one is still being read. */
  const outline = $derived(
    reader.outline && (!file || reader.outline.file === file.id) ? reader.outline : null,
  )
  /** Known from the file's own kind before the service has read it, so a PDF
   *  is already on its way while the outline is. */
  const isPdf = $derived(
    outline ? outline.format === 'pdf' : file ? previewOf(file) === 'pdf' : false,
  )
  const title = $derived(outline?.meta.title.trim() || file?.name || 'Document')
  const authors = $derived(outline?.meta.authors.join(', ') ?? '')
  const pages = $derived(outline?.pages ?? 0)

  let root = $state<HTMLElement | null>(null)
  // What each view exports, written out: a component's own type does not
  // reach the typed lint through a `.svelte` import.
  interface Focusable {
    focus(): void
  }
  interface PdfControls extends Focusable {
    step(by: 1 | -1): void
    fit(): void
  }
  let pdfView = $state<PdfControls | null>(null)
  let flow = $state<Focusable | null>(null)
  let searchPane = $state<Focusable | null>(null)

  // ── The PDF's bytes ─────────────────────────────────────────────────

  let pdfData = $state.raw<Uint8Array | null>(null)
  let pdfError = $state<string | null>(null)
  let zoom = $state(1)

  /** What fetching the bytes depends on: the file and its version, so that
   *  a rename or a tag does not read it all again. */
  const pdfKey = $derived(isPdf && file && !reader.error ? `${file.id}:${fileVersion(file)}` : null)

  // pdf.js wants all of a file's bytes, which a long one's address answers a
  // piece at a time (see `fetchWhole`), as the file viewer's does.
  $effect(() => {
    const key = pdfKey
    pdfData = null
    pdfError = null
    zoom = 1
    if (!key) return
    const f = untrack(() => file)
    if (!f) return
    let gone = false
    void (async () => {
      try {
        const bytes = await fetchWhole(fileUrl(f.id, fileVersion(f)))
        if (!gone) pdfData = bytes
      } catch {
        if (!gone) pdfError = 'This PDF could not be read.'
      }
    })()
    return () => {
      gone = true
    }
  })

  // ── Telling the store, and obeying it ───────────────────────────────

  /** The page to bring to the top, as `PdfView` takes it. */
  let pdfTarget = $state.raw<{ page: number; seq: number } | null>(null)
  let firstTarget = true

  $effect(() => {
    const target = reader.target
    const id = reader.file?.id
    untrack(() => {
      if (firstTarget && id) {
        firstTarget = false
        if (resume && resume.file === id && resume.seq === (target?.seq ?? -1)) {
          pdfTarget = { page: resume.page, seq: resume.seq }
          return
        }
      }
      if (target) pdfTarget = { page: target.page, seq: target.seq }
    })
  })

  function viewed(at: { page: number; lastPage: number }) {
    reader.reportView({ page: at.page, lastPage: at.lastPage > at.page ? at.lastPage : null })
    if (reader.file) {
      resume = { file: reader.file.id, seq: reader.target?.seq ?? -1, page: at.page }
    }
  }

  function selected(at: { page: number; quote: string } | null) {
    reader.setSelection(at ? { page: at.page, quote: at.quote } : null)
  }

  // ── Page ────────────────────────────────────────────────────────────

  let pageText = $state('1')
  let editing = $state(false)
  /** A page was asked for from the field, and the view has not caught up. */
  let jumped = false

  $effect(() => {
    const now = reader.page
    untrack(() => {
      jumped = false
      if (!editing) pageText = String(now)
    })
  })

  function commitPage() {
    const n = Math.round(Number(pageText.trim()))
    if (!Number.isFinite(n) || pages < 1) {
      pageText = String(reader.page)
      return
    }
    const to = Math.min(pages, Math.max(1, n))
    pageText = String(to)
    jumped = true
    reader.goTo({ page: to })
    focusDocument()
  }

  /** Where a step by page begins from while the view is still catching up
   *  with the last: pressing `]` twice quickly is two pages. */
  let stepped: { page: number; at: number } | null = null

  function step(by: 1 | -1) {
    if (pages < 1) return
    const base = stepped && performance.now() - stepped.at < 700 ? stepped.page : reader.page
    const to = Math.min(pages, Math.max(1, base + by))
    if (to === base) return
    stepped = { page: to, at: performance.now() }
    reader.goTo({ page: to })
  }

  function focusDocument() {
    if (isPdf) pdfView?.focus()
    else flow?.focus()
  }

  // ── Panes ───────────────────────────────────────────────────────────

  // A phone starts at the document, not at its contents: the pane is a page
  // over it there, and what was asked for is to read.
  if (phone.on) reader.panel = null

  // On a phone the document is a layer over the app it was opened from, and
  // the contents or the search a page over the document: back puts away the
  // top one.
  closesOnBack(
    () => phone.on,
    () => reader.close(),
  )
  closesOnBack(
    () => phone.on && outline !== null && reader.panel !== null,
    () => (reader.panel = null),
  )

  /** A section chosen, or a hit: the pane is for choosing, and on a phone
   *  there is no room for it beside what was chosen. */
  function chosen(e: MouseEvent) {
    if (!phone.on || !(e.target instanceof Element)) return
    if (e.target.closest('.side-row, .hit')) reader.panel = null
  }

  function toggle(which: 'outline' | 'search') {
    reader.panel = reader.panel === which ? null : which
  }

  async function find() {
    reader.panel = 'search'
    // Drawn on the next turn if it was not there; focused when it is.
    await tick()
    searchPane?.focus()
  }

  // ── Warnings ────────────────────────────────────────────────────────

  /** What the service said about the document, said once: a scan with no
   *  text on some pages, say. */
  let warning = $state<string | null>(null)
  let warningFor = ''

  $effect(() => {
    const o = outline
    untrack(() => {
      if (!o) {
        warning = null
        return
      }
      if (o.file === warningFor) return
      warningFor = o.file
      if (o.warnings.length > 0 && !told.has(o.file)) {
        told.add(o.file)
        warning = o.warnings.join(' ')
      } else warning = null
    })
  })

  // ── Keys ────────────────────────────────────────────────────────────

  function modalOpen(): boolean {
    return document.querySelector('[aria-modal="true"]') !== null
  }

  function onkeydown(e: KeyboardEvent) {
    const here = root
    if (!here || e.defaultPrevented) return
    const target = e.target
    const typing = isTyping(target)
    const inside =
      target === document.body ||
      target === document.documentElement ||
      (target instanceof Node && here.contains(target))

    // Find in the document, not in the app behind it.
    if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === 'f') {
      if (modalOpen()) return
      e.preventDefault()
      void find()
      return
    }

    if (e.key === 'Escape') {
      if (modalOpen() || menu.at !== null || !inside) return
      e.preventDefault()
      if (typing) {
        // A field gives up the caret first, to the document.
        if (target instanceof HTMLElement) target.blur()
        focusDocument()
      } else reader.close()
      return
    }

    if (typing || !inside || e.ctrlKey || e.metaKey || e.altKey || modalOpen()) return
    if (e.key === '[' || e.key === 'PageUp') {
      e.preventDefault()
      step(-1)
    } else if (e.key === ']' || e.key === 'PageDown') {
      e.preventDefault()
      step(1)
    }
  }
</script>

<svelte:window onkeydowncapture={onkeydown} />

<div class="reader" bind:this={root}>
  <header class="bar">
    {#if phone.on}
      <PhoneBack onclick={() => reader.close()} />
    {:else}
      <button
        class="tool back"
        aria-label="Close the document"
        title="Close (Esc)"
        onclick={() => reader.close()}
      >
        <Icon name="chevron" size={17} />
      </button>
    {/if}

    <div class="pane-titles">
      <span class="pane-title" {title}>{title}</span>
      {#if authors}<span class="pane-sub">{authors}</span>{/if}
    </div>

    {#if outline}
      <button
        class="tool"
        class:on={reader.panel === 'outline'}
        aria-label="Contents"
        aria-pressed={reader.panel === 'outline'}
        title="Contents"
        onclick={() => toggle('outline')}
      >
        <Icon name="list" size={16} />
      </button>
      <button
        class="tool"
        class:on={reader.panel === 'search'}
        aria-label="Search in the document"
        aria-pressed={reader.panel === 'search'}
        title="Search in the document ({findKeys})"
        onclick={() => toggle('search')}
      >
        <Icon name="search" size={16} />
      </button>

      {#if pages > 0}
        <form
          class="jump"
          onsubmit={(e) => {
            e.preventDefault()
            commitPage()
          }}
        >
          <label for="reader-page">{isPdf ? 'Page' : 'Section'}</label>
          <input
            id="reader-page"
            class="field"
            inputmode="numeric"
            autocomplete="off"
            aria-label={isPdf ? 'Go to page' : 'Go to section'}
            bind:value={pageText}
            onfocus={(e) => {
              editing = true
              e.currentTarget.select()
            }}
            onblur={() => {
              editing = false
              if (!jumped) pageText = String(reader.page)
            }}
          />
          <span>of {pages}</span>
        </form>
      {/if}

      {#if isPdf && pdfData}
        <div class="zoom" role="group" aria-label="Zoom">
          <button
            class="tool"
            aria-label="Zoom out"
            title="Zoom out"
            disabled={zoom <= (ZOOMS[0] ?? 0.5)}
            onclick={() => pdfView?.step(-1)}
          >
            <Icon name="minus" size={15} />
          </button>
          <button class="percent" title="Fit to the window" onclick={() => pdfView?.fit()}>
            {Math.round(zoom * 100)}%
          </button>
          <button
            class="tool"
            aria-label="Zoom in"
            title="Zoom in"
            disabled={zoom >= (ZOOMS[ZOOMS.length - 1] ?? 3)}
            onclick={() => pdfView?.step(1)}
          >
            <Icon name="plus" size={15} />
          </button>
        </div>
      {/if}
    {/if}

    {#if agent.supported && !pinned}
      <button
        class="btn btn-outline"
        title="Ask the assistant about this document"
        onclick={() => reader.ask()}
      >
        <Icon name="sparkle" size={15} />
        <span class="ask-word">Ask</span>
      </button>
    {/if}
    <button
      class="tool desktop-only"
      aria-label="Close"
      title="Close (Esc)"
      onclick={() => reader.close()}
    >
      <Icon name="close" size={17} />
    </button>
  </header>

  <div class="body">
    {#if outline && reader.panel}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
      <aside
        class="pane"
        aria-label={reader.panel === 'outline' ? 'Contents' : 'Search'}
        onclick={chosen}
      >
        <div class="side-head">
          <span class="eyebrow">{reader.panel === 'outline' ? 'Contents' : 'Search'}</span>
          <button
            class="side-add"
            aria-label="Hide this pane"
            title="Hide this pane"
            onclick={() => (reader.panel = null)}
          >
            <Icon name="close" size={13} />
          </button>
        </div>
        {#if reader.panel === 'outline'}
          <ReaderOutline />
        {:else}
          <ReaderSearch bind:this={searchPane} />
        {/if}
      </aside>
    {/if}

    <div class="doc" class:paper={isPdf && !reader.error}>
      {#if warning}
        <div class="note" role="status">
          <Icon name="info" size={15} />
          <span>{warning}</span>
          <button class="tool small" aria-label="Dismiss" onclick={() => (warning = null)}>
            <Icon name="close" size={13} />
          </button>
        </div>
      {/if}

      {#if reader.error}
        <EmptyState lead="This document could not be opened.">
          {#snippet note()}{reader.error}{/snippet}
          {#snippet action()}
            <button class="btn btn-primary" onclick={() => reader.close()}>Close</button>
          {/snippet}
        </EmptyState>
      {:else if pdfError}
        <EmptyState lead={pdfError}>
          {#snippet note()}
            Close it and try again, or save it from Files to look at it elsewhere.
          {/snippet}
        </EmptyState>
      {:else if !outline || (isPdf && !pdfData)}
        <Loading label="Opening…" centred />
      {:else}
        {#key file?.id}
          {#if isPdf && pdfData}
            <PdfView
              bind:this={pdfView}
              bind:zoom
              data={pdfData}
              controls={false}
              target={pdfTarget}
              onfail={(message: string) => (pdfError = message)}
              onview={viewed}
              onselect={selected}
              onready={() => pdfView?.focus()}
            />
          {:else}
            <ReaderFlow bind:this={flow} />
          {/if}
        {/key}
      {/if}
    </div>
  </div>
</div>

<style>
  /* As tall as the stage by a stated height, not stretched: see
     `.shell-stage` in app.css. */
  .reader {
    height: 100%;
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }

  /* Wraps rather than clips: on a narrow window the later controls drop to a
     second line instead of falling off the edge. */
  .bar {
    display: flex;
    flex: none;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px var(--sp-2);
    min-height: 56px;
    padding: var(--sp-2) var(--sp-4);
    border-bottom: 1px solid var(--border);
  }
  .bar .pane-titles {
    min-width: 8em;
  }
  .tool {
    display: inline-flex;
    flex: none;
    align-items: center;
    justify-content: center;
    min-width: 32px;
    height: 32px;
    padding: 0 6px;
    border-radius: var(--radius-pill);
    color: var(--fg-muted);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .tool:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .tool:disabled {
    opacity: 0.4;
  }
  .tool.on {
    background: var(--bg-selected);
    color: var(--accent);
  }
  .tool.small {
    min-width: 24px;
    height: 24px;
  }
  /* The chevron points on, as it does in a list; here it points back. */
  .back :global(.icon) {
    transform: scaleX(-1);
  }

  .jump {
    display: flex;
    flex: none;
    align-items: center;
    gap: 6px;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .jump .field {
    width: 4.2em;
    height: 28px;
    padding: 0 var(--sp-2);
    text-align: center;
  }
  .zoom {
    display: flex;
    flex: none;
    align-items: center;
    gap: 2px;
  }
  .percent {
    min-width: 48px;
    height: 32px;
    border-radius: var(--radius-pill);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    color: var(--fg-muted);
  }
  .percent:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .body {
    display: flex;
    flex: 1;
    min-height: 0;
  }
  .pane {
    flex: none;
    width: var(--reader-pane-w, 290px);
    height: 100%;
    display: flex;
    flex-direction: column;
    border-right: 1px solid var(--border);
  }
  .pane .side-head {
    padding-top: var(--sp-3);
  }

  .doc {
    flex: 1;
    min-width: 0;
    height: 100%;
    display: flex;
    flex-direction: column;
  }
  /* Pages are white paper: they stand on the muted surface of the theme. */
  .doc.paper {
    background: var(--bg-sunken);
  }
  /* The document fills what is left under the note, if there is one. */
  .doc > :global(.pdf),
  .doc > :global(.flow) {
    flex: 1;
    min-height: 0;
  }
  .note {
    display: flex;
    flex: none;
    align-items: center;
    gap: var(--sp-2);
    padding: 6px var(--sp-4);
    border-bottom: 1px solid var(--border);
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .note span {
    flex: 1;
    min-width: 0;
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* The bar: the way back, the title, the two panes, and under them -- it
     wraps -- the page to go to and what the document's kind has. Every
     control a finger's size. */
  :global([data-phone]) .bar {
    gap: 0 var(--sp-1);
    min-height: 52px;
    padding: var(--sp-1) var(--sp-2) var(--sp-1) var(--sp-1);
  }
  /* Room for a title that can be read: the controls on this row take what
     they need, and the page to go to wraps to the row below rather than
     leaving the title two letters and an ellipsis. */
  :global([data-phone]) .bar .pane-titles {
    flex: 1 1 0;
    min-width: 7em;
  }
  :global([data-phone]) .tool {
    min-width: 44px;
    height: 44px;
  }
  :global([data-phone]) .tool.small {
    min-width: 40px;
    height: 40px;
  }
  :global([data-phone]) .percent {
    height: 44px;
  }
  /* Ask is its sparkle: the word is a third of the bar. */
  :global([data-phone]) .bar .btn {
    min-width: 44px;
    min-height: 44px;
    padding: 0;
    justify-content: center;
  }
  :global([data-phone]) .ask-word {
    display: none;
  }
  :global([data-phone]) .jump {
    order: 5;
    flex: 1 1 auto;
    padding: 0 0 var(--sp-1) var(--sp-3);
  }
  :global([data-phone]) .jump .field {
    height: 40px;
  }
  :global([data-phone]) .zoom {
    order: 6;
    margin-left: auto;
  }

  /* The contents and the search are a page over the document, not a column
     beside it. */
  :global([data-phone]) .body {
    position: relative;
  }
  :global([data-phone]) .pane {
    position: absolute;
    inset: 0;
    z-index: 4;
    width: auto;
    border-right: 0;
    background: var(--bg-panel);
  }
  :global([data-phone]) .pane .side-head {
    padding-top: var(--sp-2);
  }
</style>
