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
    return () => {
      window.removeEventListener('scroll', away, true)
      window.removeEventListener('resize', away)
      window.removeEventListener('blur', away)
    }
  })
</script>

{#if menu.at}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="scrim"
    onpointerdown={() => menu.close(false)}
    oncontextmenu={(e) => {
      e.preventDefault()
      menu.close(false)
    }}
  ></div>
  <MenuPanel
    items={menu.items}
    at={menu.at}
    onclose={() => menu.close()}
    onexit={() => menu.close()}
  />
{/if}

<style>
  /* Invisible, and only there to catch the click that dismisses the menu: a
     context menu does not dim what it is about. */
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 70;
  }
</style>
