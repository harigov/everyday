<script lang="ts">
  // "What agents may do": the six-permission, two-column grid that sits in
  // both the add-account sheet and an account's detail, so it can be set
  // before the first save and changed just as easily afterwards -- the plan
  // asks for both.
  //
  // Assistant and MCP get their own column because they can disagree about
  // the same account: a work inbox can stay fully open to MCP -- the person
  // chose to connect that client -- while the assistant, which nobody
  // explicitly invited into this account's mail, waits for the "I
  // understand" below before it is offered any of these tools at all.
  //
  // Controlled, like every other editor in this codebase: the caller owns
  // the account (or the account-to-be), this only draws it and reports what
  // changed.

  import { PERMISSIONS } from '../lib/accounts'
  import type { AgentCallerKind, AgentMailAccess } from '../lib/types'

  let {
    assistantAccess,
    mcpAccess,
    providerName,
    acknowledged,
    disabled = false,
    onchange,
    onacknowledge,
  }: {
    assistantAccess: AgentMailAccess
    mcpAccess: AgentMailAccess
    /** What to call the assistant's model provider -- see `providerLabel`. */
    providerName: string
    /** Has this account's `assistantProviderAcknowledged` been set? */
    acknowledged: boolean
    disabled?: boolean
    onchange: (caller: AgentCallerKind, access: AgentMailAccess) => void
    onacknowledge: (ack: boolean) => void
  } = $props()

  function toggle(caller: AgentCallerKind, key: keyof AgentMailAccess, checked: boolean) {
    const current = caller === 'assistant' ? assistantAccess : mcpAccess
    onchange(caller, { ...current, [key]: checked })
  }
</script>

<div class="grid" role="table" aria-label="What agents may do">
  <div class="row head" role="row">
    <span role="columnheader"></span>
    <span role="columnheader">Assistant</span>
    <span role="columnheader">MCP</span>
  </div>

  <p class="provider-line">
    Your mail will be sent to <strong>{providerName}</strong> when you ask the assistant about it.
  </p>

  {#each PERMISSIONS as p (p.key)}
    <div class="row" class:send={p.key === 'send'} role="row">
      <span class="label" role="rowheader">{p.label}</span>
      <input
        type="checkbox"
        aria-label="Let the assistant {p.label.toLowerCase()}"
        checked={assistantAccess[p.key]}
        {disabled}
        onchange={(e) => toggle('assistant', p.key, e.currentTarget.checked)}
      />
      <input
        type="checkbox"
        aria-label="Let MCP {p.label.toLowerCase()}"
        checked={mcpAccess[p.key]}
        {disabled}
        onchange={(e) => toggle('mcp', p.key, e.currentTarget.checked)}
      />
    </div>
  {/each}

  <p class="hint send-hint">
    Sending is always confirmed in chat, whichever switches above allow it -- there is no setting
    that sends without asking.
  </p>

  <label class="ack">
    <input
      type="checkbox"
      checked={acknowledged}
      {disabled}
      onchange={(e) => onacknowledge(e.currentTarget.checked)}
    />
    <span>
      I understand -- until this is ticked, the assistant's mail tools stay off for this account
      regardless of the switches above.
    </span>
  </label>
</div>

<style>
  .grid {
    display: grid;
    grid-template-columns: 1fr auto auto;
    gap: var(--sp-2) var(--sp-3);
    align-items: center;
  }

  .row {
    display: grid;
    grid-template-columns: subgrid;
    grid-column: 1 / -1;
    align-items: center;
    padding: 3px 0;
  }
  .row.head {
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }
  .row.head span:not(:first-child) {
    justify-self: center;
  }
  /* Send is visually distinct -- it is the one action that reaches somebody
     who is not the user, and the plan asks for it to read that way. */
  .row.send {
    border-radius: var(--radius-sm);
    background: color-mix(in oklab, var(--accent) 8%, transparent);
  }
  .row.send .label {
    font-weight: 600;
  }

  .label {
    font-size: var(--text-sm);
    color: var(--fg);
  }

  input[type='checkbox'] {
    justify-self: center;
    accent-color: var(--accent);
  }

  .provider-line {
    grid-column: 1 / -1;
    margin: 0 0 var(--sp-1);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  .hint {
    grid-column: 1 / -1;
    margin: var(--sp-1) 0 0;
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
    color: var(--fg-faint);
  }

  .ack {
    grid-column: 1 / -1;
    display: flex;
    gap: var(--sp-2);
    align-items: flex-start;
    margin-top: var(--sp-2);
    cursor: pointer;
  }
  .ack input {
    flex: none;
    margin-top: 2px;
  }
  .ack span {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    line-height: var(--leading-normal);
  }
</style>
