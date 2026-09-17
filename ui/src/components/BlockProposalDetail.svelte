<script lang="ts">
  // A pending block proposal, open in the rail in place of a real block.
  //
  // The same shape as `TaskProposalDetail`: a local draft, nothing
  // autosaved, and accepting is the one write. Times are edited as clocks on
  // the day the block already proposes -- moving it to a different *day* is
  // what dragging the ghost on the grid is for.

  import { calendar } from '../lib/calendar.svelte'
  import { DECLINE_REASONS, edited, proposals, recordAs } from '../lib/proposals.svelte'
  import { dayHeading, formatMinutes, toLocalTimeValue } from '../lib/format'
  import { instantAt, minutesBetween } from '../lib/time'
  import Icon from './Icon.svelte'
  import type { Proposal, TimeBlock } from '../lib/types'

  let { proposal }: { proposal: Proposal } = $props()

  const original = $derived(recordAs(proposal, 'block'))

  let draft = $state<TimeBlock | null>(null)
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

  const busy = $derived(proposals.isBusy(proposal.id))
  let choosing = $state(false)

  function patch(changes: Partial<TimeBlock>) {
    if (!draft) return
    draft = { ...draft, ...changes }
  }

  /** Move a clock field, holding the day the proposal already names. */
  function setClock(field: 'start' | 'end', value: string) {
    if (!draft) return
    const [h, m] = value.split(':').map(Number)
    if (h === undefined || m === undefined) return
    patch({ [field]: instantAt(draft.localDate, h * 60 + m) })
  }

  function clockValue(ts: string): string {
    return toLocalTimeValue(new Date(ts))
  }

  async function accept() {
    if (!draft) return
    // A snapshot, not the live `$state` proxy -- see `TaskProposalDetail`'s
    // own note on why.
    const closed = changed
      ? await proposals.accept(proposal, edited('block', $state.snapshot(draft)))
      : await proposals.accept(proposal)
    if (closed) calendar.closeProposal()
  }

  async function decline(reason: (typeof DECLINE_REASONS)[number]['reason'] | null) {
    choosing = false
    await proposals.decline(proposal, reason)
    calendar.closeProposal()
  }
</script>

{#if draft}
  {@const colour = calendar.colorOfBlock(draft)}
  <div class="panel scroll proposal" style="--c: {colour}">
    <header class="phead">
      <span class="badge"><Icon name="sparkle" size={12} /> Proposed block</span>
      <button class="x" title="Close" aria-label="Close" onclick={() => calendar.closeProposal()}>
        <Icon name="close" size={15} />
      </button>
    </header>

    {#if proposal.why}<p class="why">{proposal.why}</p>{/if}

    <input
      class="titlefield"
      placeholder={calendar.titleOfBlock(draft)}
      value={draft.title}
      oninput={(e) => patch({ title: e.currentTarget.value })}
    />

    <p class="when">
      {dayHeading(new Date(draft.start))}
      <span class="dot">·</span>
      {formatMinutes(minutesBetween(draft.start, draft.end))}
    </p>

    <div class="times">
      <label class="timefield">
        <span class="lbl">From</span>
        <input
          type="time"
          step="900"
          value={clockValue(draft.start)}
          onchange={(e) => setClock('start', e.currentTarget.value)}
        />
      </label>
      <label class="timefield">
        <span class="lbl">To</span>
        <input
          type="time"
          step="900"
          value={clockValue(draft.end)}
          onchange={(e) => setClock('end', e.currentTarget.value)}
        />
      </label>
    </div>

    {#if draft.subject.type === 'task'}
      {@const task = calendar.taskOf(draft.subject.id)}
      <div class="subject">
        <Icon name="check" size={14} weight={1.6} />
        <span class="stitle">{task?.title ?? 'A task'}</span>
      </div>
    {:else if draft.subject.type === 'project'}
      {@const project = calendar.projectOf(draft.subject.id)}
      <div class="subject">
        <span class="pmark">{project?.icon ?? '•'}</span>
        <span class="stitle">{project?.name ?? 'A project'}</span>
      </div>
    {/if}

    <textarea
      class="notes"
      rows="3"
      placeholder="Notes"
      value={draft.notes}
      oninput={(e) => patch({ notes: e.currentTarget.value })}
    ></textarea>

    <footer class="answers">
      <div class="decline-wrap">
        <button class="btn" disabled={busy} onclick={() => (choosing = !choosing)}>
          Decline
        </button>
        {#if choosing}
          <div class="reasons" role="menu">
            <button role="menuitem" onclick={() => decline(null)}>Just decline</button>
            {#each DECLINE_REASONS as r (r.label)}
              <button role="menuitem" onclick={() => decline(r.reason)}>{r.label}</button>
            {/each}
          </div>
        {/if}
      </div>
      <button class="btn btn-primary" disabled={busy} onclick={accept}>
        {changed ? 'Save & accept' : 'Accept'}
      </button>
    </footer>
  </div>
{/if}

<style>
  .panel {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    padding: var(--sp-4);
  }
  .proposal {
    border-top: 3px dashed color-mix(in oklab, var(--c) 55%, var(--border));
  }

  .phead {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
  }
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--accent);
    text-transform: lowercase;
  }
  .x {
    width: 24px;
    height: 24px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .x:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .why {
    margin: 0;
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius-sm);
    background: color-mix(in oklab, var(--accent) 8%, transparent);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }

  .titlefield {
    width: 100%;
    border: none;
    background: none;
    font-size: var(--text-lg);
    font-weight: 600;
    letter-spacing: -0.012em;
    user-select: text;
  }
  .titlefield::placeholder {
    color: var(--fg-subtle);
    font-weight: 550;
  }
  .titlefield:focus {
    outline: none;
  }

  .when {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .when .dot {
    opacity: 0.5;
    margin: 0 3px;
  }

  .times {
    display: flex;
    gap: var(--sp-2);
  }
  .timefield {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .lbl {
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg-faint);
  }
  .timefield input {
    height: 30px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    user-select: text;
  }
  .timefield input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .subject {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-2);
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    color: var(--fg-muted);
    min-width: 0;
  }
  .pmark {
    font-size: var(--text-sm);
    line-height: 1;
  }
  .stitle {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .notes {
    width: 100%;
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    resize: vertical;
    user-select: text;
  }
  .notes:focus {
    outline: none;
    border-color: var(--accent);
  }

  .answers {
    display: flex;
    justify-content: flex-end;
    gap: var(--sp-2);
    margin-top: auto;
  }
  .decline-wrap {
    position: relative;
  }
  .reasons {
    position: absolute;
    left: 0;
    bottom: 100%;
    z-index: 20;
    display: flex;
    flex-direction: column;
    min-width: 160px;
    margin-bottom: 4px;
    padding: var(--sp-1);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    box-shadow: var(--shadow);
  }
  .reasons button {
    padding: var(--sp-1) var(--sp-2);
    border: none;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
  }
  .reasons button:hover {
    background: var(--bg-hover);
  }
</style>
