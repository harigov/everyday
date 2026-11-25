<script lang="ts">
  // History: what was read, heard and watched, newest first, by day.
  //
  // A page of its own, whole width, as Up Next is -- it is a record to look
  // back over and not a list to read down, so there is no pane beside it. A
  // record carries its own title, source and address and outlives the story,
  // the file or the library item it was about, so each row is drawn from the
  // record alone and only asks the vault for the thing when it is opened.
  //
  // What a row can do: open the thing where it lives (a story in its own
  // source's list, a document in the reader, a recording or a film in Files, a
  // book in the Library); read the words the Feed kept from it, when it kept
  // any; and forget it. Forgetting is the one thing here that cannot be taken
  // back -- how far somebody got is gone with it, and the assistant no longer
  // knows they took it in -- so it asks first, as every destructive act does.
  //
  // The bar's search narrows what is loaded, by title, source and address. The
  // page is read in pages of a hundred; the next is a button at the foot.

  import { onMount } from 'svelte'
  import { feed } from '../lib/feed.svelte'
  import {
    filterHistory,
    groupHistory,
    historyMark,
    historyModeWord,
    historyProgress,
  } from '../lib/feed'
  import { files } from '../lib/files.svelte'
  import { plural, timeOfDay } from '../lib/format'
  import { library } from '../lib/library.svelte'
  import { openExternal } from '../lib/open-external'
  import { app } from '../lib/state.svelte'
  import type { Consumption } from '../lib/types'
  import { upnext } from '../lib/upnext.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'

  // While this page is on screen a `consumption` change reads it again; the
  // first read is the Feed's own, made when the sidebar selected it.
  onMount(() => {
    upnext.watchHistory(true)
    return () => upnext.watchHistory(false)
  })

  const query = $derived(feed.query.trim())
  const records = $derived(filterHistory(upnext.history, query))
  const days = $derived(groupHistory(records))
  const sub = $derived.by(() => {
    if (records.length === 0) return ''
    const count = plural(records.length, 'item')
    return query ? `${count} match` : count
  })

  // ── What each row says ────────────────────────────────────────────────

  function titleOf(record: Consumption): string {
    return record.title.trim() || 'Untitled'
  }

  /** The time of day it was last touched; empty for a record with no usable time. */
  function when(record: Consumption): string {
    const at = new Date(record.lastAt)
    return Number.isNaN(at.getTime()) ? '' : timeOfDay(at)
  }

  /** What was done, for a label and a tooltip: "Listened to", "Read", "Watched". */
  function word(record: Consumption): string {
    return historyModeWord(record.mode)
  }

  // ── Doing things ──────────────────────────────────────────────────────

  /**
   * Said under the page when a story could not be opened: its source was
   * unfollowed or it aged out, and the record is all that is left of it. The
   * address, when the record has one, is offered and not followed -- the page
   * it names is somebody's website, and opening it is a choice.
   */
  let gone = $state<{ text: string; url: string } | null>(null)

  /** Open the thing a record is about, where it lives. */
  async function open(record: Consumption) {
    gone = null
    const what = record.what
    if (what.type === 'story') {
      if (await feed.openInSource(what.id)) return
      gone = { text: `“${titleOf(record)}” is no longer in the Feed.`, url: record.url }
    } else if (what.type === 'item') {
      if (await app.goTo('library')) await library.open(what.id)
    } else if (record.mode === 'read') {
      // A document is read where the assistant is, not in a viewer of its own.
      void files.read(what.id)
    } else if (await app.goTo('files')) {
      files.open(what.id)
    }
  }

  let pending = $state<Consumption | null>(null)

  function confirmForget() {
    const doomed = pending
    pending = null
    if (doomed) void upnext.forget(doomed.id)
  }
</script>

<main class="page" style:--tint={feed.accent}>
  <div class="top">
    <div class="pane-titles">
      <span class="pane-title">History</span>
      {#if sub}<span class="pane-sub">{sub}</span>{/if}
    </div>
  </div>

  {#if upnext.historyError}
    <p class="hint wait" role="alert">{upnext.historyError}</p>
  {:else if upnext.historyLoading && upnext.history.length === 0}
    <p class="hint wait">Loading…</p>
  {:else if upnext.history.length === 0}
    <EmptyState lead="Nothing has been read or heard yet.">
      {#snippet icon()}<Icon name="clock" size={30} weight={1.3} />{/snippet}
      {#snippet note()}
        The articles you read, the episodes you play and the films you watch are remembered here, by
        day, so you can find them again.
      {/snippet}
    </EmptyState>
  {:else if records.length === 0}
    <EmptyState lead={`Nothing matches “${query}”.`}>
      {#snippet note()}Try fewer words, or the name of the show or the site.{/snippet}
    </EmptyState>
  {:else}
    <div class="scroll body">
      <div class="days">
        {#each days as day (day.day || day.label)}
          <section class="day" aria-label={day.label}>
            <h2 class="eyebrow day-head">{day.label}</h2>
            <ul class="records">
              {#each day.items as record (record.id)}
                {@const progress = historyProgress(record)}
                {@const at = when(record)}
                {@const kept = record.textFile}
                <li class="entry list-row">
                  <span class="mode" title={word(record)} role="img" aria-label={word(record)}>
                    <Icon name={historyMark(record)} size={18} />
                  </span>

                  <div class="text">
                    <button class="title" title="Open it" onclick={() => void open(record)}>
                      {titleOf(record)}
                    </button>
                    <div class="meta">
                      {#if record.source.trim()}
                        <span>{record.source.trim()}</span>
                        <span class="dotsep" aria-hidden="true">·</span>
                      {/if}
                      <span class:finished={progress.finished}>{progress.label}</span>
                      {#if at}
                        <span class="dotsep" aria-hidden="true">·</span>
                        <span>{at}</span>
                      {/if}
                    </div>
                    {#if !progress.finished && progress.fraction > 0}
                      <div
                        class="bar"
                        role="progressbar"
                        aria-label="How far {word(record).toLowerCase()} it"
                        aria-valuemin="0"
                        aria-valuemax="100"
                        aria-valuenow={Math.round(progress.fraction * 100)}
                      >
                        <span style:width="{progress.fraction * 100}%"></span>
                      </div>
                    {/if}
                  </div>

                  <div class="controls">
                    {#if kept}
                      <button
                        class="small"
                        title="Read the words the Feed kept from it"
                        aria-label="Read the words kept from “{titleOf(record)}”"
                        onclick={() => void files.read(kept)}
                      >
                        <Icon name="file" size={15} />
                      </button>
                    {/if}
                    <button
                      class="small"
                      title="Forget that you took it in"
                      aria-label="Forget “{titleOf(record)}”"
                      onclick={() => (pending = record)}
                    >
                      <Icon name="trash" size={14} />
                    </button>
                  </div>
                </li>
              {/each}
            </ul>
          </section>
        {/each}

        {#if upnext.historyMore}
          <!-- Not an end-of-list cue: a page is a hundred records, and a button
               says there is more as a scroll that never reaches it cannot. -->
          <button
            class="load-more"
            disabled={upnext.historyLoading}
            onclick={() => void upnext.loadMoreHistory()}
          >
            {upnext.historyLoading ? 'Loading…' : 'Load more'}
          </button>
        {/if}
      </div>
    </div>
  {/if}

  {#if gone}
    <p class="note" role="status">
      {gone.text}
      {#if gone.url}
        <button class="link" onclick={() => void openExternal(gone?.url ?? '')}>
          Open the original
        </button>
      {/if}
    </p>
  {/if}
</main>

{#if pending}
  <ConfirmDialog
    title={'Forget “' + titleOf(pending) + '”?'}
    detail="It is taken out of your history, with how far you got, and the assistant will not know you took it in. The story, the file and any words kept from it stay where they are."
    confirmLabel="Forget"
    onconfirm={confirmForget}
    oncancel={() => (pending = null)}
  />
{/if}

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
    font-size: var(--text-sm);
  }

  .body {
    flex: 1;
    min-height: 0;
    padding: var(--sp-2) 0 var(--sp-6);
  }
  /* A measure to read the record at: a title across a wide window is a long
     way from the buttons that belong to it. */
  .days {
    max-width: 780px;
    margin: 0 auto;
  }
  .day-head {
    margin: var(--sp-4) 0 var(--sp-1);
    padding: 0 var(--sp-4);
  }
  .records {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  /* The shared `.list-row` card; what is here is the record's own layout. */
  .entry {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-3);
    cursor: default;
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
  .meta .finished {
    color: var(--fg-faint);
  }
  .bar {
    height: 3px;
    margin-top: 3px;
    max-width: 240px;
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
  /* The row's small controls: quiet until the row is under the pointer or
     holds focus, and never out of the tab order. */
  .small {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
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
  .small:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .load-more {
    width: 100%;
    padding: var(--sp-3);
    text-align: center;
    color: var(--accent);
    font-size: var(--text-sm);
  }
  .load-more:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  .note {
    flex: none;
    margin: 0;
    padding: var(--sp-2) var(--sp-5);
    border-top: 1px solid var(--border);
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }
  .link {
    color: var(--accent);
    text-decoration: underline;
  }

  /* A phone: the row's small controls are always at full strength -- there is
     no pointer to bring them up -- and each is a finger's size. */
  :global([data-phone]) .page {
    min-width: 0;
  }
  :global([data-phone]) .top {
    padding-right: var(--sp-3);
    padding-left: var(--sp-4);
  }
  :global([data-phone]) .title {
    padding: 7px 0;
  }
  :global([data-phone]) .small {
    width: 44px;
    height: 44px;
    opacity: 1;
  }
  :global([data-phone]) .load-more {
    min-height: 48px;
  }
</style>
