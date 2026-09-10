<script lang="ts">
  // Everything about one task, and the time booked against it.
  //
  // The panel edits the object that is already in the list rather than a
  // copy of it, so the row behind updates as you type and there is never a
  // moment where the two disagree about what the task says.

  import { todo } from '../lib/todo.svelte'
  import PurposeField from './PurposeField.svelte'
  import { formatInstantTime, formatMinutes, friendlyDate, toLocalInputValue } from '../lib/format'
  import Icon from './Icon.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import { PRIORITIES, TASK_STATUSES } from '../lib/types'
  import type { BlockKind, Priority, Purpose, QuickTaskDraft, TaskStatus } from '../lib/types'
  import { api } from '../lib/api'
  import { ask, quick, slot } from '../lib/quick.svelte'
  import Suggestions from './Suggestions.svelte'

  const STATUS_LABELS: Record<TaskStatus, string> = {
    backlog: 'Backlog',
    todo: 'To do',
    doing: 'Doing',
    blocked: 'Blocked',
    done: 'Done',
    cancelled: 'Cancelled',
  }

  const task = $derived(todo.detail)
  const project = $derived(todo.projectOf(task?.projectId))
  const time = $derived(todo.detailMinutes)

  let confirming = $state(false)
  let booking = $state(false)
  let bookStart = $state('')
  let bookMinutes = $state(30)
  let bookKind = $state<BlockKind>('actual')
  let tagDraft = $state('')

  // ── breaking it down, and sizing it ──────────────────────────────────
  //
  // Both on demand, from the panel, and both proposals. The estimate is the
  // interesting one: the model is handed what *this person's* similar tasks
  // were actually estimated at, so the answer is grounded in how they size
  // work rather than in the model's own sense of how long things take.

  let stepChips = $state<{ key: string; label: string }[]>([])
  let steps = $state<QuickTaskDraft[]>([])
  let breaking = $state(false)
  let estimate = $state<number | null>(null)
  const stepSlot = slot<QuickTaskDraft[]>()

  async function breakDown() {
    const target = task
    if (!target) return
    breaking = true
    await todo.flush()
    await stepSlot.run(
      'todo.subtasks',
      () => api.quickSubtasks(target.id),
      (found) => {
        breaking = false
        steps = found ?? []
        stepChips = steps.map((t, i) => ({ key: String(i), label: t.title }))
      },
    )
    breaking = false
  }

  async function acceptStep(key: string) {
    const draft = steps[Number(key)]
    const target = task
    if (!draft || !target) return
    // A subtask is a task with a parent, so this is the ordinary add with the
    // parent named -- not a second way of making one.
    await todo.add(draft.title, { parentId: target.id })
  }

  async function suggestEstimate() {
    const target = task
    if (!target) return
    await todo.flush()
    estimate = await ask('todo.estimate', () => api.quickEstimate(target.id))
  }

  function openBooking() {
    const now = new Date()
    // Round back to the last half hour: nobody starts work at 14:37, and a
    // field pre-filled with 14:37 has to be corrected before it is useful.
    now.setMinutes(now.getMinutes() - (now.getMinutes() % 30), 0, 0)
    bookStart = toLocalInputValue(now)
    bookMinutes = task?.estimateMinutes ?? 30
    booking = true
  }

  async function book() {
    if (!bookStart || bookMinutes <= 0) return
    await todo.addBlock(bookStart, bookMinutes, bookKind)
    booking = false
  }

  function addTag() {
    const tag = tagDraft.trim().replace(/^#/, '')
    tagDraft = ''
    if (!task || !tag) return
    if (task.tags.some((t) => t.toLowerCase() === tag.toLowerCase())) return
    todo.patch(task.id, { tags: [...task.tags, tag] })
  }

  /** `<input type="number">` gives '' for a cleared field, which means "none". */
  function minutesFrom(value: string): number | null {
    const n = Number(value)
    return value.trim() === '' || !Number.isFinite(n) || n <= 0 ? null : Math.round(n)
  }
</script>

{#if task}
  <aside class="detail">
    <header class="top">
      <span class="eyebrow">{project ? project.name : 'Inbox'}</span>
      <button class="close" title="Close" aria-label="Close" onclick={() => todo.open(null)}>
        <Icon name="close" size={15} />
      </button>
    </header>

    <div class="scroll body">
      <textarea
        class="title"
        rows="1"
        placeholder="Task title"
        value={task.title}
        oninput={(e) => todo.patch(task.id, { title: e.currentTarget.value })}
      ></textarea>

      <div class="grid">
        <label class="lab" for="d-status">Status</label>
        <select
          id="d-status"
          class="pick"
          value={task.status}
          onchange={(e) => todo.setStatus(task.id, e.currentTarget.value as TaskStatus)}
        >
          {#each TASK_STATUSES as s (s)}<option value={s}>{STATUS_LABELS[s]}</option>{/each}
        </select>

        <label class="lab" for="d-priority">Priority</label>
        <select
          id="d-priority"
          class="pick"
          value={task.priority}
          onchange={(e) => todo.patch(task.id, { priority: e.currentTarget.value as Priority })}
        >
          {#each PRIORITIES as p (p)}
            <option value={p}>{p === 'none' ? 'None' : p[0]!.toUpperCase() + p.slice(1)}</option>
          {/each}
        </select>

        <label class="lab" for="d-project">Project</label>
        <select
          id="d-project"
          class="pick"
          value={task.projectId ?? ''}
          onchange={(e) => todo.setProject(task.id, e.currentTarget.value || null)}
        >
          <option value="">Inbox</option>
          {#each todo.liveProjects as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
        </select>

        <label class="lab" for="d-due">Due</label>
        <div class="pair">
          <input
            id="d-due"
            class="pick"
            type="date"
            value={task.dueDate ?? ''}
            onchange={(e) =>
              todo.patch(task.id, {
                dueDate: e.currentTarget.value || null,
                // A time with no date would be an hour attached to nothing.
                dueTime: e.currentTarget.value ? task.dueTime : null,
              })}
          />
          <input
            class="pick time"
            type="time"
            aria-label="Due time"
            disabled={!task.dueDate}
            value={task.dueTime?.slice(0, 5) ?? ''}
            onchange={(e) =>
              todo.patch(task.id, {
                dueTime: e.currentTarget.value ? `${e.currentTarget.value}:00` : null,
              })}
          />
        </div>

        <label class="lab" for="d-start">Start</label>
        <input
          id="d-start"
          class="pick"
          type="date"
          value={task.startDate ?? ''}
          onchange={(e) => todo.patch(task.id, { startDate: e.currentTarget.value || null })}
        />

        <label class="lab" for="d-estimate">Estimate</label>
        <div class="pair">
          <input
            id="d-estimate"
            class="pick"
            type="number"
            min="0"
            step="15"
            placeholder="minutes"
            value={task.estimateMinutes ?? ''}
            onchange={(e) =>
              todo.patch(task.id, {
                estimateMinutes: minutesFrom(e.currentTarget.value),
              })}
          />
          <span class="unit">
            {task.estimateMinutes ? formatMinutes(task.estimateMinutes) : 'minutes'}
          </span>
          {#if quick.enabled('todo.estimate') && task.estimateMinutes == null}
            <button
              class="mini"
              onclick={() => void suggestEstimate()}
              title="Guess from similar tasks"
            >
              <Icon name="sparkle" size={11} />
            </button>
          {/if}
        </div>
        {#if estimate != null && task.estimateMinutes == null}
          <span></span>
          <Suggestions
            scope={task.id}
            items={[{ key: 'e', label: formatMinutes(estimate) }]}
            onaccept={() => {
              todo.patch(task.id, { estimateMinutes: estimate })
              estimate = null
            }}
            ondismiss={() => (estimate = null)}
          />
        {/if}

        <label class="lab" for="d-purpose">For</label>
        <div id="d-purpose">
          <!-- Left blank on nearly every task, and that is the expected
               shape: a task under a filed project is already attributed. -->
          <PurposeField
            value={task.purpose}
            onchange={(purpose: Purpose | null) => todo.patch(task.id, { purpose })}
            placeholder={project?.purpose ? 'Same as the project' : 'Not filed'}
          />
        </div>
      </div>

      <div class="head"><span class="eyebrow">Tags</span></div>
      <div class="tags">
        {#each task.tags as tag (tag)}
          <span class="tag">
            {tag}
            <button
              class="x"
              title="Remove {tag}"
              aria-label="Remove {tag}"
              onclick={() => todo.patch(task.id, { tags: task.tags.filter((t) => t !== tag) })}
            >
              <Icon name="close" size={10} weight={2} />
            </button>
          </span>
        {/each}
        <input
          class="tagin"
          placeholder="Add tag"
          bind:value={tagDraft}
          onblur={addTag}
          onkeydown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault()
              addTag()
            }
            if (e.key === 'Escape') tagDraft = ''
          }}
          list="known-tags"
        />
        <datalist id="known-tags">
          {#each todo.tags as t (t.tag)}<option value={t.tag}></option>{/each}
        </datalist>
      </div>

      <div class="head"><span class="eyebrow">Notes</span></div>
      <textarea
        class="notes"
        rows="4"
        placeholder="Anything worth remembering about this"
        value={task.notes}
        oninput={(e) => todo.patch(task.id, { notes: e.currentTarget.value })}
      ></textarea>

      {#if quick.enabled('todo.subtasks') && !task.parentId}
        <div class="head">
          <span class="eyebrow">Steps</span>
          <button class="mini" disabled={breaking} onclick={() => void breakDown()}>
            <Icon name="sparkle" size={12} />
            Break it down
          </button>
        </div>
        <!-- Proposals, one chip each. Tapping one makes an ordinary subtask
             -- a task with a parent -- rather than going through a second
             creation path that could drift from the first. -->
        <Suggestions
          scope={task.id}
          items={stepChips}
          busy={breaking}
          onaccept={(key: string) => void acceptStep(key)}
          ondismiss={() => stepSlot.dismiss(() => (stepChips = []))}
        />
      {/if}

      <div class="head">
        <span class="eyebrow">Time</span>
        <button class="mini" onclick={openBooking}>
          <Icon name="plus" size={12} weight={2} /> Book
        </button>
      </div>

      {#if time.logged > 0 || time.planned > 0}
        <p class="totals">
          {#if time.logged > 0}<strong>{formatMinutes(time.logged)}</strong> logged{/if}
          {#if time.logged > 0 && time.planned > 0}
            ·
          {/if}
          {#if time.planned > 0}{formatMinutes(time.planned)} planned{/if}
          {#if task.estimateMinutes}
            · estimate {formatMinutes(task.estimateMinutes)}
          {/if}
        </p>
      {/if}

      {#if booking}
        <div class="booker">
          <input class="pick" type="datetime-local" aria-label="Start" bind:value={bookStart} />
          <div class="pair">
            <input
              class="pick"
              type="number"
              min="5"
              step="15"
              aria-label="Minutes"
              bind:value={bookMinutes}
            />
            <select class="pick" aria-label="Kind" bind:value={bookKind}>
              <option value="actual">Actually spent</option>
              <option value="planned">Planned</option>
            </select>
          </div>
          <div class="bookrow">
            <button class="btn" onclick={() => (booking = false)}>Cancel</button>
            <button class="btn btn-primary" onclick={book}>Book it</button>
          </div>
        </div>
      {/if}

      {#each todo.detailBlocks as block (block.id)}
        <div class="block">
          <span class="kind" class:actual={block.kind === 'actual'}>
            <Icon name={block.kind === 'actual' ? 'check' : 'clock'} size={12} weight={1.7} />
          </span>
          <span class="when">
            {friendlyDate(block.localDate)}, {formatInstantTime(block.start)}
          </span>
          <span class="len">
            {formatMinutes((Date.parse(block.end) - Date.parse(block.start)) / 60_000)}
          </span>
          <button
            class="x"
            title="Remove this block"
            aria-label="Remove this block"
            onclick={() => todo.removeBlock(block.id)}
          >
            <Icon name="close" size={11} weight={2} />
          </button>
        </div>
      {/each}

      {#if todo.detailBlocks.length === 0 && !booking}
        <p class="quiet">
          Nothing booked yet. Blocks are what the calendar will draw, and what “where did my time
          go” adds up.
        </p>
      {/if}

      <div class="head"><span class="eyebrow">Danger</span></div>
      <button class="btn btn-ghost-danger wide" onclick={() => (confirming = true)}>
        <Icon name="trash" size={14} /> Delete task
      </button>

      <p class="stamp">
        Created {friendlyDate(task.createdAt.slice(0, 10))}
        {#if task.completedAt}
          · done {friendlyDate(task.completedAt.slice(0, 10))}{/if}
      </p>
    </div>
  </aside>

  {#if confirming}
    <ConfirmDialog
      title={'Delete “' + (task.title || 'this task') + '”?'}
      detail={todo.progressOf(task.id)[1] > 0
        ? 'Its subtasks and every block of time booked against them go too. This cannot be undone.'
        : 'Any time booked against it goes too. This cannot be undone.'}
      confirmLabel="Delete task"
      onconfirm={() => {
        confirming = false
        void todo.remove(task.id)
      }}
      oncancel={() => (confirming = false)}
    />
  {/if}
{/if}

<style>
  .detail {
    width: 320px;
    flex: none;
    display: flex;
    flex-direction: column;
    background: var(--bg-panel);
    border-left: 1px solid var(--border);
  }

  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: var(--header-h);
    padding: 0 var(--sp-2) 0 var(--sp-4);
    flex: none;
  }
  .close {
    width: 26px;
    height: 26px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .close:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .body {
    flex: 1;
    padding: 0 var(--sp-4) var(--sp-8);
  }

  /* A textarea rather than an input, so a long title wraps instead of
     scrolling sideways past the end of the panel. */
  .title {
    width: 100%;
    border: none;
    background: none;
    resize: none;
    field-sizing: content;
    font-family: var(--font-read);
    font-size: var(--text-lg);
    font-weight: 550;
    line-height: var(--leading-snug);
    color: var(--fg);
    user-select: text;
  }
  .title:focus {
    outline: none;
  }

  .grid {
    display: grid;
    grid-template-columns: 62px 1fr;
    align-items: center;
    gap: var(--sp-2) var(--sp-2);
    margin-top: var(--sp-4);
  }
  .lab {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  .pick {
    width: 100%;
    min-width: 0;
    height: 28px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg);
    font-size: var(--text-sm);
    user-select: text;
  }
  .pick:focus {
    outline: none;
    border-color: var(--accent);
  }
  .pick:disabled {
    opacity: 0.45;
  }

  .pair {
    display: flex;
    gap: var(--sp-1);
    min-width: 0;
  }
  .pair .time {
    flex: 0 0 82px;
  }
  .unit {
    display: flex;
    align-items: center;
    font-size: var(--text-xs);
    color: var(--fg-faint);
    white-space: nowrap;
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--sp-5) 0 var(--sp-2);
  }
  .mini {
    display: flex;
    align-items: center;
    gap: 3px;
    padding: 2px var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .mini:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .tags {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1);
    align-items: center;
  }
  .tag {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    height: 22px;
    padding: 0 5px 0 var(--sp-2);
    border-radius: 99px;
    background: var(--bg-sunken);
    border: 1px solid var(--border);
    font-size: var(--text-xs);
    color: var(--fg-muted);
  }
  .x {
    display: flex;
    color: var(--fg-faint);
    border-radius: 99px;
    padding: 2px;
  }
  .x:hover {
    color: var(--danger);
    background: var(--bg-hover);
  }
  .tagin {
    height: 22px;
    width: 84px;
    padding: 0 var(--sp-2);
    border: 1px dashed var(--border-strong);
    border-radius: 99px;
    background: none;
    font-size: var(--text-xs);
    user-select: text;
  }
  .tagin:focus {
    outline: none;
    border-style: solid;
    border-color: var(--accent);
  }

  .notes {
    width: 100%;
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    font-family: var(--font-read);
    font-size: var(--text-base);
    line-height: var(--leading-normal);
    color: var(--fg);
    resize: vertical;
    user-select: text;
  }
  .notes:focus {
    outline: none;
    border-color: var(--accent);
  }

  .totals {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    margin-bottom: var(--sp-2);
  }
  .totals strong {
    color: var(--fg);
    font-weight: 600;
  }

  .booker {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    padding: var(--sp-2);
    margin-bottom: var(--sp-2);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
  }
  .bookrow {
    display: flex;
    gap: var(--sp-1);
    justify-content: flex-end;
    margin-top: 2px;
  }
  .bookrow .btn {
    height: 26px;
    font-size: var(--text-sm);
  }

  .block {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: 4px var(--sp-1);
    font-size: var(--text-sm);
    color: var(--fg-muted);
    border-radius: var(--radius-sm);
  }
  .block:hover {
    background: var(--bg-hover);
  }
  .kind {
    display: flex;
    color: var(--fg-faint);
  }
  /* Logged time is the record; planned time is only an intention. */
  .kind.actual {
    color: #15803d;
  }
  .when {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .len {
    font-variant-numeric: tabular-nums;
    color: var(--fg-subtle);
  }

  .quiet {
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-faint);
  }

  .wide {
    width: 100%;
  }
  .stamp {
    margin-top: var(--sp-4);
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
</style>
