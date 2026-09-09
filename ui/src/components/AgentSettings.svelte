<script lang="ts">
  // Configuring the assistant.
  //
  // A dialog rather than another block in the settings dropdown, because this
  // is the one settings screen with prose in it: the instructions box is
  // where somebody writes how they want to be talked to, and a three-line
  // textarea in a popover is not somewhere anyone writes anything.
  //
  // The order of the fields is the order the decisions actually get made:
  // whether to use it at all, then what model, then the credential that model
  // needs, then how it should behave. The key is deliberately not first —
  // pasting a key before choosing an endpoint is how people end up sending
  // an OpenAI key to somebody else's gateway.

  import { agent } from '../lib/agent.svelte'
  import type { AgentSettings } from '../lib/types'
  import Icon from './Icon.svelte'

  let { onclose }: { onclose: () => void } = $props()

  // Edited on a copy. The pane has a Save, so a half-typed base URL must not
  // be what the next message is sent to.
  let draft = $state<AgentSettings | null>(null)
  let key = $state('')
  let notice = $state<string | null>(null)
  let saving = $state(false)

  $effect(() => {
    if (!draft && agent.settings) draft = structuredClone($state.snapshot(agent.settings))
  })

  void agent.loadMemories()

  /** Common endpoints, so the two local ones are not a thing to look up. */
  const PRESETS = [
    { label: 'OpenAI', url: '' },
    { label: 'Ollama', url: 'http://localhost:11434/v1' },
    { label: 'LM Studio', url: 'http://localhost:1234/v1' },
    { label: 'OpenRouter', url: 'https://openrouter.ai/api/v1' },
  ]

  const local = $derived.by(() => {
    const url = draft?.model.baseUrl
    if (!url) return false
    try {
      const host = new URL(url).hostname.toLowerCase()
      return host === 'localhost' || host === '::1' || host.startsWith('127.')
    } catch {
      return false
    }
  })

  async function save() {
    if (!draft) return
    notice = null
    saving = true
    try {
      await agent.saveSettings($state.snapshot(draft))
      if (key.trim()) {
        await agent.setKey(key.trim())
        key = ''
      }
      draft = structuredClone($state.snapshot(agent.settings!))
      notice = 'Saved.'
    } catch (e) {
      notice = e instanceof Error ? e.message : String(e)
    } finally {
      saving = false
    }
  }

  async function removeKey() {
    notice = null
    try {
      await agent.clearKey()
      if (draft) draft.hasKey = false
    } catch (e) {
      notice = e instanceof Error ? e.message : String(e)
    }
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={onclose}></div>
<div class="dialog" role="dialog" aria-label="Assistant settings">
  <header>
    <h2>Assistant</h2>
    <button class="ghost" onclick={onclose} title="Close"><Icon name="close" size={16} /></button>
  </header>

  {#if !draft}
    <p class="note">Loading…</p>
  {:else}
    <div class="body">
      <!-- The switch, and the sentence that has to be next to it. This is
           the only feature in the application that sends what you wrote to a
           computer you do not own, and that is not something to discover. -->
      <label class="toggle">
        <input type="checkbox" bind:checked={draft.enabled} />
        <span>
          <b>Use the assistant</b>
          <small>
            {#if local}
              Your journal is sent to the model running on this machine. Nothing leaves it.
            {:else}
              What you ask about — entries, tasks, whatever it reads — is sent to the model provider
              you configure below.
            {/if}
          </small>
        </span>
      </label>

      <section>
        <span class="eyebrow">Model</span>
        <div class="presets">
          {#each PRESETS as preset (preset.label)}
            <button
              class="chip"
              class:on={(draft.model.baseUrl ?? '') === preset.url}
              onclick={() => draft && (draft.model.baseUrl = preset.url || null)}
            >
              {preset.label}
            </button>
          {/each}
        </div>

        <label class="field">
          <span>Model name</span>
          <input bind:value={draft.model.model} placeholder="gpt-5.1-mini" spellcheck="false" />
        </label>

        <label class="field">
          <span>Base URL</span>
          <input
            value={draft.model.baseUrl ?? ''}
            oninput={(e) => draft && (draft.model.baseUrl = e.currentTarget.value.trim() || null)}
            placeholder="https://api.openai.com/v1"
            spellcheck="false"
          />
        </label>
        <p class="hint">Anything that speaks the OpenAI chat API. Leave empty for OpenAI itself.</p>
      </section>

      <section>
        <span class="eyebrow">API key</span>
        {#if draft.hasKey}
          <div class="stored">
            <span><Icon name="lock" size={14} /> A key is stored in this vault.</span>
            <button class="link" onclick={() => void removeKey()}>Remove</button>
          </div>
        {/if}
        <label class="field">
          <span>{draft.hasKey ? 'Replace it' : 'Key'}</span>
          <input
            type="password"
            bind:value={key}
            placeholder={local ? 'Not needed for a local model' : 'sk-…'}
            spellcheck="false"
            autocomplete="off"
          />
        </label>
        <p class="hint">
          Encrypted with everything else in the vault, so it is unreadable while the vault is locked
          — and the assistant cannot spend it while locked either.
        </p>
      </section>

      <section>
        <span class="eyebrow">Instructions</span>
        <textarea
          bind:value={draft.instructions}
          rows="6"
          placeholder="How you want it to behave. Be terse. Call me by my first name. I plan on Sunday evenings. Never use exclamation marks."
        ></textarea>
        <p class="hint">Sent with every message, before its own instructions.</p>
      </section>

      <section>
        <span class="eyebrow">Behaviour</span>
        <label class="toggle">
          <input type="checkbox" bind:checked={draft.confirmDestructive} />
          <span>
            <b>Ask before deleting anything</b>
            <small>There is no undo in this application. Leaving this on is the undo.</small>
          </span>
        </label>
        <label class="toggle">
          <input type="checkbox" bind:checked={draft.remember} />
          <span>
            <b>Let it remember things about you</b>
            <small>Short notes it keeps between conversations. Listed below.</small>
          </span>
        </label>
        <label class="field">
          <span>Steps per request</span>
          <input type="number" min="1" max="100" bind:value={draft.maxSteps} />
        </label>
        <p class="hint">
          How many times it may call a tool before giving up on one request. A ceiling, not a
          target.
        </p>
      </section>

      {#if agent.memories.length > 0}
        <section>
          <span class="eyebrow">What it remembers</span>
          {#each agent.memories as memory (memory.id)}
            <div class="memory">
              <span>{memory.text}</span>
              <button class="link" onclick={() => void agent.forget(memory.id)}>Forget</button>
            </div>
          {/each}
        </section>
      {/if}
    </div>

    <footer>
      {#if notice}<span class="notice">{notice}</span>{/if}
      <button class="primary" onclick={() => void save()} disabled={saving}>
        {saving ? 'Saving…' : 'Save'}
      </button>
    </footer>
  {/if}
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 60;
    background: rgb(0 0 0 / 0.28);
  }
  .dialog {
    position: fixed;
    z-index: 61;
    top: 50%;
    left: 50%;
    translate: -50% -50%;
    display: flex;
    flex-direction: column;
    width: min(560px, calc(100vw - var(--sp-8)));
    max-height: min(760px, calc(100vh - var(--sp-8)));
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-raised);
    box-shadow: var(--shadow-lg);
  }

  header {
    display: flex;
    align-items: center;
    padding: var(--sp-3) var(--sp-4);
    border-bottom: 1px solid var(--border);
  }
  h2 {
    flex: 1;
    margin: 0;
    font-size: var(--text-md);
  }
  .ghost {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg-subtle);
    cursor: pointer;
  }
  .ghost:hover {
    background: var(--bg-hover);
  }

  .body {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: var(--sp-4);
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .eyebrow {
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }

  .toggle {
    display: flex;
    gap: var(--sp-3);
    align-items: flex-start;
    cursor: pointer;
  }
  .toggle input {
    margin-top: 2px;
  }
  .toggle span {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .toggle b {
    font-size: var(--text-base);
    font-weight: 500;
  }
  .toggle small,
  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    color: var(--fg-subtle);
  }

  .presets {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  .chip {
    padding: var(--sp-1) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--bg);
    color: var(--fg-muted);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .chip.on {
    border-color: var(--accent);
    background: var(--bg-selected);
    color: var(--fg);
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .field span {
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  input,
  textarea {
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
  }
  textarea {
    resize: vertical;
    line-height: var(--leading-normal);
  }
  input:focus,
  textarea:focus {
    outline: none;
    border-color: var(--accent);
  }
  input[type='checkbox'] {
    width: auto;
    padding: 0;
  }
  input[type='number'] {
    width: 90px;
  }

  .stored,
  .memory {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    font-size: var(--text-sm);
  }
  .stored span,
  .memory span {
    display: flex;
    flex: 1;
    align-items: center;
    gap: var(--sp-2);
    color: var(--fg-muted);
  }
  .link {
    border: 0;
    background: none;
    color: var(--accent);
    font: inherit;
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .link:hover {
    text-decoration: underline;
  }

  footer {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--sp-3);
    padding: var(--sp-3) var(--sp-4);
    border-top: 1px solid var(--border);
  }
  .notice {
    flex: 1;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .primary {
    padding: var(--sp-2) var(--sp-4);
    border: 0;
    border-radius: var(--radius);
    background: var(--accent);
    color: var(--fg-on-accent);
    font: inherit;
    font-size: var(--text-base);
    cursor: pointer;
  }
  .primary:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
