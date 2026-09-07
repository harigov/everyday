<script lang="ts">
  // The todo app's main pane: a header, the capture line, and either view.
  //
  // The quick-add bar sits *above* the list rather than at the bottom of it,
  // because it is the thing you came here to use and it should not move when
  // the list grows.

  import { todo } from '../lib/todo.svelte'
  import { formatMinutes } from '../lib/format'
  import Icon from './Icon.svelte'
  import QuickAdd from './QuickAdd.svelte'
  import TaskList from './TaskList.svelte'
  import TaskBoard from './TaskBoard.svelte'
  import TaskDetail from './TaskDetail.svelte'
  import type { GroupBy } from '../lib/todo.svelte'

  void todo.start()

  let capture = $state<ReturnType<typeof QuickAdd> | null>(null)

  const HEADINGS: Record<string, string> = {
    today: 'Today',
    upcoming: 'Upcoming',
    inbox: 'Inbox',
    all: 'All tasks',
  }

  const heading = $derived(
    todo.scope.kind === 'project'
      ? (todo.project?.name ?? 'Project')
      : (HEADINGS[todo.scope.kind] ?? 'Tasks'),
  )

  const GROUPS: { id: GroupBy; label: string }[] = [
    { id: 'due', label: 'Due date' },
    { id: 'status', label: 'Status' },
    { id: 'priority', label: 'Priority' },
    { id: 'none', label: 'Nothing' },
  ]

  /** The one-line summary in the header: what is left, and what it cost. */
  const summary = $derived.by(() => {
    const open = todo.tasks.filter((t) => t.status !== 'done' && t.status !== 'cancelled').length
    const bits = [`${open} open`]
    const overdue = todo.tasks.filter((t) => todo.overdue(t)).length
    if (overdue > 0) bits.push(`${overdue} overdue`)
    const estimated = todo.tasks
      .filter((t) => t.status !== 'done' && t.status !== 'cancelled')
      .reduce((sum, t) => sum + (t.estimateMinutes ?? 0), 0)
    if (estimated > 0) bits.push(`${formatMinutes(estimated)} estimated`)
    return bits.join(' · ')
  })

  /** Focus the capture line. Bound to Ctrl/Cmd N by the app shell. */
  export function focusCapture() {
    capture?.focus()
  }
</script>

<main class="todo">
  <div class="pane">
    <header class="top">
      <h1 class="heading">
        {#if todo.project}<span class="mark">{todo.project.icon}</span>{/if}
        {heading}
      </h1>

      <div class="tools">
        {#if todo.boardable}
          <div class="views" role="group" aria-label="View">
            <button
              class="view"
              class:on={todo.view === 'list'}
              title="List"
              aria-label="List view"
              aria-pressed={todo.view === 'list'}
              onclick={() => todo.setView('list')}
            >
              <Icon name="list" size={15} />
            </button>
            <button
              class="view"
              class:on={todo.view === 'board'}
              title="Board"
              aria-label="Board view"
              aria-pressed={todo.view === 'board'}
              onclick={() => todo.setView('board')}
            >
              <Icon name="board" size={15} />
            </button>
          </div>
        {/if}

        {#if todo.view === 'list'}
          <label class="group">
            <span class="vh">Group by</span>
            <select
              class="select"
              value={todo.groupBy}
              onchange={(e) => (todo.groupBy = e.currentTarget.value as GroupBy)}
            >
              {#each GROUPS as g (g.id)}<option value={g.id}>Group: {g.label}</option>{/each}
            </select>
          </label>

          <button
            class="toggle"
            class:on={todo.showDone}
            title={todo.showDone ? 'Hide finished tasks' : 'Show finished tasks'}
            aria-pressed={todo.showDone}
            onclick={() => (todo.showDone = !todo.showDone)}
          >
            <Icon name="tick" size={14} weight={2} />
            Done
          </button>
        {/if}
      </div>
    </header>

    <div class="bar">
      <QuickAdd bind:this={capture} placeholder={todo.project ? `Add to ${todo.project.name}` : 'Add a task'} />
    </div>

    <div class="filterbar">
      <span class="glass"><Icon name="search" size={14} /></span>
      <input
        class="filter"
        type="search"
        placeholder="Filter tasks"
        value={todo.filter}
        oninput={(e) => todo.setFilter(e.currentTarget.value)}
        onkeydown={(e) => { if (e.key === 'Escape') todo.setFilter('') }}
      />
      <span class="summary">{summary}</span>
    </div>

    {#if todo.view === 'board' && todo.boardable}
      <TaskBoard />
    {:else}
      <TaskList />
    {/if}
  </div>

  <TaskDetail />
</main>

<style>
  .todo { flex: 1; min-width: 0; display: flex; }
  .pane { flex: 1; min-width: 0; display: flex; flex-direction: column; }

  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    height: 46px;
    padding: 0 var(--sp-4);
    flex: none;
  }
  .heading {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .mark { font-size: var(--text-base); line-height: 1; }

  .tools { display: flex; align-items: center; gap: var(--sp-2); flex: none; }

  .views {
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: var(--radius-sm);
    background: var(--bg-active);
  }
  .view {
    width: 26px; height: 22px;
    display: grid; place-items: center;
    border-radius: 4px;
    color: var(--fg-subtle);
    transition: background var(--fast) var(--ease), color var(--fast) var(--ease);
  }
  .view:hover { color: var(--fg); }
  .view.on { background: var(--bg-raised); color: var(--fg); box-shadow: var(--shadow-sm); }

  .select {
    height: 26px;
    padding: 0 var(--sp-1);
    border: none;
    border-radius: var(--radius-sm);
    background: none;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    cursor: pointer;
  }
  .select:hover { background: var(--bg-hover); color: var(--fg); }
  .select:focus { outline: none; }

  .toggle {
    display: flex; align-items: center; gap: 5px;
    height: 26px; padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
  .toggle:hover { background: var(--bg-hover); color: var(--fg-muted); }
  .toggle.on { background: var(--bg-active); color: var(--fg); }

  .bar { padding: 0 var(--sp-4) var(--sp-2); flex: none; }

  .filterbar {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-1) var(--sp-4) var(--sp-2);
    flex: none;
  }
  .glass {
    position: absolute;
    left: calc(var(--sp-4) + 8px);
    display: flex;
    color: var(--fg-faint);
    pointer-events: none;
  }
  .filter {
    width: 200px;
    height: 26px;
    padding: 0 var(--sp-2) 0 28px;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    color: var(--fg);
    user-select: text;
  }
  .filter::placeholder { color: var(--fg-faint); }
  .filter::-webkit-search-cancel-button { -webkit-appearance: none; }
  .filter:focus { outline: none; border-color: var(--accent); background: var(--bg-raised); }

  .summary {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }

  /* Visually hidden, still announced. */
  .vh {
    position: absolute;
    width: 1px; height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
</style>
