<script lang="ts">
  import { app } from '../lib/state.svelte'
  import { api } from '../lib/api'
  import { tray } from '../lib/tray.svelte'
  import { agent } from '../lib/agent.svelte'
  import AgentSettings from './AgentSettings.svelte'
  import { humanBytes, plural } from '../lib/format'
  import { trapFocus } from '../lib/focus'
  import Icon from './Icon.svelte'

  let open = $state(false)
  let changing = $state(false)
  let current = $state('')
  let next = $state('')
  let notice = $state<string | null>(null)
  let showAgent = $state(false)

  const status = $derived(app.status)

  // So the button can say "set up" or "configure" rather than guessing. Cheap
  // and idempotent; the panel calls it too.
  $effect(() => {
    if (open && agent.supported && !agent.settings) void agent.load()
  })

  async function changePassword(e: Event) {
    e.preventDefault()
    notice = null
    try {
      await api.changePassword(current, next)
      notice = 'Password changed.'
      current = ''
      next = ''
      changing = false
    } catch (err) {
      notice = err instanceof Error ? err.message : String(err)
    }
  }

  async function setAutoLock(seconds: number) {
    await api.setAutoLock(seconds)
    app.status = await api.status()
  }

  // The same strip of the screen has three names. Calling it the wrong one
  // is how a setting becomes unfindable: nobody on macOS goes looking for a
  // "system tray".
  const TRAY_WORD = navigator.userAgent.includes('Mac')
    ? 'menu bar'
    : navigator.userAgent.includes('Windows')
      ? 'notification area'
      : 'system tray'

  const LOCK_CHOICES = [
    { label: 'Never', value: 0 },
    { label: '1 min', value: 60 },
    { label: '5 min', value: 300 },
    { label: '15 min', value: 900 },
    { label: '1 hour', value: 3600 },
  ]
</script>

<!-- Escape closes the panel, as it does every other thing in the
     application that a scrim has put the window behind. -->
<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape' && open && !showAgent) open = false
  }}
/>

{#if showAgent}
  <!-- Outside the dropdown on purpose: the dropdown closes when this opens,
       and a dialog inside a popover that has gone would go with it. -->
  <AgentSettings onclose={() => (showAgent = false)} />
{/if}

<div class="wrap">
  <button class="barbtn" onclick={() => (open = !open)} aria-expanded={open}>
    <span><Icon name="settings" size={19} weight={1.7} /></span>
    <span class="barlabel">Settings</span>
  </button>

  {#if open}
    <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
    <div class="scrim" onclick={() => (open = false)}></div>
    <div class="panel" role="dialog" aria-modal="true" aria-label="Settings" use:trapFocus>
      <div class="section">
        <span class="eyebrow">Appearance</span>
        <div class="segmented">
          {#each ['system', 'light', 'dark'] as t (t)}
            <button
              class="seg"
              class:on={app.theme === t}
              onclick={() => app.setTheme(t as 'system' | 'light' | 'dark')}
              >{t[0]!.toUpperCase() + t.slice(1)}</button
            >
          {/each}
        </div>
      </div>

      {#if tray.supported}
        <div class="section">
          <span class="eyebrow">Quick actions</span>
          <label class="toggle">
            <input
              type="checkbox"
              checked={tray.enabled}
              onchange={(e) => tray.setEnabled(e.currentTarget.checked)}
            />
            <span>Show Every Day in the {TRAY_WORD}</span>
          </label>
          <p class="aside">
            {#if tray.unavailable}
              This desktop session has no {TRAY_WORD} for Every Day to appear in.
            {:else}
              Start an entry, a task or an hour without coming back to the window.
            {/if}
          </p>
        </div>
      {/if}

      {#if agent.supported}
        <div class="section">
          <span class="eyebrow">Assistant</span>
          <button
            class="btn"
            onclick={() => {
              open = false
              showAgent = true
            }}
          >
            {agent.settings?.enabled ? 'Configure the assistant' : 'Set up the assistant'}
          </button>
        </div>
      {/if}

      {#if status?.encrypted}
        <div class="section">
          <span class="eyebrow">Lock after</span>
          <div class="segmented">
            {#each LOCK_CHOICES as c (c.value)}
              <button
                class="seg"
                class:on={status.autoLockSeconds === c.value}
                onclick={() => setAutoLock(c.value)}>{c.label}</button
              >
            {/each}
          </div>
        </div>

        <div class="section">
          {#if changing}
            <form onsubmit={changePassword}>
              <input
                class="field sm"
                type="password"
                placeholder="Current password"
                bind:value={current}
                autocomplete="current-password"
              />
              <div class="gap"></div>
              <input
                class="field sm"
                type="password"
                placeholder="New password"
                bind:value={next}
                autocomplete="new-password"
              />
              <div class="row">
                <button class="btn" type="button" onclick={() => (changing = false)}>Cancel</button>
                <button class="btn btn-primary" type="submit" disabled={next.length < 8}
                  >Change</button
                >
              </div>
            </form>
          {:else}
            <button class="link" onclick={() => (changing = true)}>Change password…</button>
          {/if}
        </div>
      {/if}

      {#if notice}<p class="notice">{notice}</p>{/if}

      <hr class="sep" />

      <div class="facts">
        <div><span>Vault</span><b>{status?.name}</b></div>
        <div><span>Storage</span><b>{status?.backend}</b></div>
        <div>
          <span>Encryption</span>
          <b class:warn={!status?.encrypted}>{status?.encrypted ? 'On' : 'Off'}</b>
        </div>
        {#if status?.stats}
          <div><span>Entries</span><b>{plural(status.stats.entries, 'entry', 'entries')}</b></div>
          <div><span>Media</span><b>{humanBytes(status.stats.blobBytes)}</b></div>
        {/if}
      </div>
      <p class="path" title={status?.path}>{status?.path}</p>
    </div>
  {/if}
</div>

<style>
  .wrap {
    position: relative;
  }

  .scrim {
    position: fixed;
    inset: 0;
    z-index: 40;
  }

  /* Out of the side of the bar rather than up its width: the panel is three
     times wider than the button that opens it, and a bar is too narrow to
     hang anything under. */
  .panel {
    position: absolute;
    bottom: 0;
    left: calc(100% + var(--sp-2));
    z-index: 41;
    width: 268px;
    padding: var(--sp-4);
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-lg);
  }

  .section + .section {
    margin-top: var(--sp-4);
  }
  .section .eyebrow {
    display: block;
    margin-bottom: var(--sp-2);
  }

  .segmented {
    display: flex;
    gap: 2px;
    padding: 2px;
    background: var(--bg-sunken);
    border-radius: var(--radius-sm);
  }
  .seg {
    flex: 1;
    height: 24px;
    border-radius: 4px;
    font-size: var(--text-xs);
    font-weight: 500;
    color: var(--fg-subtle);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .seg:hover {
    color: var(--fg);
  }
  .seg.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }

  .toggle {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg);
    cursor: pointer;
  }
  .toggle input {
    flex: none;
    accent-color: var(--accent);
  }

  .aside {
    margin-top: var(--sp-2);
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  .link {
    font-size: var(--text-sm);
    color: var(--accent);
  }
  .link:hover {
    text-decoration: underline;
  }

  .field.sm {
    height: 30px;
    font-size: var(--text-sm);
  }
  .gap {
    height: var(--sp-2);
  }
  .row {
    display: flex;
    gap: var(--sp-2);
    justify-content: flex-end;
    margin-top: var(--sp-3);
  }

  .notice {
    margin-top: var(--sp-3);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  .sep {
    margin: var(--sp-4) 0;
  }

  .facts {
    display: grid;
    gap: var(--sp-1);
    font-size: var(--text-sm);
  }
  .facts div {
    display: flex;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  .facts span {
    color: var(--fg-subtle);
  }
  .facts b {
    font-weight: 550;
  }
  .facts b.warn {
    color: var(--danger);
  }

  .path {
    margin-top: var(--sp-3);
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--fg-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    user-select: text;
  }
</style>
