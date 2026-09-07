<script lang="ts">
  import { app } from '../lib/state.svelte'
  import { api } from '../lib/api'
  import { humanBytes, plural } from '../lib/format'
  import Icon from './Icon.svelte'

  let open = $state(false)
  let changing = $state(false)
  let current = $state('')
  let next = $state('')
  let notice = $state<string | null>(null)

  const status = $derived(app.status)

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

  const LOCK_CHOICES = [
    { label: 'Never', value: 0 },
    { label: '1 min', value: 60 },
    { label: '5 min', value: 300 },
    { label: '15 min', value: 900 },
    { label: '1 hour', value: 3600 },
  ]
</script>

<div class="wrap">
  <button class="trigger" onclick={() => (open = !open)} aria-expanded={open} title="Settings">
    <Icon name="settings" size={15} />
    Settings
  </button>

  {#if open}
    <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
    <div class="scrim" onclick={() => (open = false)}></div>
    <div class="panel" role="dialog" aria-label="Settings">
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

  .trigger {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    height: 28px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .trigger:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .scrim {
    position: fixed;
    inset: 0;
    z-index: 40;
  }

  .panel {
    position: absolute;
    bottom: calc(100% + 6px);
    left: 0;
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
