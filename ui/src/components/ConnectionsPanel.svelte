<script lang="ts">
  // Settings → Connections: the other services the assistant can use, each
  // through that service's MCP server.
  //
  // A platform setting rather than any app's: a connection serves the
  // assistant in every app, and what it may do is decided here once, tool by
  // tool, rather than wherever it happens to be asked. See
  // `docs/plans/connections.md`.
  //
  // Three screens in one tab, swapped in place the way a page of Settings
  // drills down: the list, the add form, and one connection's detail. The
  // list and the shared look of all three live here; the add form, the
  // detail, the fields they share and the tool list are their own files.
  //
  // The styles the children use -- captions, switches, the three-way choice,
  // the status pill -- are written once below under `.conn`, as `:global`
  // rules, so five files do not each carry a copy that drifts.

  import { onMount } from 'svelte'
  import { connections } from '../lib/connections.svelte'
  import { closesOnBack } from '../lib/phone.svelte'
  import { statusLabel, transportSummary } from '../lib/connections'
  import type { ConnectionId, ConnectionView } from '../lib/types'
  import ConnectionAdd from './ConnectionAdd.svelte'
  import ConnectionDetail from './ConnectionDetail.svelte'
  import Icon from './Icon.svelte'

  // Loading, and reloading while this is open, last as long as the pane.
  onMount(() => connections.watch())

  type Screen =
    { kind: 'list' } | { kind: 'add' } | { kind: 'detail'; id: ConnectionId; fresh: boolean }
  let screen = $state<Screen>({ kind: 'list' })

  // On a phone, back goes from the form or one connection to the list.
  closesOnBack(
    () => screen.kind !== 'list',
    () => (screen = { kind: 'list' }),
  )

  function added(v: ConnectionView) {
    screen = { kind: 'detail', id: v.connection.id, fresh: true }
  }

  /** What the list says under a name: the service it is, and where it goes. */
  function subtitle(v: ConnectionView): string {
    const preset = connections.presets.find((p) => p.key === v.connection.preset)
    const where = transportSummary(v.connection.transport)
    return preset ? `${preset.name} · ${where}` : where
  }
</script>

<div class="conn">
  {#if screen.kind === 'add'}
    <ConnectionAdd onadded={added} oncancel={() => (screen = { kind: 'list' })} />
  {:else if screen.kind === 'detail'}
    {#key screen.id}
      <ConnectionDetail
        id={screen.id}
        fresh={screen.fresh}
        onclose={() => (screen = { kind: 'list' })}
      />
    {/key}
  {:else}
    <section class="list">
      <div class="head">
        <span class="eyebrow">Connected services</span>
        <button class="btn btn-primary" onclick={() => (screen = { kind: 'add' })}>
          <Icon name="plus" size={13} /> Add connection
        </button>
      </div>
      <p class="hint lead">
        The assistant can use other services through their MCP servers: search a chat, file an
        issue, read a pull request, look through a drive. Each connection is one account, so the
        same service can be connected twice. What the assistant may do with a connection is set per
        tool, inside it.
      </p>

      {#if connections.error}<p class="error" role="alert">{connections.error}</p>{/if}

      {#if !connections.view}
        <p class="hint">Loading…</p>
      {:else if connections.connections.length === 0}
        <p class="hint">
          Nothing is connected yet. Add one to let the assistant reach a service you use.
        </p>
      {:else}
        <ul class="rows">
          {#each connections.connections as v (v.connection.id)}
            {@const busy = connections.busy[v.connection.id]}
            <li>
              <button
                class="list-row row"
                onclick={() => (screen = { kind: 'detail', id: v.connection.id, fresh: false })}
              >
                <span class="main">
                  <span class="name">{v.connection.name}</span>
                  <span class="sub">{subtitle(v)}</span>
                </span>
                {#if busy}<span class="spinner" role="status" aria-label="Working"></span>{/if}
                <span class="status tone-{v.status}">
                  <span class="dot"></span>{statusLabel(v.status)}
                </span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  {/if}
</div>

<style>
  .conn {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .list {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  .list > .hint {
    margin: 0;
  }
  .lead {
    color: var(--fg-muted);
  }

  .rows {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  /* `.list-row` is the card; this is what sits inside one here. It is drawn
     the full width of the column rather than inset, since nothing around it
     is a list's own gutter. */
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    width: 100%;
    margin-inline: 0;
    min-height: var(--row-h);
    padding: var(--sp-2) var(--sp-3);
  }
  .main {
    display: flex;
    flex: 1;
    flex-direction: column;
    min-width: 0;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-base);
    font-weight: 550;
    color: var(--fg);
  }
  .sub {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  /* ── Shared by this tab's other files ──────────────────────────────── */

  /* A caption over a field -- not app.css's `.field-label`, which sits in a
     block layout and carries a bottom margin the stacked fields here space
     with `gap` instead. */
  .conn :global(.setting) {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }
  .conn :global(.caption) {
    display: flex;
    align-items: center;
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }
  .conn :global(.pair) {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: var(--sp-3);
    min-width: 0;
  }
  .conn :global(.mono) {
    font-family: var(--font-mono);
    font-size: var(--text-sm);
  }
  .conn :global(.problem) {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--danger);
    overflow-wrap: anywhere;
  }
  .conn :global(.hint) {
    overflow-wrap: anywhere;
  }

  /* A text-only action inside a line of settings: Remove, Reset, Show more. */
  .conn :global(.link) {
    flex: none;
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .conn :global(.link:hover) {
    text-decoration: underline;
  }
  .conn :global(.link:disabled) {
    opacity: 0.5;
    text-decoration: none;
  }

  .conn :global(.more > summary) {
    cursor: pointer;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .conn :global(.more-body) {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    margin-top: var(--sp-3);
  }

  /* The three-way choice: the same control as Backup's destination and Lock
     after, whose styles are scoped to those files. */
  .conn :global(.segmented) {
    display: flex;
    gap: 3px;
    padding: 3px;
    background: var(--bg-sunken);
    border-radius: var(--radius);
  }
  .conn :global(.seg) {
    flex: 1;
    height: 28px;
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-subtle);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .conn :global(.seg:hover) {
    color: var(--fg);
  }
  .conn :global(.seg.on) {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }

  .conn :global(.toggle) {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    min-width: 0;
    cursor: pointer;
  }
  .conn :global(.toggle.disabled) {
    opacity: 0.5;
    cursor: default;
  }
  .conn :global(.toggle input) {
    flex: none;
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .conn :global(.toggle span) {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .conn :global(.toggle b) {
    font-size: var(--text-base);
    font-weight: 600;
  }
  .conn :global(.toggle small) {
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }

  /* How a connection is doing, in the theme's own colours. */
  .conn :global(.status) {
    display: inline-flex;
    flex: none;
    align-items: center;
    gap: 6px;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .conn :global(.status .dot) {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--fg-faint);
  }
  .conn :global(.status.tone-ready .dot) {
    background: var(--success);
  }
  .conn :global(.status.tone-ready) {
    color: color-mix(in oklab, var(--success) 80%, var(--fg));
  }
  .conn :global(.status.tone-needsSignIn .dot),
  .conn :global(.status.tone-needsSetup .dot) {
    background: var(--warning);
  }
  .conn :global(.status.tone-needsSignIn),
  .conn :global(.status.tone-needsSetup) {
    color: color-mix(in oklab, var(--warning) 80%, var(--fg));
  }
  .conn :global(.status.tone-error .dot) {
    background: var(--danger);
  }
  .conn :global(.status.tone-error) {
    color: var(--danger);
  }

  .conn :global(.spinner) {
    flex: none;
    width: 14px;
    height: 14px;
    border: 2px solid var(--border-strong);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: conn-spin 0.9s linear infinite;
  }
  /* Global by name: scoping would rename it away from the `:global` rule above. */
  @keyframes -global-conn-spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .conn :global(.spinner) {
      animation-duration: 3s;
    }
  }
</style>
