<script lang="ts">
  // How loud the microphone is: a few bars, each one a reading, the newest on
  // the right. Only `transform` and `opacity` move, so a reading twenty times
  // a second is the compositor's work and not a repaint of the card.
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
    height: 28px;
    display: flex;
    align-items: center;
    gap: 2px;
  }

  .meter i {
    display: block;
    width: 3px;
    height: 100%;
    border-radius: 2px;
    background: var(--tone);
    transition: transform 70ms linear;
  }

  /* Nothing said lately: the whole meter breathes, one layer, instead of
     sitting still. */
  .meter.quiet {
    animation: breathe 2s ease-in-out infinite;
  }

  @keyframes breathe {
    0%,
    100% {
      opacity: 0.45;
    }
    50% {
      opacity: 1;
    }
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
