<script lang="ts">
  // The board view: one column per status, cards in manual order.
  //
  // Only top-level tasks are cards. A subtask is a step inside one piece of
  // work, and giving it its own card would mean a board where "Pack the
  // study" sits in Doing while three of its own steps sit in four other
  // columns -- which tells you less than the progress pill on the parent
  // does.
  //
  // Dragging uses the platform's own drag and drop rather than pointer
  // events: it gives the drag image, the cursor and the escape-to-cancel for
  // free, all of which would otherwise have to be rebuilt badly.

  import { todo } from '../lib/todo.svelte'
  import { friendlyDate, formatMinutes } from '../lib/format'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { taskMenu } from '../lib/menus'
  import Icon from './Icon.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import QuickAdd from './QuickAdd.svelte'
  import type { Task, TaskStatus } from '../lib/types'

  const LABELS: Record<TaskStatus, string> = {
    backlog: 'Backlog',
    todo: 'To do',
    doing: 'Doing',
    blocked: 'Blocked',
    done: 'Done',
    cancelled: 'Cancelled',
  }

  let dragging = $state<Task | null>(null)
  /** Where the card would land: the column and the slot within it. */
  let over = $state<{ status: TaskStatus; index: number } | null>(null)
  let adding = $state<TaskStatus | null>(null)

  function start(e: DragEvent, task: Task) {
    dragging = task
    if (!e.dataTransfer) return
    e.dataTransfer.effectAllowed = 'move'
    // Some payload is required or Firefox refuses to start the drag at all.
    e.dataTransfer.setData('text/plain', task.id)
  }

  /**
   * Which slot the pointer is nearest, by asking the cards where they are.
   *
   * Measuring rather than tracking per-card `dragenter` events: the events
   * fire in an order that depends on how fast the pointer moved, and the
   * geometry does not.
   */
  function slotAt(column: HTMLElement, y: number): number {
    const cards = [...column.querySelectorAll<HTMLElement>('[data-card]')]
    let index = 0
    for (const card of cards) {
      if (card.dataset.card === dragging?.id) continue
      const box = card.getBoundingClientRect()
      if (y > box.top + box.height / 2) index += 1
    }
    return index
  }

  function moveOver(e: DragEvent, status: TaskStatus) {
    if (!dragging) return
    e.preventDefault()
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move'
    over = { status, index: slotAt(e.currentTarget as HTMLElement, e.clientY) }
  }

  async function drop(e: DragEvent, status: TaskStatus) {
    e.preventDefault()
    const task = dragging
    const at = over
    dragging = null
    over = null
    if (!task) return
    await todo.move(task.id, status, at?.status === status ? at.index : 0)
  }

  function progress(task: Task): [number, number] {
    return todo.progressOf(task.id)
  }

  let pendingDelete = $state<Task | null>(null)

  /**
   * A card's menu is the list's menu.
   *
   * The same tasks, drawn two ways, so the actions have one definition in
   * `lib/menus.ts` -- with one difference the board makes on its own: a card
   * is always a top-level task, and "add a subtask" there would add a step
   * to a piece of work whose steps the board deliberately does not draw.
   */
  function cardMenu(task: Task): MenuItem[] {
    return taskMenu(task, { onDelete: () => (pendingDelete = task) })
  }

  /** A column, where there is no card under the pointer. */
  function columnMenu(status: TaskStatus): MenuItem[] {
    return tidyMenu([
      {
        label: `Add to ${LABELS[status]}`,
        icon: 'plus',
        run: () => (adding = status),
      },
      SEP,
      { label: 'Switch to the list', icon: 'list', run: () => todo.setView('list') },
    ])
  }
</script>

<div class="scroll board">
  {#each todo.columns as status (status)}
    {@const cards = todo.column(status)}
    <section
      class="col"
      class:target={over?.status === status && dragging !== null}
      ondragover={(e) => moveOver(e, status)}
      ondragleave={(e) => {
        // Only when the pointer has actually left the column, not when it
        // crosses from the column into a card inside it.
        if (!e.currentTarget.contains(e.relatedTarget as Node)) over = null
      }}
      ondrop={(e) => drop(e, status)}
      oncontextmenu={(e) => menu.show(e, columnMenu(status))}
      aria-label={LABELS[status]}
    >
      <header class="colhead">
        <span class="eyebrow">{LABELS[status]}</span>
        <span class="n">{cards.length}</span>
        <button
          class="add"
          title="Add to {LABELS[status]}"
          aria-label="Add to {LABELS[status]}"
          onclick={() => (adding = adding === status ? null : status)}
        >
          <Icon name="plus" size={14} />
        </button>
      </header>

      {#if adding === status}
        <div class="adder">
          <QuickAdd
            {status}
            placeholder="Add to {LABELS[status]}"
            autofocus
            compact
            onclose={() => (adding = null)}
          />
        </div>
      {/if}

      <div class="cards">
        {#each cards as task, i (task.id)}
          {#if over?.status === status && over.index === i && dragging}
            <div class="slot"></div>
          {/if}
          <article
            class="card"
            class:lifted={dragging?.id === task.id}
            class:sel={todo.selectedTask === task.id}
            data-card={task.id}
            draggable="true"
            ondragstart={(e) => start(e, task)}
            ondragend={() => {
              dragging = null
              over = null
            }}
            oncontextmenu={(e) => menu.show(e, cardMenu(task))}
          >
            <button
              class="cardbody"
              onclick={() => todo.open(todo.selectedTask === task.id ? null : task.id)}
            >
              <span class="title">{task.title}</span>
              {#if task.dueDate || task.estimateMinutes || task.tags.length || progress(task)[1] > 0 || task.priority !== 'none'}
                <span class="meta">
                  {#if task.priority !== 'none' && task.priority !== 'low'}
                    <span class="chip p-{task.priority}">
                      <Icon name="flag" size={10} weight={1.8} />
                      {task.priority}
                    </span>
                  {/if}
                  {#if task.dueDate}
                    <span class="chip due" class:late={todo.overdue(task)}>
                      <Icon name="calendar" size={10} weight={1.8} />
                      {friendlyDate(task.dueDate)}
                    </span>
                  {/if}
                  {#if task.estimateMinutes}
                    <span class="chip">
                      <Icon name="clock" size={10} weight={1.8} />
                      {formatMinutes(task.estimateMinutes)}
                    </span>
                  {/if}
                  {#if progress(task)[1] > 0}
                    <span class="chip">
                      <Icon name="tick" size={10} weight={2} />
                      {progress(task)[0]}/{progress(task)[1]}
                    </span>
                  {/if}
                  {#each task.tags.slice(0, 2) as tag (tag)}<span class="chip tag">{tag}</span
                    >{/each}
                </span>
              {/if}
            </button>
          </article>
        {/each}
        {#if over?.status === status && over.index >= cards.length && dragging}
          <div class="slot"></div>
        {/if}

        {#if cards.length === 0 && adding !== status}
          <button class="empty" onclick={() => (adding = status)}>
            <Icon name="plus" size={13} /> Add
          </button>
        {/if}
      </div>
    </section>
  {/each}
</div>

{#if pendingDelete}
  <ConfirmDialog
    title={'Delete “' + (pendingDelete.title || 'this task') + '”?'}
    detail="Its subtasks and every block of time booked against them go too. This cannot be undone."
    confirmLabel="Delete task"
    onconfirm={() => {
      const doomed = pendingDelete
      pendingDelete = null
      if (doomed) void todo.remove(doomed.id)
    }}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

<style>
  .board {
    flex: 1;
    display: flex;
    gap: var(--sp-3);
    align-items: flex-start;
    padding: var(--sp-2) var(--sp-4) var(--sp-8);
    overflow-x: auto;
  }

  .col {
    width: 268px;
    flex: none;
    display: flex;
    flex-direction: column;
    max-height: 100%;
    padding: var(--sp-2);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    border: 1px solid transparent;
    transition:
      border-color var(--fast) var(--ease),
      background var(--fast) var(--ease);
  }
  .col.target {
    border-color: color-mix(in oklab, var(--accent) 45%, transparent);
    background: color-mix(in oklab, var(--accent) 5%, var(--bg-sunken));
  }

  .colhead {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-1) var(--sp-2) var(--sp-2);
    flex: none;
  }
  .n {
    flex: 1;
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  .add {
    width: 20px;
    height: 20px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .add:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .adder {
    padding: 0 2px var(--sp-2);
  }
  .cards {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    overflow-y: auto;
  }

  .card {
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    border: 1px solid var(--border);
    box-shadow: var(--shadow-sm);
    cursor: grab;
    transition:
      box-shadow var(--fast) var(--ease),
      opacity var(--fast) var(--ease);
  }
  .card:hover {
    box-shadow: var(--shadow);
  }
  .card:active {
    cursor: grabbing;
  }
  /* The card being dragged stays in place, dimmed, so the column does not
     reflow underneath the pointer while the drop target is being chosen. */
  .card.lifted {
    opacity: 0.35;
  }
  .card.sel {
    border-color: var(--accent);
  }

  .cardbody {
    display: block;
    width: 100%;
    padding: var(--sp-2) var(--sp-3);
    text-align: left;
  }
  .title {
    display: block;
    font-size: var(--text-base);
    line-height: var(--leading-snug);
    color: var(--fg);
  }

  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1);
    margin-top: var(--sp-2);
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 10px;
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
  .chip.p-high {
    color: #b45309;
  }
  .chip.p-urgent {
    color: var(--danger);
  }
  .chip.p-medium {
    color: var(--fg-subtle);
  }

  /* Where the card would land. A line, not a gap: a gap would make every
     other card move while you are still deciding. */
  .slot {
    height: 2px;
    margin: -1px 0;
    border-radius: 2px;
    background: var(--accent);
  }

  .empty {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 5px;
    height: 34px;
    border-radius: var(--radius-sm);
    border: 1px dashed var(--border-strong);
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
  .empty:hover {
    color: var(--fg-muted);
    background: var(--bg-hover);
  }
</style>
