<script lang="ts">
  // Somebody's face: their initials on a colour that belongs to their address
  // (`face` in `cards.ts`), and, where they were invited to something, their
  // answer as a small badge at its foot -- a tick, a cross, a question, or a
  // hollow dot while it is still to come. No photographs: fetching one would
  // tell a stranger's server when the card was shown.
  import { face, rsvpLook } from './cards'
  import type { Person } from './panel'

  let {
    person,
    size = 28,
    badge = false,
  }: {
    person: Pick<Person, 'name' | 'email' | 'status'>
    size?: number
    /** Show their answer to the invitation. */
    badge?: boolean
  } = $props()

  const look = $derived(face(person))
  const answer = $derived(badge ? rsvpLook(person.status) : null)
</script>

<span
  class="face"
  style:--hue={look.hue}
  style:--size="{size}px"
  title={answer ? `${person.name}: ${answer.word}` : person.name}
>
  <span class="letters" aria-hidden="true">{look.initials}</span>
  {#if answer}
    <span class="badge {answer.mark}" aria-hidden="true">
      <svg viewBox="0 0 12 12">
        {#if answer.mark === 'yes'}
          <path d="M3.2 6.2 5.1 8l3.7-4" />
        {:else if answer.mark === 'no'}
          <path d="M4 4l4 4M8 4 4 8" />
        {:else if answer.mark === 'maybe'}
          <path d="M4.6 4.6a1.5 1.5 0 1 1 2.2 1.3c-.5.3-.8.6-.8 1.1M6 8.9v.1" />
        {/if}
      </svg>
    </span>
  {/if}
</span>

<style>
  /* The ring in the card's own fill is what keeps a row of faces that overlap
     reading as separate faces. */
  .face {
    position: relative;
    flex: none;
    display: inline-grid;
    place-items: center;
    width: var(--size);
    height: var(--size);
    border-radius: 50%;
    background: color-mix(in oklab, var(--hue) 22%, var(--float));
    box-shadow: 0 0 0 2px var(--float);
    color: color-mix(in oklab, var(--hue) 58%, var(--fg));
    font-size: calc(var(--size) * 0.38);
    font-weight: 650;
    letter-spacing: 0.01em;
    line-height: 1;
    -webkit-user-select: none;
    user-select: none;
  }

  .badge {
    position: absolute;
    right: -3px;
    bottom: -3px;
    width: 13px;
    height: 13px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    box-shadow: 0 0 0 2px var(--float);
    color: var(--float);
  }

  .badge svg {
    width: 11px;
    height: 11px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.8;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .badge.yes {
    background: var(--success);
  }

  .badge.no {
    background: var(--danger);
  }

  .badge.maybe {
    background: var(--warning);
  }

  /* Still to answer: a hollow dot, quieter than an answer. */
  .badge.waiting {
    width: 10px;
    height: 10px;
    right: -1px;
    bottom: -1px;
    background: var(--float);
    box-shadow:
      0 0 0 2px var(--float),
      inset 0 0 0 2px var(--fg-faint);
  }
</style>
