<script lang="ts">
  // The Assistant app.
  //
  // Three panes and nothing clever. What it did, what it is standing by to do,
  // and what it remembers. Everything a routine *made* — a note, a task, a
  // block of time — is in the app that owns it, because that is where you were
  // going to look for it anyway. What is here is only what has no other home:
  // the runs, the routines, and the memory.
  //
  // The rail is not part of this app. It works here as it works everywhere,
  // and opening a run puts that run's transcript in it.

  import { api } from '../lib/api'
  import { assistant, PANE_LABELS } from '../lib/assistant.svelte'
  import { agent } from '../lib/agent.svelte'
  import { app } from '../lib/state.svelte'
  import { menu } from '../lib/menu.svelte'
  import { panels } from '../lib/panels.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import { relativeTime } from '../lib/format'
  import { renderMarkdown } from '../lib/markdown'
  import { focusOnMount } from '../lib/focus'
  import type { Outcome, RoutineRun, Trigger, Weekday } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import EmptyState from './EmptyState.svelte'
  import Icon from './Icon.svelte'

  // Loaded when the view appears rather than when the store is imported, so a
  // vault whose owner never opens this app never pays for the queries.
  void assistant.start()

  let adding = $state(false)
  let draftMemory = $state('')
  let editingMemory = $state<string | null>(null)
  let memoryDraft = $state('')
  let pendingForget = $state<{ id: string; text: string } | null>(null)

  const DAYS: { value: Weekday; label: string }[] = [
    { value: 'mon', label: 'M' },
    { value: 'tue', label: 'T' },
    { value: 'wed', label: 'W' },
    { value: 'thu', label: 'T' },
    { value: 'fri', label: 'F' },
    { value: 'sat', label: 'S' },
    { value: 'sun', label: 'S' },
  ]

  const OUTCOME_WORDS: Record<Outcome, string> = {
    running: 'Running',
    done: 'Done',
    failed: 'Could not run',
    skipped: 'Skipped',
  }

  const editing = $derived(assistant.editing)
  const schedule = $derived(editing?.trigger.type === 'schedule' ? editing.trigger : null)

  function setTime(at: string) {
    if (editing && editing.trigger.type === 'schedule') editing.trigger = { ...editing.trigger, at }
  }

  function setLead(minutes: number) {
    if (!editing || editing.trigger.type !== 'beforeEvent') return
    editing.trigger = { ...editing.trigger, leadMinutes: Math.max(5, Math.round(minutes)) }
  }

  function toggleDay(day: Weekday) {
    if (!editing || editing.trigger.type !== 'schedule') return
    const days = editing.trigger.days.includes(day)
      ? editing.trigger.days.filter((d) => d !== day)
      : [...editing.trigger.days, day]
    editing.trigger = { ...editing.trigger, days }
  }

  /** What a trigger the editor cannot draw says for itself. */
  function triggerWords(t: Trigger): string {
    if (t.type === 'beforeEvent') return `${t.leadMinutes} minutes before a meeting`
    if (t.type === 'taskDue') return 'Before a task falls due'
    return 'Only when you ask'
  }

  /**
   * Put a run's transcript in the rail.
   *
   * Every tool it called and every word it said, one click from the summary.
   * That transcript is what stands in for a review queue: what the assistant
   * did is auditable rather than pre-approved.
   */
  async function openRun(run: RoutineRun) {
    const thread = run.conversationId
    if (!thread) return
    assistant.openRun = run.id
    // Opened first, because opening it loads the thread list; the thread this
    // run belongs to is then swapped in over the top.
    if (!agent.open) await agent.toggle()
    await agent.openThread(thread)
  }

  function runMenu(run: RoutineRun): MenuItem[] {
    return tidyMenu([
      {
        label: 'Open the transcript',
        icon: 'quote',
        disabled: !run.conversationId,
        run: () => void openRun(run),
      },
      SEP,
      {
        label: 'Remove from the log',
        icon: 'trash',
        danger: true,
        run: () => void removeRun(run),
      },
    ])
  }

  async function removeRun(run: RoutineRun) {
    await api.deleteRun(run.id)
    await assistant.refresh()
  }

  function commitMemory() {
    const text = draftMemory
    draftMemory = ''
    adding = false
    void assistant.remember(text)
  }

  function commitRewrite(id: string) {
    const memory = assistant.memories.find((m) => m.id === id)
    const text = memoryDraft
    editingMemory = null
    memoryDraft = ''
    if (memory) void assistant.rewrite(memory, text)
  }

  async function forget() {
    const target = pendingForget
    pendingForget = null
    if (target) await assistant.forget(target.id)
  }
</script>

<main class="main">
  <header class="head">
    <h1>{PANE_LABELS[assistant.pane]}</h1>
    {#if assistant.loading}<span class="dim">· loading…</span>{/if}
    <span class="spacer"></span>
    {#if assistant.pane === 'routines' && app.supportsRoutines}
      <button class="btn btn-primary" onclick={() => void assistant.draft()}>New routine</button>
    {:else if assistant.pane === 'memory'}
      <button class="btn" onclick={() => (adding = true)}>Add a fact</button>
    {/if}
  </header>

  <div class="scroll body">
    {#if assistant.pane === 'runs'}
      {#if assistant.runs.length === 0}
        <EmptyState lead="It has not done anything yet">
          {#snippet icon()}<Icon name="sparkle" size={34} weight={1.4} />{/snippet}
          {#snippet note()}
            Set up a routine and it will start turning up here on its own. Whatever it makes — a
            note, a task, an hour set aside — goes to the app that owns it.
          {/snippet}
          {#snippet action()}
            {#if app.supportsRoutines}
              <button class="btn btn-primary" onclick={() => assistant.setPane('routines')}>
                Set one up
              </button>
            {/if}
          {/snippet}
        </EmptyState>
      {:else}
        {#each assistant.runs as run (run.id)}
          <article
            class="card"
            class:unseen={!run.seen}
            oncontextmenu={(e) => menu.show(e, runMenu(run))}
          >
            <header class="cardhead">
              <span class="name">{run.routineName}</span>
              <span class="pill {run.outcome}">{OUTCOME_WORDS[run.outcome]}</span>
              <span class="spacer"></span>
              <span class="dim">{relativeTime(run.startedAt)}</span>
            </header>

            {#if run.reason}
              <p class="reason">{run.reason}</p>
            {/if}

            {#if run.summary}
              <!-- Safe by construction, and checked: `renderMarkdown` escapes
                   the text before it converts any of it, so every angle
                   bracket that comes back was written by that module. The
                   same argument the rail makes, with the same tests behind it
                   in `scripts/markdown.test.mjs`. -->
              <!-- eslint-disable-next-line svelte/no-at-html-tags -->
              <div class="said">{@html renderMarkdown(run.summary)}</div>
            {/if}

            {#if run.conversationId}
              <footer class="cardfoot">
                <button class="link" onclick={() => void openRun(run)}>
                  What it did, step by step
                </button>
                {#if run.steps > 0}
                  <span class="dim">· {run.steps} {run.steps === 1 ? 'step' : 'steps'}</span>
                {/if}
              </footer>
            {/if}
          </article>
        {/each}
      {/if}
    {:else if assistant.pane === 'routines'}
      {#if editing}
        <section class="editor">
          <label class="field-row">
            <span class="eyebrow">Name</span>
            <input class="field" placeholder="Morning brief" bind:value={editing.name} />
          </label>

          <label class="field-row">
            <span class="eyebrow">What it should do</span>
            <textarea
              class="field area"
              rows="6"
              placeholder="Look at what is due today and leave me a note with the three things that matter."
              bind:value={editing.instructions}
            ></textarea>
            <span class="hint">
              This is the whole prompt. Nothing about the moment you wrote it survives, so say what
              to look at and what to leave behind.
            </span>
          </label>

          {#if schedule}
            <div class="field-row">
              <span class="eyebrow">When</span>
              <div class="when">
                <input
                  class="field time"
                  type="time"
                  value={schedule.at}
                  oninput={(e) => setTime(e.currentTarget.value)}
                />
                <div class="days">
                  {#each DAYS as d, i (i)}
                    <button
                      class="day"
                      class:on={schedule.days.includes(d.value)}
                      title={d.value}
                      onclick={() => toggleDay(d.value)}
                    >
                      {d.label}
                    </button>
                  {/each}
                </div>
              </div>
              <span class="hint">
                No day chosen means every day. If it misses its moment by more than
                {editing.graceMinutes} minutes it is recorded as skipped rather than run late.
              </span>
            </div>
          {:else if editing.trigger.type === 'beforeEvent'}
            <div class="field-row">
              <span class="eyebrow">When</span>
              <div class="when">
                <input
                  class="field lead"
                  type="number"
                  min="5"
                  max="1440"
                  step="5"
                  value={editing.trigger.leadMinutes}
                  oninput={(e) => setLead(Number(e.currentTarget.value))}
                />
                <span class="hint">minutes before a meeting starts</span>
              </div>
              <span class="hint">
                Once per meeting, checked every minute. A meeting the machine was asleep for is
                missed rather than prepared for afterwards.
              </span>
            </div>
          {:else}
            <div class="field-row">
              <span class="eyebrow">When</span>
              <p class="hint">{triggerWords(editing.trigger)}</p>
            </div>
          {/if}

          <label class="toggle">
            <input type="checkbox" bind:checked={editing.enabled} />
            <span>
              <b>Switched on</b>
              <small>Turn this off to stop it without losing what it says.</small>
            </span>
          </label>

          <footer class="editorfoot">
            <button class="btn" onclick={() => assistant.cancelEdit()}>Cancel</button>
            <span class="spacer"></span>
            <button
              class="btn btn-primary"
              disabled={!editing.name.trim() || !editing.instructions.trim()}
              onclick={() => void assistant.save()}
            >
              Save
            </button>
          </footer>
        </section>
      {:else if assistant.routines.length === 0}
        <section class="templates">
          <p class="lead">
            A routine is a time and an instruction in your own words. Start from one of these, or
            write your own — every word of them can be changed.
          </p>
          {#each assistant.templates as t (t.name)}
            <button
              class="template"
              disabled={!t.available}
              onclick={() => void assistant.draft(t)}
            >
              <span class="tname">{t.name}</span>
              <span class="tnote">{t.note}</span>
              {#if !t.available}
                <span class="tnote dim">Needs a calendar in this vault.</span>
              {/if}
            </button>
          {/each}
        </section>
      {:else}
        {#each assistant.routines as routine (routine.id)}
          <article
            class="card"
            class:off={!routine.enabled}
            oncontextmenu={(e) => menu.show(e, [])}
          >
            <header class="cardhead">
              <span class="name">{routine.name}</span>
              <span class="spacer"></span>
              <span class="dim">{routine.when}</span>
            </header>
            <p class="said">{routine.instructions}</p>
            <footer class="cardfoot">
              <button class="link" onclick={() => assistant.edit(routine)}>Edit</button>
              <button class="link" onclick={() => void assistant.runNow(routine.id)}>
                Run now
              </button>
              <button class="link" onclick={() => void assistant.toggle(routine)}>
                {routine.enabled ? 'Switch off' : 'Switch on'}
              </button>
              <span class="spacer"></span>
              {#if routine.lastRunAt}
                <span class="dim">last ran {relativeTime(routine.lastRunAt)}</span>
              {:else}
                <span class="dim">never run</span>
              {/if}
            </footer>
          </article>
        {/each}
      {/if}
    {:else}
      <p class="lead">
        Short facts it keeps between conversations, and reads at the start of every one. Anything
        that will not change — your name, where you live — belongs in
        <button class="link" onclick={() => panels.openSettings('profile')}>About you</button>
        instead.
      </p>

      {#if adding}
        <input
          class="field"
          placeholder="Plans the week on Sunday evening"
          bind:value={draftMemory}
          use:focusOnMount
          onblur={commitMemory}
          onkeydown={(e) => {
            if (e.key === 'Enter') commitMemory()
            if (e.key === 'Escape') {
              draftMemory = ''
              adding = false
            }
          }}
        />
      {/if}

      {#if assistant.memories.length === 0 && !adding}
        <p class="hint">Nothing yet. It will remember things as you talk to it.</p>
      {/if}

      {#each assistant.memories as memory (memory.id)}
        <div class="memory">
          {#if editingMemory === memory.id}
            <input
              class="field"
              bind:value={memoryDraft}
              use:focusOnMount
              onblur={() => commitRewrite(memory.id)}
              onkeydown={(e) => {
                if (e.key === 'Enter') commitRewrite(memory.id)
                if (e.key === 'Escape') editingMemory = null
              }}
            />
          {:else}
            <button
              class="memtext"
              onclick={() => {
                editingMemory = memory.id
                memoryDraft = memory.text
              }}
            >
              {memory.text}
            </button>
            <button
              class="pinbtn"
              class:on={memory.pinned}
              title={memory.pinned ? 'It may forget this to make room' : 'Keep this'}
              onclick={() => void assistant.togglePinned(memory)}
            >
              <Icon name="pin" size={13} />
            </button>
            <button
              class="link"
              onclick={() => (pendingForget = { id: memory.id, text: memory.text })}
            >
              Forget
            </button>
          {/if}
        </div>
      {/each}
    {/if}
  </div>
</main>

{#if pendingForget}
  <ConfirmDialog
    title="Forget this?"
    detail={'“' + pendingForget.text + '” will be removed. This cannot be undone.'}
    confirmLabel="Forget"
    onconfirm={forget}
    oncancel={() => (pendingForget = null)}
  />
{/if}

<style>
  .main {
    display: flex;
    flex: 1;
    flex-direction: column;
    min-width: 0;
    background: var(--bg-raised);
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: var(--header-h);
    flex: none;
    padding: 0 var(--sp-5);
    border-bottom: 1px solid var(--line);
  }

  h1 {
    font-size: var(--text-lg);
    font-weight: 600;
  }

  .spacer {
    flex: 1;
  }

  .dim {
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .body {
    flex: 1;
    display: grid;
    align-content: start;
    gap: var(--sp-3);
    max-width: 46rem;
    width: 100%;
    padding: var(--sp-5);
  }

  .lead {
    color: var(--fg-muted);
    font-size: var(--text-sm);
    line-height: 1.55;
  }

  .card {
    display: grid;
    gap: var(--sp-2);
    padding: var(--sp-4);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--bg);
  }

  .card.unseen {
    border-color: color-mix(in oklab, var(--accent) 45%, var(--line));
  }

  .card.off {
    opacity: 0.6;
  }

  .cardhead,
  .cardfoot {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }

  .name {
    font-weight: 600;
  }

  .pill {
    padding: 1px var(--sp-2);
    border-radius: 999px;
    background: var(--bg-hover);
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }

  .pill.failed {
    background: color-mix(in oklab, var(--danger) 18%, var(--bg-hover));
    color: var(--fg);
  }

  .pill.done {
    background: color-mix(in oklab, var(--accent) 16%, var(--bg-hover));
    color: var(--fg);
  }

  .reason {
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }

  .said {
    color: var(--fg);
    font-size: var(--text-base);
    line-height: 1.55;
  }

  .said :global(p + p) {
    margin-top: var(--sp-2);
  }

  .said :global(ul),
  .said :global(ol) {
    margin: var(--sp-2) 0 var(--sp-2) var(--sp-4);
  }

  .link {
    color: var(--accent);
    font-size: var(--text-sm);
  }

  .link:hover {
    text-decoration: underline;
  }

  .editor,
  .templates {
    display: grid;
    gap: var(--sp-4);
    padding: var(--sp-4);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--bg);
  }

  .field-row {
    display: grid;
    gap: var(--sp-2);
  }

  .eyebrow {
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--fg-faint);
  }

  .area {
    resize: vertical;
    font-family: inherit;
    line-height: 1.5;
  }

  .hint {
    color: var(--fg-faint);
    font-size: var(--text-xs);
    line-height: 1.5;
  }

  .when {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-3);
  }

  .time {
    width: 8rem;
  }

  .lead {
    width: 5rem;
  }

  .days {
    display: flex;
    gap: 4px;
  }

  .day {
    width: 28px;
    height: 28px;
    border-radius: var(--radius-sm);
    background: var(--bg-hover);
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-weight: 600;
  }

  .day.on {
    background: var(--accent);
    color: #fff;
  }

  .toggle {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
  }

  .toggle small {
    display: block;
    color: var(--fg-faint);
    font-size: var(--text-xs);
  }

  .editorfoot {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }

  .template {
    display: grid;
    gap: 2px;
    padding: var(--sp-3);
    border: 1px solid var(--line);
    border-radius: var(--radius-sm);
    text-align: left;
  }

  .template:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  .template:disabled {
    opacity: 0.5;
  }

  .tname {
    font-weight: 600;
  }

  .tnote {
    color: var(--fg-faint);
    font-size: var(--text-sm);
  }

  .memory {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-2) 0;
    border-bottom: 1px solid var(--line);
  }

  .memtext {
    flex: 1;
    color: var(--fg);
    font-size: var(--text-base);
    text-align: left;
  }

  .memtext:hover {
    text-decoration: underline dotted;
  }

  .pinbtn {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }

  .pinbtn.on {
    background: var(--bg-active);
    color: var(--fg);
  }
</style>
