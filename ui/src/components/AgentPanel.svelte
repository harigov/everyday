<script lang="ts">
  // Configuring the assistant: the Assistant tab of the settings dialog.
  //
  // This was a dialog of its own, raised out of the settings *popover* -- so
  // the application had two settings surfaces, one of which had to close
  // before the other could open, and neither of which contained the other.
  // Now there is one dialog with tabs and this is one of them; what is left
  // here is the fields and the Save, and none of the chrome.
  //
  // The order of the fields is the order the decisions actually get made:
  // whether to use it at all, then what model, then the credential that model
  // needs, then how it should behave. The key is deliberately not first —
  // pasting a key before choosing an endpoint is how people end up sending
  // an OpenAI key to somebody else's gateway.

  import { onDestroy } from 'svelte'
  import { isLoopback } from '../lib/agent'
  import { agent } from '../lib/agent.svelte'
  import type { AgentSettings } from '../lib/types'
  import Icon from './Icon.svelte'

  // Edited on a copy. The pane has a Save, so a half-typed base URL must not
  // be what the next message is sent to.
  let draft = $state<AgentSettings | null>(null)
  let key = $state('')
  let notice = $state<string | null>(null)
  let saving = $state(false)
  /**
   * Clears "Saved." after a moment, and only that.
   *
   * A confirmation is true when it appears and stops being true the moment
   * anything is typed after it -- so a footer reading "Saved." beside three
   * fields that are not is worse than a footer reading nothing. A failure is
   * the opposite: it stays, because it is a thing to act on rather than a
   * thing to notice.
   */
  let noticeTimer: ReturnType<typeof setTimeout> | null = null
  function confirm(message: string) {
    notice = message
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = setTimeout(() => {
      noticeTimer = null
      notice = null
    }, 4000)
  }
  onDestroy(() => {
    if (noticeTimer) clearTimeout(noticeTimer)
  })

  // The dialog loads the settings; this waits for them. Both are needed: the
  // tab can be opened before the round trip has come back.
  void agent.load()
  void agent.loadMemories()

  $effect(() => {
    if (!draft && agent.settings) draft = structuredClone($state.snapshot(agent.settings))
  })

  const localZone = Intl.DateTimeFormat().resolvedOptions().timeZone

  /**
   * Every zone the platform knows, with this machine's first if it is not
   * already in the list.
   *
   * `supportedValuesOf` is in every engine this application runs in; the
   * fallback is a short list rather than an empty picker, because a select
   * with one option in it reads as broken.
   */
  const ZONES: string[] = (() => {
    const of = (Intl as { supportedValuesOf?: (k: string) => string[] }).supportedValuesOf
    const all = of ? of('timeZone') : ['UTC', localZone]
    return [...new Set([localZone, ...all])].sort()
  })()

  /** Common endpoints, so the two local ones are not a thing to look up. */
  const PRESETS = [
    { label: 'OpenAI', url: '' },
    { label: 'Ollama', url: 'http://localhost:11434/v1' },
    { label: 'LM Studio', url: 'http://localhost:1234/v1' },
    { label: 'OpenRouter', url: 'https://openrouter.ai/api/v1' },
  ]

  /**
   * Is the model on this machine?
   *
   * `isLoopback`, and not a second opinion. This was a copy of it that had
   * fallen a case or two behind -- it missed `[::1]`, which is the form
   * `URL.hostname` actually returns for an IPv6 address, and `0.0.0.0`. So an
   * Ollama endpoint on IPv6 was told "what you ask about is sent to the model
   * provider you configure below" while `agent.ready` was treating the same
   * address as local and not asking for a key. Of the two things this decides
   * -- whether to demand an API key, and what to say about where your journal
   * goes -- disagreeing about the second is the one that matters.
   */
  const local = $derived(isLoopback(draft?.model.baseUrl ?? null))

  async function save() {
    if (!draft) return
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = null
    notice = null
    saving = true
    try {
      await agent.saveSettings($state.snapshot(draft))
      if (key.trim()) {
        await agent.setKey(key.trim())
        key = ''
      }
      draft = structuredClone($state.snapshot(agent.settings!))
      confirm('Saved.')
    } catch (e) {
      notice = e instanceof Error ? e.message : String(e)
    } finally {
      saving = false
    }
  }

  async function removeKey() {
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = null
    notice = null
    try {
      await agent.clearKey()
      if (draft) draft.hasKey = false
    } catch (e) {
      notice = e instanceof Error ? e.message : String(e)
    }
  }
</script>

{#if !agent.supported}
  <p class="hint">
    This vault's storage does not carry the assistant's conversations, so there is nothing to
    configure. A SQLite or Postgres vault does.
  </p>
{:else if !draft}
  <p class="hint">Loading…</p>
{:else}
  <div class="panes">
    <!-- The switch, and the sentence that has to be next to it. This is the
         only feature in the application that sends what you wrote to a
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

      <label class="setting">
        <span>Model name</span>
        <input bind:value={draft.model.model} placeholder="gpt-5.1-mini" spellcheck="false" />
      </label>

      <label class="setting">
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
      <label class="setting">
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
        Encrypted with everything else in the vault, so it is unreadable while the vault is locked —
        and the assistant cannot spend it while locked either.
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
      <label class="setting narrow">
        <span>Steps per request</span>
        <input type="number" min="1" max="100" bind:value={draft.maxSteps} />
      </label>
      <p class="hint">
        How many times it may call a tool before giving up on one request. A ceiling, not a target.
      </p>
    </section>

    <section>
      <span class="eyebrow">Your time zone</span>
      <select
        class="field"
        value={draft.timezone ?? ''}
        onchange={(e) => (draft!.timezone = e.currentTarget.value || null)}
      >
        <option value="">This computer's ({localZone})</option>
        {#each ZONES as zone (zone)}
          <option value={zone}>{zone}</option>
        {/each}
      </select>
      <p class="hint">
        Where <em>you</em> are, which is not always where the vault is. A vault served from a machine
        under a desk has that machine's clock, and “seven in the morning” has to mean seven where you
        are.
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

  <div class="save">
    {#if notice}<span class="notice">{notice}</span>{/if}
    <button class="btn btn-primary" onclick={() => void save()} disabled={saving}>
      {saving ? 'Saving…' : 'Save'}
    </button>
  </div>
{/if}

<style>
  /* Every box in here is `width: 100%` of a column that has `min-width: 0`.
     Both halves are needed and neither is obvious: a flex item's default
     `min-width: auto` is its *content* width, so a text input -- which has an
     intrinsic width of about twenty characters plus its own padding -- was
     wider than the column it sat in and pushed out of the panel rather than
     shrinking to it. The dialog then scrolled sideways, which is how a
     settings pane ends up with its labels cut off down the right-hand edge. */
  .panes {
    display: flex;
    flex-direction: column;
    gap: var(--sp-6);
    min-width: 0;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  .eyebrow {
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }

  .toggle {
    display: flex;
    gap: var(--sp-3);
    align-items: flex-start;
    min-width: 0;
    cursor: pointer;
  }
  .toggle input {
    flex: none;
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .toggle span {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .toggle b {
    font-size: var(--text-base);
    font-weight: 600;
  }
  .toggle small,
  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }

  .presets {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  .chip {
    height: 30px;
    padding: 0 var(--sp-3);
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--bg);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .chip.on {
    border-color: var(--accent);
    background: var(--bg-selected);
    color: var(--fg);
  }

  /* Deliberately not named for the global utility of the same job in
     `app.css`. That one is a 38px-high bordered box; this is the *label*
     wrapped around one. Wearing both meant the label was clamped to the
     height of a single input while holding a caption and an input, so the
     caption was drawn straight through the box below it and the pane read as
     a stack of overlapping fields. That is what this pane was reported for. */
  .setting {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }
  .setting > span {
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }
  input,
  textarea {
    width: 100%;
    min-width: 0;
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
    user-select: text;
  }
  textarea {
    resize: vertical;
    line-height: var(--leading-normal);
  }
  input:focus,
  textarea:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }
  input[type='checkbox'] {
    width: auto;
    padding: 0;
  }
  /* The one field with a natural size: a step count is two digits, and a
     box the width of the panel invites a number nobody meant to type. */
  .setting.narrow input {
    width: 110px;
  }

  .stored,
  .memory {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    font-size: var(--text-sm);
    min-width: 0;
  }
  .stored span,
  .memory span {
    display: flex;
    flex: 1;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
    line-height: var(--leading-normal);
    color: var(--fg-muted);
    /* A remembered note is a sentence somebody's model wrote, and it can be
       one very long word. It wraps rather than widening the panel. */
    overflow-wrap: anywhere;
  }
  .memory + .memory {
    margin-top: calc(var(--sp-2) * -1 + var(--sp-2));
  }
  .link {
    flex: none;
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .link:hover {
    text-decoration: underline;
  }

  .save {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--sp-3);
    margin-top: var(--sp-6);
    padding-top: var(--sp-4);
    border-top: 1px solid var(--border);
  }
  .notice {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
</style>
