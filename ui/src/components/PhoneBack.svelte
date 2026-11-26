<script lang="ts">
  // The way back from something opened over its list on a phone -- see
  // `details` in `lib/phone.svelte.ts`. Drawn only on a phone; put it first
  // in the opened thing's own header. `label` names where it goes back to,
  // when the screen would not otherwise say ("Inbox", "Today").

  import { phone } from '../lib/phone.svelte'
  import Icon from './Icon.svelte'

  interface Props {
    onclick: () => void
    label?: string
  }
  let { onclick, label }: Props = $props()
</script>

{#if phone.on}
  <button class="phone-back" {onclick} aria-label={label ? `Back to ${label}` : 'Back'}>
    <span class="chevron"><Icon name="chevron" size={20} weight={2} /></span>
    {#if label}<span class="label">{label}</span>{/if}
  </button>
{/if}

<style>
  .phone-back {
    display: inline-flex;
    flex: none;
    align-items: center;
    gap: 2px;
    min-width: 44px;
    height: 44px;
    padding: 0 var(--sp-2) 0 var(--sp-1);
    border-radius: var(--radius);
    color: var(--accent);
    font-weight: 600;
  }
  .phone-back:active {
    background: var(--bg-hover);
  }
  .chevron {
    display: grid;
    rotate: 180deg;
  }
  .label {
    max-width: 40vw;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
</style>
