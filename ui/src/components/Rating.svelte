<script lang="ts">
  import {
    MAX_STARS,
    STAR_STEP,
    fromStars,
    ratingLabel,
    ratingTitle,
    starFill,
  } from '../lib/rating'
  import Icon from './Icon.svelte'

  let {
    value = null,
    size = 15,
    readonly = false,
    showNumber = false,
    /** A muted second row, e.g. what Open Library thinks. */
    muted = false,
    label = 'Rating',
    onchange,
  }: {
    /** The stored 0-100 score, or null for "not rated". */
    value?: number | null
    size?: number
    readonly?: boolean
    showNumber?: boolean
    muted?: boolean
    label?: string
    onchange?: (value: number | null) => void
  } = $props()

  /**
   * What the row is currently drawing.
   *
   * The hovered value while the pointer is over an editable row, and the
   * stored one otherwise. A star control that only updates on click makes
   * you guess where the half steps are; one that previews under the cursor
   * does not.
   */
  let hovered = $state<number | null>(null)
  const shown = $derived(hovered ?? value ?? 0)

  /**
   * Which half-star position the pointer is on, in `0.5 .. 5`.
   *
   * Worked out from the position within the star rather than from a `<input
   * type=range>`, because the target *is* the drawing -- there is no separate
   * track to aim at, and a range input over the top would take the hover
   * feedback away from the shape underneath it.
   */
  function positionAt(event: MouseEvent, star: number): number {
    const box = (event.currentTarget as HTMLElement).getBoundingClientRect()
    const half = event.clientX - box.left < box.width / 2
    return star - (half ? STAR_STEP : 0)
  }

  function commit(next: number) {
    // Clicking the score you already gave clears it. Otherwise there is no
    // way back to "not rated" short of a second control, and "I have not
    // decided" is a real answer -- the sort order depends on it being one.
    const score = fromStars(next)
    onchange?.(score === value ? null : score)
    hovered = null
  }

  /**
   * Arrow keys move by a half star; the row is one tab stop, not five.
   *
   * Five separate buttons would make a rating cost five tabs to get past,
   * which for a control most people never touch by keyboard is the wrong
   * trade. This matches how a slider behaves, which is what it is.
   */
  function onKeydown(event: KeyboardEvent) {
    if (readonly) return
    const current = value === null ? 0 : fromStars(starsOf(value))
    let next: number | null = null
    if (event.key === 'ArrowRight' || event.key === 'ArrowUp') next = current + 10
    else if (event.key === 'ArrowLeft' || event.key === 'ArrowDown') next = current - 10
    else if (event.key === 'Home') next = 0
    else if (event.key === 'End') next = 100
    else if (event.key === 'Backspace' || event.key === 'Delete') {
      event.preventDefault()
      onchange?.(null)
      return
    } else return
    event.preventDefault()
    onchange?.(Math.min(Math.max(next, 0), 100))
  }

  function starsOf(score: number): number {
    return score / 20
  }
</script>

<!-- Two wrappers around one drawing.
     A read-only row is an image of a score and an editable one is a slider,
     and those are different elements to a screen reader and to the a11y
     linter alike -- a `role` switched by a prop is an element that is
     sometimes focusable and sometimes not, which is exactly the thing the
     rule about non-interactive tab stops exists to catch. The stars
     themselves are a snippet, so there is still only one of them. -->
{#snippet row()}
  {#each Array.from({ length: MAX_STARS }, (_, i) => i + 1) as star (star)}
    {@const fill = starFill(fromStars(shown), star)}
    <span
      class="star"
      class:empty={fill === 0}
      style="--fill: {fill * 100}%; --size: {size}px"
      onmousemove={readonly ? undefined : (e) => (hovered = positionAt(e, star))}
      onclick={readonly ? undefined : (e) => commit(positionAt(e, star))}
      role="presentation"
    >
      <!-- Two copies, one clipped. Drawing a half star as a different glyph
           would break the row's rhythm; clipping the same one keeps it. -->
      <span class="back"><Icon name="star" {size} /></span>
      <span class="front"><Icon name="star" {size} filled /></span>
    </span>
  {/each}
  {#if showNumber && value !== null}
    <span class="number">{ratingLabel(value)}</span>
  {/if}
{/snippet}

{#if readonly}
  <div
    class="rating"
    class:muted
    role="img"
    aria-label={ratingTitle(value)}
    title={ratingTitle(value)}
  >
    {@render row()}
  </div>
{:else}
  <div
    class="rating on"
    class:muted
    role="slider"
    aria-label={label}
    aria-valuenow={value === null ? undefined : starsOf(value)}
    aria-valuemin={0}
    aria-valuemax={MAX_STARS}
    aria-valuetext={ratingTitle(value)}
    title="{ratingTitle(value)} — click to set, Backspace to clear"
    tabindex="0"
    onkeydown={onKeydown}
    onmouseleave={() => (hovered = null)}
  >
    {@render row()}
  </div>
{/if}

<style>
  .rating {
    display: inline-flex;
    align-items: center;
    gap: 1px;
    color: #e0a92b;
    border-radius: var(--radius-sm);
  }
  /* Somebody else's score: quieter than yours, but still a score.
     Flattening it to the plain faint grey made the filled stars and the
     empty ones the same colour at 11px, so the row said nothing at all --
     which is worse than not drawing it. Keeping most of the hue and taking
     the chroma down leaves the fill readable and still unmistakably not
     the gold row above it. */
  .rating.muted {
    color: color-mix(in oklab, #e0a92b 45%, var(--fg-subtle));
  }
  .rating.muted .back {
    opacity: 0.4;
  }
  .rating.on {
    cursor: pointer;
  }
  .rating:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .star {
    position: relative;
    display: block;
    width: var(--size);
    height: var(--size);
  }
  .back {
    position: absolute;
    inset: 0;
    /* Neutral, not a faded version of the fill.
       Drawing the empty stars as gold at low opacity looked right on the
       dark theme and failed on the light one: pale gold on cream is still
       gold, so four-and-a-half out of five read as five. An absence should
       be the colour of an absence. */
    color: var(--fg-faint);
    opacity: 0.55;
  }
  .front {
    position: absolute;
    inset: 0;
    /* The half-star clip: hide everything past `--fill` from the right, so
       one glyph draws every position between empty and full. */
    clip-path: inset(0 calc(100% - var(--fill)) 0 0);
  }
  .rating.on .star:hover .back {
    opacity: 0.7;
  }

  .number {
    margin-left: var(--sp-1);
    font-size: var(--text-xs);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--fg-muted);
  }
</style>
