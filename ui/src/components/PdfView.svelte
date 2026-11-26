<script lang="ts">
  // A PDF attachment, page under page, drawn by pdf.js -- see `lib/pdf.ts`
  // for what it is trusted with.
  //
  // A page is drawn when it scrolls near and forgotten when it scrolls far
  // away again: a canvas holds every pixel it shows, and a hundred pages
  // drawn at once would be gigabytes. It is drawn again, sharper, when the
  // width it is shown at changes -- the window, or the zoom.
  //
  // Nothing is asked of a page before it comes near, its size included: every
  // page is laid out in the first one's shape, and corrected when it is drawn.
  // A thousand-page document opens as fast as a one-page one.
  //
  // Over each drawn page lies its text, transparent, so that words can be
  // selected and copied. It comes and goes with the canvas, and a change of
  // zoom only rescales it (one CSS variable) rather than laying it out again.
  //
  // The attachment viewer and the file viewer use this with `data` and
  // `onfail` alone. The reader (`ReaderView.svelte`) also draws its own bar,
  // so it turns the pill off (`controls`), drives the zoom, asks for a page
  // to be brought to the top (`target`) and hears where the view is
  // (`onview`) and what is selected (`onselect`).

  import { untrack } from 'svelte'
  import type {
    PDFDocumentLoadingTask,
    PDFDocumentProxy,
    PDFPageProxy,
    RenderTask,
    TextLayer,
  } from 'pdfjs-dist/legacy/build/pdf.mjs'
  import { MAX_SELECTION_QUOTE } from '../lib/types'
  import { PDF_ZOOMS as ZOOMS } from '../lib/documents'
  import Icon from './Icon.svelte'
  import Loading from './Loading.svelte'

  interface Props {
    /** The file's bytes. pdf.js takes them over. */
    data: Uint8Array
    onfail: (message: string) => void
    /** Whether the zoom and page pill is drawn over the pages. */
    controls?: boolean
    /** How wide a page is drawn: 1 fits the window. */
    zoom?: number
    /** A page to bring to the top. Obeyed once for each `seq`, as soon as the
     *  pages are laid out, so the same page can be asked for again. */
    target?: { page: number; seq: number } | null
    /** The first page in view and the last, told as the view moves. */
    onview?: (at: { page: number; lastPage: number }) => void
    /** The words selected, and the page they start on; null once none are. */
    onselect?: (at: { page: number; quote: string } | null) => void
    /** The document is open and its pages laid out. */
    onready?: () => void
  }
  let {
    data,
    onfail,
    controls = true,
    zoom = $bindable(1),
    target = null,
    onview,
    onselect,
    onready,
  }: Props = $props()

  let pdf = $state.raw<PDFDocumentProxy | null>(null)
  /** Every page's width over its height, in order: the first page's for any
   *  not yet drawn. */
  let shapes = $state.raw<number[]>([])
  let available = $state(0)
  let current = $state(1)
  let scroller = $state<HTMLElement | null>(null)

  /** The widest a page is drawn at 100%: a page as wide as a big screen is
   *  harder to read, not easier. */
  const WIDEST = 880
  /** A canvas is never given more pixels than this, whatever the zoom. */
  const MOST_PIXELS = 4096 * 4096
  /** How much of a page has to show to count as being in view: a sliver of
   *  the last page at the top of the window is not what somebody is reading. */
  const SLIVER = 40

  const pageWidth = $derived(Math.max(120, Math.min(available - 32, WIDEST)) * zoom)

  /** pdf.js's text layer class, once its module has loaded. */
  let makeText: typeof TextLayer | null = null

  $effect(() => {
    const bytes = data
    let closed = false
    let task: PDFDocumentLoadingTask | null = null
    void (async () => {
      try {
        const lib = await import('../lib/pdf')
        if (closed) return
        makeText = lib.TextLayer
        task = lib.openPdf(bytes)
        const doc = await task.promise
        if (closed) return
        const first = (await doc.getPage(1)).getViewport({ scale: 1 })
        if (closed) return
        shapes = Array<number>(doc.numPages).fill(first.width / first.height)
        pdf = doc
        onready?.()
      } catch (e) {
        if (closed) return
        onfail(
          e instanceof Error && e.name === 'PasswordException'
            ? 'This PDF is protected by a password. Save it to open it.'
            : 'This PDF could not be read. Save it to open it.',
        )
      }
    })()
    return () => {
      closed = true
      pdf = null
      shapes = []
      void task?.destroy()
    }
  })

  /** One observer for every page, rather than one each. */
  let observer: IntersectionObserver | null = null
  const watching = new Map<Element, (near: boolean) => void>()

  function watch(sheet: HTMLElement, onchange: (near: boolean) => void) {
    observer ??= new IntersectionObserver(
      (entries) => {
        for (const entry of entries) watching.get(entry.target)?.(entry.isIntersecting)
      },
      { root: scroller, rootMargin: '900px 0px' },
    )
    watching.set(sheet, onchange)
    observer.observe(sheet)
  }

  function unwatch(sheet: HTMLElement) {
    watching.delete(sheet)
    observer?.unobserve(sheet)
  }

  $effect(() => () => {
    observer?.disconnect()
    observer = null
    watching.clear()
  })

  /** Page `n`'s own shape, once it is known, in place of the first page's. */
  function reshape(n: number, shape: number) {
    if (Math.abs((shapes[n - 1] ?? shape) - shape) < 0.001) return
    const next = shapes.slice()
    next[n - 1] = shape
    shapes = next
    // A page that was asked for is held at the top while the ones above it
    // are corrected, or each correction would slide it down the window.
    if (pin && performance.now() < pin.until) requestAnimationFrame(() => align())
  }

  /** Draws page `n` onto the canvas of `sheet` while it is near the view, at
   *  `width` CSS pixels, and lays its text over it. */
  function page(sheet: HTMLElement, initial: { n: number; width: number }) {
    const drawn = sheet.querySelector('canvas')
    const over = sheet.querySelector<HTMLElement>('.textLayer')
    if (!drawn || !over) return {}
    // Named again so that the functions below, which TypeScript does not
    // follow a check into, see that neither is null.
    const canvas: HTMLCanvasElement = drawn
    const layer: HTMLElement = over
    let at = initial
    let near = false
    let drawnWidth = 0
    let task: RenderTask | null = null
    /** Bumped by every paint and forget, so one that finishes late can tell
     *  it has been overtaken. */
    let turn = 0
    let text: TextLayer | null = null

    async function paint() {
      const doc = pdf
      if (!doc || !near || drawnWidth === at.width) return
      const mine = ++turn
      const width = at.width
      drawnWidth = width
      await settle()
      if (mine !== turn) return
      try {
        const p = await doc.getPage(at.n)
        if (mine !== turn) return
        const natural = p.getViewport({ scale: 1 })
        reshape(at.n, natural.width / natural.height)
        const css = width / natural.width
        let scale = css * (window.devicePixelRatio || 1)
        const pixels = natural.width * natural.height * scale * scale
        if (pixels > MOST_PIXELS) scale *= Math.sqrt(MOST_PIXELS / pixels)
        const viewport = p.getViewport({ scale })
        canvas.width = Math.floor(viewport.width)
        canvas.height = Math.floor(viewport.height)
        task = p.render({ canvas, viewport })
        words(p, css)
        await task.promise
      } catch {
        // Cancelled for a newer paint, or a page pdf.js could not draw: it
        // stays blank, and the next change of width tries it again.
        if (mine === turn) drawnWidth = 0
      }
    }

    /** The page's text, laid over it. Made once while the page is near; after
     *  that a change of zoom is only a change of this variable, which every
     *  font size and the layer's own size are written in terms of. */
    function words(p: PDFPageProxy, css: number) {
      layer.style.setProperty('--total-scale-factor', String(css))
      if (text || !makeText) return
      const built = new makeText({
        textContentSource: p.streamTextContent(),
        container: layer,
        viewport: p.getViewport({ scale: css }),
      })
      text = built
      built.render().then(
        () => {
          if (text !== built) return
          // What pdf.js's own viewer ends the layer with: the box that keeps
          // a drag across the gaps between lines from losing the selection.
          const end = document.createElement('div')
          end.className = 'endOfContent'
          layer.append(end)
        },
        () => {
          // Cancelled with the page, or a page whose text could not be read:
          // it is a page of pictures to the person selecting, nothing more.
        },
      )
    }

    /** Lets a draw still running on this canvas stop first: pdf.js refuses
     *  two at once on one canvas. */
    async function settle() {
      const running = task
      task = null
      if (!running) return
      running.cancel()
      try {
        await running.promise
      } catch {
        // Cancelled, as asked.
      }
    }

    function forget() {
      turn++
      drawnWidth = 0
      text?.cancel()
      text = null
      layer.textContent = ''
      void settle().then(() => {
        if (near) return
        // Zero pixels: the memory goes, the box keeps its size from CSS.
        canvas.width = 0
        canvas.height = 0
      })
    }

    watch(sheet, (isNear) => {
      near = isNear
      if (near) void paint()
      else forget()
    })

    return {
      update(next: { n: number; width: number }) {
        at = next
        void paint()
      },
      destroy() {
        unwatch(sheet)
        turn++
        text?.cancel()
        task?.cancel()
      },
    }
  }

  // ── Where the view is ───────────────────────────────────────────────

  /** The first index in `0..n` for which `after` holds, `after` being false
   *  and then true; `n` when it never is. The pages are in order, so a
   *  window onto a thousand of them is found in ten steps. */
  function firstWhere(n: number, after: (i: number) => boolean): number {
    let lo = 0
    let hi = n
    while (lo < hi) {
      const mid = (lo + hi) >> 1
      if (after(mid)) hi = mid
      else lo = mid + 1
    }
    return lo
  }

  let seen = { page: 1, lastPage: 1 }
  let sent = ''
  let reporting: ReturnType<typeof setTimeout> | null = null

  /** Which page is most of the way up the view, for "3 of 12", and which
   *  are in it, for whoever is listening. */
  function track() {
    const box = scroller
    if (!box || !pdf) return
    const pages = box.children
    const n = pages.length
    if (n === 0) return
    const sheet = (i: number) => pages[i] as HTMLElement
    const top = box.scrollTop
    const line = top + box.clientHeight / 3
    current = Math.max(
      1,
      firstWhere(n, (i) => sheet(i).offsetTop > line),
    )
    const first = Math.min(
      n,
      firstWhere(n, (i) => sheet(i).offsetTop + sheet(i).offsetHeight > top + SLIVER) + 1,
    )
    const last = Math.min(
      n,
      Math.max(
        first,
        firstWhere(n, (i) => sheet(i).offsetTop >= top + box.clientHeight - SLIVER),
      ),
    )
    seen = { page: first, lastPage: last }
    reportSoon()
  }

  /** Told at most a few times a second, and only when it changed: a scroll is
   *  a stream of events, and each report may be a write. */
  function reportSoon() {
    if (!onview || reporting) return
    reporting = setTimeout(() => {
      reporting = null
      const key = `${seen.page}-${seen.lastPage}`
      if (key === sent) return
      sent = key
      onview?.(seen)
    }, 150)
  }

  $effect(() => () => {
    if (reporting) clearTimeout(reporting)
  })

  /** A page asked for is held at the top until the pages above it have been
   *  drawn and corrected, or until the person scrolls away themselves. */
  let pin: { page: number; top: number; until: number } | null = null
  let obeyed = -1

  function align() {
    const box = scroller
    if (!box || !pin) return
    const sheet = box.children[pin.page - 1]
    if (!(sheet instanceof HTMLElement)) return
    box.scrollTop = Math.max(0, sheet.offsetTop - 6)
    pin.top = box.scrollTop
  }

  function jumpTo(page: number) {
    const n = shapes.length
    if (n === 0) return
    const held = Math.min(n, Math.max(1, Math.round(page)))
    pin = { page: held, top: 0, until: performance.now() + 1500 }
    align()
  }

  function scrolled() {
    // A scroll that did not come from `align` is somebody's own.
    if (pin && scroller && Math.abs(scroller.scrollTop - pin.top) > 3) pin = null
    track()
  }

  // Obey the request once the pages exist, and say where the view is: a
  // document opened at the top gets no scroll event to say so.
  $effect(() => {
    const want = target
    if (!pdf || !scroller) return
    untrack(() => {
      if (want && want.seq !== obeyed) {
        obeyed = want.seq
        jumpTo(want.page)
      }
      track()
    })
  })

  // ── What is selected ────────────────────────────────────────────────

  let selectedSent = ''

  /** The page whose box holds `node`, from 1; null when it is not in one of
   *  ours. */
  function pageOf(node: Node | null): number | null {
    const el = node instanceof Element ? node : (node?.parentElement ?? null)
    const sheet = el?.closest('.sheet-of-paper')
    if (!sheet || !scroller?.contains(sheet)) return null
    return Array.from(scroller.children).indexOf(sheet) + 1
  }

  function readSelection(): { page: number; quote: string } | null {
    const selection = document.getSelection()
    if (!selection || selection.isCollapsed || selection.rangeCount === 0) return null
    const range = selection.getRangeAt(0)
    const start = pageOf(range.startContainer) ?? pageOf(range.endContainer)
    if (start === null) return null
    // Whitespace collapsed: a line break in a PDF is where a line ran out.
    const quote = selection.toString().replace(/\s+/g, ' ').trim().slice(0, MAX_SELECTION_QUOTE)
    return quote ? { page: start, quote: quote.trimEnd() } : null
  }

  function selectionChanged() {
    const now = readSelection()
    const key = now ? `${now.page}|${now.quote}` : ''
    if (key === selectedSent) return
    selectedSent = key
    onselect?.(now)
  }

  // The selection moves a character at a time as a drag goes on; it is read
  // when it settles.
  $effect(() => {
    if (!onselect) return
    let timer: ReturnType<typeof setTimeout> | null = null
    const soon = () => {
      if (timer) clearTimeout(timer)
      timer = setTimeout(selectionChanged, 200)
    }
    document.addEventListener('selectionchange', soon)
    return () => {
      document.removeEventListener('selectionchange', soon)
      if (timer) clearTimeout(timer)
    }
  })

  // While the pointer is down on a page's text, its end-of-content box covers
  // the layer (see the style below), so a drag that wanders into the white
  // between lines still ends the selection where it is, not somewhere odd.
  $effect(() => {
    const box = scroller
    if (!box) return
    const down = (e: PointerEvent) => {
      if (e.target instanceof Element) e.target.closest('.textLayer')?.classList.add('selecting')
    }
    const up = () => {
      for (const layer of box.querySelectorAll('.textLayer.selecting')) {
        layer.classList.remove('selecting')
      }
    }
    box.addEventListener('pointerdown', down)
    window.addEventListener('pointerup', up)
    window.addEventListener('pointercancel', up)
    return () => {
      box.removeEventListener('pointerdown', down)
      window.removeEventListener('pointerup', up)
      window.removeEventListener('pointercancel', up)
    }
  })

  // ── Zoom ────────────────────────────────────────────────────────────

  function setZoom(next: number) {
    const box = scroller
    if (!box || next === zoom) return
    // Keep the same place in the document in view across the change.
    const share = box.scrollTop / Math.max(1, box.scrollHeight)
    zoom = next
    requestAnimationFrame(() => {
      if (scroller) scroller.scrollTop = share * scroller.scrollHeight
    })
  }

  /** One step in or out along `ZOOMS`. */
  export function step(by: 1 | -1) {
    const i = ZOOMS.findIndex((z) => z >= zoom - 0.001)
    const next = ZOOMS[Math.min(ZOOMS.length - 1, Math.max(0, (i < 0 ? 3 : i) + by))]
    if (next !== undefined) setZoom(next)
  }

  /** Back to a page as wide as the window allows. */
  export function fit() {
    setZoom(1)
  }

  /** Put the keyboard on the pages, so the arrow keys scroll them. */
  export function focus() {
    scroller?.focus({ preventScroll: true })
  }
</script>

<div class="pdf">
  <div
    class="pages"
    class:bare={!controls}
    bind:this={scroller}
    bind:clientWidth={available}
    onscroll={scrolled}
    tabindex="-1"
  >
    {#if pdf}
      {#each shapes as shape, i (i)}
        <div
          class="sheet-of-paper"
          style:width="{pageWidth}px"
          style:height="{pageWidth / shape}px"
          use:page={{ n: i + 1, width: pageWidth }}
        >
          <canvas aria-label="Page {i + 1}"></canvas>
          <div class="textLayer"></div>
        </div>
      {/each}
    {:else}
      <Loading label="Opening…" centred />
    {/if}
  </div>
  {#if pdf && controls}
    <div class="controls">
      <button
        aria-label="Zoom out"
        title="Zoom out"
        onclick={() => step(-1)}
        disabled={zoom <= 0.5}
      >
        <Icon name="minus" size={15} />
      </button>
      <button class="zoom" title="Fit to the window" onclick={fit}>
        {Math.round(zoom * 100)}%
      </button>
      <button aria-label="Zoom in" title="Zoom in" onclick={() => step(1)} disabled={zoom >= 3}>
        <Icon name="plus" size={15} />
      </button>
      <span class="count">{current} of {shapes.length}</span>
    </div>
  {/if}
</div>

<style>
  .pdf {
    position: relative;
    width: 100%;
    height: 100%;
  }
  /* Block layout, not a centring flexbox: a page zoomed wider than the
     window then overflows to the right, where it can be scrolled to, rather
     than off both edges, where its left side could not be. Positioned, so
     the pages' `offsetTop` is their place in what scrolls, whatever else
     sits between this and the viewport. */
  .pages {
    position: relative;
    height: 100%;
    overflow: auto;
    /* The 16px either side is what `pageWidth` leaves out of `available`. */
    padding: 16px 16px 72px;
    outline: none;
  }
  /* No pill to clear. */
  .pages.bare {
    padding-bottom: 24px;
  }
  .sheet-of-paper {
    position: relative;
    margin: 0 auto 14px;
    background: #ffffff;
    border-radius: 2px;
    /* A hairline as well as the shadow: on a dark theme the shadow vanishes
       and the page's edge would otherwise be white on near-black. */
    outline: 1px solid rgb(0 0 0 / 0.18);
    box-shadow: 0 1px 3px rgb(0 0 0 / 0.4);
  }
  .sheet-of-paper canvas {
    display: block;
    width: 100%;
    height: 100%;
  }
  .controls {
    position: absolute;
    left: 50%;
    bottom: 16px;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 4px 6px;
    border-radius: var(--radius-pill);
    background: rgb(28 27 31 / 0.92);
    color: #f2f1ee;
    box-shadow: 0 2px 10px rgb(0 0 0 / 0.35);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }
  .controls button {
    display: grid;
    place-items: center;
    min-width: 30px;
    height: 30px;
    padding: 0 6px;
    border-radius: var(--radius-pill);
    color: inherit;
  }
  .controls button:hover:not(:disabled) {
    background: rgb(255 255 255 / 0.12);
  }
  .controls button:disabled {
    opacity: 0.4;
  }
  .zoom {
    min-width: 52px;
  }
  .count {
    padding: 0 10px 0 8px;
    color: rgb(255 255 255 / 0.7);
  }

  /* ── The text layer ───────────────────────────────────────────────
     pdf.js's own rules for it (`web/pdf_viewer.css`), cut down to what this
     layer uses and written flat, here, rather than loading the whole of its
     stylesheet -- which is mostly for the annotations, editors and thumbnails
     this viewer does not have, and which restyles `button` and `input`
     everywhere. Global because pdf.js makes the spans, not Svelte. The words
     are transparent: the canvas under them is what is seen, and these only
     take the pointer and show the selection.

     `--total-scale-factor` is set on each layer from the width its page is
     drawn at, and everything in it -- its size, every font size -- is
     written in terms of it. */
  :global(.textLayer) {
    color-scheme: only light;
    position: absolute;
    inset: 0;
    z-index: 0;
    overflow: clip;
    opacity: 1;
    text-align: initial;
    line-height: 1;
    letter-spacing: normal;
    word-spacing: normal;
    -webkit-text-size-adjust: none;
    text-size-adjust: none;
    forced-color-adjust: none;
    transform-origin: 0 0;
    caret-color: CanvasText;
    --scale-round-x: 1px;
    --scale-round-y: 1px;
    --min-font-size: 1;
    --text-scale-factor: calc(var(--total-scale-factor) * var(--min-font-size));
    --min-font-size-inv: calc(1 / var(--min-font-size));
  }
  :global(.textLayer :is(span, br)) {
    position: absolute;
    color: transparent;
    white-space: pre;
    cursor: text;
    transform-origin: 0% 0%;
    -webkit-user-select: text;
    user-select: text;
  }
  :global(.textLayer > :not(.markedContent)),
  :global(.textLayer .markedContent span:not(.markedContent)) {
    z-index: 1;
    --font-height: 0;
    font-size: calc(var(--text-scale-factor) * var(--font-height));
    --scale-x: 1;
    --rotate: 0deg;
    transform: rotate(var(--rotate)) scaleX(var(--scale-x)) scale(var(--min-font-size-inv));
  }
  :global(.textLayer .markedContent) {
    display: contents;
  }
  :global(.textLayer span[role='img']) {
    cursor: default;
    -webkit-user-select: none;
    user-select: none;
  }
  /* The selection in the theme's accent, as the rest of the window's. */
  :global(.textLayer ::selection) {
    background: color-mix(in srgb, var(--accent) 35%, transparent);
    color: transparent;
  }
  :global(.textLayer br::selection) {
    background: transparent;
  }
  :global(.textLayer .endOfContent) {
    display: block;
    position: absolute;
    inset: 100% 0 0;
    z-index: 0;
    cursor: default;
    -webkit-user-select: none;
    user-select: none;
  }
  :global(.textLayer.selecting .endOfContent) {
    top: 0;
  }
  /* A page that is turned in the file (its /Rotate) has a layer laid out in
     its own unturned shape, turned into place here. */
  :global(.textLayer[data-main-rotation='90']) {
    transform: rotate(90deg) translateY(-100%);
  }
  :global(.textLayer[data-main-rotation='180']) {
    transform: rotate(180deg) translate(-100%, -100%);
  }
  :global(.textLayer[data-main-rotation='270']) {
    transform: rotate(270deg) translateX(-100%);
  }

  /* ── On a phone ─────────────────────────────────────────────────────
     Pages as wide as the screen allows, and a pill of zoom buttons a
     finger can hit, clear of the gesture bar. */
  :global([data-phone]) .pages {
    padding: 4px 8px 96px;
    overscroll-behavior: contain;
  }
  :global([data-phone]) .pages.bare {
    padding-bottom: 24px;
  }
  :global([data-phone]) .controls {
    bottom: calc(var(--safe-bottom) + 16px);
    padding: 4px 8px;
  }
  :global([data-phone]) .controls button {
    min-width: 44px;
    height: 44px;
  }
  :global([data-phone]) .controls .zoom {
    min-width: 60px;
  }
  :global([data-phone]) .controls .count {
    white-space: nowrap;
  }
</style>
