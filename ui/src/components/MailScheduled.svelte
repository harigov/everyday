<script lang="ts">
  // The list column `MailView` swaps in for a mailbox's threads once the
  // Scheduled row is picked in `MailNav` -- every draft still queued to
  // send later, soonest first (the ordering `scheduled_sends` itself
  // promises, see `mail-api.ts`'s own doc), with the four things there is
  // ever to do to one: edit it, reschedule it, send it now, or cancel it.

  import { formatSenders, scheduledSendLabel } from '../lib/mail'
  import { mail } from '../lib/mail.svelte'
  import { sendLaterChoices } from '../lib/mailwrite'
  import { toLocalInputValue } from '../lib/format'
  import type { AccountId, ScheduledSend } from '../lib/types'
  import EmptyState from './EmptyState.svelte'

  let { accountId }: { accountId: AccountId } = $props()

  const sends = $derived(mail.scheduled.filter((s) => s.draft.accountId === accountId))

  /** The row whose reschedule picker is open, or `null`. Component-local,
   *  unlike `mail.wantsSnooze`: nothing but this component ever needs to
   *  know which row's own picker is open. */
  let reschedulingId = $state<string | null>(null)
  let customAt = $state('')

  function recipientsOf(s: ScheduledSend): string {
    return formatSenders(s.draft.to, 3) || '(no recipients)'
  }

  function openReschedule(s: ScheduledSend) {
    reschedulingId = reschedulingId === s.draft.id ? null : s.draft.id
    customAt = toLocalInputValue(new Date(s.sendAt))
  }

  function reschedule(s: ScheduledSend, at: Date) {
    reschedulingId = null
    void mail.rescheduleScheduled(s.draft.id, at)
  }

  function rescheduleCustom(s: ScheduledSend) {
    if (!customAt) return
    reschedule(s, new Date(customAt))
  }
</script>

{#if sends.length === 0}
  <EmptyState lead="Nothing scheduled">
    {#snippet note()}A message sent later shows up here until it goes.{/snippet}
  </EmptyState>
{:else}
  <div class="scroll scheduled-rows">
    {#each sends as s (s.draft.id)}
      <div class="row">
        <div class="body">
          <div class="line1">
            <span class="from">{recipientsOf(s)}</span>
          </div>
          <div class="line2">
            <span class="subject">{s.draft.subject || '(no subject)'}</span>
          </div>
          <div class="when">{scheduledSendLabel(s.sendAt)}</div>
          <div class="actions">
            <button class="link" onclick={() => void mail.editScheduled(s.draft.id)}>Edit</button>
            <button class="link" onclick={() => openReschedule(s)}>Reschedule</button>
            <button class="link" onclick={() => void mail.sendScheduledNow(s.draft.id)}>
              Send now
            </button>
            <button class="link danger" onclick={() => void mail.cancelScheduled(s.draft.id)}>
              Cancel
            </button>
          </div>
          {#if reschedulingId === s.draft.id}
            <div class="reschedule">
              {#each sendLaterChoices() as choice (choice.key)}
                <button class="choice" onclick={() => reschedule(s, choice.at)}>
                  {choice.label}
                </button>
              {/each}
              <div class="custom">
                <input type="datetime-local" bind:value={customAt} />
                <button class="btn" disabled={!customAt} onclick={() => rescheduleCustom(s)}>
                  Set
                </button>
              </div>
            </div>
          {/if}
        </div>
      </div>
    {/each}
  </div>
{/if}

<style>
  .scheduled-rows {
    flex: 1;
    min-height: 0;
  }
  .row {
    display: flex;
    padding: var(--sp-2) var(--sp-3);
    border-bottom: 1px solid var(--border);
  }
  .body {
    flex: 1;
    min-width: 0;
    display: grid;
    gap: 2px;
  }
  .line1,
  .line2 {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    overflow: hidden;
  }
  .from {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .subject {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--fg);
    font-weight: 550;
  }
  .when {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .actions {
    display: flex;
    gap: var(--sp-3);
    margin-top: 2px;
  }
  .link {
    font-size: var(--text-xs);
    color: var(--journal-accent, var(--accent));
    font-weight: 600;
  }
  .link.danger {
    color: var(--danger, #c0392b);
  }
  .reschedule {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-2);
    padding-top: var(--sp-2);
    border-top: 1px solid var(--border);
  }
  .choice {
    padding: 3px var(--sp-2);
    border-radius: var(--radius-sm);
    border: 1px solid var(--border);
    font-size: var(--text-xs);
    color: var(--fg-muted);
  }
  .choice:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .custom {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
  }
  .custom input {
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 3px var(--sp-1);
    background: var(--bg-raised);
    color: var(--fg);
    font-size: var(--text-xs);
  }
</style>
