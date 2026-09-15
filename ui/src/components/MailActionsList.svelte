<script lang="ts">
  // What one kind of non-person caller has done with mail: the assistant, or
  // an MCP client.
  //
  // Shared between `McpPanel.svelte`'s own "What connected agents did with
  // mail" and `AgentPanel.svelte`'s matching section for the assistant,
  // rather than two copies of the same list -- `docs/plans/mail.md`'s risk
  // table asks for both ("Settings → Sharing lists what each MCP client did
  // through `origin`"), and a person reading either should see the same
  // shape: newest first, an account, a subject, a link into Mail. MCP is
  // grouped by client, because more than one can be connected; the assistant
  // is always exactly one, so its own list is left flat.
  //
  // `mail_actions_by_origin` resolves the subject and the account itself --
  // this component only draws what comes back and opens Mail when a row
  // names a thread.
  import { api } from '../lib/api'
  import { relativeTime } from '../lib/format'
  import { mail } from '../lib/mail.svelte'
  import { panels } from '../lib/panels.svelte'
  import type { MailActionByOrigin, MailAgentOriginKind } from '../lib/types'

  let { kind }: { kind: MailAgentOriginKind } = $props()

  let rows = $state<MailActionByOrigin[]>([])
  let loaded = $state(false)

  async function load() {
    try {
      rows = await api.mailActionsByOrigin(kind, 20)
    } catch {
      // Nothing useful to say in a settings panel over a list that is
      // already the least important thing on the page -- the row simply
      // stays empty and the section does not draw.
      rows = []
    } finally {
      loaded = true
    }
  }
  void load()

  interface Group {
    label: string | null
    rows: MailActionByOrigin[]
  }

  const groups = $derived.by((): Group[] => {
    if (kind !== 'mcp') return [{ label: null, rows }]
    const byClient = new Map<string, MailActionByOrigin[]>()
    for (const row of rows) {
      const label = row.client ?? 'An unnamed client'
      const list = byClient.get(label)
      if (list) list.push(row)
      else byClient.set(label, [row])
    }
    return [...byClient.entries()].map(([label, rows]) => ({ label, rows }))
  })

  function open(row: MailActionByOrigin) {
    if (!row.threadId) return
    // Settings is a dialog over the app bar, not a screen of its own --
    // closed first, the same way `AgentPanel`'s own "See them" link into
    // Memory does, so the click lands on Mail rather than on Mail sitting
    // behind a dialog still open.
    panels.closeSettings()
    void mail.openFromElsewhere(row.threadId)
  }
</script>

{#if loaded && rows.length > 0}
  <section class="mail-actions">
    <span class="eyebrow">
      What {kind === 'mcp' ? 'connected agents' : 'the assistant'} did with mail
    </span>
    {#each groups as group (group.label ?? '')}
      <div class="group">
        {#if group.label}<span class="group-name">{group.label}</span>{/if}
        {#each group.rows as row (row.opId)}
          <button class="row" disabled={!row.threadId} onclick={() => open(row)}>
            <span class="what">
              {row.state}
              <b>{row.subject ?? '(no subject)'}</b>
              <small class="account">{row.account}</small>
            </span>
            <span class="when">{relativeTime(row.at)}</span>
          </button>
        {/each}
      </div>
    {/each}
  </section>
{/if}

<style>
  .mail-actions {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }

  .group {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }

  .group-name {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }

  .row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--sp-2);
    width: 100%;
    padding: 0.35rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .row:disabled {
    cursor: default;
    opacity: 0.7;
  }

  .row:not(:disabled):hover {
    background: var(--bg-hover);
  }

  .what {
    min-width: 0;
    overflow-wrap: anywhere;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  .what b {
    color: var(--fg);
    font-weight: 600;
  }

  .account {
    margin-left: 0.4em;
    color: var(--fg-faint);
  }

  .when {
    flex: none;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
</style>
