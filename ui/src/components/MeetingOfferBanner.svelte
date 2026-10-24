<!--
  "Take notes for Design sync?" -- the offer the desktop shell raises when a
  calendar meeting with other people in it is about to start, or when
  another app (Zoom, Meet in a browser, ...) takes the microphone and offers
  `ask` is on (see `MeetingsPanel`'s "When calls start"). Automatic
  recordings do not come through here at all: those raise a small toast
  instead, from `meetings.svelte.ts`'s own `onMeetingOffer` handler, since
  nothing is being asked.

  The shell owns each offer and raises a desktop notification and a tray item
  for it as well; this banner is the third place it can be answered, and
  answers by asking the shell to (`meetings.answerOffer`) so all three settle
  together. An offer made before meeting notes were set up reads "Set up
  meeting notes" instead, and stays until the person has either set them up
  and taken it, or answered it.

  Styled like `Notices.svelte`'s banner rather than a toast, on purpose: this
  is a decision with several answers, not an event to glance at and let scroll
  away, and it sits in the same place above the panes so it is not missed
  because the wrong app happened to be open.
-->
<script lang="ts">
  import type { MeetingOfferAnswer } from '../lib/api'
  import { dayHeading, timeOfDay } from '../lib/format'
  import { meetings } from '../lib/meetings.svelte'
  import { panels } from '../lib/panels.svelte'

  const offer = $derived(meetings.offers[0] ?? null)
  const more = $derived(meetings.offers.length - 1)
  const ready = $derived(offer ? meetings.offerReady(offer) : true)
  let takeError = $state<string | null>(null)

  // Reset the error once a different offer is showing -- a call refused for
  // this event should not still be on screen once the offer it belonged to
  // is gone (accepted, dismissed, or replaced by a newer one).
  $effect(() => {
    void offer?.key
    takeError = null
  })

  // An offer that says "not set up" flips to takeable when the person
  // finishes setting up without leaving, but only if the settings are here
  // to say so -- the store does not load them until something asks.
  $effect(() => {
    if (offer && !offer.ready) void meetings.load()
  })

  const microphone = $derived(offer?.source === 'microphone')
  const appName = $derived(offer?.app ?? 'Another app')
  /** An offer with a calendar meeting behind it, by either route. */
  const hasEvent = $derived(offer != null && offer.start != null && offer.end != null)

  const headline = $derived(offer && (hasEvent || !microphone) ? offer.title : null)
  const detail = $derived.by(() => {
    if (!offer) return ''
    if (microphone) {
      const using = `${appName} is using your microphone`
      return offer.calendarName ? `${using} · ${offer.calendarName}` : `${using}.`
    }
    let when: string | null = null
    if (offer.start && offer.end) {
      const start = new Date(offer.start)
      when = `${dayHeading(start)}, ${timeOfDay(start)}–${timeOfDay(new Date(offer.end))}`
    }
    return [offer.calendarName, when].filter(Boolean).join(' · ')
  })

  async function answer(which: MeetingOfferAnswer) {
    if (!offer) return
    takeError = null
    try {
      // Dropped from `meetings.offers` only once the shell has actually
      // answered -- see `answerOffer`'s own doc. A failed start leaves the
      // offer (and now the error below) on screen instead of vanishing
      // along with it.
      await meetings.answerOffer(offer, which)
    } catch (e) {
      takeError = e instanceof Error ? e.message : String(e)
    }
  }
</script>

{#if offer}
  {@const failure = takeError ?? meetings.offerErrors[offer.key] ?? null}
  <div class="notice offer" role="alert">
    <div class="text">
      {#if headline}
        <strong>Take notes for {headline}?</strong>
      {:else}
        <strong>Taking a call? Take notes</strong>
      {/if}
      <span>{detail}</span>
      {#if !ready}<span>Meeting notes aren’t set up yet.</span>{/if}
      {#if more > 0}<span class="more">+{more} more</span>{/if}
      {#if failure}<p class="error">{failure}</p>{/if}
    </div>
    <div class="actions">
      <button class="btn quiet" disabled={meetings.starting} onclick={() => void answer('never')}>
        {microphone ? 'Don’t ask when the mic turns on' : 'Never for this meeting'}
      </button>
      <button class="btn" disabled={meetings.starting} onclick={() => void answer('notNow')}>
        Not now
      </button>
      {#if !ready}
        <button class="btn btn-primary" onclick={() => panels.openSettings('meetings')}>
          Set up meeting notes
        </button>
      {:else if offer.joinUrl}
        <button class="btn" disabled={meetings.starting} onclick={() => void answer('take')}>
          Take notes
        </button>
        <button
          class="btn btn-primary"
          disabled={meetings.starting}
          onclick={() => void answer('join')}
        >
          {meetings.starting ? 'Starting…' : 'Join & take notes'}
        </button>
      {:else}
        <button
          class="btn btn-primary"
          disabled={meetings.starting}
          onclick={() => void answer('take')}
        >
          {meetings.starting ? 'Starting…' : 'Take notes'}
        </button>
      {/if}
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
  .text .more {
    color: var(--fg-subtle);
    font-size: var(--text-xs);
  }
  .quiet {
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }
  .actions {
    display: flex;
    gap: var(--sp-2);
    flex-shrink: 0;
  }
</style>
