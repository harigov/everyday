<script lang="ts">
  // The Overview: four panes over what the other four apps already store.
  //
  // Each pane answers one question and nothing is drawn that does not change
  // what somebody does next. That is the whole editorial rule here, and it
  // is why there is no row of stat tiles: a dashboard made of numbers you
  // cannot act on is a dashboard people stop opening.
  //
  //   Today    what is on, what is due, what has been ticked
  //   Week     where the hours went, by role, plan beside record
  //   Goals    what you said you wanted, oldest-touched first
  //   Habits   what is holding, and what has quietly stopped

  import { formatMinutes, friendlyDate, plural } from '../lib/format'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { overview, PANE_LABELS, PANES, type Pane } from '../lib/overview.svelte'
  import { purpose } from '../lib/purpose.svelte'
  import { calendar } from '../lib/calendar.svelte'
  import { todo } from '../lib/todo.svelte'
  import { tracking } from '../lib/tracking.svelte'
  import { describeStreak } from '../lib/habits'
  import { localeWeekStart, todayIso } from '../lib/time'
  import { describeCadence, type GoalStatus, type Purpose } from '../lib/types'
  import BalanceBars from './BalanceBars.svelte'
  import EmptyState from './EmptyState.svelte'
  import HabitHeatmap from './HabitHeatmap.svelte'
  import Icon from './Icon.svelte'
  import TrackerIcon from './TrackerIcon.svelte'
  import PurposeField from './PurposeField.svelte'

  // Loaded when the view appears rather than when the store is imported, so
  // a vault whose owner never opens this app never pays for the queries.
  // `start` is idempotent, so coming back refreshes instead of blanking.
  void overview.start()

  const weekStart = localeWeekStart()
  let pickedRole = $state<string | null>(null)
  let newGoal = $state<Record<string, string>>({})

  const rows = $derived(overview.roles)
  const today = $derived(todayIso())
  const goal = $derived(overview.selectedGoal ? purpose.goal(overview.selectedGoal) : undefined)

  const STATUS_LABELS: Record<GoalStatus, string> = {
    active: 'Active',
    paused: 'Paused',
    done: 'Done',
    dropped: 'Dropped',
  }

  /** The week's heading: "this week" when it is, and the dates when it is not. */
  const weekLabel = $derived(
    overview.thisWeek
      ? 'This week'
      : `${friendlyDate(overview.weekStart)} – ${friendlyDate(overview.weekEnd)}`,
  )

  function paneMenu(): MenuItem[] {
    return tidyMenu([
      ...PANES.map((p) => ({
        label: PANE_LABELS[p],
        checked: overview.pane === p,
        run: () => overview.setPane(p),
      })),
      SEP,
      { label: 'Refresh', icon: 'refresh', run: () => overview.refresh() },
    ])
  }

  async function addGoal(roleId: string) {
    const title = (newGoal[roleId] ?? '').trim()
    if (!title) return
    newGoal = { ...newGoal, [roleId]: '' }
    await overview.addGoal(roleId, title)
  }

  function setPane(p: Pane) {
    overview.setPane(p)
  }
</script>

<div class="overview">
  <header class="bar">
    <div class="titles">
      <h1>{PANE_LABELS[overview.pane]}</h1>
      <span class="sub">
        {#if overview.pane === 'week'}{weekLabel}{/if}
        {#if overview.loading}· loading…{/if}
      </span>
    </div>

    <div class="toolbar">
      <div class="toolbar-end"></div>
      <div class="filters" role="tablist" aria-label="Panes">
        {#each PANES as p (p)}
          <button
            class="filter"
            class:on={overview.pane === p}
            role="tab"
            aria-selected={overview.pane === p}
            onclick={() => setPane(p)}
          >
            {PANE_LABELS[p]}
          </button>
        {/each}
      </div>
      <div class="toolbar-end right">
        {#if overview.pane === 'week'}
          <div class="week-nav">
            <button aria-label="The week before" onclick={() => overview.goWeek(-1)}>
              <span class="prev"><Icon name="chevron" size={14} /></span>
            </button>
            <button onclick={() => overview.goWeek(0)} disabled={overview.thisWeek}>Today</button>
            <button aria-label="The week after" onclick={() => overview.goWeek(1)}>
              <span class="next"><Icon name="chevron" size={14} /></span>
            </button>
          </div>
        {/if}
      </div>
    </div>
  </header>

  <!-- The pane itself, where there is no row under the pointer: the same
       four panes the header offers, within reach of where you are looking. -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="scroll body" oncontextmenu={(e) => menu.show(e, paneMenu())}>
    {#if purpose.roles.length === 0}
      <!-- The one screen this app can open on that is not a failure. Roles
           are never seeded unasked: a list of what a life is made of is a
           claim, and writing one for somebody would be this application
           telling them who they are. -->
      <EmptyState lead="Nothing here yet, because nobody has said what your life is made of.">
        {#snippet icon()}<Icon name="compass" size={28} />{/snippet}
        {#snippet note()}
          A <em>role</em> is who you are being — Parent, Work, Myself. A <em>goal</em> is something you
          want under one of them. Add your own in the sidebar, or start from a handful and change them.
        {/snippet}
        {#snippet action()}
          <button class="primary" onclick={() => overview.seedRoles()}>
            Start me off with a few
          </button>
        {/snippet}
      </EmptyState>

      <!-- ── Today ─────────────────────────────────────────────────── -->
    {:else if overview.pane === 'today'}
      <section class="pane">
        <div class="cards">
          <div class="card">
            <h2>On now</h2>
            {#if calendar.timer}
              <p class="lead">
                {calendar.timer.title || 'Tracking'}
                <span class="dim">· {formatMinutes(calendar.runningMinutes)}</span>
              </p>
              <button class="quiet" onclick={() => calendar.stopTimer()}>Stop</button>
            {:else}
              <p class="lead dim">Nothing being tracked.</p>
            {/if}
          </div>

          <div class="card">
            <h2>Due today</h2>
            <p class="lead">{plural(todo.dueTodayCount, 'task')}</p>
            {#if (todo.stats?.overdue ?? 0) > 0}
              <p class="warn">{plural(todo.stats?.overdue ?? 0, 'is', 'are')} already overdue</p>
            {/if}
          </div>

          <div class="card">
            <h2>Recorded today</h2>
            <p class="lead">{formatMinutes(calendar.totalsOn(today).logged)}</p>
            <p class="dim">{formatMinutes(calendar.totalsOn(today).planned)} planned</p>
          </div>
        </div>

        <h2 class="section">Today's habits</h2>
        {#if tracking.live.length === 0}
          <p class="dim">No trackers yet. A journal's settings is where they are made.</p>
        {:else}
          <div class="chips">
            {#each tracking.live as tracker (tracker.id)}
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
                {#if summary.streak > 0}
                  <span class="streak">{summary.streak}</span>
                {/if}
              </button>
            {/each}
          </div>
        {/if}
      </section>

      <!-- ── This week ─────────────────────────────────────────────── -->
    {:else if overview.pane === 'week'}
      <section class="pane">
        {#if overview.neglectedRoles.length > 0}
          <!-- The one thing this app can say that no other can: a role has
               to be read across four domains before anybody can tell it has
               gone quiet. Said once, plainly, and never as a notification. -->
          <div class="notice">
            <Icon name="info" size={15} />
            <p>
              {#each overview.neglectedRoles.slice(0, 2) as n, i (n.role.id)}
                {i > 0 ? ' and ' : ''}<b>{n.role.name}</b> has had nothing recorded
                {n.days === null ? 'ever' : `for ${n.days} days`}
              {/each}, and still has goals open.
            </p>
          </div>
        {/if}

        <BalanceBars {rows} picked={pickedRole} onpick={(id: string | null) => (pickedRole = id)} />

        {#if pickedRole !== null}
          {@const row = rows.find((r) => r.roleId === pickedRole)}
          {#if row && row.goals.length > 0}
            <h2 class="section">{row.name}, by goal</h2>
            <ul class="plain">
              {#each row.goals as g (g.goalId)}
                <li class="goal-line">
                  <span class="dot" style="background: {row.color}"></span>
                  <span class="gname">{g.title}</span>
                  <span class="num">{formatMinutes(g.actualMinutes)}</span>
                </li>
              {/each}
            </ul>
          {/if}
        {/if}

        <p class="footnote">
          The bar is what you recorded; the hairline under it is what you planned. Meetings from
          subscribed calendars are the paler segment, counted beside your own record rather than
          added to it.
        </p>
      </section>

      <!-- ── Goals ─────────────────────────────────────────────────── -->
    {:else if overview.pane === 'goals'}
      <section class="pane">
        {#each overview.byRole as group (group.role.id)}
          <div class="rolegroup">
            <button class="rolehead" onclick={() => overview.toggleRole(group.role.id)}>
              <span class="dot" style="background: {group.role.color}"></span>
              <span class="glyph">{group.role.icon}</span>
              <span class="rname">{group.role.name}</span>
              <span class="num">{plural(group.goals.length, 'goal')}</span>
            </button>

            {#if !overview.collapsed.has(group.role.id)}
              <ul class="plain">
                {#each group.goals as row (row.goal.id)}
                  <li>
                    <button
                      class="goalrow"
                      class:sel={overview.selectedGoal === row.goal.id}
                      class:closed={row.goal.status === 'done' || row.goal.status === 'dropped'}
                      onclick={() =>
                        (overview.selectedGoal =
                          overview.selectedGoal === row.goal.id ? null : row.goal.id)}
                    >
                      <Icon name="target" size={14} />
                      <span class="gname">{row.goal.title}</span>
                      {#if row.goal.status !== 'active'}
                        <span class="tag">{STATUS_LABELS[row.goal.status]}</span>
                      {/if}
                      {#if row.activity}
                        <span class="num">
                          {#if row.activity.actualMinutes > 0}
                            {formatMinutes(row.activity.actualMinutes)}
                          {/if}
                          {#if row.activity.openTasks > 0}
                            · {row.activity.openTasks} open
                          {/if}
                        </span>
                        <span class="when">
                          {row.activity.lastTouched
                            ? friendlyDate(row.activity.lastTouched.slice(0, 10))
                            : 'never touched'}
                        </span>
                      {/if}
                    </button>
                  </li>
                {/each}
              </ul>

              <input
                class="newgoal"
                data-newgoal={group.role.id}
                placeholder="Something you want under {group.role.name}…"
                value={newGoal[group.role.id] ?? ''}
                oninput={(e) => (newGoal = { ...newGoal, [group.role.id]: e.currentTarget.value })}
                onkeydown={(e) => {
                  if (e.key === 'Enter') void addGoal(group.role.id)
                }}
              />
            {/if}
          </div>
        {/each}
      </section>

      <!-- ── Habits ────────────────────────────────────────────────── -->
    {:else}
      <section class="pane">
        {#if tracking.live.length === 0}
          <EmptyState lead="Nothing is being tracked yet.">
            {#snippet note()}
              A tracker is a habit, a dose, a symptom or a number you keep. They are made in a
              journal's settings, beside the page you would tick them on.
            {/snippet}
          </EmptyState>
        {:else}
          {#each tracking.live as tracker (tracker.id)}
            {@const summary = overview.habit(tracker.id)}
            <div class="habit">
              <div class="hhead">
                <TrackerIcon name={tracker.icon} color={tracker.color} size={26} />
                <div class="hwhat">
                  <span class="hname">{tracker.name}</span>
                  <span class="dim">
                    {tracker.cadence ? describeCadence(tracker.cadence) : 'no cadence'}
                    {#if summary.rate !== null}
                      · {Math.round(summary.rate * 100)}% kept
                    {/if}
                  </span>
                </div>
                <div class="hnums">
                  <span class="streak-big"
                    >{describeStreak(summary.streak, tracker.cadence?.per ?? 'day')}</span
                  >
                  {#if summary.best > summary.streak}
                    <span class="dim">best {summary.best}</span>
                  {/if}
                </div>
              </div>

              <HabitHeatmap
                {tracker}
                days={overview.habitDays.filter((d) => d.trackerId === tracker.id)}
                from={overview.habitFrom}
                to={today}
                {weekStart}
              />

              <div class="hfoot">
                <PurposeField
                  value={tracker.purpose}
                  onchange={(next: Purpose | null) =>
                    tracking.saveTracker({ ...$state.snapshot(tracker), purpose: next })}
                  placeholder="Not measuring a goal"
                />
              </div>
            </div>
          {/each}
        {/if}
      </section>
    {/if}
  </div>
</div>

{#if goal}
  <!-- The detail rail, on the right, as every other app's is. -->
  <aside class="detail">
    <header>
      <h2>{goal.title}</h2>
      <button class="quiet" aria-label="Close" onclick={() => (overview.selectedGoal = null)}>
        <Icon name="close" size={15} />
      </button>
    </header>

    <label class="lab" for="g-status">Status</label>
    <select
      id="g-status"
      value={goal.status}
      onchange={(e) =>
        overview.saveGoal({
          ...$state.snapshot(goal),
          status: e.currentTarget.value as GoalStatus,
        })}
    >
      {#each Object.entries(STATUS_LABELS) as [id, label] (id)}
        <option value={id}>{label}</option>
      {/each}
    </select>

    <label class="lab" for="g-horizon">Horizon</label>
    <input
      id="g-horizon"
      type="date"
      value={goal.horizon ?? ''}
      onchange={(e) =>
        overview.saveGoal({
          ...$state.snapshot(goal),
          horizon: e.currentTarget.value || null,
        })}
    />
    <p class="dim small">
      Soft, deliberately. Nothing is ever overdue against a goal and nothing is notified — a horizon
      that nagged would make it a task.
    </p>

    <label class="lab" for="g-notes">What done looks like</label>
    <textarea
      id="g-notes"
      rows="4"
      value={goal.notes}
      onchange={(e) =>
        overview.saveGoal({ ...$state.snapshot(goal), notes: e.currentTarget.value })}
    ></textarea>

    {#if overview.activity.get(goal.id)}
      {@const a = overview.activity.get(goal.id)!}
      <h3>Against it</h3>
      <ul class="tally">
        {#if a.actualMinutes > 0}<li>{formatMinutes(a.actualMinutes)} recorded</li>{/if}
        {#if a.openTasks > 0}<li>{plural(a.openTasks, 'task')} open</li>{/if}
        {#if a.doneTasks > 0}<li>{plural(a.doneTasks, 'task')} finished</li>{/if}
        {#if a.projects > 0}<li>{plural(a.projects, 'project')}</li>{/if}
        {#if a.entries > 0}<li>{plural(a.entries, 'entry', 'entries')}</li>{/if}
        {#if a.readings > 0}<li>{plural(a.readings, 'reading')}</li>{/if}
        {#if a.items > 0}<li>{plural(a.items, 'thing')} on a shelf</li>{/if}
      </ul>
      {#if a.openTasks === 0 && a.actualMinutes === 0 && a.entries === 0 && a.readings === 0}
        <p class="dim small">
          Nothing points at this yet. File a project, an hour or a tracker under it and it starts
          adding up.
        </p>
      {/if}
    {/if}

    <button class="danger" onclick={() => overview.deleteGoal(goal.id)}>Delete goal</button>
  </aside>
{/if}

<style>
  .overview {
    display: flex;
    flex: 1;
    min-width: 0;
    flex-direction: column;
  }

  .bar {
    display: flex;
    flex-direction: column;
    flex: none;
    justify-content: center;
    min-height: var(--header-h);
    padding-top: var(--sp-2);
    border-bottom: 1px solid var(--border);
  }

  .titles {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    padding: 0 var(--sp-4);
  }

  h1 {
    margin: 0;
    font-size: var(--text-lg);
    font-weight: 600;
  }

  .sub,
  .dim {
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .body {
    flex: 1;
    padding: var(--sp-4);
  }

  .pane {
    display: flex;
    max-width: 56rem;
    flex-direction: column;
    gap: var(--sp-4);
  }

  .cards {
    display: grid;
    gap: var(--sp-3);
    grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
  }

  .card {
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
  }

  .card h2 {
    margin: 0 0 var(--sp-1);
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }

  .lead {
    margin: 0;
    font-size: var(--text-md);
  }

  .warn {
    margin: var(--sp-1) 0 0;
    color: var(--danger, #be123c);
    font-size: var(--text-sm);
  }

  .section {
    margin: var(--sp-2) 0 0;
    font-size: var(--text-base);
    font-weight: 600;
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }

  .habit-chip {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-1) var(--sp-3) var(--sp-1) var(--sp-1);
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--bg-raised);
  }

  .habit-chip.on {
    border-color: var(--c);
  }

  .cname {
    font-size: var(--text-sm);
  }

  .streak {
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .notice {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    color: var(--fg-muted);
  }

  .notice p {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading);
  }

  .footnote {
    margin: 0;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    line-height: var(--leading);
  }

  .plain {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .goal-line,
  .goalrow {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    min-height: var(--row-h);
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    text-align: left;
  }

  .goalrow:hover {
    background: var(--bg-hover);
  }

  .goalrow.sel {
    background: var(--bg-active);
  }

  .goalrow.closed .gname {
    color: var(--fg-faint);
    text-decoration: line-through;
  }

  .gname,
  .rname {
    flex: 1 1 auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
  }

  .num,
  .when {
    flex: none;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  .tag {
    flex: none;
    padding: 0 var(--sp-1);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  .rolegroup {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .rolehead {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    min-height: var(--row-h);
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-weight: 600;
    text-align: left;
  }

  .rolehead:hover {
    background: var(--bg-hover);
  }

  .dot {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 50%;
  }

  .glyph {
    flex: none;
    line-height: 1;
  }

  .newgoal {
    width: 100%;
    height: var(--row-h);
    margin-left: var(--sp-2);
    padding: 0 var(--sp-2);
    border: 1px dashed var(--border);
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
  }

  .habit {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .hhead {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }

  .hwhat {
    display: flex;
    flex: 1 1 auto;
    min-width: 0;
    flex-direction: column;
  }

  .hname {
    font-size: var(--text-base);
    font-weight: 550;
  }

  .hnums {
    display: flex;
    flex: none;
    flex-direction: column;
    align-items: flex-end;
  }

  .streak-big {
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }

  .hfoot {
    max-width: 22rem;
  }

  .week-nav {
    display: flex;
    align-items: center;
    gap: 2px;
    padding-right: var(--sp-4);
  }

  .week-nav button {
    display: grid;
    place-items: center;
    min-width: 26px;
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }

  .week-nav button:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .week-nav button:disabled {
    opacity: 0.4;
  }

  .prev {
    display: grid;
    place-items: center;
    transform: rotate(90deg);
  }

  .next {
    display: grid;
    place-items: center;
    transform: rotate(-90deg);
  }

  .detail {
    display: flex;
    flex: none;
    width: 320px;
    flex-direction: column;
    gap: var(--sp-2);
    overflow-y: auto;
    padding: var(--sp-4);
    border-left: 1px solid var(--border);
  }

  .detail header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--sp-2);
  }

  .detail h2 {
    margin: 0;
    font-size: var(--text-md);
    font-weight: 600;
  }

  .detail h3 {
    margin: var(--sp-3) 0 0;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }

  .lab {
    margin-top: var(--sp-2);
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  .detail select,
  .detail input,
  .detail textarea {
    width: 100%;
    padding: var(--sp-1) var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
  }

  .detail textarea {
    resize: vertical;
  }

  .small {
    font-size: var(--text-xs);
    line-height: var(--leading);
  }

  .tally {
    margin: var(--sp-1) 0 0;
    padding: 0;
    color: var(--fg-muted);
    font-size: var(--text-sm);
    list-style: none;
  }

  .primary {
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius-sm);
    background: var(--accent);
    color: var(--on-accent, #fff);
    font-size: var(--text-sm);
  }

  .quiet {
    color: var(--fg-muted);
  }

  .danger {
    margin-top: var(--sp-4);
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--danger, #be123c);
    font-size: var(--text-sm);
  }
</style>
