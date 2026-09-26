<script lang="ts">
  // A tracker's targets, each edited as the sentence it reads as.
  //
  // "At least [3] [days] a [week]", "At most [60] [min] a [day]", "Between
  // [60] and [120] [min] a [week]". The same few controls for every kind of
  // tracker, because a target is one shape: a bound, a tally and a period.
  // Used where a tracker is edited -- a journal's settings -- and where a
  // goal is, in its detail rail.
  //
  // Changes are handed up whole through `onchange` rather than bound: one
  // caller edits a draft it saves on Done, the other saves on every change,
  // and a target half-typed ("Between 60 and") should not be written by
  // either until the field is left.

  import { describeTarget } from '../lib/tracker'
  import { PERIODS } from '../lib/types'
  import type { Period, Target, Tracker } from '../lib/types'
  import Icon from './Icon.svelte'

  let {
    tracker,
    targets,
    onchange,
    addLabel = 'Add a target',
  }: {
    tracker: Pick<Tracker, 'kind' | 'unit' | 'source'>
    targets: Target[]
    onchange: (next: Target[]) => void
    addLabel?: string
  } = $props()

  type Bound = 'min' | 'max' | 'range'

  const boundOf = (t: Target): Bound =>
    t.min != null && t.max != null ? 'range' : t.max != null ? 'max' : 'min'

  /**
   * Which bound each row was set to, where that was chosen rather than read
   * off its numbers. "Between" picked before either number is typed has no
   * numbers to be read off, and would otherwise snap back to "At least".
   */
  let modes = $state<(Bound | undefined)[]>([])
  const modeOf = (i: number): Bound => modes[i] ?? boundOf(targets[i]!)

  /** A check only ever counts days; a derived tracker only ever its value. */
  const fixedTally = $derived(
    tracker.kind === 'check'
      ? 'days'
      : tracker.source && tracker.source.type !== 'manual'
        ? 'value'
        : tracker.kind === 'scale'
          ? 'value'
          : null,
  )

  /** What a value is counted in, for the tally picker and the label. */
  const valueWord = $derived(
    tracker.source?.type === 'finished'
      ? 'done'
      : tracker.kind === 'scale'
        ? 'average'
        : tracker.unit || 'total',
  )

  function update(i: number, patch: Partial<Target>) {
    onchange(targets.map((t, j) => (j === i ? { ...t, ...patch } : t)))
  }

  function setBound(i: number, bound: Bound) {
    modes[i] = bound
    const t = targets[i]!
    const known = t.min ?? t.max ?? null
    if (bound === 'min') update(i, { min: known, max: null })
    else if (bound === 'max') update(i, { min: null, max: known })
    else update(i, { min: t.min ?? t.max ?? null, max: t.max ?? t.min ?? null })
  }

  /** An emptied field is no bound; anything else that is not a number is ignored. */
  function number(text: string): number | null | undefined {
    if (text.trim() === '') return null
    const n = Number(text)
    return Number.isFinite(n) && n >= 0 ? n : undefined
  }

  function add() {
    const tally = fixedTally ?? 'value'
    onchange([...targets, { min: null, max: null, per: 'week', tally }])
  }

  function remove(i: number) {
    modes.splice(i, 1)
    onchange(targets.filter((_, j) => j !== i))
  }

  const PERIOD_WORDS: Record<Period, string> = {
    day: 'a day',
    week: 'a week',
    month: 'a month',
    quarter: 'a quarter',
    year: 'a year',
  }
</script>

<div class="targets">
  {#each targets as t, i (i)}
    {@const bound = modeOf(i)}
    <div class="target">
      <div class="line">
        <select
          aria-label="Bound"
          value={bound}
          onchange={(e) => setBound(i, e.currentTarget.value as Bound)}
        >
          <option value="min">At least</option>
          <option value="max">At most</option>
          <option value="range">Between</option>
        </select>
        {#if bound !== 'max'}
          <input
            class="num"
            type="number"
            min="0"
            step="any"
            aria-label={bound === 'range' ? 'At least' : 'Amount'}
            value={t.min ?? ''}
            onchange={(e) => {
              const n = number(e.currentTarget.value)
              if (n !== undefined) update(i, { min: n })
            }}
          />
        {/if}
        {#if bound === 'range'}<span class="and">and</span>{/if}
        {#if bound !== 'min'}
          <input
            class="num"
            type="number"
            min="0"
            step="any"
            aria-label={bound === 'range' ? 'At most' : 'Amount'}
            value={t.max ?? ''}
            onchange={(e) => {
              const n = number(e.currentTarget.value)
              if (n !== undefined) update(i, { max: n })
            }}
          />
        {/if}
        {#if fixedTally}
          <span class="word">{fixedTally === 'days' ? 'days' : valueWord}</span>
        {:else}
          <select
            aria-label="Counted in"
            value={t.tally}
            onchange={(e) => update(i, { tally: e.currentTarget.value as Target['tally'] })}
          >
            <option value="value">{valueWord}</option>
            <option value="days">days</option>
          </select>
        {/if}
        <select
          aria-label="Period"
          value={t.per}
          onchange={(e) => update(i, { per: e.currentTarget.value as Period })}
        >
          {#each PERIODS as p (p)}<option value={p}>{PERIOD_WORDS[p]}</option>{/each}
        </select>
        <button
          class="drop"
          title="Remove this target"
          aria-label="Remove"
          onclick={() => remove(i)}
        >
          <Icon name="close" size={13} />
        </button>
      </div>
      <!-- The sentence it will be read back as, with time in hours: typing
           "90" into a minutes field is easy to get wrong by a factor. -->
      <p class="reads">
        {#if t.min == null && t.max == null}
          Give it a number.
        {:else}
          Reads as “{describeTarget(tracker, t)}”.
        {/if}
      </p>
    </div>
  {/each}
  <button class="add" onclick={add}>
    <Icon name="plus" size={13} />
    {addLabel}
  </button>
</div>

<style>
  .targets {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }

  .line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px;
  }

  select,
  .num {
    height: 28px;
    padding: 0 var(--sp-1);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
  }
  .num {
    width: 4.5rem;
    font-variant-numeric: tabular-nums;
  }
  select:focus,
  .num:focus {
    outline: none;
    border-color: var(--accent);
  }

  .and,
  .word {
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }

  .drop {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    margin-left: auto;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .drop:hover {
    background: var(--bg-hover);
    color: var(--danger);
  }

  .reads {
    margin: 2px 0 0;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  .add {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    align-self: flex-start;
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .add:hover {
    color: var(--fg);
  }
</style>
