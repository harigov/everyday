<script lang="ts">
  // A small month: a title you can step or jump from, and a 7×6 grid of
  // days. Shared by the calendar app's sidebar (`CalendarNav`) and the
  // journal's entry list (`EntryCalendar`) -- the two used to carry their
  // own copies of the same layout, which is how they drifted to two
  // different row heights for what is, on screen, the same widget.
  //
  // What counts as "on" (the highlighted day or range), what gets the dot
  // under the number, whether a day can be picked at all, and what picking
  // one means are all the caller's business -- a calendar app and a journal
  // disagree on every one of those. This file only owns the grid, the
  // paging, and the title.

  import { monthGrid, startOfDay, todayIso } from '../lib/time'
  import { monthYear, weekdayNarrow } from '../lib/format'
  import Icon from './Icon.svelte'

  let {
    anchor,
    weekStart,
    prevLabel,
    nextLabel,
    onstep,
    ontitle,
    titleDisabled = false,
    titleTitle,
    /** The colour "today" draws in. A plain CSS colour or `var(...)`, so the
     *  journal can hand in its per-journal accent instead of the app's. */
    accent = 'var(--accent)',
    /** Which days draw as highlighted (the `.on` class) -- a visible range
     *  for the calendar app's mini month, the open entry's day for the
     *  journal's. */
    isOn = () => false,
    /** Which days get the small dot under the number. */
    isMarked = () => false,
    /** Which days can be picked at all. Unpickable days are disabled, so
     *  they neither click nor hover. */
    isPickable = () => true,
    /** Per-day `aria-label`. Omitted entirely when not given, as the
     *  calendar app's mini month does -- the number alone is label enough
     *  when every day does the same thing. */
    dayLabel,
    onpick,
    oncontext,
    /** `role`/`aria-label` for the day grid itself. Left unset by default;
     *  the journal's calendar sets both, the calendar app's mini month
     *  sets neither, exactly as before this file existed. */
    gridRole,
    gridLabel,
    /** Hide the weekday initials from screen readers. The journal's grid
     *  already names the full date per day, so the single-letter column
     *  heading is noise there; the calendar app's mini month has no such
     *  per-day label, so its initials stay announced. */
    hideHeadings = false,
  }: {
    anchor: string
    weekStart: number
    prevLabel: string
    nextLabel: string
    /** The step buttons were clicked: -1 for back a month, 1 for forward. */
    onstep: (months: number) => void
    ontitle: () => void
    titleDisabled?: boolean
    titleTitle?: string
    accent?: string
    isOn?: (iso: string) => boolean
    isMarked?: (iso: string) => boolean
    isPickable?: (iso: string) => boolean
    dayLabel?: (iso: string) => string
    onpick: (iso: string) => void
    oncontext?: (e: MouseEvent, iso: string) => void
    gridRole?: string
    gridLabel?: string
    hideHeadings?: boolean
  } = $props()

  const days = $derived(monthGrid(anchor, weekStart))
  // The column headings, taken from the grid's own first week so they can
  // never disagree with the columns under them.
  const headings = $derived(days.slice(0, 7).map((iso) => weekdayNarrow(startOfDay(iso))))
  const today = $derived(todayIso())
</script>

<div class="minimonth" style="--mini-accent: {accent}">
  <div class="minihead">
    <button class="ministep" aria-label={prevLabel} onclick={() => onstep(-1)}
      ><span class="back"><Icon name="chevron" size={13} /></span></button
    >
    <button class="minititle" onclick={ontitle} disabled={titleDisabled} title={titleTitle}>
      {monthYear(startOfDay(anchor))}
    </button>
    <button class="ministep" aria-label={nextLabel} onclick={() => onstep(1)}
      ><Icon name="chevron" size={13} /></button
    >
  </div>

  <div class="mini" role={gridRole} aria-label={gridLabel}>
    {#each headings as letter, i (i)}
      <span class="initial" aria-hidden={hideHeadings ? 'true' : undefined}>{letter}</span>
    {/each}
    {#each days as iso (iso)}
      <button
        class="minday"
        class:out={iso.slice(0, 7) !== anchor.slice(0, 7)}
        class:on={isOn(iso)}
        class:today={iso === today}
        disabled={!isPickable(iso)}
        aria-label={dayLabel?.(iso)}
        onclick={() => onpick(iso)}
        oncontextmenu={(e) => oncontext?.(e, iso)}
      >
        {Number(iso.slice(8, 10))}
        {#if isMarked(iso)}<span class="bump" aria-hidden="true"></span>{/if}
      </button>
    {/each}
  </div>
</div>

<style>
  .minihead {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 0 2px var(--sp-1);
  }
  .minititle {
    flex: 1;
    height: 24px;
    padding: 0 var(--sp-1);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    font-weight: 600;
    letter-spacing: -0.004em;
    text-align: left;
    color: var(--fg-muted);
  }
  .minititle:not(:disabled):hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .ministep {
    width: 22px;
    height: 22px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .ministep:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .back {
    display: flex;
    rotate: 180deg;
  }

  .mini {
    display: grid;
    grid-template-columns: repeat(7, 1fr);
    gap: 1px;
    padding: 0 1px;
  }
  .initial {
    height: 18px;
    display: grid;
    place-items: center;
    font-size: 11px;
    font-weight: 600;
    color: var(--fg-faint);
  }
  .minday {
    position: relative;
    height: 22px;
    display: grid;
    place-items: center;
    border-radius: 4px;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--fg-muted);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .minday:not(:disabled):hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .minday:disabled {
    cursor: default;
  }
  .minday.out {
    color: var(--fg-faint);
    opacity: 0.6;
  }
  /* Whatever the caller counts as "on" -- a visible range, an open entry --
     so the small month says where you are as well as where you could go. */
  .minday.on {
    background: var(--bg-active);
    color: var(--fg);
    font-weight: 600;
  }
  .minday.today {
    color: var(--mini-accent);
    font-weight: 700;
  }

  .bump {
    position: absolute;
    bottom: 2px;
    width: 3px;
    height: 3px;
    border-radius: 50%;
    background: currentColor;
    opacity: 0.55;
  }
</style>
