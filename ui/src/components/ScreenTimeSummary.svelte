<script lang="ts">
  // The summary: how much screen time the range held, when, of what, and on
  // which apps and sites.
  //
  // Four questions, four forms, never a donut:
  //
  //   how much     a figure (`StatTile`), with last range beside it in words
  //   when         columns -- by hour for a day, by day for a week or month --
  //                stacked by category; a day's column opens it on the timeline
  //   of what      the category split, a stacked bar and its keyed list, which
  //                is also the columns' legend; a row narrows everything to it
  //   on what      the top apps and sites, each with its category chip, which
  //                is where a wrong category is most often noticed and so
  //                where it can be put right
  //
  // A day's columns are one colour, not stacked: the report sums each hour
  // without splitting it by category. Narrowed to a category, they are that
  // category's colour; otherwise the accent, which no category is given.

  import { comparison, formatDuration, itemName, keyText } from '../lib/screentime'
  import { screentime, type LabelPatch } from '../lib/screentime.svelte'
  import { dateFormat, dayNumber, hourLabel, plural, weekdayShort } from '../lib/format'
  import { startOfDay, todayIso } from '../lib/time'
  import type { ScreenCategoryId, ScreenItem } from '../lib/types'
  import Icon from './Icon.svelte'
  import RankTable from './RankTable.svelte'
  import ScreenTimeCategoryPicker from './ScreenTimeCategoryPicker.svelte'
  import ScreenTimeColumns, { type Column } from './ScreenTimeColumns.svelte'
  import ScreenTimeShare from './ScreenTimeShare.svelte'
  import StatTile from './StatTile.svelte'

  const report = $derived(screentime.report)
  const range = $derived(screentime.range)
  const filter = $derived(screentime.categoryOf(screentime.category))

  /** Where each category sits in a stack: its place in the list, uncategorised last. */
  const order = $derived(new Map(screentime.categories.map((c, i) => [c.id, i])))
  function rank(id: ScreenCategoryId | null): number {
    return id === null ? Number.MAX_SAFE_INTEGER : (order.get(id) ?? Number.MAX_SAFE_INTEGER - 1)
  }

  const dayName = dateFormat({ weekday: 'short', day: 'numeric', month: 'short' })

  const columns = $derived.by((): Column[] => {
    if (!report) return []
    if (range === 'day') {
      const color = filter ? screentime.colorOf(filter.id) : 'var(--accent)'
      return report.hours.map((secs, h) => ({
        id: `h${h}`,
        name: `${hourLabel(h)}–${hourLabel((h + 1) % 24)}`,
        tick: h % 6 === 0 ? hourLabel(h) : undefined,
        segments: [{ category: screentime.category, secs, color }],
      }))
    }
    const today = todayIso()
    return report.days.map((d, i) => {
      const n = Number(dayNumber(d.date))
      return {
        id: d.date,
        name: dayName.format(startOfDay(d.date)),
        tick:
          range === 'week'
            ? weekdayShort(d.date)
            : i === 0 || n === 1 || n % 7 === 1
              ? String(n)
              : undefined,
        segments: [...d.categories]
          .sort((a, b) => rank(a.category) - rank(b.category))
          .map((c) => ({
            category: c.category,
            secs: c.secs,
            color: screentime.colorOf(c.category),
          })),
        onpick: d.date <= today ? () => screentime.openTimeline(d.date) : undefined,
        pickLabel: 'Open on the timeline',
      }
    })
  })

  /** Days of the range that have begun, for a fair average. */
  const daysSoFar = $derived(report ? report.days.filter((d) => d.date <= todayIso()).length : 0)

  const TOP = 10
  const items = $derived(report ? [...report.items].sort((a, b) => b.secs - a.secs) : [])
  const top = $derived(items.slice(0, TOP))
  const topCeiling = $derived(Math.max(1, ...top.map((i) => i.secs)))

  function hint(item: ScreenItem): string {
    if (item.browserRest) return 'Browser, no site claimed'
    return keyText(item.key)
  }

  const deviceRows = $derived(
    (report?.devices ?? [])
      .filter((d) => d.secs > 0)
      .sort((a, b) => b.secs - a.secs)
      .map((d) => ({
        id: d.device,
        name: d.device,
        value: d.secs,
        cells: [formatDuration(d.secs)],
        onpick: () => screentime.setDevices([d.device]),
        pickLabel: `Show only ${d.device}`,
      })),
  )

  const PREVIOUS: Record<typeof range, string> = {
    day: 'the day before',
    week: 'the week before',
    month: 'the month before',
  }
</script>

<div class="summary scroll">
  <div class="page">
    {#if !report}
      <p class="quiet">Loading…</p>
    {:else}
      <div class="tiles">
        <StatTile
          label={filter ? `Screen time on ${filter.name}` : 'Screen time'}
          value={formatDuration(report.totalSecs)}
          note={comparison(report.totalSecs, report.previousTotalSecs, range) ||
            (report.previousTotalSecs === 0 ? `Nothing recorded ${PREVIOUS[range]}` : undefined)}
        />
        {#if range !== 'day' && daysSoFar > 0}
          <StatTile
            label="A day, on average"
            value={formatDuration(Math.round(report.totalSecs / daysSoFar))}
            note="Over {plural(daysSoFar, 'day')}"
          />
        {/if}
        {#if filter}
          <div class="filtered">
            <span class="dot" style:background={screentime.colorOf(filter.id)}></span>
            <span>Only {filter.name}</span>
            <button
              class="clear"
              aria-label="Show every category"
              title="Show every category"
              onclick={() => screentime.setCategory(null)}
            >
              <Icon name="close" size={12} />
            </button>
          </div>
        {/if}
      </div>

      <section class="card" aria-labelledby="st-when">
        <h2 class="eyebrow" id="st-when">{range === 'day' ? 'By hour' : 'By day'}</h2>
        {#if report.totalSecs === 0}
          <p class="quiet">Nothing recorded in this range.</p>
        {:else}
          <ScreenTimeColumns
            {columns}
            label={range === 'day' ? 'Screen time by hour' : 'Screen time by day'}
          />
          {#if range !== 'day'}
            <p class="foot">Pick a day to see it on the timeline.</p>
          {/if}
        {/if}
      </section>

      <div class="pair">
        <section class="card" aria-labelledby="st-cats">
          <div class="card-head">
            <h2 class="eyebrow" id="st-cats">By category</h2>
            <button class="link" onclick={() => screentime.setView('categories')}>Edit</button>
          </div>
          <ScreenTimeShare totals={report.categories} />
        </section>

        <section class="card" aria-labelledby="st-top">
          <div class="card-head">
            <h2 class="eyebrow" id="st-top">Top apps and sites</h2>
            <button class="link" onclick={() => screentime.setView('apps')}>All of them</button>
          </div>
          {#if top.length === 0}
            <p class="quiet">Nothing recorded in this range.</p>
          {:else}
            <ol class="top">
              {#each top as item (item.key + (item.browserRest ? ':rest' : ''))}
                {@const name = itemName(item)}
                <li>
                  <span class="glyph" aria-hidden="true">
                    <Icon name={item.kind === 'site' ? 'globe' : 'monitor'} size={14} />
                  </span>
                  <span class="what">
                    <span class="line">
                      <span class="name" title={name}>{name}</span>
                      <span class="key" title={hint(item)}>{hint(item)}</span>
                    </span>
                    <span class="bar" aria-hidden="true">
                      <span
                        class="fill"
                        style:width="{(item.secs / topCeiling) * 100}%"
                        style:background={screentime.colorOf(item.category)}
                      ></span>
                    </span>
                  </span>
                  <ScreenTimeCategoryPicker
                    value={item.category}
                    suggested={item.suggested}
                    suggestion={item.suggested ? item.category : null}
                    itemName={name}
                    align="right"
                    onchange={(patch: LabelPatch) =>
                      screentime.saveLabel(item.key, item.kind, patch)}
                  />
                  <span class="secs">{formatDuration(item.secs)}</span>
                </li>
              {/each}
            </ol>
            {#if items.length > top.length}
              <p class="foot">and {(items.length - top.length).toLocaleString()} more</p>
            {/if}
          {/if}
        </section>
      </div>

      {#if deviceRows.length > 1}
        <section class="card" aria-labelledby="st-devices">
          <h2 class="eyebrow" id="st-devices">By device</h2>
          <RankTable
            head="Device"
            columns={['Time']}
            rows={deviceRows}
            limit={8}
            empty="Nothing recorded in this range."
            color="var(--fg-subtle)"
            figureWidth="7em"
          />
        </section>
      {/if}
    {/if}
  </div>
</div>

<style>
  .summary {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .page {
    max-width: 960px;
    margin: 0 auto;
    padding: var(--sp-2) var(--sp-6) var(--fab-clear);
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }

  .tiles {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: var(--sp-5) var(--sp-8);
    padding: var(--sp-2) 0;
  }

  /* The category the summary is narrowed to, and the way out of it. */
  .filtered {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    height: 28px;
    margin-left: auto;
    padding: 0 4px 0 var(--sp-3);
    border-radius: 999px;
    background: var(--bg-sunken);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .clear {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .clear:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .card {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
  }
  .card-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  .pair {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(320px, 1fr));
    gap: var(--sp-5);
  }

  .quiet,
  .foot {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }
  .foot {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  .link {
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .link:hover {
    text-decoration: underline;
  }

  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }

  .top {
    display: flex;
    flex-direction: column;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .top li {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    padding: 6px 0;
  }
  .top li + li {
    border-top: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .glyph {
    flex: none;
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    color: var(--fg-subtle);
  }
  .what {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .line {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
  }
  .name {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg);
    font-size: var(--text-base);
  }
  .key {
    flex: 1 1 0;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
  /* A real width for the fill's percentage to resolve against. */
  .bar {
    display: block;
    width: 100%;
    height: 4px;
  }
  .fill {
    display: block;
    height: 100%;
    min-width: 2px;
    border-radius: 0 2px 2px 0;
  }
  .secs {
    flex: none;
    width: 5.5em;
    text-align: right;
    color: var(--fg);
    font-size: var(--text-sm);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
</style>
