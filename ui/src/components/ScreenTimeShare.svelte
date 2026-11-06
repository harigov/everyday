<script lang="ts">
  // Where the time went, by category: one horizontal stacked bar and a keyed
  // list under it -- `ShareBar`'s form, which is the house answer to
  // part-to-whole, with one thing that one does not do: a category's row is
  // a button that narrows the whole summary to it.
  //
  // At most six segments, the tail folded into "N others". The folded row
  // and the uncategorised row are not buttons. The fold is several
  // categories at once; and uncategorised is spelt `null`, which is also how
  // the store spells "no filter", so a filter on it could only ever mean
  // "everything" -- and drawing it as picked whenever nothing was would be
  // the phantom selection the chart notes warn about.

  import { formatDuration } from '../lib/screentime'
  import { screentime } from '../lib/screentime.svelte'
  import type { ScreenCategoryTotal } from '../lib/types'

  interface Props {
    totals: ScreenCategoryTotal[]
  }

  const { totals }: Props = $props()

  interface Row {
    id: string
    category: ScreenCategoryTotal['category']
    name: string
    color: string
    secs: number
    /** For the folded tail: who is in it. */
    detail?: string
    pickable: boolean
  }

  const rows = $derived(
    totals
      .filter((t) => t.secs > 0)
      .sort((a, b) => b.secs - a.secs)
      .map((t): Row => ({
        id: t.category ?? '__none',
        category: t.category,
        name: screentime.categoryOf(t.category)?.name ?? 'Uncategorised',
        color: screentime.colorOf(t.category),
        secs: t.secs,
        pickable: t.category !== null,
      })),
  )
  const total = $derived(rows.reduce((sum, r) => sum + r.secs, 0))

  const CAP = 6
  const shown = $derived.by((): Row[] => {
    if (rows.length <= CAP) return rows
    const head = rows.slice(0, CAP - 1)
    const tail = rows.slice(CAP - 1)
    return [
      ...head,
      {
        id: '__rest',
        category: null,
        name: `${tail.length} others`,
        color: 'var(--fg-faint)',
        secs: tail.reduce((sum, r) => sum + r.secs, 0),
        detail: tail.map((r) => r.name).join(', '),
        pickable: false,
      },
    ]
  })

  const share = (secs: number) => (total === 0 ? 0 : Math.round((secs / total) * 100))

  /** Picked only by a real id: `null` is "no filter", never a row. */
  function isPicked(row: Row): boolean {
    return row.pickable && screentime.category !== null && screentime.category === row.category
  }

  function pick(row: Row) {
    if (!row.pickable) return
    screentime.setCategory(isPicked(row) ? null : row.category)
  }
</script>

{#if total === 0}
  <p class="none">Nothing recorded in this range.</p>
{:else}
  <div class="stack" role="img" aria-label="Share of screen time by category">
    {#each shown as row (row.id)}
      <span
        class="seg"
        style:flex-grow={row.secs}
        style:background={row.color}
        title="{row.name} — {formatDuration(row.secs)} ({share(row.secs)}%)"
      ></span>
    {/each}
  </div>

  <ul class="key">
    {#each shown as row (row.id)}
      <li>
        <svelte:element
          this={row.pickable ? 'button' : 'div'}
          class="entry"
          class:pick={row.pickable}
          class:on={isPicked(row)}
          role={row.pickable ? 'button' : undefined}
          aria-pressed={row.pickable ? isPicked(row) : undefined}
          title={row.detail ??
            (row.pickable
              ? isPicked(row)
                ? 'Show every category again'
                : `Show only ${row.name}`
              : undefined)}
          onclick={row.pickable ? () => pick(row) : undefined}
        >
          <span class="swatch" style:background={row.color}></span>
          <span class="name">{row.name}</span>
          <span class="pct">{share(row.secs)}%</span>
          <span class="amount">{formatDuration(row.secs)}</span>
        </svelte:element>
      </li>
    {/each}
  </ul>
{/if}

<style>
  .stack {
    display: flex;
    gap: 2px;
    width: 100%;
    height: 14px;
    border-radius: 999px;
    overflow: hidden;
  }
  .seg {
    flex-basis: 0;
    min-width: 3px;
  }

  .key {
    display: grid;
    gap: 2px;
    margin: var(--sp-3) 0 0;
    padding: 0;
    list-style: none;
  }

  .entry {
    display: grid;
    grid-template-columns: auto 1fr auto 6.5em;
    align-items: baseline;
    gap: var(--sp-2);
    width: 100%;
    padding: 3px var(--sp-2);
    margin: 0 calc(-1 * var(--sp-2));
    box-sizing: content-box;
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    color: var(--fg-muted);
    text-align: left;
  }
  .entry.pick {
    cursor: pointer;
  }
  .entry.pick:hover {
    background: var(--bg-hover);
  }
  .entry.on {
    background: var(--bg-selected);
    color: var(--fg);
  }

  .swatch {
    width: 9px;
    height: 9px;
    border-radius: 2px;
    translate: 0 -1px;
  }
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pct {
    color: var(--fg);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .amount {
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    text-align: right;
  }

  .none {
    margin: 0;
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }
</style>
