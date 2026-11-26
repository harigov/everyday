<script lang="ts">
  // The tags on one note or journal entry, and the way to add and take off.
  //
  // One component for both pages, because they had drifted: Notes offered the
  // tags already in use as you typed and the journal was a bare field, so the
  // same idea grew a second spelling in whichever page was written second.
  // Drawn as a run of chips with no box of its own (`display: contents`), so
  // it wraps with whatever row the page puts it in.
  //
  // Typed text is kept only by Enter or by choosing an offered tag. Leaving
  // the field -- a click elsewhere, Tab -- puts it away: committing on blur
  // filed a half-typed word nobody had finished.

  import { focusOnMount } from '../lib/focus'
  import { mergeTags } from '../lib/tags'
  import Icon from './Icon.svelte'

  interface Props {
    tags: string[]
    /** Tags already in use elsewhere, most used first. Offered as you type. */
    known?: { tag: string; count: number }[]
    onchange: (next: string[]) => void
    /** Distinguishes the listbox when two editors are on a page. */
    id?: string
  }

  const { tags, known = [], onchange, id = 'tag-options' }: Props = $props()

  let adding = $state(false)
  let draft = $state('')
  /** The offered tag the arrow keys are on, or -1 for the typed text. */
  let pick = $state(-1)

  const options = $derived.by(() => {
    if (!adding) return []
    const typed = draft.trim().replace(/^#/, '').toLowerCase()
    const have = new Set(tags.map((t) => t.toLowerCase()))
    const free = known.filter((t) => !have.has(t.tag.toLowerCase()))
    if (!typed) return free.slice(0, 6)
    // Those starting with what was typed before those merely containing it,
    // each run still most used first.
    const starts = free.filter((t) => t.tag.toLowerCase().startsWith(typed))
    const within = free.filter(
      (t) => !t.tag.toLowerCase().startsWith(typed) && t.tag.toLowerCase().includes(typed),
    )
    return [...starts, ...within].slice(0, 6)
  })

  function close() {
    draft = ''
    pick = -1
    adding = false
  }

  function commit(picked?: string) {
    const typed = (picked ?? draft).trim().replace(/^#/, '')
    close()
    if (!typed) return
    // A tag already in use is taken as it is already spelled, so "Boats"
    // typed beside an existing "boats" does not start a second one.
    const tag = known.find((t) => t.tag.toLowerCase() === typed.toLowerCase())?.tag ?? typed
    const next = mergeTags(tags, [tag])
    if (next.length !== tags.length) onchange(next)
  }

  /**
   * The arrows walk the offered tags. Tab does too, but only once something
   * has been typed and only while there is an option to move to: the list
   * shows as soon as the field opens, and a Tab that never left it would
   * trap the keyboard in the field.
   */
  function keys(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      pick = Math.min(pick + 1, options.length - 1)
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      pick = Math.max(pick - 1, -1)
    } else if (e.key === 'Tab' && draft.trim()) {
      const next = pick + (e.shiftKey ? -1 : 1)
      if (next >= -1 && next < options.length) {
        e.preventDefault()
        pick = next
      }
    } else if (e.key === 'Enter') {
      e.preventDefault()
      commit(options[pick]?.tag)
    } else if (e.key === 'Escape') {
      close()
    }
  }
</script>

<span class="tags">
  {#each tags as tag (tag)}
    <button
      class="chip"
      onclick={() => onchange(tags.filter((t) => t !== tag))}
      title="Remove tag"
      aria-label="Remove tag {tag}"
    >
      {tag}<span class="x"><Icon name="close" size={11} weight={1.8} /></span>
    </button>
  {/each}

  {#if adding}
    <span class="field">
      <input
        class="input"
        placeholder="tag"
        aria-label="New tag"
        bind:value={draft}
        use:focusOnMount
        oninput={() => (pick = -1)}
        onblur={close}
        onkeydown={keys}
        role="combobox"
        aria-expanded={options.length > 0}
        aria-controls={id}
        aria-autocomplete="list"
      />
      {#if options.length}
        <!-- Held open through a click: `mousedown` is cancelled so the field
             keeps focus, and its blur does not put the list away before the
             click lands. -->
        <ul class="options" {id} role="listbox">
          {#each options as t, i (t.tag)}
            <!-- The keys are the field's: it is the combobox, and these
                 options are reached through it. -->
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <li
              role="option"
              aria-selected={i === pick}
              class:on={i === pick}
              onmousedown={(e) => e.preventDefault()}
              onclick={() => commit(t.tag)}
            >
              <span class="opt-tag"><span class="hash">#</span>{t.tag}</span>
              <span class="opt-n">{t.count.toLocaleString()}</span>
            </li>
          {/each}
        </ul>
      {/if}
    </span>
  {:else}
    <button class="add" onclick={() => (adding = true)}>
      <Icon name="plus" size={11} weight={1.8} /> Tag
    </button>
  {/if}
</span>

<style>
  .tags {
    display: contents;
  }

  .chip,
  .add {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    height: 22px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-pill);
    background: var(--bg-hover);
    color: var(--fg-muted);
    font-size: var(--text-xs);
  }
  .chip:hover,
  .add:hover {
    color: var(--fg);
  }
  .chip .x {
    display: none;
  }
  .chip:hover .x,
  .chip:focus-visible .x {
    display: inline-flex;
  }

  .field {
    position: relative;
    display: inline-flex;
  }
  .input {
    width: 6rem;
    height: 22px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-pill);
    background: var(--bg);
    color: var(--fg);
    font-size: var(--text-xs);
    user-select: text;
  }

  .options {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 20;
    min-width: 12rem;
    margin: 0;
    padding: 4px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--card);
    box-shadow: var(--shadow);
    list-style: none;
  }
  .options li {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    height: 26px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    cursor: pointer;
  }
  .options li:hover,
  .options li.on {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .opt-tag {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .opt-tag .hash {
    color: var(--fg-faint);
  }
  .opt-n {
    color: var(--fg-faint);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }

  /* ── On a phone ──────────────────────────────────────────────────────── */

  /* Chips a finger can hit and take off -- the cross always there, since
     there is no hover to show it -- and the field for a new one the width of
     the page, where it has room for what is typed and the keyboard's own
     suggestions. */
  :global([data-phone]) .chip,
  :global([data-phone]) .add {
    height: 34px;
    padding: 0 var(--sp-3);
    font-size: var(--text-sm);
    gap: 5px;
  }
  :global([data-phone]) .chip .x {
    display: inline-flex;
  }
  :global([data-phone]) .field {
    flex: 1 0 100%;
  }
  :global([data-phone]) .input {
    width: 100%;
    height: 40px;
    padding: 0 var(--sp-3);
  }
  :global([data-phone]) .options {
    right: 0;
    min-width: 0;
  }
  :global([data-phone]) .options li {
    height: 44px;
    font-size: var(--text-base);
  }
</style>
