<script lang="ts">
  // The todo app's main pane: a header, the capture line, and either view.
  //
  // The quick-add bar sits *above* the list rather than at the bottom of it,
  // because it is the thing you came here to use and it should not move when
  // the list grows.

  import { untrack } from 'svelte'
  import { FILTER_LABELS, TASK_FILTERS, todo } from '../lib/todo.svelte'
  import { details, phone } from '../lib/phone.svelte'
  import { purpose } from '../lib/purpose.svelte'
  import { formatMinutes } from '../lib/format'
  import Icon from './Icon.svelte'
  import ProgressPie from './ProgressPie.svelte'
  import GoalsPane from './GoalsPane.svelte'
  import GoalProjects from './GoalProjects.svelte'
  import QuickAdd from './QuickAdd.svelte'
  import TaskList from './TaskList.svelte'
  import TaskBoard from './TaskBoard.svelte'
  import TaskDetail from './TaskDetail.svelte'
  import TaskProposalDetail from './TaskProposalDetail.svelte'
  import type { GroupBy } from '../lib/todo.svelte'
  import type { Priority } from '../lib/types'

  void todo.start()

  // ── On a phone ───────────────────────────────────────────────────────
  //
  // The list is the page, and what is opened from it -- a task, a proposed
  // task, a goal in the goals pane -- is a page laid over it, with a way back
  // (`PhoneBack`, in its own header; see `details` in `phone.svelte.ts`).
  // Whatever opens something here -- a row, a card, a menu's "Open details",
  // the goals pane -- only says which one is selected; this keeps the flag
  // true to that, and the selection true to the flag when back closes it.

  /** What is open over the list, if anything: a task, a proposal or a goal. */
  const selection = $derived(
    todo.showingGoals
      ? purpose.selected
      : todo.goal
        ? null
        : (todo.selectedTask ?? todo.selectedProposal?.id ?? null),
  )

  $effect(() => {
    const open = selection !== null
    untrack(() => (open ? details.open('todo') : details.close('todo')))
  })

  // Back, by the button or Android's: the page goes, and the selection with
  // it -- a row left selected under a closed page would be "Close details" in
  // its menu and a toggle off on its next tap.
  let wasOpen = false
  $effect(() => {
    const open = details.isOpen('todo')
    if (wasOpen && !open) {
      untrack(() => {
        if (todo.showingGoals) purpose.selected = null
        else void todo.open(null)
      })
    }
    wasOpen = open
  })

  let capture = $state<ReturnType<typeof QuickAdd> | null>(null)

  const HEADINGS: Record<string, string> = {
    today: 'Today',
    upcoming: 'Upcoming',
    inbox: 'Inbox',
    all: 'All tasks',
    goals: 'Goals',
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

<!-- Goals are not tasks, so what they get is not a task list: no capture line
     for a task, no status chips, no board, and a detail rail of their own. It
     replaces the whole of this app's pane rather than being a branch inside
     it, because `.pane` is a *column* -- a rail rendered in there would sit
     under the list rather than beside it. -->
{#if todo.showingGoals}
  <main class="todo" class:layered={phone.on && details.isOpen('todo')}><GoalsPane /></main>
{:else if todo.goal}
  <!-- One goal: the projects filed under it, as a project is its tasks. -->
  <main class="todo"><GoalProjects id={todo.goal} /></main>
{:else}
  <main class="todo" class:layered={phone.on && details.isOpen('todo')}>
    <div class="pane">
      <header class="top">
        <div class="pane-titles">
          <h1 class="pane-title">
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
          <span class="pane-sub">{summary}</span>
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
        </div>
      </div>

      {#if todo.view === 'board' && todo.boardable}
        <TaskBoard />
      {:else}
        <TaskList />
      {/if}
    </div>

    {#if todo.selectedProposal}
      <!-- Its own header carries its close; a phone puts it over the list. -->
      <div class="proposalpage">
        <TaskProposalDetail proposal={todo.selectedProposal} />
      </div>
    {:else}
      <TaskDetail />
    {/if}
  </main>
{/if}

<style>
  .todo {
    /* As tall as the stage by a stated height, not stretched: see
       `.shell-stage` in app.css. */
    height: 100%;
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
  /* The heading's size is the shared `.pane-title`'s; on its line, before
     the name, sit a project's own mark and the dial of how much is done. */
  .pane-title {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
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

  /* Round, like every other small control in a header now, and a pill
     around them like the filter chips'. */
  .views {
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: 999px;
    background: var(--bg-active);
  }
  .view {
    width: 26px;
    height: 22px;
    display: grid;
    place-items: center;
    border-radius: 999px;
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
    padding: 0 var(--sp-2);
    border: none;
    border-radius: 999px;
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

  /* `.toolbar`'s ends carry more than most of the shared filter bar's other
     callers: two `<select>`s on the left, and a Clear button on the right
     while anything is narrowed. Its `.toolbar-end` sets `min-width: 0` so
     the centred chips always win any argument over space, which at a
     narrow window left the selects with nowhere to shrink to -- they
     painted straight over the filter chips instead of wrapping below them.
     Restoring a real minimum on both ends and letting the row wrap is what
     lets each one drop to a line of its own instead. */
  .toolbar {
    flex-wrap: wrap;
  }
  .toolbar-end {
    min-width: max-content;
  }

  .bar {
    padding: 0 var(--sp-4) var(--sp-2);
    flex: none;
  }

  .clear {
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    white-space: nowrap;
  }
  .clear:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  /* Visually hidden, still announced. */
  .vh {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* The page for what is opened is laid over the list, which is hidden, not
     taken away, so it is as it was -- scrolled to the same place -- when the
     page is put away. */
  :global([data-phone]) .todo {
    position: relative;
  }
  :global([data-phone]) .todo.layered > :global(.pane) {
    visibility: hidden;
  }
  /* Not a box of its own on a desktop: the rail stays the flex item it was. */
  .proposalpage {
    display: contents;
  }
  :global([data-phone]) .proposalpage {
    position: absolute;
    inset: 0;
    z-index: 3;
    display: flex;
    background: var(--bg-panel);
  }
  :global([data-phone]) .proposalpage > :global(.detail) {
    flex: 1;
    width: auto;
    min-width: 0;
    border-left: none;
  }
  :global([data-phone]) .proposalpage :global(.close) {
    width: 44px;
    height: 44px;
  }

  /* The header: the name and how it stands, and under them the two choices
     that shape the list, each a size a finger can hit. */
  :global([data-phone]) .top {
    height: auto;
    min-height: var(--header-h);
    flex-wrap: wrap;
    gap: var(--sp-1) var(--sp-3);
    padding: var(--sp-2) var(--sp-4) var(--sp-1);
  }
  :global([data-phone]) .tools {
    flex-wrap: wrap;
    gap: var(--sp-1);
  }
  :global([data-phone]) .view {
    width: 40px;
    height: 34px;
  }
  :global([data-phone]) .views {
    padding: 3px;
  }
  :global([data-phone]) .select {
    height: 40px;
    padding: 0 var(--sp-3);
    font-size: var(--text-base);
  }
  :global([data-phone]) .clear {
    height: 40px;
    padding: 0 var(--sp-3);
    font-size: var(--text-base);
  }
  :global([data-phone]) .bar {
    padding: var(--sp-1) var(--sp-4) var(--sp-2);
  }

  /* Priority and tag, then the status chips as a strip that slides sideways
     -- one line rather than a block of wrapped chips over the list. */
  :global([data-phone]) .toolbar {
    flex-direction: column;
    /* Not the desktop's wrapping: in a column that makes each line as wide as
       its widest item, which is a strip of chips that never needs to scroll. */
    flex-wrap: nowrap;
    align-items: stretch;
    gap: var(--sp-1);
    padding: 0 0 var(--sp-2);
  }
  :global([data-phone]) .toolbar-end {
    flex: none;
    flex-wrap: wrap;
    padding: 0 var(--sp-3);
    min-width: 0;
  }
  :global([data-phone]) .toolbar .filters {
    order: 2;
    flex-wrap: nowrap;
    justify-content: flex-start;
    overflow-x: auto;
    margin: 0 var(--sp-4);
    scrollbar-width: none;
    overscroll-behavior-x: contain;
  }
  :global([data-phone]) .toolbar .filters::-webkit-scrollbar {
    display: none;
  }
  :global([data-phone]) .toolbar .filter {
    flex: none;
    min-height: 40px;
  }
  :global([data-phone]) .toolbar-end.right {
    order: 1;
  }

  /* The name has the line to itself, and the choices that shape the list are
     the line under it. */
  :global([data-phone]) .pane-titles {
    flex: 1 0 100%;
  }
  :global([data-phone]) .tools {
    flex: 1 0 100%;
    justify-content: space-between;
  }
</style>
