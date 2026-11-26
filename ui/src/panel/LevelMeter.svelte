<script lang="ts">
  // How loud the microphone is: a few bars, each one a reading, the newest on
  // the right. Only `transform` and `opacity` move, so a reading twenty times
  // a second is the compositor's work and not a repaint of the card -- and
  // nothing else on the card reads the levels, so nothing else is redrawn.
  import { barScale, isQuiet } from './cards'

  let { levels }: { levels: readonly number[] } = $props()
</script>

<span class="meter" class:quiet={isQuiet(levels)} aria-hidden="true">
  {#each levels as level, i (i)}
    <i style:transform="scaleY({barScale(level)})"></i>
  {/each}
</span>

<style>
  .meter {
    flex: none;
    height: 24px;
    display: flex;
    align-items: center;
    gap: 2px;
  }

  .meter i {
    display: block;
    width: 3px;
    height: 100%;
    border-radius: 2px;
    background: var(--meter, var(--kind));
    transition: transform 70ms linear;
  }

  /* Nothing said lately: the whole meter breathes, one layer, instead of
     sitting still -- the microphone is open, and the card should say so. */
  .meter.quiet {
    animation: pc-breathe 2s ease-in-out infinite;
  }

  @media (prefers-reduced-motion: reduce) {
    .meter.quiet {
      animation: none;
    }

    .meter i {
      transition: none;
    }
  }
</style>
