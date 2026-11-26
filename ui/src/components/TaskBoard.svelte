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
  import { phone } from '../lib/phone.svelte'
  import { friendlyDate, formatMinutes } from '../lib/format'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { taskMenu } from '../lib/menus'
  import { lateLabel } from '../lib/tasklist'
  import { proposals, recordAs } from '../lib/proposals.svelte'
  import Icon from './Icon.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import TaskGhostRow from './TaskGhostRow.svelte'
  import QuickAdd from './QuickAdd.svelte'
  import type { Task, TaskStatus } from '../lib/types'
  import { STATUS_LABELS as LABELS, PRIORITY_LABELS } from '../lib/labels'

  /** Pending task-create proposals for one column, at its foot. Never counted
   *  in the column's own total -- a ghost is not yet a card. */
  function ghostsFor(status: TaskStatus) {
    return todo.ghostTasks.filter((p) => recordAs(p, 'task')?.status === status)
  }

  // Every ghost across every column is "on screen" the moment the board is,
  // since the board (unlike the list) never scrolls a column out of view.
  const allGhosts = $derived(todo.columns.flatMap((s) => ghostsFor(s)))
  $effect(() => {
    if (allGhosts.length > 0) void proposals.markSeen(allGhosts)
  })

  let dragging = $state<Task | null>(null)
  /** Where the card would land: the column and the slot within it. */
  let over = $state<{ status: TaskStatus; index: number } | null>(null)
  let adding = $state<TaskStatus | null>(null)

  // ── On a phone ───────────────────────────────────────────────────────
  //
  // The columns are a strip, one to a screen, that snaps; which one is showing
  // is the tab that is lit, and a tab is also how to get to another.

  let strip = $state<HTMLElement | null>(null)
  let showing = $state(0)

  function onStrip() {
    if (strip) showing = Math.round(strip.scrollLeft / Math.max(1, strip.clientWidth))
  }

  function showColumn(i: number) {
    strip?.scrollTo({ left: i * strip.clientWidth, behavior: 'smooth' })
  }

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
    return taskMenu(task, { reorder: false, onDelete: () => (pendingDelete = task) })
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

{#if phone.on}
  <div class="colbar" role="tablist" aria-label="Columns">
    {#each todo.columns as status, i (status)}
      <button
        class="coltab"
        class:on={showing === i}
        role="tab"
        aria-selected={showing === i}
        onclick={() => showColumn(i)}
      >
        {LABELS[status]}
        <span class="tabn">{todo.column(status).length}</span>
      </button>
    {/each}
  </div>
{/if}

<div class="scroll board" bind:this={strip} onscroll={phone.on ? onStrip : undefined}>
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
            draggable={!phone.on}
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
              {#if task.dueDate || task.estimateMinutes || task.tags.length || progress(task)[1] > 0 || task.priority !== 'none' || (todo.scope.kind !== 'project' && todo.projectOf(task.projectId))}
                <span class="meta">
                  {#if todo.scope.kind !== 'project' && todo.projectOf(task.projectId)}
                    {@const home = todo.projectOf(task.projectId)}
                    <span class="chip project" style="--dot: {home?.color}">
                      <span class="pdot"></span>{home?.name}
                    </span>
                  {/if}
                  {#if task.priority !== 'none' && task.priority !== 'low'}
                    <span class="chip p-{task.priority}">
                      <Icon name="flag" size={10} weight={1.8} />
                      {PRIORITY_LABELS[task.priority]}
                    </span>
                  {/if}
                  {#if task.dueDate}
                    <span class="chip due" class:late={todo.overdue(task)}>
                      <Icon name="calendar" size={10} weight={1.8} />
                      {friendlyDate(task.dueDate)}
                    </span>
                  {/if}
                  {#if todo.daysLate(task) > 0}
                    <span class="chip late">{lateLabel(todo.daysLate(task))}</span>
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

        <!-- Ghosts always sit at the foot of the column, after every real
             card -- never part of the column's own count above. -->
        {#each ghostsFor(status) as p (p.id)}
          <div class="ghost-card">
            <TaskGhostRow proposal={p} />
          </div>
        {/each}
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
    /* Stretch (the flex default) rather than `flex-start`: every column
       should read as one row of boxes, each as tall as the board itself,
       with its own cards scrolling inside -- not as tall as its own cards,
       which left a short column (one card) and a long one (six cards)
       sitting side by side with ragged bottoms. `.col`'s `max-height: 100%`
       below only does anything once stretch gives it a 100% to be capped
       to. */
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
    /* `flex: 1` is what makes the column's own scrollbar do the work: with
       the column now stretched to the board's height, the card list is the
       part that has to give, not the column around it. `min-height: 0`
       because a flex child's content-based minimum height otherwise wins
       over `flex: 1` and the scrollbar never appears. */
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }

  .card {
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    border: 1px solid var(--border);
    cursor: grab;
    /* No resting shadow: a Done column can hold hundreds of cards, and a
       blurred shadow each is what made long lists slow to scroll. The border
       draws the card; hover changes it instead. */
    transition:
      border-color var(--fast) var(--ease),
      background var(--fast) var(--ease),
      opacity var(--fast) var(--ease);
  }
  .card:hover {
    border-color: var(--border-strong);
    background: color-mix(in oklab, var(--bg-raised) 94%, var(--fg));
  }
  .card:active {
    cursor: grabbing;
  }
  /* The card being dragged stays in place, dimmed, so the column does not
     reflow underneath the pointer while the drop target is being chosen. */
  .card.lifted {
    opacity: 0.35;
  }
  .card.sel,
  .card.sel:hover {
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
    font-size: var(--text-xs);
    font-weight: 550;
    color: var(--fg-faint);
    padding: 1px var(--sp-2);
    border-radius: var(--radius-pill);
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
  /* How far behind, filled so it outranks the tinted date beside it. */
  .chip.late {
    color: var(--fg-on-danger);
    background: var(--danger);
  }
  .chip.project {
    background: none;
    padding-left: 0;
  }
  .pdot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--dot);
  }
  .chip.p-high {
    color: var(--warning);
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

  .ghost-card {
    display: flex;
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* The board as a strip of columns, one to a screen, that snaps -- six
     columns side by side would each be a sliver -- under a row of tabs that
     say which is showing and go to another. A card is moved to another column
     by its status, in its page or its menu: not by dragging it across a strip
     that is also being scrolled along. The strip scrolls up and down as a
     whole, for what is longer than the screen. */
  .colbar {
    display: flex;
    flex: none;
    gap: var(--sp-1);
    padding: 0 var(--sp-3) var(--sp-2);
    overflow-x: auto;
    scrollbar-width: none;
    overscroll-behavior-x: contain;
  }
  .colbar::-webkit-scrollbar {
    display: none;
  }
  .coltab {
    display: inline-flex;
    flex: none;
    align-items: center;
    gap: var(--sp-2);
    min-height: 40px;
    padding: 0 var(--sp-3);
    border-radius: var(--radius-pill);
    color: var(--fg-subtle);
    font-size: var(--text-sm);
    font-weight: 550;
    white-space: nowrap;
  }
  .coltab.on {
    background: var(--bg-active);
    color: var(--fg);
  }
  .tabn {
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  :global([data-phone]) .board {
    gap: 0;
    padding: 0 0 var(--fab-clear);
    scroll-snap-type: x mandatory;
    overscroll-behavior-x: contain;
  }
  /* The margin round a column is a transparent border, so that the snap
     lines up its edge and the one before it is wholly off the screen. */
  :global([data-phone]) .col {
    flex: 0 0 100%;
    width: 100%;
    max-height: none;
    align-self: flex-start;
    box-sizing: border-box;
    border: 12px solid transparent;
    border-top-width: 2px;
    border-radius: calc(var(--radius) + 12px);
    background-clip: padding-box;
    scroll-snap-align: start;
    scroll-snap-stop: always;
  }
  :global([data-phone]) .cards {
    flex: none;
    overflow: visible;
  }
  :global([data-phone]) .colhead {
    padding: var(--sp-1) var(--sp-1) var(--sp-2) var(--sp-2);
  }
  :global([data-phone]) .add {
    width: 44px;
    height: 44px;
    margin: -8px 0;
  }
  :global([data-phone]) .card {
    cursor: default;
  }
  :global([data-phone]) .card:active {
    background: var(--bg-hover);
  }
  :global([data-phone]) .cardbody {
    min-height: 48px;
    padding: var(--sp-3);
  }
  :global([data-phone]) .empty {
    height: 48px;
  }
</style>
