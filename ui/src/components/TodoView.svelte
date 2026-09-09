<script lang="ts">
  // The todo app's main pane: a header, the capture line, and either view.
  //
  // The quick-add bar sits *above* the list rather than at the bottom of it,
  // because it is the thing you came here to use and it should not move when
  // the list grows.

  import { FILTER_LABELS, TASK_FILTERS, todo } from '../lib/todo.svelte'
  import { formatMinutes } from '../lib/format'
  import Icon from './Icon.svelte'
  import ProgressPie from './ProgressPie.svelte'
  import QuickAdd from './QuickAdd.svelte'
  import TaskList from './TaskList.svelte'
  import TaskBoard from './TaskBoard.svelte'
  import TaskDetail from './TaskDetail.svelte'
  import type { GroupBy } from '../lib/todo.svelte'
  import type { Priority } from '../lib/types'

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

  /**
   * How much of what is in scope is finished, as a dial beside the heading.
   *
   * Over every loaded task rather than over what the filter is showing: the
   * point of the dial is to answer "how far through this am I", and a filter
   * set to "Done" would otherwise report every project as complete.
   */
  const done = $derived(todo.tasks.filter((t) => t.status === 'done').length)
  const counted = $derived(todo.tasks.filter((t) => t.status !== 'cancelled').length)

  /** The tags in scope, most used first, for the tag filter. */
  const tags = $derived(
    [...new Set(todo.tasks.flatMap((t) => t.tags))].sort((a, b) => a.localeCompare(b)),
  )

  // Lend the capture line to the store, so Ctrl/Cmd N and the tray's "add a
  // task" can put the cursor in it without holding a reference to this
  // component. Retired on unmount: a request that arrives while the journal
  // is on screen has to wait for the next mount, not focus a dead input.
  $effect(() => {
    todo.bindCapture(() => {
      // A `bind:this` component instance is `any` to typescript-eslint,
      // which cannot resolve types through a `.svelte` import the way
      // svelte-check does -- and that checker does verify this call.
      // eslint-disable-next-line @typescript-eslint/no-unsafe-call
      capture?.focus()
    })
    return () => todo.bindCapture(null)
  })
</script>

<main class="todo">
  <div class="pane">
    <header class="top">
      <h1 class="heading">
        {#if todo.project}<span class="mark">{todo.project.icon}</span>{/if}
        {#if counted > 0}
          <ProgressPie
            {done}
            total={counted}
            size={15}
            color={todo.accent}
            title="{done} of {counted} done"
          />
        {/if}
        {heading}
      </h1>

      <div class="search">
        <Icon name="search" size={14} />
        <input
          data-search
          type="search"
          placeholder="Filter tasks"
          value={todo.filter}
          oninput={(e) => todo.setFilter(e.currentTarget.value)}
          onkeydown={(e) => {
            if (e.key === 'Escape') todo.setFilter('')
          }}
        />
      </div>

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
        {/if}
      </div>
    </header>

    <div class="bar">
      <QuickAdd
        bind:this={capture}
        placeholder={todo.project ? `Add to ${todo.project.name}` : 'Add a task'}
      />
    </div>

    <!-- The chips are centred on the pane and the two dropdowns are pushed to
         its edges, so the row reads the same as the library's. See `.toolbar`
         in `app.css` for why the ends are separate elements. -->
    <div class="toolbar" style="--tint: {todo.accent}">
      <div class="toolbar-end">
        <select
          class="select"
          aria-label="Priority"
          value={todo.priorityFilter ?? ''}
          onchange={(e) =>
            (todo.priorityFilter = (e.currentTarget.value || null) as Priority | null)}
        >
          <option value="">Any priority</option>
          {#each todo.usedPriorities as p (p)}
            <option value={p}>{p[0]!.toUpperCase() + p.slice(1)}</option>
          {/each}
        </select>
        {#if tags.length > 0}
          <select
            class="select"
            aria-label="Tag"
            value={todo.tagFilter ?? ''}
            onchange={(e) => (todo.tagFilter = e.currentTarget.value || null)}
          >
            <option value="">Any tag</option>
            {#each tags as tag (tag)}<option value={tag}>{tag}</option>{/each}
          </select>
        {/if}
      </div>

      <div class="filters" role="tablist" aria-label="Status">
        {#each TASK_FILTERS as filter (filter)}
          {@const n = todo.countFor(filter)}
          <button
            class="filter"
            class:on={todo.statusFilter === filter}
            role="tab"
            aria-selected={todo.statusFilter === filter}
            onclick={() => todo.setStatusFilter(filter)}
          >
            {FILTER_LABELS[filter]}
            {#if n > 0}<span class="n">{n}</span>{/if}
          </button>
        {/each}
      </div>

      <div class="toolbar-end right">
        {#if todo.narrowed}
          <button class="clear" onclick={() => todo.clearFilters()}>Clear</button>
        {/if}
        <span class="summary">{summary}</span>
      </div>
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
  .todo {
    flex: 1;
    min-width: 0;
    display: flex;
  }
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
  .heading {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .mark {
    font-size: var(--text-base);
    line-height: 1;
  }

  .tools {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    flex: none;
  }

  .views {
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: var(--radius-sm);
    background: var(--bg-active);
  }
  .view {
    width: 26px;
    height: 22px;
    display: grid;
    place-items: center;
    border-radius: 4px;
    color: var(--fg-subtle);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .view:hover {
    color: var(--fg);
  }
  .view.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }

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
  .select:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .select:focus {
    outline: none;
  }

  .bar {
    padding: 0 var(--sp-4) var(--sp-2);
    flex: none;
  }

  /* The same shape as the library's, because it is the same control. */
  .search {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    flex: 1;
    max-width: 300px;
    margin-left: auto;
    height: 30px;
    padding: 0 var(--sp-3);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    color: var(--fg-faint);
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: 0;
    background: none;
    font-size: var(--text-sm);
    color: var(--fg);
    user-select: text;
  }
  .search input:focus {
    outline: none;
  }
  .search input::-webkit-search-cancel-button {
    -webkit-appearance: none;
  }

  .clear {
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    white-space: nowrap;
  }
  .clear:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .summary {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Visually hidden, still announced. */
  .vh {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }
</style>
