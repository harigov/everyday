<script lang="ts">
  // What the assistant may do with one connection: every tool the server
  // lists, each with its own say -- run it, ask first, or leave it out.
  //
  // A tool nobody has chosen for follows its own hints, read-only runs and
  // everything else asks (see `defaultPolicy`), but hints are only the
  // server's claim about itself, so each row can be set by hand and "Reset"
  // goes back to following the hints. The list is what the last check
  // found; the server may have changed since, which is why checking is a
  // button and not something this draws on its own.

  import { connections } from '../lib/connections.svelte'
  import { toolRows } from '../lib/connections'
  import type { ConnectionView, ToolPolicy } from '../lib/types'

  let { view }: { view: ConnectionView } = $props()

  const id = $derived(view.connection.id)
  let query = $state('')
  /** Tools whose long description is open in full. */
  let open = $state<string[]>([])

  const rows = $derived(toolRows(view, query))
  const total = $derived(view.state.tools.length)

  const POLICIES: { value: ToolPolicy; label: string; hint: string }[] = [
    { value: 'allow', label: 'Allow', hint: 'Runs without asking' },
    { value: 'ask', label: 'Ask', hint: 'Asks you each time' },
    { value: 'off', label: 'Off', hint: 'Never used or offered' },
  ]

  function toggle(name: string) {
    open = open.includes(name) ? open.filter((n) => n !== name) : [...open, name]
  }
  function long(description: string): boolean {
    return description.length > 150 || description.includes('\n')
  }
</script>

{#if total === 0}
  <p class="hint">Check the connection to see its tools.</p>
{:else}
  <div class="tools-head">
    <input
      class="field"
      type="search"
      aria-label="Search this connection’s tools"
      placeholder="Search {total} tools"
      bind:value={query}
      autocomplete="off"
    />
  </div>

  {#if rows.length === 0}
    <p class="hint">No tool matches “{query}”.</p>
  {:else}
    <ul class="tools">
      {#each rows as row (row.tool.name)}
        {@const t = row.tool}
        {@const expanded = open.includes(t.name)}
        <li class="tool" class:off={row.policy === 'off'}>
          <div class="tool-text">
            <div class="tool-title">
              <b class="mono">{t.name}</b>
              {#if t.title && t.title !== t.name}<span class="tool-nick">{t.title}</span>{/if}
              {#if t.hints.readOnly}<span class="tag">Read-only</span>{/if}
              {#if t.hints.destructive}<span class="tag danger">Destructive</span>{/if}
            </div>
            {#if t.description}
              <p class="tool-desc" class:clamped={!expanded}>{t.description}</p>
              {#if long(t.description)}
                <button class="link" onclick={() => toggle(t.name)} aria-expanded={expanded}>
                  {expanded ? 'Show less' : 'Show more'}
                </button>
              {/if}
            {/if}
          </div>
          <div class="tool-set">
            <div
              class="seg-group"
              role="radiogroup"
              aria-label="What happens when the assistant uses {t.name}"
            >
              {#each POLICIES as p (p.value)}
                <button
                  class="seg-opt"
                  class:on={row.policy === p.value}
                  role="radio"
                  aria-checked={row.policy === p.value}
                  title={p.hint}
                  onclick={() => void connections.setPolicy(id, t.name, p.value)}
                >
                  {p.label}
                </button>
              {/each}
            </div>
            {#if row.chosen}
              <button
                class="link"
                title="Follow the tool’s own hints again"
                onclick={() => void connections.setPolicy(id, t.name, null)}
              >
                Reset
              </button>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
  {/if}
{/if}

<style>
  .tools {
    display: flex;
    flex-direction: column;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .tool {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-4);
    padding: var(--sp-3) 0;
    border-top: 1px solid var(--border);
    min-width: 0;
  }
  .tool:first-child {
    border-top: none;
  }
  .tool.off .tool-text {
    opacity: 0.55;
  }
  .tool-text {
    display: flex;
    flex: 1;
    flex-direction: column;
    align-items: flex-start;
    gap: 3px;
    min-width: 0;
  }
  .tool-title {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }
  .tool-title b {
    font-size: var(--text-sm);
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .tool-nick {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .tag.danger {
    color: var(--danger);
    border-color: color-mix(in oklab, var(--danger) 35%, var(--border));
  }
  .tool-desc {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-muted);
    overflow-wrap: anywhere;
    white-space: pre-line;
  }
  .tool-desc.clamped {
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    overflow: hidden;
  }
  .tool-set {
    display: flex;
    flex: none;
    flex-direction: column;
    align-items: flex-end;
    gap: var(--sp-2);
  }
  .tool-set .seg-group {
    width: 168px;
  }
</style>
