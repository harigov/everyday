<script lang="ts">
  // The todo app's half of the sidebar: smart lists above, projects below.
  //
  // The four smart lists are queries, not folders, which is why they sit
  // apart from the project list: "Today" is a question about every project
  // at once, and filing something into it is not a thing you can do.

  import { todo, type Scope } from '../lib/todo.svelte'
  import { DEFAULT_COLORS } from '../lib/colors'
  import { focusOnMount } from '../lib/focus'
  import Icon from './Icon.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import type { IconName } from '../lib/icons'
  import type { Project } from '../lib/types'

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

  const due = $derived(todo.dueTodayCount)
  const overdue = $derived(todo.stats?.overdue ?? 0)
</script>

<nav class="scroll nav">
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
    <button
      class="row"
      class:sel={selected({ kind: 'project', id: p.id })}
      class:paused={p.status === 'paused'}
      style="--dot: {p.color}"
      onclick={() => todo.setScope({ kind: 'project', id: p.id })}
      oncontextmenu={(e) => {
        e.preventDefault()
        pendingDelete = p
      }}
      title={p.notes || p.name}
    >
      <span class="icon">{p.icon}</span>
      <span class="text">{p.name}</span>
      {#if todo.openCount(p.id) > 0}<span class="count">{todo.openCount(p.id)}</span>{/if}
      <span class="dot" aria-hidden="true"></span>
    </button>
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
    height: 29px;
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
    height: 29px;
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
