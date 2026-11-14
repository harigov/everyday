<script lang="ts">
  // Settings → Screen time, in two halves that live in different places.
  //
  // "This computer" is the machine this window runs on: whether it records,
  // what it is called in reports, how it sees the window in front, the GNOME
  // extension where that is how, and the browsers sending it websites. Each
  // machine answers for itself, so two computers on one vault each show
  // their own here.
  //
  // "For this vault" is the same on every machine: when to stop counting for
  // want of input, whether window titles are kept, how long the detail is
  // kept, and deleting what was recorded.
  //
  // A browser's connection code is shown once, in this component's own
  // state, and gone when the tab closes: it carries a token that can add
  // screen time to this vault, and there is no asking for it again -- only
  // connecting the browser afresh.

  import { onDestroy } from 'svelte'
  import { screentime } from '../lib/screentime.svelte'
  import { relativeTime } from '../lib/format'
  import { startOfDay, todayIso } from '../lib/time'
  import type { BrowserConnection, ScreenTimeSettings } from '../lib/types'
  import Icon from './Icon.svelte'

  void screentime.refreshStatus()
  // The vault's half comes with the app's first load; opened straight from
  // Settings, before the app ever was, it is loaded here.
  if (!screentime.loaded) void screentime.start()

  const status = $derived(screentime.status)
  const settings = $derived(screentime.settings)

  const BACKENDS: Record<string, string> = {
    gnome: 'GNOME Shell, through a small extension that runs inside the desktop',
    kde: 'KDE Plasma, through a KWin script',
    wlroots: 'the Wayland compositor’s list of windows (Sway, Hyprland, river and others)',
    x11: 'the X11 window manager’s active window',
    windows: 'Windows’ foreground window',
    macos: 'macOS’s frontmost app',
    unsupported: 'nothing yet: this desktop is not one Karma Yoga can watch',
  }

  const BROWSERS: { id: string; label: string }[] = [
    { id: 'chrome', label: 'Chrome' },
    { id: 'firefox', label: 'Firefox' },
    { id: 'edge', label: 'Edge' },
    { id: 'brave', label: 'Brave' },
    { id: 'other', label: 'Other' },
  ]

  const KEEP: { days: number; label: string }[] = [
    { days: 0, label: 'Forever' },
    { days: 365, label: '1 year' },
    { days: 183, label: '6 months' },
    { days: 90, label: '90 days' },
    { days: 30, label: '30 days' },
  ]

  type Span = 'hour' | 'today' | 'all'
  const SPANS: { id: Span; label: string; words: string }[] = [
    { id: 'hour', label: 'The last hour', words: 'the last hour' },
    { id: 'today', label: 'Today', words: 'today' },
    { id: 'all', label: 'Everything', words: 'everything' },
  ]

  // ── this computer ─────────────────────────────────────────────────

  let installing = $state(false)
  async function install() {
    installing = true
    try {
      await screentime.installGnomeExtension()
    } finally {
      installing = false
    }
  }

  function renameDevice(input: HTMLInputElement) {
    const next = input.value.trim()
    if (!status) return
    if (next === '' || next === status.device) {
      input.value = status.device
      return
    }
    void screentime.setDevice(next)
  }

  let browser = $state('chrome')
  let connecting = $state(false)
  let connection = $state<BrowserConnection | null>(null)
  let copied = $state(false)
  let copiedTimer: ReturnType<typeof setTimeout> | null = null

  async function connect() {
    connecting = true
    try {
      connection = await screentime.connectBrowser(browser)
      copied = false
    } finally {
      connecting = false
    }
  }

  async function copyCode() {
    if (!connection) return
    await navigator.clipboard.writeText(connection.code)
    copied = true
    if (copiedTimer) clearTimeout(copiedTimer)
    copiedTimer = setTimeout(() => (copied = false), 2000)
  }

  onDestroy(() => {
    if (copiedTimer) clearTimeout(copiedTimer)
    connection = null
  })

  // ── for this vault ────────────────────────────────────────────────

  function save(patch: Partial<ScreenTimeSettings>) {
    if (!settings) return
    void screentime.saveSettings({ ...$state.snapshot(settings), ...patch })
  }

  function saveIdle(input: HTMLInputElement) {
    if (!settings) return
    const n = Math.round(Number(input.value))
    const idle = Number.isFinite(n) ? Math.min(60, Math.max(1, n)) : settings.idleMinutes
    input.value = String(idle)
    if (idle !== settings.idleMinutes) save({ idleMinutes: idle })
  }

  function saveBrowsers(input: HTMLInputElement) {
    if (!settings) return
    const list = input.value
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean)
    input.value = list.join(', ')
    if (list.join('\u0000') !== settings.browsers.join('\u0000')) save({ browsers: list })
  }

  const keepOptions = $derived(
    settings && !KEEP.some((k) => k.days === settings.titleDays)
      ? [...KEEP, { days: settings.titleDays, label: `${settings.titleDays} days` }]
      : KEEP,
  )

  let span = $state<Span>('hour')
  function deleteSpan() {
    const now = new Date()
    const to = new Date(now.getTime() + 60_000).toISOString()
    const from =
      span === 'hour'
        ? new Date(now.getTime() - 3_600_000).toISOString()
        : span === 'today'
          ? startOfDay(todayIso()).toISOString()
          : new Date(0).toISOString()
    const words = SPANS.find((s) => s.id === span)?.words ?? span
    screentime.deleteRange(from, to, words)
  }
</script>

<div class="panes">
  <section aria-labelledby="st-here">
    <span class="eyebrow" id="st-here">This computer</span>

    {#if !status}
      <p class="hint">Asking this computer…</p>
    {:else}
      <label class="toggle">
        <input
          type="checkbox"
          checked={status.tracking}
          onchange={(e) => void screentime.setTracking(e.currentTarget.checked)}
        />
        <span>
          <b>Record this computer’s screen time</b>
          <small>
            Counts the app or website in front of you, and only while you are using it: not a window
            behind another, not while the screen is locked, and not after a few minutes with no
            input. Nothing is ever blocked.
          </small>
        </span>
      </label>

      <label class="setting">
        <span>Called</span>
        <input
          value={status.device}
          placeholder="laptop"
          spellcheck="false"
          autocomplete="off"
          onblur={(e) => renameDevice(e.currentTarget)}
          onkeydown={(e) => {
            if (e.key === 'Enter') e.currentTarget.blur()
          }}
        />
        <small class="hint">How this computer is named in reports, beside your other devices.</small
        >
      </label>

      <div class="fact">
        <span class="fact-label">How it watches</span>
        <p class="hint">
          Through {BACKENDS[status.backend] ?? status.backend}.
          {#if status.note}<br />{status.note}{/if}
        </p>
      </div>

      {#if status.extension}
        {@const ext = status.extension}
        <div class="fact">
          <span class="fact-label">GNOME extension</span>
          {#if !ext.installed}
            <p class="hint">
              Not installed. GNOME shows the window in front only to code running inside the
              desktop, so a small extension does the watching and sends it here.
            </p>
            <div>
              <button class="btn btn-outline" disabled={installing} onclick={() => void install()}>
                {installing ? 'Installing…' : 'Install the GNOME extension'}
              </button>
            </div>
          {:else if ext.active}
            <p class="state good"><Icon name="tick" size={14} /> Working</p>
          {:else if ext.needsLogin}
            <p class="state">
              <Icon name="info" size={14} /> Installed. Log out and back in to start: GNOME loads a new
              extension at the next login.
            </p>
          {:else if !ext.enabled}
            <p class="state"><Icon name="alert" size={14} /> Installed but switched off.</p>
            <div>
              <button class="btn btn-outline" disabled={installing} onclick={() => void install()}>
                {installing ? 'Turning it on…' : 'Turn it on'}
              </button>
            </div>
          {:else}
            <p class="state">
              <Icon name="info" size={14} /> Installed and switched on, but not heard from in the last
              two minutes.
            </p>
          {/if}
        </div>
      {/if}

      <div class="fact">
        <span class="fact-label">Browsers</span>
        <p class="hint">
          A browser extension counts each website as its own app, but only the tab in front, in the
          browser window in front.
        </p>
        {#if status.clients.filter((c) => c.collector !== 'gnome').length > 0}
          <ul class="clients">
            {#each status.clients.filter((c) => c.collector !== 'gnome') as c (c.collector)}
              <li>
                <Icon name="globe" size={14} />
                <span class="client-name">{c.collector}</span>
                <span class="client-seen">heard from {relativeTime(c.lastSeen)}</span>
              </li>
            {/each}
          </ul>
        {:else}
          <p class="hint">None connected yet.</p>
        {/if}

        <div class="connect">
          <select bind:value={browser} aria-label="Browser" disabled={connecting}>
            {#each BROWSERS as b (b.id)}
              <option value={b.id}>{b.label}</option>
            {/each}
          </select>
          <button class="btn btn-outline" disabled={connecting} onclick={() => void connect()}>
            {connecting ? 'Connecting…' : 'Connect a browser'}
          </button>
        </div>

        {#if connection}
          <div class="code">
            <p class="hint">
              Install the extension from <code>extensions/browser</code> (Load unpacked in Chrome; about:debugging
              in Firefox), open its options, and paste this code. It is shown once.
            </p>
            <div class="code-row">
              <input
                class="code-text"
                readonly
                value={connection.code}
                aria-label="Connection code"
                onfocus={(e) => e.currentTarget.select()}
              />
              <button class="btn btn-outline" onclick={() => void copyCode()}>
                <Icon name="copy" size={14} />
                {copied ? 'Copied' : 'Copy'}
              </button>
            </div>
            <p class="hint small">
              It sends to <code>{connection.endpoint}</code> as {connection.device}.
            </p>
          </div>
        {/if}
      </div>

      <div class="fact">
        <span class="fact-label">Listening</span>
        <p class="hint">
          {#if status.listening}
            Extensions send to <code>{status.listening}</code>.
          {:else}
            Not listening for extensions just now.
          {/if}
          {#if status.lastSent}
            This computer’s own time last reached the vault {relativeTime(status.lastSent)}.
          {/if}
        </p>
        {#if status.lastError}
          <p class="error">Could not send: {status.lastError}</p>
        {/if}
      </div>
    {/if}
  </section>

  <section aria-labelledby="st-vault">
    <span class="eyebrow" id="st-vault">For this vault</span>

    {#if !settings}
      <p class="hint">Loading…</p>
    {:else}
      <label class="row-setting">
        <span>
          <b>Stop counting after</b>
          <small
            >Minutes with no keyboard or mouse. A video playing in the tab in front still counts.</small
          >
        </span>
        <span class="with-unit">
          <input
            type="number"
            min="1"
            max="60"
            value={settings.idleMinutes}
            onchange={(e) => saveIdle(e.currentTarget)}
          />
          min
        </span>
      </label>

      <label class="toggle">
        <input
          type="checkbox"
          checked={settings.keepTitles}
          onchange={(e) => save({ keepTitles: e.currentTarget.checked })}
        />
        <span>
          <b>Keep window titles</b>
          <small>
            A page’s or a document’s title, shown on the timeline. Off, only the app or the site is
            kept.
          </small>
        </span>
      </label>

      <label class="row-setting">
        <span>
          <b>Keep titles for</b>
          <small>
            Older titles are cleared from the timeline. The time itself is always kept, so reports
            and goals keep their history.
          </small>
        </span>
        <select
          value={settings.titleDays}
          onchange={(e) => save({ titleDays: Number(e.currentTarget.value) })}
        >
          {#each keepOptions as k (k.days)}
            <option value={k.days}>{k.label}</option>
          {/each}
        </select>
      </label>

      <label class="setting">
        <span>Also browsers</span>
        <input
          value={settings.browsers.join(', ')}
          placeholder="e.g. vivaldi, librewolf"
          spellcheck="false"
          autocomplete="off"
          onblur={(e) => saveBrowsers(e.currentTarget)}
          onkeydown={(e) => {
            if (e.key === 'Enter') e.currentTarget.blur()
          }}
        />
        <small class="hint">
          App ids that are web browsers, beyond the ones already known, separated by commas. Their
          time is split by the sites the browser extension reports.
        </small>
      </label>

      <div class="danger-zone">
        <span>
          <b>Delete screen time</b>
          <small>Everything recorded in the stretch you choose, on every device.</small>
        </span>
        <span class="delete">
          <select bind:value={span} aria-label="What to delete">
            {#each SPANS as s (s.id)}
              <option value={s.id}>{s.label}</option>
            {/each}
          </select>
          <button class="btn btn-ghost-danger btn-outline" onclick={deleteSpan}>Delete…</button>
        </span>
      </div>
    {/if}
  </section>
</div>

<style>
  .panes {
    display: flex;
    flex-direction: column;
    gap: var(--sp-8);
    min-width: 0;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    min-width: 0;
  }

  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }
  .hint.small {
    font-size: var(--text-xs);
  }
  .error {
    margin: 0;
  }
  code {
    font-size: var(--text-xs);
    padding: 1px 4px;
    border-radius: 4px;
    background: var(--bg-sunken);
    color: var(--fg-muted);
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
  .toggle > span,
  .row-setting > span:first-child,
  .danger-zone > span:first-child {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  b {
    font-size: var(--text-base);
    font-weight: 600;
  }
  small {
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }

  .setting {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }
  .setting > span,
  .fact-label {
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }

  .row-setting,
  .danger-zone {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-4);
    min-width: 0;
  }
  .danger-zone {
    padding-top: var(--sp-4);
    border-top: 1px solid var(--border);
  }

  .fact {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }

  input:not([type='checkbox']),
  select {
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
  input:not([type='checkbox']):focus,
  select:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }
  .setting input {
    width: 100%;
  }
  .with-unit {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .with-unit input {
    width: 5rem;
    text-align: right;
  }
  .row-setting select {
    flex: none;
  }

  .state {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-muted);
  }
  .state :global(svg) {
    translate: 0 2px;
  }
  .state.good {
    color: var(--fg);
    font-weight: 550;
  }

  .clients {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .clients li {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .client-name {
    color: var(--fg);
    font-weight: 550;
    text-transform: capitalize;
  }
  .client-seen {
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  .connect,
  .delete,
  .code-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }
  .delete {
    flex: none;
  }

  .code {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-sunken);
  }
  .code-text {
    flex: 1;
    font-family: var(--font-mono);
    font-size: var(--text-xs);
  }
</style>
