<script lang="ts">
  // What is happening right now, as a pill with a sign that lives the way the
  // mode does -- bars moving with a voice while listening, the wing
  // breathing while a thought is on its way, a wave while speaking, the red dot of a recording,
  // a slow ripple while waiting for the person -- and under it the tool at
  // work, with the symbol of the app it touches: the mail symbol and
  // "Searching your mail…". The motion is CSS on transform and opacity, and
  // stops under reduced motion, where the word alone says it.
  import Symbol from '../../components/Symbol.svelte'
  import WingPulse from '../../components/WingPulse.svelte'
  import Glyph from '../Glyph.svelte'
  import { modeLook, toolLook } from '../cards'
  import type { PanelBlock } from '../panel'

  let { block }: { block: Extract<PanelBlock, { type: 'activity' }> } = $props()

  const look = $derived(modeLook(block.mode))
  const tool = $derived(block.tool ? { ...block.tool, look: toolLook(block.tool.icon) } : null)
</script>

<div class="activity" role="status">
  <span class="mode" style:--mode={look.colour}>
    <span class="sign" data-motion={look.motion} aria-hidden="true">
      {#if look.motion === 'bars'}
        <i></i><i></i><i></i><i></i>
      {:else if look.motion === 'wave'}
        <i></i><i></i><i></i><i></i><i></i>
      {:else if look.motion === 'dots'}
        <WingPulse size={16} />
      {:else if look.motion === 'rec' || look.motion === 'pulse'}
        <span class="dot"></span>
      {:else if look.glyph}
        <Glyph name={look.glyph} size={14} weight={2} />
      {:else}
        <span class="dot still"></span>
      {/if}
    </span>
    <span class="word">{block.label ?? look.word}</span>
  </span>
  {#if tool}
    <span class="tool" style:--tile={tool.look.colour} title={tool.name}>
      <span class="mark"><Symbol name={tool.look.symbol} size={14} /></span>
      <span class="what">{tool.label}</span>
    </span>
  {/if}
</div>

<style>
  .activity {
    min-width: 0;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--sp-2);
  }

  .mode {
    max-width: 100%;
    height: 26px;
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 0 11px 0 9px;
    border-radius: var(--radius-pill);
    background: color-mix(in oklab, var(--mode) 13%, transparent);
    color: color-mix(in oklab, var(--mode) 70%, var(--fg));
    font-size: var(--text-sm);
    font-weight: 600;
    white-space: nowrap;
  }

  .word {
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .sign {
    flex: none;
    width: 16px;
    height: 16px;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 2px;
  }

  /* A voice heard: four bars out of step. */
  .sign[data-motion='bars'] i,
  .sign[data-motion='wave'] i {
    display: block;
    width: 2.5px;
    height: 14px;
    border-radius: 2px;
    background: var(--mode);
    transform: scaleY(0.3);
    animation: pc-bar 0.9s ease-in-out infinite;
  }

  .sign[data-motion='bars'] i:nth-child(2) {
    animation-duration: 0.7s;
    animation-delay: -0.3s;
  }

  .sign[data-motion='bars'] i:nth-child(3) {
    animation-duration: 1.05s;
    animation-delay: -0.5s;
  }

  .sign[data-motion='bars'] i:nth-child(4) {
    animation-duration: 0.8s;
    animation-delay: -0.15s;
  }

  /* Speaking: five thinner bars, a wave travelling along them. */
  .sign[data-motion='wave'] {
    gap: 1.5px;
  }

  .sign[data-motion='wave'] i {
    width: 2px;
    animation-duration: 1.2s;
  }

  .sign[data-motion='wave'] i:nth-child(2) {
    animation-delay: -1s;
  }

  .sign[data-motion='wave'] i:nth-child(3) {
    animation-delay: -0.8s;
  }

  .sign[data-motion='wave'] i:nth-child(4) {
    animation-delay: -0.6s;
  }

  .sign[data-motion='wave'] i:nth-child(5) {
    animation-delay: -0.4s;
  }

  .dot {
    position: relative;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--mode);
  }

  .dot.still {
    width: 6px;
    height: 6px;
    opacity: 0.6;
  }

  /* Recording: the red dot, breathing. */
  .sign[data-motion='rec'] .dot {
    animation: pc-breathe 1.6s ease-in-out infinite;
  }

  /* Waiting for the person: a ring spreading out from the dot. */
  .sign[data-motion='pulse'] .dot::after {
    content: '';
    position: absolute;
    inset: 0;
    border-radius: 50%;
    background: var(--mode);
    animation: pc-ripple 1.8s ease-out infinite;
  }

  .tool {
    max-width: 100%;
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  .mark {
    flex: none;
    width: 22px;
    height: 22px;
    display: grid;
    place-items: center;
    border-radius: 7px;
    background: color-mix(in oklab, var(--tile) 15%, transparent);
    color: var(--tile);
  }

  .what {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  @media (prefers-reduced-motion: reduce) {
    .sign i,
    .sign .dot,
    .sign .dot::after {
      animation: none !important;
    }

    .sign[data-motion='bars'] i,
    .sign[data-motion='wave'] i {
      transform: scaleY(0.6);
    }

    .sign[data-motion='pulse'] .dot::after {
      display: none;
    }
  }
</style>
