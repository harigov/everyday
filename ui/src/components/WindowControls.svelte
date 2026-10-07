<script lang="ts">
  // Minimise, maximise and close, for the desktops where the window has no
  // title bar of its own -- see `lib/window.svelte.ts`. Nothing on macOS,
  // which keeps the system's.

  import { windowControls as win } from '../lib/window.svelte'
  import Icon from './Icon.svelte'

  $effect(() => win.watch())
</script>

{#if win.drawn}
  <div class="controls">
    <button onclick={() => win.minimize()} aria-label="Minimise" title="Minimise">
      <Icon name="minus" size={15} weight={1.6} />
    </button>
    <button
      onclick={() => win.toggleMaximize()}
      aria-label={win.maximized ? 'Restore' : 'Maximise'}
      title={win.maximized ? 'Restore' : 'Maximise'}
    >
      <Icon name={win.maximized ? 'restore' : 'maximize'} size={15} weight={1.6} />
    </button>
    <button class="close" onclick={() => win.close()} aria-label="Close" title="Close">
      <Icon name="close" size={15} weight={1.6} />
    </button>
  </div>
{/if}

<style>
  .controls {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
  }
  button {
    display: grid;
    place-items: center;
    width: 30px;
    height: 28px;
    border-radius: var(--radius-sm);
    color: var(--fg-subtle);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  button:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .close:hover {
    background: var(--danger);
    color: #fff;
  }
</style>
