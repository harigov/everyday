<script lang="ts">
  // The todo app's half of the sidebar: smart lists above, projects below.
  //
  // The four smart lists are queries, not folders, which is why they sit
  // apart from the project list: "Today" is a question about every project
  // at once, and filing something into it is not a thing you can do.

  import { todo, type Scope } from '../lib/todo.svelte'
  import { purpose } from '../lib/purpose.svelte'
  import { targets } from '../lib/targets.svelte'
  import { describeProgress } from '../lib/tracker'
  import { panels } from '../lib/panels.svelte'
  import { app } from '../lib/state.svelte'
  import { DEFAULT_COLORS } from '../lib/colors'
  import { focusOnMount } from '../lib/focus'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { colourItems, purposeItems } from '../lib/menus'
  import Icon from './Icon.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import type { IconName } from '../lib/icons'
  import type { GoalId, Project, ProjectStatus } from '../lib/types'

  let creating = $state(false)
  let draft = $state('')
  let pendingDelete = $state<Project | null>(null)

  const SMART: { scope: Scope; label: string; icon: IconName }[] = [
    { scope: { kind: 'today' }, label: 'Today', icon: 'sun' },
    { scope: { kind: 'upcoming' }, label: 'Upcoming', icon: 'calendar' },
    { scope: { kind: 'inbox' }, label: 'Inbox', icon: 'inbox' },
    { scope: { kind: 'all' }, label: 'All tasks', icon: 'layers' },
  ]

  function selected(scope: Scope): boolean {
    return scope.kind === 'project'
      ? todo.scope.kind === 'project' && todo.scope.id === scope.id
      : todo.scope.kind === scope.kind
  }

  // Roles and goals load when the pane opens; the count beside the row has
  // to be right before anybody has opened it, or the sidebar would read
  // "Goals" with nothing beside it until it was clicked once.
  // The same goes for how each goal is doing against its targets, which is
  // the meter on its row.
  $effect(() => {
    if (app.supportsOverview) void purpose.load().then(() => targets.refresh())
  })

  const openGoals = $derived(purpose.openGoals.length)

  /**
   * Every goal still being pursued, one row each, in role order.
   *
   * The count alone used to be all the sidebar said, so a goal written under
   * a role showed up nowhere you could click it -- only as a number going up
   * beside a row that opened the whole pane. Grouped by role without a
   * heading per role: the role's glyph and colour on each row say which part
   * of life it belongs to, and a sidebar with one subheading per role would
   * push the projects below the fold.
   */
  const goalRows = $derived(
    purpose.activeRoles.flatMap((role) =>
      purpose.openGoals
        .filter((g) => g.roleId === role.id)
        .sort((a, b) => a.sortOrder - b.sortOrder || a.title.localeCompare(b.title))
        .map((goal) => ({ goal, role })),
    ),
  )

  /** Open the goals pane with its rail already on this goal. */
  async function openGoal(id: GoalId | null) {
    purpose.selected = id
    await todo.setScope({ kind: 'goals' })
  }

  function goalSelected(id: GoalId | null): boolean {
    return todo.scope.kind === 'goals' && purpose.selected === id
  }

  async function create() {
    const name = draft.trim()
    draft = ''
    creating = false
    if (!name) return
    // The id, timestamps and defaults come from the core, for the same
    // reason a journal's do: they are UUIDv7, and `crypto.randomUUID` is
    // both a different ordering and missing outside a secure context.
    await todo.newProject(name, DEFAULT_COLORS[todo.projects.length % DEFAULT_COLORS.length]!)
  }

  async function remove() {
    const p = pendingDelete
    pendingDelete = null
    if (p) await todo.removeProject(p.id)
  }

  /** The project whose name is being edited in place, if any. */
  let renaming = $state<Project | null>(null)
  let renameDraft = $state('')

  async function commitRename() {
    const p = renaming
    const name = renameDraft.trim()
    renaming = null
    if (!p || !name || name === p.name) return
    await todo.saveProject({ ...$state.snapshot(p), name })
  }

  const PROJECT_STATUSES: { id: ProjectStatus; label: string; hint?: string }[] = [
    { id: 'active', label: 'Active' },
    { id: 'paused', label: 'Paused' },
    { id: 'done', label: 'Done' },
    { id: 'archived', label: 'Archived', hint: 'hidden' },
  ]

  /**
   * What a right-click on a project offers.
   *
   * Like the journal rows in the sidebar above, this used to raise the
   * delete confirmation and nothing else -- a destructive action on a
   * gesture with no menu to discover it from.
   */
  function projectMenu(p: Project): MenuItem[] {
    const open = selected({ kind: 'project', id: p.id })
    return tidyMenu([
      {
        label: 'Open project',
        icon: 'list',
        disabled: open,
        run: () => todo.setScope({ kind: 'project', id: p.id }),
      },
      {
        label: 'Add a task here',
        icon: 'plus',
        run: async () => {
          await todo.setScope({ kind: 'project', id: p.id })
          // The capture line belongs to `TodoView`, which is not an ancestor
          // of this one; reaching it by selector is what `Ctrl+F` already
          // does for the search field in `App.svelte`.
          document.querySelector<HTMLInputElement>('.quickadd .field')?.focus()
        },
      },
      SEP,
      {
        label: 'Rename…',
        icon: 'pencil',
        run: () => {
          renameDraft = p.name
          renaming = p
        },
      },
      {
        label: 'Colour',
        dot: p.color,
        items: colourItems(p.color, (color) => todo.saveProject({ ...$state.snapshot(p), color })),
      },
      {
        // The highest-value place in the whole application to set one:
        // everything under this project inherits it, so a body of work is
        // attributed once rather than task by task.
        label: 'File under',
        icon: 'compass',
        items: purposeItems({ ...$state.snapshot(p) }.purpose, (purpose) =>
          todo.saveProject({ ...$state.snapshot(p), purpose }),
        ),
      },
      {
        label: 'Status',
        icon: 'flag',
        items: PROJECT_STATUSES.map((s) => ({
          label: s.label,
          hint: s.hint,
          checked: p.status === s.id,
          run: () => todo.saveProject({ ...$state.snapshot(p), status: s.id }),
        })),
      },
      SEP,
      { label: 'Delete project…', icon: 'trash', danger: true, run: () => (pendingDelete = p) },
    ])
  }

  /** The panel itself, where there is no project under the pointer. */
  function navMenu(): MenuItem[] {
    return tidyMenu([
      { label: 'New project', icon: 'plus', run: () => (creating = true) },
      app.supportsOverview && {
        label: 'Goals',
        icon: 'target',
        checked: todo.scope.kind === 'goals',
        run: () => todo.setScope({ kind: 'goals' }),
      },
      SEP,
      app.supportsOverview && {
        label: 'Roles…',
        icon: 'compass',
        hint: 'in About You',
        run: () => panels.openSettings('profile'),
      },
    ])
  }

  const due = $derived(todo.dueTodayCount)
  const overdue = $derived(todo.stats?.overdue ?? 0)
</script>

<nav class="scroll side-nav" oncontextmenu={(e) => menu.show(e, navMenu())}>
  {#each SMART as item (item.label)}
    <button
      class="side-row"
      class:sel={selected(item.scope)}
      onclick={() => todo.setScope(item.scope)}
    >
      <span class="side-icon" class:today={item.icon === 'sun'}
        ><Icon name={item.icon} size={15} /></span
      >
      <span class="side-text">{item.label}</span>
      {#if item.scope.kind === 'today' && due > 0}
        <span class="side-count strong" class:late={overdue > 0}>{due}</span>
      {:else if item.scope.kind === 'inbox' && todo.openCount(null) > 0}
        <span class="side-count">{todo.openCount(null)}</span>
      {:else if item.scope.kind === 'all' && todo.stats}
        <span class="side-count">{todo.stats.openTasks}</span>
      {/if}
    </button>
  {/each}

  <!-- Goals sit under the four queries and above the projects, because that
       is the order of the answers they give: what is on today, what is left
       this week, and then what any of it is for. They came from the Overview,
       which was a report on the work rather than the place it is done. -->
  {#if app.supportsOverview}
    <div class="side-head">
      <span class="eyebrow">Goals</span>
      <button
        class="side-add"
        title="Roles, in Settings"
        aria-label="Set up your roles"
        onclick={() => panels.openSettings('profile')}
      >
        <Icon name="settings" size={14} />
      </button>
    </div>
    <button class="side-row" class:sel={goalSelected(null)} onclick={() => openGoal(null)}>
      <span class="side-icon"><Icon name="target" size={15} /></span>
      <span class="side-text">What this is all for</span>
      {#if openGoals > 0}<span class="side-count">{openGoals}</span>{/if}
    </button>
    {#each goalRows as { goal, role } (goal.id)}
      {@const measure = targets.headline(goal.id)}
      {@const words = measure ? describeProgress(measure.tracker, measure.progress) : null}
      <!-- The role's colour is set on the whole row rather than on its dot,
           because the meter in place of the dot is drawn in it too. -->
      <button
        class="side-row"
        class:sel={goalSelected(goal.id)}
        class:paused={goal.status === 'paused'}
        style:--dot={role.color}
        onclick={() => openGoal(goal.id)}
        title="{role.name} · {goal.title}"
      >
        <span class="side-icon">
          {#if role.icon}{role.icon}{:else}<Icon name="target" size={14} />{/if}
        </span>
        <span class="side-text">{goal.title}</span>
        {#if words}
          <!-- Its first target, this period: a fill in the role's colour and
               the numbers beside it, so the bar is never the only reading.
               Past a limit is the one state drawn in the danger colour, and
               it says so in the tooltip as well. -->
          <span
            class="gmeter"
            class:over={words.over}
            title="{words.line}{words.status ? ` — ${words.status}` : ''}"
          >
            <span class="gtrack"
              ><span class="gfill" style="width: {Math.min(1, words.ratio) * 100}%"></span></span
            >
            <span class="side-count">{words.short}</span>
          </span>
        {:else}
          <span class="side-dot" aria-hidden="true"></span>
        {/if}
      </button>
    {/each}
  {/if}

  <div class="side-head">
    <span class="eyebrow">Projects</span>
    <button
      class="side-add"
      title="New project"
      aria-label="New project"
      onclick={() => (creating = true)}
    >
      <Icon name="plus" size={15} />
    </button>
  </div>

  {#each todo.liveProjects as p (p.id)}
    {#if renaming?.id === p.id}
      <input
        class="side-new"
        aria-label="Project name"
        bind:value={renameDraft}
        use:focusOnMount
        onblur={commitRename}
        onkeydown={(e) => {
          if (e.key === 'Enter') void commitRename()
          if (e.key === 'Escape') renaming = null
        }}
      />
    {:else}
      <button
        class="side-row"
        class:sel={selected({ kind: 'project', id: p.id })}
        class:paused={p.status === 'paused'}
        onclick={() => todo.setScope({ kind: 'project', id: p.id })}
        oncontextmenu={(e) => menu.show(e, projectMenu(p))}
        title={p.notes || p.name}
      >
        <span class="side-icon">{p.icon}</span>
        <span class="side-text">{p.name}</span>
        {#if todo.openCount(p.id) > 0}<span class="side-count">{todo.openCount(p.id)}</span>{/if}
        <span class="side-dot" style:--dot={p.color} aria-hidden="true"></span>
      </button>
    {/if}
  {/each}

  {#if todo.liveProjects.length === 0 && !creating}
    <p class="blank">No projects yet. Tasks go to the inbox until you make one.</p>
  {/if}

  {#if creating}
    <input
      class="side-new"
      placeholder="Project name"
      bind:value={draft}
      use:focusOnMount
      onblur={create}
      onkeydown={(e) => {
        if (e.key === 'Enter') void create()
        if (e.key === 'Escape') {
          draft = ''
          creating = false
        }
      }}
    />
  {/if}

  {#if todo.stats && todo.stats.loggedMinutes > 0}
    <div class="side-head"><span class="eyebrow">Time</span></div>
    <p class="logged">
      {Math.round(todo.stats.loggedMinutes / 60)}h logged across
      {todo.stats.blocks}
      {todo.stats.blocks === 1 ? 'block' : 'blocks'}
    </p>
  {/if}
</nav>

{#if pendingDelete}
  <ConfirmDialog
    title={'Delete “' + pendingDelete.name + '”?'}
    detail="Its tasks, their subtasks and every block of time booked against them will be removed too. This cannot be undone."
    confirmLabel="Delete project"
    onconfirm={remove}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

<style>
  /* The rows, headings, counts and the new-project field are the shared
     sidebar's -- see "Sidebars" in app.css. Deliberately: every app shares
     the one sidebar, and a row that changed height when you switched would
     read as two applications rather than one. What is the todo app's own:
     the sun's colour, an overdue count, a goal's meter, a paused row and
     the time tally at the foot. */

  .side-icon.today {
    color: #e0a92b;
  }
  /* Something has already slipped, which is worth a colour -- and the
     danger colour wins over the accent the badge would otherwise wear, open
     or not. */
  .side-count.strong.late {
    background: color-mix(in oklab, var(--danger) 14%, transparent);
    color: var(--danger);
  }
  /* A paused project or goal is still there, just not shouting. */
  .side-row.paused .side-text {
    opacity: 0.6;
  }

  .gmeter {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    flex: none;
  }
  /* The track is a lighter step of the fill's own hue, as in `Meter`, so the
     state reads across the whole bar. */
  .gtrack {
    position: relative;
    width: 28px;
    height: 4px;
    border-radius: 2px;
    background: color-mix(in oklab, var(--dot) 22%, transparent);
    overflow: hidden;
  }
  .gfill {
    display: block;
    height: 100%;
    border-radius: 2px;
    background: var(--dot);
  }
  .gmeter.over .gfill {
    background: var(--danger);
  }
  .gmeter.over .gtrack {
    background: color-mix(in oklab, var(--danger) 22%, transparent);
  }

  /* Set in by the rows' own padding, so the lines start where the rows'
     icons do. */
  .blank {
    padding: var(--sp-2) var(--sp-3);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-faint);
  }
  .logged {
    padding: 0 var(--sp-3);
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
</style>
