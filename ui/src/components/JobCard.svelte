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
  // Three kinds are drawn the same way, and one differently. Research, a
  // project and a task are one long turn each: a progress line and a count of
  // steps. A watch is not -- it looks, sleeps and looks again -- so what it
  // says is when it next looks, what it was asked to tell them, and when it
  // stops; and what a watch or a project has told them so far is listed
  // beneath, the last update first and the rest folded. The thread's compact
  // card leaves that list out: the thread has the updates as lines of its own.
  // See `docs/plans/watches.md`, "The interface".
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
  import { plural, relativeTime } from '../lib/format'
  import {
    everyText,
    isLive,
    isLookingNow,
    jobs,
    nextLookText,
    notifyText,
    sentToText,
    troubleText,
    untilText,
    watchOf,
  } from '../lib/jobs.svelte'
  import { renderMarkdown } from '../lib/markdown'
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

  const KIND_LABELS: Record<JobKind['type'], string> = {
    research: 'Research',
    task: 'Task',
    watch: 'Watch',
    project: 'Project',
  }

  /** What the job is about, in the words it was given. */
  function aboutOf(kind: JobKind): string {
    switch (kind.type) {
      case 'research':
        return kind.question
      case 'task':
        return kind.title
      case 'watch':
        return kind.what
      case 'project':
        return kind.goal
    }
  }

  /** Set only for a watch, which is drawn by what it is told to look for. */
  const watch = $derived(watchOf(job))
  const kind = $derived(KIND_LABELS[job.kind.type])
  const active = $derived(isLive(job))
  /** "Watching CI on PR 412" while it looks, and what it watched once it has stopped. */
  const about = $derived(watch && active ? `Watching ${watch.what}` : aboutOf(job.kind))

  // A watch's card has a clock on it -- "Next look in 3 min" -- and a clock
  // that did not move would say the wrong thing within the minute. Ticking
  // only while it is asleep, which is when there is a countdown to show.
  let now = $state(Date.now())
  $effect(() => {
    if (job.status !== 'waiting') return
    now = Date.now()
    const timer = setInterval(() => (now = Date.now()), 15_000)
    return () => clearInterval(timer)
  })

  /** How many looks it has taken: "14 looks". */
  const looks = $derived(
    job.observed && job.observed.checks > 0 ? plural(job.observed.checks, 'look') : '',
  )
  /** Looking at this moment: due, and not yet written down. */
  const looking = $derived(watch !== null && isLookingNow(job, now))

  /** What it is told to tell them, how often it looks, and when it stops. */
  const rules = $derived(job.rules)
  const act = $derived(rules?.act?.trim() ?? '')
  const looksEvery = $derived.by(() => {
    const probe = watch?.probe
    const at = probe?.type === 'connection' ? ` at ${probe.connection}` : ''
    const until = untilText(rules?.until)
    return `Looks${at} every ${everyText(rules?.everyMinutes)}${until ? ` · ${until}` : ''}`
  })
  /** Failed looks, while it is still going: the part of a watch that needs a person. */
  const trouble = $derived(
    watch && active && job.observed && job.observed.failures > 0
      ? troubleText(job.observed.failures, job.observed.trouble)
      : '',
  )

  /** What it has told them: the last, and the ones before it newest first. */
  const updates = $derived(job.updates ?? [])
  const last = $derived(updates.at(-1) ?? null)
  const earlier = $derived(updates.slice(0, -1).reverse())

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
        if (watch) return { text: 'Looking now', more: looks }
        return {
          text: job.progress.trim() || 'Working on it',
          more: job.steps > 0 ? `${job.steps} ${job.steps === 1 ? 'step' : 'steps'}` : '',
        }
      case 'waiting':
        return { text: nextLookText(job.wakeAt, now), more: looks }
      case 'done': {
        const when = job.finishedAt ? relativeTime(job.finishedAt) : ''
        // A watch ends when what it was asked about is over, or its time is up.
        if (watch) {
          return { text: 'Stopped watching', more: [when, looks].filter(Boolean).join(' · ') }
        }
        const took =
          job.startedAt && job.finishedAt
            ? `took ${elapsed(Date.parse(job.finishedAt) - Date.parse(job.startedAt))}`
            : ''
        const told = owed ? 'not yet answered in the conversation' : ''
        return { text: 'Finished', more: [when, took, told].filter(Boolean).join(' · ') }
      }
      case 'failed':
        return { text: watch ? 'Stopped watching' : 'Could not finish', more: job.reason.trim() }
      case 'cancelled':
        return { text: watch ? 'Stopped watching' : 'Cancelled', more: job.reason.trim() }
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

  <!-- A countdown that changed in a live region would be read out every
       quarter minute, so a sleeping watch's line is not one. It is the
       arrival of a state -- looking, stopped -- that is worth hearing. -->
  <p
    class="status"
    data-state={job.status}
    aria-live={job.status === 'waiting' && !looking ? 'off' : 'polite'}
    aria-atomic="true"
  >
    {#if job.status === 'running' || looking}
      <span class="spin" aria-hidden="true"></span>
    {:else if job.status === 'queued' || job.status === 'waiting'}
      <span class="dot" aria-hidden="true"></span>
    {/if}
    <span class="now">{status.text}</span>
    {#if status.more}<span class="more">{status.more}</span>{/if}
  </p>

  {#if watch && active}
    <div class="facts">
      <p class="fact">Tells you: {notifyText(rules?.notifyWhen)}</p>
      {#if act}<p class="fact">Then: {act}</p>{/if}
      <p class="fact">{looksEvery}</p>
      {#if rules?.callMe}<p class="fact">Calls you with updates</p>{/if}
      {#if trouble}<p class="fact trouble">{trouble}</p>{/if}
    </div>
  {/if}

  {#if last && !compact}
    <!-- Safe by construction, and checked: `renderMarkdown` escapes the text
         before it converts any of it, so every angle bracket that comes back
         was written by that module. What an update says was written by a
         model, from what a service returned. -->
    <div class="update">
      <p class="when">
        <span class="label">Last update</span>
        <span class="dim">{relativeTime(last.at)}</span>
      </p>
      <!-- eslint-disable-next-line svelte/no-at-html-tags -->
      <div class="said">{@html renderMarkdown(last.text)}</div>
      {#if sentToText(last)}<p class="sent">{sentToText(last)}</p>{/if}
    </div>
    {#if earlier.length > 0}
      <details class="earlier">
        <summary>{plural(earlier.length, 'earlier update')}</summary>
        <ol class="log">
          {#each earlier as update, i (i)}
            <li>
              <p class="when"><span class="dim">{relativeTime(update.at)}</span></p>
              <!-- eslint-disable-next-line svelte/no-at-html-tags -->
              <div class="said">{@html renderMarkdown(update.text)}</div>
              {#if sentToText(update)}<p class="sent">{sentToText(update)}</p>{/if}
            </li>
          {/each}
        </ol>
      </details>
    {/if}
  {/if}

  {#if hasActions}
    <footer class="acts">
      {#if active}
        <button
          class="btn act btn-ghost-danger"
          disabled={pending}
          onclick={() => void run(() => jobs.cancel(job.id))}
        >
          {watch ? 'Stop watching' : 'Cancel'}
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

  /* What a watch was asked to do, a line each: quieter than the status, which
     is the one that moves. */
  .facts {
    display: grid;
    gap: 2px;
    min-width: 0;
  }
  .fact {
    margin: 0;
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: var(--leading-snug);
    overflow-wrap: anywhere;
  }
  .compact .fact {
    font-size: var(--text-xs);
  }
  .fact.trouble {
    color: var(--danger);
  }

  /* What it has told them. The last one set off by a rule down its side, as
     a quotation is; the rest folded beneath it. */
  .update,
  .log li {
    display: grid;
    gap: var(--sp-1);
    min-width: 0;
    padding-left: var(--sp-3);
    border-left: 2px solid var(--border);
  }
  .when {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    margin: 0;
  }
  .label {
    color: var(--fg-muted);
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .said {
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    overflow-wrap: anywhere;
  }
  .said :global(p) {
    margin: 0;
  }
  .said :global(p + p) {
    margin-top: var(--sp-2);
  }
  .said :global(ul),
  .said :global(ol) {
    margin: var(--sp-1) 0 var(--sp-1) var(--sp-4);
  }
  .sent {
    margin: 0;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
  .earlier summary {
    color: var(--fg-faint);
    font-size: var(--text-sm);
    cursor: pointer;
    user-select: none;
  }
  .earlier summary:hover {
    color: var(--fg-muted);
  }
  .log {
    display: grid;
    gap: var(--sp-3);
    margin: var(--sp-2) 0 0;
    padding: 0;
    list-style: none;
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
