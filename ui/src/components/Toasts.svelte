<!--
  The toast stack: the in-window half of the notification service.

  It sits outside the screen switch in `App.svelte`, above everything, and
  that placement is the whole design. A notification is a fact about the
  application, not about whichever of the three apps happens to be open, and
  it must still arrive on the lock screen and the error screen -- those are
  exactly the moments something is going wrong.

  Distinct from `Notices.svelte`, which is a banner and stays. The two are
  not competing: a notice is a *condition* that is true right now and is
  displayed for as long as it holds -- a conflict awaiting a decision, a
  read-only vault -- so it takes space in the layout and pushes the app down.
  A toast is an *event* that has happened, so it floats over the layout and
  leaves. Putting an event in the banner would make the window jump under
  someone's cursor; putting a condition in a toast would let it expire while
  still being true.
-->
<script lang="ts">
  import { notify } from '../lib/notify.svelte'
  import type { NotifyLevel, Toast } from '../lib/notify-policy'
  import Icon from './Icon.svelte'
  import type { IconName } from '../lib/icons'

  const ICON: Record<NotifyLevel, IconName> = {
    info: 'info',
    success: 'tick',
    warning: 'alert',
    error: 'alert',
  }

  // Split by how much of an interruption the level is worth, because the two
  // groups are two live regions -- see the markup below.
  const urgent = $derived(notify.toasts.filter((t) => t.level === 'error' || t.level === 'warning'))
  const calm = $derived(notify.toasts.filter((t) => t.level === 'info' || t.level === 'success'))
</script>

<!--
  Two live regions, both present from the first paint and empty most of the
  time. That emptiness is the point: a region announces what changes *inside*
  it, so it has to be on the page before the change arrives. Putting
  `aria-live` on the toast itself -- the element being inserted -- is the
  usual way this is got wrong, and it announces nothing.

  Two rather than one because politeness is a property of the region, not of
  what goes in it. `assertive` interrupts whatever is being read, which is
  right for "your journal is not being saved" and completely wrong for a tick
  saying a calendar refreshed; a success toast that cuts someone off
  mid-sentence is what gets screen readers turned off.

  The cost is that the two groups are stacked rather than strictly
  chronological, so an error always sits below the chatter. That is the
  better end of the trade: the corner of the screen is where the eye lands,
  and the sticky one belongs there.
-->
<div class="stack">
  <div class="group" role="log" aria-live="polite" aria-label="Notifications">
    {#each calm as toast (toast.id)}
      {@render row(toast)}
    {/each}
  </div>
  <div class="group" role="log" aria-live="assertive" aria-label="Alerts">
    {#each urgent as toast (toast.id)}
      {@render row(toast)}
    {/each}
  </div>
</div>

{#snippet row(toast: Toast)}
  <div class="toast {toast.level}">
    <span class="mark"><Icon name={ICON[toast.level]} size={17} /></span>
    <div class="text">
      <strong>{toast.title}</strong>
      {#if toast.body}<span>{toast.body}</span>{/if}
    </div>
    {#if toast.action}
      <button class="btn btn-primary" onclick={() => notify.act(toast)}>
        {toast.action.label}
      </button>
    {/if}
    <button class="dismiss" onclick={() => notify.dismiss(toast.id)} aria-label="Dismiss">
      <Icon name="close" size={14} />
    </button>
  </div>
{/snippet}

<style>
  .stack {
    position: fixed;
    /* Clear of the window controls at the top and of the capture line at
       the bottom left, which is where the todo app puts the one input
       somebody may be typing into while these arrive. */
    right: var(--sp-4);
    bottom: var(--sp-4);
    z-index: 90;
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    /* The stack is a fixed overlay across the whole window; without this it
       would swallow clicks on everything behind it. The toasts themselves
       take their pointer events back. */
    pointer-events: none;
    width: min(380px, calc(100vw - var(--sp-8)));
  }

  /* The two regions are structural, not visual: they lay their toasts out
     exactly as the stack itself would, so splitting for the sake of
     politeness costs nothing on screen. An empty one is zero-height and
     transparent -- it leaves a gap's worth of nothing, in a stack that does
     not take pointer events. */
  .group {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }

  .toast {
    pointer-events: auto;
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
    padding: var(--sp-3) var(--sp-3) var(--sp-3) var(--sp-4);
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: var(--shadow-lg);
    font-size: var(--text-sm);
    animation: enter 160ms ease-out;
  }

  .text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    flex: 1;
  }
  .text strong {
    font-weight: 550;
    /* A title is one line of summary. Long ones wrap rather than being cut,
       because the interesting half of "could not write to /Volumes/..." is
       the end of it. */
    overflow-wrap: anywhere;
  }
  .text span {
    color: var(--fg-muted);
    overflow-wrap: anywhere;
  }

  /* The level is carried by a coloured mark rather than by tinting the whole
     card. A stack of four fully-tinted rectangles floating over the journal
     is a lot of colour for what is usually one piece of news, and the mark
     is enough to tell them apart at a glance. */
  .mark {
    display: flex;
    padding-top: 1px;
    color: var(--fg-subtle);
  }
  .error .mark,
  .warning .mark {
    color: var(--danger);
  }
  .success .mark {
    color: var(--accent);
  }

  /* Except for an error, which does get the tint: it is the one level that
     stays until dismissed, and it should not be mistakeable for chatter. */
  .error {
    background: color-mix(in oklab, var(--danger) 10%, var(--bg-raised));
    border-color: color-mix(in oklab, var(--danger) 28%, var(--border));
  }

  .dismiss {
    display: flex;
    padding: var(--sp-1);
    margin: -2px -2px 0 0;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg-faint);
    cursor: pointer;
  }
  .dismiss:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  @keyframes enter {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
  }

  /* A message that slides in from the corner of the eye is the last thing
     someone who asked for less motion needs. */
  @media (prefers-reduced-motion: reduce) {
    .toast {
      animation: none;
    }
  }
</style>
