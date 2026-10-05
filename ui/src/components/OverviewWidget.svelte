<script lang="ts">
  // What each widget actually draws.
  //
  // One component with a branch per type rather than twenty files. Every one
  // of these is between five and twenty lines of markup over data the store
  // has already fetched, and splitting them would mean twenty imports, twenty
  // prop contracts, and a shape somebody has to learn before adding the
  // twenty-first. The frame, the handles and the menu are `WidgetCard`; this
  // is only the contents.
  //
  // The editorial rule the four fixed panes were built under still holds and
  // is the reason the catalogue is a closed set: nothing is drawn that does
  // not change what somebody does next. Where a widget can be acted on it is
  // -- the habit chips log a reading, the goal meters open the goal in the
  // todo app, the timer stops -- because a number you cannot act on is a
  // number people stop reading.

  import { formatClock, formatMinutes, plural, relativeTime, timeOfDay } from '../lib/format'
  import { calendar } from '../lib/calendar.svelte'
  import { api } from '../lib/api'
  import { overview } from '../lib/overview.svelte'
  import { ask, quick } from '../lib/quick.svelte'
  import { purpose } from '../lib/purpose.svelte'
  import { proposals, recordAs } from '../lib/proposals.svelte'
  import { app } from '../lib/state.svelte'
  import { todo } from '../lib/todo.svelte'
  import { tracking } from '../lib/tracking.svelte'
  import { notes as noteStore } from '../lib/notes.svelte'
  import { byRole, type RoleTotals } from '../lib/balance'
  import { describeStreak, streakTarget } from '../lib/habits'
  import { addDays, localeWeekStart, startOfDay, startOfWeek, todayIso } from '../lib/time'
  import { formatValue } from '../lib/tracker'
  import { aqiTier, byTimeOrLast, weatherIcon, type Widget } from '../lib/dashboard'
  import { panels } from '../lib/panels.svelte'
  import type { Proposal, WeatherCurrent, WeatherDay } from '../lib/types'
  import BalanceBars from './BalanceBars.svelte'
  import ColumnChart from './ColumnChart.svelte'
  import HabitHeatmap from './HabitHeatmap.svelte'
  import Meter from './Meter.svelte'
  import ProposalGhost from './ProposalGhost.svelte'
  import ShareBar from './ShareBar.svelte'
  import StatTile from './StatTile.svelte'
  import TrackerIcon from './TrackerIcon.svelte'
  import Icon from './Icon.svelte'

  const { widget }: { widget: Widget } = $props()

  const today = todayIso()
  const weekStart = localeWeekStart()

  /**
   * The tracker a habit widget is about.
   *
   * Falls through to the first live tracker when nothing has been chosen, so
   * a widget added from the catalogue draws something immediately rather than
   * asking a question before it has said what it is.
   */
  const subject = $derived(
    (widget.subject ? tracking.tracker(widget.subject) : null) ?? tracking.live[0] ?? null,
  )

  /** Roles as slices, meetings counted beside your own record. */
  const roleSlices = $derived(
    overview.roles.map((r) => ({
      id: r.roleId ?? 'none',
      name: r.name,
      color: r.color,
      icon: r.icon,
      value: r.actualMinutes + r.eventMinutes,
      label: formatMinutes(r.actualMinutes + r.eventMinutes),
    })),
  )

  // ── the week, in words ───────────────────────────────────────────────
  //
  // The totals are rendered here and sent as prose rather than re-derived in
  // Rust, so the sentence and the bars above it cannot disagree about what
  // the week contained.
  //
  // Cached in `localStorage` against the ISO week, beside the layout, for the
  // reason the layout is there: it decides what is drawn, not what is true.
  // The cache is the load-bearing part -- without it this card bills somebody
  // every time they reopen the window.

  const weekKey = $derived(`everyday:week-words:${overview.weekStart}`)
  let composing = $state(false)
  // Writable-derived: it re-reads when the week changes, and `writeTheWeek`
  // assigns straight into it after a run. A `$state` plus an `$effect` would
  // be the same thing with a frame of the previous week's sentence in it.
  let weekWords = $derived(localStorage.getItem(weekKey) ?? '')

  /** One line per role, as a model can read it. */
  function render(rows: RoleTotals[]): string {
    const lines = rows
      .filter((r) => r.actualMinutes + r.eventMinutes > 0)
      .map((r) => `${r.name}: ${formatMinutes(r.actualMinutes + r.eventMinutes)} recorded`)
    return lines.join('\n') || 'Nothing recorded.'
  }

  async function writeTheWeek() {
    composing = true

    // The week before, fetched here rather than left empty. The job's
    // instruction asks for "the one thing that changed most against the week
    // before" and forbids conclusions the numbers do not support -- so
    // sending nothing to compare against was asking for either an invented
    // comparison or a refusal, and this card's whole claim is the comparison.
    //
    // One extra query, on a button, once a week. `overview` holds only the
    // week on screen, and widening its load for a card that is off by default
    // would make every other card pay for this one.
    const priorStart = addDays(overview.weekStart, -7)
    const prior = await api
      .balance(priorStart, addDays(priorStart, 6))
      .then((report) => byRole(report, purpose.roles, purpose.goals))
      .catch(() => null)

    const text = await ask('overview.week', () =>
      api.quickWeekNote({
        thisWeek: render(overview.roles),
        // Empty rather than "Nothing recorded" when the query failed: the
        // instruction treats an absent field as nothing to compare with,
        // where a sentence saying nothing was recorded is a claim about a
        // week we did not actually read.
        lastWeek: prior ? render(prior) : '',
      }),
    )
    composing = false
    if (!text) return
    weekWords = text
    localStorage.setItem(weekKey, text)
  }

  /** Open goals, freshest first, with how far through their tasks they are. */
  const goalRows = $derived(
    purpose.goals
      .filter((g) => g.status === 'active' || g.status === 'paused')
      .map((goal) => {
        const a = purpose.activity.get(goal.id)
        const total = (a?.openTasks ?? 0) + (a?.doneTasks ?? 0)
        return {
          goal,
          role: purpose.role(goal.roleId),
          done: a?.doneTasks ?? 0,
          total,
          value: total === 0 ? 0 : (a?.doneTasks ?? 0) / total,
          minutes: a?.actualMinutes ?? 0,
        }
      })
      .sort((a, b) => b.total - a.total || a.goal.title.localeCompare(b.goal.title))
      .slice(0, 8),
  )

  /** The chosen tracker's window, a point a day, oldest first. */
  const series = $derived.by(() => {
    if (!subject) return []
    const days = widget.days ?? 30
    const byDate = new Map(
      overview.habitDays.filter((d) => d.trackerId === subject.id).map((d) => [d.date, d]),
    )
    return Array.from({ length: days }, (_, i) => {
      const date = addDays(today, -(days - 1 - i))
      const day = byDate.get(date)
      // The same rule the heatmap uses, and for the same reason: what a day
      // is *worth* depends on what the tracker is. A scale is the day's mean
      // -- three readings of "4" is a 4 kind of day, not a 12 kind of day.
      const value = !day
        ? 0
        : subject.kind === 'check'
          ? day.count
          : subject.kind === 'scale'
            ? day.count === 0
              ? 0
              : day.sum / day.count
            : day.sum
      return { date, value, label: formatValue(subject, value) }
    })
  })

  /** Trackers, longest chain first. */
  const streaks = $derived(
    tracking.live
      .map((tracker) => ({ tracker, summary: overview.habit(tracker.id) }))
      .sort((a, b) => b.summary.streak - a.summary.streak),
  )

  /** The week's tasks, and what was finished on each of its days. */
  const weekTasks = $derived.by(() => {
    const rows = overview.weekTasks
    const done = rows.filter((t) => t.status === 'done')
    const perDay = overview.weekDays.map((date) => ({
      date,
      value: done.filter((t) => (t.completedAt ?? '').slice(0, 10) === date).length,
      label: '',
    }))
    for (const point of perDay) point.label = plural(point.value, 'task')
    return { scheduled: rows.length, done: done.length, perDay }
  })

  /**
   * Entries per week over the window, oldest first.
   *
   * Bucketed by week rather than by day, because writing is a weekly habit
   * for almost everybody who keeps a journal and a daily column chart of it
   * is a comb: two hundred slots, a dozen of them one high.
   */
  const writing = $derived.by(() => {
    const days = widget.days ?? 90
    const buckets = new Map<string, number>()
    for (let at = startOfWeek(addDays(today, -days), weekStart); at <= today;) {
      buckets.set(at, 0)
      at = addDays(at, 7)
    }
    // Counted from the buckets, not from `overview.entries`. The store
    // fetches the *longest* window any writing card asks for, so two cards
    // set to thirty days and a year read the same array -- and the shorter
    // one would have reported the longer one's totals under the words
    // "in this window", contradicting the chart drawn directly beneath it.
    let entries = 0
    let words = 0
    for (const entry of overview.entries) {
      const bucket = startOfWeek(entry.localDate, weekStart)
      if (!buckets.has(bucket)) continue
      buckets.set(bucket, (buckets.get(bucket) ?? 0) + 1)
      entries += 1
      words += entry.wordCount
    }
    return {
      entries,
      words,
      points: [...buckets].map(([date, value]) => ({
        date,
        value,
        label: plural(value, 'entry', 'entries'),
      })),
    }
  })

  function openNote(id: string) {
    void app.goTo('notes').then(() => noteStore.openNote(id))
  }

  /**
   * Follow a meter through to the goal it measures.
   *
   * The selection is set *before* the scope, so the pane draws with its rail
   * already open on the right goal rather than opening it a frame later.
   */
  async function openGoal(id: string) {
    if (!(await app.goTo('todo'))) return
    purpose.selected = id
    await todo.setScope({ kind: 'goals' })
  }

  // ── Plan for tomorrow ────────────────────────────────────────────────
  //
  // Tomorrow's pending task and block proposals, in the order the day would
  // run -- so answering them here is answering them in the order you would
  // hit them tomorrow, not the order a dream happened to write them in.

  /** A clock string sortable against a block's, or `null` for "no time". */
  function timeOf(p: Proposal): string | null {
    if (p.kind === 'block') return recordAs(p, 'block')?.start.slice(11, 19) ?? null
    if (p.kind === 'task') return recordAs(p, 'task')?.dueTime ?? null
    return null
  }

  const tomorrowProposals = $derived(
    byTimeOrLast(
      proposals.forDate(overview.tomorrow).filter((p) => p.kind === 'task' || p.kind === 'block'),
      timeOf,
    ),
  )

  $effect(() => {
    if (tomorrowProposals.length > 0) void proposals.markSeen(tomorrowProposals)
  })

  /** "16 km/h W, gusts 27": one string, so the template cannot space it wrongly. */
  function windLine(cur: WeatherCurrent, unit: string): string {
    let line = `${Math.round(cur.windSpeed ?? 0)} ${unit}`
    if (cur.windDirectionCompass) line += ` ${cur.windDirectionCompass}`
    if (cur.windGusts != null) line += `, gusts ${Math.round(cur.windGusts)}`
    return line
  }

  /** "Today: high 21°C, low 14°C, 10% chance of rain". */
  function todayLine(day: WeatherDay, unit: string): string {
    const temp = (t: number | null) => (t != null ? `${Math.round(t)}${unit}` : '—')
    let line = `Today: high ${temp(day.high)}, low ${temp(day.low)}`
    if (day.precipitationChance != null) line += `, ${day.precipitationChance}% chance of rain`
    return line
  }
</script>

{#if widget.type === 'onNow'}
  {#if calendar.timer}
    <StatTile
      label={calendar.timer.title || 'Tracking'}
      value={formatMinutes(calendar.runningMinutes)}
      note="Running since {new Date(calendar.timer.since).toLocaleTimeString(undefined, {
        hour: '2-digit',
        minute: '2-digit',
      })}"
    >
      <button class="btn" onclick={() => calendar.stopTimer()}>Stop</button>
    </StatTile>
  {:else}
    <StatTile value="Nothing" note="No timer is running.">
      <button class="btn" onclick={() => calendar.startTimer({ type: 'adhoc' })}>
        Start one
      </button>
    </StatTile>
  {/if}

  <!-- ── Due today ─────────────────────────────────────────────────── -->
{:else if widget.type === 'dueToday'}
  <StatTile
    value={plural(overview.taskStats?.dueToday ?? 0, 'task')}
    note={(overview.taskStats?.overdue ?? 0) > 0
      ? `${plural(overview.taskStats?.overdue ?? 0, 'is', 'are')} already overdue`
      : 'Nothing has slipped.'}
    warn={(overview.taskStats?.overdue ?? 0) > 0}
  />

  <!-- ── Recorded today ────────────────────────────────────────────── -->
{:else if widget.type === 'recordedToday'}
  <StatTile
    value={formatMinutes(overview.recordedToday.logged)}
    note="{formatMinutes(overview.recordedToday.planned)} planned"
  />

  <!-- ── Today's habits ────────────────────────────────────────────── -->
{:else if widget.type === 'habitsToday'}
  {#if tracking.loggable.length === 0}
    <p class="dim">
      Nothing is tracked yet. “Record something” at the top of this page makes one out of what you
      type.
    </p>
  {:else}
    <div class="chips">
      {#each tracking.loggable as tracker (tracker.id)}
        {@const summary = overview.habit(tracker.id)}
        {@const done = summary.days.includes(today)}
        <button
          class="habit-chip"
          class:on={done}
          style="--c: {tracker.color}"
          title={done ? `${tracker.name} — recorded today` : `Record ${tracker.name}`}
          onclick={() => tracking.log(tracker, tracker.defaultValue)}
        >
          <TrackerIcon name={tracker.icon} color={tracker.color} size={22} solid={done} />
          <span class="cname">{tracker.name}</span>
          {#if summary.streak > 0}<span class="streak">{summary.streak}</span>{/if}
        </button>
      {/each}
    </div>
  {/if}

  <!-- ── Plan for tomorrow ─────────────────────────────────────────── -->
{:else if widget.type === 'planTomorrow'}
  {#if tomorrowProposals.length === 0}
    <p class="dim">Nothing proposed for tomorrow.</p>
  {:else}
    <ul class="plain plan-list">
      {#each tomorrowProposals as p (p.id)}
        {@const task = recordAs(p, 'task')}
        {@const block = recordAs(p, 'block')}
        <li>
          <ProposalGhost
            proposal={p}
            color={p.kind === 'block' ? calendar.colorOfSubject(block!.subject) : 'var(--accent)'}
          >
            <span class="plan-time">
              {#if block}{timeOfDay(new Date(block.start))}{:else if task?.dueTime}{formatClock(
                  task.dueTime,
                )}{:else}All day{/if}
            </span>
            <span class="plan-title">
              {task?.title ?? (block ? calendar.titleOfBlock(block) : p.caption)}
            </span>
          </ProposalGhost>
        </li>
      {/each}
    </ul>
  {/if}

  <!-- ── Weather ──────────────────────────────────────────────────── -->
{:else if widget.type === 'weather'}
  {#if overview.weatherNoLocation}
    <p class="dim">Add where you live in Settings to see the weather here.</p>
    <button class="footnote-btn" onclick={() => panels.openSettings('profile')}>
      Set your location
    </button>
  {:else if overview.weather}
    {@const w = overview.weather}
    {@const cur = w.current}
    {@const today = w.days[0] ?? null}
    {@const rest = w.days.slice(1, 3)}
    {@const aq = w.airQuality?.current ?? null}
    {@const tier = aq?.aqi != null ? aqiTier(aq.scale, aq.category) : null}
    {@const aqiColor = tier ? `var(--aqi-${tier})` : 'var(--fg-faint)'}
    <div class="w-head">
      <span class="w-icon">
        <Icon name={cur ? weatherIcon(cur.condition) : 'cloud'} size={26} />
      </span>
      <div class="w-now">
        <span class="w-temp">
          {cur?.temperature != null ? `${Math.round(cur.temperature)}${w.units.temperature}` : '—'}
        </span>
        <span class="dim">{cur?.condition ?? 'Unknown'}</span>
      </div>
      <span class="w-place dim">{w.place}</span>
    </div>

    {#if widget.size !== 'small'}
      <p class="footnote">
        {#if cur?.feelsLike != null}Feels like {Math.round(cur.feelsLike)}{w.units.temperature} ·
        {/if}
        {cur?.humidity ?? '—'}% humidity
      </p>
      {#if cur?.windSpeed != null}
        <!-- Its own line, with the icon held to its text: inline in the line
             above, the icon wrapped away from the speed it labels. -->
        <p class="footnote w-wind">
          <Icon name="wind" size={12} />
          <span>{windLine(cur, w.units.wind)}</span>
        </p>
      {/if}

      {#if aq?.aqi != null}
        <p class="aqi-row" style={`--aqi: ${aqiColor}`}>
          <span class="aqi-dot"></span>
          AQI {aq.aqi} — {aq.category ?? 'Unknown'}
        </p>
      {/if}

      {#if today}
        <p class="footnote">{todayLine(today, w.units.temperature)}</p>
      {/if}
    {/if}

    {#if widget.size === 'large' && rest.length > 0}
      <div class="w-strip">
        {#each rest as day (day.date)}
          <div class="w-day">
            <span class="dim">
              {startOfDay(day.date).toLocaleDateString(undefined, { weekday: 'short' })}
            </span>
            <Icon name={weatherIcon(day.condition)} size={18} />
            <span class="w-day-temps">
              {day.high != null ? Math.round(day.high) : '—'}° / {day.low != null
                ? Math.round(day.low)
                : '—'}°
            </span>
          </div>
        {/each}
      </div>
    {/if}
  {:else if overview.weatherError}
    <p class="dim">Could not load the weather. {overview.weatherError}</p>
  {:else}
    <p class="dim">Loading the weather…</p>
  {/if}

  <!-- ── Where the week went ───────────────────────────────────────── -->
{:else if widget.type === 'roleBalance'}
  <BalanceBars rows={overview.roles} />
  <p class="footnote">
    The bar is what you recorded; the hairline under it is what you planned. Meetings from
    subscribed calendars are the paler segment, counted beside your own record rather than added to
    it.
  </p>

  <!-- ── The week, in words ────────────────────────────────────────── -->
  <!-- Every other widget on this page is a number; none of them says "you
       logged eleven hours against Parent and two against Yourself, which is
       the reverse of the fortnight before". The catalogue's editorial rule is
       that a widget names something you would act on, and this one does.

       Behind a button and cached per week, not fired on mount. The Overview
       is the page the app opens on, and a card that made a request every time
       somebody reopened a window would bill them for reopening a window. -->
{:else if widget.type === 'weekInWords'}
  {#if weekWords}
    <p class="words">{weekWords}</p>
    <button class="footnote-btn" onclick={() => void writeTheWeek()} disabled={composing}>
      Write it again
    </button>
  {:else if composing}
    <p class="footnote">Reading the week…</p>
  {:else if quick.enabled('overview.week')}
    <button class="footnote-btn" onclick={() => void writeTheWeek()}>
      <Icon name="sparkle" size={12} /> Write the week
    </button>
  {:else}
    <p class="footnote">Switch on “Write the week” under the quick model in Settings.</p>
  {/if}

  <!-- ── Share of your week ────────────────────────────────────────── -->
{:else if widget.type === 'roleShare'}
  <ShareBar slices={roleSlices} empty="No hours recorded in this week yet." />

  <!-- ── Gone quiet ────────────────────────────────────────────────── -->
{:else if widget.type === 'neglected'}
  {#if overview.neglectedRoles.length === 0}
    <p class="dim">Every role with a goal open has had something recorded against it.</p>
  {:else}
    <ul class="plain">
      {#each overview.neglectedRoles as row (row.role.id)}
        <li class="quiet-row">
          <span class="dot" style="background: {row.role.color}"></span>
          <span class="qname">{row.role.name}</span>
          <span class="dim">
            {row.days === null ? 'nothing ever recorded' : `${row.days} days`}
          </span>
        </li>
      {/each}
    </ul>
  {/if}

  <!-- ── Goals under way ───────────────────────────────────────────── -->
{:else if widget.type === 'goalProgress'}
  {#if goalRows.length === 0}
    <p class="dim">No goals are open. They live in the Todo app, under Goals.</p>
  {:else}
    <div class="meters">
      {#each goalRows as row (row.goal.id)}
        <Meter
          label={row.goal.title}
          value={row.value}
          color={row.role?.color ?? 'var(--accent)'}
          note={row.total === 0
            ? row.minutes > 0
              ? `${formatMinutes(row.minutes)} recorded, no tasks yet`
              : 'Nothing points at this yet'
            : `${row.done} of ${plural(row.total, 'task')} done`}
          onpick={() => void openGoal(row.goal.id)}
        />
      {/each}
    </div>
  {/if}

  <!-- ── Goals, counted ────────────────────────────────────────────── -->
{:else if widget.type === 'goalTally'}
  {@const open = purpose.goals.filter((g) => g.status === 'active' || g.status === 'paused')}
  {@const done = purpose.goals.filter((g) => g.status === 'done')}
  <StatTile
    value={plural(open.length, 'goal')}
    note={done.length > 0 ? `${done.length} finished` : 'Nothing finished yet'}
  />

  <!-- ── Streaks ───────────────────────────────────────────────────── -->
{:else if widget.type === 'habitStreaks'}
  {#if streaks.length === 0}
    <p class="dim">Nothing is tracked yet.</p>
  {:else}
    <ul class="plain">
      {#each streaks as row (row.tracker.id)}
        <li class="streak-row">
          <TrackerIcon name={row.tracker.icon} color={row.tracker.color} size={18} />
          <span class="sname">{row.tracker.name}</span>
          {#if row.summary.rate !== null}
            <span class="dim">{Math.round(row.summary.rate * 100)}% kept</span>
          {/if}
          <span class="chain">
            {describeStreak(row.summary.streak, streakTarget(row.tracker).per)}
          </span>
        </li>
      {/each}
    </ul>
  {/if}

  <!-- ── A habit, day by day ───────────────────────────────────────── -->
{:else if widget.type === 'habitHeatmap'}
  {#if !subject}
    <p class="dim">Nothing is tracked yet.</p>
  {:else}
    <HabitHeatmap
      tracker={subject}
      days={overview.habitDays.filter((d) => d.trackerId === subject.id)}
      from={overview.habitFrom}
      to={today}
      {weekStart}
    />
  {/if}

  <!-- ── A number over time ────────────────────────────────────────── -->
{:else if widget.type === 'trackerChart'}
  {#if !subject}
    <p class="dim">Nothing is tracked yet.</p>
  {:else}
    <ColumnChart points={series} color={subject.color} unit={subject.unit} />
  {/if}

  <!-- ── This week, in tasks ───────────────────────────────────────── -->
{:else if widget.type === 'weekTasks'}
  <StatTile
    value="{weekTasks.done} of {weekTasks.scheduled}"
    note={weekTasks.scheduled === 0
      ? 'Nothing is dated to this week.'
      : 'dated to this week, and done'}
  />
  {#if weekTasks.scheduled > 0}
    <ColumnChart points={weekTasks.perDay} height={64} />
  {/if}

  <!-- ── Tasks, counted ────────────────────────────────────────────── -->
{:else if widget.type === 'taskTally'}
  <StatTile
    value={plural(overview.taskStats?.openTasks ?? 0, 'task')}
    note="{overview.taskStats?.doneTasks ?? 0} finished · {formatMinutes(
      overview.taskStats?.loggedMinutes ?? 0,
    )} logged"
  />

  <!-- ── On the shelves ────────────────────────────────────────────── -->
{:else if widget.type === 'shelves'}
  <StatTile
    value={plural(overview.libraryStats?.active ?? 0, 'thing')}
    note="under way · {overview.libraryStats?.finishedThisYear ?? 0} finished this year"
  />

  <!-- ── What you have written ─────────────────────────────────────── -->
{:else if widget.type === 'writing'}
  <StatTile
    value={plural(writing.entries, 'entry', 'entries')}
    note="{writing.words.toLocaleString()} words in this window"
  />
  <ColumnChart points={writing.points} height={64} />

  <!-- ── Notes you touched last ────────────────────────────────────── -->
{:else if overview.notes.length === 0}
  <p class="dim">No notes yet.</p>
{:else}
  <ul class="plain">
    {#each overview.notes as note (note.id)}
      <li>
        <button class="notelink" onclick={() => void openNote(note.id)}>
          <span class="nname">{note.title || 'Untitled note'}</span>
          <span class="dim">{relativeTime(note.updatedAt)}</span>
        </button>
      </li>
    {/each}
  </ul>
{/if}

<style>
  .words {
    margin: 0;
    color: var(--fg);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }

  .footnote-btn {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    margin-top: var(--sp-2);
    padding: 0;
    border: 0;
    background: none;
    color: var(--fg-subtle);
    font: inherit;
    font-size: var(--text-xs);
    cursor: pointer;
  }

  .footnote-btn:hover:not(:disabled) {
    color: var(--fg-muted);
  }

  .dim {
    margin: 0;
    color: var(--fg-faint);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }

  .footnote {
    margin: var(--sp-3) 0 0;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
  }

  .plain {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .plan-list li {
    display: flex;
  }
  .plan-time {
    flex: none;
    margin-right: var(--sp-2);
    font-variant-numeric: tabular-nums;
    color: var(--fg-faint);
  }
  .plan-title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .meters {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }

  /* ── Weather ───────────────────────────────────────────────────── */

  .w-wind {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .w-head {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }

  .w-icon {
    display: grid;
    flex: none;
    place-items: center;
    color: var(--fg-muted);
  }

  .w-now {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .w-temp {
    font-size: var(--text-2xl);
    font-weight: 600;
    line-height: var(--leading-tight);
    font-variant-numeric: tabular-nums;
  }

  /* Pushed to the far side of the header rather than under the
     temperature: the card's own heading already says "Weather", so the
     place name is a detail to confirm rather than a second headline. */
  .w-place {
    margin-left: auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* The AQI dot borrows the same colour-band idea `TrackerIcon` and the
     habit chips already use for their own entity colour -- a small solid
     disc rather than a coloured background, so a sensitive reading does not
     tint the whole card the way a toast or a banner would. */
  .aqi-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin: var(--sp-2) 0 0;
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }

  .aqi-dot {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--aqi);
  }

  .w-strip {
    display: flex;
    gap: var(--sp-4);
    margin-top: var(--sp-3);
    padding-top: var(--sp-3);
    border-top: 1px solid var(--border);
  }

  .w-day {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }

  .w-day-temps {
    color: var(--fg);
    font-variant-numeric: tabular-nums;
  }

  /* ── Today's habits ─────────────────────────────────────────────── */

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }

  .habit-chip {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: 38px;
    padding: 0 var(--sp-3);
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--bg);
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .habit-chip.on {
    border-color: color-mix(in oklab, var(--c) 55%, transparent);
    background: color-mix(in oklab, var(--c) 12%, transparent);
    color: var(--fg);
  }
  .habit-chip:hover {
    border-color: var(--border-strong);
  }
  .cname {
    max-width: 12rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .streak {
    padding: 0 6px;
    border-radius: 999px;
    background: var(--bg-active);
    font-size: var(--text-xs);
    font-weight: 650;
    font-variant-numeric: tabular-nums;
  }

  /* ── Rows ───────────────────────────────────────────────────────── */

  .quiet-row,
  .streak-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-base);
    color: var(--fg-muted);
  }

  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }

  .qname,
  .sname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg);
  }

  .chain {
    flex: none;
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
    font-variant-numeric: tabular-nums;
  }

  .notelink {
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
    width: 100%;
    padding: var(--sp-1) var(--sp-2);
    margin: 0 calc(var(--sp-2) * -1);
    border-radius: var(--radius-sm);
    text-align: left;
    color: var(--fg-muted);
    font-size: var(--text-base);
  }
  .notelink:hover {
    background: var(--bg-hover);
  }
  .nname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg);
  }
</style>
