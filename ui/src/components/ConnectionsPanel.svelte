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
  // The styles only the children use -- captions, the status pill -- are
  // written once below under `.conn`, as `:global` rules, so five files do
  // not each carry a copy that drifts. Switches, the segmented choice and the
  // spinner are the shared ones in app.css.

  import { onMount } from 'svelte'
  import { connections } from '../lib/connections.svelte'
  import { closesOnBack } from '../lib/phone.svelte'
  import { statusLabel, transportSummary } from '../lib/connections'
  import { plural } from '../lib/format'
  import type { ConnectionId, ConnectionView } from '../lib/types'
  import ConnectionAdd from './ConnectionAdd.svelte'
  import ConnectionDetail from './ConnectionDetail.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import Loading from './Loading.svelte'

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
    const tools = v.state.tools.length
    const line = preset ? `${preset.name} · ${where}` : where
    return tools > 0 ? `${line} · ${plural(tools, 'tool')}` : line
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
      <!-- No title of its own: the page already has one. -->
      <div class="head">
        <p class="hint lead">
          Let the assistant use other services you have an account with: search a chat, file an
          issue, look through a drive. What it may do is set per tool, inside each connection.
        </p>
        {#if connections.view && connections.connections.length > 0}
          <button class="btn btn-primary" onclick={() => (screen = { kind: 'add' })}>
            <Icon name="plus" size={13} /> Add connection
          </button>
        {/if}
      </div>
      <details class="hint how">
        <summary>How this works</summary>
        <p>
          Each connection is one account on a service that offers its tools to assistants (an “MCP
          server”), so the same service can be connected twice.
        </p>
      </details>

      {#if connections.error}<p class="error" role="alert">{connections.error}</p>{/if}

      {#if !connections.view}
        <Loading centred />
      {:else if connections.connections.length === 0}
        <EmptyState lead="Nothing is connected yet" centred={false}>
          {#snippet note()}
            Add one to let the assistant reach a service you use.
          {/snippet}
          {#snippet action()}
            <button class="btn btn-primary" onclick={() => (screen = { kind: 'add' })}>
              <Icon name="plus" size={13} /> Add connection
            </button>
          {/snippet}
        </EmptyState>
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
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  .head .hint {
    margin: 0;
  }
  .head .btn {
    flex: none;
  }
  .how summary {
    cursor: pointer;
    font-weight: 600;
  }
  .how p {
    margin: var(--sp-2) 0 0;
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
</style>
