<script lang="ts">
  // A pending task proposal, open in the detail rail in place of a real one.
  //
  // Fields are editable, like `TaskDetail`'s, but nothing here autosaves --
  // there is no task in the vault yet for a keystroke to land on. The draft
  // lives only in this component; accepting hands it to `proposals.accept`,
  // which is the one write that happens, and only once.

  import { menu } from '../lib/menu.svelte'
  import { declineItems, edited, proposals, recordAs } from '../lib/proposals.svelte'
  import { todo } from '../lib/todo.svelte'
  import { formatMinutes, friendlyDate } from '../lib/format'
  import { reminderLabel } from '../lib/tasklist'
  import { closesOnBack } from '../lib/phone.svelte'
  import Icon from './Icon.svelte'
  import PhoneBack from './PhoneBack.svelte'
  import PurposeField from './PurposeField.svelte'
  import { PRIORITIES, TASK_STATUSES } from '../lib/types'
  import type { DeclineReason, Priority, Purpose, Task, TaskStatus } from '../lib/types'
  import { STATUS_LABELS, PRIORITY_LABELS } from '../lib/labels'

  let { proposal }: { proposal: import('../lib/types').Proposal } = $props()

  const original = $derived(recordAs(proposal, 'task'))

  /** The draft this pane edits, reset whenever a different proposal opens. */
  let draft = $state<Task | null>(null)
  let openId = $state<string | null>(null)
  $effect(() => {
    if (proposal.id !== openId) {
      openId = proposal.id
      draft = original ? { ...original } : null
    }
  })

  const changed = $derived.by(() => {
    if (!draft || !original) return false
    return JSON.stringify(draft) !== JSON.stringify(original)
  })

  // A phone's back button closes it, as its own way back does.
  closesOnBack(
    () => draft !== null,
    () => todo.closeProposal(),
  )

  const busy = $derived(proposals.isBusy(proposal.id))
  const reasons = Symbol('decline reasons')
  const choosing = $derived(menu.owner === reasons)

  function patch(changes: Partial<Task>) {
    if (!draft) return
    draft = { ...draft, ...changes }
  }

  async function accept() {
    if (!draft) return
    // A snapshot, not the live `$state` proxy: `accept` carries this over
    // the same transport every other write goes through, and a proxy is not
    // the plain object the other end can clone.
    const closed = changed
      ? await proposals.accept(proposal, edited('task', $state.snapshot(draft)))
      : await proposals.accept(proposal)
    if (closed) todo.closeProposal()
  }

  async function decline(reason: DeclineReason | null) {
    await proposals.decline(proposal, reason)
    todo.closeProposal()
  }

  function toggleReasons(e: MouseEvent) {
    if (choosing) menu.close()
    else menu.show(e, declineItems(decline), reasons)
  }

  /** `<input type="number">` gives '' for a cleared field, which means "none". */
  function minutesFrom(value: string): number | null {
    const n = Number(value)
    return value.trim() === '' || !Number.isFinite(n) || n <= 0 ? null : Math.round(n)
  }
</script>

{#if draft}
  <aside class="detail proposal">
    <header class="top">
      <!-- A phone shows it over the list, with the way back to it. -->
      <PhoneBack onclick={() => todo.closeProposal()} label="Tasks" />
      <span class="badge"><Icon name="sparkle" size={12} /> Proposed task</span>
      <button
        class="close desktop-only"
        title="Close"
        aria-label="Close"
        onclick={() => todo.closeProposal()}
      >
        <Icon name="close" size={15} />
      </button>
    </header>

    <div class="scroll body">
      {#if proposal.why}<p class="why">{proposal.why}</p>{/if}

      <textarea
        class="title"
        rows="1"
        placeholder="Task title"
        value={draft.title}
        oninput={(e) => patch({ title: e.currentTarget.value })}
      ></textarea>

      <div class="grid">
        <label class="lab" for="pd-status">Status</label>
        <select
          id="pd-status"
          class="pick"
          value={draft.status}
          onchange={(e) => patch({ status: e.currentTarget.value as TaskStatus })}
        >
          {#each TASK_STATUSES as s (s)}<option value={s}>{STATUS_LABELS[s]}</option>{/each}
        </select>

        <label class="lab" for="pd-priority">Priority</label>
        <select
          id="pd-priority"
          class="pick"
          value={draft.priority}
          onchange={(e) => patch({ priority: e.currentTarget.value as Priority })}
        >
          {#each PRIORITIES as p (p)}
            <option value={p}>{PRIORITY_LABELS[p]}</option>
          {/each}
        </select>

        <label class="lab" for="pd-project">Project</label>
        <select
          id="pd-project"
          class="pick"
          value={draft.projectId ?? ''}
          onchange={(e) => patch({ projectId: e.currentTarget.value || null })}
        >
          <option value="">Inbox</option>
          {#each todo.liveProjects as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
        </select>

        <label class="lab" for="pd-due">Due</label>
        <div class="pair">
          <input
            id="pd-due"
            class="pick"
            type="date"
            value={draft.dueDate ?? ''}
            onchange={(e) =>
              patch({
                dueDate: e.currentTarget.value || null,
                dueTime: e.currentTarget.value ? draft?.dueTime : null,
              })}
          />
          <input
            class="pick time"
            type="time"
            aria-label="Due time"
            disabled={!draft.dueDate}
            value={draft.dueTime?.slice(0, 5) ?? ''}
            onchange={(e) =>
              patch({ dueTime: e.currentTarget.value ? `${e.currentTarget.value}:00` : null })}
          />
        </div>

        <label class="lab" for="pd-estimate">Estimate</label>
        <div class="pair">
          <input
            id="pd-estimate"
            class="pick"
            type="number"
            min="0"
            step="15"
            placeholder="minutes"
            value={draft.estimateMinutes ?? ''}
            onchange={(e) => patch({ estimateMinutes: minutesFrom(e.currentTarget.value) })}
          />
          <span class="unit">
            {draft.estimateMinutes ? formatMinutes(draft.estimateMinutes) : 'minutes'}
          </span>
        </div>

        <label class="lab" for="pd-purpose" title="Which goal or area of your life this is for"
          >Goal or area</label
        >
        <div id="pd-purpose">
          <PurposeField
            value={draft.purpose}
            onchange={(purpose: Purpose | null) => patch({ purpose })}
            placeholder={todo.projectOf(draft.projectId)?.purpose
              ? 'Same as the project'
              : 'No goal or area'}
          />
        </div>

        <!-- What a proposal can carry that this pane has no field for. Shown
             as read, when set, so accepting is never a surprise: it is
             carried over exactly as proposed. -->
        {#if draft.startDate}
          <span class="lab">Start</span>
          <span class="ro">{friendlyDate(draft.startDate)}</span>
        {/if}
        {#if draft.assignee === 'assistant'}
          <span class="lab">Done by</span>
          <span class="ro">The assistant</span>
        {/if}
        {#if draft.remindAt}
          <span class="lab">{draft.assignee === 'assistant' ? 'Do at' : 'Remind'}</span>
          <span class="ro">{reminderLabel(draft.remindAt)}</span>
        {/if}
        {#if draft.tags.length > 0}
          <span class="lab">Tags</span>
          <span class="ro">{draft.tags.join(', ')}</span>
        {/if}
        {#if draft.raiseWith?.length}
          <span class="lab">Bring up with</span>
          <span class="ro">{draft.raiseWith.map((p) => p.name).join(', ')}</span>
        {/if}
      </div>

      <div class="head"><span class="eyebrow">Notes</span></div>
      <textarea
        class="notes"
        rows="4"
        placeholder="Anything worth remembering about this"
        value={draft.notes}
        oninput={(e) => patch({ notes: e.currentTarget.value })}
      ></textarea>
    </div>

    <footer class="answers">
      <button
        class="btn"
        disabled={busy}
        data-menu-trigger
        aria-haspopup="menu"
        aria-expanded={choosing}
        onclick={toggleReasons}
      >
        Decline
      </button>
      <button class="btn btn-primary" disabled={busy} onclick={accept}>
        {changed ? 'Save & accept' : 'Accept'}
      </button>
    </footer>
  </aside>
{/if}

<style>
  .detail {
    width: var(--detail-w);
    flex: none;
    display: flex;
    flex-direction: column;
    background: var(--bg-panel);
    border-left: 1px dashed color-mix(in oklab, var(--accent) 55%, var(--border));
  }

  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: var(--header-h);
    padding: 0 var(--sp-2) 0 var(--sp-4);
    flex: none;
  }
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--accent);
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
    padding: 0 var(--sp-4) var(--sp-4);
  }

  .why {
    margin: 0 0 var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius-sm);
    background: color-mix(in oklab, var(--accent) 8%, transparent);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }

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
  .title:focus:not(:focus-visible) {
    outline: none;
  }

  .grid {
    display: grid;
    grid-template-columns: 88px 1fr;
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
    border-color: var(--accent);
  }
  .pick:focus:not(:focus-visible) {
    outline: none;
  }
  .pick:disabled {
    opacity: 0.45;
  }
  .pair {
    display: flex;
    gap: var(--sp-1);
    min-width: 0;
    align-items: center;
  }
  .pair .time {
    flex: 0 0 82px;
  }
  .ro {
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--fg-muted);
    overflow-wrap: anywhere;
  }
  .unit {
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
    border-color: var(--accent);
  }
  .notes:focus:not(:focus-visible) {
    outline: none;
  }

  .answers {
    display: flex;
    justify-content: flex-end;
    gap: var(--sp-2);
    padding: var(--sp-3) var(--sp-4);
    border-top: 1px solid var(--border);
    flex: none;
  }

  /* A phone: the whole of the screen between its bars, over the list the
     proposal was chosen from, with the fields a finger can use and the
     answers where a thumb rests. */
  :global([data-phone]) .detail {
    position: fixed;
    inset: calc(var(--safe-top) + var(--topbar-h)) var(--safe-right)
      calc(var(--bottombar-h) + var(--safe-bottom)) var(--safe-left);
    z-index: 40;
    width: auto;
    border-left: 0;
  }
  :global([data-phone]) .top {
    height: 56px;
    justify-content: flex-start;
    gap: var(--sp-2);
    padding: 0 var(--sp-3) 0 var(--sp-1);
  }
  :global([data-phone]) .grid {
    grid-template-columns: 96px minmax(0, 1fr);
    gap: var(--sp-3) var(--sp-2);
  }
  :global([data-phone]) .lab {
    font-size: var(--text-base);
  }
  :global([data-phone]) .pick {
    height: 44px;
    padding: 0 var(--sp-3);
    font-size: max(16px, var(--text-sm));
  }
  :global([data-phone]) .pair .time {
    flex: 0 0 120px;
  }
  :global([data-phone]) .answers {
    gap: var(--sp-3);
    padding: var(--sp-3);
  }
  :global([data-phone]) .answers .btn {
    flex: 1;
    justify-content: center;
  }
</style>
