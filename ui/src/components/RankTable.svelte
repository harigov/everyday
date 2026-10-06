<script lang="ts">
  // A ranked list, as a table: who or what, a thin bar for the figure it is
  // ranked by, and the figures themselves in columns beside it.
  //
  // A table rather than a bar chart, because every row here is a name worth
  // reading in full -- a sender, a meeting, a person -- and each carries two
  // or three numbers, not one. The bar is there so the eye can see the drop
  // from first to fifth without reading digits; the digits are there so
  // nobody has to estimate a length. One hue for every bar, because the bar
  // is magnitude and the rows are already told apart by their names; where a
  // row is an entity with its own colour (a calendar) that colour is a dot
  // beside the name, never the bar.
  //
  // Real `<table>` markup, so a screen reader walks it as rows and columns,
  // and so the figures line up under their headings without a grid of
  // hand-set widths.

  interface Row {
    id: string
    name: string
    /** A second, quieter line: an address, how long ago. */
    detail?: string
    /** A word beside the name: "Newsletter". */
    tag?: string | null
    /** The entity's own colour, drawn as a dot before the name. */
    dot?: string
    /** What the row is ranked by. Sets the bar's length. */
    value: number
    /** Ready-formatted, one per entry in `columns`. */
    cells: string[]
    /** Makes the row's name a button: open the mail, the person. */
    onpick?: () => void
    /** The button's accessible name, when `onpick` is set. */
    pickLabel?: string
  }

  interface Props {
    /** The name column's heading. */
    head: string
    /** The figure columns' headings, in `cells` order. */
    columns: string[]
    rows: Row[]
    /** How many rows to draw. The rest are counted under the table. */
    limit?: number
    /** What an empty table says instead. */
    empty: string
    /** The bar colour. One hue for every row. */
    color?: string
    /**
     * Which figure column is the one the rows are ranked by, drawn a step
     * stronger. `-1` for none, where the rank is a sum of the columns.
     */
    lead?: number
  }

  const {
    head,
    columns,
    rows,
    limit = 6,
    empty,
    color = 'var(--accent)',
    lead = 0,
  }: Props = $props()

  const shown = $derived(rows.slice(0, limit))
  const ceiling = $derived(Math.max(1, ...rows.map((r) => r.value)))
</script>

{#if rows.length === 0}
  <p class="empty">{empty}</p>
{:else}
  <table style="--bar: {color}">
    <thead>
      <tr>
        <th scope="col" class="who">{head}</th>
        {#each columns as column (column)}
          <th scope="col" class="num">{column}</th>
        {/each}
      </tr>
    </thead>
    <tbody>
      {#each shown as row (row.id)}
        <tr>
          <th scope="row" class="who">
            <svelte:element
              this={row.onpick ? 'button' : 'div'}
              class="name-cell"
              class:pick={!!row.onpick}
              role={row.onpick ? 'button' : undefined}
              aria-label={row.onpick ? row.pickLabel : undefined}
              onclick={row.onpick}
            >
              <span class="line">
                {#if row.dot}<span class="dot" style="background: {row.dot}"></span>{/if}
                <span class="name" title={row.name}>{row.name}</span>
                {#if row.tag}<span class="tag">{row.tag}</span>{/if}
              </span>
              {#if row.detail}<span class="detail" title={row.detail}>{row.detail}</span>{/if}
              <!-- Decorative: the same figure is in the first column. -->
              <span class="bar" aria-hidden="true">
                <span class="fill" style="width: {(row.value / ceiling) * 100}%"></span>
              </span>
            </svelte:element>
          </th>
          {#each row.cells as cell, i (i)}
            <td class="num" class:lead={i === lead}>{cell}</td>
          {/each}
        </tr>
      {/each}
    </tbody>
  </table>
  {#if rows.length > shown.length}
    <p class="more">and {(rows.length - shown.length).toLocaleString()} more</p>
  {/if}
{/if}

<style>
  .empty {
    margin: 0;
    color: var(--fg-faint);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }

  table {
    width: 100%;
    border-collapse: collapse;
    /* Fixed, so a long name ellipsises inside its column rather than
       pushing the figures off the card. */
    table-layout: fixed;
  }

  th,
  td {
    padding: 0;
    font-weight: inherit;
    text-align: left;
    vertical-align: top;
  }

  thead th {
    padding-bottom: var(--sp-1);
    border-bottom: 1px solid var(--border);
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-weight: 550;
  }

  tbody tr + tr > * {
    border-top: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }

  tbody th,
  tbody td {
    padding-top: var(--sp-2);
    padding-bottom: var(--sp-2);
  }

  .num {
    width: 5.5em;
    padding-left: var(--sp-2);
    text-align: right;
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  thead .num {
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  td.lead {
    color: var(--fg);
    font-weight: 600;
  }

  .name-cell {
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 100%;
    min-width: 0;
    padding: 0;
    text-align: left;
    color: inherit;
    font: inherit;
  }

  .pick {
    cursor: pointer;
  }
  .pick:hover .name {
    text-decoration: underline;
    text-decoration-color: var(--fg-faint);
    text-underline-offset: 2px;
  }

  .line {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }

  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }

  .name,
  .detail {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .name {
    color: var(--fg);
    font-size: var(--text-base);
  }

  .detail {
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  .tag {
    flex: none;
    padding: 0 6px;
    border-radius: 999px;
    background: var(--bg-active);
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }

  /* A real width for the fill's percentage to resolve against -- see the
     chart notes on the bar that drew nothing because its box was
     shrink-to-fit around it. */
  .bar {
    display: block;
    width: 100%;
    height: 4px;
    margin-top: 2px;
  }

  /* Square at the baseline, rounded at the data end. */
  .fill {
    display: block;
    height: 100%;
    min-width: 2px;
    border-radius: 0 2px 2px 0;
    background: var(--bar);
  }

  .more {
    margin: var(--sp-2) 0 0;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
</style>
