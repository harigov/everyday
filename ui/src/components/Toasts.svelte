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
  import type { NotifyLevel } from '../lib/notify-policy'
  import Icon from './Icon.svelte'
  import type { IconName } from '../lib/icons'

  const ICON: Record<NotifyLevel, IconName> = {
    info: 'info',
    success: 'tick',
    warning: 'alert',
    error: 'alert',
  }
</script>

<!--
  One live region for the whole stack rather than one per toast, because a
  region announces what changes *inside* it -- a region that is itself added
  to the page announces nothing, which is the usual way this is got wrong.

  `assertive` is reserved for the levels that mean something did not happen.
  A success toast interrupting someone mid-sentence to say their calendar
  refreshed is precisely the behaviour that gets screen readers turned off.
-->
<div class="stack" role="region" aria-label="Notifications">
  {#each notify.toasts as toast (toast.id)}
    <div
      class="toast {toast.level}"
      role={toast.level === 'error' || toast.level === 'warning' ? 'alert' : 'status'}
      aria-live={toast.level === 'error' || toast.level === 'warning' ? 'assertive' : 'polite'}
    >
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
  {/each}
</div>

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
