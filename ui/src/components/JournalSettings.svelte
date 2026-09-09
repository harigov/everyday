<script lang="ts">
  // A journal's settings, and the place tracking is specified.
  //
  // Journals had no settings screen at all before this: a name at creation,
  // and a right-click to delete. That was tolerable while a journal was a
  // name and a colour, and stopped being tolerable the moment it also
  // carried the answer to "what am I recording here".
  //
  // The tracker list is the heart of it, and the design constraint was that
  // adding one must not feel like filling in a form. So there are two ways
  // in: a shelf of ready-made trackers that are one click each, and a name
  // field for everything else, with the fiddly fields (unit, step, goal)
  // appearing only once the kind that needs them has been chosen.

  import { untrack } from 'svelte'
  import { api } from '../lib/api'
  import { app } from '../lib/state.svelte'
  import { tracking } from '../lib/tracking.svelte'
  import { KIND_COPY, TRACKER_PRESETS, fromPreset } from '../lib/tracker-presets'
  import { TRACKER_ICON_GROUPS } from '../lib/tracker-icons'
  import { focusOnMount, trapFocus } from '../lib/focus'
  import { DEFAULT_COLORS } from '../lib/colors'
  import type { Journal, Tracker, TrackerKind } from '../lib/types'
  import { TRACKER_KINDS } from '../lib/types'
  import Icon from './Icon.svelte'
  import TrackerIcon from './TrackerIcon.svelte'
  import ConfirmDialog from './ConfirmDialog.svelte'

  let { journal, onclose }: { journal: Journal; onclose: () => void } = $props()

  /**
   * The journal as edited here, taken once and deliberately not re-taken.
   *
   * Every committed change is written straight through with `save`, which
   * refreshes the store and hands this component a new `journal` prop --
   * and following that back into the draft would overwrite whatever is
   * half-typed in the field beside it. `untrack` says that is on purpose.
   */
  let draft = $state<Journal>(untrack(() => structuredClone($state.snapshot(journal))))

  /** The tracker being edited, or a freshly minted one being added. */
  let editing = $state<Tracker | null>(null)
  let adding = $state(false)
  let pickingIcon = $state(false)
  let newName = $state('')
  let pendingDelete = $state<Tracker | null>(null)
  let pendingJournalDelete = $state(false)
  let showArchived = $state(false)

  const enabled = $derived(app.status?.capabilities?.trackers === true)
  /**
   * Every tracker in the vault, not just this journal's.
   *
   * A tracker is a vault record now, so this dialog is two things at once:
   * where a tracker is defined, and where a journal says which chips it
   * draws. The second is `shown`, the tick beside each row.
   */
  const live = $derived(tracking.live)
  const archived = $derived(tracking.trackers.filter((t) => t.archived))

  const PALETTE = [
    '#e11d48',
    '#f97316',
    '#f59e0b',
    '#16a34a',
    '#0d9488',
    '#0284c7',
    '#4f46e5',
    '#9333ea',
    '#db2777',
    '#78716c',
  ]

  async function save() {
    draft.updatedAt = new Date().toISOString()
    await app.saveJournal($state.snapshot(draft))
  }

  /** Does this journal draw a chip for it? */
  function shown(id: string): boolean {
    return draft.shownTrackers.includes(id)
  }

  /**
   * Start or stop drawing a chip, without touching the tracker itself.
   *
   * The whole of what is still a per-journal setting. Everything else about
   * a tracker belongs to the vault, which is why hiding one here keeps its
   * definition and every reading it ever made.
   */
  async function toggleShown(id: string) {
    draft.shownTrackers = shown(id)
      ? draft.shownTrackers.filter((held) => held !== id)
      : [...draft.shownTrackers, id]
    await save()
  }

  async function upsert(tracker: Tracker) {
    await tracking.saveTracker(tracker)
  }

  /** Mint a tracker in the backend, which is where ids and clocks live. */
  async function mint(name: string, kind: TrackerKind): Promise<Tracker | null> {
    try {
      const tracker = await api.newTracker(name, kind)
      tracker.sortOrder = tracking.trackers.length
      return tracker
    } catch {
      app.error = 'The tracker could not be created.'
      return null
    }
  }

  async function addPreset(preset: (typeof TRACKER_PRESETS)[number]) {
    const minted = await mint(preset.name, preset.kind)
    if (!minted) return
    await upsert(fromPreset(minted, preset))
    // Made here, so it is drawn here. Adding a tracker from a journal's own
    // settings and then having to tick it on would be a form with a step
    // in it that is never the answer.
    await toggleShown(minted.id)
  }

  async function addNamed() {
    const name = newName.trim()
    if (!name) return
    newName = ''
    const minted = await mint(name, 'check')
    if (!minted) return
    // A colour that is not already in the row, so a new chip is
    // distinguishable from the one beside it without being chosen.
    minted.color = PALETTE[tracking.trackers.length % PALETTE.length]!
    minted.icon = 'check'
    await upsert(minted)
    await toggleShown(minted.id)
    // A *copy* to edit. Handing over the object that is in the draft makes
    // Cancel a lie -- every keystroke would already have landed -- and
    // clearing the name and cancelling would leave a nameless tracker that
    // the next save silently drops, orphaning any readings it had made.
    editing = structuredClone(minted)
    adding = true
  }

  /**
   * A number, or the fallback.
   *
   * Emptying a `type="number"` field makes Svelte's binding write `null`,
   * and `Tracker.scale_max` and `default_value` are plain `f64`s in the
   * core -- `serde(default)` covers a *missing* key, not an explicit null,
   * so the whole `save_journal` would be refused. Every numeric field this
   * form owns comes back through here on its way out.
   */
  function sane(value: number | null | undefined, fallback: number): number {
    return typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : fallback
  }

  async function commitEdit() {
    if (!editing) return
    const tracker = $state.snapshot(editing)
    if (!tracker.name.trim()) return
    tracker.scaleMax = Math.min(sane(tracker.scaleMax, 10), 100)
    tracker.defaultValue = sane(tracker.defaultValue, 1)
    tracker.target = tracker.target && Number.isFinite(tracker.target) ? tracker.target : null
    // A check has nothing to measure, so it keeps neither unit nor goal.
    if (tracker.kind === 'check') {
      tracker.unit = ''
      tracker.target = null
      tracker.defaultValue = 1
    }
    tracker.updatedAt = new Date().toISOString()
    await upsert(tracker)
    editing = null
    adding = false
    pickingIcon = false
  }

  function setKind(kind: TrackerKind) {
    if (!editing) return
    editing.kind = kind
    // Sensible companions rather than a blank field: a dose without a unit
    // records numbers nobody can interpret a year later.
    if (kind === 'dose' && !editing.unit) editing.unit = 'mg'
    if (kind === 'amount' && !editing.unit) editing.unit = 'min'
    if (kind === 'check') editing.unit = ''
  }

  async function toggleArchived(tracker: Tracker) {
    await upsert({ ...tracker, archived: !tracker.archived, updatedAt: new Date().toISOString() })
  }

  /**
   * Archive from inside the editor, and close it.
   *
   * The flag has to travel on the copy being edited rather than straight
   * into the draft: `Done` writes that copy back wholesale, so archiving
   * beneath it and then confirming would restore the tracker it had just
   * retired.
   */
  async function archiveFromEditor() {
    if (!editing) return
    editing.archived = !editing.archived
    await commitEdit()
  }

  async function toggleCalendar(tracker: Tracker) {
    await upsert({
      ...tracker,
      onCalendar: !tracker.onCalendar,
      updatedAt: new Date().toISOString(),
    })
  }

  async function move(tracker: Tracker, by: -1 | 1) {
    const order = live.slice()
    const i = order.findIndex((t) => t.id === tracker.id)
    const j = i + by
    if (i < 0 || j < 0 || j >= order.length) return
    ;[order[i], order[j]] = [order[j]!, order[i]!]
    for (const [n, t] of order.entries()) await upsert({ ...t, sortOrder: n })
  }

  async function reallyDelete() {
    const tracker = pendingDelete
    pendingDelete = null
    if (!tracker) return
    await tracking.deleteTracker(tracker.id)
    // The id stays in this journal's list and is skipped when the strip is
    // built. Rewriting six journals to tidy one array is a great deal of
    // writing to avoid a lookup that already has to handle a miss.
  }

  /**
   * The line under a tracker's name: what it records, in its own terms.
   *
   * Says the unit rather than the kind where the two differ, because "Doses
   * in mg" tells you what a reading will look like and "Took this much"
   * only tells you which of four buttons was pressed.
   */
  function describe(tracker: Tracker): string {
    const goal = tracker.target
      ? ` · goal ${tracker.target}${tracker.unit ? ` ${tracker.unit}` : ''}`
      : ''
    switch (tracker.kind) {
      case 'check':
        return 'Done, or not yet'
      case 'dose':
        return `Doses in ${tracker.unit || 'units'}${goal}`
      case 'scale':
        return `Severity out of ${tracker.scaleMax}`
      default:
        return `${tracker.unit ? `Counted in ${tracker.unit}` : 'A number'}${goal}`
    }
  }

  function deleteDetail(tracker: Tracker): string {
    return (
      `Every reading of “${tracker.name}” goes with it, and that cannot be undone. ` +
      'To stop recording it but keep the history, archive it instead.'
    )
  }
</script>

<svelte:window
  onkeydown={(e: KeyboardEvent) => {
    if (e.key !== 'Escape') return
    if (pickingIcon) pickingIcon = false
    else if (editing) editing = null
    else if (!pendingDelete && !pendingJournalDelete) onclose()
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
<div class="scrim" onclick={onclose}></div>
<div class="sheet wide" role="dialog" aria-modal="true" aria-label="Journal settings" use:trapFocus>
  <header class="head">
    <span class="glyph" style="--c: {draft.color}">{draft.icon}</span>
    <h2>{draft.name || 'Journal settings'}</h2>
    <button class="x" aria-label="Close" onclick={onclose}><Icon name="close" size={16} /></button>
  </header>

  <div class="body scroll">
    <!-- ── The journal itself ──────────────────────────────────────── -->
    <section>
      <div class="row two">
        <label class="lbl">
          Name
          <input class="text" bind:value={draft.name} use:focusOnMount onchange={save} />
        </label>
        <label class="lbl narrow">
          Symbol
          <input class="text mid" bind:value={draft.icon} onchange={save} maxlength="4" />
        </label>
      </div>

      <label class="lbl">
        Description
        <input
          class="text"
          bind:value={draft.description}
          onchange={save}
          placeholder="What goes in here"
        />
      </label>

      <div class="lbl">
        Colour
        <div class="swatches">
          {#each DEFAULT_COLORS as c (c)}
            <button
              class="swatch"
              class:on={draft.color.toLowerCase() === c.toLowerCase()}
              style="--c: {c}"
              aria-label={c}
              onclick={() => {
                draft.color = c
                void save()
              }}
            ></button>
          {/each}
        </div>
      </div>
    </section>

    <!-- ── Tracking ────────────────────────────────────────────────── -->
    {#if enabled}
      <section>
        <h3>Tracking</h3>
        <p class="hint">
          Anything you want to record beside an entry — a habit, a dose, a symptom, a number.
          Trackers belong to the vault rather than to one journal, so the tick beside each says
          whether <em>this</em> journal offers its chip. Unticking one keeps it and every reading it has
          ever made.
        </p>

        {#if live.length}
          <ul class="trackers">
            {#each live as tracker, i (tracker.id)}
              <li class="tracker" class:off={!shown(tracker.id)}>
                <button
                  class="ghost tick"
                  class:on={shown(tracker.id)}
                  role="switch"
                  aria-checked={shown(tracker.id)}
                  title={shown(tracker.id)
                    ? `Shown on ${draft.name}`
                    : `Not shown on ${draft.name}`}
                  onclick={() => toggleShown(tracker.id)}
                >
                  <Icon name={shown(tracker.id) ? 'check' : 'circle'} size={15} />
                </button>
                <TrackerIcon name={tracker.icon} color={tracker.color} size={30} />
                <div class="what">
                  <span class="tname">{tracker.name}</span>
                  <span class="sub">{describe(tracker)}</span>
                </div>
                <button
                  class="ghost"
                  class:on={tracker.onCalendar}
                  title={tracker.onCalendar ? 'Shown on the calendar' : 'Not shown on the calendar'}
                  aria-pressed={tracker.onCalendar}
                  onclick={() => toggleCalendar(tracker)}
                >
                  <Icon name="calendar" size={15} />
                </button>
                <div class="order">
                  <button
                    class="ghost tiny"
                    aria-label="Move up"
                    disabled={i === 0}
                    onclick={() => move(tracker, -1)}
                  >
                    <span class="up"><Icon name="chevron" size={13} /></span>
                  </button>
                  <button
                    class="ghost tiny"
                    aria-label="Move down"
                    disabled={i === live.length - 1}
                    onclick={() => move(tracker, 1)}
                  >
                    <span class="down"><Icon name="chevron" size={13} /></span>
                  </button>
                </div>
                <button
                  class="ghost"
                  aria-label="Edit"
                  onclick={() => {
                    editing = structuredClone($state.snapshot(tracker))
                    adding = false
                  }}
                >
                  <Icon name="settings" size={15} />
                </button>
              </li>
            {/each}
          </ul>
        {/if}

        <!-- The editor, inline rather than a second dialog: what is being
             changed stays visible in the list behind it. -->
        {#if editing}
          <div class="editor" style="--c: {editing.color}">
            <div class="row">
              <button
                class="iconbtn"
                aria-label="Choose an icon"
                onclick={() => (pickingIcon = !pickingIcon)}
              >
                <TrackerIcon name={editing.icon} color={editing.color} size={38} />
              </button>
              <input class="text" bind:value={editing.name} placeholder="What are you recording?" />
            </div>

            {#if pickingIcon}
              <div class="picker scroll">
                {#each TRACKER_ICON_GROUPS as group (group.label)}
                  <p class="glabel">{group.label}</p>
                  <div class="icons">
                    {#each group.icons as name (name)}
                      <button
                        class="pick"
                        class:on={editing.icon === name}
                        aria-label={name}
                        onclick={() => {
                          if (editing) editing.icon = name
                          pickingIcon = false
                        }}
                      >
                        <TrackerIcon {name} color={editing.color} size={30} />
                      </button>
                    {/each}
                  </div>
                {/each}
              </div>
            {/if}

            <div class="kinds">
              {#each TRACKER_KINDS as kind (kind)}
                <button
                  class="kind"
                  class:on={editing.kind === kind}
                  title={KIND_COPY[kind].hint}
                  onclick={() => setKind(kind)}
                >
                  <span class="klabel">{KIND_COPY[kind].label}</span>
                  <span class="khint">{KIND_COPY[kind].hint}</span>
                </button>
              {/each}
            </div>

            <div class="swatches">
              {#each PALETTE as c (c)}
                <button
                  class="swatch"
                  class:on={editing.color.toLowerCase() === c}
                  style="--c: {c}"
                  aria-label={c}
                  onclick={() => editing && (editing.color = c)}
                ></button>
              {/each}
            </div>

            {#if editing.kind !== 'check'}
              <div class="row three">
                {#if editing.kind !== 'scale'}
                  <label class="lbl narrow">
                    Unit
                    <input class="text mid" bind:value={editing.unit} placeholder="mg" />
                  </label>
                  <label class="lbl narrow">
                    Usual
                    <input
                      class="text mid"
                      type="number"
                      min="0"
                      step="any"
                      bind:value={editing.defaultValue}
                    />
                  </label>
                  <label class="lbl narrow">
                    Daily goal
                    <input
                      class="text mid"
                      type="number"
                      min="0"
                      step="any"
                      placeholder="none"
                      value={editing.target ?? ''}
                      onchange={(e) => {
                        const v = Number(e.currentTarget.value)
                        if (editing) editing.target = e.currentTarget.value && v > 0 ? v : null
                      }}
                    />
                  </label>
                {:else}
                  <label class="lbl narrow">
                    Worst is
                    <input
                      class="text mid"
                      type="number"
                      min="1"
                      max="100"
                      bind:value={editing.scaleMax}
                    />
                  </label>
                {/if}
              </div>
            {/if}

            <label class="check">
              <input type="checkbox" bind:checked={editing.onCalendar} />
              <span>
                Show on the calendar
                <em
                  >Only worth it when the time of day is real — a migraine at 14:20, not a box
                  ticked at bedtime for the whole day.</em
                >
              </span>
            </label>

            <div class="editor-foot">
              {#if !adding}
                <button class="link" onclick={archiveFromEditor}>
                  {editing.archived ? 'Unarchive' : 'Archive'}
                </button>
                <button
                  class="link danger"
                  onclick={() => {
                    pendingDelete = editing
                    editing = null
                  }}>Delete…</button
                >
              {/if}
              <span class="spacer"></span>
              <button
                class="btn"
                onclick={() => {
                  editing = null
                  adding = false
                  pickingIcon = false
                }}>Cancel</button
              >
              <button class="btn btn-primary" onclick={commitEdit}>Done</button>
            </div>
          </div>
        {/if}

        <div class="add">
          <input
            class="text"
            bind:value={newName}
            placeholder="Track something new…"
            onkeydown={(e) => {
              if (e.key === 'Enter') void addNamed()
            }}
          />
          <button class="btn" disabled={!newName.trim()} onclick={addNamed}>
            <Icon name="plus" size={14} /> Add
          </button>
        </div>

        <p class="glabel">Or start from one of these</p>
        <div class="presets">
          {#each TRACKER_PRESETS as preset (preset.name)}
            {@const already = tracking.trackers.some(
              (t) => t.name.toLowerCase() === preset.name.toLowerCase(),
            )}
            <button
              class="preset"
              disabled={already}
              style="--c: {preset.color}"
              title={KIND_COPY[preset.kind].hint}
              onclick={() => addPreset(preset)}
            >
              <TrackerIcon name={preset.icon} color={preset.color} size={22} />
              {preset.name}
            </button>
          {/each}
        </div>

        {#if archived.length}
          <button class="link" onclick={() => (showArchived = !showArchived)}>
            {showArchived ? 'Hide' : 'Show'}
            {archived.length} archived
          </button>
          {#if showArchived}
            <ul class="trackers muted">
              {#each archived as tracker (tracker.id)}
                <li class="tracker">
                  <TrackerIcon name={tracker.icon} color={tracker.color} size={30} />
                  <div class="what">
                    <span class="tname">{tracker.name}</span>
                    <span class="sub">Archived — its readings are kept</span>
                  </div>
                  <button class="link" onclick={() => toggleArchived(tracker)}>Restore</button>
                </li>
              {/each}
            </ul>
          {/if}
        {/if}
      </section>
    {/if}

    <section>
      <h3>Delete</h3>
      <p class="hint">
        Removes the journal, its entries and everything recorded in it. This cannot be undone.
      </p>
      <button class="btn btn-danger" onclick={() => (pendingJournalDelete = true)}>
        Delete this journal
      </button>
    </section>
  </div>
</div>

{#if pendingDelete}
  <ConfirmDialog
    title={'Delete “' + pendingDelete.name + '” and its readings?'}
    detail={deleteDetail(pendingDelete)}
    confirmLabel="Delete tracker"
    onconfirm={reallyDelete}
    oncancel={() => (pendingDelete = null)}
  />
{/if}

{#if pendingJournalDelete}
  <ConfirmDialog
    title={'Delete “' + draft.name + '”?'}
    detail="Its entries and everything tracked in it will be removed. This cannot be undone."
    confirmLabel="Delete journal"
    onconfirm={async () => {
      pendingJournalDelete = false
      onclose()
      await app.deleteJournal(draft.id)
    }}
    oncancel={() => (pendingJournalDelete = false)}
  />
{/if}

<style>
  .wide {
    width: min(560px, calc(100vw - var(--sp-8)));
    max-height: min(760px, calc(100vh - var(--sp-10)));
    display: flex;
    flex-direction: column;
    padding: 0;
  }

  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-4) var(--sp-5);
    border-bottom: 1px solid var(--border);
  }

  .glyph {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: var(--radius-sm);
    background: color-mix(in oklab, var(--c) 16%, transparent);
    font-size: 14px;
  }

  h2 {
    flex: 1;
    font-size: var(--text-md);
    font-weight: 620;
    letter-spacing: -0.008em;
  }

  .x {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: var(--radius-sm);
    color: var(--fg-subtle);
  }
  .x:hover {
    color: var(--fg);
    background: var(--bg-hover);
  }

  .body {
    padding: var(--sp-4) var(--sp-5) var(--sp-5);
    overflow-y: auto;
  }

  section + section {
    margin-top: var(--sp-6);
    padding-top: var(--sp-5);
    border-top: 1px solid var(--border);
  }

  h3 {
    font-size: var(--text-base);
    font-weight: 620;
    margin-bottom: var(--sp-1);
  }

  .hint {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
    line-height: var(--leading-snug);
    margin-bottom: var(--sp-3);
  }

  .lbl {
    display: block;
    font-size: var(--text-xs);
    font-weight: 560;
    color: var(--fg-subtle);
    margin-bottom: var(--sp-3);
  }

  .row {
    display: flex;
    align-items: flex-end;
    gap: var(--sp-2);
  }
  .row.two .lbl,
  .row.three .lbl {
    flex: 1;
    margin-bottom: var(--sp-3);
  }
  .narrow {
    flex: 0 0 96px !important;
  }

  .text {
    display: block;
    width: 100%;
    height: 32px;
    margin-top: 4px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    border: 1px solid var(--border-strong);
    background: var(--bg);
    font: inherit;
    font-size: var(--text-base);
    color: var(--fg);
    user-select: text;
  }
  .text:focus {
    outline: none;
    border-color: var(--accent);
  }
  .text.mid {
    font-variant-numeric: tabular-nums;
  }

  .swatches {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 6px;
  }

  .swatch {
    width: 20px;
    height: 20px;
    border-radius: 99px;
    background: var(--c);
    box-shadow: 0 0 0 1px rgb(0 0 0 / 0.08) inset;
  }
  .swatch.on {
    box-shadow:
      0 0 0 2px var(--bg-raised) inset,
      0 0 0 4px var(--c);
  }

  /* ── The tracker list ──────────────────────────────────────────────── */

  .trackers {
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-bottom: var(--sp-3);
  }
  .trackers.muted {
    opacity: 0.75;
  }

  /* A tracker this journal does not draw is still listed -- it is a vault
     record and this is where they are edited -- but it should not read as
     part of this page's strip. */
  .tracker.off .what {
    opacity: 0.55;
  }

  .tick.on {
    color: var(--accent);
  }

  .tracker {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2);
    border-radius: var(--radius);
  }
  .tracker:hover {
    background: var(--bg-hover);
  }

  .what {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .tname {
    font-size: var(--text-base);
    font-weight: 560;
    color: var(--fg);
  }

  .sub {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
  }

  .ghost {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    flex: none;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .ghost:hover:not(:disabled) {
    color: var(--fg);
    background: var(--bg-active);
  }
  .ghost.on {
    color: var(--accent);
  }
  .ghost:disabled {
    opacity: 0.3;
  }
  .ghost.tiny {
    width: 20px;
    height: 16px;
  }

  .order {
    display: flex;
    flex-direction: column;
  }
  .up {
    display: block;
    transform: rotate(-90deg);
  }
  .down {
    display: block;
    transform: rotate(90deg);
  }

  /* ── The editor ────────────────────────────────────────────────────── */

  .editor {
    margin: var(--sp-3) 0;
    padding: var(--sp-3);
    border-radius: var(--radius-lg);
    border: 1px solid var(--c);
    background: var(--bg-panel);
  }

  .iconbtn {
    border-radius: var(--radius);
    padding: 2px;
  }
  .iconbtn:hover {
    background: var(--bg-active);
  }

  .picker {
    max-height: 210px;
    overflow-y: auto;
    margin: var(--sp-2) 0;
    padding: var(--sp-2);
    border-radius: var(--radius);
    background: var(--bg-sunken);
  }

  .glabel {
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.02em;
    text-transform: uppercase;
    color: var(--fg-faint);
    margin: var(--sp-3) 0 var(--sp-2);
  }
  .picker .glabel:first-child {
    margin-top: 0;
  }

  .icons {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(38px, 1fr));
    gap: 4px;
  }

  .pick {
    display: grid;
    place-items: center;
    height: 38px;
    border-radius: var(--radius);
  }
  .pick:hover {
    background: var(--bg-active);
  }
  .pick.on {
    box-shadow: 0 0 0 2px var(--c) inset;
  }

  .kinds {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 4px;
    margin: var(--sp-3) 0;
  }

  .kind {
    display: flex;
    flex-direction: column;
    gap: 1px;
    align-items: flex-start;
    padding: var(--sp-2);
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--bg-raised);
    text-align: left;
  }
  .kind:hover {
    border-color: var(--border-strong);
  }
  .kind.on {
    border-color: var(--c);
    background: color-mix(in oklab, var(--c) 8%, transparent);
  }

  .klabel {
    font-size: var(--text-sm);
    font-weight: 600;
    color: var(--fg);
  }
  .khint {
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    line-height: 1.3;
  }

  .check {
    display: flex;
    gap: var(--sp-2);
    align-items: flex-start;
    margin-top: var(--sp-3);
    font-size: var(--text-sm);
    color: var(--fg);
  }
  .check em {
    display: block;
    font-style: normal;
    font-size: var(--text-xs);
    color: var(--fg-subtle);
    line-height: 1.35;
  }

  .editor-foot {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    margin-top: var(--sp-4);
  }
  .spacer {
    flex: 1;
  }

  .link {
    font-size: var(--text-sm);
    color: var(--fg-muted);
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .link:hover {
    color: var(--fg);
  }
  .link.danger:hover {
    color: var(--danger);
  }

  /* ── Adding ────────────────────────────────────────────────────────── */

  .add {
    display: flex;
    gap: var(--sp-2);
    align-items: center;
  }
  .add .text {
    margin-top: 0;
  }
  .add .btn {
    flex: none;
    gap: 4px;
  }

  .presets {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .preset {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 32px;
    padding: 0 var(--sp-3) 0 5px;
    border-radius: 99px;
    border: 1px solid var(--border);
    background: var(--bg-raised);
    font-size: var(--text-sm);
    color: var(--fg-muted);
  }
  .preset:hover:not(:disabled) {
    border-color: var(--c);
    color: var(--fg);
  }
  .preset:disabled {
    opacity: 0.4;
  }
</style>
