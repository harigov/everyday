<script lang="ts">
  // Where health data comes from: each way in, how it is doing, and which
  // device to believe when two of them saw the same thing.
  //
  // A source is one connection -- the iPhone sending on its own through
  // Health Auto Export, an Apple Health export file, readings typed here.
  // Its card says when it was last heard from, how much it has brought,
  // which devices that was (a Watch, a phone, a scale) and, for the phone,
  // each automation that has pushed: its name, the window it asked for, and
  // when. A source can be renamed, paused (what it sends is refused until
  // it is resumed) or deleted -- which asks, and never takes back what it
  // already brought.
  //
  // Priority is the order devices are believed in. Two devices that both
  // counted the same steps in the same quarter hour would add up to double,
  // so only the first one in this list that reported in it is counted.
  // The list is the devices every source has named, with the saved order
  // first; moving one saves the whole order.

  import { health } from '../lib/health.svelte'
  import { focusOnMount } from '../lib/focus'
  import { relativeTime } from '../lib/format'
  import type { IconName } from '../lib/icons'
  import type { Connector, HealthSource, HealthSourceId } from '../lib/types'
  import ConfirmDialog from './ConfirmDialog.svelte'
  import HealthPushSetup from './HealthPushSetup.svelte'
  import Icon from './Icon.svelte'

  const CONNECTORS: Record<Connector, string> = {
    appleExport: 'Apple Health export',
    healthAutoExport: 'iPhone, through Health Auto Export',
    manual: 'Typed in here',
  }
  const ICONS: Record<Connector, IconName> = {
    appleExport: 'archive',
    healthAutoExport: 'globe',
    manual: 'pencil',
  }

  const settings = $derived(health.settings)

  // ── connecting and setting up ────────────────────────────────────────

  let connecting = $state(false)
  /** The source whose phone setup is open. */
  let settingUp = $state<HealthSourceId | null>(null)
  /** A source asked to show its setup again, waiting to be told that this
   *  retires the old key. */
  let renewing = $state<{ id: HealthSourceId; name: string } | null>(null)

  async function connect() {
    connecting = true
    try {
      // The first is "iPhone"; a second phone is told apart from it.
      const phones = health.sources.filter((s) => s.connector === 'healthAutoExport').length
      const made = await health.createSource(
        'healthAutoExport',
        phones === 0 ? 'iPhone' : `iPhone ${phones + 1}`,
      )
      if (made) settingUp = made.id
    } finally {
      connecting = false
    }
  }

  // ── renaming and pausing ─────────────────────────────────────────────

  let renaming = $state<HealthSourceId | null>(null)
  let draftName = $state('')

  function startRename(s: HealthSource) {
    renaming = s.id
    draftName = s.name
  }

  function finishRename(s: HealthSource) {
    // Escape and Enter have already cleared this; a blur after them is not
    // a second answer.
    if (renaming !== s.id) return
    renaming = null
    const next = draftName.trim()
    if (next === '' || next === s.name) return
    void health.saveSource({ ...$state.snapshot(s), name: next })
  }

  function renameKeys(e: KeyboardEvent & { currentTarget: HTMLInputElement }, s: HealthSource) {
    if (e.key === 'Enter') {
      e.preventDefault()
      finishRename(s)
    } else if (e.key === 'Escape') {
      e.preventDefault()
      e.stopPropagation()
      renaming = null
    }
  }

  function togglePaused(s: HealthSource) {
    void health.saveSource({ ...$state.snapshot(s), paused: !s.paused })
  }

  // ── which device counts first ────────────────────────────────────────

  const devices = $derived([...new Set(health.sources.flatMap((s) => s.status.devices))])
  const ranked = $derived([...new Set([...(settings?.priority ?? []), ...devices])])

  let dragFrom = $state<number | null>(null)
  let dragOver = $state<number | null>(null)
  /** What the last move said, for a screen reader. */
  let said = $state('')

  function reorder(from: number, to: number) {
    const current = settings
    if (!current || from === to) return
    const next = [...ranked]
    const [moved] = next.splice(from, 1)
    if (moved === undefined) return
    next.splice(Math.max(0, Math.min(to, next.length)), 0, moved)
    said = `${moved} is now number ${next.indexOf(moved) + 1} of ${next.length}.`
    void health.saveSettings({ ...$state.snapshot(current), priority: next })
  }

  function onDragStart(e: DragEvent, i: number) {
    dragFrom = i
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = 'move'
      e.dataTransfer.setData('text/plain', ranked[i] ?? '')
    }
  }

  function onDragOver(e: DragEvent, i: number) {
    if (dragFrom === null) return
    e.preventDefault()
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move'
    dragOver = i
  }

  function onDrop(e: DragEvent, i: number) {
    e.preventDefault()
    const from = dragFrom
    dragFrom = null
    dragOver = null
    if (from !== null) reorder(from, i)
  }

  function onDragEnd() {
    dragFrom = null
    dragOver = null
  }
</script>

<section class="source-list" aria-labelledby="sources-head">
  <div class="head">
    <span class="eyebrow" id="sources-head">Sources</span>
    <button class="btn btn-outline btn-sm" disabled={connecting} onclick={() => void connect()}>
      <Icon name="plus" size={13} />
      {connecting ? 'Connecting…' : 'Connect your iPhone'}
    </button>
  </div>

  <p class="hint">
    Each source is one way health data gets here: the iPhone sending on its own, an Apple Health
    export you import, or readings you type. Pausing one refuses what it sends. Deleting one never
    deletes what it already brought.
  </p>

  {#if health.sources.length === 0}
    <p class="hint">
      Nothing is connected yet. Connect your iPhone to have your Apple Watch’s readings sent here
      every hour, or import an Apple Health export below.
    </p>
  {/if}

  <ul class="cards">
    {#each health.sources as s (s.id)}
      <li class="card" class:paused={s.paused}>
        <div class="card-head">
          <span class="mark" aria-hidden="true"><Icon name={ICONS[s.connector]} size={16} /></span>
          <div class="titles">
            {#if renaming === s.id}
              <input
                class="rename"
                bind:value={draftName}
                aria-label="Name of this source"
                autocomplete="off"
                spellcheck="false"
                use:focusOnMount
                onblur={() => finishRename(s)}
                onkeydown={(e) => renameKeys(e, s)}
              />
            {:else}
              <span class="name">
                {s.name}
                <button
                  class="icon"
                  aria-label="Rename {s.name}"
                  title="Rename"
                  onclick={() => startRename(s)}
                >
                  <Icon name="pencil" size={13} />
                </button>
              </span>
            {/if}
            <span class="sub">
              {CONNECTORS[s.connector]}{#if s.paused}
                · Paused{/if}
            </span>
          </div>
          <div class="actions">
            <button class="btn btn-outline btn-sm" onclick={() => togglePaused(s)}>
              {s.paused ? 'Resume' : 'Pause'}
            </button>
            <button class="btn btn-ghost-danger btn-sm" onclick={() => health.deleteSource(s)}>
              Delete…
            </button>
          </div>
        </div>

        <dl class="facts">
          <div>
            <dt>Last heard from</dt>
            <dd>
              {s.status.lastReceivedAt ? relativeTime(s.status.lastReceivedAt) : 'not yet'}
            </dd>
          </div>
          <div>
            <dt>Readings so far</dt>
            <dd>{s.status.samplesTotal.toLocaleString()}</dd>
          </div>
          {#if s.status.newestSampleAt}
            <div>
              <dt>Newest reading</dt>
              <dd>{relativeTime(s.status.newestSampleAt)}</dd>
            </div>
          {/if}
        </dl>

        {#if s.status.devices.length > 0}
          <div class="devices">
            <span class="sub-label">Devices</span>
            {#each [...new Set(s.status.devices)] as d (d)}
              <span class="tag">{d}</span>
            {/each}
          </div>
        {/if}

        {#if s.status.automations.length > 0}
          <div class="table-wrap">
            <table>
              <caption class="sub-label">Automations heard from</caption>
              <thead>
                <tr>
                  <th scope="col">Name</th>
                  <th scope="col">Asked for</th>
                  <th scope="col">Last push</th>
                  <th scope="col">In it</th>
                </tr>
              </thead>
              <tbody>
                {#each s.status.automations as a (a.id)}
                  <tr>
                    <td>{a.name || a.id}</td>
                    <td>{a.period || '—'}</td>
                    <td>{a.lastAt ? relativeTime(a.lastAt) : '—'}</td>
                    <td>{a.lastCount.toLocaleString()}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}

        {#if s.status.lastError}
          <p class="error" role="status">Last problem: {s.status.lastError}</p>
        {/if}

        {#if s.connector === 'healthAutoExport'}
          {#if settingUp === s.id}
            <HealthPushSetup source={s} onclose={() => (settingUp = null)} />
          {:else}
            <div>
              <button
                class="btn btn-outline btn-sm"
                onclick={() => (renewing = { id: s.id, name: s.name })}
              >
                Phone setup…
              </button>
            </div>
          {/if}
        {/if}
      </li>
    {/each}
  </ul>

  <div class="priority">
    <span class="sub-label" id="priority-head">Which device counts first</span>
    <p class="hint">
      When two devices counted the same steps or energy in the same quarter hour, only the first one
      on this list is counted, so a Watch and a phone don’t add up to double. Drag to reorder, or
      use the arrows.
    </p>
    {#if ranked.length === 0}
      <p class="hint">Devices show up here once a source has sent something.</p>
    {:else}
      <ol class="ranks" aria-labelledby="priority-head">
        {#each ranked as name, i (name)}
          <li
            class="rank"
            class:lifted={dragFrom === i}
            class:over={dragOver === i && dragFrom !== null && dragFrom !== i}
            ondragover={(e) => onDragOver(e, i)}
            ondragleave={() => {
              if (dragOver === i) dragOver = null
            }}
            ondrop={(e) => onDrop(e, i)}
          >
            <button
              class="grip"
              draggable="true"
              tabindex="-1"
              aria-hidden="true"
              ondragstart={(e) => onDragStart(e, i)}
              ondragend={onDragEnd}
            >
              <Icon name="grip" size={14} />
            </button>
            <span class="place">{i + 1}</span>
            <span class="device">{name}</span>
            <button
              class="icon up"
              aria-label="Move {name} up"
              title="Move up"
              disabled={i === 0}
              onclick={() => reorder(i, i - 1)}
            >
              <Icon name="chevron" size={14} />
            </button>
            <button
              class="icon down"
              aria-label="Move {name} down"
              title="Move down"
              disabled={i === ranked.length - 1}
              onclick={() => reorder(i, i + 1)}
            >
              <Icon name="chevron" size={14} />
            </button>
          </li>
        {/each}
      </ol>
      <p class="sr-only" role="status">{said}</p>
    {/if}
  </div>
</section>

{#if renewing}
  <ConfirmDialog
    title="Make a new key for {renewing.name}?"
    detail="Showing the setup makes a new key and retires the old one, so the phone’s automations need the new key before they can send again. What the phone already sent stays."
    confirmLabel="Make a new key"
    danger={false}
    onconfirm={() => {
      if (renewing) settingUp = renewing.id
      renewing = null
    }}
    oncancel={() => (renewing = null)}
  />
{/if}

<style>
  .source-list {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  .hint {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
    overflow-wrap: anywhere;
  }

  .cards {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    min-width: 0;
    padding: var(--sp-4);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
  }
  .card.paused .name {
    color: var(--fg-subtle);
  }
  .card-head {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
  }
  .mark {
    flex: none;
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border-radius: 50%;
    background: var(--bg-sunken);
    color: var(--fg-subtle);
  }
  .titles {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .name {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    font-size: var(--text-md);
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .sub {
    font-size: var(--text-sm);
    color: var(--fg-subtle);
  }
  .rename {
    width: 100%;
    padding: var(--sp-1) var(--sp-2);
    border: 1px solid var(--accent);
    border-radius: var(--radius-sm);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: var(--text-md);
    font-weight: 600;
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
    outline: none;
  }
  .actions {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--sp-1);
  }
  .icon {
    display: inline-grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .icon:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .icon:disabled {
    opacity: 0.35;
  }

  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2) var(--sp-6);
    margin: 0;
  }
  .facts div {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  dt {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  dd {
    margin: 0;
    font-size: var(--text-base);
    font-variant-numeric: tabular-nums;
  }

  .devices {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
  }
  .sub-label {
    font-size: var(--text-sm);
    font-weight: 550;
    color: var(--fg-muted);
  }

  .table-wrap {
    overflow-x: auto;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--text-sm);
  }
  caption {
    padding-bottom: var(--sp-1);
    text-align: left;
  }
  th {
    padding: var(--sp-1) var(--sp-3) var(--sp-1) 0;
    text-align: left;
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--fg-faint);
    border-bottom: 1px solid var(--border);
  }
  td {
    padding: var(--sp-1) var(--sp-3) var(--sp-1) 0;
    color: var(--fg-muted);
    font-variant-numeric: tabular-nums;
    border-bottom: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }
  .error {
    margin: 0;
  }

  .priority {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding-top: var(--sp-3);
    border-top: 1px solid var(--border);
  }
  .ranks {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .rank {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-1) var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
  }
  .rank.lifted {
    opacity: 0.5;
  }
  .rank.over {
    border-color: var(--accent);
    box-shadow: 0 -2px 0 var(--accent);
  }
  .grip {
    display: grid;
    place-items: center;
    width: 22px;
    height: 28px;
    color: var(--fg-faint);
    cursor: grab;
  }
  .place {
    width: 1.4em;
    font-size: var(--text-sm);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  .device {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .up :global(svg) {
    rotate: -90deg;
  }
  .down :global(svg) {
    rotate: 90deg;
  }

  /* ── A phone ────────────────────────────────────────────────────────
     A card's name gets the width beside its mark, with its buttons on a
     line of their own under it; the little round buttons are a finger
     wide; the order of devices is moved by its arrows, not by a drag a
     finger cannot start. (The wrapper is not `.sources`, which Health's
     settings page has phone rules for that size every `.icon` inside it --
     the drawn icons are `.icon` too.) */
  :global([data-phone]) .head {
    flex-wrap: wrap;
  }
  :global([data-phone]) .card-head {
    flex-wrap: wrap;
  }
  :global([data-phone]) .titles {
    flex: 1 1 calc(100% - 32px - var(--sp-3));
  }
  :global([data-phone]) .actions {
    flex: 1 1 100%;
    justify-content: flex-end;
    gap: var(--sp-2);
  }
  :global([data-phone]) button.icon {
    width: 40px;
    height: 40px;
  }
  :global([data-phone]) .rename {
    min-height: 44px;
  }
  :global([data-phone]) .grip {
    display: none;
  }
  :global([data-phone]) .rank {
    padding-block: 0;
  }
  :global([data-phone]) .card {
    padding: var(--sp-3);
  }
</style>
