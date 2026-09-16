<!--
  A recording that is not a note yet: still moving through the pipeline, or
  stopped on a failure. Drawn above the ordinary notes list in `NotesNav`,
  because that is where the note it becomes will eventually sit -- and a
  recording that failed belongs beside the notes, not buried in a settings
  screen nobody thinks to check after a call.
-->
<script lang="ts">
  import { relativeTime } from '../lib/format'
  import { stageLabel } from '../lib/meetings-format'
  import { meetings } from '../lib/meetings.svelte'
  import type { Recording } from '../lib/types'
  import Icon from './Icon.svelte'

  let { recording }: { recording: Recording } = $props()

  const failed = $derived(recording.stage.type === 'failed')
  const reason = $derived(recording.stage.type === 'failed' ? recording.stage.reason : null)

  let busy = $state(false)

  async function retry() {
    busy = true
    try {
      await meetings.retryRecording(recording.id)
    } finally {
      busy = false
    }
  }

  async function discard() {
    busy = true
    try {
      await meetings.discardRecording(recording.id)
    } finally {
      busy = false
    }
  }
</script>

<div class="card" class:failed>
  <div class="head">
    {#if failed}
      <Icon name="alert" size={14} />
    {:else}
      <span class="spinner" aria-hidden="true"></span>
    {/if}
    <span class="title">{recording.title}</span>
  </div>
  <p class="stage">
    {stageLabel(recording.stage)} · started {relativeTime(recording.startedAt)}
  </p>
  {#if failed && reason}
    <p class="reason">{reason}</p>
    <div class="actions">
      <button class="btn" disabled={busy} onclick={() => void discard()}>Discard</button>
      <button class="btn btn-primary" disabled={busy} onclick={() => void retry()}>Retry</button>
    </div>
  {/if}
</div>

<style>
  .card {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: var(--sp-2) var(--sp-3);
    margin: 0 var(--sp-2) var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
  }
  .card.failed {
    border-color: color-mix(in oklab, var(--danger) 30%, var(--border));
    background: color-mix(in oklab, var(--danger) 6%, var(--bg));
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    color: var(--danger);
  }
  .card:not(.failed) .head {
    color: var(--fg-muted);
  }

  .title {
    overflow: hidden;
    color: var(--fg);
    font-size: var(--text-sm);
    font-weight: 600;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .stage {
    margin: 0;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }
  .reason {
    margin: 0;
    color: var(--fg-muted);
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--sp-2);
    margin-top: var(--sp-1);
  }

  .spinner {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 50%;
    border: 1.5px solid var(--fg-faint);
    border-top-color: var(--accent);
    animation: spin 0.8s linear infinite;
  }
  @media (prefers-reduced-motion: reduce) {
    .spinner {
      animation: none;
    }
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
