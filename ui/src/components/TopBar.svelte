<script lang="ts">
  // The strip across the top of the window: the sidebar's fold button, and
  // the one field that searches and does things -- see `CommandBar.svelte`.
  //
  // Across the whole window rather than over one app, because neither thing
  // in it belongs to an app. The fold button is the app bar's neighbour for
  // the same reason the app bar is outside the sidebar: it changes the shape
  // of whatever is open. The bar is the same field in every app, in the same
  // place, which is the point of having one.
  //
  // It is also the window's title bar, and the only one: the empty
  // stretches of it are where the window is dragged from, and a double
  // click on one maximises it. On macOS the system draws its controls over
  // this strip's left end (`titleBarStyle: Overlay`) -- nothing of ours is
  // drawn under them. Everywhere else the window has no bar of its own, so
  // its minimise, maximise and close buttons are ours, at the right end --
  // see `WindowControls.svelte`.

  import { app } from '../lib/state.svelte'
  import { keysLabel } from '../lib/keys'
  import { panels } from '../lib/panels.svelte'
  import { sidebar } from '../lib/sidebar.svelte'
  import CommandBar from './CommandBar.svelte'
  import Icon from './Icon.svelte'
  import Logo from './Logo.svelte'
  import WindowControls from './WindowControls.svelte'

  const mac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform ?? '')

  /** The Overview has no sidebar, and Settings draws its own list of tabs. */
  const foldable = $derived(app.section !== 'overview' && panels.settings === null)
  const folded = $derived(foldable && sidebar.folded(app.section))
  const shortcut = keysLabel('mod+\\', mac)
</script>

<header class="strip" data-tauri-drag-region>
  <div class="lead" data-tauri-drag-region>
    {#if !mac}<Logo size={18} tile />{/if}
  </div>

  <div class="side" data-tauri-drag-region>
    {#if foldable}
      <!-- Resting on it shows a folded sidebar for a moment, the way the
           window's left edge does: the button is where somebody looking for
           the sidebar will put the pointer first. -->
      <button
        class="fold"
        class:folded
        onclick={() => sidebar.toggle(app.section)}
        onpointerenter={(e) => {
          if (folded && e.pointerType === 'mouse') sidebar.reach()
        }}
        onpointerleave={() => {
          if (folded) sidebar.release()
        }}
        aria-pressed={!folded}
        aria-label={folded ? 'Show the sidebar' : 'Hide the sidebar'}
        title="{folded ? 'Show the sidebar' : 'Hide the sidebar'} ({shortcut})"
      >
        <Icon name="sidebar" size={17} weight={1.7} />
      </button>
    {/if}
  </div>

  <div class="middle">
    <CommandBar />
  </div>

  <div class="end" data-tauri-drag-region>
    <WindowControls />
  </div>
</header>

<style>
  .strip {
    display: grid;
    /* The middle column is the bar's, capped so it reads as a field rather
       than a ruler across a wide window; the two either side share what is
       left equally, so the bar stays centred on the window. Neither gives up
       more than its contents, though: in a narrow window the bar shrinks, or
       sits a little off centre, rather than sliding under the window's own
       buttons. */
    grid-template-columns:
      var(--appbar-w) minmax(max-content, 1fr) minmax(220px, 600px)
      minmax(max-content, 1fr);
    align-items: center;
    height: var(--topbar-h);
    flex: none;
    background: var(--bg-sunken);
    border-bottom: 1px solid var(--border);
  }

  .lead {
    display: grid;
    place-items: center;
    height: 100%;
  }

  .side,
  .end {
    display: flex;
    align-items: center;
    height: 100%;
    min-width: 0;
  }
  .side {
    padding-left: var(--sp-2);
  }
  .end {
    justify-content: flex-end;
    padding-right: var(--sp-2);
  }

  .middle {
    min-width: 0;
    padding: 0 var(--sp-3);
  }

  .fold {
    display: grid;
    place-items: center;
    width: 30px;
    height: 28px;
    border-radius: var(--radius-sm);
    color: var(--fg-subtle);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .fold:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .fold.folded {
    color: var(--fg-faint);
  }
</style>
