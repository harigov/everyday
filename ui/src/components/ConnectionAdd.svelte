<script lang="ts">
  // Adding a connection: pick what it is, then fill in what that needs.
  //
  // A preset is only a starting form -- its URL or command, its kind of
  // sign-in, and the few things it cannot work without -- and every field
  // stays editable afterwards, so a service nobody listed is the two "custom"
  // entries and a server that moved is a changed URL. Nothing is sent until
  // Save, and what Save needs is said beside the field that lacks it.

  import { connections } from '../lib/connections.svelte'
  import { cleanPatch, formFromPreset, formProblem, type ConnectionForm } from '../lib/connections'
  import { openExternal } from '../lib/open-external'
  import type { ConnectionPreset, ConnectionView } from '../lib/types'
  import ConnectionFields from './ConnectionFields.svelte'

  let {
    onadded,
    oncancel,
  }: {
    /** Saved: the new connection, to open. */
    onadded: (added: ConnectionView) => void
    oncancel: () => void
  } = $props()

  /** The services first, in the catalogue's order, and the two blanks last. */
  const presets = $derived(
    [...connections.presets].sort(
      (a, b) => Number(a.key.startsWith('custom-')) - Number(b.key.startsWith('custom-')),
    ),
  )

  let preset = $state<ConnectionPreset | null>(null)
  let form = $state<ConnectionForm | null>(null)
  let saving = $state(false)

  function choose(p: ConnectionPreset) {
    preset = p
    form = formFromPreset(
      p,
      connections.connections.map((v) => v.connection.name),
    )
  }
  function back() {
    preset = null
    form = null
  }

  const problem = $derived(form ? formProblem(form, preset) : null)

  async function save() {
    if (!form || problem || saving) return
    saving = true
    try {
      const sent: ConnectionForm = $state.snapshot(form)
      sent.secrets = cleanPatch(sent.secrets)
      const added = await connections.add(sent)
      if (added) onadded(added)
    } finally {
      saving = false
    }
  }

  function kindLabel(p: ConnectionPreset): string {
    return p.transport.type === 'stdio' ? 'Runs a program on this computer' : 'Over the web'
  }
</script>

{#if !form || !preset}
  <section class="stack">
    <p class="hint">
      Choose the service. You can change anything about it afterwards, and the same service can be
      added twice, for two accounts.
    </p>
    <ul class="presets">
      {#each presets as p (p.key)}
        <li>
          <button class="preset" onclick={() => choose(p)}>
            <b>{p.name}</b>
            <span>{p.description}</span>
            <small>{kindLabel(p)}</small>
          </button>
        </li>
      {/each}
    </ul>
    <div class="actions">
      <span class="spacer"></span>
      <button class="btn" onclick={oncancel}>Cancel</button>
    </div>
  </section>
{:else}
  <section class="stack">
    {#if preset.help}
      <p class="hint lead">{preset.help}</p>
    {/if}
    {#if preset.helpUrl}
      {@const url = preset.helpUrl}
      <p class="hint">
        <button class="link" onclick={() => void openExternal(url)}>Where to find this</button>
      </p>
    {/if}

    <div class="setting">
      <label class="caption" for="conn-add-name">Name</label>
      <input
        id="conn-add-name"
        class="field"
        bind:value={form.name}
        placeholder="What you call it"
        autocomplete="off"
      />
      <span class="hint">
        The assistant refers to the connection by this name, so give two accounts of one service
        names that tell them apart.
      </span>
    </div>

    {#key preset.key}
      <ConnectionFields
        bind:transport={form.transport}
        bind:auth={form.auth}
        bind:secrets={form.secrets}
        needs={preset.needs}
      />
    {/key}

    {#if problem}<p class="problem" role="status">{problem}</p>{/if}
    {#if connections.error}<p class="error" role="alert">{connections.error}</p>{/if}

    <div class="actions">
      <button class="btn" onclick={back}>Back</button>
      <span class="spacer"></span>
      <button class="btn" onclick={oncancel}>Cancel</button>
      <button class="btn btn-primary" disabled={!!problem || saving} onclick={() => void save()}>
        {saving ? 'Adding…' : 'Add connection'}
      </button>
    </div>
  </section>
{/if}

<style>
  .stack {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    min-width: 0;
  }
  .lead {
    color: var(--fg-muted);
    margin: 0;
  }
  .stack > .hint {
    margin: 0;
  }

  .presets {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: var(--sp-3);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .preset {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 3px;
    width: 100%;
    height: 100%;
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    text-align: left;
    transition: background var(--fast) var(--ease);
  }
  .preset:hover {
    background: var(--bg-hover);
  }
  .preset b {
    font-size: var(--text-base);
    font-weight: 600;
  }
  .preset span {
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-muted);
  }
  .preset small {
    margin-top: auto;
    padding-top: var(--sp-1);
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  .actions {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding-top: var(--sp-4);
    border-top: 1px solid var(--border);
  }
  .spacer {
    flex: 1;
  }
</style>
