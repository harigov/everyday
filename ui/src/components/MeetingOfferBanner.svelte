<!--
  "Take notes for Design sync?" -- the offer a detected call raises when
  `offer` is `ask` (see `MeetingsPanel`'s "When calls start"). Automatic
  recordings do not come through here at all: those raise a small toast
  instead, from `meetings.svelte.ts`'s own `onMeetingOffer` handler, since
  nothing is being asked.

  Styled like `Notices.svelte`'s banner rather than a toast, on purpose: this
  is a decision with three answers, not an event to glance at and let scroll
  away, and it sits in the same place above the panes so it is not missed
  because the wrong app happened to be open.
-->
<script lang="ts">
  import { dayHeading, timeOfDay } from '../lib/format'
  import { meetings } from '../lib/meetings.svelte'

  const offer = $derived(meetings.offers[0] ?? null)

  async function take() {
    if (offer) await meetings.acceptOffer(offer)
  }

  function notNow() {
    if (offer) void meetings.dismissOffer(offer.eventId, false)
  }

  function never() {
    if (offer) void meetings.dismissOffer(offer.eventId, true)
  }
</script>

{#if offer}
  <div class="notice offer" role="alert">
    <div class="text">
      <strong>Take notes for {offer.title}?</strong>
      <span>
        {offer.calendarName} · {dayHeading(new Date(offer.start))}, {timeOfDay(
          new Date(offer.start),
        )}–{timeOfDay(new Date(offer.end))}
      </span>
    </div>
    <div class="actions">
      <button class="btn" onclick={never}>Never for this meeting</button>
      <button class="btn" onclick={notNow}>Not now</button>
      <button class="btn btn-primary" onclick={() => void take()}>Take notes</button>
    </div>
  </div>
{/if}

<style>
  .notice {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-4);
    align-items: center;
    justify-content: space-between;
    padding: var(--sp-3) var(--sp-4);
    border-bottom: 1px solid var(--border);
    font-size: var(--text-sm);
    background: color-mix(in oklab, var(--accent) 10%, var(--bg-raised));
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .text span {
    color: var(--fg-muted);
  }
  .actions {
    display: flex;
    gap: var(--sp-2);
    flex-shrink: 0;
  }
</style>
