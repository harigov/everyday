<script lang="ts">
  // The apps, and the two buttons that are not apps, as a bar down the left
  // edge of the window.
  //
  // They were a segmented control at the top of the sidebar, which was the
  // right shape for two of them and the wrong one for four, let alone for
  // what there are now: the labels stopped fitting on a row, so it wrapped --
  // and a wrapped segmented control reads as a set of filters over what is
  // below it rather than as the thing that decides what the whole window is.
  //
  // A bar says it better. Each app is a target you can hit without aiming,
  // with its name under its mark rather than beside it, and it sits outside
  // the sidebar because it is not part of any one app's navigation: the
  // panel to the right of it changes completely when one of these is
  // pressed, and the bar does not.

  import { app, type Section } from '../lib/state.svelte'
  import { assistant } from '../lib/assistant.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { panels } from '../lib/panels.svelte'
  import { tray, type TrayEntry } from '../lib/tray.svelte'
  import type { Group } from '../lib/shortcuts.svelte'
  import Icon from './Icon.svelte'
  import type { IconName } from '../lib/icons'

  interface AppEntry {
    id: Section
    /**
     * What the tab says, and -- the same word, deliberately -- the heading
     * this app's quick actions are filed under in the action table.
     *
     * Typed as a `Group` so the two cannot come apart. There used to be a
     * separate `source` field holding the same name in lower case, and two
     * spellings of one name is how the right-click menu below would quietly
     * come back empty after the table was renamed.
     */
    label: Group
    icon: IconName
  }

  // Each tab is hidden on a backend that cannot carry it, so a vault never
  // offers an app that cannot work.
  const APPS: AppEntry[] = [
    { id: 'journal', label: 'Journal', icon: 'quote' },
    { id: 'notes', label: 'Notes', icon: 'pencil' },
    { id: 'todo', label: 'Todo', icon: 'check' },
    { id: 'calendar', label: 'Calendar', icon: 'calendar' },
    { id: 'library', label: 'Library', icon: 'book' },
    // Last but for the assistant, and deliberately: it is a view over what
    // the apps above store, so the bar reads top to bottom as the things you
    // do and then the thing they add up to.
    { id: 'overview', label: 'Overview', icon: 'compass' },
    { id: 'assistant', label: 'Assistant', icon: 'sparkle' },
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
      ...tray.entriesFor(entry.label).map(toItem),
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
          class="barbtn"
          class:on
          aria-current={on ? 'page' : undefined}
          onclick={() => app.setSection(a.id)}
          oncontextmenu={(e) => menu.show(e, appMenu(a))}
        >
          <span class="glyph">
            <Icon name={a.icon} size={21} weight={1.7} />
            <!-- How anybody finds out the assistant did something while they
                 were away. A number rather than a stream of banners: work
                 done overnight is a queue, not an interruption. -->
            {#if a.id === 'assistant' && assistant.unseen > 0}
              <span class="badge">{assistant.unseen > 9 ? '9+' : assistant.unseen}</span>
            {/if}
          </span>
          <span class="barlabel">{a.label}</span>
        </button>
      {/each}
    </nav>
  {/if}

  <!-- Settings and the lock live down here rather than under the sidebar's
       nav, because neither belongs to whichever app is open: they are the
       vault's, and so is the bar.

       The assistant used to be here too. It is a floating button in the
       corner of the pane now -- see `App.svelte` -- because it is not one of
       the vault's controls either: it works *on* whatever app is open, and
       the corner of that app is where it belongs. -->
  <div class="foot">
    <button
      class="barbtn"
      class:on={panels.settings !== null}
      onclick={() => panels.openSettings()}
      title="Settings (Ctrl+,)"
    >
      <span><Icon name="settings" size={19} weight={1.7} /></span>
      <span class="barlabel">Settings</span>
    </button>
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
    height: var(--header-h);
    flex: none;
  }

  .glyph {
    position: relative;
    display: grid;
    place-items: center;
  }

  .badge {
    position: absolute;
    top: -4px;
    right: -7px;
    display: grid;
    place-items: center;
    min-width: 15px;
    height: 15px;
    padding: 0 4px;
    border-radius: 999px;
    background: var(--accent);
    color: #fff;
    font-size: 10px;
    font-weight: 700;
    line-height: 1;
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
  .barbtn.on {
    background: color-mix(in oklab, var(--journal-accent, var(--accent)) 14%, transparent);
    color: var(--journal-accent, var(--accent));
  }
</style>
