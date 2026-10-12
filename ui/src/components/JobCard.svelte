<script lang="ts">
  // One background job: what it is for, where it stands, and what can be done
  // about it.
  //
  // Drawn in two places from this one component. In the thread it stands in
  // for the `start_research` call that began the job (`compact`, so that it
  // sits among tool cards without shouting), and in Settings → "What it did"
  // it is a row of the list of background work, at the size the routine runs
  // beside it are drawn. See `docs/plans/jobs.md`, "The interface".
  //
  // The status line is the part that moves. It is a live region, so somebody
  // who cannot see the card still hears "Finished" arrive; the store throttles
  // how often a running job's progress changes, so it does not talk over them.
  //
  // What it opens lives elsewhere, and this only knows how to get there: the
  // note in the Notes app, and the job's own transcript as a conversation in
  // the Assistant app -- the same two moves a routine run's "step by step"
  // and the Overview's notes already make.

  import { elapsed } from '../lib/agent'
  import { agent } from '../lib/agent.svelte'
  import { relativeTime } from '../lib/format'
  import { jobs } from '../lib/jobs.svelte'
  import { notes } from '../lib/notes.svelte'
  import { panels } from '../lib/panels.svelte'
  import { app } from '../lib/state.svelte'
  import type { Job, JobKind } from '../lib/types'

  let {
    job,
    compact = false,
    onremove,
  }: {
    job: Job
    /** The thread's size: less padding, and no age or unread mark. */
    compact?: boolean
    /** Offered as Delete once the job is over. Only the list in Settings passes it. */
    onremove?: () => void
  } = $props()

  const KIND_LABELS: Record<JobKind['type'], string> = { research: 'Research' }

  /** What the job is about, in the words it was given. */
  function aboutOf(kind: JobKind): string {
    switch (kind.type) {
      case 'research':
        return kind.question
    }
  }

  const kind = $derived(KIND_LABELS[job.kind.type])
  const about = $derived(aboutOf(job.kind))
  const active = $derived(job.status === 'queued' || job.status === 'running')

  /** The first note it made. A research job's appears as soon as it exists. */
  const noteId = $derived(job.outputs.find((o) => o.type === 'note')?.noteId ?? null)

  /** Done, with a conversation waiting to hear about it, and not yet told. */
  const owed = $derived(job.status === 'done' && !job.answered && !!job.askedIn)

  /**
   * The status line, in two parts: what is true, and the detail after it.
   *
   * The detail is the part that varies -- a count of steps, a time, a reason
   * -- so the first part stays short enough to be the thing that is read.
   */
  const status = $derived.by((): { text: string; more: string } => {
    switch (job.status) {
      case 'queued':
        return { text: 'Waiting to start', more: '' }
      case 'running':
        return {
          text: job.progress.trim() || 'Working on it',
          more: job.steps > 0 ? `${job.steps} ${job.steps === 1 ? 'step' : 'steps'}` : '',
        }
      case 'done': {
        const took =
          job.startedAt && job.finishedAt
            ? `took ${elapsed(Date.parse(job.finishedAt) - Date.parse(job.startedAt))}`
            : ''
        const when = job.finishedAt ? relativeTime(job.finishedAt) : ''
        const told = owed ? 'not yet answered in the conversation' : ''
        return { text: 'Finished', more: [when, took, told].filter(Boolean).join(' · ') }
      }
      case 'failed':
        return { text: 'Could not finish', more: job.reason.trim() }
      case 'cancelled':
        return { text: 'Cancelled', more: job.reason.trim() }
    }
  })

  /** One action in flight at a time: Cancel and Answer now do not stack. */
  let pending = $state(false)
  async function run(action: () => Promise<void>) {
    if (pending) return
    pending = true
    try {
      await action()
    } finally {
      pending = false
    }
  }

  /**
   * Open the note in the Notes app.
   *
   * The same two steps every cross-app link here is: switch the app, which
   * also closes Settings, then hand the other app's store the record.
   */
  async function openNote() {
    const id = noteId
    if (!id) return
    if (await app.goTo('notes')) await notes.openNote(id)
  }

  /**
   * Show the job's own transcript: every tool it called and every word it
   * said, as the conversation it was.
   *
   * From the thread there is nothing to leave -- the thread is where the
   * transcript goes. From Settings the rail is not on screen, so it goes to
   * the Assistant app, exactly as a routine run's transcript does. Loaded
   * first because loading opens the latest conversation when none is open,
   * and the job's thread is then put on screen over the top.
   */
  async function openTranscript() {
    const thread = job.conversationId
    if (!thread) return
    if (panels.settings !== null) {
      panels.closeSettings()
      await agent.load()
      if (!(await app.goTo('assistant'))) return
    }
    await agent.openThread(thread)
  }

  const hasActions = $derived(
    active || !!noteId || !!job.conversationId || owed || (!!onremove && !active),
  )
</script>

<article
  class="job"
  class:compact
  class:unseen={!compact && !job.seen}
  class:bad={job.status === 'failed'}
  aria-label="{kind}: {about}"
>
  <header class="head">
    <span class="eyebrow">{kind}</span>
    {#if !compact}
      <span class="spacer"></span>
      <span class="dim">{relativeTime(job.createdAt)}</span>
    {/if}
  </header>

  <p class="about">{about}</p>

  <p class="status" data-state={job.status} aria-live="polite" aria-atomic="true">
    {#if job.status === 'running'}
      <span class="spin" aria-hidden="true"></span>
    {:else if job.status === 'queued'}
      <span class="dot" aria-hidden="true"></span>
    {/if}
    <span class="now">{status.text}</span>
    {#if status.more}<span class="more">{status.more}</span>{/if}
  </p>

  {#if hasActions}
    <footer class="acts">
      {#if active}
        <button
          class="btn act btn-ghost-danger"
          disabled={pending}
          onclick={() => void run(() => jobs.cancel(job.id))}
        >
          Cancel
        </button>
      {/if}
      {#if owed}
        <button
          class="btn act btn-outline"
          disabled={pending}
          onclick={() => void run(() => jobs.answer(job.id))}
        >
          Answer now
        </button>
      {/if}
      {#if noteId && app.supportsNotes}
        <button class="btn act btn-outline" onclick={() => void openNote()}>Open note</button>
      {/if}
      {#if job.conversationId}
        <!-- Opening a thread is refused while a reply is being written into
             the one on screen, so the button says it cannot rather than
             doing nothing when pressed. -->
        <button
          class="btn act"
          disabled={agent.busy}
          title={agent.busy ? 'Available when the reply has finished' : undefined}
          onclick={() => void openTranscript()}
        >
          See what it did
        </button>
      {/if}
      {#if onremove && !active}
        <span class="spacer"></span>
        <button class="btn act btn-ghost-danger" onclick={onremove}>Delete</button>
      {/if}
    </footer>
  {/if}
</article>

<style>
  /* The list's size: the same card the routine runs beside it are drawn on. */
  .job {
    display: grid;
    gap: var(--sp-2);
    min-width: 0;
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
  }
  .job.unseen {
    border-color: color-mix(in oklab, var(--accent) 45%, var(--border));
  }
  .job.bad {
    border-color: color-mix(in oklab, var(--danger) 45%, var(--border));
  }
  /* The thread's size: a tool card's, so it sits among them. */
  .job.compact {
    gap: var(--sp-1);
    padding: var(--sp-2);
    background: var(--bg-raised);
    font-size: var(--text-sm);
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }
  .spacer {
    flex: 1;
  }
  .dim {
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .about {
    margin: 0;
    font-weight: 600;
    line-height: var(--leading-snug);
    overflow-wrap: anywhere;
  }
  .compact .about {
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    overflow: hidden;
    font-size: var(--text-sm);
  }

  .status {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    column-gap: var(--sp-2);
    margin: 0;
    color: var(--fg-muted);
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
  }
  .status[data-state='failed'] .now {
    color: var(--danger);
  }
  .more {
    color: var(--fg-faint);
  }
  .compact .status {
    font-size: var(--text-xs);
  }

  /* A running job turns, and a queued one waits: the two things the line has
     to say before it has said anything, and the first is the one that tells
     "still going" from "stuck". Placed on the line's first row rather than
     centred on a wrapped one. */
  .spin,
  .dot {
    flex: none;
    align-self: center;
  }
  .spin {
    width: 10px;
    height: 10px;
    border: 1.5px solid color-mix(in oklab, var(--accent) 25%, transparent);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: turn 0.8s linear infinite;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--fg-faint);
  }
  @keyframes turn {
    to {
      rotate: 360deg;
    }
  }
  @keyframes pulse {
    0%,
    100% {
      opacity: 0.3;
    }
    50% {
      opacity: 1;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .spin {
      animation: pulse 1.1s ease-in-out infinite;
    }
  }

  .acts {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-1);
  }
  /* The shared pill, tightened: a thirty-four pixel button is the right size
     for a page and a lot of button for a card in a column. */
  .act {
    height: 28px;
    padding: 0 var(--sp-3);
    font-size: var(--text-sm);
  }
</style>
