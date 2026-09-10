<script lang="ts">
  // The todo app's half of the sidebar: smart lists above, projects below.
  //
  // The four smart lists are queries, not folders, which is why they sit
  // apart from the project list: "Today" is a question about every project
  // at once, and filing something into it is not a thing you can do.

  import { todo, type Scope } from '../lib/todo.svelte'
  import { purpose } from '../lib/purpose.svelte'
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
  import type { Project, ProjectStatus } from '../lib/types'

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
  $effect(() => {
    if (app.supportsOverview) void purpose.load()
  })

  const openGoals = $derived(purpose.openGoals.length)

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

<nav class="scroll nav" oncontextmenu={(e) => menu.show(e, navMenu())}>
  {#each SMART as item (item.label)}
    <button class="row" class:sel={selected(item.scope)} onclick={() => todo.setScope(item.scope)}>
      <span class="icon" class:today={item.icon === 'sun'}><Icon name={item.icon} size={15} /></span
      >
      <span class="text">{item.label}</span>
      {#if item.scope.kind === 'today' && due > 0}
        <span class="count" class:late={overdue > 0}>{due}</span>
      {:else if item.scope.kind === 'inbox' && todo.openCount(null) > 0}
        <span class="count">{todo.openCount(null)}</span>
      {:else if item.scope.kind === 'all' && todo.stats}
        <span class="count">{todo.stats.openTasks}</span>
      {/if}
    </button>
  {/each}

  <!-- Goals sit under the four queries and above the projects, because that
       is the order of the answers they give: what is on today, what is left
       this week, and then what any of it is for. They came from the Overview,
       which was a report on the work rather than the place it is done. -->
  {#if app.supportsOverview}
    <div class="head">
      <span class="eyebrow">Goals</span>
      <button
        class="plus"
        title="Roles, in Settings"
        aria-label="Set up your roles"
        onclick={() => panels.openSettings('profile')}
      >
        <Icon name="settings" size={14} />
      </button>
    </div>
    <button
      class="row"
      class:sel={selected({ kind: 'goals' })}
      onclick={() => todo.setScope({ kind: 'goals' })}
    >
      <span class="icon"><Icon name="target" size={15} /></span>
      <span class="text">What this is all for</span>
      {#if openGoals > 0}<span class="count">{openGoals}</span>{/if}
    </button>
  {/if}

  <div class="head">
    <span class="eyebrow">Projects</span>
    <button
      class="plus"
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
        class="new"
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
        class="row"
        class:sel={selected({ kind: 'project', id: p.id })}
        class:paused={p.status === 'paused'}
        style="--dot: {p.color}"
        onclick={() => todo.setScope({ kind: 'project', id: p.id })}
        oncontextmenu={(e) => menu.show(e, projectMenu(p))}
        title={p.notes || p.name}
      >
        <span class="icon">{p.icon}</span>
        <span class="text">{p.name}</span>
        {#if todo.openCount(p.id) > 0}<span class="count">{todo.openCount(p.id)}</span>{/if}
        <span class="dot" aria-hidden="true"></span>
      </button>
    {/if}
  {/each}

  {#if todo.liveProjects.length === 0 && !creating}
    <p class="blank">No projects yet. Tasks go to the inbox until you make one.</p>
  {/if}

  {#if creating}
    <input
      class="new"
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
    <div class="head"><span class="eyebrow">Time</span></div>
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
  /* Deliberately the same metrics as the journal nav: the two apps share a
     sidebar, and a row that changed height when you switched would read as
     two applications rather than one. */
  .nav {
    flex: 1;
    padding: var(--sp-2) var(--sp-2) var(--sp-4);
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--sp-5) var(--sp-2) var(--sp-2);
  }
  .plus {
    width: 20px;
    height: 20px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .plus:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    height: var(--row-h);
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-base);
    color: var(--fg-muted);
    text-align: left;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .row:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .row.sel {
    background: var(--bg-active);
    color: var(--fg);
    font-weight: 550;
  }
  /* A paused project is still there, just not shouting. */
  .row.paused .text {
    opacity: 0.6;
  }

  .icon {
    width: 16px;
    height: 16px;
    flex: none;
    display: grid;
    place-items: center;
    font-size: var(--text-sm);
    line-height: 1;
  }
  .icon.today {
    color: #e0a92b;
  }
  .text {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  /* Something has already slipped, which is worth a colour. */
  .count.late {
    color: var(--danger);
  }

  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--dot);
    flex: none;
    opacity: 0.85;
  }

  .new {
    width: 100%;
    height: var(--row-h);
    margin-top: 2px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--accent);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    font-size: var(--text-base);
    user-select: text;
  }
  .new:focus {
    outline: none;
  }

  .blank {
    padding: var(--sp-2);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-faint);
  }
  .logged {
    padding: 0 var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
</style>
