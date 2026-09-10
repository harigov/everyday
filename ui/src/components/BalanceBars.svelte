<script lang="ts">
  // Where the week went, one row per role.
  //
  // Rows rather than a stacked column per day, and the reason is not taste.
  // The question is "how much of my week went to each of these", which is a
  // magnitude comparison across categories — bars from a common baseline are
  // the form that answers it, and a stack answers "what is this day made of"
  // instead, which is a different and lesser question.
  //
  // It also settles an accessibility problem the app's palette has. Role
  // colours come from the journal palette, which somebody can also change
  // per role: two of its eight hues (the indigo and the magenta) separate
  // well for normal vision and badly for protanopia. In a stack, colour is
  // the *only* identity channel and that would matter. Here every row is
  // directly labelled with its own name and icon, so the colour is a
  // reinforcement rather than the message.
  //
  // Three things are drawn per row and only one of them is the bar:
  //
  //   actual      solid fill, 4px rounded at the tip, square at the baseline
  //   planned     a hairline rail behind it — the comparison the whole
  //               planned/actual split exists to make
  //   meetings    a lighter segment after a 2px surface gap, never summed
  //               into the actual: an event is somebody's claim on an hour
  //               and a block is your record of one

  import { formatMinutes } from '../lib/format'
  import type { RoleTotals } from '../lib/balance'

  interface Props {
    rows: RoleTotals[]
  }

  const { rows }: Props = $props()

  // One scale for every row, including the planned rail and the meetings, or
  // the bars would not be comparable with each other — which is the only
  // thing they are for.
  const ceiling = $derived(
    Math.max(60, ...rows.map((r) => Math.max(r.actualMinutes + r.eventMinutes, r.plannedMinutes))),
  )

  const pct = (minutes: number) => `${Math.min(100, (minutes / ceiling) * 100)}%`
</script>

<div class="bars">
  {#each rows as row (row.roleId ?? 'none')}
    {@const total = row.actualMinutes + row.eventMinutes}
    {@const quiet = total === 0 && row.plannedMinutes === 0}
    <!-- A row rather than a button. It used to be one, with a `picked` prop
         behind it that opened a per-goal breakdown underneath -- and the
         breakdown now lives in the todo app, beside the goals themselves. It
         also carried a bug worth recording: the selected row was decided by
         `picked === row.roleId`, and the unattributed row's id *is* `null`,
         which is also what "nothing is picked" was spelt as. So the one row
         nobody had chosen was always drawn as chosen. Two meanings, one
         value; the fix was to stop having the state at all. -->
    <div
      class="row"
      class:quiet
      title={[
        row.name,
        `${formatMinutes(row.actualMinutes)} recorded`,
        row.plannedMinutes > 0 ? `${formatMinutes(row.plannedMinutes)} planned` : null,
        row.eventMinutes > 0 ? `${formatMinutes(row.eventMinutes)} in meetings` : null,
      ]
        .filter(Boolean)
        .join(' · ')}
    >
      <span class="label">
        <span class="swatch" style="background: {row.color}"></span>
        {#if row.icon}<span class="glyph">{row.icon}</span>{/if}
        <span class="name">{row.name}</span>
      </span>

      <span class="track">
        <!-- The plan, behind and beneath: a hairline rather than a second
             bar, because it is the reference and not a rival series. -->
        {#if row.plannedMinutes > 0}
          <span class="planned" style="width: {pct(row.plannedMinutes)}"></span>
        {/if}
        <span class="fills">
          {#if row.actualMinutes > 0}
            <span class="actual" style="width: {pct(row.actualMinutes)}; background: {row.color}"
            ></span>
          {/if}
          {#if row.eventMinutes > 0}
            <span class="events" style="width: {pct(row.eventMinutes)}; background: {row.color}"
            ></span>
          {/if}
        </span>
      </span>

      <!-- The value at the tip, on every row: there are six of them, not
           sixty, and the axis this chart does not have would otherwise be
           the only place to read a number. -->
      <span class="value">{total > 0 ? formatMinutes(total) : '—'}</span>
    </div>
  {/each}
</div>

<style>
  .bars {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }

  .row {
    display: grid;
    grid-template-columns: minmax(96px, 168px) 1fr auto;
    align-items: center;
    gap: var(--sp-3);
    width: 100%;
    padding: var(--sp-1) var(--sp-2);
    border-radius: var(--radius-sm);
    text-align: left;
  }

  /* A role with nothing recorded is still drawn — its absence is the
     finding — but it should not compete with the rows that have something
     to say. */
  .row.quiet .label,
  .row.quiet .value {
    opacity: 0.5;
  }

  .label {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    /* Text tokens, never the series colour: a light hue is illegible as
       text, and the swatch beside it carries the identity. */
    color: var(--fg);
    font-size: var(--text-sm);
  }

  .swatch {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 50%;
  }

  .glyph {
    flex: none;
    font-size: var(--text-sm);
    line-height: 1;
  }

  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .track {
    position: relative;
    display: block;
    min-width: 0;
    height: 18px;
  }

  /* `inset: 0`, and the `0 auto 0 0` it used to be is why no bar was ever
     drawn. With `right: auto` this box has no width of its own: it is
     shrink-to-fit around its contents, and its contents are sized as a
     *percentage of it*. CSS calls that cycle and resolves the percentage to
     `auto`, which for these is zero -- so every fill was a zero-width block
     of colour and the chart was six labels and six numbers with nothing
     between them. Stretched to the track, the percentages have something
     real to be a percentage of. */
  .fills {
    position: absolute;
    display: flex;
    /* The 2px surface gap that separates touching marks. No stroke: a
       border would add ink that is not data. */
    gap: 2px;
    inset: 0;
    align-items: center;
    justify-content: flex-start;
    height: 100%;
  }

  .actual,
  .events {
    display: block;
    height: 14px;
    /* Rounded at the data end, square at the baseline. */
    border-radius: 0 4px 4px 0;
  }

  /* Somebody else's meetings, in the same hue at a wash: the same part of a
     life, not the same kind of record. */
  .events {
    opacity: 0.32;
  }

  .planned {
    position: absolute;
    bottom: 0;
    left: 0;
    height: 2px;
    border-radius: 1px;
    background: var(--border-strong, var(--border));
  }

  .value {
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    text-align: right;
  }
</style>
