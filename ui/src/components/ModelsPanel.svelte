<script lang="ts">
  // Settings → Models: which model each size of work runs on, and where.
  //
  // Application settings rather than the assistant's -- the assistant is one
  // of three things here that ask a model, and its own tab is about who it is
  // and how it behaves. Three cards, largest first, each opening with what it
  // is used for, because that is what decides which model belongs in it.
  //
  // A key belongs to an endpoint, not to a card: two cards at one address
  // share one key, and the card says so. Moving a card to another address
  // does not take its key along -- which is the point, since the other
  // address belongs to somebody else. Within a card the order is the order
  // the decisions get made: where, which model, then the credential. Pasting
  // a key before choosing an endpoint is how people end up sending an OpenAI
  // key to somebody else's gateway.

  import { onDestroy } from 'svelte'
  import { KNOWN_LLM_ENDPOINTS, endpointOf, providerLabel } from '../lib/accounts'
  import { isLoopback } from '../lib/agent'
  import { models } from '../lib/models.svelte'
  import { quick } from '../lib/quick.svelte'
  import type {
    LLMConfig,
    ModelSettings,
    ModelTier,
    QuickAskRow,
    ReasoningEffort,
  } from '../lib/types'
  import Icon from './Icon.svelte'

  // Edited on a copy. The pane has a Save, so a half-typed base URL must not
  // be what the next message is sent to.
  let draft = $state<ModelSettings | null>(null)
  let keys = $state<Record<ModelTier, string>>({ large: '', medium: '', small: '' })
  let notice = $state<string | null>(null)
  let saving = $state(false)
  /** Clears "Saved." after a moment, and only that -- see `AgentPanel`. */
  let noticeTimer: ReturnType<typeof setTimeout> | null = null
  function confirm(message: string) {
    notice = message
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = setTimeout(() => {
      noticeTimer = null
      notice = null
    }, 4000)
  }
  function clearNotice() {
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = null
    notice = null
  }
  onDestroy(() => {
    if (noticeTimer) clearTimeout(noticeTimer)
  })

  // Fresh every time the tab opens, and the draft seeded from that read
  // rather than from whatever was loaded at startup: seeded from the cache,
  // a Save would write back settings another window has changed since, and
  // undo them -- clearing every account's acknowledgement on the way.
  void models.refresh().then((ok) => {
    if (ok && models.settings) draft = structuredClone($state.snapshot(models.settings))
  })
  void quick.load()

  type Card = {
    tier: ModelTier
    title: string
    /** What it is used for -- the first thing a card says. */
    uses: string
    /** What a card switched on starts as. `null` for Medium, which is never off. */
    fresh: Omit<LLMConfig, 'provider' | 'baseUrl'> | null
    placeholder: string
    /** Under the reasoning effort: what to weigh when picking one for this work. */
    effortHint: string
    /** The levels this card offers. */
    efforts: { value: ReasoningEffort; label: string }[]
  }

  /**
   * Every level any endpoint takes, in order. No per-model list: model names
   * are free text, and one that does not take a level says so when it is
   * next asked -- see `ReasoningEffort` in `crates/everyday-core/src/llm.rs`.
   */
  const EFFORTS: { value: ReasoningEffort; label: string }[] = [
    { value: 'none', label: 'None' },
    { value: 'minimal', label: 'Minimal' },
    { value: 'low', label: 'Low' },
    { value: 'medium', label: 'Medium' },
    { value: 'high', label: 'High' },
    { value: 'xhigh', label: 'Extra high' },
    { value: 'max', label: 'Max' },
  ]

  /**
   * Small's levels: Low and below. The save refuses more
   * (`SMALL_MAX_EFFORT` in `crates/everyday-core/src/llm.rs`), because a quick
   * ask spends its few tokens and seconds thinking and never answers.
   */
  const SMALL_EFFORTS = EFFORTS.filter((e) => ['none', 'minimal', 'low'].includes(e.value))

  const CARDS: Card[] = [
    {
      tier: 'large',
      title: 'Large',
      uses: 'Deep research: the jobs the assistant goes away to work on, reading pages and writing up what it found as a note.',
      fresh: { model: 'gpt-5.1', temperature: null, maxTokens: null, reasoningEffort: null },
      placeholder: 'gpt-5.1',
      effortHint:
        'How long it thinks before it acts. A research job runs unattended, so this is where High is worth what it costs.',
      efforts: EFFORTS,
    },
    {
      tier: 'medium',
      title: 'Medium',
      uses: 'The assistant: conversations, routines, dreams, meeting notes, and the mail features that write prose — summaries and drafts.',
      fresh: null,
      placeholder: 'gpt-5.1-mini',
      effortHint:
        'How long it thinks before it answers or reaches for a tool. Higher is slower and costs more, and plans multi-step work better — mail summaries and drafts wait for it too.',
      efforts: EFFORTS,
    },
    {
      tier: 'small',
      title: 'Small',
      uses: 'Quick, one-shot work: filling in fields and suggesting things as you type, sorting mail, and suggesting short replies.',
      fresh: { model: 'gpt-5.1-nano', temperature: 0, maxTokens: 1024, reasoningEffort: null },
      placeholder: 'gpt-5.1-nano',
      effortHint:
        'Low at most: these run as you type, and thinking comes out of the few tokens and seconds a quick answer is given.',
      efforts: SMALL_EFFORTS,
    },
  ]

  /**
   * Turning Large or Small on and off.
   *
   * Off writes `null` rather than an empty model name, because those mean
   * different things: null is "do not do this work on a model of its own"
   * (Large falls back to Medium; Small's work is simply not done), and an
   * empty name is a configuration that fails at the endpoint. On starts at
   * Medium's address, so it shares the key that is already there.
   */
  function toggle(card: Card, on: boolean) {
    const tier = card.tier
    if (!draft || !card.fresh || tier === 'medium') return
    draft[tier] = on ? { provider: 'openAi', baseUrl: draft.medium.baseUrl, ...card.fresh } : null
  }

  function configOf(tier: ModelTier): LLMConfig | null {
    return draft?.[tier] ?? null
  }

  /** Where the tier points as saved -- what a key command acts on. */
  function savedEndpoint(tier: ModelTier): string | null {
    const saved = models.settings?.[tier]
    return saved ? endpointOf(saved.baseUrl) : null
  }

  /** The other cards at the same address as this one, which share its key. */
  function sharing(tier: ModelTier): string[] {
    const mine = configOf(tier)
    if (!mine) return []
    return CARDS.filter(
      (c) =>
        c.tier !== tier &&
        configOf(c.tier) != null &&
        endpointOf(configOf(c.tier)!.baseUrl) === endpointOf(mine.baseUrl),
    ).map((c) => c.title)
  }

  /**
   * The quick-ask switches, grouped the way the app bar is -- moved here from
   * the Assistant tab with the small model they run on.
   *
   * Grouped rather than listed because twenty checkboxes in one column is a
   * wall nobody reads, and the decision somebody is actually making is
   * per-app: "the library may look things up, the journal may not".
   */
  const grouped = $derived(
    quick.asks.reduce<Record<string, QuickAskRow[]>>((acc, ask) => {
      ;(acc[ask.app] ??= []).push(ask)
      return acc
    }, {}),
  )

  /**
   * A quick-ask switch, saved on the spot rather than with the pane: these are
   * twenty checkboxes somebody flicks one at a time, and round-tripping a
   * whole settings record per flick is how two panes open at once overwrite
   * one another.
   */
  async function setAsk(name: string, on: boolean) {
    try {
      await quick.setAsk(name, on)
    } catch (e) {
      notice = e instanceof Error ? e.message : String(e)
    }
  }

  async function save() {
    if (!draft) return
    clearNotice()
    saving = true
    try {
      // The addresses first, then the keys: a key is stored for the address
      // its card points at *as saved*, so saving it first would file it under
      // the old one.
      await models.save($state.snapshot(draft))
      for (const card of CARDS) {
        const key = keys[card.tier].trim()
        if (key && draft[card.tier]) await models.setKey(card.tier, key)
      }
      keys = { large: '', medium: '', small: '' }
      draft = structuredClone($state.snapshot(models.settings!))
      confirm('Saved.')
    } catch (e) {
      notice = e instanceof Error ? e.message : String(e)
    } finally {
      saving = false
    }
  }

  async function removeKey(tier: ModelTier) {
    clearNotice()
    try {
      await models.clearKey(tier)
    } catch (e) {
      notice = e instanceof Error ? e.message : String(e)
    }
  }
</script>

{#if !models.supported}
  <p class="hint">
    This vault's storage does not carry the assistant, so there is no model to configure. A SQLite
    or Postgres vault does.
  </p>
{:else if !draft && models.error}
  <p class="hint">Where the models are could not be read: {models.error}</p>
{:else if !draft}
  <p class="hint">Loading…</p>
{:else}
  <div class="panes">
    <p class="hint lead">
      Three models, one for each size of work, each with its own provider and model name. Anything
      that speaks the OpenAI chat API works — leave the address empty for OpenAI itself. What you
      ask about is sent to the provider of whichever model does the work; a model on this machine
      sends nothing anywhere. A reasoning effort left at the provider's default is the provider's
      choice, and can be no thinking at all — OpenAI's gpt-5.1 defaults to None.
    </p>

    {#each CARDS as card (card.tier)}
      {@const config = configOf(card.tier)}
      <section class="card" aria-labelledby="model-{card.tier}">
        <div class="head">
          <span class="eyebrow" id="model-{card.tier}">{card.title}</span>
          {#if card.fresh}
            <label class="toggle compact">
              <input
                type="checkbox"
                checked={config != null}
                onchange={(e) => toggle(card, e.currentTarget.checked)}
              />
              <span>{config ? 'On' : 'Off'}</span>
            </label>
          {/if}
        </div>
        <p class="hint">{card.uses}</p>

        {#if !config}
          <p class="hint">
            {#if card.tier === 'large'}
              Off: research runs on the Medium model.
            {:else}
              Off: none of this is done. It never falls back to a bigger model — these run as you
              type, and a bill at the assistant's rates for every keystroke is not one anybody
              agreed to.
            {/if}
          </p>
        {:else}
          {@const local = isLoopback(config.baseUrl)}
          {@const keyed = models.hasKey(config)}
          {@const shared = sharing(card.tier)}
          <div class="presets" role="group" aria-label="{card.title} provider">
            {#each KNOWN_LLM_ENDPOINTS as preset (preset.label)}
              <button
                class="chip"
                class:on={(config.baseUrl ?? '') === preset.url}
                aria-pressed={(config.baseUrl ?? '') === preset.url}
                onclick={() => (config.baseUrl = preset.url || null)}
              >
                {preset.label}
              </button>
            {/each}
          </div>

          <label class="setting">
            <span>Base URL</span>
            <input
              value={config.baseUrl ?? ''}
              oninput={(e) => (config.baseUrl = e.currentTarget.value.trim() || null)}
              placeholder="https://api.openai.com/v1"
              spellcheck="false"
            />
          </label>

          <label class="setting">
            <span>Model name</span>
            <input bind:value={config.model} placeholder={card.placeholder} spellcheck="false" />
          </label>

          <label class="setting">
            <span>Reasoning effort</span>
            <select
              value={config.reasoningEffort ?? ''}
              onchange={(e) =>
                (config.reasoningEffort = (e.currentTarget.value ||
                  null) as ReasoningEffort | null)}
            >
              <option value="">The provider's default</option>
              {#each card.efforts as effort (effort.value)}
                <option value={effort.value}>{effort.label}</option>
              {/each}
            </select>
          </label>
          <p class="hint">{card.effortHint}</p>

          {#if keyed}
            <div class="stored">
              <span>
                <Icon name="lock" size={14} />
                A key for {providerLabel(config.baseUrl)} is stored{shared.length
                  ? `, shared with ${shared.join(' and ')}`
                  : ''}.
              </span>
              {#if savedEndpoint(card.tier) === endpointOf(config.baseUrl)}
                <button class="link" onclick={() => void removeKey(card.tier)}>Remove</button>
              {/if}
            </div>
          {/if}
          <label class="setting">
            <span>{keyed ? 'Replace the key' : 'API key'}</span>
            <input
              type="password"
              bind:value={keys[card.tier]}
              placeholder={local ? 'Not needed for a model on this machine' : 'sk-…'}
              spellcheck="false"
              autocomplete="off"
            />
          </label>
          {#if !keyed && !local && shared.length}
            <p class="hint">One key serves {card.title} and {shared.join(' and ')}.</p>
          {/if}

          {#if card.tier === 'small'}
            <div class="switches">
              {#each Object.entries(grouped) as [app, asks] (app)}
                <div class="group">
                  <span class="group-name">{app}</span>
                  {#each asks as ask (ask.name)}
                    <label class="switch">
                      <input
                        type="checkbox"
                        checked={ask.on}
                        onchange={(e) => void setAsk(ask.name, e.currentTarget.checked)}
                      />
                      <span class="switch-text">
                        <span class="switch-label">{ask.label}</span>
                        <span class="switch-blurb">{ask.blurb}</span>
                      </span>
                    </label>
                  {/each}
                </div>
              {/each}
            </div>
            <p class="hint">
              A separate switch from the assistant, and deliberately: wanting your shelves filled in
              is not the same as wanting something to talk to. Each one says what it sends, because
              “AI features: on” is not a decision anybody can make; the ones that read a journal
              entry start switched off. None of them ever changes what you already typed.
            </p>
          {/if}
        {/if}
      </section>
    {/each}

    <p class="hint">
      Keys are encrypted with everything else in the vault, so they are unreadable while it is
      locked — and nothing can spend them while it is locked either. Moving a model to another
      address does not take its key along, and a key no model points at any more is forgotten when
      you save.
    </p>
  </div>

  <div class="save">
    {#if notice}<span class="notice">{notice}</span>{/if}
    <button class="btn btn-primary" onclick={() => void save()} disabled={saving}>
      {saving ? 'Saving…' : 'Save'}
    </button>
  </div>
{/if}

<style>
  /* Every box in here is `width: 100%` of a column that has `min-width: 0` --
     see `AgentPanel`'s note on why both halves are needed. */
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
  .card {
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
  }

  .toggle {
    display: flex;
    gap: var(--sp-2);
    align-items: center;
    min-width: 0;
    cursor: pointer;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .toggle input {
    flex: none;
    accent-color: var(--accent);
  }
  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }
  .hint.lead {
    color: var(--fg-muted);
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

  /* The label wrapped around a caption and an input -- not the global
     utility of the same job in `app.css`; see `AgentPanel` for what wearing
     both did. */
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
  select {
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
  input:focus,
  select:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }
  input[type='checkbox'] {
    width: auto;
    padding: 0;
  }

  .stored {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    min-width: 0;
  }
  .stored span {
    display: flex;
    flex: 1;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
    line-height: var(--leading-normal);
    color: var(--fg-muted);
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

  /* The quick-ask switches. Grouped by app, and each one carries the sentence
     that says what it sends -- which is the only thing that makes the
     checkbox a decision rather than a shrug. */
  .switches {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    margin-top: var(--sp-2);
  }
  .group {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .group-name {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .switch {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
    cursor: pointer;
  }
  .switch input {
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .switch-text {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .switch-label {
    font-size: var(--text-sm);
  }
  .switch-blurb {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
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
