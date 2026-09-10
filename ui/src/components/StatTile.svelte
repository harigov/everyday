<script lang="ts">
  // A number, and what it is a number of.
  //
  // The form a dashboard reaches for far more often than it reaches for a
  // chart: one current value has no shape to plot, and a one-bar bar chart is
  // the classic way of pretending otherwise. Everything here is text.
  //
  // Proportional figures on the value rather than `tabular-nums`. Tabular
  // gives every digit the width of a zero, which is what you want in a column
  // of numbers that must line up and exactly what you do not want on a
  // standalone 28px figure -- "121" comes out visibly loose.

  interface Props {
    /** Sentence case, no trailing colon. */
    label?: string
    /** The figure itself, already formatted. */
    value: string
    /** One line under it: the comparison, the context, the caveat. */
    note?: string
    /**
     * Whether the note is a warning rather than a fact.
     *
     * A status colour, and reserved for that -- never a fourth series colour.
     * It never travels alone: the words say what is wrong as well.
     */
    warn?: boolean
    /** Drawn under the note. Where a widget wants a button, or a bar. */
    children?: import('svelte').Snippet
  }

  const { label, value, note, warn = false, children }: Props = $props()
</script>

<div class="tile">
  {#if label}<span class="label">{label}</span>{/if}
  <span class="value">{value}</span>
  {#if note}<span class="note" class:warn>{note}</span>{/if}
  {#if children}<div class="extra">{@render children()}</div>{/if}
</div>

<style>
  .tile {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .label {
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }

  .value {
    font-size: var(--text-2xl);
    font-weight: 600;
    line-height: var(--leading-tight);
    color: var(--fg);
    overflow-wrap: anywhere;
  }

  .note {
    color: var(--fg-subtle);
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
  }

  .note.warn {
    color: var(--danger);
  }

  .extra {
    margin-top: var(--sp-2);
  }
</style>
