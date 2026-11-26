<script lang="ts">
  // People, one to a row: their face, their name with a word for the
  // organiser and for the person themself, who they are in a line, and their
  // answer where they were invited. The first few show; the rest wait under
  // "More" with the card's other folded parts.
  import Avatar from '../Avatar.svelte'
  import { peopleShown, PEOPLE_SHOWN, rsvpLook } from '../cards'
  import type { PanelBlock } from '../panel'

  let {
    block,
    open,
    ontoggle,
  }: {
    block: Extract<PanelBlock, { type: 'people' }>
    /** The card's folded parts are showing. */
    open: boolean
    ontoggle: () => void
  } = $props()

  const shown = $derived(peopleShown(block.people, open))
  const hidden = $derived(open ? 0 : Math.max(0, block.people.length - PEOPLE_SHOWN))
</script>

<section class="people">
  <h3 class="pc-label">
    <span>{block.heading ?? 'People'}</span>
    {#if hidden > 0}
      <button type="button" class="pc-aside more" onclick={ontoggle}>+{hidden} more</button>
    {/if}
  </h3>
  <ul>
    {#each shown as person, i (i)}
      {@const answer = rsvpLook(person.status)}
      <li>
        <Avatar {person} size={30} badge={answer !== null} />
        <span class="words">
          <span class="name">
            <span class="named">{person.name}</span>
            {#if person.organizer}<span class="tag">Organiser</span>{/if}
            {#if person.you}<span class="tag">You</span>{/if}
          </span>
          {#if person.detail}
            <span class="detail">{person.detail}</span>
          {:else if person.email && person.email !== person.name}
            <span class="detail">{person.email}</span>
          {/if}
        </span>
        {#if answer && answer.mark !== 'waiting'}
          <span class="answer {answer.mark}">{answer.word}</span>
        {/if}
      </li>
    {/each}
  </ul>
</section>

<style>
  ul {
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  li {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }

  .words {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .name {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    font-size: var(--text-sm);
    font-weight: 600;
  }

  .named {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tag {
    flex: none;
    padding: 0 6px;
    border-radius: var(--radius-pill);
    background: var(--bg-sunken);
    box-shadow: inset 0 0 0 1px var(--border);
    font-size: 11px;
    font-weight: 600;
    line-height: 17px;
    color: var(--fg-muted);
  }

  .detail {
    font-size: var(--text-xs);
    line-height: var(--leading-snug);
    color: var(--fg-muted);
    overflow: hidden;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow-wrap: anywhere;
  }

  .answer {
    flex: none;
    font-size: var(--text-xs);
    font-weight: 600;
  }

  .answer.yes {
    color: color-mix(in oklab, var(--success) 75%, var(--fg));
  }

  .answer.no {
    color: color-mix(in oklab, var(--danger) 75%, var(--fg));
  }

  .answer.maybe {
    color: color-mix(in oklab, var(--warning) 75%, var(--fg));
  }

  .more {
    padding: 0;
    border: 0;
    background: none;
    color: var(--accent);
    cursor: pointer;
  }

  .more:hover {
    text-decoration: underline;
  }
</style>
