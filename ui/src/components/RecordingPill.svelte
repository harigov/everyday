<!--
  The recording pill: a live indicator in the app bar while a call is being
  recorded, and the controls for it.

  A compact button rather than a full card, because the app bar is a 90px
  rail and every other button in it is an icon and a one-word label. The full
  detail -- both level meters, the warnings, Stop and Discard -- lives in a
  popover raised from it, the same way a tray entry's submenu is more than
  its row. The one thing that does *not* wait for the popover to open is
  "still on the call?": that is a decision, not a status, and it is drawn as
  its own small dialog so it is not missed because nobody had the popover
  open.
-->
<script lang="ts">
  import { formatTimer, SYSTEM_SILENT_WARN_MS } from '../lib/meetings-format'
  import { meetings } from '../lib/meetings.svelte'
  import { timeOfDay } from '../lib/format'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import Icon from './Icon.svelte'
  import Meter from './Meter.svelte'

  let open = $state(false)
  let confirmingDiscard = $state(false)

  const capture = $derived(meetings.capture)
  const stillOn = $derived(meetings.stillOn != null && meetings.stillOn === capture?.recordingId)

  async function stop() {
    open = false
    await meetings.stopCapture(false)
  }

  async function discard() {
    confirmingDiscard = false
    open = false
    await meetings.stopCapture(true)
  }

  async function keepRecording() {
    meetings.stillOn = null
  }
</script>

{#if capture}
  <div class="wrap">
    <button
      class="pill"
      class:open
      onclick={() => (open = !open)}
      aria-expanded={open}
      aria-label={`Recording ${capture.title}, ${formatTimer(capture.elapsedMs)}`}
    >
      <span class="dot" aria-hidden="true"></span>
      <span class="barlabel">{formatTimer(capture.elapsedMs)}</span>
    </button>

    {#if open}
      <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
      <div class="scrim" onclick={() => (open = false)}></div>
      <div class="panel" role="dialog" aria-label="Recording">
        <header>
          <span class="dot" aria-hidden="true"></span>
          <span class="title">{capture.title}</span>
          <span class="timer">{formatTimer(capture.elapsedMs)}</span>
        </header>

        {#if capture.automatic}
          <p class="badge">Recording automatically</p>
        {/if}

        <div class="meters">
          <Meter label="You" value={capture.micLevel} color="var(--accent)" />
          <Meter label="Call" value={capture.systemLevel} color="var(--accent-2, var(--accent))" />
        </div>

        {#if capture.systemSilentMs > SYSTEM_SILENT_WARN_MS}
          <p class="warn">
            <Icon name="alert" size={14} /> Not hearing the call. Using speakers? Headphones give cleaner
            notes.
          </p>
        {/if}
        {#if capture.systemUnavailable}
          <p class="warn"><Icon name="alert" size={14} /> {capture.systemUnavailable}</p>
        {/if}
        {#if capture.autoStopAt}
          <p class="hint">Stops automatically at {timeOfDay(new Date(capture.autoStopAt))}.</p>
        {/if}

        <div class="actions">
          <button class="btn btn-danger" onclick={() => (confirmingDiscard = true)}>
            <Icon name="trash" size={14} /> Discard
          </button>
          <button class="btn btn-primary" onclick={() => void stop()}>
            <Icon name="stop" size={14} /> Stop
          </button>
        </div>
      </div>
    {/if}
  </div>
{/if}

{#if stillOn}
  <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
  <div class="scrim" onclick={keepRecording}></div>
  <div class="prompt" role="alertdialog" aria-label="Still on the call?">
    <p><strong>Still on the call?</strong></p>
    <div class="actions">
      <button class="btn" onclick={() => void stop()}>Stop</button>
      <button class="btn btn-primary" onclick={keepRecording}>Keep recording</button>
    </div>
  </div>
{/if}

{#if confirmingDiscard}
  <ConfirmDialog
    title="Discard this recording?"
    detail="The audio and anything transcribed so far are deleted. No note is written."
    confirmLabel="Discard"
    onconfirm={() => void discard()}
    oncancel={() => (confirmingDiscard = false)}
  />
{/if}

<style>
  .wrap {
    position: relative;
  }

  .pill {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 26px;
    padding: 0 var(--sp-2);
    margin: var(--sp-2) 0;
    border-radius: 999px;
    background: color-mix(in oklab, var(--danger) 14%, transparent);
    color: var(--danger);
    font-size: var(--text-xs);
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }
  .pill.open,
  .pill:hover {
    background: color-mix(in oklab, var(--danger) 24%, transparent);
  }

  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--danger);
    animation: pulse 1.6s ease-in-out infinite;
  }
  @media (prefers-reduced-motion: reduce) {
    .dot {
      animation: none;
    }
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }

  .scrim {
    position: fixed;
    inset: 0;
    z-index: 60;
  }

  .panel {
    position: absolute;
    left: calc(100% + var(--sp-2));
    top: 0;
    z-index: 61;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    width: 260px;
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-raised);
    box-shadow: var(--shadow-lg);
  }

  header {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }
  .timer {
    flex: none;
    font-variant-numeric: tabular-nums;
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }

  .badge {
    align-self: flex-start;
    padding: 2px var(--sp-2);
    border-radius: 999px;
    background: var(--bg-hover);
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-weight: 550;
  }

  .meters {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }

  .warn {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    margin: 0;
    color: var(--danger);
    font-size: var(--text-xs);
    line-height: var(--leading-normal);
  }

  .hint {
    margin: 0;
    color: var(--fg-subtle);
    font-size: var(--text-xs);
  }

  .actions {
    display: flex;
    gap: var(--sp-2);
    justify-content: flex-end;
  }

  .prompt {
    position: fixed;
    left: 50%;
    bottom: var(--sp-8);
    translate: -50% 0;
    z-index: 71;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-raised);
    box-shadow: var(--shadow-lg);
  }
  .prompt p {
    margin: 0;
  }
  .prompt .actions {
    justify-content: flex-end;
  }
</style>
