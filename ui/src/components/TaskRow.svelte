<script module lang="ts">
  import type { TaskId } from '../lib/types'
  import type { Zone } from '../lib/tasklist'

  /**
   * The drag in progress, shared by every row in the list: which task is
   * being carried, and the row and zone it would land in. Module state
   * rather than a prop threaded through the recursion, because a subtask
   * three components down has to know about a drag that began at the top.
   */
  const drag = $state<{
    id: TaskId | null
    over: { id: TaskId; zone: Zone; parentId: TaskId | null } | null
  }>({ id: null, over: null })

  /** The row whose menu is open, which keeps its buttons showing under it. */
  const menuOwner = $state<{ id: TaskId | null }>({ id: null })
</script>

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
  import { zoneAt } from '../lib/tasklist'
  import { tick } from 'svelte'
  import Icon from './Icon.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import ProgressPie from './ProgressPie.svelte'
  import ProposalGhost from './ProposalGhost.svelte'
  import QuickAdd from './QuickAdd.svelte'
  import Self from './TaskRow.svelte'

  let {
    node,
    depth = 0,
    section = '',
  }: {
    node: TaskNode
    depth?: number
    /** The key of the list section this row is drawn in. See `sectionPatch`. */
    section?: string
  } = $props()

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
  let renaming = $state(false)
  let field = $state<HTMLInputElement | null>(null)
  let body = $state<HTMLButtonElement | null>(null)

  /** The parent this row is drawn under, which is not always its record's. */
  const drawnParent = $derived(depth === 0 ? null : (task.parentId ?? null))
  const over = $derived(drag.over?.id === task.id ? drag.over.zone : null)

  /** A pending change to *this* task, if a dream proposed one. */
  const replaceProposal = $derived(todo.replaceProposalFor(task.id))
  const deleteProposal = $derived(todo.deleteProposalFor(task.id))

  function rowMenu() {
    return taskMenu(task, {
      // Two levels is what the list offers, so only a top-level row is
      // asked whether it wants another one under it.
      onAddSubtask: depth === 0 ? () => (adding = true) : undefined,
      onRename: () => (renaming = true),
      onDelete: () => (confirming = true),
    })
  }

  function openMenu(e: MouseEvent) {
    menu.show(e, rowMenu())
    menuOwner.id = task.id
  }

  $effect(() => {
    if (!menu.at && menuOwner.id === task.id) menuOwner.id = null
  })

  $effect(() => {
    if (!field) return
    field.focus()
    field.select()
  })

  async function finishRename(save: boolean) {
    if (!renaming || !field) return
    const title = field.value.trim()
    renaming = false
    // An emptied title is a slip, not a rename: a task with no name is one
    // nobody can find again.
    if (save && title && title !== task.title) todo.patch(task.id, { title })
    await tick()
    body?.focus()
  }

  function onRenameKey(e: KeyboardEvent) {
    // The field has the keyboard. Unstopped, Left and Right would walk the
    // row's controls and take the caret out of the word being edited.
    e.stopPropagation()
    if (e.key === 'Enter') void finishRename(true)
    else if (e.key === 'Escape') void finishRename(false)
  }

  /** What dropping here would mean, if the list allows it at all. */
  function dropTarget(e: DragEvent) {
    if (!drag.id) return null
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    const zone = zoneAt(e.clientY, box, {
      // Only a task with no parent takes a drop onto it. A subtask drawn at
      // the top level because its parent is hidden still has one.
      nest: !task.parentId && todo.progressOf(drag.id)[1] === 0,
      openParent: open && node.children.length > 0,
    })
    const target = { id: task.id, zone, parentId: drawnParent }
    return todo.canDrop(drag.id, target, section) ? target : null
  }

  function onDragOver(e: DragEvent) {
    if (!drag.id) return
    const target = dropTarget(e)
    if (!target) {
      if (over) drag.over = null
      return
    }
    // Accepting is what `preventDefault` means here; left alone, the
    // platform shows the no-entry cursor, which is the refusal.
    e.preventDefault()
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move'
    if (over !== target.zone) drag.over = target
  }

  async function onDrop(e: DragEvent) {
    e.preventDefault()
    const id = drag.id
    const target = drag.over
    drag.id = null
    drag.over = null
    if (id && target?.id === task.id) await todo.place(id, target, section)
  }
</script>

<div class="wrap" style="--depth: {depth}">
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="row"
    class:sel={todo.selectedTask === task.id}
    class:done={task.status === 'done'}
    class:cancelled={task.status === 'cancelled'}
    class:lifted={drag.id === task.id}
    class:drop-before={over === 'before'}
    class:drop-after={over === 'after'}
    class:drop-into={over === 'into'}
    data-row={task.id}
    draggable={!renaming}
    ondragstart={(e) => {
      drag.id = task.id
      if (!e.dataTransfer) return
      e.dataTransfer.effectAllowed = 'move'
      // Some payload is required or Firefox refuses to start the drag at all.
      e.dataTransfer.setData('text/plain', task.id)
    }}
    ondragend={() => {
      drag.id = null
      drag.over = null
    }}
    ondragover={onDragOver}
    ondragleave={(e) => {
      if (over && !e.currentTarget.contains(e.relatedTarget as Node)) drag.over = null
    }}
    ondrop={onDrop}
    oncontextmenu={openMenu}
  >
    <span class="grip" aria-hidden="true"><Icon name="grip" size={14} /></span>

    <button
      class="tick"
      class:on={task.status === 'done'}
      title={task.status === 'done' ? 'Mark as not done' : 'Mark as done'}
      aria-label={task.status === 'done' ? 'Mark as not done' : 'Mark as done'}
      onclick={() => todo.toggleDone(task.id)}
    >
      <Icon name={task.status === 'done' ? 'check' : 'circle'} size={17} weight={1.6} />
    </button>

    {#if renaming}
      <div class="body">
        <input
          bind:this={field}
          class="rename"
          value={task.title}
          aria-label="Task title"
          onkeydown={onRenameKey}
          onblur={() => finishRename(true)}
        />
      </div>
    {:else}
      <button
        bind:this={body}
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
    {/if}

    {#if deleteProposal}
      <!-- A dream wants this gone. Shown as a chip rather than folded into
           the row's own menu, because "somebody proposed deleting this" is
           not a thing you want to notice only after opening a menu. -->
      <div class="delete-chip">
        <ProposalGhost proposal={deleteProposal} compact color="var(--danger)">
          Proposed: delete
        </ProposalGhost>
      </div>
    {/if}

    <!-- On hover, or when the keyboard is in the row: the things done to a
         task often enough to be worth one click rather than a right-click. -->
    <div class="actions" class:held={menu.at !== null && menuOwner.id === task.id}>
      <button class="act" title="Rename" aria-label="Rename" onclick={() => (renaming = true)}>
        <Icon name="pencil" size={13} weight={1.7} />
      </button>
      {#if depth === 0}
        <button
          class="act"
          title="Add a subtask"
          aria-label="Add a subtask"
          onclick={() => (adding = true)}
        >
          <Icon name="plus" size={13} weight={1.8} />
        </button>
      {/if}
      <button
        class="act danger"
        title="Delete"
        aria-label="Delete"
        onclick={() => (confirming = true)}
      >
        <Icon name="trash" size={13} weight={1.7} />
      </button>
      <button
        class="act"
        data-menu-trigger
        title="More"
        aria-label="More actions"
        aria-haspopup="menu"
        onclick={(e) => (menu.at && menuOwner.id === task.id ? menu.close() : openMenu(e))}
      >
        <Icon name="more" size={14} weight={1.8} />
      </button>
    </div>

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
    {/if}
  </div>

  {#if replaceProposal}
    <!-- The proposed after-image, directly under the row it would replace.
         No `children`: the caption alone says what changed ("Update task:
         …"), and opening it is what shows the whole draft. -->
    <div class="replace-ghost">
      <ProposalGhost
        proposal={replaceProposal}
        color={project?.color}
        onopen={() => todo.openProposal(replaceProposal)}
      />
    </div>
  {/if}

  <!-- `node.children.length` and not just `open`: subtasks are expanded by
       default now, so a task with none would otherwise draw an empty
       disclosure containing nothing but its own "Add a subtask" row -- a
       permanent second line under every task in the list. The button is
       still there on hover, at the end of the row. -->
  {#if (open && node.children.length > 0) || adding}
    <div class="kids">
      {#each node.children as child (child.task.id)}
        <Self node={child} depth={depth + 1} {section} />
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
    position: relative;
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

  /* In the gutter to the left of the tick, so showing it moves nothing. */
  .grip {
    position: absolute;
    left: -14px;
    top: calc(var(--sp-2) + 2px);
    display: flex;
    color: var(--fg-faint);
    cursor: grab;
    opacity: 0;
    transition: opacity var(--fast) var(--ease);
  }
  .row:hover .grip {
    opacity: 1;
  }

  .row.lifted {
    opacity: 0.45;
  }
  /* A line where it would land beside, a wash where it would land onto. */
  .row.drop-before::before,
  .row.drop-after::after {
    content: '';
    position: absolute;
    left: var(--sp-2);
    right: 0;
    height: 2px;
    border-radius: 1px;
    background: var(--accent);
    pointer-events: none;
  }
  .row.drop-before::before {
    top: -1px;
  }
  .row.drop-after::after {
    bottom: -1px;
  }
  .row.drop-into {
    background: color-mix(in oklab, var(--accent) 12%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in oklab, var(--accent) 45%, transparent);
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
  .rename {
    width: 100%;
    margin: -2px 0 -2px -5px;
    padding: 1px 4px;
    border: 1px solid var(--accent);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg);
    font-size: var(--text-base);
    line-height: var(--leading-snug);
    outline: none;
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

  .disclose {
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
  .disclose:hover {
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

  /* The row's buttons appear on hover: useful, and not worth a permanent
     column of icons down the list. Kept while the row's own menu is open,
     so the button that opened it does not vanish from under it. */
  .actions {
    display: flex;
    flex: none;
    gap: 1px;
    margin: -1px 0;
    opacity: 0;
    transition: opacity var(--fast) var(--ease);
  }
  .row:hover .actions,
  .row:focus-within .actions,
  .actions.held {
    opacity: 1;
  }
  .row.lifted .actions {
    opacity: 0;
  }
  .act {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .act:hover {
    background: var(--bg-active);
    color: var(--fg);
  }
  .act.danger:hover {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
  }

  /* Always shown, not just on hover -- a proposed deletion is not the kind
     of thing a row should hide until you happen to point at it. */
  .delete-chip {
    flex: none;
    margin-top: 1px;
  }

  .replace-ghost {
    margin: 2px 0 var(--sp-2) 26px;
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
