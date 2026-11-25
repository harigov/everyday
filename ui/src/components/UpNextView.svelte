<script lang="ts">
  // Up Next: what is lined up to hear, watch and read, in the order it will be
  // taken in.
  //
  // A page of its own, not a list with a reading pane beside it: the point of a
  // queue is that nothing has to be picked, so what it needs is room to see the
  // order, change it, and press Play. An episode, a file in Files and an
  // article all stand in it the same way; what differs is what pressing the
  // round button does -- play it, get the file first, or open it to read.
  //
  // The order is the service's, and the page only asks for changes to it: a
  // drag, or the arrows beside a row, or Alt and the arrow keys on one, all
  // say "move this just before that" and wait for the queue to be sent back.
  // Where each entry came from is said quietly and only when it was not the
  // person's own doing, because "you added it" is what everything in a queue
  // is assumed to be.

  import { tick } from 'svelte'
  import { feed } from '../lib/feed.svelte'
  import { files } from '../lib/files.svelte'
  import { plural, relativeTime } from '../lib/format'
  import type { IconName } from '../lib/icons'
  import { library } from '../lib/library.svelte'
  import { listening } from '../lib/listen.svelte'
  import { menu } from '../lib/menu.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { phone } from '../lib/phone.svelte'
  import { player } from '../lib/player.svelte'
  import {
    addedByLabel,
    downloadLine,
    downloadPercent,
    dropBefore,
    episodeProgress,
    formatSpan,
    queueRemainingMs,
  } from '../lib/podcast'
  import { app } from '../lib/state.svelte'
  import type { ConsumeMode, QueueEntryId, QueueItem } from '../lib/types'
  import { upnext } from '../lib/upnext.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'

  // Loaded when the page opens: the sidebar's count is read from the same
  // list, and a change made on another device lands through `live`, but a
  // page opened after a long while should not wait to be told.
  void upnext.load()

  const items = $derived(upnext.items)
  const remaining = $derived(queueRemainingMs(items))
  const sub = $derived(
    items.length === 0
      ? ''
      : [plural(items.length, 'item'), remaining > 0 ? `${formatSpan(remaining)} to go` : '']
          .filter(Boolean)
          .join(' · '),
  )

  // ── What each entry is ────────────────────────────────────────────────

  function titleOf(item: QueueItem): string {
    return item.entry.title.trim() || item.story?.title.trim() || 'Untitled'
  }

  /**
   * How an entry is taken in. What there is to play is heard or watched
   * whatever it was lined up as: an article saved as audio since it was added
   * is still an article entry, and now it plays.
   */
  function modeOf(item: QueueItem): ConsumeMode {
    if (item.play) return item.play.kind === 'video' ? 'watch' : 'listen'
    return item.entry.mode
  }

  /** The mark in front of a row: how it is taken in. */
  function modeIcon(item: QueueItem): IconName {
    const mode = modeOf(item)
    if (mode === 'watch') return 'film'
    if (mode === 'listen') {
      if (item.story?.media) return 'podcast'
      return item.story ? 'volume' : 'music'
    }
    return item.item && !item.story ? 'book' : 'newspaper'
  }

  /** The mark, or a speaker while it is the thing being heard. */
  function markIcon(item: QueueItem, now: boolean): IconName {
    return now && modeOf(item) !== 'read' ? 'volume' : modeIcon(item)
  }

  function modeWord(item: QueueItem): string {
    const mode = modeOf(item)
    return mode === 'watch' ? 'To watch' : mode === 'listen' ? 'To listen to' : 'To read'
  }

  /** Reading it aloud, through Listen: an article's way of being played. */
  function isReading(item: QueueItem): boolean {
    return !!item.story && listening.isReading({ type: 'story', storyId: item.story.id })
  }

  function isNow(item: QueueItem): boolean {
    return player.isCurrent(item.entry.what) || isReading(item)
  }

  /** An episode whose file is not here yet: it has to come first. */
  function needsDownload(item: QueueItem): boolean {
    return !item.play && !!item.story?.media && !item.story.files?.media
  }

  /** "Added by you, 3 days ago": said on hover for every entry, quiet or not. */
  function whenAdded(item: QueueItem): string {
    return `${addedByLabel(item.entry.addedBy)}, ${relativeTime(item.entry.addedAt)}`
  }

  /** What the line under a title says: whose it is, how long, how far, and where it came from. */
  function facts(item: QueueItem): string[] {
    const out: string[] = []
    if (item.sourceTitle.trim()) out.push(item.sourceTitle.trim())
    const length = item.entry.durationMs ?? item.consumption?.durationMs ?? null
    const progress = episodeProgress(item.consumption, length)
    if (progress) out.push(progress.label)
    else if (length) out.push(formatSpan(length))
    // What was made of it in Files: its words written down, an article's voice.
    if (item.story?.files?.transcript) out.push('Has a transcript')
    if (item.story && !item.story.media && item.story.files?.audio) out.push('Saved as audio')
    if (item.entry.addedBy !== 'person') out.push(addedByLabel(item.entry.addedBy))
    return out
  }

  // ── Doing things ──────────────────────────────────────────────────────

  function playAll() {
    const first = items[0]
    if (first) void player.playItem(first)
  }

  /**
   * Open an entry where it lives: a story in its source, a library item on its
   * shelf, a file in Files. A file that is to be heard or watched has nowhere
   * better to be opened than the player.
   */
  async function open(item: QueueItem) {
    const what = item.entry.what
    if (item.story) {
      await feed.select({ kind: 'source', id: item.story.sourceId })
      await feed.open(item.story.id)
    } else if (item.item) {
      if (await app.goTo('library')) await library.open(item.item.id)
    } else if (what.type === 'file' && modeOf(item) === 'read') {
      if (await app.goTo('files')) files.open(what.id)
    } else {
      void player.playItem(item)
    }
  }

  /** The round button: play or pause it, get it, or open it to read. */
  function primary(item: QueueItem) {
    if (modeOf(item) === 'read') {
      void open(item)
    } else if (needsDownload(item) && item.story) {
      if (item.downloading) void upnext.cancelDownload(item.story.id)
      else void upnext.download(item.story.id)
    } else if (isReading(item)) {
      listening.stop()
    } else if (player.isCurrent(item.entry.what)) {
      player.toggle()
    } else {
      void player.playItem(item)
    }
  }

  function primaryIcon(item: QueueItem): IconName {
    if (modeOf(item) === 'read') return item.item && !item.story ? 'book' : 'newspaper'
    if (needsDownload(item)) return item.downloading ? 'close' : 'download'
    if (isReading(item) || player.isPlaying(item.entry.what)) {
      return isReading(item) ? 'stop' : 'pause'
    }
    return 'play'
  }

  function primaryLabel(item: QueueItem): string {
    const name = titleOf(item)
    if (modeOf(item) === 'read') return `Open “${name}” to read`
    if (needsDownload(item)) {
      return item.downloading ? `Cancel the download of “${name}”` : `Download “${name}”`
    }
    if (isReading(item)) return `Stop reading “${name}” aloud`
    if (player.isPlaying(item.entry.what)) return `Pause “${name}”`
    return `Play “${name}”`
  }

  /** "Downloading 43%" for an episode on its way; empty for anything else. */
  function downloadNote(item: QueueItem): string {
    if (!needsDownload(item)) return ''
    const status = item.story ? upnext.downloadOf(item.story.id) : undefined
    if (status) return downloadLine(status)
    return item.downloading ? 'Downloading' : 'Not downloaded yet'
  }

  function percent(item: QueueItem): number | null {
    const status = item.story ? upnext.downloadOf(item.story.id) : undefined
    return downloadPercent(status)
  }

  // ── Changing the order ────────────────────────────────────────────────

  let list = $state<HTMLElement | null>(null)

  /** After a move the row is given focus again: a node that is moved loses it. */
  async function refocus(id: QueueEntryId) {
    await tick()
    list?.querySelector<HTMLElement>(`[data-entry="${CSS.escape(id)}"] .title`)?.focus()
  }

  async function shift(item: QueueItem, delta: -1 | 1, keyboard = false) {
    const ids = items.map((i) => i.entry.id)
    const at = ids.indexOf(item.entry.id)
    const to = at + delta
    if (at < 0 || to < 0 || to >= ids.length) return
    // "Just before": above is the one that was above; below is the one two
    // down, or the end when there are none.
    const before = delta < 0 ? ids[to]! : (ids[to + 1] ?? null)
    await upnext.move(item.entry.id, before)
    if (keyboard) await refocus(item.entry.id)
  }

  /**
   * What a row's arrows and cross do, as a menu: on a phone they are this
   * one button -- or a long press on the row -- and not three small ones that
   * would take the title's room.
   */
  function entryMenu(item: QueueItem, at: number): MenuItem[] {
    return tidyMenu([
      {
        label: 'Move up',
        disabled: at === 0,
        run: () => void shift(item, -1),
      },
      {
        label: 'Move down',
        disabled: at === items.length - 1,
        run: () => void shift(item, 1),
      },
      SEP,
      {
        label: 'Take it out of Up Next',
        icon: 'close',
        run: () => void upnext.remove(item.entry.id),
      },
    ])
  }

  let dragging = $state<QueueEntryId | null>(null)
  let over = $state<{ id: QueueEntryId; after: boolean } | null>(null)

  function dragStart(e: DragEvent, item: QueueItem) {
    dragging = item.entry.id
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = 'move'
      // Some webviews will not start a drag with nothing to carry.
      e.dataTransfer.setData('text/plain', titleOf(item))
    }
  }

  function dragOver(e: DragEvent, item: QueueItem) {
    if (!dragging) return
    e.preventDefault()
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move'
    const box = (e.currentTarget as HTMLElement).getBoundingClientRect()
    over = { id: item.entry.id, after: e.clientY > box.top + box.height / 2 }
  }

  function dragEnd() {
    dragging = null
    over = null
  }

  function drop(e: DragEvent) {
    e.preventDefault()
    const moved = dragging
    const target = over
    dragEnd()
    if (!moved || !target) return
    const before = dropBefore(
      items.map((i) => i.entry.id),
      moved,
      target.id,
      target.after,
    )
    if (before !== undefined) void upnext.move(moved, before)
  }

  // ── The keyboard ──────────────────────────────────────────────────────
  //
  // The arrow keys walk the titles, one tab stop each; with Alt they move the
  // row. Kept on the titles and not the whole list so a press in a button
  // beside them is that button's own.

  function titleKeys(e: KeyboardEvent, item: QueueItem) {
    if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return
    const delta = e.key === 'ArrowUp' ? -1 : 1
    e.preventDefault()
    if (e.altKey) {
      void shift(item, delta, true)
      return
    }
    const titles = [...(list?.querySelectorAll<HTMLElement>('.title') ?? [])]
    titles[titles.indexOf(e.currentTarget as HTMLElement) + delta]?.focus()
  }
</script>

<main class="page" style:--tint={feed.accent}>
  <div class="top">
    <div class="pane-titles">
      <span class="pane-title">Up Next</span>
      {#if sub}<span class="pane-sub">{sub}</span>{/if}
    </div>
    {#if items.length > 0}
      <button class="btn btn-primary" onclick={playAll}>
        <Icon name="play" size={15} filled /> Play all
      </button>
    {/if}
  </div>

  {#if upnext.loading && items.length === 0}
    <p class="hint wait">Loading…</p>
  {:else if items.length === 0}
    <EmptyState lead="Nothing is lined up.">
      {#snippet icon()}<Icon name="queue-add" size={30} weight={1.3} />{/snippet}
      {#snippet note()}
        Put an episode, a file or an article here with the queue button beside it, and it plays when
        the one before it ends. Recommended has a few to start with.
      {/snippet}
      {#snippet action()}
        <div class="actions">
          <button class="btn btn-primary" onclick={() => void feed.select({ kind: 'recommended' })}>
            See what is recommended
          </button>
          <button class="btn" onclick={() => feed.openDirectory('podcasts')}>
            Browse podcasts
          </button>
        </div>
      {/snippet}
    </EmptyState>
  {:else}
    <div class="scroll body">
      <ol
        class="queue"
        aria-label="Up Next, in order"
        bind:this={list}
        ondragleave={(e) => {
          // Out of the list altogether, not just from one row to the next.
          if (!list?.contains(e.relatedTarget as Node | null)) over = null
        }}
      >
        {#each items as item, i (item.entry.id)}
          {@const now = isNow(item)}
          {@const note = downloadNote(item)}
          {@const pct = percent(item)}
          {@const progress = episodeProgress(item.consumption, item.entry.durationMs)}
          {@const icon = primaryIcon(item)}
          <!-- Draggable by the whole row: a handle is the one place a drag would
               have to be found, and the arrows beside it are for whoever
               cannot or would rather not. -->
          <li
            class="entry list-row"
            class:sel={now}
            class:dragging={dragging === item.entry.id}
            class:before={over?.id === item.entry.id && !over.after}
            class:after={over?.id === item.entry.id && over.after}
            data-entry={item.entry.id}
            draggable={!phone.on}
            oncontextmenu={phone.on ? (e) => menu.show(e, entryMenu(item, i)) : undefined}
            ondragstart={(e) => dragStart(e, item)}
            ondragover={(e) => dragOver(e, item)}
            ondrop={drop}
            ondragend={dragEnd}
          >
            <span class="grip" aria-hidden="true"><Icon name="grip" size={14} /></span>
            <span class="mode" title={modeWord(item)} role="img" aria-label={modeWord(item)}>
              <Icon name={markIcon(item, now)} size={18} />
            </span>

            <div class="text">
              <button
                class="title"
                class:read={!!item.consumption?.finished}
                title="Open it"
                onclick={() => void open(item)}
                onkeydown={(e) => titleKeys(e, item)}
              >
                {titleOf(item)}
              </button>
              <div class="meta" title={whenAdded(item)}>
                {#each facts(item) as fact, n (n)}
                  {#if n > 0}<span class="dotsep" aria-hidden="true">·</span>{/if}
                  <span>{fact}</span>
                {/each}
                {#if note}
                  <span class="dotsep" aria-hidden="true">·</span>
                  <span class="note">{note}</span>
                {/if}
              </div>
              {#if progress && !progress.finished}
                <div
                  class="bar"
                  role="progressbar"
                  aria-label="How much has been played"
                  aria-valuemin="0"
                  aria-valuemax="100"
                  aria-valuenow={Math.round(progress.fraction * 100)}
                >
                  <span style:width="{progress.fraction * 100}%"></span>
                </div>
              {:else if pct !== null}
                <div
                  class="bar"
                  role="progressbar"
                  aria-label="How much has been downloaded"
                  aria-valuemin="0"
                  aria-valuemax="100"
                  aria-valuenow={pct}
                >
                  <span style:width="{pct}%"></span>
                </div>
              {/if}
            </div>

            <div class="controls">
              <button
                class="round"
                class:go={!needsDownload(item) && modeOf(item) !== 'read'}
                title={primaryLabel(item)}
                aria-label={primaryLabel(item)}
                onclick={() => primary(item)}
              >
                <Icon name={icon} size={15} filled={icon === 'play' || icon === 'pause'} />
              </button>
              <button
                class="small more phone-only"
                data-menu-trigger
                aria-label="More for “{titleOf(item)}”"
                aria-haspopup="menu"
                aria-expanded={menu.at !== null}
                onclick={(e) => (menu.at ? menu.close() : menu.show(e, entryMenu(item, i)))}
              >
                <Icon name="more" size={18} />
              </button>
              <button
                class="small move"
                title="Move up (Alt and the up arrow)"
                aria-label="Move “{titleOf(item)}” up"
                disabled={i === 0}
                onclick={() => void shift(item, -1)}
              >
                <span class="up"><Icon name="chevron" size={14} /></span>
              </button>
              <button
                class="small move"
                title="Move down (Alt and the down arrow)"
                aria-label="Move “{titleOf(item)}” down"
                disabled={i === items.length - 1}
                onclick={() => void shift(item, 1)}
              >
                <span class="down"><Icon name="chevron" size={14} /></span>
              </button>
              <button
                class="small"
                title="Take it out of Up Next"
                aria-label="Take “{titleOf(item)}” out of Up Next"
                onclick={() => void upnext.remove(item.entry.id)}
              >
                <Icon name="close" size={14} />
              </button>
            </div>
          </li>
        {/each}
      </ol>
    </div>
  {/if}
</main>

<style>
  .page {
    /* As tall as the stage by a stated height, not stretched: see
       `.shell-stage` in app.css. */
    height: 100%;
    flex: 1;
    min-width: 360px;
    display: flex;
    flex-direction: column;
  }
  .top {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: var(--header-h);
    padding: var(--sp-2) var(--sp-5) var(--sp-1);
    flex: none;
  }
  .wait {
    padding: var(--sp-4) var(--sp-5);
    color: var(--fg-faint);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: var(--sp-2);
  }

  .body {
    flex: 1;
    min-height: 0;
    padding: var(--sp-2) 0 var(--sp-6);
  }
  /* A measure to read the queue at: a title across a wide window is a long
     way from the buttons that belong to it. */
  .queue {
    max-width: 780px;
    margin: 0 auto;
    padding: 0;
    list-style: none;
  }

  /* The shared `.list-row` card; what is here is the queue row's own layout.
     A row being played is `.sel`, the accent tint every open thing has. */
  .entry {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-3) var(--sp-3) var(--sp-3) var(--sp-2);
    cursor: default;
  }
  .entry.dragging {
    opacity: 0.45;
  }
  /* Where a dropped row would land: a line in the accent, on the edge it
     would sit against. */
  .entry.before::before,
  .entry.after::after {
    content: '';
    position: absolute;
    left: var(--sp-3);
    right: var(--sp-3);
    height: 2px;
    border-radius: 1px;
    background: var(--accent);
    pointer-events: none;
  }
  .entry.before::before {
    top: -1px;
  }
  .entry.after::after {
    bottom: -1px;
  }

  .grip {
    flex: none;
    display: grid;
    place-items: center;
    width: 14px;
    color: var(--fg-faint);
    cursor: grab;
    opacity: 0.6;
  }
  .entry:hover .grip {
    opacity: 1;
  }
  .mode {
    flex: none;
    display: grid;
    place-items: center;
    width: 38px;
    height: 38px;
    border-radius: var(--radius);
    background: var(--bg-sunken);
    color: var(--fg-muted);
  }
  .entry.sel .mode {
    background: color-mix(in oklab, var(--accent) 14%, transparent);
    color: var(--accent);
  }

  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .title {
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    width: fit-content;
    max-width: 100%;
    text-align: left;
    font-size: var(--text-base);
    font-weight: 620;
    line-height: 1.3;
    color: var(--fg);
    overflow-wrap: anywhere;
  }
  .title:hover {
    color: var(--accent);
  }
  /* Finished but still lined up: quieter, not struck through. */
  .title.read {
    font-weight: 520;
    color: var(--fg-muted);
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0 6px;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    font-variant-numeric: tabular-nums;
  }
  .dotsep {
    color: var(--fg-faint);
  }
  .note {
    color: var(--accent);
  }
  .bar {
    height: 3px;
    margin-top: 3px;
    border-radius: 2px;
    background: var(--border);
    overflow: hidden;
  }
  .bar > span {
    display: block;
    height: 100%;
    background: var(--accent);
  }

  .controls {
    flex: none;
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .round {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    margin-right: var(--sp-1);
    border: 1px solid var(--border-strong);
    border-radius: 50%;
    background: var(--bg-raised);
    color: var(--fg-muted);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .round:hover {
    color: var(--fg);
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
  /* The row's small controls: quiet until the row is under the pointer or
     holds focus, and never out of the tab order. */
  .small {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    color: var(--fg-faint);
    opacity: 0.55;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease),
      opacity var(--fast) var(--ease);
  }
  .entry:hover .small,
  .entry:focus-within .small {
    opacity: 1;
  }
  .small:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .small:disabled {
    opacity: 0.2;
    pointer-events: none;
  }
  /* The chevron points right, as it does in the sidebar; turned up and down. */
  .up {
    display: grid;
    transform: rotate(-90deg);
  }
  .down {
    display: grid;
    transform: rotate(90deg);
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  :global([data-phone]) .page {
    min-width: 0;
  }
  :global([data-phone]) .top {
    padding: var(--sp-2) var(--sp-4) var(--sp-1);
  }
  :global([data-phone]) .body {
    padding-bottom: var(--fab-clear);
  }
  /* A row: what it is, its title over its facts, the round control, and one
     button for everything else -- no handle to drag by, and three small
     buttons would leave the title a sliver. */
  :global([data-phone]) .entry {
    min-height: 68px;
    padding: var(--sp-2) var(--sp-1) var(--sp-2) var(--sp-3);
  }
  :global([data-phone]) .grip,
  :global([data-phone]) .small:not(.more) {
    display: none;
  }
  :global([data-phone]) .round {
    width: 44px;
    height: 44px;
    margin-right: 0;
  }
  :global([data-phone]) .small.more {
    width: 44px;
    height: 44px;
    opacity: 1;
  }

  :global([data-phone]) .title {
    padding: 6px 0;
  }
</style>
