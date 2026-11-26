<script lang="ts">
  // Searching inside the open document. A field, and under it the places the
  // service found: the page, the section it is in, and a snippet of the words
  // there with what was asked for picked out. Choosing one takes the reader
  // there.
  //
  // The search runs as it is typed, after a moment's pause, and at once on
  // Enter. What the store keeps is the field's text (`reader.query`) and the
  // places (`reader.hits`), so that closing this pane to read and opening it
  // again finds the list where it was left.

  import { focusOnMount } from '../lib/focus'
  import { reader } from '../lib/reader.svelte'
  import type { DocHit, DocSection } from '../lib/types'
  import Icon from './Icon.svelte'

  /** How long typing has to stop for before the search is made. */
  const PAUSE_MS = 250

  let field = $state<HTMLInputElement | null>(null)
  let results = $state<HTMLElement | null>(null)
  /** The text the places on screen were asked for: what to pick out in
   *  them, and what "no matches" is about. */
  let asked = $state(reader.query.trim())
  let picked = $state<number | null>(null)
  let timer: ReturnType<typeof setTimeout> | null = null

  const sections = $derived(reader.outline?.sections ?? [])

  function submit() {
    if (timer) clearTimeout(timer)
    timer = null
    asked = reader.query.trim()
    picked = null
    // As typed, not trimmed: the store puts the text back in the field, and a
    // space at the end of it is somebody about to type the next word.
    void reader.search(reader.query)
  }

  function typed() {
    if (timer) clearTimeout(timer)
    timer = setTimeout(submit, PAUSE_MS)
  }

  $effect(() => () => {
    if (timer) clearTimeout(timer)
  })

  /** Put the caret in the field with its words selected, ready to be
   *  replaced. */
  export function focus() {
    field?.focus()
    field?.select()
  }

  /** The section a hit is in and those it is inside, outermost first, as
   *  "Part One › Chapter 3". The titles are the document's own words and are
   *  drawn as text. */
  function trail(hit: DocHit): string {
    const names: string[] = []
    let at: number | null | undefined = hit.section
    const seen = new Set<number>()
    while (at !== null && at !== undefined && !seen.has(at)) {
      seen.add(at)
      const section: DocSection | undefined = sections[at]
      if (!section) break
      if (section.title) names.unshift(section.title)
      at = section.parent
    }
    return names.join(' › ')
  }

  /** A snippet in pieces: the odd ones are what was asked for. Split on the
   *  words of the query rather than the whole of it, since the search finds
   *  places that have the words and not only the phrase. */
  function pieces(snippet: string, query: string): string[] {
    const words = [...new Set(query.split(/\s+/).filter((w) => w.length > 0))]
      .slice(0, 8)
      .map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'))
    if (words.length === 0) return [snippet]
    return snippet.split(new RegExp(`(${words.join('|')})`, 'gi'))
  }

  function go(hit: DocHit, i: number) {
    picked = i
    // Both: a PDF is drawn by page, anything else by block.
    reader.goTo({ page: hit.page, block: hit.block })
  }

  /** Down from the field into the list, and up and down through it. */
  function keys(e: KeyboardEvent) {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return
    const rows = [...(results?.querySelectorAll<HTMLElement>('button.hit') ?? [])]
    if (rows.length === 0) return
    const at = rows.indexOf(document.activeElement as HTMLElement)
    const next = e.key === 'ArrowDown' ? at + 1 : at - 1
    if (at === -1 && e.key === 'ArrowUp') return
    e.preventDefault()
    if (next < 0) field?.focus()
    else rows[Math.min(rows.length - 1, next)]?.focus()
  }

  const hits = $derived(reader.hits)
  const empty = $derived(
    asked !== '' && !reader.searching && hits.length === 0 && asked === reader.query.trim(),
  )
</script>

<div class="search">
  <div class="top">
    <input
      class="field"
      type="search"
      placeholder="Search this document"
      aria-label="Search this document"
      autocomplete="off"
      spellcheck="false"
      bind:this={field}
      use:focusOnMount
      value={reader.query}
      oninput={(e) => {
        reader.query = e.currentTarget.value
        typed()
      }}
      onkeydown={(e) => {
        if (e.key === 'Enter') submit()
        else keys(e)
      }}
    />
    {#if reader.searching}
      <span class="spin" role="status" aria-label="Searching">
        <Icon name="refresh" size={15} />
      </span>
    {/if}
  </div>

  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="results scroll" bind:this={results} onkeydown={keys}>
    {#if hits.length > 0}
      {#each hits as hit, i (i)}
        {@const where = trail(hit)}
        <button class="hit list-row" class:sel={picked === i} onclick={() => go(hit, i)}>
          <span class="place">
            <span class="page">p. {hit.page}</span>
            {#if where}<span class="where">{where}</span>{/if}
          </span>
          <span class="snippet">
            {#each pieces(hit.snippet, asked) as piece, n (n)}
              <svelte:element this={n % 2 === 1 ? 'mark' : 'span'}>{piece}</svelte:element>
            {/each}
          </span>
        </button>
      {/each}
    {:else if empty}
      <p class="note">No matches.</p>
    {:else if asked === '' && !reader.searching}
      <p class="note">Type to look for words in this document.</p>
    {/if}
  </div>
</div>

<style>
  .search {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .top {
    position: relative;
    flex: none;
    padding: var(--sp-1) var(--sp-3) var(--sp-2);
  }
  .top .field {
    height: 34px;
    padding-right: 34px;
  }
  /* A search in progress: turning, and quiet under reduced motion, where the
     status label still says so. */
  .spin {
    position: absolute;
    top: 50%;
    right: calc(var(--sp-3) + var(--sp-3));
    translate: 0 -55%;
    display: grid;
    color: var(--fg-faint);
  }
  .spin :global(svg) {
    animation: spin 1s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .spin :global(svg) {
      animation: none;
    }
  }

  .results {
    flex: 1;
    min-height: 0;
    padding-bottom: var(--sp-4);
  }
  .hit {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: var(--sp-2) var(--sp-3);
  }
  .place {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .page {
    flex: none;
    font-variant-numeric: tabular-nums;
    font-weight: 600;
    color: var(--fg-muted);
  }
  .where {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .snippet {
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    color: var(--fg);
    overflow-wrap: anywhere;
  }
  mark {
    padding: 0 1px;
    border-radius: 2px;
    background: color-mix(in oklab, var(--accent) 22%, transparent);
    color: inherit;
  }
  .note {
    padding: var(--sp-3) var(--sp-5);
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }

  /* On a phone: a field and hits a finger can use. */
  :global([data-phone]) .top .field {
    height: 44px;
  }
  :global([data-phone]) .hit {
    min-height: 56px;
    padding: var(--sp-3);
  }
</style>
