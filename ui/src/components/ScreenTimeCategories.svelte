<script lang="ts">
  // The categories time is sorted into: their names, colours and order.
  //
  // The order is the order everything else draws them in -- the stack of
  // every column on the summary, bottom first, and the lists of the
  // category picker -- so it is set here with plain up and down buttons
  // rather than by dragging, which a keyboard can do too.
  //
  // Colours come from the palette (`CATEGORY_COLORS`) and nowhere else: a
  // category's colour is its identity on every chart, and a free colour
  // picker is how two categories end up the same orange. Deleting one asks
  // first; what was in it goes back to uncategorised, or to a suggestion.

  import { CATEGORY_COLORS } from '../lib/screentime'
  import { screentime } from '../lib/screentime.svelte'
  import { dismissable } from '../lib/dismiss'
  import type { ScreenCategory, ScreenCategoryId } from '../lib/types'
  import Icon from './Icon.svelte'

  const categories = $derived([...screentime.categories].sort((a, b) => a.sortOrder - b.sortOrder))

  /** A prefix unique to this instance, so `dismissable` knows a palette's own swatch. */
  const uidBase = `stc-${Math.random().toString(36).slice(2, 9)}`

  /** Whose palette is open. */
  let choosing = $state<ScreenCategoryId | null>(null)
  let newName = $state('')
  let adding = $state(false)

  function save(c: ScreenCategory, patch: Partial<ScreenCategory>) {
    void screentime.saveCategory({ ...$state.snapshot(c), ...patch })
  }

  function rename(c: ScreenCategory, input: HTMLInputElement) {
    const next = input.value.trim()
    if (next === '' || next === c.name) {
      input.value = c.name
      return
    }
    save(c, { name: next })
  }

  function nameKeys(c: ScreenCategory, e: KeyboardEvent) {
    const input = e.currentTarget as HTMLInputElement
    if (e.key === 'Enter') {
      e.preventDefault()
      input.blur()
    } else if (e.key === 'Escape') {
      e.preventDefault()
      e.stopPropagation()
      input.value = c.name
      input.blur()
    }
  }

  function pickColor(c: ScreenCategory, color: string) {
    choosing = null
    if (color !== c.color) save(c, { color })
  }

  /** The first colour nobody uses yet, else the next round the palette. */
  function nextColor(): string {
    const used = new Set(screentime.categories.map((c) => c.color.toLowerCase()))
    return (
      CATEGORY_COLORS.find((c) => !used.has(c.toLowerCase())) ??
      CATEGORY_COLORS[screentime.categories.length % Math.max(1, CATEGORY_COLORS.length)] ??
      ''
    )
  }

  async function add() {
    const name = newName.trim()
    if (!name || adding) return
    adding = true
    const made = await screentime.createCategory(name, nextColor())
    adding = false
    if (made) newName = ''
  }
</script>

<div class="cats scroll">
  <div class="page">
    <p class="lead">
      Each app and site counts under one category. The order here is the order the charts stack them
      in, first at the bottom.
    </p>

    {#if categories.length === 0}
      <p class="quiet">No categories yet. Make one below.</p>
    {:else}
      <ol class="rows">
        {#each categories as c, i (c.id)}
          {@const uid = `${uidBase}-${i}`}
          <li class="row">
            <div class="swatch-host">
              <button
                class="swatch {uid}"
                style:background={screentime.colorOf(c.id)}
                aria-haspopup="true"
                aria-expanded={choosing === c.id}
                aria-label="Colour of {c.name}"
                title="Change the colour"
                onclick={() => (choosing = choosing === c.id ? null : c.id)}
              ></button>
              {#if choosing === c.id}
                <div
                  class="palette"
                  role="group"
                  aria-label="Colours"
                  use:dismissable={{ onaway: () => (choosing = null), within: `.${uid}` }}
                >
                  {#each CATEGORY_COLORS as color, n (color)}
                    <button
                      class="chip"
                      class:on={color.toLowerCase() === c.color.toLowerCase()}
                      style:background={color}
                      aria-label="Colour {n + 1}"
                      aria-pressed={color.toLowerCase() === c.color.toLowerCase()}
                      title={color}
                      onclick={() => pickColor(c, color)}
                    ></button>
                  {/each}
                </div>
              {/if}
            </div>

            <input
              class="name"
              value={c.name}
              aria-label="Name of the category {c.name}"
              spellcheck="false"
              autocomplete="off"
              onblur={(e) => rename(c, e.currentTarget)}
              onkeydown={(e) => nameKeys(c, e)}
            />

            <div class="tools">
              <button
                class="icon-btn"
                aria-label="Move {c.name} up"
                title="Move up"
                disabled={i === 0}
                onclick={() => void screentime.moveCategory(c, -1)}
              >
                <Icon name="arrow-up" size={14} />
              </button>
              <button
                class="icon-btn down"
                aria-label="Move {c.name} down"
                title="Move down"
                disabled={i === categories.length - 1}
                onclick={() => void screentime.moveCategory(c, 1)}
              >
                <Icon name="arrow-up" size={14} />
              </button>
              <button
                class="icon-btn danger"
                aria-label="Delete {c.name}"
                title="Delete…"
                onclick={() => screentime.deleteCategory(c)}
              >
                <Icon name="trash" size={14} />
              </button>
            </div>
          </li>
        {/each}
      </ol>
    {/if}

    <form
      class="new"
      onsubmit={(e) => {
        e.preventDefault()
        void add()
      }}
    >
      <span class="swatch idle" style:background={nextColor() || 'var(--fg-faint)'}></span>
      <input
        class="name field-new"
        bind:value={newName}
        placeholder="New category"
        aria-label="New category"
        spellcheck="false"
        autocomplete="off"
      />
      <button class="btn btn-outline" type="submit" disabled={!newName.trim() || adding}>
        <Icon name="plus" size={14} /> Add
      </button>
    </form>
  </div>
</div>

<style>
  .cats {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .page {
    max-width: 640px;
    margin: 0 auto;
    padding: var(--sp-2) var(--sp-6) var(--fab-clear);
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }

  .lead,
  .quiet {
    margin: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-subtle);
  }

  .rows {
    margin: 0;
    padding: 0;
    list-style: none;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-2) var(--sp-3);
  }
  .row + .row {
    border-top: 1px solid color-mix(in oklab, var(--border) 55%, transparent);
  }

  .swatch-host {
    position: relative;
    flex: none;
  }
  .swatch {
    display: block;
    width: 18px;
    height: 18px;
    border-radius: 5px;
  }
  button.swatch:hover,
  button.swatch[aria-expanded='true'] {
    box-shadow:
      0 0 0 2px var(--bg),
      0 0 0 3px var(--border-strong);
  }
  .swatch.idle {
    flex: none;
    opacity: 0.5;
  }

  .palette {
    position: absolute;
    z-index: 40;
    top: calc(100% + 6px);
    left: -6px;
    display: grid;
    grid-template-columns: repeat(6, 22px);
    gap: 6px;
    padding: var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    box-shadow: var(--shadow-lg);
  }
  .chip {
    width: 22px;
    height: 22px;
    border-radius: 6px;
  }
  .chip.on,
  .chip:hover,
  .chip:focus-visible {
    box-shadow:
      0 0 0 2px var(--bg-raised),
      0 0 0 4px var(--fg-muted);
    outline: none;
  }

  /* A field that reads as the name until it is clicked. */
  .name {
    flex: 1;
    min-width: 0;
    padding: 3px 6px;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: var(--text-base);
    user-select: text;
  }
  .name:hover {
    border-color: var(--border);
  }
  .name:focus {
    outline: none;
    border-color: var(--accent);
    background: var(--bg);
  }

  .tools {
    flex: none;
    display: flex;
    gap: 2px;
  }
  .icon-btn {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border-radius: 50%;
    color: var(--fg-subtle);
  }
  .icon-btn:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .icon-btn:disabled {
    opacity: 0.3;
  }
  .icon-btn.down :global(svg) {
    rotate: 180deg;
  }
  .icon-btn.danger:hover:not(:disabled) {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
  }

  .new {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: 0 var(--sp-3);
  }
  .field-new {
    border-color: var(--border);
    background: var(--bg);
  }
</style>
