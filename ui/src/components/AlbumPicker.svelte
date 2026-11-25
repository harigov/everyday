<script lang="ts">
  // "Add these to an album": a small popover listing the albums, with a field
  // that filters them and, when what is typed is not an album yet, starts one.
  //
  // Drawn one of two ways. Under whatever opened it: put it inside a wrapper
  // that is `position: relative` and give the button that toggles it the
  // attribute `data-album-trigger`, so a press on that button is the button's
  // to handle -- it closes the popover itself -- rather than this one closing
  // on the press and the button's click opening it again straight away (see
  // `dismiss.ts`). Or at a point, given `at` in window coordinates: where a
  // right-click or a key on a picture raised it, kept inside the window.

  import { onMount } from 'svelte'
  import { dismissable } from '../lib/dismiss'
  import { focusOnMount } from '../lib/focus'
  import { files } from '../lib/files.svelte'
  import { filesUi } from '../lib/files-ui.svelte'
  import { closesOnBack, phone } from '../lib/phone.svelte'
  import type { AlbumId, FileId } from '../lib/types'
  import Icon from './Icon.svelte'

  let {
    ids,
    onclose,
    align = 'left',
    at,
  }: {
    /** What to add. */
    ids: FileId[]
    onclose: () => void
    /** Which edge of the button the popover lines up with. */
    align?: 'left' | 'right'
    /** Draw it at this point of the window instead of under a button. */
    at?: { x: number; y: number }
  } = $props()

  /** Wide and tall enough for the popover, to keep it on screen. */
  const WIDTH = 264
  const HEIGHT = 330

  const place = $derived(
    at && !phone.on
      ? `position: fixed; left: ${Math.max(8, Math.min(at.x, window.innerWidth - WIDTH - 8))}px; ` +
          `top: ${Math.max(8, Math.min(at.y + 4, window.innerHeight - HEIGHT))}px`
      : undefined,
  )

  let name = $state('')
  let busy = $state(false)

  // The albums, fetched once if the store has none yet.
  onMount(() => void filesUi.ensureAlbums())

  const typed = $derived(name.trim())
  const shown = $derived(
    typed
      ? files.albums.filter((v) => v.album.name.toLowerCase().includes(typed.toLowerCase()))
      : files.albums,
  )
  /** What was typed is an album already: Enter adds to it rather than making
   *  a second with the same name. */
  const same = $derived(
    files.albums.find((v) => v.album.name.toLowerCase() === typed.toLowerCase()),
  )

  async function add(id: AlbumId) {
    if (busy || ids.length === 0) return
    busy = true
    await files.addToAlbum(id, ids)
    onclose()
  }

  async function create() {
    if (busy || !typed || ids.length === 0) return
    busy = true
    await files.createAlbum(typed, ids)
    onclose()
  }

  function submit() {
    if (same) void add(same.album.id)
    else void create()
  }

  // Back puts it away, on a phone.
  closesOnBack(
    () => true,
    () => onclose(),
  )
</script>

<svelte:window onkeydown={(e: KeyboardEvent) => e.key === 'Escape' && onclose()} />

<!-- A phone's is a sheet, hanging from the top of the screen, over a scrim. -->
{#if phone.on}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim" onclick={onclose}></div>
{/if}

<div
  class="pop frost"
  class:right={align === 'right'}
  role="dialog"
  aria-label="Add to album"
  tabindex="-1"
  style={place}
  use:dismissable={{ onaway: onclose, within: '[data-album-trigger]' }}
>
  <input
    class="field"
    use:focusOnMount
    bind:value={name}
    placeholder="Find an album or start one"
    aria-label="Album name"
    autocomplete="off"
    spellcheck="false"
    onkeydown={(e) => e.key === 'Enter' && submit()}
  />
  <div class="list scroll">
    {#each shown as view (view.album.id)}
      <button class="side-row" disabled={busy} onclick={() => void add(view.album.id)}>
        <span class="side-icon"><Icon name="image" size={14} /></span>
        <span class="side-text">{view.album.name}</span>
        <span class="side-count">{view.count}</span>
      </button>
    {/each}
    {#if typed && !same}
      <button class="side-row" disabled={busy} onclick={() => void create()}>
        <span class="side-icon"><Icon name="plus" size={14} /></span>
        <span class="side-text">New album “{typed}”</span>
      </button>
    {:else if files.albums.length === 0}
      <p class="hint">No albums yet. Type a name to start one.</p>
    {/if}
  </div>
</div>

<style>
  .pop {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    z-index: 50;
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    width: 264px; /* `WIDTH` */
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-lg);
  }
  .pop.right {
    left: auto;
    right: 0;
  }
  .field {
    height: 34px;
  }
  /* Roughly seven rows, then it scrolls. */
  .list {
    display: flex;
    flex-direction: column;
    max-height: calc(var(--side-row-h) * 7);
  }
  .hint {
    padding: var(--sp-2) var(--sp-3);
  }

  /* ── A phone ──────────────────────────────────────────────────────────
     Hung from the top of the screen like every sheet, as wide as it allows,
     above the scrim and the bar of apps. */
  :global([data-phone]) .pop {
    position: fixed;
    z-index: 66;
    top: calc(var(--safe-top) + max(8dvh, var(--sp-4)));
    left: calc(var(--safe-left) + var(--sp-3));
    right: calc(var(--safe-right) + var(--sp-3));
    width: auto;
    padding: var(--sp-3);
  }
  :global([data-phone]) .field {
    height: 44px;
  }
  :global([data-phone]) .list {
    max-height: min(calc(var(--side-row-h) * 7), 40dvh);
  }
</style>
