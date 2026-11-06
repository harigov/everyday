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

  import type {
    PDFDocumentLoadingTask,
    PDFDocumentProxy,
    RenderTask,
  } from 'pdfjs-dist/legacy/build/pdf.mjs'
  import Icon from './Icon.svelte'

  interface Props {
    /** The file's bytes. pdf.js takes them over. */
    data: Uint8Array
    onfail: (message: string) => void
  }
  let { data, onfail }: Props = $props()

  let pdf = $state.raw<PDFDocumentProxy | null>(null)
  /** Every page's width over its height, in order: the first page's for any
   *  not yet drawn. */
  let shapes = $state.raw<number[]>([])
  let zoom = $state(1)
  let available = $state(0)
  let current = $state(1)
  let scroller = $state<HTMLElement | null>(null)

  const ZOOMS = [0.5, 0.67, 0.8, 1, 1.25, 1.5, 2, 3]
  /** The widest a page is drawn at 100%: a page as wide as a big screen is
   *  harder to read, not easier. */
  const WIDEST = 880
  /** A canvas is never given more pixels than this, whatever the zoom. */
  const MOST_PIXELS = 4096 * 4096

  const pageWidth = $derived(Math.max(120, Math.min(available - 32, WIDEST)) * zoom)

  $effect(() => {
    const bytes = data
    let closed = false
    let task: PDFDocumentLoadingTask | null = null
    void (async () => {
      try {
        const { openPdf } = await import('../lib/pdf')
        if (closed) return
        task = openPdf(bytes)
        const doc = await task.promise
        if (closed) return
        const first = (await doc.getPage(1)).getViewport({ scale: 1 })
        if (closed) return
        shapes = Array<number>(doc.numPages).fill(first.width / first.height)
        pdf = doc
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

  function watch(canvas: HTMLCanvasElement, onchange: (near: boolean) => void) {
    observer ??= new IntersectionObserver(
      (entries) => {
        for (const entry of entries) watching.get(entry.target)?.(entry.isIntersecting)
      },
      { root: scroller, rootMargin: '900px 0px' },
    )
    watching.set(canvas, onchange)
    observer.observe(canvas)
  }

  function unwatch(canvas: HTMLCanvasElement) {
    watching.delete(canvas)
    observer?.unobserve(canvas)
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
  }

  /** Draws page `n` onto `canvas` while it is near the view, at `width` CSS
   *  pixels. */
  function page(canvas: HTMLCanvasElement, initial: { n: number; width: number }) {
    let at = initial
    let near = false
    let drawnWidth = 0
    let task: RenderTask | null = null
    /** Bumped by every paint and forget, so one that finishes late can tell
     *  it has been overtaken. */
    let turn = 0

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
        let scale = (width / natural.width) * (window.devicePixelRatio || 1)
        const pixels = natural.width * natural.height * scale * scale
        if (pixels > MOST_PIXELS) scale *= Math.sqrt(MOST_PIXELS / pixels)
        const viewport = p.getViewport({ scale })
        canvas.width = Math.floor(viewport.width)
        canvas.height = Math.floor(viewport.height)
        task = p.render({ canvas, viewport })
        await task.promise
      } catch {
        // Cancelled for a newer paint, or a page pdf.js could not draw: it
        // stays blank, and the next change of width tries it again.
        if (mine === turn) drawnWidth = 0
      }
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
      void settle().then(() => {
        if (near) return
        // Zero pixels: the memory goes, the box keeps its size from CSS.
        canvas.width = 0
        canvas.height = 0
      })
    }

    watch(canvas, (isNear) => {
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
        unwatch(canvas)
        turn++
        task?.cancel()
      },
    }
  }

  /** Which page is most of the way up the view, for "3 of 12". */
  function track() {
    if (!scroller) return
    const line = scroller.scrollTop + scroller.clientHeight / 3
    const pages = scroller.querySelectorAll<HTMLElement>('canvas')
    let n = 1
    for (const [i, el] of pages.entries()) {
      if (el.offsetTop <= line) n = i + 1
      else break
    }
    current = n
  }

  function step(by: 1 | -1) {
    const i = ZOOMS.findIndex((z) => z >= zoom - 0.001)
    const next = ZOOMS[Math.min(ZOOMS.length - 1, Math.max(0, (i < 0 ? 3 : i) + by))]
    if (next === undefined || next === zoom || !scroller) return
    // Keep the same place in the document in view across the change.
    const share = scroller.scrollTop / Math.max(1, scroller.scrollHeight)
    zoom = next
    requestAnimationFrame(() => {
      if (scroller) scroller.scrollTop = share * scroller.scrollHeight
    })
  }
</script>

<div class="pdf">
  <div
    class="pages"
    bind:this={scroller}
    bind:clientWidth={available}
    onscroll={track}
    tabindex="-1"
  >
    {#if pdf}
      {#each shapes as shape, i (i)}
        <canvas
          class="sheet-of-paper"
          style:width="{pageWidth}px"
          style:height="{pageWidth / shape}px"
          aria-label="Page {i + 1}"
          use:page={{ n: i + 1, width: pageWidth }}
        ></canvas>
      {/each}
    {:else}
      <p class="wait">Opening…</p>
    {/if}
  </div>
  {#if pdf}
    <div class="controls">
      <button
        aria-label="Zoom out"
        title="Zoom out"
        onclick={() => step(-1)}
        disabled={zoom <= 0.5}
      >
        <Icon name="minus" size={15} />
      </button>
      <button class="zoom" title="Fit to the window" onclick={() => (zoom = 1)}>
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
     than off both edges, where its left side could not be. */
  .pages {
    height: 100%;
    overflow: auto;
    padding: 4px 16px 72px;
    outline: none;
  }
  .sheet-of-paper {
    display: block;
    margin: 0 auto 14px;
    background: #ffffff;
    border-radius: 2px;
    box-shadow: 0 1px 3px rgb(0 0 0 / 0.4);
  }
  .wait {
    margin-top: 30vh;
    text-align: center;
    color: rgb(255 255 255 / 0.7);
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
    border-radius: 999px;
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
    border-radius: 999px;
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
</style>
