<script lang="ts">
  // One field, one Enter, one reading.
  //
  // The shape the todo app's capture line already established, and for the
  // same reason: adding one thing should cost one line, and adding twelve
  // should cost twelve lines and twelve Enters. That rules out a dialog with
  // a name, a kind, a unit and a ceiling in it — which is exactly what
  // defining a tracker in a settings panel is, and exactly why a structured
  // note never gets made in the moment.
  //
  // Two things teach the grammar. The hint under the field says what is
  // possible; the preview above the button says what *this* line means, and
  // the preview is the one that works. "New tracker “swim” (in min) — 60"
  // is the only moment a wrong guess can be caught before it is made.
  //
  // The trackers already in the vault are listed below, because the
  // commonest log is of something logged before and a click beats typing.

  import { QUICK_TRACK_HINT, describeQuickTrack, parseQuickTrack } from '../lib/quicktrack'
  import { tracking } from '../lib/tracking.svelte'
  import { formatValue } from '../lib/tracker'
  import type { EntryId, JournalId, Tracker } from '../lib/types'
  import Icon from './Icon.svelte'
  import TrackerIcon from './TrackerIcon.svelte'

  interface Props {
    /** Where it was ticked. Both absent from the Overview and the tray. */
    journalId?: JournalId | null
    entryId?: EntryId | null
    /** The day to file it under. Defaults to whatever day the store is on. */
    date?: string
    onclose?: () => void
  }

  const { journalId = null, entryId = null, date, onclose }: Props = $props()

  let draft = $state('')
  /** A note under the field after something landed. Cleared on the next keystroke. */
  let said = $state('')

  // Queued, not guarded. A `busy` flag would silently eat the second line of
  // anybody typing faster than the disk, which is the bug `QuickAdd` was
  // written to avoid.
  let chain: Promise<unknown> = Promise.resolve()
  let field = $state<HTMLInputElement | null>(null)

  const parsed = $derived(parseQuickTrack(draft, tracking.trackers))
  const preview = $derived(describeQuickTrack(parsed))
  const ready = $derived(!!parsed.target)

  function submit() {
    const line = draft.trim()
    if (!line || !ready) return
    // Cleared synchronously, before any await: the field has to be empty and
    // focused by the time the next character arrives.
    draft = ''
    said = ''
    chain = chain
      .then(() => tracking.logLine(line, { journalId, entryId, date }))
      .then((done) => {
        if (!done) return
        said = done.created
          ? `Made “${done.tracker.name}” and recorded it.`
          : `Recorded ${done.tracker.name}.`
      })
      .catch(() => {
        /* already reported by the store's one error policy */
      })
      .finally(() => field?.focus())
  }

  /** One click for something logged before, at whatever it usually is. */
  function quick(tracker: Tracker) {
    said = ''
    chain = chain
      .then(() => tracking.log(tracker, tracker.defaultValue))
      .then(() => {
        said = `Recorded ${tracker.name}.`
      })
      .catch(() => {})
      .finally(() => field?.focus())
  }

  export function focus() {
    field?.focus()
  }
</script>

<div class="log">
  <div class="line">
    <Icon name="plus" size={14} />
    <!-- Focused on open: this popover exists to be typed into. -->
    <!-- svelte-ignore a11y_autofocus -->
    <input
      autofocus
      bind:this={field}
      bind:value={draft}
      placeholder="swim 60min"
      aria-label="Record a reading"
      oninput={() => (said = '')}
      onkeydown={(e) => {
        if (e.key === 'Enter') {
          e.preventDefault()
          submit()
        } else if (e.key === 'Escape') {
          e.preventDefault()
          onclose?.()
        }
      }}
    />
    <button class="go" disabled={!ready} onclick={submit} title="Record it (Enter)">
      <Icon name="check" size={14} />
    </button>
  </div>

  <!-- The preview replaces the hint the moment the line means something,
       exactly as the quick-add line's parsed chips replace its hint. -->
  {#if preview}
    <p class="preview" class:new={parsed.target?.kind === 'new'} aria-live="polite">{preview}</p>
  {:else if said}
    <p class="preview said" aria-live="polite">{said}</p>
  {:else}
    <p class="hint">{QUICK_TRACK_HINT}</p>
  {/if}

  {#if tracking.live.length > 0}
    <div class="known">
      {#each tracking.live as tracker (tracker.id)}
        <button
          class="chip"
          style="--c: {tracker.color}"
          title="Record {tracker.name} at {formatValue(tracker, tracker.defaultValue)}"
          onclick={() => quick(tracker)}
        >
          <TrackerIcon name={tracker.icon} color={tracker.color} size={18} />
          <span class="cname">{tracker.name}</span>
        </button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .log {
    display: flex;
    min-width: 280px;
    max-width: 340px;
    flex-direction: column;
    padding: var(--sp-2);
  }

  .line {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    height: 32px;
    padding: 0 var(--sp-1) 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg-faint);
  }

  .line input {
    flex: 1 1 auto;
    min-width: 0;
    border: 0;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-sm);
  }

  .line input:focus {
    outline: none;
  }

  .go {
    display: grid;
    flex: none;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: var(--radius-sm);
    color: var(--fg-muted);
  }

  .go:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .go:not(:disabled):hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .hint,
  .preview {
    margin: var(--sp-2) 0 0;
    padding: 0 var(--sp-1);
    color: var(--fg-faint);
    font-size: var(--text-xs);
    line-height: var(--leading);
  }

  .preview {
    color: var(--fg-muted);
  }

  /* A line that would make something new says so louder than one that would
     not: it is the only case worth reading twice. */
  .preview.new {
    color: var(--fg);
    font-weight: 550;
  }

  .preview.said {
    color: var(--fg-faint);
  }

  .known {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1);
    margin-top: var(--sp-2);
    padding-top: var(--sp-2);
    border-top: 1px solid var(--border);
  }

  .chip {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    padding: 2px var(--sp-2) 2px 2px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: none;
  }

  .chip:hover {
    border-color: var(--c);
    background: var(--bg-hover);
  }

  .cname {
    font-size: var(--text-xs);
  }
</style>
