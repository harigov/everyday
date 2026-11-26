<script lang="ts">
  // The app bar, on a phone: along the bottom, where a thumb reaches, with
  // the four apps the person chose (`phonebar.svelte.ts`) and "More" for the
  // rest. "More" opens every app as a grid, with the vault's own two --
  // Settings and Lock -- under it, and is where the bar is chosen: "Edit bar",
  // then tap apps on and off it. The desktop's `AppBar` is the same list
  // standing up the window's left edge; this is drawn instead of it.

  import { app, type Section } from '../lib/state.svelte'
  import { APPS, APP_ORDER } from '../lib/apps'
  import { assistant } from '../lib/assistant.svelte'
  import { jobs } from '../lib/jobs.svelte'
  import { proposals } from '../lib/proposals.svelte'
  import { panels } from '../lib/panels.svelte'
  import { phoneBar, BAR_SIZE } from '../lib/phonebar.svelte'
  import { closesOnBack } from '../lib/phone.svelte'
  import { trapFocus } from '../lib/focus'
  import Icon from './Icon.svelte'
  import Symbol from './Symbol.svelte'

  let more = $state(false)
  let editing = $state(false)
  closesOnBack(
    () => more,
    () => close(),
  )

  const bar = $derived(phoneBar.apps.filter((s) => app.canShow(s)))
  const all = $derived(APP_ORDER.filter((s) => app.canShow(s)))
  /** The assistant's badge, as on the desktop's bar: runs, jobs and
   *  proposals nobody has looked at yet. */
  const unseen = $derived(assistant.unseen + jobs.unseen + proposals.unseen)
  /** Where you are is under "More" when it is not one of the four, or is
   *  Settings. */
  const elsewhere = $derived(panels.settings !== null || !bar.includes(app.section))

  function close() {
    more = false
    editing = false
  }

  function open(section: Section) {
    close()
    app.setSection(section)
  }

  function pick(section: Section) {
    if (editing) phoneBar.toggle(section)
    else open(section)
  }
</script>

<!-- A keyboard on a phone is rare, but an external one is not unheard of, and
     a dialog that cannot be left with Escape is a trap. -->
<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key === 'Escape' && more) close()
  }}
/>

<nav class="bottombar" data-bottom-bar aria-label="Apps">
  {#each bar as id (id)}
    {@const on = app.section === id && panels.settings === null && !more}
    <button
      class="tab"
      class:on
      style:--app="var(--app-{id})"
      aria-current={on ? 'page' : undefined}
      title={APPS[id].label}
      onclick={() => open(id)}
    >
      <span class="glyph">
        <Symbol name={APPS[id].symbol} size={24} />
        {#if id === 'assistant' && unseen > 0}
          <span class="badge">{unseen > 9 ? '9+' : unseen}</span>
        {/if}
      </span>
      <span class="label">{APPS[id].label}</span>
    </button>
  {/each}
  <button
    class="tab more"
    class:on={more || elsewhere}
    aria-expanded={more}
    aria-haspopup="dialog"
    title="More apps"
    onclick={() => (more ? close() : (more = true))}
  >
    <span class="glyph"><Icon name="grid" size={22} weight={1.7} /></span>
    <span class="label">More</span>
  </button>
</nav>

{#if more}
  <!-- Over everything but the bar's own row, so "More" closes it again. -->
  <div class="scrim" aria-hidden="true" onclick={close}></div>
  <div class="moresheet sheet" role="dialog" aria-modal="true" aria-label="All apps" use:trapFocus>
    <div class="head">
      <h2>{editing ? `Up to ${BAR_SIZE} on the bar` : 'Apps'}</h2>
      <button class="btn btn-sm" onclick={() => (editing = !editing)}>
        {editing ? 'Done' : 'Edit bar'}
      </button>
    </div>
    <div class="grid">
      {#each all as id (id)}
        {@const on = !editing && app.section === id && panels.settings === null}
        {@const pinned = phoneBar.has(id)}
        <button
          class="app"
          class:on
          class:pinned={editing && pinned}
          style:--app="var(--app-{id})"
          aria-pressed={editing ? pinned : undefined}
          aria-current={on ? 'page' : undefined}
          disabled={editing && !pinned && phoneBar.apps.length >= BAR_SIZE}
          onclick={() => pick(id)}
        >
          <span class="glyph">
            <Symbol name={APPS[id].symbol} size={28} />
            {#if editing && pinned}
              <span class="tick"><Icon name="tick" size={12} weight={2.4} /></span>
            {/if}
          </span>
          <span class="label">{APPS[id].label}</span>
        </button>
      {/each}
    </div>
    {#if !editing}
      <div class="vault">
        <button
          class="row"
          onclick={() => {
            close()
            panels.openSettings()
          }}
        >
          <Symbol name="settings" size={20} />
          <span>Settings</span>
        </button>
        <button
          class="row"
          onclick={() => {
            close()
            void app.lock()
          }}
        >
          <Symbol name="lock" size={20} />
          <span>Lock</span>
        </button>
      </div>
    {/if}
  </div>
{/if}

<style>
  .bottombar {
    display: flex;
    flex: none;
    gap: 2px;
    padding: 4px max(4px, var(--safe-left)) calc(4px + var(--safe-bottom))
      max(4px, var(--safe-right));
    background: var(--chrome);
    border-top: 1px solid var(--border);
    /* Over the scrim of the sheet it opens, so "More" closes it again. */
    position: relative;
    z-index: 62;
  }

  .tab {
    flex: 1;
    min-width: 0;
    display: grid;
    justify-items: center;
    gap: 4px;
    padding: 7px 0 6px;
    border-radius: var(--radius-lg);
    color: var(--fg-subtle);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .tab .glyph {
    position: relative;
    display: grid;
    place-items: center;
    height: 24px;
    color: var(--app, currentColor);
  }
  .tab.more .glyph {
    color: inherit;
  }
  .tab.on {
    background: color-mix(in oklab, var(--app, var(--accent)) 16%, transparent);
    color: var(--fg);
  }
  .label {
    max-width: 100%;
    overflow: hidden;
    font-size: 11px;
    font-weight: 600;
    line-height: 1.1;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .badge {
    position: absolute;
    top: -4px;
    right: -9px;
    display: grid;
    place-items: center;
    min-width: 16px;
    height: 16px;
    padding: 0 4px;
    border-radius: var(--radius-pill);
    background: var(--accent);
    color: var(--fg-on-accent);
    font-size: 10px;
    font-weight: 700;
    line-height: 1;
  }

  /* A sheet up from the bar rather than the centred one `.sheet` is
     elsewhere: the bar stays where it is, under the thumb that opened it. */
  .moresheet {
    top: auto;
    bottom: calc(var(--bottombar-h) + var(--safe-bottom));
    left: 0;
    right: 0;
    translate: none;
    width: auto;
    max-height: calc(100dvh - var(--bottombar-h) - var(--safe-bottom) - var(--safe-top) - 24px);
    overflow-y: auto;
    padding: var(--sp-4) max(var(--sp-4), var(--safe-left)) var(--sp-3)
      max(var(--sp-4), var(--safe-right));
    border-radius: var(--radius-lg) var(--radius-lg) 0 0;
    border-bottom: none;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--sp-3);
  }
  .head h2 {
    font-size: var(--text-md);
    font-weight: 650;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: var(--sp-1);
  }
  .app {
    display: grid;
    justify-items: center;
    gap: 6px;
    padding: 12px 2px 10px;
    border-radius: var(--radius-lg);
    color: var(--fg-muted);
  }
  .app .glyph {
    position: relative;
    display: grid;
    place-items: center;
    height: 28px;
    color: var(--app);
  }
  .app.on,
  .app.pinned {
    background: color-mix(in oklab, var(--app, var(--accent)) 16%, transparent);
    color: var(--fg);
  }
  .app[disabled] {
    opacity: 0.45;
  }
  .app .label {
    font-size: var(--text-xs);
  }
  .tick {
    position: absolute;
    right: -8px;
    bottom: -4px;
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border-radius: var(--radius-pill);
    background: var(--accent);
    color: var(--fg-on-accent);
  }

  .vault {
    display: flex;
    gap: var(--sp-2);
    margin-top: var(--sp-3);
    padding-top: var(--sp-3);
    border-top: 1px solid var(--border);
  }
  .row {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--sp-2);
    min-height: 44px;
    border-radius: var(--radius);
    background: var(--bg-hover);
    color: var(--fg);
    font-weight: 600;
  }
</style>
