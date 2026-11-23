<script lang="ts">
  // What can be done with a story as something to hear or watch, and to line
  // up: play it, get it, put it in Up Next, say it has been played.
  //
  // One component for the two places it is wanted. In the story list a row is
  // already a button, so what it holds beside the row is only the one round
  // control a podcast app puts at the end of every episode -- play, pause, or
  // the way to get the file -- and everything else is in the row's menu
  // (`compact`). In the open story the whole set is drawn as pills, and an
  // article, which has nothing to play, is offered only the one thing it can
  // do here: go in Up Next, to be heard through Listen or read in its turn --
  // and be saved as audio, which turns it into a recording of its own: once
  // that is in Files it plays here and in Up Next like an episode does.
  //
  // An episode plays from the files folder and never from the network (see
  // `player.svelte.ts`), so "play" is only offered once the file is there;
  // until then the one control is "download", turning into its own progress
  // while it runs, which is also how it is cancelled. Nothing here decides
  // what the player does next: it is asked to play, and it reports back
  // through the same stores everything else reads.

  import { playEpisode } from '../lib/episodes'
  import { player } from '../lib/player.svelte'
  import { downloadLine, downloadPercent, workLine } from '../lib/podcast'
  import type { ContentRef, Story } from '../lib/types'
  import { upnext } from '../lib/upnext.svelte'
  import Icon from './Icon.svelte'

  let { story, compact = false }: { story: Story; compact?: boolean } = $props()

  const ref = $derived<ContentRef>({ type: 'story', id: story.id })
  const isEpisode = $derived(!!story.media)
  const downloaded = $derived(!!story.files?.media)

  const status = $derived(upnext.downloadOf(story.id))
  const running = $derived(status?.state === 'downloading' || status?.state === 'waiting')
  const failed = $derived(status?.state === 'failed')
  const pct = $derived(downloadPercent(status))

  const queued = $derived(upnext.isQueued(ref))
  const progress = $derived(upnext.progressOf(ref))
  const played = $derived(!!progress?.finished)
  /** Something to resume from: begun, and not finished. */
  const started = $derived(!!progress && !progress.finished && (progress.positionMs ?? 0) > 0)

  const current = $derived(player.isCurrent(ref))
  const playing = $derived(player.isPlaying(ref))

  // An article saved as audio plays that audio -- while the file is in Files:
  // the story's link is not the file, and the open story's pane looks it up --
  // and one that is being saved says how far.
  const hasAudio = $derived(!isEpisode && upnext.linkedFile(story.files?.audio) !== null)
  const audioWork = $derived(upnext.workFor({ story: story.id }, 'audio'))
  const savingAudio = $derived(!!audioWork && audioWork.state !== 'failed')
  const audioFailed = $derived(audioWork?.state === 'failed')

  /** "Play", "Pause", or "Resume" for one that was left part way. */
  const playWord = $derived(playing ? 'Pause' : current || started ? 'Resume' : 'Play')

  /** The round control's one press: play it, get it, or stop getting it. */
  function primary() {
    if (downloaded) playEpisode(story)
    else if (running) void upnext.cancelDownload(story.id)
    else void upnext.download(story.id)
  }

  function toggleQueued() {
    if (queued) void upnext.removeFor(ref)
    else void upnext.add(ref)
  }

  /** What the round control is, for the eye that cannot see its picture. */
  const primaryLabel = $derived(
    downloaded
      ? playWord
      : running
        ? `${downloadLine(status)}. Cancel the download`
        : failed
          ? `Download failed${status?.error ? `: ${status.error}` : ''}. Try again`
          : 'Download',
  )
</script>

{#if compact}
  {#if isEpisode}
    <button
      class="round"
      class:go={downloaded}
      class:ring={running}
      class:failed
      style:--pct={pct ?? 0}
      title={primaryLabel}
      aria-label={primaryLabel}
      aria-busy={running}
      onclick={primary}
    >
      {#if downloaded}
        <Icon name={playing ? 'pause' : 'play'} size={15} filled />
      {:else if running}
        <Icon name="close" size={13} />
      {:else}
        <Icon name="download" size={15} />
      {/if}
    </button>
  {/if}
{:else if isEpisode}
  <div class="actions" role="group" aria-label="Episode actions">
    {#if downloaded}
      <button class="btn btn-primary" onclick={() => playEpisode(story)}>
        <Icon name={playing ? 'pause' : 'play'} size={15} filled />
        {playWord}
      </button>
    {:else if running}
      <span class="status" role="status">
        <span class="spin"><Icon name="download" size={15} /></span>
        {downloadLine(status)}
      </span>
      <button class="btn btn-outline" onclick={primary}>Cancel</button>
    {:else}
      <button class="btn btn-primary" onclick={primary} title={failed ? (status?.error ?? '') : ''}>
        <Icon name="download" size={15} />
        {failed ? 'Download failed, try again' : 'Download'}
      </button>
    {/if}

    <button
      class="btn btn-outline"
      class:on={queued}
      aria-pressed={queued}
      title={queued ? 'Take it out of Up Next' : 'Line it up in Up Next'}
      onclick={toggleQueued}
    >
      <Icon name="queue-add" size={15} />
      {queued ? 'In Up Next' : 'Add to Up Next'}
    </button>

    <button
      class="btn btn-outline"
      class:on={played}
      aria-pressed={played}
      title={played ? 'Say it has not been played' : 'Say it has been played, and finish with it'}
      onclick={() => void upnext.markFinished(ref, !played)}
    >
      <Icon name="tick" size={15} />
      {played ? 'Played' : 'Mark played'}
    </button>

    {#if downloaded}
      <button
        class="btn btn-outline"
        title="Move the file to the trash in Files. The episode stays here, and can be downloaded again."
        onclick={() => void upnext.removeDownload(story.id)}
      >
        <Icon name="trash" size={15} />
        Remove download
      </button>
    {/if}
  </div>
{:else}
  <!-- An article: nothing to play until it is saved as audio, but it can wait its turn. -->
  {#if hasAudio}
    <button
      class="btn btn-outline"
      class:on={playing}
      title="Play the audio this article was saved as"
      onclick={() => playEpisode(story)}
    >
      <Icon name={playing ? 'pause' : 'play'} size={15} filled />
      {playing ? 'Pause' : current || started ? 'Resume' : 'Play audio'}
    </button>
  {:else if savingAudio}
    <span class="status" role="status">
      <span class="spin"><Icon name="volume" size={15} /></span>
      {workLine(audioWork)}
    </span>
  {:else if !story.expired}
    <button
      class="btn btn-outline"
      title={audioFailed
        ? (audioWork?.error ?? '')
        : 'Record the article as audio and keep it in Files, to play here and in Up Next'}
      onclick={() => void upnext.renderAudio(story.id)}
    >
      <Icon name="volume" size={15} />
      {audioFailed ? 'Saving as audio failed, try again' : 'Save as audio'}
    </button>
  {/if}
  <button
    class="btn btn-outline"
    class:on={queued}
    aria-pressed={queued}
    title={queued
      ? 'Take it out of Up Next'
      : 'Line it up in Up Next, to be read or heard in its turn'}
    onclick={toggleQueued}
  >
    <Icon name="queue-add" size={15} />
    {queued ? 'In Up Next' : 'Add to Up Next'}
  </button>
{/if}

<style>
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px var(--sp-2);
  }
  /* Done is the accent, in colour alone: In Up Next, Played. */
  .btn.on {
    color: var(--accent);
  }

  .status {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  /* The arrow breathes while the file is coming; stopped outright under
     reduced motion, where the words beside it still say it is going. */
  .spin {
    display: grid;
    animation: breathe 1.4s ease-in-out infinite;
  }
  @keyframes breathe {
    50% {
      opacity: 0.35;
    }
  }

  /* The round control at the end of a row. 34px, the face's own size, so it
     sits level with the icon at the other end. */
  .round {
    position: relative;
    display: grid;
    place-items: center;
    flex: none;
    width: 34px;
    height: 34px;
    border: 1px solid var(--border-strong);
    border-radius: 50%;
    background: var(--bg-raised);
    color: var(--fg-muted);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease),
      border-color var(--fast) var(--ease);
  }
  .round:hover {
    color: var(--fg);
    border-color: var(--fg-faint);
  }
  .round.go {
    border-color: transparent;
    background: var(--accent);
    color: var(--fg-on-accent);
  }
  .round.go:hover {
    background: var(--accent-hover);
    color: var(--fg-on-accent);
  }
  .round.failed {
    border-color: var(--danger);
    color: var(--danger);
  }
  /* The download's progress as a ring around the cross that stops it: a
     conic fill, with a disc over its middle. Cheap, and one element. */
  .round.ring {
    border: 0;
    background: conic-gradient(var(--accent) calc(var(--pct) * 1%), var(--border-strong) 0);
  }
  .round.ring::before {
    content: '';
    position: absolute;
    inset: 3px;
    border-radius: 50%;
    background: var(--bg-raised);
  }
  .round.ring :global(svg) {
    position: relative;
  }
</style>
