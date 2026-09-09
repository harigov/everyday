<script lang="ts">
  // The apps, and the two buttons that are not apps, as a bar down the left
  // edge of the window.
  //
  // They were a segmented control at the top of the sidebar, which was the
  // right shape for two of them and the wrong one for four: the labels
  // stopped fitting on a row, so it wrapped to two rows of two -- and a
  // wrapped segmented control reads as a set of filters over what is below
  // it rather than as the thing that decides what the whole window is.
  //
  // A bar says it better. Each app is a target you can hit without aiming,
  // with its name under its mark rather than beside it, and it sits outside
  // the sidebar because it is not part of any one app's navigation: the
  // panel to the right of it changes completely when one of these is
  // pressed, and the bar does not.

  import { app, type Section } from '../lib/state.svelte'
  import { agent } from '../lib/agent.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { tray, type TrayEntry } from '../lib/tray.svelte'
  import Icon from './Icon.svelte'
  import SettingsMenu from './SettingsMenu.svelte'
  import type { IconName } from '../lib/icons'

  interface AppEntry {
    id: Section
    label: string
    icon: IconName
    /** The key its quick actions are registered under; see `tray.svelte.ts`. */
    source: string
  }

  // Each tab is hidden on a backend that cannot carry it, so a vault never
  // offers an app that cannot work.
  const APPS: AppEntry[] = [
    { id: 'journal', label: 'Journal', icon: 'quote', source: 'journal' },
    { id: 'todo', label: 'Todo', icon: 'check', source: 'todo' },
    { id: 'calendar', label: 'Calendar', icon: 'calendar', source: 'calendar' },
    { id: 'library', label: 'Library', icon: 'book', source: 'library' },
  ]
  const shown = $derived(APPS.filter((a) => app.canShow(a.id)))

  /**
   * A tray entry as a menu row.
   *
   * The two menus are the same shape by luck rather than by design -- one is
   * drawn by the platform and one by us -- but the mapping is three lines,
   * and the alternative is a second list of an app's quick actions written
   * beside the first.
   */
  function toItem(entry: TrayEntry): MenuItem {
    if ('separator' in entry) return SEP
    if ('items' in entry) {
      return {
        label: entry.label,
        disabled: entry.enabled === false,
        items: entry.items.map(toItem),
      }
    }
    return {
      label: entry.label,
      disabled: entry.enabled === false,
      checked: entry.checked,
      run: entry.run,
    }
  }

  /** Go there, and whatever that app can start from a standing stop. */
  function appMenu(entry: AppEntry): MenuItem[] {
    return tidyMenu([
      {
        label: `Open ${entry.label}`,
        icon: entry.icon,
        disabled: app.section === entry.id,
        run: () => app.setSection(entry.id),
      },
      SEP,
      ...tray.entriesFor(entry.source).map(toItem),
    ])
  }
</script>

<div class="bar">
  <!-- Aligned with the brand row beside it, and empty on purpose: on macOS
       this is where the window controls sit, and nothing of ours may be
       drawn under them. -->
  <div class="cap"></div>

  <!-- A bar with one app on it is a decoration: a backend that stores
       journals and nothing else leaves nothing to switch between. The two
       buttons at the foot are there either way. -->
  {#if shown.length > 1}
    <nav aria-label="Apps">
      {#each shown as a (a.id)}
        {@const on = app.section === a.id}
        <button
          class="barbtn app"
          class:on
          aria-current={on ? 'page' : undefined}
          onclick={() => app.setSection(a.id)}
          oncontextmenu={(e) => menu.show(e, appMenu(a))}
        >
          <span><Icon name={a.icon} size={21} weight={1.7} /></span>
          <span class="barlabel">{a.label}</span>
        </button>
      {/each}
    </nav>
  {/if}

  <!-- Settings and the lock live down here rather than under the sidebar's
       nav, because neither belongs to whichever app is open: they are the
       vault's, and so is the bar. -->
  <div class="foot">
    <!-- Not one of the apps, and so not in the nav above: the assistant does
         not replace what is on screen, it opens beside it. -->
    {#if agent.supported}
      <button
        class="barbtn"
        class:on={agent.open}
        onclick={() => void agent.toggle()}
        title="Assistant"
      >
        <span><Icon name="sparkle" size={19} weight={1.7} /></span>
        <span class="barlabel">Assistant</span>
      </button>
    {/if}
    <SettingsMenu />
    <button class="barbtn" onclick={() => app.lock()} title="Lock now (Ctrl+L)">
      <span><Icon name="lock" size={19} weight={1.7} /></span>
      <span class="barlabel">Lock</span>
    </button>
  </div>
</div>

<style>
  .bar {
    width: var(--appbar-w);
    flex: none;
    display: flex;
    flex-direction: column;
    align-items: center;
    background: var(--bg-sunken);
    border-right: 1px solid var(--border);
  }

  /* The height of the brand row and of every other header in the window. */
  .cap {
    height: 46px;
    flex: none;
  }

  nav {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
  }

  /* Pushed to the bottom, and ruled off: the same distinction the sidebar's
     foot drew when these two lived there. */
  .foot {
    margin-top: auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    padding: var(--sp-2) 0;
    border-top: 1px solid var(--border);
    width: 100%;
  }

  /* The same active treatment the editor's toolbar uses for a mark that is
     on, in the accent of whatever is open -- so the bar is tinted by the
     journal or the project the rest of the window is already tinted by. */
  .app.on {
    background: color-mix(in oklab, var(--journal-accent, var(--accent)) 14%, transparent);
    color: var(--journal-accent, var(--accent));
  }
</style>
