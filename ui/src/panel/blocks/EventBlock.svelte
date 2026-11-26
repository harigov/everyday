<script lang="ts">
  // A calendar event drawn as one: a tile in the calendar's colour with its
  // edge marked like an event on the calendar's grid, the day as a chip and
  // the time beside it, where it is and which service its call is on, and a
  // row of the faces of who is coming, each with their answer -- then, in a
  // line, how the answers add up and, for an invitation, the person's own.
  import Avatar from '../Avatar.svelte'
  import Glyph from '../Glyph.svelte'
  import { faces, repeats, rsvpSummary, yourAnswer } from '../cards'
  import type { PanelBlock } from '../panel'

  let {
    block,
    cardTitle,
  }: {
    block: Extract<PanelBlock, { type: 'event' }>
    /** The card's own title: the event's is left out where it says the same. */
    cardTitle: string
  } = $props()

  const people = $derived(block.people ?? [])
  const row = $derived(faces(people))
  const summary = $derived(rsvpSummary(people))
  const yours = $derived(block.rsvp ? yourAnswer(block.rsvp) : null)
</script>

<div class="pc-tile event" style:--tile="var(--app-calendar)">
  {#if !repeats(block.title, cardTitle)}
    <p class="title">{block.title}</p>
  {/if}
  <p class="when">
    {#if block.date}
      <span class="date pc-ink" style:--ink="var(--app-calendar)">{block.date}</span>
    {/if}
    <span class="time">{block.when}</span>
  </p>
  {#if block.place || block.join || block.calendar}
    <ul class="facts">
      {#if block.place}
        <li><Glyph name="place" size={13} /><span>{block.place}</span></li>
      {/if}
      {#if block.join}
        <li><Glyph name="join" size={13} /><span>{block.join}</span></li>
      {/if}
      {#if block.calendar}
        <li><span class="swatch" aria-hidden="true"></span><span>{block.calendar}</span></li>
      {/if}
    </ul>
  {/if}
  {#if row.shown.length > 0}
    <div class="who">
      <span class="faces" role="list" aria-label="Who is invited">
        {#each row.shown as person, i (i)}
          <span role="listitem">
            <Avatar {person} size={26} badge={person.status !== undefined} />
          </span>
        {/each}
        {#if row.more > 0}<span class="more" role="listitem">+{row.more}</span>{/if}
      </span>
      {#if summary}<span class="summary">{summary}</span>{/if}
    </div>
  {/if}
  {#if yours}
    <p class="yours {yours.mark}">{yours.words}</p>
  {/if}
</div>

<style>
  .event {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-left: 15px;
  }

  /* The edge an event has on the calendar's own grid. */
  .event::before {
    content: '';
    position: absolute;
    left: 0;
    top: 10px;
    bottom: 10px;
    width: 3px;
    border-radius: 0 3px 3px 0;
    background: var(--tile);
  }

  .title {
    font-size: var(--text-base);
    font-weight: 600;
    line-height: var(--leading-tight);
    overflow-wrap: anywhere;
  }

  .when {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }

  .date {
    padding: 1px 8px;
    border-radius: var(--radius-pill);
    background: color-mix(in oklab, var(--tile) 16%, transparent);
    font-size: var(--text-xs);
    font-weight: 650;
    line-height: 18px;
  }

  .time {
    font-size: var(--text-base);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  .facts {
    padding: 0;
    list-style: none;
    display: flex;
    flex-wrap: wrap;
    gap: 2px 14px;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }

  .facts li {
    min-width: 0;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--fg-muted);
  }

  .facts li :global(svg) {
    color: var(--fg-subtle);
  }

  .facts span {
    overflow-wrap: anywhere;
  }

  .swatch {
    flex: none;
    width: 8px;
    height: 8px;
    margin: 0 2.5px;
    border-radius: 50%;
    background: var(--tile);
  }

  .who {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    margin-top: 4px;
    min-width: 0;
  }

  /* Faces overlap a little, each ringed in the card's fill (`Avatar`). */
  .faces {
    flex: none;
    display: flex;
    align-items: center;
    padding-left: 2px;
  }

  .faces > :not(:first-child) {
    margin-left: -5px;
  }

  .more {
    display: inline-grid;
    place-items: center;
    min-width: 26px;
    height: 26px;
    padding: 0 6px;
    border-radius: var(--radius-pill);
    background: var(--bg-sunken);
    box-shadow: 0 0 0 2px var(--float);
    font-size: var(--text-xs);
    font-weight: 650;
    color: var(--fg-muted);
  }

  .summary {
    min-width: 0;
    font-size: var(--text-xs);
    color: var(--fg-muted);
  }

  .yours {
    align-self: flex-start;
    padding: 2px 9px;
    border-radius: var(--radius-pill);
    font-size: var(--text-xs);
    font-weight: 650;
    line-height: 18px;
  }

  .yours.yes {
    background: color-mix(in oklab, var(--success) 14%, transparent);
    color: color-mix(in oklab, var(--success) 70%, var(--fg));
  }

  .yours.no {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: color-mix(in oklab, var(--danger) 70%, var(--fg));
  }

  .yours.maybe {
    background: color-mix(in oklab, var(--warning) 14%, transparent);
    color: color-mix(in oklab, var(--warning) 70%, var(--fg));
  }

  .yours.waiting {
    background: color-mix(in oklab, var(--accent) 12%, transparent);
    color: color-mix(in oklab, var(--accent) 70%, var(--fg));
  }
</style>
