<script lang="ts">
  // A row that says what something is for, and opens the picker.
  //
  // The button reads as the value rather than as a control: the common case
  // is nothing at all, and a field that shouted "Purpose: none" at the top of
  // every task detail would be a nag about a thing that is optional on
  // purpose. Unset it is quiet grey text; set it wears its role's colour,
  // which is the same colour the balance chart will draw that hour in.

  import Icon from './Icon.svelte'
  import PurposePicker from './PurposePicker.svelte'
  import { dismissable } from '../lib/dismiss'
  import { purpose as store } from '../lib/purpose.svelte'
  import type { Purpose } from '../lib/types'

  interface Props {
    value: Purpose | null | undefined
    onchange: (next: Purpose | null) => void
    /** Roles only, no goals. What a subscribed calendar takes. */
    rolesOnly?: boolean
    /** What the button says when nothing is set. */
    placeholder?: string
  }

  const { value, onchange, rolesOnly = false, placeholder = 'Not filed' }: Props = $props()

  let open = $state(false)

  const label = $derived(store.describe(value))
  const set = $derived(!!value && label.roleId !== null)
</script>

<div class="field">
  <button
    class="value"
    class:set
    onclick={() => (open = !open)}
    aria-haspopup="listbox"
    aria-expanded={open}
    title={set ? `Filed under ${label.name}` : 'Say what this is for'}
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
    <div class="pop" use:dismissable={{ onaway: () => (open = false), within: '.field .value' }}>
      <PurposePicker {value} {onchange} {rolesOnly} onclose={() => (open = false)} />
    </div>
  {/if}
</div>

<style>
  .field {
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
    color: var(--text-muted);
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
    color: var(--text);
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
    color: var(--text-faint);
  }

  .pop {
    position: absolute;
    z-index: 40;
    top: calc(100% + 4px);
    left: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    box-shadow: var(--shadow-lg);
  }
</style>
