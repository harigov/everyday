<script lang="ts">
  // What the assistant is waiting on you for, at the top of the Overview.
  //
  // Everything it prepared and did not do is a proposal -- a task, time on
  // the calendar, a note, a routine, a skill, a memory, mail to send -- and
  // each app draws its own where the record would go: a task at the foot of
  // its list, a block on the grid. That is the right place to answer one
  // while you are there, and the wrong place to find them all: nothing said
  // how many were waiting or where. So they are gathered here as well, with
  // the same two answers, beside the other thing that waits on a word from
  // you -- what it has noticed about you and wants to know is true.
  //
  // Drawn only when something is waiting, so a page with nothing to answer
  // looks the way it was arranged. Not a card in the grid: it would have to
  // be added, could be removed, and would sit wherever it was put; this is
  // the one part of the page that is about now.

  import { SvelteSet } from 'svelte/reactivity'
  import { assistant } from '../lib/assistant.svelte'
  import { groupMemories, PROPOSAL_KIND_LABELS } from '../lib/dream'
  import { panels } from '../lib/panels.svelte'
  import { proposals } from '../lib/proposals.svelte'
  import { app } from '../lib/state.svelte'
  import { PROPOSAL_KINDS } from '../lib/types'
  import type { Proposal, ProposalKind } from '../lib/types'
  import Icon from './Icon.svelte'
  import ProposalGhost from './ProposalGhost.svelte'

  // Fresh every time the Overview opens: a proposal a dream made overnight
  // is what this is for, and a list loaded yesterday would not have it. Only
  // what is drawn here, and only where there is an assistant to have made it.
  if (proposals.enabled) {
    void proposals.refresh()
    void assistant.refreshMemories()
  }

  /** Rows a group shows before it folds the rest behind "more". */
  const FOLD = 4

  /** Groups unfolded to show every row. */
  const unfolded = new SvelteSet<ProposalKind | 'noticed'>()
  let hidden = $state(false)

  const groups = $derived(
    PROPOSAL_KINDS.map((kind) => ({ kind, rows: proposals.forKind(kind) })).filter(
      (g) => g.rows.length > 0,
    ),
  )
  const noticed = $derived(groupMemories(assistant.memories).noticed)
  const count = $derived(proposals.pending.length + noticed.length)

  function shown<T>(key: ProposalKind | 'noticed', rows: T[]): T[] {
    return unfolded.has(key) ? rows : rows.slice(0, FOLD)
  }

  // What is drawn here has been looked at, the rule every other place a
  // proposal is drawn follows -- and what clears it from the app bar's count.
  $effect(() => {
    if (hidden) return
    const drawn = groups.flatMap((g) => shown(g.kind, g.rows))
    if (drawn.some((p) => !p.seen)) void proposals.markSeen(drawn)
  })

  /** Open a proposal's record where it lives, in its own app's editor. */
  function open(p: Proposal) {
    switch (p.kind) {
      case 'task':
        void app.goTo('todo')
        break
      case 'block':
        void app.goTo('calendar')
        break
      case 'note':
        void app.goTo('notes')
        break
      case 'mail':
        void app.goTo('mail')
        break
      case 'memory':
        panels.openSettings('memory')
        break
      case 'routine':
        panels.openSettings('routines')
        break
      case 'skill':
        panels.openSettings('skills')
        break
    }
  }
</script>

{#if proposals.enabled && count > 0}
  <section class="attention" aria-label="Needs attention">
    <header>
      <h2>Needs attention</h2>
      <span class="count">{count}</span>
      <span class="spacer"></span>
      <button class="link" onclick={() => panels.openSettings('proposals')}>
        Recently answered
      </button>
      <button
        class="fold"
        aria-expanded={!hidden}
        aria-label={hidden ? 'Show what needs attention' : 'Hide what needs attention'}
        onclick={() => (hidden = !hidden)}
      >
        <span class:down={!hidden}><Icon name="chevron" size={13} /></span>
      </button>
    </header>

    {#if !hidden}
      <div class="groups">
        {#each groups as g (g.kind)}
          <div class="group">
            <h3>{PROPOSAL_KIND_LABELS[g.kind]}</h3>
            {#each shown(g.kind, g.rows) as p (p.id)}
              <ProposalGhost
                proposal={p}
                confirm={p.kind === 'memory'}
                color="var(--accent)"
                onopen={() => open(p)}
              />
            {/each}
            {#if g.rows.length > FOLD && !unfolded.has(g.kind)}
              <button class="link more" onclick={() => unfolded.add(g.kind)}>
                {g.rows.length - FOLD} more
              </button>
            {/if}
          </div>
        {/each}

        {#if noticed.length > 0}
          <div class="group">
            <h3>What it has noticed about you</h3>
            {#each shown('noticed', noticed) as memory (memory.id)}
              <div class="noticed">
                <span class="text">{memory.text}</span>
                <button
                  class="answer yes"
                  onclick={() => void assistant.answerNoticed(memory.id, 'confirmed')}
                >
                  <Icon name="tick" size={13} /> True
                </button>
                <button
                  class="answer"
                  onclick={() => void assistant.answerNoticed(memory.id, 'rejected')}
                >
                  <Icon name="close" size={13} /> Not true
                </button>
              </div>
            {/each}
            {#if noticed.length > FOLD && !unfolded.has('noticed')}
              <button class="link more" onclick={() => unfolded.add('noticed')}>
                {noticed.length - FOLD} more
              </button>
            {/if}
          </div>
        {/if}
      </div>
    {/if}
  </section>
{/if}

<style>
  .attention {
    margin-bottom: var(--sp-4);
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-left: 3px solid var(--accent);
    border-radius: var(--radius-lg);
    background: var(--bg-raised);
  }

  header {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }

  h2 {
    margin: 0;
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }

  .count {
    padding: 0 6px;
    border-radius: 999px;
    background: var(--accent);
    color: #fff;
    font-size: var(--text-xs);
    font-weight: 650;
    font-variant-numeric: tabular-nums;
  }

  .spacer {
    flex: 1;
  }

  .fold {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .fold:hover {
    background: var(--bg-hover);
    color: var(--fg-muted);
  }
  /* The icon set's one chevron points right; an open section points down. */
  .down {
    display: grid;
    place-items: center;
    rotate: 90deg;
  }

  .groups {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 22rem), 1fr));
    gap: var(--sp-4);
    margin-top: var(--sp-3);
  }

  .group {
    display: grid;
    align-content: start;
    gap: var(--sp-2);
    min-width: 0;
  }

  h3 {
    margin: 0;
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-weight: 600;
  }

  .link {
    color: var(--accent);
    font-size: var(--text-sm);
  }
  .link:hover {
    text-decoration: underline;
  }

  .more {
    justify-self: start;
    font-size: var(--text-xs);
  }

  .noticed {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-2);
    border: 1px dashed var(--border-strong);
    border-radius: var(--radius);
  }

  .noticed .text {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
  }

  /* The same two pills a proposal answers with -- see `ProposalGhost` -- so
     "True" reads as the accept it is. */
  .answer {
    display: inline-flex;
    flex: none;
    align-items: center;
    gap: 3px;
    padding: 2px 10px 2px var(--sp-2);
    border: 1px solid var(--border-strong);
    border-radius: 999px;
    background: var(--bg-raised);
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }
  .answer:hover {
    color: var(--fg);
    background: var(--bg-hover);
  }
  .answer.yes:hover {
    color: var(--fg-on-accent);
    background: var(--accent);
    border-color: var(--accent);
  }
</style>
