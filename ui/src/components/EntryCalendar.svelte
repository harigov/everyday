<script lang="ts">
  // A month, over the list of entries, with a mark on the days you wrote.
  //
  // The list answers "what have I written lately" well and "did I write on
  // the 14th" not at all: it is grouped by month once you are past a week,
  // so finding a particular day means scrolling and reading dates. A journal
  // is a record of *days*, and the shape a record of days is read in is a
  // calendar.
  //
  // Two things it deliberately is not. It is not the calendar app -- there
  // is one of those and it draws hours, feeds and blocks; this draws one
  // mark per day and nothing else. And it is not a filter: clicking a day
  // opens that day's entry rather than narrowing the list to it, because the
  // list is how you read *around* a day and losing it is the opposite of
  // what a jump is for.

  import { addMonths, localeWeekStart, monthGrid, startOfDay, todayIso } from '../lib/time'
  import { app } from '../lib/state.svelte'
  import Icon from './Icon.svelte'

  const locale = navigator.language || 'en'
  const weekStart = localeWeekStart()

  /** The month on screen. Starts on today's, follows the open entry after. */
  let anchor = $state(todayIso())
  /** Which day each entry is on, for the month showing. See the effect below. */
  let marks = $state<Map<string, string>>(new Map())

  const days = $derived(monthGrid(anchor, weekStart))
  const month = $derived(
    new Intl.DateTimeFormat(locale, { month: 'long', year: 'numeric' }).format(startOfDay(anchor)),
  )
  const today = todayIso()

  // The single-letter column headings, in the reader's own locale and week
  // order. Built from the grid's own first week, so they cannot disagree
  // with the columns under them.
  const initialFmt = new Intl.DateTimeFormat(locale, { weekday: 'narrow' })
  const headings = $derived(days.slice(0, 7).map((iso) => initialFmt.format(startOfDay(iso))))

  /** Is this date in the month the grid is centred on, rather than a spill? */
  function inMonth(iso: string): boolean {
    return iso.slice(0, 7) === anchor.slice(0, 7)
  }

  // Re-read whenever the month, the selected journal, or the list itself
  // changes. The last of those is what keeps a mark appearing the moment an
  // entry is written and disappearing when one is deleted -- `entries` is
  // replaced by every refresh, so reading its length is enough to subscribe.
  $effect(() => {
    const from = days[0]!
    const to = days[days.length - 1]!
    void app.selectedJournal
    void app.entries.length
    let live = true
    void app.entriesInRange(from, to).then((found) => {
      if (live) marks = found
    })
    return () => {
      live = false
    }
  })

  // Follow the entry that is open, so opening something from November puts
  // November on screen rather than leaving the reader to page back to it.
  let followed: string | null = null
  $effect(() => {
    const on = app.entries.find((e) => e.id === app.selectedEntry)?.localDate
    if (!on || on === followed) return
    followed = on
    if (on.slice(0, 7) !== anchor.slice(0, 7)) anchor = on
  })

  function step(months: number) {
    anchor = addMonths(anchor, months)
  }
</script>

<div class="cal">
  <div class="head">
    <button class="step" onclick={() => step(-1)} aria-label="The month before">
      <span class="back"><Icon name="chevron" size={14} weight={2} /></span>
    </button>
    <button
      class="month"
      onclick={() => (anchor = todayIso())}
      title="Back to this month"
      disabled={anchor.slice(0, 7) === today.slice(0, 7)}
    >
      {month}
    </button>
    <button class="step" onclick={() => step(1)} aria-label="The month after">
      <Icon name="chevron" size={14} weight={2} />
    </button>
  </div>

  <div class="grid" role="grid" aria-label="Entries by day">
    {#each headings as initial, i (i)}
      <span class="dow" aria-hidden="true">{initial}</span>
    {/each}

    {#each days as iso (iso)}
      {@const id = marks.get(iso)}
      {@const outside = !inMonth(iso)}
      <button
        class="day"
        class:outside
        class:written={!!id}
        class:on={!!id && id === app.selectedEntry}
        class:today={iso === today}
        disabled={!id}
        aria-label={new Intl.DateTimeFormat(locale, { dateStyle: 'full' }).format(startOfDay(iso)) +
          (id ? ' — has an entry' : ' — nothing written')}
        onclick={() => id && void app.openEntry(id)}
      >
        {Number(iso.slice(8))}
      </button>
    {/each}
  </div>
</div>

<style>
  .cal {
    flex: none;
    padding: 0 var(--sp-3) var(--sp-3);
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    padding: 0 var(--sp-1) var(--sp-2);
  }
  .month {
    flex: 1;
    height: 26px;
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    font-weight: 650;
    color: var(--fg-muted);
  }
  .month:not(:disabled):hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .step {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    flex: none;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .step:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .back {
    display: flex;
    rotate: 180deg;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(7, 1fr);
    gap: 1px;
  }
  .dow {
    display: grid;
    place-items: center;
    height: 18px;
    font-size: 10px;
    font-weight: 650;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }

  /* A circle per day, so the month reads as a pattern of filled and empty
     rather than as a block of numbers. The disc is the entry; the ring is
     today; a day with neither is just the number. */
  .day {
    position: relative;
    display: grid;
    place-items: center;
    aspect-ratio: 1;
    width: 100%;
    border-radius: 50%;
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums lining-nums;
    color: var(--fg-subtle);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .day.outside {
    color: var(--fg-faint);
    opacity: 0.5;
  }
  .day:disabled {
    cursor: default;
  }

  .day.written {
    background: color-mix(in oklab, var(--journal-accent, var(--accent)) 22%, transparent);
    color: color-mix(in oklab, var(--journal-accent, var(--accent)) 70%, var(--fg));
    font-weight: 650;
  }
  .day.written:hover {
    background: color-mix(in oklab, var(--journal-accent, var(--accent)) 38%, transparent);
  }
  .day.on {
    background: var(--journal-accent, var(--accent));
    color: var(--fg-on-accent);
  }

  /* Today is a ring rather than a fill, so it can be worn at the same time
     as "written" without either of them being hidden. */
  .day.today::after {
    content: '';
    position: absolute;
    inset: 0;
    border-radius: 50%;
    border: 1.5px solid var(--journal-accent, var(--accent));
    opacity: 0.85;
  }
</style>
