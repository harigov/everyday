<script lang="ts">
  // Settings → Keeping in touch.
  //
  // How often each circle wants a catch-up, how many people a week to be
  // reminded of, how far ahead of a birthday or a festival, and whether the
  // journal is read for who you talked to. People themselves are cards on the
  // Contacts shelf; this is only the rules. See docs/plans/keep-in-touch.md.

  import { onDestroy } from 'svelte'
  import { api } from '../lib/api'
  import { friendlyDate, plural } from '../lib/format'
  import { app, handle } from '../lib/state.svelte'
  import type { KeepInTouchSettings, KeepInTouchStatus, PassReport } from '../lib/types'

  let status = $state<KeepInTouchStatus | null>(null)
  let draft = $state<KeepInTouchSettings | null>(null)
  let saving = $state(false)
  let running = $state(false)
  let notice = $state<string | null>(null)
  let failed = $state(false)

  async function load() {
    try {
      // The answer itself, not `status`, which is a proxy by now and cannot be
      // cloned.
      const fresh = await api.keepInTouch()
      status = fresh
      draft = structuredClone(fresh.settings)
    } catch (e) {
      await handle(e)
    }
  }
  void load()

  let noticeTimer: ReturnType<typeof setTimeout> | null = null
  function say(message: string, error = false) {
    notice = message
    failed = error
    if (noticeTimer) clearTimeout(noticeTimer)
    noticeTimer = setTimeout(() => {
      noticeTimer = null
      notice = null
    }, 6000)
  }
  onDestroy(() => {
    if (noticeTimer) clearTimeout(noticeTimer)
  })

  async function save() {
    if (!draft) return
    saving = true
    try {
      await api.saveKeepInTouch($state.snapshot(draft))
      await load()
      say('Saved.')
    } catch (e) {
      say(e instanceof Error ? e.message : String(e), true)
    } finally {
      saving = false
    }
  }

  async function runNow() {
    running = true
    try {
      const report = await api.runKeepInTouch()
      await load()
      say(describe(report), !!report.journalProblem)
    } catch (e) {
      say(e instanceof Error ? e.message : String(e), true)
    } finally {
      running = false
    }
  }

  /** What a pass did, in a sentence. */
  function describe(r: PassReport): string {
    const parts = [
      r.entriesRead > 0 && `read ${plural(r.entriesRead, 'entry', 'entries')}`,
      r.logged > 0 && `logged ${plural(r.logged, 'catch-up')}`,
      r.asked > 0 && `asked about ${plural(r.asked, 'name')}`,
      r.suggested > 0 && `suggested ${plural(r.suggested, 'person', 'people')}`,
      r.occasions > 0 && `proposed ${plural(r.occasions, 'occasion')}`,
    ].filter(Boolean)
    const did = parts.length > 0 ? parts.join(', ') : 'nothing new to do'
    const said = did.charAt(0).toUpperCase() + did.slice(1) + '.'
    return r.journalProblem ? `${said} The journal was not read: ${r.journalProblem}.` : said
  }

  const dirty = $derived(
    !!draft && !!status && JSON.stringify(draft) !== JSON.stringify(status.settings),
  )
</script>

{#if !draft || !status}
  <p class="hint">Loading…</p>
{:else}
  <div class="panes">
    <label class="toggle">
      <input type="checkbox" bind:checked={draft.enabled} />
      <span>
        <b>Keep in touch</b>
        <small>
          Logs who you were in touch with, proposes a task when somebody is overdue a catch-up, and
          reminds you of their birthdays and the festivals they celebrate. People are the cards on
          the Library's Contacts shelf.
        </small>
      </span>
    </label>

    <section>
      <span class="eyebrow">How often</span>
      <p class="hint">
        Put somebody in a circle on their card, or give them a number of days of their own. Anyone
        with neither is never suggested.
      </p>
      <div class="circles">
        {#each draft.circles as circle, i (i)}
          <label class="circle">
            <span>{circle.name}</span>
            <span class="every">
              every
              <input
                type="number"
                min="1"
                max="3650"
                bind:value={circle.everyDays}
                aria-label="{circle.name}: every how many days"
              />
              days
            </span>
          </label>
        {/each}
      </div>
    </section>

    <section>
      <span class="eyebrow">Reminders</span>
      <label class="circle">
        <span>People suggested a week, at most</span>
        <input type="number" min="0" max="50" bind:value={draft.perWeek} />
      </label>
      <label class="circle">
        <span>Days ahead of a birthday or festival</span>
        <input type="number" min="0" max="6" bind:value={draft.leadDays} />
      </label>
      <p class="hint">
        Each suggestion is a task to accept or decline, gone after a week if you do neither.
        Festivals come from all-day events on your calendars, so subscribe to a holiday calendar in
        the Calendar app and say on each card what they celebrate.
      </p>
    </section>

    <section>
      <span class="eyebrow">Your journal</span>
      <label class="toggle">
        <input type="checkbox" bind:checked={draft.readJournal} />
        <span>
          <b>Read the journal for who you talked to</b>
          <small>
            Once a night, each new entry is sent to the small model, or the medium one if there is
            no small one, to ask who you were in touch with. Needs the assistant switched on. A name
            that could be two people, or somebody not on the shelf yet, is asked about rather than
            guessed.
          </small>
        </span>
      </label>
      <p class="hint">
        Mail you send to somebody's address, and small meetings they are invited to, are logged too,
        without asking any model.
      </p>
    </section>

    {#if status.lastRun || status.lastReport}
      <section>
        <span class="eyebrow">Last check</span>
        <p class="hint">
          {#if status.lastRun}{friendlyDate(status.lastRun)}:{/if}
          {status.lastReport ? describe(status.lastReport) : ''}
        </p>
      </section>
    {/if}
  </div>

  <div class="save">
    {#if notice}<span class="notice" class:err={failed}>{notice}</span>{/if}
    <button
      class="btn"
      disabled={running || !status.settings.enabled || dirty || !app.supportsLibrary}
      title={dirty ? 'Save first' : 'Read today and the last fortnight now, rather than tonight'}
      onclick={() => void runNow()}
    >
      {running ? 'Checking…' : 'Check now'}
    </button>
    <button class="btn btn-primary" disabled={saving || !dirty} onclick={() => void save()}>
      {saving ? 'Saving…' : 'Save'}
    </button>
  </div>
{/if}

<style>
  .panes {
    display: flex;
    flex-direction: column;
    gap: var(--sp-6);
    min-width: 0;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  .toggle {
    display: flex;
    gap: var(--sp-3);
    align-items: flex-start;
    min-width: 0;
    cursor: pointer;
  }
  .toggle input {
    flex: none;
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .toggle span {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .toggle b {
    font-size: var(--text-base);
    font-weight: 600;
  }
  .toggle small,
  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }
  .circles {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .circle {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    font-size: var(--text-base);
  }
  .every {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    color: var(--fg-muted);
    font-size: var(--text-sm);
  }
  input[type='number'] {
    width: 5.5rem;
    padding: var(--sp-1) var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    text-align: right;
  }
  input[type='number']:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }
  .save {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--sp-3);
    margin-top: var(--sp-6);
    padding-top: var(--sp-4);
    border-top: 1px solid var(--border);
  }
  .notice {
    flex: 1;
    min-width: 0;
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .notice.err {
    color: var(--danger);
  }
</style>
