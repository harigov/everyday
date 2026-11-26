<script lang="ts" module>
  /** Where the last view of a document was when it was taken down, so that
   *  one put back (Settings is drawn in the reader's place, and closing it
   *  builds the reader again) opens where the person was reading and not at
   *  the last place they were sent. `seq` is the jump it had obeyed. */
  let resume: { file: string; seq: number; block: number } | null = null
</script>

<script lang="ts">
  // An EPUB, Markdown, text or web page, drawn block by block from what the
  // service parsed, in a column made to be read: a measure of about 38em, the
  // reading face, theme colours.
  //
  // # What is on the page
  //
  // A book is up to a few hundred chapters, and drawing all of them is a
  // hundred thousand elements, each with its pictures. So a chapter is drawn
  // only when it is within a screen or two of the view, and is a blank box of
  // about the right height otherwise -- its measured height once it has been
  // drawn, an estimate from its block count before. A few either side of the
  // one in view are kept, so that going back a page is not a load; the rest
  // are let go.
  //
  // A chapter that changes height above the view (it was an estimate, and now
  // it is the real thing; or a picture in it has arrived) would push what is
  // being read out of sight, and this webview has no scroll anchoring to say
  // otherwise. So every chapter is watched, and a change in one wholly above
  // the view is taken out of the scroll position at once, before it is
  // painted.
  //
  // # Places
  //
  // Every block is an element with `data-block` -- its index in the
  // document's reading order, the address the assistant reads by and a search
  // hit names -- and `data-page`. What is reported (`reader.reportView`) is
  // the block at the top of the view and the page of the block at its foot;
  // what is selected is the blocks it covers. An internal link in the text
  // is an `<a data-block>` too, which is why a block is always looked for
  // as `[data-block]:not(a)`.

  import { tick, untrack } from 'svelte'
  import { openExternal } from '../lib/open-external'
  import { reader } from '../lib/reader.svelte'
  import type { DocBlockView, DocChapterView, ReaderSelection } from '../lib/types'

  /** About how tall a block is, for a chapter not yet drawn. */
  const PER_BLOCK = 78
  const SHORTEST = 160
  /** How far under the top of the view a block asked for is put. */
  const TOP_GAP = 32
  /** Chapters kept drawn either side of the one in view. */
  const KEEP = 3
  /** The most chapters asked for at once. */
  const MOST_LOADS = 10

  const outline = $derived(reader.outline)
  const chapters = $derived(outline?.chapters ?? [])

  let scroller = $state<HTMLElement | null>(null)
  /** Chapters this view has had back, by index; `reader.chapters` has them as
   *  well, and either is looked in. */
  let loaded = $state.raw<Record<number, DocChapterView>>({})
  /** The chapters drawn in full. */
  let mounted = $state.raw<ReadonlySet<number>>(new Set())

  function view(i: number): DocChapterView | null {
    return loaded[i] ?? reader.chapters[i] ?? null
  }

  function showing(i: number): boolean {
    return mounted.has(i) && view(i) !== null
  }

  function blocksIn(i: number): DocBlockView[] {
    return view(i)?.blocks ?? []
  }

  // ── Loading ─────────────────────────────────────────────────────────

  const inflight = new Map<number, Promise<DocChapterView | null>>()
  const failed = new Set<number>()

  /** Chapter `i`, asked for once however many want it; null when it cannot
   *  be had, which is not asked about again until somebody jumps there. */
  function load(i: number): Promise<DocChapterView | null> {
    const have = view(i)
    if (have) return Promise.resolve(have)
    if (failed.has(i)) return Promise.resolve(null)
    let asked = inflight.get(i)
    if (!asked) {
      asked = reader.loadChapter(i).then(
        (got) => {
          if (got) loaded = { ...loaded, [i]: got }
          else failed.add(i)
          return got
        },
        () => {
          failed.add(i)
          return null
        },
      )
      void asked.finally(() => inflight.delete(i))
      inflight.set(i, asked)
    }
    return asked
  }

  // ── What is drawn ───────────────────────────────────────────────────

  /** The chapters within reach of the view, as the observer last said. */
  const near = new Set<number>()
  /** The chapter at the top of the view. */
  let current = 0
  /** A chapter a jump is bringing in, drawn until the observer has caught up. */
  let pinned: number | null = null
  /** Nothing is drawn until the place the document opens at is known, or the
   *  first chapter would be loaded for nothing. */
  let settled = false

  function reconcile() {
    if (!settled) return
    if (pinned !== null && near.has(pinned)) pinned = null
    const count = chapters.length
    const wanted = new Set<number>()
    for (const i of near) {
      for (const j of [i - 1, i, i + 1]) if (j >= 0 && j < count) wanted.add(j)
    }
    if (pinned !== null) wanted.add(pinned)
    const nearest = [...wanted]
      .sort((a, b) => Math.abs(a - current) - Math.abs(b - current))
      .slice(0, MOST_LOADS)
    for (const i of nearest) if ((chapters[i]?.blocks ?? 0) > 0) void load(i)

    const next = new Set<number>(near)
    if (pinned !== null) next.add(pinned)
    for (const i of mounted) if (Math.abs(i - current) <= KEEP) next.add(i)
    if (next.size !== mounted.size || [...next].some((i) => !mounted.has(i))) mounted = next
  }

  /** The height of chapter `i` while it is not drawn. */
  const measured = new Map<number, number>()
  function heightOf(i: number, blocks: number): number {
    const known = measured.get(i)
    if (known !== undefined) return known
    return blocks === 0 ? 0 : Math.max(SHORTEST, blocks * PER_BLOCK)
  }

  // One observer says which chapters are near; another says when one changed
  // height. Both are made when the first chapter's box is, and watch every
  // chapter, drawn or not, from then on.
  let nearObserver: IntersectionObserver | null = null
  let sizeObserver: ResizeObserver | null = null
  const sizes = new Map<number, number>()

  function chapterOf(el: Element): number {
    return Number((el as HTMLElement).dataset.chapter)
  }

  function sized(entries: ResizeObserverEntry[]) {
    const box = scroller
    const changes: { i: number; el: Element; delta: number }[] = []
    for (const entry of entries) {
      const i = chapterOf(entry.target)
      const height = entry.borderBoxSize[0]?.blockSize ?? entry.contentRect.height
      const before = sizes.get(i)
      sizes.set(i, height)
      if (showing(i)) measured.set(i, height)
      if (before !== undefined && before !== height) {
        changes.push({ i, el: entry.target, delta: height - before })
      }
    }
    if (!box || changes.length === 0) return
    // Each box is where it was plus every change above it, its own included.
    // One that ended wholly above the view before it changed is what the
    // view has to be moved back by.
    changes.sort((a, b) => a.i - b.i)
    const top = box.getBoundingClientRect().top
    let above = 0
    let adjust = 0
    for (const change of changes) {
      above += change.delta
      const was = change.el.getBoundingClientRect().bottom - above
      if (was <= top + 1) adjust += change.delta
    }
    if (adjust !== 0) box.scrollTop += adjust
  }

  /** Watch chapter `i`'s box. */
  function watch(el: HTMLElement, i: number) {
    nearObserver ??= new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          const n = chapterOf(entry.target)
          if (entry.isIntersecting) near.add(n)
          else near.delete(n)
        }
        reconcile()
      },
      { root: el.closest<HTMLElement>('[data-flow]'), rootMargin: '1400px 0px' },
    )
    sizeObserver ??= new ResizeObserver(sized)
    nearObserver.observe(el)
    sizeObserver.observe(el)
    return {
      destroy() {
        nearObserver?.unobserve(el)
        sizeObserver?.unobserve(el)
        near.delete(i)
        sizes.delete(i)
      },
    }
  }

  $effect(() => () => {
    nearObserver?.disconnect()
    sizeObserver?.disconnect()
    nearObserver = null
    sizeObserver = null
    if (looking) clearTimeout(looking)
    if (choosing) clearTimeout(choosing)
  })

  // ── Going to a place ────────────────────────────────────────────────

  /** The chapter a block is in. */
  function chapterHolding(block: number): number {
    let lo = 0
    let hi = chapters.length - 1
    let found = -1
    while (lo <= hi) {
      const mid = (lo + hi) >> 1
      if ((chapters[mid]?.firstBlock ?? 0) <= block) {
        found = mid
        lo = mid + 1
      } else hi = mid - 1
    }
    return found
  }

  /** The first block on page `page`. A page is wherever the blocks say it is,
   *  and only the chapters know; so they are searched, a few loads, narrowed
   *  by the contents: a section that starts before the page bounds the
   *  search from below, one that starts on it or after from above. */
  async function firstBlockOn(page: number): Promise<number | null> {
    const sections = outline?.sections ?? []
    let lo = 0
    let hi = chapters.length - 1
    for (const section of sections) {
      if (section.page < page) lo = Math.max(lo, chapterHolding(section.block))
      else {
        hi = Math.min(hi, Math.max(lo, chapterHolding(section.block)))
        break
      }
    }
    while (lo < hi) {
      const mid = (lo + hi) >> 1
      if ((chapters[mid]?.blocks ?? 0) === 0) {
        lo = mid + 1
        continue
      }
      const got = await load(mid)
      const last = got?.blocks[got.blocks.length - 1]
      if (!last || last.page < page) lo = mid + 1
      else hi = mid
    }
    const got = await load(lo)
    const hit = got?.blocks.find((b) => b.page >= page) ?? got?.blocks[got.blocks.length - 1]
    return hit?.index ?? null
  }

  /** Counts jumps, so one overtaken by a newer while it waited gives up. */
  let jumps = 0

  async function jump(place: { page?: number; block?: number | null }, flash: boolean) {
    const mine = ++jumps
    let block = place.block ?? null
    if (block === null && place.page !== undefined) block = await firstBlockOn(place.page)
    if (mine !== jumps || block === null) return
    const index = chapterHolding(block)
    if (index < 0) return
    failed.delete(index)
    pinned = index
    const got = await load(index)
    if (mine !== jumps || !got) return
    if (!mounted.has(index)) mounted = new Set([...mounted, index])
    await tick()
    if (mine !== jumps) return
    bring(block, index, flash)
  }

  /** Scroll so that `block` is at the top of the view. */
  function bring(block: number, index: number, flash: boolean) {
    const box = scroller
    if (!box) return
    const el = box.querySelector<HTMLElement>(`[data-block="${block}"]:not(a)`)
    const at = el ?? box.querySelector<HTMLElement>(`[data-chapter="${index}"]`)
    if (!at) return
    box.scrollTop += at.getBoundingClientRect().top - box.getBoundingClientRect().top - TOP_GAP
    if (el && flash) {
      // So that where a search hit or a link led is seen, not hunted for.
      el.classList.remove('landed')
      void el.offsetWidth
      el.classList.add('landed')
      el.addEventListener('animationend', () => el.classList.remove('landed'), { once: true })
    }
  }

  /** The target last obeyed. */
  let obeyed = -1
  let begun = false

  // Where the document opens: the place it was left at if this view was put
  // back, else the jump asked for, else the top.
  $effect(() => {
    if (begun || !scroller || chapters.length === 0 || !outline) return
    begun = true
    const file = outline.file
    untrack(() => void begin(file))
  })

  /** `n` frames from now, or a little over that in time when the window is
   *  hidden and none is being drawn. */
  function frames(n: number): Promise<void> {
    return new Promise((done) => {
      const fallback = setTimeout(done, 100 * n)
      let left = n
      const step = () => {
        if (--left > 0) {
          requestAnimationFrame(step)
          return
        }
        clearTimeout(fallback)
        done()
      }
      requestAnimationFrame(step)
    })
  }

  async function begin(file: string) {
    const target = reader.target
    let place: { page?: number; block?: number | null } | null = null
    if (resume && resume.file === file && resume.seq === (target?.seq ?? -1)) {
      place = { block: resume.block }
      obeyed = resume.seq
    } else if (target) {
      // A document opened at its first page is at its top already, and asking
      // where page 1 begins would load chapters to be told so.
      if (target.block != null || target.page > 1) {
        place = { page: target.page, block: target.block }
      }
      obeyed = target.seq
    } else if (reader.block !== null) {
      place = { block: reader.block }
    }
    if (place) {
      await jump(place, false)
      // Let the observer say what is near the place arrived at, not the top
      // it was looking at a moment ago.
      await frames(2)
    }
    settled = true
    reconcile()
    watching()
    scroller?.focus({ preventScroll: true })
  }

  // Later jumps.
  $effect(() => {
    const target = reader.target
    if (!begun || !target || target.seq === obeyed) return
    obeyed = target.seq
    untrack(() => void jump(target, true))
  })

  // ── Where the view is ───────────────────────────────────────────────

  let looking: ReturnType<typeof setTimeout> | null = null
  let told = ''

  /** The block at the top of the view and the page at its foot, told to the
   *  store a few times a second at most. */
  function watching() {
    if (looking) return
    looking = setTimeout(look, 140)
  }

  function elementOf(node: Node): Element | null {
    return node instanceof Element ? node : node.parentElement
  }

  function ownBlock(el: Element | null): HTMLElement | null {
    return el?.closest<HTMLElement>('[data-block]:not(a)') ?? null
  }

  /** The block under the first point, going down (or up, for a foot) in
   *  steps from `from`, that is one of this view's. */
  function blockFrom(box: HTMLElement, x: number, from: number, by: 1 | -1): HTMLElement | null {
    for (let dy = 6; dy < 220; dy += 14) {
      const hit = document.elementFromPoint(x, from + by * dy)
      if (!hit || !box.contains(hit)) continue
      const found = ownBlock(hit)
      if (found) return found
    }
    return null
  }

  function look() {
    looking = null
    const box = scroller
    const file = outline?.file
    if (!settled || !box || !file) return
    pinned = null
    const rect = box.getBoundingClientRect()
    const x = rect.left + rect.width / 2
    const first = blockFrom(box, x, rect.top, 1)
    // The chapter being read decides which are kept; where it is not a drawn
    // block (a blank chapter box, in a fast scroll) the box itself says.
    const holder = (first ?? document.elementFromPoint(x, rect.top + 8))?.closest('[data-chapter]')
    if (holder instanceof HTMLElement) current = chapterOf(holder)
    reconcile()
    if (!first) return
    const block = Number(first.dataset.block)
    const page = Number(first.dataset.page)
    if (!Number.isFinite(block) || !Number.isFinite(page)) return
    const foot = Number(blockFrom(box, x, rect.bottom, -1)?.dataset.page)
    const lastPage = Number.isFinite(foot) && foot > page ? foot : null
    resume = { file, seq: obeyed, block }
    const key = `${block}|${page}|${lastPage}`
    if (key === told) return
    told = key
    reader.reportView({ page, lastPage, block })
  }

  // ── What is selected ────────────────────────────────────────────────

  let choosing: ReturnType<typeof setTimeout> | null = null
  let chosen = ''

  /** The blocks a range touches, first to last. A drag ends in a block, but
   *  a triple click or Select All can end between two, on the column itself,
   *  and then the blocks are asked one by one. */
  function blocksOf(
    range: Range,
    box: HTMLElement,
  ): { from: number; to: number; page: number } | null {
    const start = ownBlock(elementOf(range.startContainer))
    let end = ownBlock(elementOf(range.endContainer))
    if (start && end) {
      // A selection that ends at the very start of the next block, as a
      // triple click's does, has not touched it.
      if (end !== start && range.endOffset === 0 && end.previousElementSibling) {
        const before = ownBlock(end.previousElementSibling)
        if (before) end = before
      }
      return {
        from: Number(start.dataset.block),
        to: Number(end.dataset.block),
        page: Number(start.dataset.page),
      }
    }
    let from = Infinity
    let to = -Infinity
    let page = 0
    for (const el of box.querySelectorAll<HTMLElement>('[data-block]:not(a)')) {
      if (!range.intersectsNode(el)) continue
      const n = Number(el.dataset.block)
      if (n < from) {
        from = n
        page = Number(el.dataset.page)
      }
      if (n > to) to = n
    }
    return Number.isFinite(from) && Number.isFinite(to) ? { from, to, page } : null
  }

  function readSelection(): ReaderSelection | null {
    const box = scroller
    const selection = document.getSelection()
    if (!box || !selection || selection.isCollapsed || selection.rangeCount === 0) return null
    const range = selection.getRangeAt(0)
    if (!box.contains(range.commonAncestorContainer)) return null
    const got = blocksOf(range, box)
    if (!got || ![got.from, got.to, got.page].every(Number.isFinite)) return null
    return { page: got.page, from: Math.min(got.from, got.to), to: Math.max(got.from, got.to) }
  }

  function selectionChanged() {
    const now = readSelection()
    const key = now ? `${now.page}|${now.from}|${now.to}` : ''
    if (key === chosen) return
    chosen = key
    reader.setSelection(now)
  }

  // The selection moves with every character of a drag; it is read when it
  // settles. Cleared when the view goes, so a selection in a document that
  // has been closed is not still on the assistant's screen.
  $effect(() => {
    const soon = () => {
      if (choosing) clearTimeout(choosing)
      choosing = setTimeout(selectionChanged, 160)
    }
    document.addEventListener('selectionchange', soon)
    return () => {
      document.removeEventListener('selectionchange', soon)
      if (chosen !== '') reader.setSelection(null)
    }
  })

  // ── Links ───────────────────────────────────────────────────────────

  /** A link in the text: inside the document, a jump; outside it, the
   *  system's browser. Never a navigation of this window. */
  function clicked(e: MouseEvent) {
    const link = e.target instanceof Element ? e.target.closest('a') : null
    if (!link || !scroller?.contains(link)) return
    e.preventDefault()
    const to = link.getAttribute('data-block')
    if (to !== null && /^\d+$/.test(to)) {
      reader.goTo({ block: Number(to) })
      return
    }
    const href = link.getAttribute('href') ?? ''
    if (/^(https?:|mailto:)/i.test(href)) void openExternal(href)
  }

  /** What a picture is described as, as plain text: its words arrive as the
   *  service's own escaped HTML, which is read here by an inert template and
   *  never put in the page. */
  function plain(html: string): string {
    const holder = document.createElement('template')
    holder.innerHTML = html
    return holder.content.textContent ?? ''
  }

  /** The heading element for a level, kept within `h1`..`h6`. */
  function tag(level: number | undefined): string {
    return `h${Math.min(6, Math.max(1, Math.round(level ?? 2)))}`
  }

  /** Where the first chapter's blocks arrive, say so: the view has something
   *  to report as soon as there is a block under its top. */
  $effect(() => {
    void mounted
    void loaded
    void tick().then(watching)
  })

  /** Put the keyboard on the text, so the arrow keys and space scroll it. */
  export function focus() {
    scroller?.focus({ preventScroll: true })
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div
  class="flow"
  data-flow
  bind:this={scroller}
  tabindex="-1"
  onscroll={watching}
  onclick={clicked}
>
  <div class="column">
    {#each chapters as chapter, i (i)}
      <section
        class="chapter"
        data-chapter={i}
        use:watch={i}
        style:height={showing(i) ? undefined : `${heightOf(i, chapter.blocks)}px`}
      >
        {#if showing(i)}
          <!-- Every block's inline markup is HTML the service wrote itself
               from a fixed set of elements, with every character of the
               file's own text escaped: safe to put in the page as it is. -->
          <!-- eslint-disable svelte/no-at-html-tags -->
          {#each blocksIn(i) as b (b.index)}
            {#if b.kind === 'heading'}
              <svelte:element this={tag(b.level)} data-block={b.index} data-page={b.page}>
                {@html b.html}
              </svelte:element>
            {:else if b.kind === 'quote'}
              <blockquote data-block={b.index} data-page={b.page}>{@html b.html}</blockquote>
            {:else if b.kind === 'listItem'}
              <p class="item" data-block={b.index} data-page={b.page}>{@html b.html}</p>
            {:else if b.kind === 'code'}
              <pre data-block={b.index} data-page={b.page}>{@html b.html}</pre>
            {:else if b.kind === 'rule'}
              <hr data-block={b.index} data-page={b.page} />
            {:else if b.kind === 'image'}
              {@const description = plain(b.html)}
              <figure data-block={b.index} data-page={b.page}>
                {#if b.src}
                  <img src={b.src} alt={description} loading="lazy" draggable="false" />
                {:else}
                  <figcaption class="absent">
                    {description ? `Picture: ${description}` : 'A picture, not shown here.'}
                  </figcaption>
                {/if}
              </figure>
            {:else}
              <p data-block={b.index} data-page={b.page}>{@html b.html}</p>
            {/if}
          {/each}
          <!-- eslint-enable svelte/no-at-html-tags -->
        {/if}
      </section>
    {:else}
      <p class="empty">This document has no text to show.</p>
    {/each}
  </div>
</div>

<style>
  /* As tall as the stage by a stated height, not stretched: see
     `.shell-stage` in app.css. `overflow-anchor` off, because the chapters
     correct the scroll position themselves (above), and a webview that also
     anchored would correct it twice. */
  .flow {
    height: 100%;
    overflow-y: auto;
    overflow-anchor: none;
    padding: var(--sp-6) var(--sp-6) var(--sp-16);
    outline: none;
    -webkit-user-select: text;
    user-select: text;
  }
  .column {
    max-width: 38em;
    margin-inline: auto;
    font-family: var(--font-read);
    font-size: var(--text-prose);
    line-height: 1.6;
    color: var(--fg);
    overflow-wrap: break-word;
  }
  /* A formatting context of its own, so that a chapter's first and last
     margins are inside its height: the height measured for a chapter that is
     put away is then the height of the box that stands in for it. */
  .chapter {
    display: flow-root;
  }
  .chapter + .chapter {
    margin-top: 2em;
  }
  .empty {
    margin: var(--sp-10) 0;
    text-align: center;
    color: var(--fg-subtle);
    font-size: var(--text-base);
    font-family: var(--font-ui);
  }

  /* ── The blocks ─────────────────────────────────────────────────── */

  p {
    margin: 0 0 0.85em;
  }
  /* A list item is a paragraph with a dot hung in the margin: the service
     gives items one at a time, without the list they came from. */
  .item {
    position: relative;
    margin-bottom: 0.35em;
    padding-inline-start: 1.5em;
  }
  .item::before {
    content: '';
    position: absolute;
    left: 0.55em;
    top: 0.72em;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: currentColor;
    opacity: 0.6;
  }
  .item + p:not(.item) {
    margin-top: 0.65em;
  }
  blockquote {
    margin: 0 0 0.85em;
    padding-inline-start: 1em;
    border-inline-start: 3px solid var(--border-strong);
    color: var(--fg-muted);
  }
  pre {
    margin: 0 0 1em;
    padding: var(--sp-3) var(--sp-4);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    tab-size: 4;
  }
  hr {
    width: 30%;
    height: 1px;
    margin: 2em auto;
    border: 0;
    background: var(--border-strong);
  }
  figure {
    margin: 1.2em 0;
    text-align: center;
  }
  figure img {
    max-width: 100%;
    height: auto;
    border-radius: var(--radius-sm);
  }
  .absent {
    font-family: var(--font-ui);
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
  h1,
  h2,
  h3,
  h4,
  h5,
  h6 {
    margin: 1.6em 0 0.5em;
    font-weight: 700;
    line-height: var(--leading-tight);
    letter-spacing: -0.015em;
    text-wrap: balance;
  }
  h1 {
    font-size: var(--text-2xl);
  }
  h2 {
    font-size: var(--text-xl);
  }
  h3 {
    font-size: var(--text-lg);
  }
  h4,
  h5,
  h6 {
    font-size: var(--text-md);
  }
  h5,
  h6 {
    font-weight: 650;
  }
  /* The first thing in a chapter is not pushed down by a margin that has
     nothing above it to separate from. */
  .chapter > :global(:first-child) {
    margin-top: 0;
  }

  /* The inline markup the service writes, which Svelte did not see. */
  .column :global(em),
  .column :global(i) {
    font-style: italic;
  }
  .column :global(strong),
  .column :global(b) {
    font-weight: 650;
  }
  .column :global(code) {
    font-family: var(--font-mono);
    font-size: 0.88em;
    padding: 0.05em 0.3em;
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
  }
  .column :global(pre code) {
    padding: 0;
    background: none;
  }
  .column :global(sup),
  .column :global(sub) {
    font-size: 0.75em;
    line-height: 0;
  }
  .column :global(a) {
    color: var(--accent);
    cursor: pointer;
  }

  /* Where a jump landed, fading out. Global because the class is put on by
     hand; the keyframes are named globally to match. */
  .column :global(.landed) {
    animation: -global-reader-landed 1.8s var(--ease) 1;
    border-radius: var(--radius-sm);
  }
  @keyframes -global-reader-landed {
    from {
      background: color-mix(in oklab, var(--accent) 24%, transparent);
    }
    to {
      background: transparent;
    }
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* The page is the screen's width: the measure is already short, and the
     margins of a desktop's reading column would take a fifth of it. */
  :global([data-phone]) .flow {
    padding: var(--sp-4) var(--sp-4) var(--sp-12);
  }
  :global([data-phone]) h1 {
    font-size: var(--text-xl);
  }
  :global([data-phone]) h2 {
    font-size: var(--text-lg);
  }
  :global([data-phone]) .column :global(a) {
    padding: 2px 0;
  }
</style>
