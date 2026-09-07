<script lang="ts">
  // "What am I doing right now."
  //
  // The strip at the top of the calendar's right-hand rail, and the answer
  // to the question the whole planned/actual split exists for. Pressing
  // start writes an *actual* time block that begins now; pressing stop puts
  // an end on it. Two writes for a two-hour session, because between them
  // the length is drawn from the wall clock rather than from storage.
  //
  // What it offers to track, in order of how likely it is to be right:
  //
  //   1. whatever is running
  //   2. the thing you planned to be doing at this minute
  //   3. the meeting that is on right now
  //   4. anything at all, as a note to yourself

  import { calendar } from '../lib/calendar.svelte'
  import { formatMinutes } from '../lib/format'
  import { minutesOfDay, todayIso } from '../lib/time'
  import Icon from './Icon.svelte'
  import type { BlockSubject } from '../lib/types'

  let note = $state('')

  /** The planned block or event covering this minute, if there is one. */
  const current = $derived.by(() => {
    const now = minutesOfDay(new Date(calendar.now))
    const iso = todayIso()
    return (
      calendar
        .slotsOn(iso)
        .filter((s) => !s.live && s.start <= now && s.end > now)
        // A plan you made beats a meeting somebody else booked: if both are
        // on, the one you chose is the better guess at what you are doing.
        .sort((a, b) => (a.kind === 'planned' ? -1 : 0) - (b.kind === 'planned' ? -1 : 0))[0] ??
      null
    )
  })

  const running = $derived(calendar.timer)

  async function startCurrent() {
    if (!current) return
    const subject: BlockSubject = current.block?.subject ?? { type: 'adhoc' }
    await calendar.startTimer(
      $state.snapshot(subject),
      current.block?.subject.type === 'adhoc' || !current.block ? current.title : '',
    )
  }

  async function startNote() {
    const title = note.trim()
    if (!title) return
    note = ''
    await calendar.startTimer({ type: 'adhoc' }, title)
  }

  /** Elapsed time, as a clock rather than a phrase: it is changing. */
  const elapsed = $derived.by(() => {
    const timer = running
    if (!timer) return '0:00'
    const secs = Math.max(0, Math.floor((calendar.now - Date.parse(timer.since)) / 1000))
    const h = Math.floor(secs / 3600)
    const m = Math.floor((secs % 3600) / 60)
    const s = secs % 60
    return h > 0
      ? `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`
      : `${m}:${String(s).padStart(2, '0')}`
  })

  /** When it started, so "since 14:05" can be shown. */
  const startedAt = $derived.by(() => {
    const timer = running
    if (!timer) return ''
    return new Intl.DateTimeFormat(navigator.language || 'en', {
      hour: 'numeric',
      minute: '2-digit',
    }).format(new Date(timer.since))
  })

  const loggedToday = $derived(calendar.totalsOn(todayIso()).logged)
</script>

<section class="track" class:on={!!running}>
  {#if running}
    {@const colour = calendar.colorOfSubject(running.subject)}
    <div class="live" style="--c: {colour}">
      <span class="pulse" aria-hidden="true"></span>
      <div class="what">
        <span class="title">{running.title || calendar.titleOfSubject(running.subject)}</span>
        <span class="since">since {startedAt}</span>
      </div>
      <span class="elapsed">{elapsed}</span>
      <button class="stop" title="Stop tracking" onclick={() => calendar.stopTimer()}>
        <Icon name="stop" size={14} />
      </button>
    </div>
  {:else}
    <div class="idle">
      {#if current}
        <button class="start" style="--c: {current.color}" onclick={startCurrent}>
          <Icon name="play" size={14} />
          <span class="startlabel">
            <span class="title">{current.title}</span>
            <span class="hint">on now — start tracking it</span>
          </span>
        </button>
      {/if}
      <div class="noterow">
        <input
          class="notefield"
          placeholder="…or what are you doing?"
          bind:value={note}
          onkeydown={(e) => {
            if (e.key === 'Enter') void startNote()
          }}
        />
        <button class="go" disabled={!note.trim()} title="Start tracking" onclick={startNote}>
          <Icon name="play" size={14} />
        </button>
      </div>
    </div>
  {/if}

  {#if loggedToday > 0}
    <p class="tally">{formatMinutes(loggedToday)} logged today</p>
  {/if}
</section>

<style>
  .track {
    flex: none;
    padding: var(--sp-3);
    border-bottom: 1px solid var(--border);
    background: var(--bg-panel);
  }
  .track.on {
    background: var(--bg-raised);
  }

  /* ── Running ────────────────────────────────────────────────────────── */

  .live {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
  }
  .pulse {
    width: 8px;
    height: 8px;
    flex: none;
    border-radius: 50%;
    background: var(--c);
    animation: beat 2s var(--ease) infinite;
  }
  @keyframes beat {
    0%,
    100% {
      box-shadow: 0 0 0 0 color-mix(in oklab, var(--c) 55%, transparent);
    }
    60% {
      box-shadow: 0 0 0 6px color-mix(in oklab, var(--c) 0%, transparent);
    }
  }

  .what {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .title {
    font-size: var(--text-base);
    font-weight: 570;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .since {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }

  /* Tabular, and wide enough for `1:02:03`, so the strip does not jitter as
     the seconds tick over. */
  .elapsed {
    flex: none;
    font-variant-numeric: tabular-nums;
    font-size: var(--text-md);
    font-weight: 600;
    letter-spacing: -0.01em;
    color: var(--c);
  }

  .stop {
    width: 28px;
    height: 28px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-subtle);
  }
  .stop:hover {
    background: var(--bg-hover);
    color: var(--danger);
  }

  /* ── Idle ───────────────────────────────────────────────────────────── */

  .idle {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }

  .start {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    padding: 6px var(--sp-2);
    border-radius: var(--radius-sm);
    border-left: 3px solid var(--c);
    background: color-mix(in oklab, var(--c) 10%, transparent);
    color: color-mix(in oklab, var(--c) 66%, var(--fg));
    text-align: left;
    min-width: 0;
  }
  .start:hover {
    background: color-mix(in oklab, var(--c) 18%, transparent);
  }
  .startlabel {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .hint {
    font-size: var(--text-xs);
    opacity: 0.75;
  }

  .noterow {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
  }
  .notefield {
    flex: 1;
    min-width: 0;
    height: 28px;
    padding: 0 var(--sp-2);
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
    font-size: var(--text-sm);
    user-select: text;
  }
  .notefield::placeholder {
    color: var(--fg-faint);
  }
  .notefield:focus {
    outline: none;
    border-color: var(--accent);
    background: var(--bg-raised);
  }

  .go {
    width: 28px;
    height: 28px;
    flex: none;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    color: var(--fg-subtle);
  }
  .go:hover {
    background: var(--bg-hover);
    color: var(--accent);
  }
  .go[disabled] {
    opacity: 0.35;
    pointer-events: none;
  }

  .tally {
    margin-top: var(--sp-2);
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
</style>
