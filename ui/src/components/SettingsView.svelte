<script lang="ts">
  // Settings, as a page with tabs.
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
  // There was one dialog after that, and the assistant was a tab in it —
  // until it outgrew the dialog the same way it had outgrown the popover.
  // Eleven tabs do not fit any better in 780×660 than six once fit in 268px:
  // a list of accounts, a vault's worth of facts, the assistant's four
  // standing panes (Runs, Routines, Memory, Proposals) each wanting room for
  // a real list rather than three rows scrolling inside a box the shape of a
  // phone held sideways. So this is a page now, laid out the way every other
  // app in the window is — `AppBar` stays where it always is, the tab list
  // below is this page's left column the way `TodoNav` or `CalendarNav` is
  // the todo or calendar app's, and the chosen tab's content fills the rest.
  //
  // The public surface — `panels.openSettings(tab)` / `closeSettings()` /
  // `panels.settings` — did not have to change for that, and callers never
  // noticed: the app bar's Settings button, the `Ctrl+,` shortcut, the
  // palette and the assistant's "not set up yet" screen all still just ask
  // for a tab. What changed is where `App.svelte` draws the result: in the
  // panes, in place of whichever app was open, rather than over the top of
  // it on a scrim. `app.section` is left untouched while this is open, so
  // leaving Settings — the close button, Escape outside a field, or picking
  // a different app from the bar — returns to exactly what was there before.
  //
  // It is also, for the same reason, no longer modal. `panels.modal` does
  // not count it, `shortcuts.svelte.ts`'s `dialogOpen` does not either, and
  // there is no focus trap and no `role="dialog"` below — the app-switching
  // keys, the palette and the rest of the window's shortcuts work over this
  // exactly as they do over Notes or Mail. See that file's own `inApp` and
  // `dialogOpen` for how a stray letter is kept from reaching the app this
  // page is currently standing in front of, and the `Escape` row in its
  // table for how this closes: not a local `<svelte:window>` handler here
  // any more, because that was the one way `Escape` in this file ever
  // differed from `Escape` in every other app's.

  import { api } from '../lib/api'
  import { agent } from '../lib/agent.svelte'
  import { assistant, PANE_LABELS, type Pane } from '../lib/assistant.svelte'
  import { proposals } from '../lib/proposals.svelte'
  import { humanBytes, plural } from '../lib/format'
  import { focusOnMount } from '../lib/focus'
  import { keysLabel } from '../lib/keys'
  import { panels, type SettingsTab } from '../lib/panels.svelte'
  import { app } from '../lib/state.svelte'
  import { meetings } from '../lib/meetings.svelte'
  import { tray } from '../lib/tray.svelte'
  import { badge } from '../lib/badge.svelte'
  import AccountsPanel from './AccountsPanel.svelte'
  import AgentPanel from './AgentPanel.svelte'
  import AssistantPanes from './AssistantPanes.svelte'
  import DataPanel from './DataPanel.svelte'
  import MeetingsPanel from './MeetingsPanel.svelte'
  import McpPanel from './McpPanel.svelte'
  import ProfilePanel from './ProfilePanel.svelte'
  import SharePanel from './SharePanel.svelte'
  import SkillsPanel from './SkillsPanel.svelte'
  import Icon from './Icon.svelte'
  import type { IconName } from '../lib/icons'
  import type { BadgeSources, HotkeyStatus } from '../lib/types'

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

  /** `sub` marks a tab drawn indented under the one before it. */
  const TABS: { id: SettingsTab; label: string; icon: IconName; sub?: true }[] = [
    { id: 'general', label: 'General', icon: 'settings' },
    // "About You" rather than "You": the tab now holds the roles as well as
    // the six fields, and "You" beside a list of the parts of a life read as
    // a label for one of them.
    { id: 'profile', label: 'About You', icon: 'star' },
    { id: 'accounts', label: 'Accounts', icon: 'inbox' },
    { id: 'assistant', label: 'Assistant', icon: 'sparkle' },
    // The assistant's standing work, which was the Assistant app's four
    // panes until that app became the conversation. Under the Assistant tab
    // rather than beside it, because they are its, and a flat list of eleven
    // tabs would hide that. See `AssistantPanes.svelte`.
    { id: 'routines', label: PANE_LABELS.routines, icon: 'clock', sub: true },
    { id: 'runs', label: PANE_LABELS.runs, icon: 'inbox', sub: true },
    { id: 'memory', label: 'Memory', icon: 'sparkle', sub: true },
    // A skill is managed from here, not drawn as a pane of the Assistant
    // app -- see `panels.svelte.ts`'s own note on why `SettingsTab` carries
    // it apart from `Pane`.
    { id: 'skills', label: 'Skills', icon: 'book', sub: true },
    { id: 'proposals', label: PANE_LABELS.proposals, icon: 'tick', sub: true },
    { id: 'meetings', label: 'Meetings', icon: 'mic' },
    { id: 'data', label: 'Data', icon: 'upload' },
    { id: 'vault', label: 'Vault', icon: 'lock' },
  ]

  const PANES: readonly SettingsTab[] = ['routines', 'runs', 'memory', 'proposals']
  function isPane(t: SettingsTab): t is Pane {
    return PANES.includes(t)
  }

  /** The count a tab carries -- the same two the app bar adds together. */
  function count(t: SettingsTab): number {
    if (t === 'runs') return assistant.unseen
    if (t === 'proposals') return proposals.unseen
    return 0
  }

  // The assistant tabs are not offered on a backend that cannot store a
  // conversation, for the same reason the rail is not: an empty tab that
  // explains why it is empty is worse than no tab. The panes under it follow
  // what the vault can hold, as they did in the app. Meetings rides the
  // notes capability -- see `meetings.svelte.ts`'s own `supported`.
  const shown = $derived(
    TABS.filter(
      (t) => (t.id !== 'assistant' && t.id !== 'skills' && !isPane(t.id)) || agent.supported,
    )
      .filter((t) => (t.id !== 'routines' && t.id !== 'runs') || app.supportsRoutines)
      .filter((t) => t.id !== 'proposals' || app.supportsProposals)
      .filter((t) => t.id !== 'meetings' || meetings.supported),
  )

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

  // And the place the application's icon lives has three more. The badge is
  // drawn on that icon, so the setting has to say which one.
  const DOCK_WORD = navigator.userAgent.includes('Mac')
    ? 'Dock'
    : navigator.userAgent.includes('Windows')
      ? 'taskbar'
      : 'dock'

  const BADGE_TASKS: { label: string; value: BadgeSources['tasks'] }[] = [
    { label: 'No tasks', value: 'off' },
    { label: 'Due today', value: 'due' },
    { label: 'All open', value: 'open' },
  ]

  // What the hints below point people at once the idle locks are off: the
  // same binding `shortcuts.svelte.ts` has `mod+l` doing already, spelled
  // out from the one place that knows the platform's own glyph for it,
  // rather than a second "Ctrl+L" written out by hand that could drift from
  // the real shortcut.
  const LOCK_SHORTCUT = keysLabel('mod+l', navigator.userAgent.includes('Mac'))

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

<!-- This page's left column, the way `TodoNav` or `CalendarNav` is the todo
     or calendar app's -- see `Sidebar.svelte`, which draws exactly this shape
     around those. Settings has no app of its own to be drawn inside, so it
     draws its own aside rather than being handed one. -->
<aside class="nav">
  <div class="head">
    <h2>Settings</h2>
    <button class="ghost" onclick={() => panels.closeSettings()} title="Close settings">
      <Icon name="close" size={16} />
    </button>
  </div>
  <nav class="scroll tabs" aria-label="Settings sections">
    {#each shown as t (t.id)}
      <button
        class="tab"
        class:on={tab === t.id}
        class:sub={t.sub}
        aria-current={tab === t.id ? 'page' : undefined}
        onclick={() => panels.openSettings(t.id)}
      >
        <Icon name={t.icon} size={t.sub ? 14 : 16} />
        <span class="label">{t.label}</span>
        {#if count(t.id) > 0}<span class="count">{count(t.id)}</span>{/if}
      </button>
    {/each}
  </nav>
</aside>

<!-- A landmark rather than `role="dialog"`: this is a page in the panes, not
     a sheet raised over them, and nothing here traps focus or needs Escape
     caught locally -- see the header comment, and the `Escape` row in
     `shortcuts.svelte.ts`'s table for how this closes instead. -->
<main class="pane scroll" aria-label="Settings">
  <div class="content">
    {#if tab === 'assistant'}
      <AgentPanel />
    {:else if isPane(tab)}
      <AssistantPanes pane={tab} />
    {:else if tab === 'skills'}
      <SkillsPanel />
    {:else if tab === 'meetings'}
      <MeetingsPanel />
    {:else if tab === 'profile'}
      <ProfilePanel />
    {:else if tab === 'accounts'}
      <AccountsPanel />
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
          <!-- Only meaningful once the switch above is on -- greyed out
                 rather than hidden, the same way the lock choices above are
                 suspended while the keychain switch is on, so the choice
                 underneath is still visible for when it is turned back on. -->
          <label class="toggle" class:disabled={!tray.enabled}>
            <input
              type="checkbox"
              checked={tray.meetingEnabled}
              disabled={!tray.enabled}
              onchange={(e) => tray.setMeetingEnabled(e.currentTarget.checked)}
            />
            <span>
              <b>Show the next meeting in the {TRAY_WORD}</b>
              <small>
                {#if !tray.enabled}
                  Only shown while Every Day is in the {TRAY_WORD}.
                {:else}
                  The meeting you are in, or the next one starting soon today. Nothing is shown
                  while the vault is locked.
                {/if}
              </small>
            </span>
          </label>
        </section>
      {/if}

      {#if badge.supported}
        <section>
          <span class="eyebrow">Icon badge</span>
          <p class="hint">
            A number on Every Day's icon in the {DOCK_WORD}, adding up whatever you choose here.
            Nothing is shown while the vault is locked.
          </p>
          <div class="segmented">
            {#each BADGE_TASKS as c (c.value)}
              <button
                class="seg"
                class:on={badge.sources.tasks === c.value}
                aria-pressed={badge.sources.tasks === c.value}
                onclick={() => badge.set({ tasks: c.value })}
              >
                {c.label}
              </button>
            {/each}
          </div>
          {#if badge.sources.tasks === 'due'}
            <p class="hint">Open tasks due today or overdue, the number beside Today in Todo.</p>
          {:else if badge.sources.tasks === 'open'}
            <p class="hint">Every task not yet done, the number beside All tasks in Todo.</p>
          {/if}
          <label class="toggle">
            <input
              type="checkbox"
              checked={badge.sources.mail}
              onchange={(e) => badge.set({ mail: e.currentTarget.checked })}
            />
            <span>
              <b>Unread mail</b>
              <small>Unread messages in every account's inbox, leaving out snoozed ones.</small>
            </span>
          </label>
          <label class="toggle">
            <input
              type="checkbox"
              checked={badge.sources.assistant}
              onchange={(e) => badge.set({ assistant: e.currentTarget.checked })}
            />
            <span>
              <b>Waiting in the assistant</b>
              <small>
                Runs and proposals you have not looked at yet, the number on the assistant's button.
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
            class="btn btn-outline"
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
          <!-- Suspended rather than hidden while the key is in the
                 keychain: the choice underneath is still there, waiting, for
                 the day the switch below goes off again. -->
          <div class="segmented" class:disabled={opensItself}>
            {#each LOCK_CHOICES as c (c.value)}
              <button
                class="seg"
                class:on={status.autoLockSeconds === c.value}
                disabled={opensItself}
                onclick={() => setAutoLock(c.value)}
              >
                {c.label}
              </button>
            {/each}
          </div>
          <p class="hint">
            {#if opensItself}
              Off while the key is in this computer's keychain — signing in to this computer is what
              unlocks it. Lock it yourself with {LOCK_SHORTCUT}.
            {:else}
              How long this window may sit untouched before it hides what it is showing and asks for
              the password again. The vault stays open behind it.
            {/if}
          </p>
        </section>

        <section>
          <span class="eyebrow">Forget the key after</span>
          <div class="segmented" class:disabled={opensItself}>
            {#each KEY_CHOICES as c (c.value)}
              <button
                class="seg"
                class:on={status.forgetKeySeconds === c.value}
                disabled={opensItself}
                onclick={() => setForgetKey(c.value)}
              >
                {c.label}
              </button>
            {/each}
          </div>
          <p class="hint">
            {#if opensItself}
              Off while the key is in this computer's keychain — signing in to this computer is what
              unlocks it. Lock it yourself with {LOCK_SHORTCUT}.
            {:else}
              The heavier of the two. This computer holds the key while the vault is open, and
              serves it to your other windows, to any device you have paired, and to the assistant's
              own routines. Forgetting it stops all of them until somebody types the password again.
              Quitting always forgets it.
            {/if}
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
                desk, logged in as you, can read it. Signing in here is what unlocks it from then
                on, so it also stops both idle locks above from firing; lock it yourself with
                {LOCK_SHORTCUT} when you mean to.
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
              <button class="btn btn-outline" onclick={() => (changing = true)}>
                Change password…
              </button>
            </div>
          {/if}
        </section>
      {/if}

      <SharePanel />

      <McpPanel />

      {#if notice}<p class="notice">{notice}</p>{/if}

      <!-- `vault-facts`: this section's own `margin-top`, not the generic
           `section + section` rule below -- that rule only reaches a literal
           sibling `<section>` written in this file, and the element directly
           above this one is drawn by `SharePanel` or `McpPanel` (or is
           absent, on a build with neither), never that. -->
      <section class="vault-facts">
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
</main>

<style>
  /* The same width, background and border every app's sidebar draws with --
     see `Sidebar.svelte`'s own `.sidebar`. Settings is not handed that
     component (it has no journal list, no "All entries" row, none of what
     makes a sidebar a sidebar everywhere else), so this is a second place
     that says so; the two have to be kept in step by eye. */
  .nav {
    width: var(--sidebar-w);
    flex: none;
    display: flex;
    flex-direction: column;
    background: var(--bg-sunken);
    border-right: 1px solid var(--border);
  }
  /* The brand row every sidebar has is `Sidebar.svelte`'s alone to draw --
     Settings is not inside one -- so this is this page's own equivalent of
     it: same height, so the window's top edge still lines up app to app,
     with a title and a close button standing in for the logo. The same
     macOS traffic-light allowance `Sidebar.svelte`'s `.brand` carries,
     for the same reason: this sits in exactly the spot that row would. */
  .head {
    display: flex;
    align-items: center;
    flex: none;
    height: var(--header-h);
    padding: 0 var(--sp-3) 0 var(--sp-4);
    padding-left: max(var(--sp-4), env(titlebar-area-x, var(--sp-4)));
    border-bottom: 1px solid var(--border);
  }
  .head h2 {
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
     rather than icons, there is room for a fourth now under Assistant, and
     it is the shape every other app's own nav column already draws in. */
  .tabs {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: var(--sp-3);
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
  .tab.sub {
    gap: var(--sp-2);
    height: calc(var(--row-h) - 4px);
    padding-left: calc(var(--sp-3) + 10px);
    font-size: var(--text-sm);
  }
  .label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .count {
    flex: none;
    min-width: 18px;
    padding: 1px 5px;
    border-radius: 999px;
    background: var(--accent);
    color: #fff;
    font-size: 10px;
    font-weight: 700;
    text-align: center;
  }

  /* The page's own flex item, next to `.nav` the way every other app's main
     view sits next to its sidebar. Padding lives here rather than on
     `.content`, so the same top padding applies above every tab regardless
     of its own height; the bottom figure is `Editor.svelte`'s own `.page`,
     so a long Accounts list does not end up with its last row behind the
     floating assistant button. */
  .pane {
    flex: 1;
    min-width: 0;
    padding: var(--sp-8) var(--sp-8) var(--fab-size);
  }

  /* One column, every tab: the same `--measure` the journal's own editor is
     wrapped to, centred the same way. This used to opt a handful of
     list-heavy tabs out to the pane's full width -- a row of email accounts
     reads fine stretched, the thinking went -- but the width and the left
     edge then changed walking from tab to tab, which read as broken rather
     than considered. A row of accounts or of routines is no harder to read
     at this width than a row in any other list in this application, and a
     settings page people visit rarely is not the place to ask them to
     relearn where the edge is. */
  .content {
    width: 100%;
    max-width: calc(var(--measure) + var(--sp-8) * 2);
    margin: 0 auto;
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
  /* See the comment on the markup above: this section's predecessor in the
     Vault tab is drawn by a different component, so it cannot rely on
     `section + section` the way the rest of this file's sections do. */
  .vault-facts {
    margin-top: var(--sp-6);
  }

  .segmented {
    display: flex;
    gap: 3px;
    padding: 3px;
    background: var(--bg-sunken);
    border-radius: var(--radius);
  }
  /* Suspended, not hidden: the choice underneath is still there for the day
     the keychain switch goes off again. */
  .segmented.disabled {
    opacity: 0.5;
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
  .seg:disabled {
    cursor: default;
  }
  .seg:disabled:hover {
    color: var(--fg-subtle);
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
  /* Suspended, the same `.segmented.disabled` reads above: the choice
     underneath stays visible rather than vanishing while it cannot be used. */
  .toggle.disabled {
    opacity: 0.5;
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
    margin-top: var(--sp-6);
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
