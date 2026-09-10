<script lang="ts">
  // One ratio against its limit: a fill in a track of the same hue.
  //
  // A meter rather than a two-slice pie or a one-bar chart. The reader's job
  // is "how far along is this", which is a single magnitude against a known
  // whole, and the track is the whole drawn in a lighter step of the fill's
  // own colour so the state reads across the entire bar rather than only
  // where the fill stops.
  //
  // The number is always written beside it. A bar is a shape somebody
  // estimates; the percentage is the value, and a meter whose only reading is
  // its own length gates the information behind eyesight.

  interface Props {
    label: string
    /** `0..=1`. Clamped, because a count can exceed its own total. */
    value: number
    /** The colour of the thing being measured. */
    color?: string
    /** One line under the bar: what the ratio is of. */
    note?: string
    /** Called when the row is chosen. Makes the whole row a button. */
    onpick?: () => void
  }

  const { label, value, color = 'var(--accent)', note, onpick }: Props = $props()

  const pct = $derived(Math.round(Math.min(1, Math.max(0, value)) * 100))
</script>

<svelte:element
  this={onpick ? 'button' : 'div'}
  class="meter"
  class:pressable={!!onpick}
  style="--c: {color}"
  role={onpick ? 'button' : undefined}
  onclick={onpick}
>
  <span class="top">
    <span class="name">{label}</span>
    <span class="pct">{pct}%</span>
  </span>
  <span class="track">
    <span class="fill" style="width: {pct}%"></span>
  </span>
  {#if note}<span class="note">{note}</span>{/if}
</svelte:element>

<style>
  .meter {
    display: flex;
    flex-direction: column;
    gap: 3px;
    width: 100%;
    min-width: 0;
    text-align: left;
    padding: 0;
  }

  .pressable {
    border-radius: var(--radius-sm);
    padding: var(--sp-1) var(--sp-2);
    margin: 0 calc(var(--sp-2) * -1);
    width: calc(100% + var(--sp-4));
    transition: background var(--fast) var(--ease);
  }
  .pressable:hover {
    background: var(--bg-hover);
  }

  .top {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
  }

  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    color: var(--fg);
  }

  .pct {
    flex: none;
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  /* The unfilled part is a lighter step of the fill's own hue rather than a
     neutral grey, so an empty meter still says which thing it belongs to. */
  .track {
    display: block;
    height: 6px;
    border-radius: 999px;
    background: color-mix(in oklab, var(--c) 16%, transparent);
    overflow: hidden;
  }

  .fill {
    display: block;
    height: 100%;
    border-radius: 999px;
    background: var(--c);
    transition: width var(--med) var(--ease);
  }

  .note {
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
</style>
