<script lang="ts">
  // The list column `MailView` swaps in for a mailbox's threads once an
  // Outbox row is picked in `MailNav` -- every message asked to be sent that
  // the server would not take, newest first (`outbox` in `mail.rs`): one
  // failing and waiting to be tried again, or one that failed for good and is
  // back to being written. Each says why, in the server's own words, with
  // what there is to do about it: edit it, send it again, or throw it away.

  import { formatSenders } from '../lib/mail'
  import { mail } from '../lib/mail.svelte'
  import { accounts } from '../lib/accounts.svelte'
  import { relativeTime } from '../lib/format'
  import type { AccountId, OutboxItem } from '../lib/types'
  import EmptyState from './EmptyState.svelte'

  /** One account's Outbox, or every account's (`null`). */
  let { accountId }: { accountId: AccountId | null } = $props()

  const items = $derived(
    mail.outbox.filter((i) => accountId === null || i.draft.accountId === accountId),
  )

  function recipientsOf(item: OutboxItem): string {
    return (
      formatSenders([...item.draft.to, ...item.draft.cc, ...item.draft.bcc], 3) || '(no recipients)'
    )
  }

  function status(item: OutboxItem): string {
    if (!item.nextAttempt) return 'Not sent'
    const tries = item.attempts === 1 ? 'Tried once' : `Tried ${item.attempts} times`
    return `${tries} · trying again ${relativeTime(item.nextAttempt)}`
  }
</script>

{#if items.length === 0}
  <EmptyState lead="Nothing in the Outbox">
    {#snippet note()}A message the server will not take waits here until it goes or you change it.{/snippet}
  </EmptyState>
{:else}
  <div class="scroll outbox-rows">
    {#each items as item (item.draft.id)}
      <div class="row">
        <div class="body">
          <div class="line1">
            <span class="to">{recipientsOf(item)}</span>
            {#if accountId === null}
              <span class="from"
                >{accounts.account(item.draft.accountId)?.address ?? item.draft.identity}</span
              >
            {/if}
          </div>
          <div class="line2">
            <span class="subject">{item.draft.subject || '(no subject)'}</span>
          </div>
          <div class="state">{status(item)}</div>
          {#if item.draft.sendError}
            <div class="why" title={item.draft.sendError}>{item.draft.sendError}</div>
          {/if}
          <div class="actions">
            <button class="link" onclick={() => void mail.editOutbox(item)}>Edit</button>
            <button class="link" onclick={() => void mail.retryOutbox(item)}>
              {item.nextAttempt ? 'Try now' : 'Send again'}
            </button>
            <button class="link danger" onclick={() => void mail.discardOutbox(item)}>
              Discard
            </button>
          </div>
        </div>
      </div>
    {/each}
  </div>
{/if}

<style>
  .outbox-rows {
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
  .to,
  .subject {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
  }
  .to {
    color: var(--fg-muted);
  }
  .from {
    flex: none;
    max-width: 45%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .subject {
    color: var(--fg);
    font-weight: 550;
  }
  .state {
    font-size: var(--text-xs);
    color: var(--danger);
    font-weight: 600;
  }
  .why {
    font-size: var(--text-xs);
    color: var(--fg-muted);
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
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
</style>
