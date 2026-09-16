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
  let takeError = $state<string | null>(null)

  // Reset the error once a different offer is showing -- a call refused for
  // this event should not still be on screen once the offer it belonged to
  // is gone (accepted, dismissed, or replaced by a newer one).
  $effect(() => {
    void offer
    takeError = null
  })

  async function take() {
    if (!offer) return
    takeError = null
    try {
      // Dropped from `meetings.offers` only once this actually succeeds --
      // see `acceptOffer`'s own doc. A failed start leaves the offer (and
      // now the error below) on screen instead of vanishing along with it.
      await meetings.acceptOffer(offer)
    } catch (e) {
      takeError = e instanceof Error ? e.message : String(e)
    }
  }

  function notNow() {
    if (offer) void meetings.dismissOffer(offer.calendarId, offer.uid, offer.series ?? null, false)
  }

  function never() {
    if (offer) void meetings.dismissOffer(offer.calendarId, offer.uid, offer.series ?? null, true)
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
      {#if takeError}<p class="error">{takeError}</p>{/if}
    </div>
    <div class="actions">
      <button class="btn" disabled={meetings.starting} onclick={never}>
        Never for this meeting
      </button>
      <button class="btn" disabled={meetings.starting} onclick={notNow}>Not now</button>
      <button class="btn btn-primary" disabled={meetings.starting} onclick={() => void take()}>
        {meetings.starting ? 'Starting…' : 'Take notes'}
      </button>
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
  .text .error {
    margin: 0;
    color: var(--danger);
    font-size: var(--text-xs);
  }
  .actions {
    display: flex;
    gap: var(--sp-2);
    flex-shrink: 0;
  }
</style>
