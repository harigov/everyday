<script lang="ts">
  // One goal, and the projects filed under it, in the todo app.
  //
  // What a project is to its tasks, a goal is to its projects: picking a
  // project in the sidebar lists the work in it, and picking a goal lists the
  // projects that are for it. Picking a goal used to open the pane of every
  // goal with a rail on this one, which answered "how is it going" but not
  // "what am I doing about it" -- and the second is the question somebody
  // asks of a goal in the app where the work is done.
  //
  // The goal's own fields that decide where it sits -- its area and its
  // status -- are on the header, because moving a goal to another part of a
  // life is a thing people do from where they are looking at it. Its targets,
  // notes and horizon stay in the goals pane's rail, one click away, rather
  // than being drawn twice.

  import { DEFAULT_COLORS } from '../lib/colors'
  import { focusOnMount } from '../lib/focus'
  import { plural } from '../lib/format'
  import { GOAL_STATUS_LABELS } from '../lib/labels'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { projectStatusItems, purposeItems } from '../lib/menus'
  import { purpose } from '../lib/purpose.svelte'
  import { app } from '../lib/state.svelte'
  import { todo } from '../lib/todo.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import { GOAL_STATUSES } from '../lib/types'
  import type { GoalId, GoalStatus, Project, ProjectId } from '../lib/types'

  let { id }: { id: GoalId } = $props()

  const goal = $derived(purpose.goal(id))
  const area = $derived(goal ? purpose.area(goal.areaId) : undefined)

  const areas = $derived(goal ? purpose.areasFor(goal) : [])

  const filed = $derived(todo.projectsUnder(id))
  /** Still being worked on, in the order the sidebar draws them. */
  const live = $derived(filed.filter((p) => p.status === 'active' || p.status === 'paused'))
  /** Finished or put away: kept on the page, below and quieter. */
  const finished = $derived(filed.filter((p) => p.status === 'done' || p.status === 'archived'))
  const openTasks = $derived(live.reduce((sum, p) => sum + todo.openCount(p.id), 0))

  /** Projects that could be filed here and are not yet. */
  const fileable = $derived(
    todo.liveProjects.filter((p) => !(p.purpose?.type === 'goal' && p.purpose.id === id)),
  )

  const summary = $derived(
    [
      plural(live.length, 'project'),
      openTasks > 0 ? `${openTasks} open ${openTasks === 1 ? 'task' : 'tasks'}` : null,
      finished.length > 0 ? `${finished.length} finished` : null,
    ]
      .filter(Boolean)
      .join(' · '),
  )

  let draft = $state('')
  let picking = $state('')

  /**
   * Move the goal to the area just picked, or put the select back if the
   * save is refused -- it shows the pick before anything has been written.
   */
  async function moveTo(select: HTMLSelectElement) {
    if (!goal || goal.areaId === select.value) return
    if (!(await purpose.saveGoal({ ...$state.snapshot(goal), areaId: select.value }))) {
      select.value = goal.areaId
    }
  }

  async function setStatus(select: HTMLSelectElement) {
    if (!goal || goal.status === select.value) return
    const status = select.value as GoalStatus
    if (!(await purpose.saveGoal({ ...$state.snapshot(goal), status }))) select.value = goal.status
  }

  /** Is the goal's name being edited in the header? */
  let renaming = $state(false)
  let renameDraft = $state('')

  function startRename() {
    if (!goal) return
    renameDraft = goal.title
    renaming = true
  }

  async function commitRename() {
    const title = renameDraft.trim()
    renaming = false
    if (!goal || !title || title === goal.title) return
    await purpose.saveGoal({ ...$state.snapshot(goal), title })
  }

  /** The goals pane, with its rail open on this goal: targets, notes, horizon. */
  async function openDetails() {
    purpose.selected = id
    await todo.setScope({ kind: 'goals' })
  }

  /** Make a project filed under this goal. The name stays if the save fails. */
  async function addProject() {
    const name = draft.trim()
    if (!name) return
    const color = DEFAULT_COLORS[todo.projects.length % DEFAULT_COLORS.length]!
    if (await todo.newProject(name, color, { type: 'goal', id })) {
      draft = ''
      void purpose.refreshActivity()
    }
  }

  async function fileHere(projectId: ProjectId) {
    picking = ''
    const project = todo.projects.find((p) => p.id === projectId)
    if (!project) return
    await todo.saveProject({ ...$state.snapshot(project), purpose: { type: 'goal', id } })
    void purpose.refreshActivity()
  }

  async function unfile(project: Project) {
    await todo.saveProject({ ...$state.snapshot(project), purpose: null })
    void purpose.refreshActivity()
  }

  function projectMenu(p: Project): MenuItem[] {
    return tidyMenu([
      {
        label: 'Open project',
        icon: 'list',
        run: () => todo.setScope({ kind: 'project', id: p.id }),
      },
      SEP,
      {
        label: 'File under…',
        icon: 'compass',
        items: purposeItems(p.purpose, (next) =>
          todo
            .saveProject({ ...$state.snapshot(p), purpose: next })
            .then(() => purpose.refreshActivity()),
        ),
      },
      {
        label: 'Status',
        icon: 'flag',
        items: projectStatusItems(p.status, (status) =>
          todo.saveProject({ ...$state.snapshot(p), status }),
        ),
      },
      SEP,
      { label: 'Unfile from this goal', icon: 'close', run: () => unfile(p) },
    ])
  }

  /** The pane itself, where there is no project under the pointer. */
  function paneMenu(): MenuItem[] {
    return tidyMenu([
      { label: 'Rename…', icon: 'pencil', run: startRename },
      { label: 'Targets and notes…', icon: 'target', run: openDetails },
      { label: 'All goals', icon: 'layers', run: () => todo.setScope({ kind: 'goals' }) },
    ])
  }
</script>

<div class="pane">
  {#if !goal}
    <!-- Deleted from another window, or a goal the vault no longer has. -->
    <EmptyState lead="This goal is not here any more">
      {#snippet action()}
        <button class="btn" onclick={() => todo.setScope({ kind: 'goals' })}>See every goal</button>
      {/snippet}
    </EmptyState>
  {:else}
    <header class="top" style:--dot={area?.color ?? 'var(--accent)'}>
      <div class="pane-titles">
        <h1 class="pane-title">
          <span class="mark">
            {#if area?.icon}{area.icon}{:else}<Icon name="target" size={15} />{/if}
          </span>
          {#if renaming}
            <input
              class="titleedit"
              aria-label="Goal name"
              bind:value={renameDraft}
              use:focusOnMount
              onblur={commitRename}
              onkeydown={(e) => {
                if (e.key === 'Enter') void commitRename()
                if (e.key === 'Escape') renaming = false
              }}
            />
          {:else}
            <button
              class="name"
              class:closed={goal.status === 'done' || goal.status === 'dropped'}
              title="Rename this goal"
              onclick={startRename}>{goal.title}</button
            >
          {/if}
        </h1>
        <span class="pane-sub">{summary}</span>
      </div>

      <div class="tools">
        <!-- The goal's area, changed where the goal is being looked at. -->
        <label class="pick">
          <span class="dot" aria-hidden="true"></span>
          <span class="sr-only">Area</span>
          <select
            class="select"
            value={goal.areaId}
            disabled={!app.status?.writable}
            onchange={(e) => moveTo(e.currentTarget)}
          >
            {#if !area}
              <!-- An area that has gone -- a goal imported without one, or one
                   whose area another device deleted. Said rather than shown as
                   the first area, which would make that one unpickable. -->
              <option value={goal.areaId} disabled>No area</option>
            {/if}
            {#each areas as a (a.id)}
              <option value={a.id}>{a.name}{a.archived ? ' (retired)' : ''}</option>
            {/each}
          </select>
        </label>
        <label class="pick">
          <span class="sr-only">Status</span>
          <select
            class="select"
            value={goal.status}
            disabled={!app.status?.writable}
            onchange={(e) => setStatus(e.currentTarget)}
          >
            {#each GOAL_STATUSES as status (status)}
              <option value={status}>{GOAL_STATUS_LABELS[status]}</option>
            {/each}
          </select>
        </label>
        <button class="select" onclick={openDetails} title="Targets, notes and horizon">
          Targets and notes
        </button>
      </div>
    </header>

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="scroll list" oncontextmenu={(e) => menu.show(e, paneMenu())}>
      {#if filed.length === 0}
        <p class="blank">
          No projects are filed under this goal yet. Make one below, or file one you already have —
          everything in it then counts towards the goal.
        </p>
      {/if}

      {#if live.length > 0}
        <ul class="plain">
          {#each live as p (p.id)}
            {@const open = todo.openCount(p.id)}
            <li>
              <button
                class="prow"
                class:paused={p.status === 'paused'}
                onclick={() => todo.setScope({ kind: 'project', id: p.id })}
                oncontextmenu={(e) => menu.show(e, projectMenu(p))}
                title={p.notes || p.name}
              >
                <span class="picon">{p.icon}</span>
                <span class="pname">{p.name}</span>
                {#if p.status === 'paused'}<span class="tag">Paused</span>{/if}
                <span class="num">
                  {open > 0 ? `${open} open` : 'Nothing open'}
                </span>
                <span class="pdot" style:--dot={p.color} aria-hidden="true"></span>
                <Icon name="chevron" size={13} />
              </button>
            </li>
          {/each}
        </ul>
      {/if}

      <div class="add">
        <input
          class="newproject"
          data-newgoal="project"
          placeholder="A new project for {goal.title}…"
          aria-label="New project under this goal"
          disabled={!app.status?.writable}
          bind:value={draft}
          onkeydown={(e) => {
            if (e.key === 'Enter') void addProject()
            if (e.key === 'Escape') draft = ''
          }}
        />
        {#if fileable.length > 0}
          <select
            class="select"
            aria-label="File a project you already have under this goal"
            disabled={!app.status?.writable}
            bind:value={picking}
            onchange={() => fileHere(picking)}
          >
            <option value="">File a project here…</option>
            {#each fileable as p (p.id)}
              <option value={p.id}
                >{p.icon}
                {p.name}{p.purpose ? ` — now under ${purpose.label(p.purpose)}` : ''}</option
              >
            {/each}
          </select>
        {/if}
      </div>

      {#if finished.length > 0}
        <h2 class="eyebrow sub">Finished</h2>
        <ul class="plain">
          {#each finished as p (p.id)}
            <li>
              <button
                class="prow closed"
                onclick={() => todo.setScope({ kind: 'project', id: p.id })}
                oncontextmenu={(e) => menu.show(e, projectMenu(p))}
                title={p.notes || p.name}
              >
                <span class="picon">{p.icon}</span>
                <span class="pname">{p.name}</span>
                <span class="tag">{p.status === 'done' ? 'Done' : 'Archived'}</span>
                <span class="pdot" style:--dot={p.color} aria-hidden="true"></span>
                <Icon name="chevron" size={13} />
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
</div>

<style>
  /* The same metrics as the task pane's and the goals pane's, written out
     for the reason `GoalsPane` gives: Svelte scopes styles per component,
     and switching between the three should not move the heading. */
  .pane {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }

  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    height: var(--header-h);
    padding: 0 var(--sp-4);
    flex: none;
  }
  .pane-title {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .mark {
    display: inline-flex;
    font-size: var(--text-base);
    line-height: 1;
    color: var(--dot);
  }
  /* The name is a button so that it can be renamed in place; it reads as the heading. */
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    color: inherit;
    font: inherit;
    letter-spacing: inherit;
    text-align: left;
    cursor: text;
  }
  .titleedit {
    min-width: 0;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
  }
  .name.closed {
    opacity: 0.6;
  }

  .tools {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    flex: none;
  }
  .pick {
    display: inline-flex;
    align-items: center;
  }
  .pick .dot {
    width: 8px;
    height: 8px;
    margin-right: -2px;
    border-radius: 50%;
    background: var(--dot);
  }

  /* The header's small controls, round like the task pane's. */
  .select {
    height: 26px;
    padding: 0 var(--sp-2);
    border: none;
    border-radius: var(--radius-pill);
    background: none;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    cursor: pointer;
  }
  .select:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .list {
    flex: 1;
    min-width: 0;
    padding: var(--sp-2) var(--sp-4) var(--sp-4);
  }
  .list > * {
    max-width: var(--page-w);
  }

  .plain {
    display: flex;
    flex-direction: column;
    gap: 1px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .prow {
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
  .prow:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .prow :global(svg) {
    flex: none;
    color: var(--fg-faint);
  }
  /* A paused project is still there, just not shouting; a finished one is a
     fact worth keeping and not a thing to act on. */
  .prow.paused .pname {
    opacity: 0.6;
  }
  .prow.closed .pname {
    opacity: 0.55;
    text-decoration: line-through;
  }

  .picon {
    flex: none;
    width: 1.2em;
    text-align: center;
    line-height: 1;
  }
  .pname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pdot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--dot);
  }

  .num {
    flex: none;
    color: var(--fg-faint);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
  }

  .tag {
    flex: none;
    padding: 1px 6px;
    border-radius: var(--radius-pill);
    background: var(--bg-active);
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    font-weight: 600;
  }

  .add {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-1);
  }
  .newproject {
    flex: 1;
    min-width: 0;
    height: var(--row-h);
    padding: 0 var(--sp-2);
    border: 1px dashed var(--border);
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
  }
  .newproject:focus {
    outline: none;
    border-style: solid;
    border-color: var(--accent);
    background: var(--bg-raised);
  }
  .add .select {
    flex: none;
    max-width: 16rem;
  }

  .sub {
    display: block;
    margin: var(--sp-6) 0 var(--sp-2);
    padding: 0 var(--sp-2);
  }

  .blank {
    margin: 0 0 var(--sp-3);
    padding: var(--sp-2);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-faint);
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* The goal's name, and under it the two choices and the way to its
     details, each a size a finger can hit. */
  :global([data-phone]) .top {
    height: auto;
    min-height: var(--header-h);
    flex-wrap: wrap;
    gap: var(--sp-1) var(--sp-3);
    padding: var(--sp-2) var(--sp-4);
  }
  :global([data-phone]) .tools {
    flex-wrap: wrap;
  }
  :global([data-phone]) .select {
    height: 40px;
    padding: 0 var(--sp-3);
    font-size: var(--text-base);
  }
  :global([data-phone]) .list {
    padding: var(--sp-2) var(--sp-3) var(--fab-clear);
  }
  :global([data-phone]) .prow {
    min-height: 48px;
    padding: 0 var(--sp-2);
  }
  :global([data-phone]) .newproject {
    height: 44px;
  }
  :global([data-phone]) .add {
    flex-wrap: wrap;
  }
  :global([data-phone]) .add .select {
    max-width: none;
    flex: 1 1 100%;
    border: 1px solid var(--border);
  }

  :global([data-phone]) .add .select {
    min-width: 0;
    width: 100%;
  }
</style>
