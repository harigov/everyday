<script lang="ts">
  // Everything the application can do, findable by typing.
  //
  // The list is the one action table in `shortcuts.svelte.ts` — the same rows
  // the keyboard dispatches and the tray offers — filtered by whether each one
  // applies right now. So an action reaches the palette by existing, and there
  // is no second registry to forget.
  //
  // # What it does when nothing matches
  //
  // Offers to keep what was typed. That is the Spotlight-shaped half of this
  // and it is the reason the field is a text field rather than a filter: most
  // of what somebody opens a palette to do is put a thing somewhere before
  // they forget it, and "no results" is a dead end where "add as a task" is
  // the answer.
  //
  // The grammars are the apps' own, not a third one written here. A task line
  // is `parseQuickAdd`, so `Book the flights #travel !high ~1h30 @fri` works
  // exactly as it does in the todo app; a reading is `parseQuickTrack`, so
  // `ibuprofen 400mg` and `mood 7/10` do what they do in the journal's strip
  // and the Overview's today pane. Reimplementing either here would be a
  // fourth place for them to disagree.
  import { applicableActions } from '../lib/shortcuts.svelte'
  import { keysLabel } from '../lib/keys'
  import type { Binding } from '../lib/keys'
  import type { IconName } from '../lib/icons'
  import { panels } from '../lib/panels.svelte'
  import { app } from '../lib/state.svelte'
  import { todo } from '../lib/todo.svelte'
  import { library } from '../lib/library.svelte'
  import { notify } from '../lib/notify.svelte'
  import { parseQuickAdd } from '../lib/quickadd'
  import { describeQuickTrack, parseQuickTrack } from '../lib/quicktrack'
  import { tracking } from '../lib/tracking.svelte'
  import { trapFocus } from '../lib/focus'
  import Icon from './Icon.svelte'

  let query = $state('')
  let at = $state(0)
  let field = $state<HTMLInputElement | null>(null)
  let busy = $state(false)

  const mac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform ?? '')

  /**
   * What a row can be: an action from the table, or a way to keep the text.
   *
   * One list rather than two sections, because the keyboard has to move
   * through them as one thing — a palette where the arrow keys stop at a
   * boundary is a palette people stop using.
   */
  type Row =
    | { kind: 'action'; action: Binding; score: number }
    | { kind: 'capture'; label: string; icon: IconName; hint: string; run: () => Promise<unknown> }

  /**
   * How well `action` matches what has been typed.
   *
   * Three tiers rather than a fuzzy distance, because the useful orderings
   * here are coarse and a scored edit distance puts surprising things first.
   * A prefix of the label beats a word inside it, which beats a keyword or the
   * group — so typing "lo" offers "Lock now" before "Library, covers or a
   * list", which is what somebody typing two letters meant.
   */
  function score(action: Binding, needle: string): number {
    if (!needle) return 1
    const label = action.label.toLowerCase()
    if (label.startsWith(needle)) return 4
    if (label.includes(needle)) return 3
    if (action.keywords?.some((k) => k.includes(needle))) return 2
    if (action.group.toLowerCase().includes(needle)) return 1
    return 0
  }

  const needle = $derived(query.trim().toLowerCase())

  const actions = $derived(
    applicableActions()
      // The palette itself, from inside the palette, is a joke rather than an
      // action.
      .filter((a) => a.keys !== 'mod+k')
      .map((action) => ({ kind: 'action' as const, action, score: score(action, needle) }))
      .filter((row) => row.score > 0)
      .sort((a, b) => b.score - a.score),
  )

  /** What to do with the text when it is not the name of anything. */
  const captures = $derived.by((): Row[] => {
    const text = query.trim()
    if (!text) return []
    const rows: Row[] = []
    if (app.supportsTasks) {
      const parsed = parseQuickAdd(text)
      rows.push({
        kind: 'capture',
        label: `Add a task: ${parsed.title || text}`,
        icon: 'check',
        // The parsed fields, so somebody can see the grammar took effect
        // before they commit to it.
        hint: [
          parsed.tags.length > 0 ? parsed.tags.map((t) => `#${t}`).join(' ') : null,
          parsed.priority && parsed.priority !== 'none' ? `!${parsed.priority}` : null,
          parsed.dueDate ?? null,
        ]
          .filter(Boolean)
          .join('  '),
        run: () => todo.add(text),
      })
    }
    // A reading, when the line names a tracker -- or describes one worth
    // making. `logLine` is the one place that resolve-or-create step lives,
    // shared with the journal's strip and the Overview's pane.
    if (app.supportsTrackers) {
      const reading = parseQuickTrack(text, tracking.trackers)
      if (reading.target) {
        rows.push({
          kind: 'capture',
          label: `Record: ${describeQuickTrack(reading)}`,
          icon: 'compass',
          hint: reading.target.kind === 'new' ? 'makes the tracker' : '',
          run: () => tracking.logLine(text),
        })
      }
    }
    if (app.supportsLibrary && library.kinds.length > 0) {
      const shelf = library.kind ?? library.kinds[0]!
      rows.push({
        kind: 'capture',
        label: `Add to ${shelf.name}: ${text}`,
        icon: 'book',
        hint: 'looks it up',
        run: () => library.add(shelf.id, text),
      })
    }
    rows.push({
      kind: 'capture',
      label: `Search for “${text}”`,
      icon: 'search',
      hint: 'this app',
      run: async () => {
        // The debounced setter the search field uses, so this behaves exactly
        // as typing into it does -- including the cancellation that stops an
        // earlier answer landing after a later one.
        app.setQuery(text)
        document.querySelector<HTMLInputElement>('[data-search]')?.focus()
      },
    })
    return rows
  })

  const rows = $derived<Row[]>([...actions, ...captures])

  // Keep the cursor inside the list as it shortens under typing. Without this,
  // typing a fourth letter leaves the highlight past the end and Enter does
  // nothing at all.
  $effect(() => {
    if (at >= rows.length) at = Math.max(0, rows.length - 1)
  })

  $effect(() => {
    if (panels.palette) field?.focus()
  })

  async function choose(row: Row | undefined) {
    if (!row || busy) return
    busy = true
    // Closed *before* the action runs, not after. Almost every one of these
    // ends by putting a cursor somewhere — a capture field, an editor — and a
    // dialog still over the window would take that focus straight back.
    panels.closePalette()
    try {
      if (row.kind === 'action') await row.action.run()
      else await row.run()
    } catch (e) {
      notify.error(e instanceof Error ? e.message : String(e))
    } finally {
      busy = false
      query = ''
      at = 0
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault()
      panels.closePalette()
      return
    }
    if (e.key === 'ArrowDown' || (e.key === 'n' && e.ctrlKey)) {
      e.preventDefault()
      at = rows.length === 0 ? 0 : (at + 1) % rows.length
      return
    }
    if (e.key === 'ArrowUp' || (e.key === 'p' && e.ctrlKey)) {
      e.preventDefault()
      at = rows.length === 0 ? 0 : (at - 1 + rows.length) % rows.length
      return
    }
    if (e.key === 'Enter') {
      e.preventDefault()
      void choose(rows[at])
    }
  }

  /** The heading a row sits under, or null when it repeats the one above. */
  function heading(row: Row, i: number): string | null {
    const label = row.kind === 'action' ? row.action.group : 'Keep what you typed'
    const before = rows[i - 1]
    const previous =
      before === undefined
        ? null
        : before.kind === 'action'
          ? before.action.group
          : 'Keep what you typed'
    return label === previous ? null : label
  }
</script>

{#if panels.palette}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="scrim"
    onclick={() => panels.closePalette()}
    role="presentation"
    onkeydown={onKeydown}
  >
    <div
      class="palette"
      role="dialog"
      aria-modal="true"
      tabindex="-1"
      aria-label="Find a command"
      onclick={(e) => e.stopPropagation()}
      use:trapFocus
    >
      <div class="query">
        <Icon name="search" />
        <!-- svelte-ignore a11y_autofocus -->
        <input
          bind:this={field}
          bind:value={query}
          onkeydown={onKeydown}
          placeholder="Do something, or write a task"
          spellcheck="false"
          autocomplete="off"
          autofocus
          aria-label="Find a command"
        />
      </div>

      {#if rows.length === 0}
        <p class="empty">Nothing here does that.</p>
      {:else}
        <ul class="rows">
          {#each rows as row, i (row.kind === 'action' ? `a:${row.action.group}:${row.action.label}` : `c:${row.label}`)}
            {@const head = heading(row, i)}
            {#if head}
              <li class="head" aria-hidden="true">{head}</li>
            {/if}
            <li>
              <button
                class="row"
                class:on={i === at}
                onclick={() => choose(row)}
                onmouseenter={() => (at = i)}
              >
                {#if row.kind === 'action'}
                  <span class="glyph">
                    {#if row.action.icon}<Icon name={row.action.icon} />{/if}
                  </span>
                  <span class="label">{row.action.label}</span>
                  {#if row.action.checked?.()}
                    <span class="tick"><Icon name="check" /></span>
                  {/if}
                  {#if row.action.keys}
                    <kbd>{keysLabel(row.action.keys, mac)}</kbd>
                  {/if}
                {:else}
                  <span class="glyph"><Icon name={row.icon} /></span>
                  <span class="label">{row.label}</span>
                  {#if row.hint}<span class="hint">{row.hint}</span>{/if}
                {/if}
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 60;
    background: color-mix(in srgb, var(--bg) 55%, transparent);
    backdrop-filter: blur(2px);
    display: flex;
    justify-content: center;
    /* Not centred. A palette that grows downwards from a fixed point does not
       move the row under the cursor as the list shortens. */
    align-items: flex-start;
    padding-top: 12vh;
  }

  .palette {
    width: min(34rem, calc(100vw - 2rem));
    max-height: 60vh;
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border: 1px solid var(--rule);
    border-radius: calc(var(--radius) + 4px);
    box-shadow: 0 18px 48px rgb(0 0 0 / 0.28);
    overflow: hidden;
  }

  .query {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    padding: 0.7rem 0.85rem;
    border-bottom: 1px solid var(--rule);
    color: var(--text-muted);
  }

  .query input {
    flex: 1;
    border: none;
    background: none;
    color: var(--text);
    font: inherit;
    font-size: var(--text-md);
    outline: none;
  }

  .rows {
    list-style: none;
    margin: 0;
    padding: 0.3rem;
    overflow-y: auto;
  }

  .head {
    padding: 0.5rem 0.6rem 0.25rem;
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .row {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.45rem 0.6rem;
    border: none;
    border-radius: var(--radius);
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .row.on {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
  }

  .glyph {
    display: inline-flex;
    width: 1.1rem;
    justify-content: center;
    color: var(--text-muted);
  }

  .label {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tick {
    color: var(--accent);
    display: inline-flex;
  }

  .hint,
  kbd {
    color: var(--text-muted);
    font-size: var(--text-xs);
    white-space: nowrap;
  }

  kbd {
    font-family: inherit;
    border: 1px solid var(--rule);
    border-radius: 4px;
    padding: 0.05rem 0.3rem;
  }

  .empty {
    margin: 0;
    padding: 1.1rem 0.9rem;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
</style>
