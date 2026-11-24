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
  import { untrack } from 'svelte'
  import { api, type MeetingOfferAnswer } from '../lib/api'
  import { dayHeading, timeOfDay } from '../lib/format'
  import { meetings } from '../lib/meetings.svelte'
  import { panels } from '../lib/panels.svelte'
  import type { MeetingBrief } from '../lib/types'
  import { settledBrief } from '../lib/meetingbrief'

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

  const counting = $derived.by(() => {
    const left = meetings.countdown
    if (!offer?.autoRecord || !left || left.key !== offer.key) return null
    return left.seconds
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

  // What to know walking in, as one quiet line under the offer: the brief the
  // service made for the meeting, when it made one. Asked for once per offer
  // -- an offer is a minute from its meeting, and the brief was prepared a
  // quarter of an hour before that -- and only for a calendar meeting, which
  // is the only kind a brief is made for. No brief, or no answer, is no line.
  let brief = $state<MeetingBrief | null>(null)
  const briefFor = $derived(offer?.calendarId && offer.uid ? offer.key : null)

  $effect(() => {
    const key = briefFor
    brief = null
    const ask = untrack(() => offer)
    if (!key || !ask?.calendarId || !ask.uid) return
    let live = true
    void api
      .meetingBrief(ask.calendarId, ask.uid)
      .then(settledBrief)
      .then((found) => {
        if (live) brief = found
      })
      .catch(() => {})
    return () => {
      live = false
    }
  })

  /** The first names of who is coming, as many as fit a line. */
  function withLine(people: MeetingBrief['people']): string {
    const names = people.map((p) => p.name.split(/\s+/)[0] ?? p.name)
    const shown = names.slice(0, 3).join(', ')
    return names.length > 3 ? `With ${shown} and ${names.length - 3} more` : `With ${shown}`
  }

  /** The brief in a line: what it is for, else who is coming, else the first agenda item. */
  const briefLine = $derived.by(() => {
    if (!brief) return null
    const about =
      brief.summary ||
      (brief.people.length > 0 ? withLine(brief.people) : (brief.agenda[0] ?? null))
    const raise = brief.talkingPoints.filter((p) => !p.done).length
    const things = raise === 1 ? '1 thing to raise' : `${raise} things to raise`
    return [about, raise > 0 ? things : null].filter(Boolean).join(' · ') || null
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
      {#if counting != null}
        <strong>Recording starts in {counting}s</strong>
      {:else if headline}
        <strong>{offer.autoRecord ? headline : `Take notes for ${headline}?`}</strong>
      {:else}
        <strong>{offer.autoRecord ? 'Start recording?' : 'Taking a call? Take notes'}</strong>
      {/if}
      <span>{detail}</span>
      {#if briefLine}<span class="brief">{briefLine}</span>{/if}
      {#if !ready}<span>Meeting notes aren’t set up yet.</span>{/if}
      {#if more > 0}<span class="more">+{more} more</span>{/if}
      {#if failure}<p class="error">{failure}</p>{/if}
    </div>
    <div class="actions">
      {#if counting != null}
        <button class="btn" disabled={meetings.starting} onclick={() => void answer('notNow')}>
          Cancel
        </button>
        <button
          class="btn btn-primary"
          disabled={meetings.starting}
          onclick={() => void answer('take')}
        >
          {meetings.starting ? 'Starting…' : 'Take notes'}
        </button>
      {:else}
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
        {:else if offer.autoRecord}
          <button
            class="btn btn-primary"
            disabled={meetings.starting}
            onclick={() => void answer('join')}
          >
            {offer.joinUrl ? 'Join and start recording' : 'Start recording'}
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
  /* Two lines at most: a brief's summary is a sentence, and the offer is a
     question that wants answering, not reading. */
  .text span.brief {
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    overflow: hidden;
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
