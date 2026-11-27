<script lang="ts">
  import { mediaUrl, scaledSet } from '../lib/api'
  import Icon from './Icon.svelte'

  let {
    blob = null,
    title = '',
    icon = '',
    color = 'var(--accent)',
    ratio = '2 / 3',
    rounded = true,
    drawn,
  }: {
    /** A blob id. Covers live in the vault; nothing here loads a remote URL. */
    blob?: string | null
    title?: string
    /** The shelf's emoji, drawn when there is no picture. */
    icon?: string
    color?: string
    /**
     * Aspect ratio. `2 / 3` is a book jacket and a film poster; a shelf of
     * restaurants passes `4 / 3`, because a square photograph of a dining
     * room in a portrait frame is a photograph of a chair.
     */
    ratio?: string
    rounded?: boolean
    /**
     * The length in CSS pixels of the picture's longer side as drawn here,
     * when that is small: a grid's card or a list's thumbnail. The webview
     * then asks for a copy scaled down to fit (see `scaledSet`) rather than
     * the stored cover, which is sized for the widest card on a sharp screen.
     * Left out, the stored cover is drawn, as the detail panel's is.
     */
    drawn?: number
  } = $props()

  /**
   * Set when the image element gives up.
   *
   * A blob id can outlive its blob -- the garbage collector has a grace
   * period, not a guarantee, and a vault restored from an older backup can
   * be missing one. The fallback below is what stops that being a broken-
   * image glyph in the middle of an otherwise finished grid.
   */
  let broken = $state(false)
  /** Set when a scaled copy would not load: the stored cover is tried before
   *  the placeholder is drawn. */
  let unscaled = $state(false)
  // A new blob deserves a new attempt; without this, one failure would stick
  // to the card even after a cover was successfully re-fetched into it.
  $effect(() => {
    void blob
    broken = false
    unscaled = false
  })

  const src = $derived(blob && !broken ? mediaUrl(blob) : null)
  const srcset = $derived(blob && drawn && !unscaled ? scaledSet(blob) : undefined)
  const sizes = $derived(drawn ? `${Math.ceil(drawn)}px` : undefined)
  /**
   * The first letter, for a cover that does not exist.
   *
   * Better than the shelf's emoji alone: a grid of identical book emoji is
   * indistinguishable at a glance, whereas a grid of initials is scannable.
   * The emoji goes in the corner, where it says which shelf.
   */
  const initial = $derived(title.trim().slice(0, 1).toUpperCase() || '?')
</script>

<div class="cover" class:rounded style="--ratio: {ratio}; --tint: {color}">
  {#if src}
    <!-- `sizes` and `srcset` before `src`, so the copy is chosen with the
         frame's size already known rather than chosen twice. -->
    <img
      {sizes}
      {srcset}
      {src}
      alt=""
      loading="lazy"
      decoding="async"
      onerror={() => {
        if (srcset) unscaled = true
        else broken = true
      }}
    />
  {:else}
    <div class="blank" aria-hidden="true">
      <span class="initial">{initial}</span>
      {#if icon}
        <span class="badge">{icon}</span>
      {:else}
        <span class="badge"><Icon name="image" size={13} /></span>
      {/if}
    </div>
  {/if}
</div>

<style>
  .cover {
    position: relative;
    aspect-ratio: var(--ratio);
    /* So the placeholder initial can be sized against the card rather than
       the viewport -- a grid that reflows to three columns should grow its
       letters, and `vw` would shrink them. */
    container-type: inline-size;
    overflow: hidden;
    background: var(--bg-sunken);
    /* A jacket has an edge. Without this, a pale cover on a pale panel has
       no boundary and the grid reads as floating text. */
    box-shadow: inset 0 0 0 1px rgb(0 0 0 / 0.08);
  }
  .rounded {
    border-radius: var(--radius);
  }

  img {
    display: block;
    width: 100%;
    height: 100%;
    /* Cover art arrives at every ratio anyone has ever used. Cropping to the
       frame keeps the grid a grid; letterboxing would make each row a
       different shape. */
    object-fit: cover;
  }

  /* The placeholder is a wash of the shelf's own colour, so an unfetched
     cover still says which shelf it is on -- and so a grid mid-fetch looks
     deliberate rather than half-loaded. */
  .blank {
    display: grid;
    place-items: center;
    width: 100%;
    height: 100%;
    background:
      linear-gradient(
        160deg,
        color-mix(in oklab, var(--tint) 22%, transparent),
        color-mix(in oklab, var(--tint) 6%, transparent)
      ),
      var(--bg-raised);
  }
  .initial {
    font-family: var(--font-read);
    font-size: clamp(var(--text-xl), 34cqw, 56px);
    font-weight: 300;
    line-height: 1;
    color: color-mix(in oklab, var(--tint) 62%, var(--fg));
    opacity: 0.55;
    user-select: none;
  }
  .badge {
    position: absolute;
    right: var(--sp-2);
    bottom: var(--sp-2);
    display: grid;
    place-items: center;
    font-size: var(--text-sm);
    line-height: 1;
    color: var(--fg-faint);
    opacity: 0.8;
  }
</style>
