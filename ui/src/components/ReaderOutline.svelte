<script lang="ts">
  // The open document's contents: its own table of contents, else its
  // headings, as an indented list with the page each starts on. The section
  // being read is marked, and kept in sight as the reading moves down it.
  //
  // A section's title is the document's own words, so it is drawn as text and
  // never as markup.

  import { reader } from '../lib/reader.svelte'

  /** How far each level in is set, and the most it is set: a contents with
   *  six levels is not a staircase off the edge of a narrow pane. */
  const INDENT = 14
  const DEEPEST = 5

  /** The left padding of a row at `level`. */
  function indented(level: number): string {
    const steps = Math.min(DEEPEST, Math.max(0, level - 1))
    return `calc(var(--sp-3) + ${steps * INDENT}px)`
  }

  const outline = $derived(reader.outline)
  const sections = $derived(outline?.sections ?? [])

  /** The section being read: the last one that starts at or before the view.
   *  By the page in a PDF, which has no blocks to say; by the block in
   *  anything else, once the view has reported one. */
  const current = $derived.by(() => {
    if (!outline) return -1
    const byPage = outline.format === 'pdf' || reader.block === null
    let at = -1
    for (const [i, section] of sections.entries()) {
      if (byPage ? section.page <= reader.page : section.block <= (reader.block ?? -1)) at = i
    }
    return at
  })

  let list = $state<HTMLElement | null>(null)

  // Follow the reading, but only when the row is out of the pane: a pane that
  // re-centred itself on every section would slide under the pointer.
  $effect(() => {
    const at = current
    if (at < 0 || !list) return
    list.querySelector(`[data-section="${at}"]`)?.scrollIntoView({ block: 'nearest' })
  })
</script>

{#if sections.length === 0}
  <p class="none">This document has no table of contents.</p>
{:else}
  <nav class="side-nav scroll" bind:this={list} aria-label="Contents">
    {#each sections as section, i (i)}
      <button
        class="side-row"
        class:sel={i === current}
        style:padding-left={indented(section.level)}
        data-section={i}
        aria-current={i === current ? 'location' : undefined}
        title={section.title}
        onclick={() => reader.goTo({ section: i })}
      >
        <span class="side-text">{section.title || 'Untitled'}</span>
        <span class="side-count">{section.page}</span>
      </button>
    {/each}
  </nav>
{/if}

<style>
  /* The list takes what the pane has left and scrolls in it. */
  nav {
    flex: 1;
    min-height: 0;
  }
  .none {
    padding: var(--sp-4) var(--sp-5);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-faint);
  }
</style>
