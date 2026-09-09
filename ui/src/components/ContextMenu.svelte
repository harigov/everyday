<script lang="ts">
  // The window's one context menu.
  //
  // Mounted once by `App.svelte`; every list opens it through the `menu`
  // store rather than owning a menu of its own. All this adds around the
  // panel is the reasons a menu closes without anything being chosen:
  // clicking away, right-clicking away, the window changing shape or
  // scrolling under it, and the vault locking.

  import { menu } from '../lib/menu.svelte'
  import MenuPanel from './MenuPanel.svelte'

  /**
   * A menu is anchored to a point in the window, so anything that moves what
   * is under that point invalidates it.
   *
   * Scroll is watched on the capture phase because the lists scroll, not the
   * window -- but not the menu's own scrolling, which is a long menu being
   * read rather than the ground moving under it.
   *
   * A press somewhere else is handled by `dismissable` on the panel rather
   * than by a scrim over the window. That module's comment has the argument;
   * the short version is that a scrim eats the click it is dismissed by, so
   * with a menu open the first click on the app bar did nothing at all.
   */
  $effect(() => {
    if (!menu.at) return
    const away = (e: Event) => {
      if (e.target instanceof Node && (e.target as Element).closest?.('[role="menu"]')) return
      menu.close(false)
    }
    window.addEventListener('scroll', away, true)
    window.addEventListener('resize', away)
    window.addEventListener('blur', away)
    // A right-click elsewhere dismisses this one -- and is then free to open
    // its own, because nothing is standing in front of the row it was on.
    window.addEventListener('contextmenu', away, true)
    return () => {
      window.removeEventListener('scroll', away, true)
      window.removeEventListener('resize', away)
      window.removeEventListener('blur', away)
      window.removeEventListener('contextmenu', away, true)
    }
  })
</script>

{#if menu.at}
  <MenuPanel
    items={menu.items}
    at={menu.at}
    onclose={() => menu.close()}
    onexit={() => menu.close()}
    ondismiss={() => menu.close(false)}
  />
{/if}
