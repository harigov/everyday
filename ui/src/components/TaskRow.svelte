<script lang="ts">
  // One task in the list view, with its subtasks nested under it.
  //
  // Recursive by importing itself: the model allows any depth, the interface
  // offers two, and drawing it this way means the third costs nothing if it
  // is ever wanted.

  import { todo, type TaskNode } from '../lib/todo.svelte'
  import { friendlyDate, formatClock, formatMinutes } from '../lib/format'
  import { menu } from '../lib/menu.svelte'
  import { taskMenu } from '../lib/menus'
  import Icon from './Icon.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import ProgressPie from './ProgressPie.svelte'
  import QuickAdd from './QuickAdd.svelte'
  import Self from './TaskRow.svelte'

  let { node, depth = 0 }: { node: TaskNode; depth?: number } = $props()

  const task = $derived(node.task)
  const open = $derived(todo.isExpanded(task.id))
  // A tuple in one binding: `$derived` initialises a single declaration.
  const progress = $derived(todo.progressOf(task.id))
  const overdue = $derived(todo.overdue(task))
  const project = $derived(todo.projectOf(task.projectId))
  /** The project badge is noise inside a project and orientation outside it. */
  const showProject = $derived(project !== null && todo.scope.kind !== 'project')

  let adding = $state(false)
  let confirming = $state(false)
</script>

<div class="wrap" style="--depth: {depth}">
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="row"
    class:sel={todo.selectedTask === task.id}
    class:done={task.status === 'done'}
    class:cancelled={task.status === 'cancelled'}
    data-row={task.id}
    oncontextmenu={(e) =>
      menu.show(
        e,
        taskMenu(task, {
          // Two levels is what the list offers, so only a top-level row is
          // asked whether it wants another one under it.
          onAddSubtask: depth === 0 ? () => (adding = true) : undefined,
          onDelete: () => (confirming = true),
        }),
      )}
  >
    <button
      class="tick"
      class:on={task.status === 'done'}
      title={task.status === 'done' ? 'Mark as not done' : 'Mark as done'}
      aria-label={task.status === 'done' ? 'Mark as not done' : 'Mark as done'}
      onclick={() => todo.toggleDone(task.id)}
    >
      <Icon name={task.status === 'done' ? 'check' : 'circle'} size={17} weight={1.6} />
    </button>

    <button
      class="body"
      data-rowfocus
      onclick={() => todo.open(todo.selectedTask === task.id ? null : task.id)}
    >
      <span class="title">
        {#if task.priority === 'urgent' || task.priority === 'high'}
          <span class="flag p-{task.priority}" title="{task.priority} priority">
            <Icon name="flag" size={12} weight={1.7} />
          </span>
        {/if}
        <span class="titletext">{task.title}</span>
      </span>

      {#if task.dueDate || task.estimateMinutes || task.tags.length || showProject || task.status === 'blocked' || task.status === 'doing'}
        <span class="meta">
          {#if showProject && project}
            <span class="chip project" style="--dot: {project.color}">
              <span class="dot"></span>{project.name}
            </span>
          {/if}
          {#if task.status === 'doing'}<span class="chip state doing">Doing</span>{/if}
          {#if task.status === 'blocked'}<span class="chip state blocked">Blocked</span>{/if}
          {#if task.dueDate}
            <span class="chip due" class:late={overdue}>
              <Icon name="calendar" size={11} weight={1.7} />
              {friendlyDate(task.dueDate)}{task.dueTime ? ` ${formatClock(task.dueTime)}` : ''}
            </span>
          {/if}
          {#if task.estimateMinutes}
            <span class="chip"
              ><Icon name="clock" size={11} weight={1.7} />
              {formatMinutes(task.estimateMinutes)}</span
            >
          {/if}
          {#each task.tags.slice(0, 3) as tag (tag)}<span class="chip tag">{tag}</span>{/each}
        </span>
      {/if}
    </button>

    {#if progress[1] > 0}
      <button
        class="disclose"
        class:open
        title="{progress[0]} of {progress[1]} done — {open ? 'hide' : 'show'} them"
        aria-expanded={open}
        onclick={() => todo.toggleExpanded(task.id)}
      >
        <!-- The dial rather than the fraction, for the reason in
             `ProgressPie`: "2/5" is read and a filled circle is seen. The
             numbers stay beside it, because a dial cannot tell you that the
             five are five. -->
        <ProgressPie
          done={progress[0]}
          total={progress[1]}
          size={14}
          color={project?.color ?? 'var(--journal-accent, var(--accent))'}
        />
        <span class="progress">{progress[0]}/{progress[1]}</span>
        <Icon name="chevron" size={13} weight={1.8} />
      </button>
    {:else if depth === 0}
      <button
        class="addsub"
        title="Add a subtask"
        aria-label="Add a subtask"
        onclick={() => (adding = true)}
      >
        <Icon name="plus" size={13} weight={1.8} />
      </button>
    {/if}
  </div>

  <!-- `node.children.length` and not just `open`: subtasks are expanded by
       default now, so a task with none would otherwise draw an empty
       disclosure containing nothing but its own "Add a subtask" row -- a
       permanent second line under every task in the list. The button is
       still there on hover, at the end of the row. -->
  {#if (open && node.children.length > 0) || adding}
    <div class="kids">
      {#each node.children as child (child.task.id)}
        <Self node={child} depth={depth + 1} />
      {/each}
      {#if adding}
        <div class="subadd">
          <QuickAdd
            parentId={task.id}
            placeholder="Add a subtask"
            autofocus
            compact
            onclose={() => (adding = false)}
          />
        </div>
      {:else}
        <!-- Its own row in the arrow-key sequence, which is what it looks
             like: arrowing down past the last subtask lands here. Without a
             marker it is a control inside the list that the roving tabindex
             does not own, and so a second tab stop for every expanded task. -->
        <button class="addsub-inline" data-row="{task.id}:add" onclick={() => (adding = true)}>
          <Icon name="plus" size={12} weight={1.8} /> Add a subtask
        </button>
      {/if}
    </div>
  {/if}
</div>

{#if confirming}
  <ConfirmDialog
    title={'Delete “' + (task.title || 'this task') + '”?'}
    detail={progress[1] > 0
      ? `Its ${progress[1]} ${progress[1] === 1 ? 'subtask' : 'subtasks'} and every block of time booked against them go too. This cannot be undone.`
      : 'Any time booked against it goes too. This cannot be undone.'}
    confirmLabel="Delete task"
    onconfirm={() => {
      confirming = false
      void todo.remove(task.id)
    }}
    oncancel={() => (confirming = false)}
  />
{/if}

<style>
  .wrap {
    padding-left: calc(var(--depth) * 28px);
  }

  .row {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    border-radius: var(--radius-sm);
    padding: var(--sp-2) var(--sp-2);
    transition: background var(--fast) var(--ease);
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row.sel {
    background: var(--bg-selected);
  }

  .tick {
    display: flex;
    flex: none;
    margin-top: 1px;
    color: var(--fg-faint);
    transition:
      color var(--fast) var(--ease),
      scale var(--fast) var(--ease);
  }
  .tick:hover {
    color: var(--accent);
    scale: 1.08;
  }
  .tick.on {
    color: var(--accent);
  }

  .body {
    flex: 1;
    min-width: 0;
    text-align: left;
    color: inherit;
  }

  .title {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: var(--text-base);
    line-height: var(--leading-snug);
    color: var(--fg);
  }
  .titletext {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Finished work stays legible -- it is the record of what you did -- but
     stops competing with what is left. */
  .row.done .titletext {
    color: var(--fg-subtle);
    text-decoration: line-through;
  }
  .row.cancelled .titletext {
    color: var(--fg-faint);
    text-decoration: line-through;
  }

  .flag {
    display: flex;
    flex: none;
    color: var(--fg-faint);
  }
  .flag.p-high {
    color: #d97706;
  }
  .flag.p-urgent {
    color: var(--danger);
  }

  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-1);
    margin-top: 3px;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: var(--text-xs);
    font-weight: 550;
    color: var(--fg-faint);
    padding: 1px var(--sp-2);
    border-radius: 99px;
    background: var(--bg-sunken);
    white-space: nowrap;
  }
  .chip.tag {
    color: var(--fg-subtle);
  }
  .chip.due.late {
    color: var(--danger);
    background: color-mix(in oklab, var(--danger) 12%, transparent);
  }
  .chip.state.doing {
    color: #0369a1;
    background: color-mix(in oklab, #0369a1 13%, transparent);
  }
  .chip.state.blocked {
    color: #b45309;
    background: color-mix(in oklab, #b45309 15%, transparent);
  }
  .chip.project {
    background: none;
    padding-left: 0;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--dot);
  }

  .disclose,
  .addsub {
    display: flex;
    align-items: center;
    gap: 3px;
    flex: none;
    margin-top: 1px;
    padding: 2px 5px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .disclose:hover,
  .addsub:hover {
    background: var(--bg-active);
    color: var(--fg);
  }
  /* One chevron, rotated, rather than two glyphs that could drift apart. */
  .disclose :global(svg) {
    transition: rotate var(--fast) var(--ease);
  }
  .disclose.open :global(svg) {
    rotate: 90deg;
  }

  /* The per-row add button appears on hover: it is useful, and it is not
     worth a permanent column of plus signs down the list. */
  .addsub {
    opacity: 0;
    transition: opacity var(--fast) var(--ease);
  }
  .row:hover .addsub,
  .addsub:focus-visible {
    opacity: 1;
  }

  .kids {
    padding: 2px 0 var(--sp-2);
  }
  .subadd {
    padding: 2px 0 0 26px;
  }

  .addsub-inline {
    display: flex;
    align-items: center;
    gap: 5px;
    margin-left: 26px;
    padding: 3px var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
  .addsub-inline:hover {
    background: var(--bg-hover);
    color: var(--fg-muted);
  }
</style>
