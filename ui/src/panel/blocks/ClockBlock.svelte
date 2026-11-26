<script lang="ts">
  // A running clock -- how long a meeting has been recorded -- counted here
  // from `sinceMs`, so it ticks every second without the shell sending the
  // whole stack each time. The timer is set for just after each whole second
  // of the count, so the digits turn over together rather than drifting.
  import { clockText } from '../cards'
  import type { PanelBlock } from '../panel'

  let { block }: { block: Extract<PanelBlock, { type: 'clock' }> } = $props()

  let now = $state(Date.now())

  $effect(() => {
    const since = block.sinceMs
    let timer: ReturnType<typeof setTimeout> | undefined
    const tick = () => {
      now = Date.now()
      const into = (((now - since) % 1000) + 1000) % 1000
      timer = setTimeout(tick, 1000 - into + 15)
    }
    tick()
    return () => clearTimeout(timer)
  })

  const text = $derived(clockText(block.sinceMs, now))
</script>

<p class="clock" role="timer" aria-label="{block.label ?? 'Running for'} {text}">
  {#if block.label}<span class="label">{block.label}</span>{/if}
  <time>{text}</time>
</p>

<style>
  .clock {
    flex: none;
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 1px;
    margin-left: auto;
    line-height: 1;
  }

  .label {
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg-subtle);
  }

  time {
    font-size: var(--text-lg);
    font-weight: 620;
    font-variant-numeric: tabular-nums;
    letter-spacing: -0.01em;
    color: var(--fg);
  }
</style>
