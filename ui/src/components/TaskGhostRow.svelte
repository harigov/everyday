<script lang="ts">
  // A pending task proposal, drawn with the task row's own formatting inside
  // the shared ghost shell. Used at the foot of a list section and at the
  // foot of a board column -- `compact` is what tells the two apart.

  import { recordAs } from '../lib/proposals.svelte'
  import { todo } from '../lib/todo.svelte'
  import { friendlyDate, formatClock, formatMinutes } from '../lib/format'
  import type { Proposal } from '../lib/types'
  import ProposalGhost from './ProposalGhost.svelte'

  let { proposal, compact = false }: { proposal: Proposal; compact?: boolean } = $props()

  const task = $derived(recordAs(proposal, 'task'))
</script>

{#if task}
  <ProposalGhost
    {proposal}
    {compact}
    color={todo.accent}
    onopen={() => todo.openProposal(proposal)}
  >
    <span class="title">{task.title}</span>
    {#if task.dueDate || task.estimateMinutes}
      <span class="meta">
        {#if task.dueDate}
          {friendlyDate(task.dueDate)}{task.dueTime ? ` ${formatClock(task.dueTime)}` : ''}
        {/if}
        {#if task.dueDate && task.estimateMinutes}·{/if}
        {#if task.estimateMinutes}{formatMinutes(task.estimateMinutes)}{/if}
      </span>
    {/if}
  </ProposalGhost>
{/if}

<style>
  .title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta {
    margin-left: var(--sp-2);
    font-size: var(--text-xs);
    color: var(--fg-faint);
    white-space: nowrap;
  }
</style>
