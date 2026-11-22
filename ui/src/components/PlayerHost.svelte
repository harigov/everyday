<script lang="ts">
  // The window's one media element, and the panel that stands over it.
  //
  // Mounted once, in `App.svelte`, outside every app, so switching from the
  // Feed to Mail or Files never stops what is playing: the element is never
  // created again and never moves in the document. That is also why a film
  // does not live in the panel that shows it. A video element moved to
  // another parent (or one inside a block that is shown and hidden) is
  // re-created or paused by the engine, and the sound would stop with it.
  // So the element stays here, in a box fixed to the window, and when the
  // panel is open on a video the box is laid exactly over the panel's picture
  // area (`PlayerPanel` hands that element over with `picture`), following it
  // frame by frame. At every other time the box is one transparent pixel:
  // still on the page and still playing, which is all audio needs.
  //
  // `<video>` rather than `<audio>` because it plays both, and the player
  // (`player.svelte.ts`) is handed this one element to play everything on.
  //
  // Up Next and the settings are asked for here once the vault is open
  // (this is only mounted then), so "In Up Next" is right wherever a button
  // for it is drawn -- an item's page, a file's menu -- without each of them
  // loading the queue.

  import { onMount } from 'svelte'
  import { player } from '../lib/player.svelte'
  import { upnext } from '../lib/upnext.svelte'
  import PlayerPanel from './PlayerPanel.svelte'

  let media = $state<HTMLVideoElement | null>(null)
  /** The panel's picture area, while the panel is open on a video. */
  let picture = $state<HTMLElement | null>(null)

  onMount(() => {
    if (media) player.attach(media)
    void upnext.load()
    void upnext.loadSettings()
    return () => player.detach()
  })

  interface Box {
    left: number
    top: number
    width: number
    height: number
  }
  /** Where the film is drawn: over the picture area, or `null` for hidden. */
  let box = $state<Box | null>(null)

  /** A film is wanted on screen: loaded, the panel open, its picture area there. */
  const showing = $derived(player.video && player.expanded && picture !== null)

  // Follow the picture area while it is wanted. A frame loop rather than
  // observers: the panel fades in, the window is resized, a banner above the
  // picture appears and moves it -- a size observer hears of none of the
  // moves that leave the size alone. It reads one rectangle per frame and
  // writes only when it differs, and runs only while a film is showing.
  $effect(() => {
    const target = picture
    if (!showing || !target) {
      box = null
      return
    }
    let frame = 0
    let last = ''
    const follow = () => {
      const r = target.getBoundingClientRect()
      const key = `${r.left}|${r.top}|${r.width}|${r.height}`
      if (key !== last) {
        last = key
        box = { left: r.left, top: r.top, width: r.width, height: r.height }
      }
      frame = requestAnimationFrame(follow)
    }
    follow()
    return () => {
      cancelAnimationFrame(frame)
      box = null
    }
  })
</script>

<!-- A click on the picture plays and pauses, as on every video. The panel has
     the buttons for those who would rather press one, so this is no one's
     only way. -->
<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div
  class="stage"
  class:shown={showing && box !== null}
  style:left={box ? `${box.left}px` : undefined}
  style:top={box ? `${box.top}px` : undefined}
  style:width={box ? `${box.width}px` : undefined}
  style:height={box ? `${box.height}px` : undefined}
  onclick={() => player.toggle()}
>
  <!-- What is played may be a stranger's film, which comes with no captions
       to offer. -->
  <video bind:this={media} playsinline preload="auto"></video>
</div>

{#if player.expanded}
  <PlayerPanel bind:picture />
{/if}

<style>
  /* One transparent pixel when nothing is to be seen: not `display: none`,
     which some engines take as leave to stop a video decoding, and not off
     the window's edge, where one may decide it need not keep up. Over the
     panel's own layer (45), since the film is drawn on the panel. */
  .stage {
    position: fixed;
    left: 0;
    top: 0;
    z-index: 46;
    width: 1px;
    height: 1px;
    overflow: hidden;
    opacity: 0;
    pointer-events: none;
  }
  .stage.shown {
    opacity: 1;
    pointer-events: auto;
    border-radius: var(--radius);
    background: var(--bg-sunken);
    cursor: pointer;
  }
  video {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
</style>
