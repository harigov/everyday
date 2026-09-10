<script lang="ts">
  // Goals, grouped under the roles they belong to, in the todo app.
  //
  // This was a pane of the Overview, which was the wrong address for it in a
  // way that was only obvious once the Overview stopped being four fixed
  // screens. A goal is what work is *for*: the projects and tasks that make
  // it happen are one click away in this app, "File under" on a project's
  // menu points at it, and reading how a goal is going means reading what is
  // open under it. Keeping it in the reporting app meant leaving the place
  // where the work is to look at the reason for the work.
  //
  // The roles it groups by are not edited here. They are defined once, in
  // Settings under About You, because a role is a fact about the person that
  // four different apps file things under -- not this pane's data.

  import { friendlyDate, formatMinutes, plural } from '../lib/format'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { panels } from '../lib/panels.svelte'
  import { purpose } from '../lib/purpose.svelte'
  import { todo } from '../lib/todo.svelte'
  import { app } from '../lib/state.svelte'
  import type { GoalStatus, RoleId } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'

  let newGoal = $state<Record<string, string>>({})
  let pendingDelete = $state<{ id: string; title: string } | null>(null)

  const STATUS_LABELS: Record<GoalStatus, string> = {
    active: 'Active',
    paused: 'Paused',
    done: 'Done',
    dropped: 'Dropped',
  }

  const goal = $derived(purpose.selected ? purpose.goal(purpose.selected) : undefined)
  const activity = $derived(goal ? (purpose.activity.get(goal.id) ?? null) : null)

  async function add(roleId: RoleId) {
    const title = (newGoal[roleId] ?? '').trim()
    if (!title) return
    newGoal = { ...newGoal, [roleId]: '' }
    const made = await purpose.addGoal(roleId, title)
    if (made) purpose.selected = made.id
  }

  function goalMenu(id: string, title: string, status: GoalStatus): MenuItem[] {
    return tidyMenu([
      {
        label: 'Open',
        icon: 'target',
        disabled: purpose.selected === id,
        run: () => (purpose.selected = id),
      },
      SEP,
      {
        label: 'Status',
        icon: 'flag',
        items: (Object.keys(STATUS_LABELS) as GoalStatus[]).map((s) => ({
          label: STATUS_LABELS[s],
          checked: status === s,
          run: () => {
            const target = purpose.goal(id)
            if (target) void purpose.saveGoal({ ...$state.snapshot(target), status: s })
          },
        })),
      },
      SEP,
      {
        label: 'Delete goal…',
        icon: 'trash',
        danger: true,
        run: () => (pendingDelete = { id, title }),
      },
    ])
  }

  /** The pane itself, where there is no goal under the pointer. */
  function paneMenu(): MenuItem[] {
    return tidyMenu([
      {
        label: 'Roles…',
        icon: 'compass',
        hint: 'in About You',
        run: () => panels.openSettings('profile'),
      },
      { label: 'Back to the tasks', icon: 'list', run: () => todo.setScope({ kind: 'all' }) },
    ])
  }
</script>

<div class="pane">
  <header class="top">
    <h1 class="heading"><Icon name="target" size={15} /> Goals</h1>
    <span class="summary">{plural(purpose.openGoals.length, 'open goal')}</span>
  </header>

  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="scroll goals" oncontextmenu={(e) => menu.show(e, paneMenu())}>
    {#if purpose.roles.filter((r) => !r.archived).length === 0}
      <!-- Roles are never written for somebody. A list of what a life is made
         of is a claim, and an application that filled it in would be telling
         its owner who they are. -->
      <EmptyState lead="Nothing here yet, because nobody has said what your life is made of.">
        {#snippet icon()}<Icon name="compass" size={28} />{/snippet}
        {#snippet note()}
          A <em>role</em> is who you are being — Parent, Work, Myself. A <em>goal</em> is something you
          want under one of them, and it is what a project or an hour gets filed under. Roles are set
          up once, in Settings.
        {/snippet}
        {#snippet action()}
          <button class="btn btn-primary" onclick={() => panels.openSettings('profile')}>
            Set up your roles
          </button>
        {/snippet}
      </EmptyState>
    {:else}
      {#each purpose.byRole as group (group.role.id)}
        <div class="rolegroup">
          <button class="rolehead" onclick={() => purpose.toggleRole(group.role.id)}>
            <span class="dot" style="background: {group.role.color}"></span>
            <span class="glyph">{group.role.icon}</span>
            <span class="rname">{group.role.name}</span>
            <span class="num">{plural(group.goals.length, 'goal')}</span>
          </button>

          {#if !purpose.collapsed.has(group.role.id)}
            <ul class="plain">
              {#each group.goals as row (row.goal.id)}
                <li>
                  <button
                    class="goalrow"
                    class:sel={purpose.selected === row.goal.id}
                    class:closed={row.goal.status === 'done' || row.goal.status === 'dropped'}
                    oncontextmenu={(e) =>
                      menu.show(e, goalMenu(row.goal.id, row.goal.title, row.goal.status))}
                    onclick={() =>
                      (purpose.selected = purpose.selected === row.goal.id ? null : row.goal.id)}
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
                if (e.key === 'Enter') void add(group.role.id)
              }}
            />
          {/if}
        </div>
      {/each}
    {/if}
  </div>
</div>

{#if goal}
  <!-- The detail rail, on the right, as this app's task detail is. -->
  <aside class="detail scroll">
    <header>
      <h2>{goal.title}</h2>
      <button class="quiet" aria-label="Close" onclick={() => (purpose.selected = null)}>
        <Icon name="close" size={15} />
      </button>
    </header>

    <label class="lab" for="g-status">Status</label>
    <select
      id="g-status"
      value={goal.status}
      onchange={(e) =>
        purpose.saveGoal({
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
        purpose.saveGoal({
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
      onchange={(e) => purpose.saveGoal({ ...$state.snapshot(goal), notes: e.currentTarget.value })}
    ></textarea>

    {#if activity}
      <h3>Against it</h3>
      <ul class="tally">
        {#if activity.actualMinutes > 0}<li>
            {formatMinutes(activity.actualMinutes)} recorded
          </li>{/if}
        {#if activity.openTasks > 0}<li>{plural(activity.openTasks, 'task')} open</li>{/if}
        {#if activity.doneTasks > 0}<li>{plural(activity.doneTasks, 'task')} finished</li>{/if}
        {#if activity.projects > 0}<li>{plural(activity.projects, 'project')}</li>{/if}
        {#if activity.entries > 0}<li>{plural(activity.entries, 'entry', 'entries')}</li>{/if}
        {#if activity.readings > 0}<li>{plural(activity.readings, 'reading')}</li>{/if}
        {#if activity.items > 0}<li>{plural(activity.items, 'thing')} on a shelf</li>{/if}
      </ul>
      {#if activity.openTasks === 0 && activity.actualMinutes === 0 && activity.entries === 0 && activity.readings === 0}
        <p class="dim small">
          Nothing points at this yet. File a project, an hour or a tracker under it and it starts
          adding up.
        </p>
      {/if}
    {/if}

    <button
      class="danger"
      disabled={!app.status?.writable}
      onclick={() => (pendingDelete = { id: goal.id, title: goal.title })}
    >
      Delete goal
    </button>
  </aside>
{/if}

{#if pendingDelete}
  <ConfirmDialog
    title={'Delete “' + pendingDelete.title + '”?'}
    detail="Nothing filed under it is deleted. Projects, tasks, hours and entries that pointed at it stop being counted towards any goal. This cannot be undone."
    confirmLabel="Delete goal"
    onconfirm={() => {
      const doomed = pendingDelete
      pendingDelete = null
      if (doomed) void purpose.deleteGoal(doomed.id)
    }}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

<style>
  /* The same metrics the task pane uses, and deliberately: switching between
     the two should not move the heading. Written out rather than shared,
     because Svelte scopes styles per component and the alternative is a
     global class for two panes. */
  .pane {
    display: flex;
    flex: 1;
    min-width: 0;
    flex-direction: column;
  }

  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    flex: none;
    height: var(--header-h);
    padding: 0 var(--sp-4);
  }

  .heading {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    margin: 0;
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
  }

  .summary {
    flex: none;
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .goals {
    flex: 1;
    min-width: 0;
    padding: var(--sp-4);
  }

  .rolegroup {
    max-width: 56rem;
    margin-bottom: var(--sp-6);
  }

  .rolehead {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    padding: var(--sp-2) 0;
    text-align: left;
    color: var(--fg);
    font-size: var(--text-md);
    font-weight: 600;
  }

  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }

  .glyph {
    font-size: var(--text-base);
    line-height: 1;
  }

  .rname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .num,
  .when {
    flex: none;
    color: var(--fg-faint);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }

  .plain {
    display: flex;
    flex-direction: column;
    gap: 1px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .goalrow {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    min-height: var(--row-h);
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    color: var(--fg-muted);
    text-align: left;
    font-size: var(--text-base);
  }
  .goalrow:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .goalrow.sel {
    background: var(--bg-active);
    color: var(--fg);
  }
  /* A finished goal is a fact worth keeping and not a thing to act on. */
  .goalrow.closed .gname {
    opacity: 0.55;
    text-decoration: line-through;
  }

  .gname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tag {
    flex: none;
    padding: 1px 6px;
    border-radius: 999px;
    background: var(--bg-active);
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    font-weight: 600;
  }

  .newgoal {
    width: 100%;
    height: var(--row-h);
    margin-top: var(--sp-1);
    padding: 0 var(--sp-2);
    border: 1px dashed var(--border);
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
  }
  .newgoal:focus {
    outline: none;
    border-style: solid;
    border-color: var(--accent);
    background: var(--bg-raised);
  }

  /* ── The rail ───────────────────────────────────────────────────── */

  .detail {
    flex: none;
    width: 320px;
    padding: var(--sp-4);
    border-left: 1px solid var(--border);
    background: var(--bg-panel);
  }

  .detail header {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    margin-bottom: var(--sp-4);
  }

  .detail h2 {
    flex: 1;
    margin: 0;
    font-size: var(--text-md);
    font-weight: 650;
    line-height: var(--leading-tight);
  }

  .detail h3 {
    margin: var(--sp-5) 0 var(--sp-2);
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }

  .quiet {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .quiet:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .lab {
    display: block;
    margin: var(--sp-4) 0 var(--sp-1);
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }

  .detail select,
  .detail input,
  .detail textarea {
    width: 100%;
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
  }
  .detail textarea {
    resize: vertical;
    line-height: var(--leading-normal);
  }

  .tally {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0;
    padding-left: var(--sp-4);
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }

  .dim {
    color: var(--fg-faint);
  }
  .small {
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
  }

  .danger {
    width: 100%;
    height: 32px;
    margin-top: var(--sp-6);
    border-radius: var(--radius-sm);
    color: var(--danger);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .danger:hover:not(:disabled) {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
  }
  .danger:disabled {
    opacity: 0.4;
  }
</style>
