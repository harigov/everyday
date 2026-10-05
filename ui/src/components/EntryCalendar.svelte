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
  //
  // The grid itself is `MiniMonth.svelte`, the same small month the calendar
  // app's sidebar uses -- it used to be its own, taller grid of discs, which
  // is more month than a list column has room for. Here a day with an entry
  // gets the dot the calendar app already uses for "something happened",
  // rather than a filled circle, and the highlight the calendar app gives
  // its visible range marks the open entry's day instead.

  import { addMonths, localeWeekStart, monthGrid, startOfDay, todayIso } from '../lib/time'
  import { dateFormat } from '../lib/format'
  import { app } from '../lib/state.svelte'
  import MiniMonth from './MiniMonth.svelte'

  const weekStart = localeWeekStart()

  /** The month on screen. Starts on today's, follows the open entry after. */
  let anchor = $state(todayIso())
  /** Which day each entry is on, for the month showing. See the effect below. */
  let marks = $state<Map<string, string>>(new Map())

  const today = todayIso()

  // Re-read whenever the month, the selected journal, or the list itself
  // changes. The last of those is what keeps a mark appearing the moment an
  // entry is written and disappearing when one is deleted -- `entries` is
  // replaced by every refresh, so reading its length is enough to subscribe.
  $effect(() => {
    const days = monthGrid(anchor, weekStart)
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
</script>

<div class="cal">
  <MiniMonth
    {anchor}
    {weekStart}
    prevLabel="The month before"
    nextLabel="The month after"
    onstep={(months: number) => (anchor = addMonths(anchor, months))}
    ontitle={() => (anchor = todayIso())}
    titleDisabled={anchor.slice(0, 7) === today.slice(0, 7)}
    titleTitle="Back to this month"
    accent="var(--journal-accent, var(--accent))"
    isMarked={(iso: string) => marks.has(iso)}
    isOn={(iso: string) => {
      const id = marks.get(iso)
      return !!id && id === app.selectedEntry
    }}
    isPickable={(iso: string) => marks.has(iso)}
    dayLabel={(iso: string) =>
      dateFormat({ dateStyle: 'full' }).format(startOfDay(iso)) +
      (marks.has(iso) ? ' — has an entry' : ' — nothing written')}
    onpick={(iso: string) => {
      const id = marks.get(iso)
      if (id) void app.openEntry(id)
    }}
    gridRole="grid"
    gridLabel="Entries by day"
    hideHeadings
  />
</div>

<style>
  .cal {
    flex: none;
    padding: 0 var(--sp-3) var(--sp-3);
  }
</style>
