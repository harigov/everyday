<script lang="ts">
  // A row that says what something is for, and opens the picker.
  //
  // The button reads as the value rather than as a control: the common case
  // is nothing at all, and a field that shouted "Purpose: none" at the top of
  // every task detail would be a nag about a thing that is optional on
  // purpose. Unset it is quiet grey text; set it wears its area's colour,
  // which is the same colour the balance chart will draw that hour in.

  import Icon from './Icon.svelte'
  import PurposePicker from './PurposePicker.svelte'
  import { dismissable } from '../lib/dismiss'
  import { purpose as store } from '../lib/purpose.svelte'
  import type { Purpose } from '../lib/types'

  interface Props {
    value: Purpose | null | undefined
    onchange: (next: Purpose | null) => void
    /** Areas only, no goals. What a subscribed calendar takes. */
    areasOnly?: boolean
    /** What the button says when nothing is set. */
    placeholder?: string
  }

  const { value, onchange, areasOnly = false, placeholder = 'No goal or area' }: Props = $props()

  let open = $state(false)

  const label = $derived(store.describe(value))
  const set = $derived(!!value && label.areaId !== null)
</script>

<div class="purposefield">
  <button
    class="value"
    class:set
    onclick={() => (open = !open)}
    aria-haspopup="listbox"
    aria-expanded={open}
    title={set ? `Filed under ${label.name}` : 'Which goal or area of your life this is for'}
  >
    {#if set}
      <span class="swatch" style="background: {label.color}"></span>
      <span class="name">{label.name}</span>
    {:else}
      <Icon name="compass" size={13} />
      <span class="name muted">{placeholder}</span>
    {/if}
  </button>

  {#if open}
    <div
      class="pop"
      use:dismissable={{ onaway: () => (open = false), within: '.purposefield .value' }}
    >
      <PurposePicker {value} {onchange} {areasOnly} onclose={() => (open = false)} />
    </div>
  {/if}
</div>

<style>
  .purposefield {
    position: relative;
  }

  .value {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    min-height: 28px;
    padding: 0 var(--sp-2);
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg-muted);
    font: inherit;
    font-size: var(--text-sm);
    text-align: left;
    cursor: pointer;
  }

  .value:hover,
  .value:focus-visible {
    border-color: var(--border);
    background: var(--bg-raised);
  }

  .value.set {
    color: var(--fg);
  }

  .swatch {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 50%;
  }

  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .name.muted {
    color: var(--fg-faint);
  }

  .pop {
    position: absolute;
    z-index: 40;
    top: calc(100% + 4px);
    left: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--float);
    box-shadow: var(--shadow-lg);
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* A row a finger can hit, and the picker a sheet hung from the top under
     the status bar, as every sheet is: opening downward from a field in the
     middle of a page ran it off the right edge and under the bar of apps. */
  :global([data-phone]) .value {
    min-height: 44px;
    font-size: var(--text-base);
  }
  :global([data-phone]) .pop {
    position: fixed;
    z-index: 66;
    left: var(--sp-3);
    right: var(--sp-3);
    top: calc(var(--safe-top) + max(8dvh, var(--sp-4)));
    display: flex;
    flex-direction: column;
    max-height: calc(
      100dvh - var(--safe-top) - max(8dvh, var(--sp-4)) - var(--bottombar-h) - var(--safe-bottom) -
        var(--sp-3)
    );
  }
</style>
