<script lang="ts">
  // Settings, as a dialog with tabs.
  //
  // It was a popover hanging out of the side of the app bar, and it had
  // outgrown that shape twice over. A popover is right for two or three
  // switches; this holds an appearance control, a tray switch, the auto-lock
  // interval, a password form, a page of facts about the vault, and — the
  // one that broke it — the assistant, which has an instructions box
  // somebody is expected to write a paragraph into. A 268px column hanging
  // off a 82px bar could not hold that, so the assistant became a *second*
  // dialog raised out of the popover: two settings surfaces, one of which
  // had to close before the other could open.
  //
  // There is one now, and the assistant is a tab in it. The tabs are the
  // three questions people actually arrive with — how it looks, what the
  // assistant does, and what is true of this vault — rather than a
  // transcription of the storage layout.

  import { api } from '../lib/api'
  import { agent } from '../lib/agent.svelte'
  import { humanBytes, plural } from '../lib/format'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { panels, type SettingsTab } from '../lib/panels.svelte'
  import { app } from '../lib/state.svelte'
  import { tray } from '../lib/tray.svelte'
  import AgentPanel from './AgentPanel.svelte'
  import DataPanel from './DataPanel.svelte'
  import McpPanel from './McpPanel.svelte'
  import ProfilePanel from './ProfilePanel.svelte'
  import SharePanel from './SharePanel.svelte'
  import Icon from './Icon.svelte'
  import type { IconName } from '../lib/icons'
  import type { HotkeyStatus } from '../lib/types'

  let changing = $state(false)
  let current = $state('')
  let next = $state('')
  let notice = $state<string | null>(null)

  /**
   * The OS-wide key that raises the palette.
   *
   * `null` until it has been asked for, and after a failure: a build with no
   * shell to ask has no switch to draw. `registered` false with nothing else
   * to say means the desktop refused it, which is the ordinary answer on a
   * Wayland session with no portal.
   */
  let hotkey = $state<HotkeyStatus | null>(null)
  let hotkeyNotice = $state<string | null>(null)
  /** Has claiming it ever worked? A refusal leaves the switch stuck off. */
  let hotkeyAvailable = $state(true)

  void api
    .hotkeyStatus()
    .then((s) => (hotkey = s))
    .catch(() => (hotkey = null))

  async function setHotkey(on: boolean) {
    hotkeyNotice = null
    try {
      hotkey = await api.setHotkey(on)
      if (on && !hotkey.registered) {
        hotkeyAvailable = false
        hotkeyNotice =
          'This desktop did not grant that key. On Wayland it needs a portal the ' +
          'compositor may not provide; the tray is the way in instead.'
      }
    } catch (e) {
      hotkeyNotice = e instanceof Error ? e.message : String(e)
    }
  }

  const status = $derived(app.status)
  const tab = $derived(panels.settings ?? 'general')

  const TABS: { id: SettingsTab; label: string; icon: IconName }[] = [
    { id: 'general', label: 'General', icon: 'settings' },
    { id: 'profile', label: 'You', icon: 'star' },
    { id: 'assistant', label: 'Assistant', icon: 'sparkle' },
    { id: 'data', label: 'Data', icon: 'upload' },
    { id: 'vault', label: 'Vault', icon: 'lock' },
  ]
  // The assistant tab is not offered on a backend that cannot store a
  // conversation, for the same reason the rail is not: an empty tab that
  // explains why it is empty is worse than no tab.
  const shown = $derived(TABS.filter((t) => t.id !== 'assistant' || agent.supported))

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

  async function setForgetKey(seconds: number) {
    await api.setForgetKey(seconds)
    app.status = await api.status()
  }

  let opensItself = $state(app.opensItself)
  let askingForKey = $state(false)
  let keyPassword = $state('')

  /**
   * Turning it on asks for the password; turning it off does not.
   *
   * The asymmetry is the point. This is the one switch whose whole effect is
   * that the password stops being needed, so switching it *on* should cost the
   * password once, from somebody who knows it. Switching it off only ever
   * makes things stricter, and should not be gated behind a thing somebody may
   * have turned this on precisely because they cannot remember.
   */
  async function setOpensItself(on: boolean) {
    notice = null
    if (on) {
      askingForKey = true
      return
    }
    try {
      opensItself = await api.setOpensItself(false, null)
      app.opensItself = opensItself
      notice = 'It will ask for the password again.'
    } catch (err) {
      notice = err instanceof Error ? err.message : String(err)
    }
  }

  async function confirmOpensItself(e: Event) {
    e.preventDefault()
    try {
      opensItself = await api.setOpensItself(true, keyPassword)
      app.opensItself = opensItself
      notice = "The key is in this computer's keychain."
    } catch (err) {
      notice = err instanceof Error ? err.message : String(err)
    } finally {
      keyPassword = ''
      askingForKey = false
    }
  }

  function cancelOpensItself() {
    keyPassword = ''
    askingForKey = false
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

  // Longer than the screen's, and starting at never, because these are the
  // hours a vault is asleep rather than the minutes a window is idle.
  const KEY_CHOICES = [
    { label: 'Never', value: 0 },
    { label: '1 hour', value: 3600 },
    { label: '8 hours', value: 28_800 },
    { label: '24 hours', value: 86_400 },
  ]
</script>

<!-- Escape closes it, as it does every other dialog in the application. A
     modal that only the mouse can dismiss is one people learn to distrust. -->
<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape') panels.closeSettings()
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={() => panels.closeSettings()}></div>
<div class="dialog" role="dialog" aria-modal="true" aria-label="Settings" use:trapFocus>
  <header>
    <h2>Settings</h2>
    <button class="ghost" onclick={() => panels.closeSettings()} title="Close">
      <Icon name="close" size={16} />
    </button>
  </header>

  <div class="split">
    <nav class="tabs" aria-label="Settings sections">
      {#each shown as t (t.id)}
        <button
          class="tab"
          class:on={tab === t.id}
          aria-current={tab === t.id ? 'page' : undefined}
          onclick={() => panels.openSettings(t.id)}
        >
          <Icon name={t.icon} size={16} />
          {t.label}
        </button>
      {/each}
    </nav>

    <div class="body scroll">
      {#if tab === 'assistant'}
        <AgentPanel />
      {:else if tab === 'profile'}
        <ProfilePanel />
      {:else if tab === 'data'}
        <DataPanel />
      {:else if tab === 'general'}
        <section>
          <span class="eyebrow">Appearance</span>
          <div class="segmented">
            {#each ['system', 'light', 'dark'] as t (t)}
              <button
                class="seg"
                class:on={app.theme === t}
                onclick={() => app.setTheme(t as 'system' | 'light' | 'dark')}
              >
                {t[0]!.toUpperCase() + t.slice(1)}
              </button>
            {/each}
          </div>
        </section>

        {#if tray.supported}
          <section>
            <span class="eyebrow">Quick actions</span>
            <label class="toggle">
              <input
                type="checkbox"
                checked={tray.enabled}
                onchange={(e) => tray.setEnabled(e.currentTarget.checked)}
              />
              <span>
                <b>Show Every Day in the {TRAY_WORD}</b>
                <small>
                  {#if tray.unavailable}
                    This desktop session has no {TRAY_WORD} for Every Day to appear in.
                  {:else}
                    Start an entry, a task or an hour without coming back to the window.
                  {/if}
                </small>
              </span>
            </label>
          </section>
        {/if}

        <section>
          <span class="eyebrow">Keyboard</span>
          <p class="hint">
            Two keys for anything you do often: <kbd>G</kbd> then <kbd>J</kbd> for the journal,
            <kbd>C</kbd> to start the next thing, <kbd>/</kbd> to search.
          </p>

          {#if hotkey}
            <label class="toggle">
              <input
                type="checkbox"
                checked={hotkey.registered}
                disabled={!hotkey.registered && !hotkeyAvailable}
                onchange={(e) => setHotkey(e.currentTarget.checked)}
              />
              <span>
                <b>Reach Every Day from anywhere with {hotkey.shortcut}</b>
                <small>
                  {#if hotkeyNotice}
                    {hotkeyNotice}
                  {:else}
                    Raises the window with the command palette open, whatever you are looking at.
                  {/if}
                </small>
              </span>
            </label>
          {/if}
          <div>
            <button
              class="btn"
              onclick={() => {
                panels.closeSettings()
                panels.shortcuts = true
              }}
            >
              Show every shortcut
            </button>
          </div>
        </section>
      {:else}
        {#if status?.encrypted}
          <section>
            <span class="eyebrow">Lock after</span>
            <div class="segmented">
              {#each LOCK_CHOICES as c (c.value)}
                <button
                  class="seg"
                  class:on={status.autoLockSeconds === c.value}
                  onclick={() => setAutoLock(c.value)}
                >
                  {c.label}
                </button>
              {/each}
            </div>
            <p class="hint">
              How long this window may sit untouched before it hides what it is showing and asks for
              the password again. The vault stays open behind it.
            </p>
          </section>

          <section>
            <span class="eyebrow">Forget the key after</span>
            <div class="segmented">
              {#each KEY_CHOICES as c (c.value)}
                <button
                  class="seg"
                  class:on={status.forgetKeySeconds === c.value}
                  onclick={() => setForgetKey(c.value)}
                >
                  {c.label}
                </button>
              {/each}
            </div>
            <p class="hint">
              The heavier of the two. This computer holds the key while the vault is open, and
              serves it to your other windows, to any device you have paired, and to the assistant's
              own routines. Forgetting it stops all of them until somebody types the password again.
              Quitting always forgets it.
            </p>
          </section>

          <section>
            <span class="eyebrow">Opening this vault</span>
            <!-- The box is driven by `opensItself` and nothing else, and the
                 handler puts it straight back: turning it on only opens the
                 password step, so a cancelled or refused attempt must not
                 leave a switch that says the key is kept when it is not. -->
            <label class="toggle">
              <input
                type="checkbox"
                checked={opensItself}
                onchange={(e) => {
                  e.currentTarget.checked = opensItself
                  void setOpensItself(!opensItself)
                }}
              />
              <span>
                <b>Open without a password when the app starts</b>
                <small>
                  This computer keeps the key in its own keychain. The vault is then as safe as your
                  login here, rather than as safe as its password — anybody already sitting at this
                  desk, logged in as you, can read it.
                </small>
              </span>
            </label>
            <p class="hint">
              Worth it for one thing: the assistant's routines run where the vault is, and a vault
              that is shut from the moment this machine boots until somebody types a password is a
              vault whose morning brief does not happen.
            </p>
            {#if askingForKey}
              <form onsubmit={confirmOpensItself}>
                <input
                  class="field"
                  type="password"
                  placeholder="Your vault password"
                  bind:value={keyPassword}
                  autocomplete="current-password"
                  use:focusOnMount
                />
                <div class="row">
                  <button class="btn" type="button" onclick={cancelOpensItself}>Cancel</button>
                  <button class="btn btn-primary" type="submit" disabled={!keyPassword}>
                    Keep the key
                  </button>
                </div>
              </form>
            {/if}
          </section>

          <section>
            <span class="eyebrow">Password</span>
            {#if changing}
              <form onsubmit={changePassword}>
                <input
                  class="field"
                  type="password"
                  placeholder="Current password"
                  bind:value={current}
                  autocomplete="current-password"
                />
                <div class="gap"></div>
                <input
                  class="field"
                  type="password"
                  placeholder="New password"
                  bind:value={next}
                  autocomplete="new-password"
                />
                <div class="row">
                  <button class="btn" type="button" onclick={() => (changing = false)}>
                    Cancel
                  </button>
                  <button class="btn btn-primary" type="submit" disabled={next.length < 8}>
                    Change
                  </button>
                </div>
              </form>
            {:else}
              <div>
                <button class="btn" onclick={() => (changing = true)}>Change password…</button>
              </div>
            {/if}
          </section>
        {/if}

        <SharePanel />

        <McpPanel />

        {#if notice}<p class="notice">{notice}</p>{/if}

        <section>
          <span class="eyebrow">This vault</span>
          <div class="facts">
            <div><span>Name</span><b>{status?.name}</b></div>
            <div><span>Storage</span><b>{status?.backend}</b></div>
            <div>
              <span>Encryption</span>
              <b class:warn={!status?.encrypted}>{status?.encrypted ? 'On' : 'Off'}</b>
            </div>
            {#if status?.stats}
              <div>
                <span>Entries</span><b>{plural(status.stats.entries, 'entry', 'entries')}</b>
              </div>
              <div><span>Media</span><b>{humanBytes(status.stats.blobBytes)}</b></div>
            {/if}
          </div>
          <p class="path" title={status?.path}>{status?.path}</p>
        </section>
      {/if}
    </div>
  </div>
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
    width: min(720px, calc(100vw - var(--sp-8)));
    height: min(620px, calc(100vh - var(--sp-8)));
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-raised);
    box-shadow: var(--shadow-lg);
    overflow: hidden;
  }

  header {
    display: flex;
    align-items: center;
    flex: none;
    padding: var(--sp-3) var(--sp-3) var(--sp-3) var(--sp-5);
    border-bottom: 1px solid var(--border);
  }
  h2 {
    flex: 1;
    margin: 0;
    font-size: var(--text-md);
    font-weight: 650;
  }
  .ghost {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: var(--radius-sm);
    color: var(--fg-subtle);
  }
  .ghost:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  /* A rail rather than a strip of tabs across the top: the labels are words
     rather than icons, there is room for a fourth, and it puts the content
     in a column narrow enough that a line of prose in it is readable. */
  .split {
    display: flex;
    flex: 1;
    min-height: 0;
  }
  .tabs {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: none;
    width: 176px;
    padding: var(--sp-3);
    border-right: 1px solid var(--border);
    background: var(--bg-panel);
  }
  .tab {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    height: var(--row-h);
    padding: 0 var(--sp-3);
    border-radius: var(--radius-sm);
    font-size: var(--text-base);
    color: var(--fg-muted);
    text-align: left;
  }
  .tab:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .tab.on {
    background: var(--bg-active);
    color: var(--fg);
    font-weight: 600;
  }

  .body {
    flex: 1;
    min-width: 0;
    padding: var(--sp-5) var(--sp-6) var(--sp-6);
  }

  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  section + section {
    margin-top: var(--sp-6);
  }

  .segmented {
    display: flex;
    gap: 3px;
    padding: 3px;
    background: var(--bg-sunken);
    border-radius: var(--radius);
  }
  .seg {
    flex: 1;
    height: 30px;
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    font-weight: 550;
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
    align-items: flex-start;
    gap: var(--sp-3);
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
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }

  kbd {
    font-family: var(--font-ui);
    font-size: var(--text-xs);
    font-weight: 600;
    padding: 1px 6px;
    border: 1px solid var(--border);
    border-bottom-width: 2px;
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    color: var(--fg-muted);
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
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  .facts {
    display: grid;
    gap: var(--sp-2);
    font-size: var(--text-base);
  }
  .facts div {
    display: flex;
    justify-content: space-between;
    gap: var(--sp-4);
  }
  .facts span {
    color: var(--fg-subtle);
  }
  .facts b {
    font-weight: 600;
    text-align: right;
    overflow-wrap: anywhere;
  }
  .facts b.warn {
    color: var(--danger);
  }

  .path {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    color: var(--fg-faint);
    overflow-wrap: anywhere;
    user-select: text;
  }
</style>
