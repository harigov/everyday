<script lang="ts">
  // The player, opened out: a video's picture, the large controls, a scrubber,
  // speed and volume, "Mark played", and Up Next to play from, reorder and
  // clear. A panel standing over the right edge of the window rather than a
  // dialog: nothing behind it is blocked, no focus is trapped, and what is
  // playing goes on whatever is done in the app beside it.
  //
  // It draws no media element. The one the window has is `PlayerHost`'s, and
  // for a film it is laid over `picture` -- the empty box below, handed up
  // with a binding -- which is why that box is not inside the scrolling part:
  // a film that is fixed to the window cannot scroll with a list.
  //
  // Escape closes it, but only with the keyboard in it. A panel that took
  // Escape from the whole window would close when somebody meant to leave
  // Settings or drop a selection, and one is always an Escape away from
  // either.
  //
  // "Ask about this" pauses what is playing and starts a spoken conversation
  // with the assistant (`voice.askAboutPlaying`). The assistant's rail is
  // where the conversation is drawn, which is under this panel's place, so
  // asking puts the panel away.

  import { onMount } from 'svelte'
  import { agent } from '../lib/agent.svelte'
  import { thumbnailUrl } from '../lib/api'
  import { files } from '../lib/files.svelte'
  import { focusOnMount } from '../lib/focus'
  import type { IconName } from '../lib/icons'
  import { PLAYER_SPEEDS, player } from '../lib/player.svelte'
  import { dropBefore, formatDuration, formatRemaining } from '../lib/podcast'
  import type { QueueEntryId, QueueItem } from '../lib/types'
  import { upnext } from '../lib/upnext.svelte'
  import { voice } from '../lib/voice.svelte'
  import Icon from './Icon.svelte'

  interface Props {
    /** The box a film is laid over: only drawn for one. */
    picture?: HTMLElement | null
  }
  let { picture = $bindable(null) }: Props = $props()

  // Up Next is read again whenever the panel opens: it is cheap, and what the
  // feeds added while it was shut is then in it.
  onMount(() => void upnext.load())

  /** Where the keyboard was when the panel opened, to go back to. */
  const opener = typeof document !== 'undefined' ? document.activeElement : null

  function close() {
    player.expanded = false
    if (opener instanceof HTMLElement && opener.isConnected) opener.focus()
  }

  const current = $derived(player.current)
  /** Asking is on offer where it can be done at all. */
  const askable = $derived(voice.available && agent.supported)
  const back = $derived(upnext.settings?.skipBackS ?? 15)
  const forward = $derived(upnext.settings?.skipForwardS ?? 30)

  /** Where the scrubber is while it is being dragged; the player's own
   *  position moves under it as the sound plays, and would fight the hand. */
  let scrub = $state<number | null>(null)
  const duration = $derived(player.durationMs || current?.durationMs || 0)
  const position = $derived(Math.min(scrub ?? player.positionMs, duration || Infinity))

  /** A picture that would not load is not tried again. */
  let broken = $state<string | null>(null)
  const artwork = $derived(current?.artwork && current.artwork !== broken ? current.artwork : null)

  const glyph = $derived<IconName>(
    current?.kind === 'video' ? 'film' : current?.what.type === 'file' ? 'music' : 'podcast',
  )

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault()
      e.stopPropagation()
      close()
      return
    }
    // The panel's own keys, only when nothing inside it has the keyboard: a
    // button takes Space, a slider takes the arrows.
    if (e.target !== e.currentTarget || e.ctrlKey || e.metaKey || e.altKey) return
    if (e.key === ' ') {
      e.preventDefault()
      player.toggle()
    } else if (e.key === 'ArrowLeft') {
      e.preventDefault()
      player.skip(-back)
    } else if (e.key === 'ArrowRight') {
      e.preventDefault()
      player.skip(forward)
    }
  }

  function setVolume(volume: number) {
    // Moving the slider is wanting to hear it.
    if (player.muted && volume > 0) player.toggleMute()
    player.setVolume(volume)
  }

  // ── Up Next ──────────────────────────────────────────────────────────

  /** What a row says besides its title: the show, how long, how far. */
  function meta(item: QueueItem): string {
    const seen = upnext.progressOf(item.entry.what) ?? item.consumption
    const total = item.entry.durationMs ?? item.play?.durationMs ?? seen?.durationMs ?? 0
    const parts: string[] = []
    if (player.isPlaying(item.entry.what)) parts.push('Playing')
    else if (player.isCurrent(item.entry.what)) parts.push('Paused')
    const source = item.sourceTitle || item.entry.source
    if (source) parts.push(source)
    if (seen?.finished) parts.push('Played')
    else if (total > 0) {
      const at = seen?.positionMs ?? 0
      parts.push(at > 0 ? formatRemaining(at, total) : formatDuration(total))
    }
    return parts.join(' · ')
  }

  /** The state of an episode that has to be downloaded before it plays. */
  function fetching(item: QueueItem): { text: string; failed: boolean } | null {
    const status = item.story ? upnext.downloadOf(item.story.id) : undefined
    if (status?.state === 'failed') {
      const why = status.error ? `: ${status.error}` : ''
      return { text: `Download failed${why}`, failed: true }
    }
    if (status?.state === 'downloading') {
      const share = status.total ? ` ${Math.round((status.received / status.total) * 100)}%` : ''
      return { text: `Downloading${share}`, failed: false }
    }
    if (status?.state === 'waiting') return { text: 'Waiting to download', failed: false }
    return item.downloading ? { text: 'Downloading', failed: false } : null
  }

  function rowIcon(item: QueueItem): IconName {
    if (item.entry.mode === 'watch') return 'film'
    if (item.entry.mode === 'listen') return item.entry.what.type === 'story' ? 'podcast' : 'music'
    return 'book'
  }

  /** What pressing a row does, in a word: an article is heard, not played. */
  function verb(item: QueueItem): string {
    return item.play ? 'Play' : 'Listen to'
  }

  // Reordering. Dragging a row, and for the keyboard and anybody who would
  // rather not drag, the arrows on each. Both end in `upnext.move`, which
  // takes the entry and the one it goes in front of -- none for the end.

  let dragging = $state<QueueEntryId | null>(null)
  let over = $state<{ id: QueueEntryId; after: boolean } | null>(null)

  function dragStart(e: DragEvent, id: QueueEntryId) {
    dragging = id
    if (!e.dataTransfer) return
    e.dataTransfer.effectAllowed = 'move'
    // WebKit starts no drag without something to carry.
    e.dataTransfer.setData('text/plain', id)
  }

  function dragOver(e: DragEvent, id: QueueEntryId) {
    if (dragging === null) return
    e.preventDefault()
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move'
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    const after = e.clientY > box.top + box.height / 2
    if (over?.id !== id || over.after !== after) over = { id, after }
  }

  function drop(e: DragEvent) {
    e.preventDefault()
    const moving = dragging
    const target = over
    dragging = null
    over = null
    if (moving === null || target === null) return
    const ids = upnext.items.map((i) => i.entry.id)
    const before = dropBefore(ids, moving, target.id, target.after)
    if (before !== undefined) void upnext.move(moving, before)
  }

  function endDrag() {
    dragging = null
    over = null
  }

  function moveUp(index: number) {
    const above = upnext.items[index - 1]
    const item = upnext.items[index]
    if (above && item) void upnext.move(item.entry.id, above.entry.id)
  }

  function moveDown(index: number) {
    const item = upnext.items[index]
    if (!item || index >= upnext.items.length - 1) return
    void upnext.move(item.entry.id, upnext.items[index + 2]?.entry.id ?? null)
  }
</script>

<!-- A landmark with the keyboard's Escape on it, not a dialog: see the header.
     Focus is moved in so Escape works the moment it opens. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<aside class="panel frost" aria-label="Player" tabindex="-1" use:focusOnMount {onkeydown}>
  <header class="head">
    <h2 class="pane-title">{current ? 'Now playing' : 'Player'}</h2>
    <button class="tool" aria-label="Close the player" title="Close (Esc)" onclick={close}>
      <Icon name="close" size={16} />
    </button>
  </header>

  {#if current}
    {#if player.video}
      <!-- Empty: the window's film is laid over it. See `PlayerHost`. -->
      <div class="picture" bind:this={picture} aria-hidden="true"></div>
    {/if}

    <div class="now">
      {#if !player.video}
        <div class="art">
          {#if artwork}
            <img src={artwork} alt="" onerror={() => (broken = artwork)} />
          {:else}
            <Icon name={glyph} size={28} weight={1.3} />
          {/if}
        </div>
      {/if}
      <div class="titles">
        <h3 class="title">{current.title}</h3>
        {#if current.source}<span class="source">{current.source}</span>{/if}
      </div>
    </div>

    {#if player.error}
      <p class="error" role="alert">
        {player.error}
        {#if files.local && current.what.type === 'file'}
          <button class="link" onclick={() => void files.openExternally(current.fileId)}>
            Open in another program
          </button>
        {/if}
      </p>
    {/if}

    <div class="scrubber">
      <input
        class="seek"
        type="range"
        min="0"
        max={duration}
        step="1000"
        value={position}
        disabled={duration <= 0}
        aria-label="Position"
        aria-valuetext={formatDuration(position)}
        oninput={(e) => (scrub = Number(e.currentTarget.value))}
        onchange={(e) => {
          player.seek(Number(e.currentTarget.value))
          scrub = null
        }}
      />
      <div class="times">
        <span>{formatDuration(position)}</span>
        {#if duration > 0}<span>{formatRemaining(position, duration)}</span>{/if}
      </div>
    </div>

    <div class="transport" class:busy={player.buffering}>
      <button
        class="skip"
        aria-label="Back {back} seconds"
        title="Back {back} seconds"
        onclick={() => player.skip(-back)}
      >
        <Icon name="skip-back" size={26} weight={1.4} />
        <span class="secs" aria-hidden="true">{back}</span>
      </button>
      <button
        class="play"
        aria-label={player.playing ? 'Pause' : 'Play'}
        title={player.playing ? 'Pause' : 'Play'}
        aria-busy={player.buffering}
        onclick={() => player.toggle()}
      >
        <Icon name={player.playing ? 'pause' : 'play'} size={24} weight={1.6} filled />
      </button>
      <button
        class="skip"
        aria-label="Forward {forward} seconds"
        title="Forward {forward} seconds"
        onclick={() => player.skip(forward)}
      >
        <Icon name="skip-forward" size={26} weight={1.4} />
        <span class="secs" aria-hidden="true">{forward}</span>
      </button>
    </div>

    <div class="extras">
      <div class="speeds" role="group" aria-label="Speed">
        {#each PLAYER_SPEEDS as s (s)}
          <button
            class="chip"
            class:on={player.speed === s}
            aria-pressed={player.speed === s}
            onclick={() => player.setSpeed(s)}
          >
            {s}×
          </button>
        {/each}
      </div>
      <div class="volume">
        <button
          class="tool"
          aria-label={player.muted ? 'Unmute' : 'Mute'}
          aria-pressed={player.muted}
          title={player.muted ? 'Unmute' : 'Mute'}
          onclick={() => player.toggleMute()}
        >
          <Icon name={player.muted || player.volume === 0 ? 'volume-off' : 'volume'} size={17} />
        </button>
        <input
          class="level"
          type="range"
          min="0"
          max="1"
          step="0.05"
          value={player.muted ? 0 : player.volume}
          aria-label="Volume"
          oninput={(e) => setVolume(Number(e.currentTarget.value))}
        />
      </div>
      <button
        class="btn btn-sm btn-outline"
        title="Mark it played and go on to the next"
        onclick={() => void player.finish()}
      >
        <Icon name="tick" size={14} /> Mark played
      </button>
      {#if askable}
        <button
          class="btn btn-sm btn-outline"
          disabled={voice.active}
          title={voice.active
            ? 'You are talking to the assistant'
            : 'Pause this and ask the assistant about it, out loud'}
          onclick={() => void voice.askAboutPlaying()}
        >
          <Icon name="mic" size={14} /> Ask about this
        </button>
      {/if}
    </div>
  {:else}
    <p class="hint idle">
      Nothing is playing. Press play on anything below, or on an episode, a recording or a film.
    </p>
  {/if}

  <section class="queue" aria-label="Up Next">
    <div class="queue-head">
      <h3 class="eyebrow">Up Next</h3>
      {#if upnext.items.length > 0}<span class="count">{upnext.items.length}</span>{/if}
    </div>

    {#if upnext.error}
      <p class="error" role="alert">{upnext.error}</p>
    {/if}

    {#if upnext.items.length === 0}
      <p class="hint empty">
        {#if upnext.loading}
          Loading…
        {:else}
          Nothing is queued. Add episodes, articles and files from the Feed, the Library or Files.
        {/if}
      </p>
    {:else}
      <ul class="rows scroll">
        {#each upnext.items as item, i (item.entry.id)}
          {@const entry = item.entry}
          {@const seen = upnext.progressOf(entry.what) ?? item.consumption}
          {@const download = fetching(item)}
          {@const art = item.file ? thumbnailUrl(item.file) : null}
          <li
            class="row list-row"
            class:sel={player.isCurrent(entry.what)}
            class:dragging={dragging === entry.id}
            class:before={over?.id === entry.id && !over.after}
            class:after={over?.id === entry.id && over.after}
            draggable="true"
            ondragstart={(e) => dragStart(e, entry.id)}
            ondragover={(e) => dragOver(e, entry.id)}
            ondrop={drop}
            ondragend={endDrag}
          >
            <span class="grip" aria-hidden="true"><Icon name="grip" size={14} /></span>
            <button
              class="main"
              title="{verb(item)} {entry.title}"
              aria-label="{verb(item)} {entry.title}"
              onclick={() => void player.playItem(item)}
            >
              <span class="thumb">
                {#if art}
                  <img src={art} alt="" />
                {:else}
                  <Icon name={rowIcon(item)} size={16} weight={1.4} />
                {/if}
              </span>
              <span class="text">
                <span class="name">{entry.title}</span>
                <span class="line">{meta(item)}</span>
                {#if download}
                  <span class="line fetch" class:failed={download.failed}>{download.text}</span>
                {/if}
                {#if seen && !seen.finished && seen.progress > 0}
                  <span
                    class="bar"
                    role="progressbar"
                    aria-label="Played"
                    aria-valuemin="0"
                    aria-valuemax="100"
                    aria-valuenow={Math.round(seen.progress * 100)}
                  >
                    <span style:width="{Math.round(seen.progress * 100)}%"></span>
                  </span>
                {/if}
              </span>
            </button>
            {#if download?.failed && item.story}
              <button
                class="icon-btn"
                aria-label="Try the download again"
                title="Try the download again"
                onclick={() => void upnext.download(item.story!.id)}
              >
                <Icon name="refresh" size={14} />
              </button>
            {/if}
            <div class="actions">
              <button
                class="icon-btn up"
                aria-label="Move {entry.title} up"
                title="Move up"
                disabled={i === 0}
                onclick={() => moveUp(i)}
              >
                <Icon name="chevron" size={14} />
              </button>
              <button
                class="icon-btn down"
                aria-label="Move {entry.title} down"
                title="Move down"
                disabled={i === upnext.items.length - 1}
                onclick={() => moveDown(i)}
              >
                <Icon name="chevron" size={14} />
              </button>
              <button
                class="icon-btn"
                aria-label="Remove {entry.title} from Up Next"
                title="Remove from Up Next"
                onclick={() => void upnext.remove(entry.id)}
              >
                <Icon name="close" size={14} />
              </button>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </section>
</aside>

<style>
  /* Over the right edge, under the top bar, above the apps and the assistant's
     button (30) and under a menu, a sheet or a toast. `frost` needs the
     position and the layer. */
  .panel {
    position: fixed;
    z-index: 45;
    top: calc(var(--topbar-h) + var(--sp-2));
    right: var(--sp-3);
    bottom: var(--sp-3);
    display: flex;
    flex-direction: column;
    width: min(400px, calc(100vw - var(--sp-6)));
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-lg);
    outline: none;
    overflow: hidden;
    animation: arrive var(--med) var(--ease);
  }
  @keyframes arrive {
    from {
      opacity: 0;
    }
  }

  .head {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-3) var(--sp-3) var(--sp-2) var(--sp-4);
  }
  .head .pane-title {
    flex: 1;
    min-width: 0;
    margin: 0;
  }

  .tool {
    flex: none;
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: 999px;
    color: var(--fg-muted);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .tool:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  /* The film is drawn over this. 16:9 by its width, and no taller than a
     third of the window so the controls and Up Next stay on screen. */
  .picture {
    flex: none;
    box-sizing: border-box;
    width: calc(100% - var(--sp-4) * 2);
    max-height: 34vh;
    margin: 0 var(--sp-4) var(--sp-3);
    aspect-ratio: 16 / 9;
  }

  .now {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: 0 var(--sp-4) var(--sp-3);
  }
  .art {
    flex: none;
    display: grid;
    place-items: center;
    width: 84px;
    height: 84px;
    overflow: hidden;
    border-radius: var(--radius);
    background: var(--bg-sunken);
    color: var(--fg-subtle);
  }
  .art img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .titles {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .title {
    margin: 0;
    display: -webkit-box;
    overflow: hidden;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    font-size: var(--text-md);
    font-weight: 650;
    line-height: var(--leading-tight);
    overflow-wrap: anywhere;
  }
  .source {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }

  .error {
    flex: none;
    margin: 0 var(--sp-4) var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--radius-sm);
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
    font-size: var(--text-sm);
    font-weight: 500;
  }
  .link {
    margin-left: var(--sp-1);
    color: inherit;
    font-size: inherit;
    text-decoration: underline;
  }

  .scrubber {
    flex: none;
    padding: 0 var(--sp-4);
  }
  .seek,
  .level {
    width: 100%;
    margin: 0;
    accent-color: var(--accent);
  }
  .seek:disabled {
    opacity: 0.4;
  }
  .times {
    display: flex;
    justify-content: space-between;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }

  .transport {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--sp-6);
    padding: var(--sp-2) var(--sp-4) var(--sp-3);
  }
  .skip {
    position: relative;
    display: grid;
    place-items: center;
    width: 44px;
    height: 44px;
    border-radius: 999px;
    color: var(--fg-muted);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .skip:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  /* The seconds, in the middle of the arrow. */
  .secs {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    padding-top: 1px;
    font-size: 9px;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }
  .play {
    display: grid;
    place-items: center;
    width: 56px;
    height: 56px;
    border-radius: 50%;
    background: var(--accent);
    color: var(--fg-on-accent);
    box-shadow: var(--shadow-sm);
    transition: background var(--fast) var(--ease);
  }
  .play:hover {
    background: var(--accent-hover);
  }
  /* Waiting for data: the button breathes rather than being replaced by a
     spinner that would move the controls. */
  .transport.busy .play {
    animation: breathe 1s ease-in-out infinite;
  }
  @keyframes breathe {
    50% {
      opacity: 0.55;
    }
  }

  .extras {
    flex: none;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2) var(--sp-3);
    padding: 0 var(--sp-4) var(--sp-3);
  }
  .speeds {
    display: flex;
    flex-wrap: wrap;
    gap: 3px;
    padding: 3px;
    border-radius: var(--radius);
    background: var(--bg-sunken);
  }
  .chip {
    height: 24px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-xs);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--fg-subtle);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .chip:hover {
    color: var(--fg);
  }
  .chip.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }
  .volume {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    min-width: 0;
    flex: 1 1 8rem;
  }
  .idle {
    flex: none;
    margin: 0;
    padding: var(--sp-2) var(--sp-4) var(--sp-4);
  }

  /* Up Next takes what is left, and is the only part that scrolls. */
  .queue {
    flex: 1;
    min-height: 7rem;
    display: flex;
    flex-direction: column;
    border-top: 1px solid var(--border);
  }
  .queue-head {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-3) var(--sp-4) var(--sp-1);
  }
  .queue-head .eyebrow {
    margin: 0;
  }
  .count {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  .empty {
    margin: 0;
    padding: var(--sp-2) var(--sp-4) var(--sp-4);
  }

  .rows {
    flex: 1;
    min-height: 0;
    margin: 0;
    padding: var(--sp-1) 0 var(--sp-3);
    overflow-y: auto;
    list-style: none;
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    padding: var(--sp-1) var(--sp-1) var(--sp-1) 0;
  }
  .row.dragging {
    opacity: 0.45;
  }
  /* Where a dropped row would land: a line along the edge it goes against. */
  .row.before::before,
  .row.after::after {
    content: '';
    position: absolute;
    left: var(--sp-2);
    right: var(--sp-2);
    height: 2px;
    border-radius: 1px;
    background: var(--accent);
  }
  .row.before::before {
    top: -1px;
  }
  .row.after::after {
    bottom: -1px;
  }
  .grip {
    flex: none;
    display: grid;
    place-items: center;
    width: 16px;
    color: var(--fg-faint);
    cursor: grab;
  }
  .main {
    flex: 1;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
    padding: var(--sp-1) 0;
    text-align: left;
  }
  .thumb {
    flex: none;
    display: grid;
    place-items: center;
    width: 38px;
    height: 38px;
    overflow: hidden;
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    color: var(--fg-subtle);
  }
  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .line {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .line.fetch {
    color: var(--accent);
  }
  .line.fetch.failed {
    color: var(--danger);
  }
  .bar {
    display: block;
    height: 2px;
    margin-top: 3px;
    border-radius: 1px;
    background: var(--bg-active);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--accent);
  }

  .actions {
    flex: none;
    display: flex;
    gap: 0;
  }
  .icon-btn {
    flex: none;
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .icon-btn:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .icon-btn:disabled {
    opacity: 0.3;
  }
  /* The chevron points right; these turn it up and down. */
  .icon-btn.up :global(svg) {
    transform: rotate(-90deg);
  }
  .icon-btn.down :global(svg) {
    transform: rotate(90deg);
  }

  @media (max-width: 520px) {
    .panel {
      top: var(--topbar-h);
      right: 0;
      bottom: 0;
      width: 100vw;
      border-radius: 0;
    }
  }
</style>
