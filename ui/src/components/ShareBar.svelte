<script lang="ts">
  // Part-to-whole, as one horizontal stacked bar with a keyed list under it.
  //
  // Not a pie and not a donut. The question is "what proportion of the week
  // went where", and a stacked bar answers it while staying readable at three
  // segments and at eight -- a ring does neither, and comparing two similar
  // slices of a ring is the classic thing people cannot do.
  //
  // The list below the bar is doing three jobs at once and is the reason this
  // works: it is the legend (identity, never colour alone), the direct labels
  // (which will not fit inside a 6% segment), and the table view (every value
  // is readable without hovering anything). That is why no percentage is ever
  // written inside a segment: a label clipped by its own mark is worse than
  // no label.
  //
  // Segments are separated by a 2px gap in the surface colour rather than by
  // a stroke -- the same spacer the rest of the application's charts use --
  // so two neighbouring roles with close hues still read as two.

  interface Slice {
    id: string
    name: string
    color: string
    /** Glyph, where the entity has one. Beside the swatch, never instead. */
    icon?: string
    value: number
    /** Ready-formatted. What the list shows beside the share. */
    label: string
  }

  interface Props {
    slices: Slice[]
    /** What a zero total should say. */
    empty?: string
  }

  const { slices, empty = 'Nothing recorded yet.' }: Props = $props()

  const rows = $derived(slices.filter((s) => s.value > 0).sort((a, b) => b.value - a.value))
  const total = $derived(rows.reduce((sum, s) => sum + s.value, 0))

  /**
   * At most six segments, and the tail folded into one.
   *
   * Past about six classes adjacent shares stop being distinguishable, and
   * the honest answer to "too many categories" is never more colours. The
   * fold is named "Everything else" rather than dropped, because a share
   * chart that quietly omits a slice adds up to less than the week.
   */
  const CAP = 6
  const shown = $derived.by(() => {
    if (rows.length <= CAP) return rows
    const head = rows.slice(0, CAP - 1)
    const tail = rows.slice(CAP - 1)
    const value = tail.reduce((sum, s) => sum + s.value, 0)
    return [
      ...head,
      {
        id: '__rest',
        name: `${tail.length} others`,
        color: 'var(--fg-faint)',
        value,
        label: tail.map((s) => s.name).join(', '),
      },
    ]
  })

  const share = (value: number) => (total === 0 ? 0 : Math.round((value / total) * 100))
</script>

{#if total === 0}
  <p class="none">{empty}</p>
{:else}
  <div class="stack" role="img" aria-label="Share by role">
    {#each shown as slice (slice.id)}
      <span
        class="seg"
        style="flex: {slice.value}; background: {slice.color}"
        title="{slice.name} — {slice.label} ({share(slice.value)}%)"
      ></span>
    {/each}
  </div>

  <ul class="key">
    {#each shown as slice (slice.id)}
      <li>
        <span class="swatch" style="background: {slice.color}"></span>
        {#if slice.icon}<span class="glyph">{slice.icon}</span>{/if}
        <span class="name">{slice.name}</span>
        <span class="pct">{share(slice.value)}%</span>
        <span class="amount">{slice.label}</span>
      </li>
    {/each}
  </ul>
{/if}

<style>
  .stack {
    display: flex;
    /* The gap *is* the separator. A border round each segment would add ink
       that is not data, and would double up where two segments meet. */
    gap: 2px;
    height: 14px;
    border-radius: 999px;
    overflow: hidden;
  }

  .seg {
    min-width: 3px;
  }

  .key {
    display: grid;
    gap: var(--sp-1);
    margin: var(--sp-3) 0 0;
    padding: 0;
    list-style: none;
  }

  .key li {
    display: grid;
    grid-template-columns: auto auto 1fr auto auto;
    align-items: baseline;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    /* Text tokens throughout. The swatch carries the identity; a name
       written in a pale role colour would fail contrast on the surface. */
    color: var(--fg-muted);
  }

  .swatch {
    width: 9px;
    height: 9px;
    border-radius: 2px;
    translate: 0 -1px;
  }

  .glyph {
    font-size: var(--text-xs);
    line-height: 1;
  }

  .name {
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
  }

  .none {
    margin: 0;
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }
</style>
