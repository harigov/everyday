<script lang="ts">
  // How loud the microphone is: a few bars, each one a reading, the newest on
  // the right. Only `transform` and `opacity` move, so a reading twenty times
  // a second is the compositor's work and not a repaint of the card -- and
  // nothing else on the card reads the levels, so nothing else is redrawn.
  // Each reading is drawn as it is, with no easing between them: an eased
  // bar kept the card composited at the display's rate for the whole of a
  // recording, for a smoothing nobody could see at twenty readings a second.
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
  }

  /* Nothing said lately: the whole meter breathes, one layer, instead of
     sitting still -- the microphone is open, and the card should say so. A
     few breaths and it rests, ending bright: a meeting can be quiet for an
     hour, and the bars are there all along to say the microphone is open. */
  .meter.quiet {
    animation: pc-breathe 2s ease-in-out 3.5;
  }

  @media (prefers-reduced-motion: reduce) {
    .meter.quiet {
      animation: none;
    }
  }
</style>
