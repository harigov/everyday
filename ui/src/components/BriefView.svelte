<script lang="ts">
  // The brief page: the morning, evening or weekly brief, read as a page --
  // and the daily podcast, played from it.
  //
  // A brief is a short document -- the shape of the day in one sentence,
  // then a few sections of the model's prose -- and the page is laid out
  // like one: a reading column for the words, and the pictures, which are
  // drawn from the digest the brief was written from rather than by the
  // model, a little wider beside them. Bottom line first, so the headline
  // is the loudest thing on the page and the counts under it the next.
  //
  // Above it, the three things to do with a brief: hear it (the player in
  // the top bar reads the spoken version), make one now, and change what it
  // covers and when (Settings → Briefs). Older briefs are a step back
  // through the run log, which is all a brief's history is. See
  // `docs/plans/briefs.md`.
  //
  // The daily podcast is a brief too, and its page leads with what it is
  // for: a Play button that hands its audio to the player -- the one in the
  // top bar, which goes on when somebody leaves the page -- and Up Next, with
  // the show notes and the script it was recorded from below. Until its
  // audio is made the page says so.

  import { onMount } from 'svelte'
  import { agent } from '../lib/agent.svelte'
  import { assistant } from '../lib/assistant.svelte'
  import {
    KIND_INFO,
    SECTION_INFO,
    briefDay,
    greeting,
    hasFigure,
    listeningMinutes,
    podcastPlayRequest,
    readingMinutes,
    scriptMinutes,
    sectionTitle,
  } from '../lib/brief'
  import { briefs } from '../lib/brief.svelte'
  import { formatClock, friendlyDate, timeOfDay } from '../lib/format'
  import type { IconName } from '../lib/icons'
  import { listening } from '../lib/listen.svelte'
  import { renderMarkdown } from '../lib/markdown'
  import { panels } from '../lib/panels.svelte'
  import { details } from '../lib/phone.svelte'
  import { player } from '../lib/player.svelte'
  import { app } from '../lib/state.svelte'
  import { isoDate } from '../lib/time'
  import {
    BRIEF_KINDS,
    type BriefKind,
    type BriefSection,
    type ContentRef,
    type Delivered,
    type RoutineRun,
    type RoutineRunId,
  } from '../lib/types'
  import { upnext } from '../lib/upnext.svelte'
  import BriefFigure from './BriefFigure.svelte'
  import BriefStats from './BriefStats.svelte'
  import BriefTimeline from './BriefTimeline.svelte'
  import BriefWeek from './BriefWeek.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'
  import Loading from './Loading.svelte'

  onMount(() => {
    void briefs.start()
    // Whether the podcast is already in Up Next is read from it.
    void upnext.load()
  })

  /** The kind somebody picked; until then, the newest brief's. */
  let chosen = $state<BriefKind | null>(null)
  /** The brief being read, when it is not the newest of its kind. */
  let picked = $state<RoutineRunId | null>(null)

  const KIND_SHORT: Record<BriefKind, string> = {
    morning: 'Morning',
    evening: 'Evening',
    weekly: 'Weekly',
    podcast: 'Podcast',
  }

  /**
   * Each section's mark wears the colour of the app its facts come from --
   * the calendar's for the day, Mail's for the mail -- the way the app bar
   * draws them, so the page reads as made of the rest of the application.
   * Weather is no app's, and gets a sky of its own at the same lightness.
   */
  const SECTION_TINT: Record<BriefSection, string> = {
    review: 'var(--app-journal)',
    week: 'var(--app-calendar)',
    schedule: 'var(--app-calendar)',
    people: 'var(--app-library)',
    tasks: 'var(--app-todo)',
    mail: 'var(--app-mail)',
    weather: 'oklch(var(--app-lightness) var(--app-chroma) 232)',
    goals: 'var(--app-overview)',
    tidbit: 'var(--app-assistant)',
    occasions: 'var(--app-library)',
    reading: 'var(--app-feed)',
  }

  /** The kinds worth a pill: those with a brief, or switched on at some point. */
  const kinds = $derived(
    BRIEF_KINDS.filter((k) => briefs.latestOf(k) !== null || briefs.routineOf(k) !== undefined),
  )
  const kind = $derived<BriefKind>(
    chosen && kinds.includes(chosen)
      ? chosen
      : (briefs.latest?.brief?.kind ?? kinds[0] ?? 'morning'),
  )
  /** This kind's briefs, newest first. */
  const history = $derived(briefs.briefs.filter((r) => r.brief?.kind === kind))
  const run = $derived(history.find((r) => r.id === picked) ?? history[0] ?? null)
  const brief = $derived(run?.brief ?? null)
  const digest = $derived(brief?.digest ?? null)
  const position = $derived(run ? history.indexOf(run) : -1)
  const older = $derived(position >= 0 ? (history[position + 1] ?? null) : null)
  const newer = $derived(position > 0 ? (history[position - 1] ?? null) : null)

  const anyRoutine = $derived(BRIEF_KINDS.some((k) => briefs.routineOf(k) !== undefined))
  const making = $derived(briefs.isMaking(kind))
  const hearing = $derived(run ? briefs.isListening(run) : false)

  function pickKind(k: BriefKind) {
    chosen = k
    picked = null
  }

  function makeNow(k: BriefKind = kind) {
    // The new one is shown when it arrives, wherever the reader had stepped
    // back to.
    picked = null
    chosen = k
    void briefs.makeNow(k)
  }

  function listen() {
    if (!run) return
    if (hearing) listening.stop()
    else void briefs.listen(run)
  }

  function openSettings() {
    panels.openSettings('briefs')
  }

  // ── The daily podcast ────────────────────────────────────────────────

  const podcast = $derived(brief?.kind === 'podcast' ? brief : null)
  const audioRef = $derived<ContentRef | null>(
    podcast?.audio ? { type: 'file', id: podcast.audio } : null,
  )
  const current = $derived(audioRef !== null && player.isCurrent(audioRef))
  const playing = $derived(audioRef !== null && player.isPlaying(audioRef))
  const queued = $derived(audioRef !== null && upnext.isQueued(audioRef))

  /** How long after a podcast was written its audio is still expected. */
  const AUDIO_WAIT_MS = 30 * 60_000

  /** No audio yet, and the podcast is recent enough for it to be on its way. */
  const finishedMs = $derived(run ? Date.parse(run.finishedAt ?? run.startedAt) : Number.NaN)
  const recording = $derived(!!podcast && !podcast.audio && Date.now() - finishedMs < AUDIO_WAIT_MS)

  /** Play the podcast, or pause it, or take it up again where it was left. */
  function playPodcast() {
    if (!podcast) return
    if (audioRef && player.isCurrent(audioRef)) {
      player.toggle()
      return
    }
    const request = podcastPlayRequest(podcast)
    if (request) void player.play(request)
  }

  function queuePodcast() {
    if (audioRef && !queued) void upnext.add(audioRef)
  }

  /** Who says a line: the speaker's name when there are several, else nobody. */
  function said(speaker: string): string {
    const speakers = podcast?.script?.speakers ?? []
    if (speakers.length < 2) return ''
    return speakers.find((s) => s.id === speaker)?.name || speaker
  }

  /** The mark on the page for a kind that has no brief yet. */
  const emptyIcon = $derived<IconName>(
    kind === 'podcast' ? 'podcast' : kind === 'evening' ? 'clock' : 'sun',
  )

  /** About how many minutes the podcast takes to hear. */
  const podcastMinutes = $derived(
    podcast ? scriptMinutes(podcast.script) || listeningMinutes(podcast) : 0,
  )

  /**
   * Show the run's transcript in the Assistant app -- every tool it called
   * and every word it said -- as the routine log's "What it did, step by
   * step" does (`AssistantPanes.svelte`'s `openRun`).
   */
  async function openTranscript(r: RoutineRun) {
    const thread = r.conversationId
    if (!thread) return
    assistant.openRun = r.id
    await agent.load()
    if (await app.goTo('assistant')) {
      details.open('assistant')
      await agent.openThread(thread)
    }
  }

  /** Markdown for one line: **bold** and the rest, without the paragraph. */
  function inline(md: string): string {
    const html = renderMarkdown(md.trim())
    const one = /^<p>([\s\S]*)<\/p>$/.exec(html)
    return one && !one[1]!.includes('<p>') ? one[1]! : html
  }

  /** "tomorrow at 7:00 AM", "on Sunday at 8:00 AM" -- when a brief next comes. */
  function arrives(k: BriefKind): string | null {
    const routine = briefs.routineOf(k)
    if (!routine?.enabled || routine.trigger.type !== 'schedule') return null
    if (!routine.nextDue) return `at ${formatClock(routine.trigger.at)}`
    const next = new Date(routine.nextDue)
    const day = friendlyDate(isoDate(next))
    const on = day === 'Today' ? 'today' : day === 'Tomorrow' ? 'tomorrow' : `on ${day}`
    return `${on} at ${timeOfDay(next)}`
  }

  function sentLine(d: Delivered): string {
    if (d.ok)
      return d.to === 'email' ? `Emailed to ${d.detail}` : `Sent to ${d.detail || 'your chat'}`
    return d.to === 'email' ? `Not emailed: ${d.detail}` : `Not sent to your chat: ${d.detail}`
  }

  const madeAt = $derived(run ? (digest?.madeAt ?? run.finishedAt ?? run.startedAt) : null)
  const greetingLine = $derived(
    digest ? greeting(digest.firstName, digest.madeAt ? new Date(digest.madeAt) : undefined) : '',
  )
</script>

<div class="briefs">
  <header class="bar">
    {#if kinds.length > 1}
      <div class="filters kinds" role="group" aria-label="Which brief">
        {#each kinds as k (k)}
          <button
            class="filter"
            class:on={k === kind}
            aria-pressed={k === kind}
            onclick={() => pickKind(k)}
          >
            {KIND_SHORT[k]}
            {#if briefs.isMaking(k)}<span class="spinner small" aria-hidden="true"></span>{/if}
          </button>
        {/each}
      </div>
    {:else}
      <div class="pane-titles">
        <!-- Not a heading: the brief's headline is the page's. -->
        <span class="pane-title">{KIND_INFO[kind].name}</span>
      </div>
    {/if}

    {#if brief}
      <div class="history" role="group" aria-label="Earlier briefs">
        <button
          class="step"
          aria-label="An older brief"
          title="An older brief"
          disabled={!older}
          onclick={() => older && (picked = older.id)}
        >
          <span class="prev"><Icon name="chevron" size={14} /></span>
        </button>
        <span class="when" title={briefDay(brief.day)} aria-live="polite">
          {friendlyDate(brief.day)}
        </span>
        <button
          class="step"
          aria-label="A newer brief"
          title="A newer brief"
          disabled={!newer}
          onclick={() => newer && (picked = newer.id)}
        >
          <span class="next"><Icon name="chevron" size={14} /></span>
        </button>
        {#if newer}
          <button class="latest" onclick={() => (picked = null)}>Latest</button>
        {/if}
      </div>
    {/if}

    <div class="actions">
      {#if run}
        <!-- A podcast is played, from the button on its page: the player in
             the top bar, with its own speed and place. Listen would only say
             its words in the voice again. -->
        {#if kind !== 'podcast'}
          <button
            class="btn btn-outline"
            class:hearing
            aria-pressed={hearing}
            disabled={!listening.available}
            title={listening.available
              ? hearing
                ? 'Stop reading it aloud'
                : 'Read this brief aloud'
              : 'Listening works on your computer for now.'}
            onclick={listen}
          >
            {#if hearing}
              <span class="eq" aria-hidden="true"><i></i><i></i><i></i></span> Listening…
            {:else}
              <Icon name="play" size={13} /> Listen
            {/if}
          </button>
        {/if}
        <button class="btn" disabled={making} onclick={() => makeNow()}>
          {#if making}
            <span class="spinner" aria-hidden="true"></span>
            {kind === 'podcast' ? 'Making your podcast…' : 'Writing your brief…'}
          {:else}
            <Icon name="sparkle" size={14} /> Make one now
          {/if}
        </button>
      {/if}
      <button
        class="btn icon-only"
        aria-label="Brief settings"
        title="Brief settings"
        onclick={openSettings}
      >
        <Icon name="settings" size={16} />
      </button>
    </div>
  </header>

  <div class="scroll body">
    {#if briefs.error}
      <p class="oops" role="alert">
        <span class="oops-icon"><Icon name="alert" size={14} weight={1.8} /></span>
        {briefs.error}
      </p>
    {/if}

    {#if run && kind !== 'podcast' && !listening.available}
      <!-- Said on the page, not only in the disabled button's tooltip, which a
           phone has no way to show. -->
      <p class="hint text listen-note">Listening works on your computer for now.</p>
    {/if}

    {#if !briefs.enabled}
      <EmptyState lead="Briefs aren’t available on this device">
        {#snippet icon()}<Icon name="sun" size={28} />{/snippet}
      </EmptyState>
    {:else if !briefs.loaded}
      <Loading label="Loading your briefs…" centred />
    {:else if run && brief && digest}
      {#if making}
        <p class="writing text" role="status">
          <span class="spinner" aria-hidden="true"></span>
          {#if kind === 'podcast'}
            Making a new daily podcast: the script first, then the audio. It appears here in a few
            minutes.
          {:else}
            Writing a new {KIND_INFO[kind].name.toLowerCase()}. It appears here in a minute or two.
          {/if}
        </p>
      {/if}

      {#key run.id}
        <article class="page" aria-labelledby="brief-headline">
          <header class="hero text">
            <p class="kicker">{KIND_INFO[brief.kind].name} · {briefDay(brief.day)}</p>
            {#if greetingLine}<p class="greet">{greetingLine}</p>{/if}
            <h1 class="headline" id="brief-headline">
              <!-- Safe by construction: `renderMarkdown` escapes its input
                   before any pattern runs (see `lib/markdown.ts`). -->
              <!-- eslint-disable-next-line svelte/no-at-html-tags -->
              {@html inline(brief.headline)}
            </h1>
            <p class="meta">
              {#if podcast}
                {#if podcastMinutes > 0}<span>About {podcastMinutes} min to hear</span>{/if}
              {:else}
                <span>{readingMinutes(brief)} min read</span>
                <span class="sep-dot" aria-hidden="true">·</span>
                <span>{listeningMinutes(brief)} min to hear</span>
              {/if}
            </p>
          </header>

          {#if podcast}
            <div class="text listen" role="group" aria-label="The podcast">
              {#if audioRef}
                <button
                  class="btn btn-primary play"
                  onclick={playPodcast}
                  title={playing ? 'Pause the podcast' : 'Play the podcast'}
                >
                  <Icon name={playing ? 'pause' : 'play'} size={18} weight={1.6} filled />
                  {playing ? 'Pause' : current ? 'Resume' : 'Play'}
                </button>
                <button
                  class="btn btn-outline"
                  disabled={queued}
                  title={queued ? 'It is in Up Next' : 'Play it after what is queued'}
                  onclick={queuePodcast}
                >
                  <Icon name={queued ? 'tick' : 'queue-add'} size={14} />
                  {queued ? 'In Up Next' : 'Add to Up Next'}
                </button>
              {:else if recording}
                <p class="recording" role="status">
                  <span class="spinner" aria-hidden="true"></span>
                  The audio is being made. It will be here in a few minutes; the script is below.
                </p>
              {:else}
                <p class="recording" role="status">
                  <span class="note-icon"><Icon name="info" size={13} weight={1.7} /></span>
                  No audio was made for this one. The script is below.
                </p>
              {/if}
            </div>
          {:else}
            <div class="wide stats">
              <BriefStats {digest} />
            </div>
          {/if}

          {#each brief.parts as part, i (i)}
            {@const section = part.section && part.section in SECTION_INFO ? part.section : null}
            <section
              class="part"
              class:intro={!section && !part.title}
              aria-labelledby={section || part.title ? `brief-part-${i}` : undefined}
              aria-label={section || part.title ? undefined : 'Introduction'}
            >
              {#if section || part.title}
                <h2
                  class="part-title text"
                  id="brief-part-{i}"
                  style="--tint: {section ? SECTION_TINT[section] : 'var(--accent)'}"
                >
                  {#if section}
                    <span class="mark" aria-hidden="true">
                      <Icon name={SECTION_INFO[section].icon} size={15} weight={1.7} />
                    </span>
                  {/if}
                  <span>{part.title || (section ? sectionTitle(section, brief.kind) : '')}</span>
                </h2>
              {/if}

              {#if section && hasFigure(section, digest)}
                <div class="wide figure">
                  {#if section === 'schedule'}
                    <BriefTimeline {digest} />
                  {:else if section === 'week'}
                    <BriefWeek {digest} />
                  {:else}
                    <BriefFigure {section} {digest} />
                  {/if}
                </div>
              {/if}

              {#if part.body.trim()}
                <div class="prose text">
                  <!-- eslint-disable-next-line svelte/no-at-html-tags -->
                  {@html renderMarkdown(part.body)}
                </div>
              {/if}
            </section>
          {/each}

          {#if podcast?.script && podcast.script.lines.length > 0}
            <section class="part" aria-labelledby="brief-script">
              <h2 class="part-title text" id="brief-script" style="--tint: var(--app-feed)">
                <span class="mark" aria-hidden="true">
                  <Icon name="podcast" size={15} weight={1.7} />
                </span>
                <span>The script</span>
              </h2>
              <div class="prose text">
                {#each podcast.script.lines as line, i (i)}
                  <p>
                    {#if said(line.speaker)}<b class="speaker">{said(line.speaker)}</b>{/if}
                    {line.text}
                  </p>
                {/each}
              </div>
            </section>
          {/if}

          <footer class="foot text">
            <p class="made">
              {#if madeAt}<span>Made at {timeOfDay(new Date(madeAt))}</span>{/if}
              {#if run.steps > 0}
                <span class="sep-dot" aria-hidden="true">·</span>
                <span>{run.steps} {run.steps === 1 ? 'step' : 'steps'}</span>
              {/if}
              {#if run.conversationId}
                <span class="sep-dot" aria-hidden="true">·</span>
                <button class="link" onclick={() => void openTranscript(run)}>
                  See how it was written
                </button>
              {/if}
            </p>
            {#each run.delivered ?? [] as d, i (i)}
              <p class="note" class:failed={!d.ok}>
                <span class="note-icon">
                  <Icon
                    name={d.ok ? (d.to === 'email' ? 'mail' : 'share') : 'alert'}
                    size={13}
                    weight={1.7}
                  />
                </span>
                {sentLine(d)}
              </p>
            {/each}
            {#each digest.gaps as gap, i (i)}
              <p class="note">
                <span class="note-icon"><Icon name="info" size={13} weight={1.7} /></span>
                {gap}
              </p>
            {/each}
          </footer>
        </article>
      {/key}
    {:else if !anyRoutine}
      <EmptyState lead="A brief is your day on one page, ready before it starts.">
        {#snippet icon()}<Icon name="sun" size={28} />{/snippet}
        {#snippet note()}
          Each morning the assistant reads your calendar, tasks, mail, goals and the weather, and
          writes up only what matters, to read in a few minutes or to hear. Every time and number in
          it comes from your vault, and it changes nothing while it writes.
        {/snippet}
        {#snippet action()}
          <button class="btn btn-primary" disabled={making} onclick={() => makeNow('morning')}>
            {#if making}
              <span class="spinner on-accent" aria-hidden="true"></span> Writing your brief…
            {:else}
              Make my first brief
            {/if}
          </button>
          <button class="btn" onclick={openSettings}>Set up briefs</button>
        {/snippet}
      </EmptyState>
    {:else}
      {@const when = arrives(kind)}
      {@const routine = briefs.routineOf(kind)}
      <EmptyState
        lead={making
          ? `Writing your first ${KIND_INFO[kind].name.toLowerCase()}…`
          : when
            ? `Your first ${KIND_INFO[kind].name.toLowerCase()} arrives ${when}.`
            : routine && !routine.enabled
              ? `The ${KIND_INFO[kind].name.toLowerCase()} is switched off.`
              : `There is no ${KIND_INFO[kind].name.toLowerCase()} yet.`}
      >
        {#snippet icon()}<Icon name={emptyIcon} size={28} />{/snippet}
        {#snippet note()}
          {#if making}
            It takes a minute or two, and appears here when it is done.
          {:else}
            {KIND_INFO[kind].blurb}
          {/if}
        {/snippet}
        {#snippet action()}
          <button class="btn btn-primary" disabled={making} onclick={() => makeNow(kind)}>
            {#if making}
              <span class="spinner on-accent" aria-hidden="true"></span> Writing…
            {:else}
              Make one now
            {/if}
          </button>
          <button class="btn" onclick={openSettings}>Settings</button>
        {/snippet}
      </EmptyState>
    {/if}
  </div>
</div>

<style>
  .briefs {
    /* Inside the Overview's own scrolling body, so as tall as what it
       holds: the body scrolls, not this. */
    min-height: 100%;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    background: var(--bg);
    --tint: var(--app-overview);
    /* The reading column, and the wider one the pictures take. */
    --text-w: 660px;
    --wide-w: 880px;
  }

  /* ── The bar ──────────────────────────────────────────────────────── */

  .bar {
    display: flex;
    flex: none;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2) var(--sp-3);
    min-height: var(--header-h);
    padding: var(--sp-2) var(--sp-4);
    border-bottom: 1px solid var(--border);
  }

  .kinds {
    --tint: var(--app-overview);
  }
  .pane-titles {
    flex: 0 1 auto;
  }

  .history {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .history button {
    display: grid;
    place-items: center;
    height: 28px;
    border-radius: var(--radius-pill);
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .history button:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .history button:disabled {
    opacity: 0.35;
  }
  .step {
    width: 28px;
  }
  .prev,
  .next {
    display: grid;
    place-items: center;
  }
  .prev {
    rotate: 180deg;
  }
  .when {
    min-width: 6.5em;
    padding: 0 var(--sp-1);
    color: var(--fg);
    font-size: var(--text-sm);
    font-weight: 600;
    text-align: center;
    white-space: nowrap;
  }
  .latest {
    padding: 0 var(--sp-3);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: flex-end;
    gap: var(--sp-2);
    margin-left: auto;
  }
  .icon-only {
    width: 34px;
    padding: 0;
  }
  .btn.hearing {
    border-color: color-mix(in oklab, var(--accent) 50%, var(--border-strong));
    background: color-mix(in oklab, var(--accent) 10%, transparent);
    color: var(--fg);
  }

  /* Three bars rising and falling while the player reads this brief. */
  .eq {
    display: inline-flex;
    align-items: flex-end;
    gap: 2px;
    height: 12px;
  }
  .eq i {
    display: block;
    width: 3px;
    height: 100%;
    border-radius: 1px;
    background: var(--accent);
    transform-origin: bottom;
    animation: eq 0.9s ease-in-out infinite;
  }
  .eq i:nth-child(2) {
    animation-delay: -0.3s;
  }
  .eq i:nth-child(3) {
    animation-delay: -0.6s;
  }
  @keyframes eq {
    0%,
    100% {
      transform: scaleY(0.35);
    }
    50% {
      transform: scaleY(1);
    }
  }

  /* The ring is the global `.spinner`; these only size and colour it for the
     tab chip and for a button on the accent. */
  .spinner.small {
    width: 10px;
    height: 10px;
    border-width: 1.5px;
  }
  .spinner.on-accent {
    color: var(--fg-on-accent);
    opacity: 1;
  }

  /* ── The page ─────────────────────────────────────────────────────── */

  .body {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-height: 0;
    padding: var(--sp-8) var(--sp-6) var(--fab-clear);
  }

  .text {
    width: 100%;
    max-width: var(--text-w);
    margin-inline: auto;
  }
  .wide {
    width: 100%;
    max-width: var(--wide-w);
    margin-inline: auto;
  }

  .listen-note {
    margin-bottom: var(--sp-4);
  }

  .oops,
  .writing {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin: 0 auto var(--sp-5);
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  .oops {
    width: 100%;
    max-width: var(--text-w);
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius);
    background: color-mix(in oklab, var(--danger) 8%, transparent);
  }
  .oops-icon {
    display: flex;
    flex: none;
    color: var(--danger);
  }
  .writing {
    padding: var(--sp-2) var(--sp-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
  }
  .page {
    display: flex;
    flex-direction: column;
    animation: arrive var(--med) var(--ease);
  }
  @keyframes arrive {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
  }

  /* Bottom line first: a small kicker, the greeting, and the headline as
     the loudest thing on the page. */
  .hero {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin-bottom: var(--sp-6);
  }
  .kicker {
    margin: 0;
    color: color-mix(in oklab, var(--tint) 72%, var(--fg));
    font-size: var(--text-xs);
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .greet {
    margin: var(--sp-1) 0 0;
    color: var(--fg-muted);
    font-family: var(--font-read);
    font-size: var(--text-lg);
    line-height: var(--leading-snug);
  }
  .headline {
    margin: 0;
    color: var(--fg);
    font-family: var(--font-read);
    font-size: clamp(var(--text-xl), 2.4vw + 10px, 30px);
    font-weight: 600;
    letter-spacing: -0.012em;
    line-height: 1.28;
    text-wrap: balance;
  }
  /* What the headline marks as the one thing that matters: heavier, with a
     highlighter's stroke under its lower half -- set in text ink, the tint
     only behind it. */
  .headline :global(strong) {
    font-weight: 700;
    background: linear-gradient(
      transparent 60%,
      color-mix(in oklab, var(--tint) 24%, transparent) 60%,
      color-mix(in oklab, var(--tint) 24%, transparent) 92%,
      transparent 92%
    );
    box-decoration-break: clone;
    -webkit-box-decoration-break: clone;
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
    margin: var(--sp-1) 0 0;
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }
  .sep-dot {
    color: var(--fg-faint);
  }

  .stats {
    margin-bottom: var(--sp-8);
  }

  /* The podcast's point: the button that plays it, with what else to do
     with it beside, where a brief has the day's figures. */
  .listen {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-3);
    margin-bottom: var(--sp-8);
  }
  .play {
    height: 46px;
    padding: 0 var(--sp-6) 0 var(--sp-5);
    font-size: var(--text-md);
    font-weight: 600;
  }
  .recording {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin: 0;
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  /* Who says a line, when more than one voice does. */
  .speaker {
    margin-right: 0.4em;
    color: var(--fg-muted);
    font-family: var(--font-ui);
    font-size: 0.82em;
    font-weight: 650;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  /* ── Sections ─────────────────────────────────────────────────────── */

  .part {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .part + .part {
    margin-top: var(--sp-10);
  }
  .part.intro + .part {
    margin-top: var(--sp-8);
  }

  .part-title {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    margin: 0;
    color: var(--fg);
    font-size: var(--text-md);
    font-weight: 650;
    letter-spacing: -0.005em;
    line-height: var(--leading-tight);
  }
  /* The section's mark, in the colour of the app its facts are from, the
     way a card on the Overview wears its own. */
  .mark {
    display: grid;
    flex: none;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 9px;
    background: color-mix(in oklab, var(--tint) 18%, transparent);
    color: color-mix(in oklab, var(--tint) 85%, var(--fg));
  }

  .figure {
    min-width: 0;
  }

  /* The model's words: the reading face at the size the journal uses, with
     room between the lines. */
  .prose {
    color: var(--fg);
    font-family: var(--font-read);
    font-size: var(--text-prose);
    line-height: 1.6;
    overflow-wrap: break-word;
  }
  .prose :global(> *) {
    margin: 0;
  }
  .prose :global(> * + *) {
    margin-top: 0.75em;
  }
  .prose :global(strong) {
    font-weight: 650;
  }
  .prose :global(ul),
  .prose :global(ol) {
    padding-left: 1.3em;
  }
  .prose :global(li + li) {
    margin-top: 0.3em;
  }
  .prose :global(li::marker) {
    color: var(--fg-faint);
  }
  .prose :global(a) {
    color: var(--accent);
    text-decoration: underline;
    text-decoration-thickness: 1px;
    text-underline-offset: 2px;
  }
  .prose :global(blockquote) {
    margin-left: 0;
    padding-left: var(--sp-4);
    border-left: 2px solid var(--border-strong);
    color: var(--fg-muted);
  }
  .prose :global(h3),
  .prose :global(h4) {
    font-family: var(--font-ui);
    font-size: var(--text-base);
    font-weight: 650;
  }
  .prose :global(code) {
    padding: 1px 4px;
    border-radius: 4px;
    background: var(--bg-sunken);
    font-family: var(--font-mono);
    font-size: 0.88em;
  }
  .prose :global(hr) {
    border: none;
    border-top: 1px solid var(--border);
  }
  /* A paragraph with no heading before it, ahead of the sections: set a
     touch larger, as a standfirst. */
  .part.intro .prose {
    font-size: var(--text-lg);
    color: var(--fg-muted);
  }

  /* ── Footer ───────────────────────────────────────────────────────── */

  .foot {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin-top: var(--sp-12);
    padding-top: var(--sp-4);
    border-top: 1px solid var(--border);
    color: var(--fg-subtle);
    font-size: var(--text-sm);
  }
  .made,
  .note {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    margin: 0;
  }
  .note {
    flex-wrap: nowrap;
    align-items: flex-start;
    line-height: var(--leading-snug);
  }
  .note-icon {
    display: flex;
    flex: none;
    margin-top: 2px;
    color: var(--fg-faint);
  }
  .note.failed .note-icon {
    color: var(--warning);
  }
  .note.failed {
    color: var(--fg-muted);
  }
  .link {
    color: var(--accent);
    font-size: var(--text-sm);
    font-weight: 550;
  }
  .link:hover {
    text-decoration: underline;
  }

  /* ── A phone ──────────────────────────────────────────────────────
     The bar is three rows, each a line of its own -- which brief, which day,
     and what to do with it -- rather than a wrapped tangle of the same
     controls; the page has the screen's width less a narrow margin instead
     of the margins of a reading column; and what a finger presses is 40 px
     tall at least. */
  :global([data-phone]) .bar {
    gap: var(--sp-2);
    padding: var(--sp-3) var(--sp-4);
  }
  :global([data-phone]) .kinds {
    max-width: 100%;
    flex-wrap: nowrap;
    overflow-x: auto;
    scrollbar-width: none;
  }
  :global([data-phone]) .kinds :global(.filter) {
    min-height: 38px;
  }
  :global([data-phone]) .history {
    gap: var(--sp-1);
  }
  :global([data-phone]) .history button {
    height: 40px;
  }
  :global([data-phone]) .step {
    width: 40px;
  }
  :global([data-phone]) .actions {
    flex: 1 1 100%;
    flex-wrap: nowrap;
    justify-content: flex-start;
    margin-left: 0;
  }
  :global([data-phone]) .actions .btn:not(.icon-only) {
    flex: 1 1 0;
    min-width: 0;
    justify-content: center;
    white-space: nowrap;
  }
  :global([data-phone]) .icon-only {
    flex: none;
    width: 40px;
  }
  :global([data-phone]) .body {
    padding: var(--sp-5) var(--sp-4) var(--fab-clear);
  }
  :global([data-phone]) .stats {
    margin-bottom: var(--sp-6);
  }
  :global([data-phone]) .part + .part {
    margin-top: var(--sp-8);
  }
  :global([data-phone]) .foot {
    margin-top: var(--sp-8);
  }
  :global([data-phone]) .foot .link {
    display: inline-flex;
    align-items: center;
    min-height: 40px;
  }
  :global([data-phone]) .listen .btn {
    flex: 1 1 auto;
    justify-content: center;
  }

  @media (prefers-reduced-motion: reduce) {
    .page {
      animation: none;
    }
    .eq i {
      animation: none;
      transform: scaleY(0.7);
    }
  }
</style>
