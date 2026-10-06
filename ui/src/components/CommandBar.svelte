<script lang="ts">
  // One field at the top of the window: search whatever app is open, or do
  // anything the application can do, or keep what was typed as a task.
  //
  // It replaced two things. Every app had a search field of its own, in its
  // own place and its own shape -- the sidebar in Mail and Notes, the header
  // in Todo and the Library, over the list in the journal, none in the
  // calendar -- so finding the search was a small hunt per app. And the
  // command palette opened in a dialog over everything, which was the right
  // shape for "do something" and the wrong one for "find something": a
  // search somebody is narrowing has to stay put beside the list it narrows.
  //
  // # Two modes, one field
  //
  // **Search** is what clicking the field, `/` or Ctrl/Cmd+F gives. The text
  // *is* the open app's query -- each store keeps its own, through
  // `apps.ts`' `search` -- so typing narrows the list as it goes, exactly as
  // the old fields did, and switching apps puts back what was typed there.
  // A short list underneath offers the commands and captures the text also
  // matches; if one of those is chosen, the app's search is put back the way
  // it was, because what was typed turned out not to be a search.
  //
  // **Command** is what Ctrl/Cmd+K gives, and what the system-wide key raises
  // the window into. The text is the bar's own: nothing is filtered while it
  // is typed, every applicable action is listed when it is empty, and the
  // app's search is one row among the rest. An app with nothing to search
  // -- the calendar, the Overview -- only ever has this mode.
  //
  // The list itself is the one action table in `shortcuts.svelte.ts`, the
  // same rows the keyboard dispatches and the tray offers, filtered by
  // whether each applies right now; and the captures are the apps' own
  // grammars (`parseQuickAdd`, `parseQuickTrack`), not a third copy.

  import { untrack } from 'svelte'
  import { agent } from '../lib/agent.svelte'
  import { APPS } from '../lib/apps'
  import { calendar } from '../lib/calendar.svelte'
  import { headingOf, keyOf, rankActions, type Row } from '../lib/commandbar'
  import { keysLabel } from '../lib/keys'
  import { library } from '../lib/library.svelte'
  import { notify } from '../lib/notify.svelte'
  import { toWire } from '../lib/onscreen'
  import { panels } from '../lib/panels.svelte'
  import { parseQuickAdd } from '../lib/quickadd'
  import { quick as quickState } from '../lib/quick.svelte'
  import { describeQuickTrack, isRecordable, parseQuickTrack } from '../lib/quicktrack'
  import { onScreenNow, searchNow } from '../lib/screen.svelte'
  import { applicableActions } from '../lib/shortcuts.svelte'
  import { app } from '../lib/state.svelte'
  import { todo } from '../lib/todo.svelte'
  import { tracking } from '../lib/tracking.svelte'
  import Icon from './Icon.svelte'

  const mac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform ?? '')

  /** `null` while the bar is not in use. See the header for the two modes. */
  let mode = $state<'search' | 'command' | null>(null)
  /** What is typed in command mode. Search mode types into the app instead. */
  let text = $state('')
  let at = $state(0)
  let open = $state(false)
  let field = $state<HTMLInputElement | null>(null)
  let wrapper = $state<HTMLDivElement | null>(null)
  let busy = false
  /** The app's query when this use of the bar began -- put back if the text
   *  turns out to have been a command rather than a search. */
  let before: string | null = null
  /**
   * What had the keyboard before the bar took it, given back when the bar is
   * dismissed -- Escape, or Ctrl/Cmd+K pressed again. Without it focus fell
   * to the page, where every letter is a shortcut: Ctrl/Cmd+K from the
   * middle of an entry, Escape, and the next word typed started a new entry
   * and opened the assistant. Not given back when a row is chosen, because
   * what the row does decides where the caret goes next.
   */
  let opener: HTMLElement | null = null

  const search = $derived(searchNow())
  const commanding = $derived(mode === 'command' || search === null)
  const value = $derived(commanding ? (mode === null ? '' : text) : search!.query)
  const needle = $derived(value.trim())
  const appName = $derived(panels.settings !== null ? 'Settings' : APPS[app.section].label)

  /** What to do with the text when it is not the name of anything. */
  function captures(typed: string): Row[] {
    const rows: Row[] = []
    if (app.supportsTasks) {
      const parsed = parseQuickAdd(typed)
      rows.push({
        kind: 'capture',
        label: `Add a task: ${parsed.title || typed}`,
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
        run: () => todo.add(typed),
      })
    }
    // A reading, when the line names a tracker -- or describes one worth
    // making. `logLine` is the one place that resolve-or-create step lives,
    // shared with the journal's strip and the Overview's pane.
    if (app.supportsTrackers) {
      const reading = parseQuickTrack(typed, tracking.trackers)
      if (isRecordable(reading)) {
        rows.push({
          kind: 'capture',
          label: `Record: ${describeQuickTrack(reading)}`,
          icon: 'compass',
          hint: reading.target.kind === 'new' ? 'makes the tracker' : '',
          run: () => tracking.logLine(typed),
        })
      }
    }
    // An appointment written as a sentence. A row rather than a parse: no
    // request is made until somebody picks this, which is what keeps
    // "nothing is looked up unless you ask" true here too.
    if (app.supportsCalendar && quickState.enabled('calendar.parse')) {
      rows.push({
        kind: 'capture',
        label: `Book: ${typed}`,
        icon: 'calendar',
        hint: 'reads the date and time from the sentence',
        run: () => calendar.bookFromSentence(typed),
      })
    }
    if (app.supportsLibrary && library.kinds.length > 0) {
      const shelf = library.kind ?? library.kinds[0]!
      rows.push({
        kind: 'capture',
        label: `Add to ${shelf.name}: ${typed}`,
        icon: 'book',
        hint: 'looks it up',
        run: () => library.add(shelf.id, typed),
      })
    }
    return rows
  }

  const rows = $derived.by((): Row[] => {
    if (!open) return []
    const out: Row[] = []
    const searchRow: Row[] =
      search && needle
        ? [{ kind: 'search', label: `Search ${appName.toLowerCase()} for “${needle}”` }]
        : []
    const actions = applicableActions().filter((a) => a.keys !== 'mod+k')
    if (commanding) {
      // Commands first: that is what this mode is for, and Enter takes the
      // first row. The app's search follows whatever matched by name.
      const ranked = rankActions(actions, needle)
      const named = ranked.filter((r) => r.kind === 'action' && r.score >= 3)
      out.push(...named, ...searchRow, ...ranked.slice(named.length))
      if (needle) out.push(...captures(needle))
    } else if (needle) {
      out.push(...searchRow)
      // Under a search the list is a suggestion, not the point, and it sits
      // over the top of the very list being narrowed -- so it stays to what
      // the text plainly names: a command whose own words match, and the
      // one capture people reach for most. Ctrl/Cmd+K has the rest.
      out.push(
        ...rankActions(actions, needle, 3).filter((r) => r.kind === 'action' && r.score >= 3),
      )
      out.push(...captures(needle).slice(0, 1))
    }
    if (needle && agent.supported && app.section !== 'assistant') {
      out.push({ kind: 'ask', label: `Ask ${agent.displayName}: “${needle}”` })
    }
    return out
  })

  // `before` belongs to the app it was read from. Changing app while the bar
  // is in use -- Ctrl/Cmd+J from inside it -- re-reads it, or choosing a
  // command afterwards would put the first app's search into the second's.
  const where = $derived(panels.settings !== null ? 'settings' : app.section)
  $effect(() => {
    void where
    untrack(() => {
      if (mode !== null) before = search?.query ?? null
    })
  })

  // Keep the highlight inside the list as it shortens under typing; without
  // this a fourth letter leaves it past the end and Enter does nothing.
  $effect(() => {
    if (at >= rows.length) at = Math.max(0, rows.length - 1)
  })

  // Ctrl/Cmd+K, the system-wide key and the tray all say "commands" through
  // `panels.palette`; this is the one place that hears it. Closing it the
  // same way -- the same key pressed again -- ends command mode.
  $effect(() => {
    if (panels.palette) {
      if (untrack(() => mode) !== 'command') begin('command', document.activeElement)
    } else if (untrack(() => mode) === 'command') {
      finish()
    }
  })

  /**
   * Start using the bar, or change mode while using it. `from` is what had
   * focus before -- see `opener`. Only a fresh start records it and the
   * app's query: Ctrl/Cmd+K in the middle of a search is the same use of
   * the bar, and must not forget where it began.
   */
  function begin(next: 'search' | 'command', from: EventTarget | null = null) {
    const m = search === null ? 'command' : next
    if (mode === null) {
      opener =
        from instanceof HTMLElement && from !== document.body && !wrapper?.contains(from)
          ? from
          : null
      before = search?.query ?? null
    }
    mode = m
    text = ''
    at = 0
    open = m === 'command'
    if (m === 'command') panels.openPalette()
    field?.focus()
  }

  /** From command mode to search, in place -- Ctrl/Cmd+F while commanding. */
  function toSearch() {
    if (search === null || mode !== 'command') return
    mode = 'search'
    text = ''
    open = false
    at = 0
    panels.closePalette()
  }

  /**
   * Done with the bar: whatever the app's search says now, stays. `restore`
   * gives the keyboard back to `opener`; a chosen row passes `false`.
   */
  function finish(restore = true) {
    const back = restore && opener?.isConnected ? opener : null
    opener = null
    mode = null
    text = ''
    open = false
    at = 0
    before = null
    if (panels.palette) panels.closePalette()
    if (back) back.focus()
    else if (field && document.activeElement === field) field.blur()
  }

  function type(next: string) {
    if (mode === null) begin('search')
    if (commanding) text = next
    else search!.set(next)
    open = commanding || next.trim() !== ''
    at = 0
  }

  async function choose(row: Row | undefined) {
    if (!row || busy) return
    if (row.kind === 'search') {
      // In command mode the text was the bar's own until now.
      if (mode === 'command' && search) search.set(needle)
      finish()
      return
    }
    busy = true
    const typed = needle
    // What was typed was a command, so the app's list goes back to what it
    // was showing before -- not left filtered by "lock".
    if (mode === 'search' && search && before !== null) search.set(before)
    // Closed before it runs: nearly every one of these ends by putting a
    // cursor somewhere -- a capture field, an editor -- and the bar still
    // holding focus would take it straight back.
    finish(false)
    try {
      if (row.kind === 'action') await row.action.run()
      else if (row.kind === 'capture') await row.run()
      else await ask(typed)
    } catch (e) {
      notify.error(e instanceof Error ? e.message : String(e))
    } finally {
      busy = false
    }
  }

  /** Open the rail if it is shut, and say it there -- with the screen, which
   *  is the whole reason to ask from here rather than from the rail. */
  async function ask(typed: string) {
    if (!agent.open) await agent.toggle()
    await agent.ensureLoaded()
    const screen = onScreenNow()
    await agent.send(typed, toWire(screen.app, screen.showing))
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.isComposing) return
    // Ctrl/Cmd+F is "search this app" everywhere else; in command mode the
    // caret is already here, so it is the mode that has to change.
    if (mode === 'command' && search && e.key === 'f' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault()
      e.stopPropagation()
      toSearch()
      return
    }
    if (e.key === 'Escape') {
      e.preventDefault()
      // The window's own Escape -- closing a detail rail, stopping a turn --
      // is not what somebody in the bar meant.
      e.stopPropagation()
      if (open && !commanding) open = false
      else if (!commanding && needle) search!.set('')
      else finish()
      return
    }
    if (e.key === 'ArrowDown' || (e.key === 'n' && e.ctrlKey)) {
      e.preventDefault()
      if (!open) open = true
      else at = rows.length === 0 ? 0 : (at + 1) % rows.length
      return
    }
    if (e.key === 'ArrowUp' || (e.key === 'p' && e.ctrlKey)) {
      e.preventDefault()
      at = rows.length === 0 ? 0 : (at - 1 + rows.length) % rows.length
      return
    }
    if (e.key === 'Enter') {
      e.preventDefault()
      // A search with nothing else asked of it is already applied; Enter
      // hands the keyboard back to the list it narrowed.
      if (open && rows.length > 0) void choose(rows[at])
      else finish()
      return
    }
    if (e.key === 'Tab') open = false
  }

  /** Leaving the bar for anywhere but its own list ends this use of it. */
  function onFocusOut(e: FocusEvent) {
    const to = e.relatedTarget
    if (to instanceof Node && (e.currentTarget as HTMLElement).contains(to)) return
    mode = null
    text = ''
    open = false
    before = null
    opener = null
    if (panels.palette) panels.closePalette()
  }

  const placeholder = $derived(
    commanding ? 'Type a command, or something to keep…' : `${search!.placeholder}…`,
  )
</script>

<div class="bar" class:active={mode !== null} onfocusout={onFocusOut} bind:this={wrapper}>
  <div class="field" class:commanding={mode === 'command'}>
    <span class="lead" aria-hidden="true">
      {#if mode === 'command' || (mode !== null && search === null)}
        <span class="prompt">›</span>
      {:else}
        <Icon name="search" size={15} />
      {/if}
    </span>
    <input
      bind:this={field}
      data-search
      type="text"
      role="combobox"
      aria-expanded={open && rows.length > 0}
      aria-controls="commandbar-rows"
      aria-activedescendant={open && rows[at] ? `commandbar-row-${at}` : undefined}
      aria-label={commanding ? 'Find a command' : (search?.placeholder ?? 'Search')}
      autocomplete="off"
      spellcheck="false"
      {placeholder}
      {value}
      onfocus={(e) => {
        if (mode === null) begin('search', e.relatedTarget)
      }}
      oninput={(e) => type(e.currentTarget.value)}
      onkeydown={onKeydown}
    />
    {#if !commanding && search && search.query}
      <button
        class="clear"
        aria-label="Clear the search"
        onmousedown={(e) => e.preventDefault()}
        onclick={() => search?.set('')}
      >
        <Icon name="close" size={13} />
      </button>
    {:else if mode === null}
      <kbd class="hint" aria-hidden="true">{keysLabel('mod+k', mac)}</kbd>
    {/if}
  </div>

  {#if open && rows.length > 0}
    <!-- The rows keep the caret in the field: pressing one must not blur it
         first, or the bar would end before the click lands. -->
    <ul
      class="rows scroll"
      id="commandbar-rows"
      role="listbox"
      onmousedown={(e) => e.preventDefault()}
    >
      {#each rows as row, i (keyOf(row))}
        {#if i === 0 || headingOf(rows[i - 1]!) !== headingOf(row)}
          <li class="head" role="presentation">{headingOf(row)}</li>
        {/if}
        <li role="presentation">
          <button
            class="row"
            class:on={i === at}
            id="commandbar-row-{i}"
            role="option"
            aria-selected={i === at}
            tabindex="-1"
            onmousedown={(e) => e.preventDefault()}
            onclick={() => void choose(row)}
            onmouseenter={() => (at = i)}
          >
            {#if row.kind === 'action'}
              <span class="glyph">
                {#if row.action.icon}<Icon name={row.action.icon} size={15} />{/if}
              </span>
              <span class="label">{row.action.label}</span>
              {#if row.action.checked?.()}
                <span class="tick"><Icon name="check" size={14} /></span>
              {/if}
              {#if row.action.keys}
                <kbd>{keysLabel(row.action.keys, mac)}</kbd>
              {/if}
            {:else if row.kind === 'capture'}
              <span class="glyph"><Icon name={row.icon} size={15} /></span>
              <span class="label">{row.label}</span>
              {#if row.hint}<span class="note">{row.hint}</span>{/if}
            {:else if row.kind === 'ask'}
              <span class="glyph"><Icon name="sparkle" size={15} /></span>
              <span class="label">{row.label}</span>
              <span class="note">with what is on screen</span>
            {:else}
              <span class="glyph"><Icon name="search" size={15} /></span>
              <span class="label">{row.label}</span>
              <kbd>↵</kbd>
            {/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .bar {
    position: relative;
    width: 100%;
  }

  /* The todo app's capture line, at the size of a title bar: the same
     border, radius and focus ring, so the one control in the window that
     takes typing anywhere looks like the controls that take it somewhere. */
  .field {
    position: relative;
    display: flex;
    align-items: center;
    height: 30px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    color: var(--fg-faint);
    transition:
      border-color var(--fast) var(--ease),
      box-shadow var(--fast) var(--ease);
  }
  .bar.active .field {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }
  .lead {
    display: grid;
    place-items: center;
    width: 32px;
    flex: none;
  }
  .bar.active .lead {
    color: var(--accent);
  }
  .prompt {
    font-size: 17px;
    font-weight: 600;
    line-height: 1;
    translate: 0 -1px;
  }
  input {
    flex: 1;
    min-width: 0;
    height: 100%;
    padding: 0 var(--sp-2) 0 0;
    border: 0;
    background: none;
    color: var(--fg);
    font-size: var(--text-base);
    user-select: text;
  }
  input::placeholder {
    color: var(--fg-faint);
  }
  input:focus {
    outline: none;
  }
  .clear {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    margin-right: 2px;
    border-radius: var(--radius-sm);
    color: var(--fg-faint);
  }
  .clear:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .hint {
    margin-right: var(--sp-2);
  }

  .rows {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    right: 0;
    z-index: 60;
    max-height: min(60vh, 460px);
    margin: 0;
    padding: 4px;
    list-style: none;
    overflow-y: auto;
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: calc(var(--radius) + 2px);
    box-shadow: 0 18px 48px rgb(0 0 0 / 0.28);
  }
  .head {
    padding: 8px 10px 4px;
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .row {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 10px;
    border-radius: var(--radius);
    color: var(--fg);
    font-size: var(--text-base);
    text-align: left;
  }
  .row.on {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
  }
  .glyph {
    display: inline-flex;
    width: 18px;
    justify-content: center;
    color: var(--fg-muted);
  }
  .label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tick {
    display: inline-flex;
    color: var(--accent);
  }
  .note,
  kbd {
    flex: none;
    color: var(--fg-muted);
    font-size: var(--text-xs);
    white-space: nowrap;
  }
  kbd {
    font-family: inherit;
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 1px 5px;
  }
</style>
