<script lang="ts">
  // Choose the category an app or a site counts under: a small chip that
  // reads as the current one, and a popover listing every category.
  //
  // Answers with a label patch rather than an id, because there are three
  // things somebody can mean and an id spells only two of them: a category
  // of their own choosing, "no category" (which has to be said, or the
  // suggestion comes back), and "back to the suggestion" (which is neither).
  //
  // A suggested category is drawn quieter than a chosen one, with a title
  // saying so, so the list of apps shows at a glance which were sorted by
  // hand. A name typed that matches nothing makes a category, in the next
  // colour the palette offers -- the commonest reason not to find one is
  // that it does not exist yet.

  import { tick } from 'svelte'
  import { screentime, type LabelPatch } from '../lib/screentime.svelte'
  import { CATEGORY_COLORS } from '../lib/screentime'
  import { dismissable } from '../lib/dismiss'
  import { focusOnMount } from '../lib/focus'
  import type { ScreenCategory, ScreenCategoryId } from '../lib/types'
  import Icon from './Icon.svelte'

  interface Props {
    /** The category the item counts under now; `null` is none. */
    value: ScreenCategoryId | null
    /** `value` is the suggestion, not the person's choice. */
    suggested?: boolean
    /** The suggestion, when there is one -- offered as a way back. */
    suggestion?: ScreenCategoryId | null
    /** Called with what to save on the item's label. */
    onchange: (patch: LabelPatch) => unknown
    /** What the item is called, for the button's accessible name. */
    itemName: string
    disabled?: boolean
    /** Which edge of the chip the popover lines up with. */
    align?: 'left' | 'right'
  }

  const {
    value,
    suggested = false,
    suggestion = null,
    onchange,
    itemName,
    disabled = false,
    align = 'left',
  }: Props = $props()

  let open = $state(false)
  let text = $state('')
  let active = $state(0)
  let busy = $state(false)
  let listEl = $state<HTMLElement | null>(null)

  /** A class unique to this instance, so `dismissable` knows its own chip. */
  const uid = `stcp-${Math.random().toString(36).slice(2, 9)}`

  const current = $derived(screentime.categoryOf(value))
  const needle = $derived(text.trim().toLowerCase())

  type Option =
    | { kind: 'category'; category: ScreenCategory }
    | { kind: 'none' }
    | { kind: 'suggested'; category: ScreenCategory }
    | { kind: 'new'; name: string }

  const options = $derived.by((): Option[] => {
    const out: Option[] = []
    for (const c of screentime.categories) {
      if (needle === '' || c.name.toLowerCase().includes(needle)) {
        out.push({ kind: 'category', category: c })
      }
    }
    if (needle === '') {
      out.push({ kind: 'none' })
      const back = screentime.categoryOf(suggestion)
      if (back && !suggested) out.push({ kind: 'suggested', category: back })
    }
    const exact = screentime.categories.some((c) => c.name.trim().toLowerCase() === needle)
    if (needle !== '' && !exact) out.push({ kind: 'new', name: text.trim() })
    return out
  })

  function toggle() {
    if (disabled) return
    if (open) {
      close()
      return
    }
    text = ''
    active = 0
    open = true
  }

  function close() {
    open = false
    text = ''
  }

  function send(patch: LabelPatch) {
    close()
    void onchange(patch)
  }

  /** The next colour the palette offers: the first nobody uses yet. */
  function nextColor(): string {
    const used = new Set(screentime.categories.map((c) => c.color.toLowerCase()))
    return (
      CATEGORY_COLORS.find((c) => !used.has(c.toLowerCase())) ??
      CATEGORY_COLORS[screentime.categories.length % Math.max(1, CATEGORY_COLORS.length)] ??
      ''
    )
  }

  async function create(name: string) {
    if (busy) return
    busy = true
    const made = await screentime.createCategory(name, nextColor())
    busy = false
    if (made) send({ category: made.id, uncategorized: false })
  }

  function pick(option: Option | undefined) {
    if (!option) return
    if (option.kind === 'category') send({ category: option.category.id, uncategorized: false })
    else if (option.kind === 'none') send({ category: null, uncategorized: true })
    else if (option.kind === 'suggested') send({ category: null, uncategorized: false })
    else void create(option.name)
  }

  async function reveal() {
    await tick()
    listEl?.querySelector<HTMLElement>('[data-active="true"]')?.scrollIntoView({ block: 'nearest' })
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      active = Math.min(options.length - 1, active + 1)
      void reveal()
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      active = Math.max(0, active - 1)
      void reveal()
    } else if (e.key === 'Enter') {
      e.preventDefault()
      pick(options[Math.min(active, options.length - 1)])
    } else if (e.key === 'Escape') {
      e.preventDefault()
      e.stopPropagation()
      close()
    }
  }

  function optionKey(option: Option): string {
    return option.kind === 'category' ? option.category.id : option.kind
  }
</script>

<div class="host" class:right={align === 'right'}>
  <button
    class="{uid} chip"
    class:suggested={suggested && !!current}
    {disabled}
    aria-haspopup="listbox"
    aria-expanded={open}
    aria-label="Category of {itemName}: {current?.name ?? 'uncategorised'}{suggested && current
      ? ' (suggested)'
      : ''}"
    title={suggested && current ? `Suggested: ${current.name}` : undefined}
    onclick={toggle}
  >
    {#if current}
      <span class="dot" style:background={screentime.colorOf(current.id)}></span>
      <span class="name">{current.name}</span>
    {:else}
      <span class="dot empty"></span>
      <span class="name none">Uncategorised</span>
    {/if}
  </button>

  {#if open}
    <div class="pop" use:dismissable={{ onaway: close, within: `.${uid}` }}>
      <input
        class="search"
        bind:value={text}
        {onkeydown}
        oninput={() => (active = 0)}
        placeholder="Find or make a category"
        aria-label="Find or make a category"
        aria-controls="{uid}-list"
        autocomplete="off"
        spellcheck="false"
        use:focusOnMount
      />
      <div class="rows" id="{uid}-list" role="listbox" aria-label="Categories" bind:this={listEl}>
        {#each options as option, i (optionKey(option))}
          <button
            class="row"
            class:on={active === i}
            class:new={option.kind === 'new'}
            data-active={active === i}
            role="option"
            aria-selected={option.kind === 'category'
              ? !suggested && value === option.category.id
              : option.kind === 'none'
                ? value === null
                : false}
            disabled={option.kind === 'new' && busy}
            onmouseenter={() => (active = i)}
            onclick={() => pick(option)}
          >
            {#if option.kind === 'category'}
              <span class="dot" style:background={screentime.colorOf(option.category.id)}></span>
              <span class="name">{option.category.name}</span>
              {#if value === option.category.id}<Icon name="tick" size={13} />{/if}
            {:else if option.kind === 'none'}
              <span class="dot empty"></span>
              <span class="name">Uncategorised</span>
              {#if value === null}<Icon name="tick" size={13} />{/if}
            {:else if option.kind === 'suggested'}
              <Icon name="refresh" size={13} />
              <span class="name">Back to the suggestion, {option.category.name}</span>
            {:else}
              <Icon name="plus" size={13} />
              <span class="name">New category “{option.name}”</span>
            {/if}
          </button>
        {:else}
          <p class="empty">No categories yet. Type a name to make one.</p>
        {/each}
      </div>
    </div>
  {/if}
</div>

<style>
  .host {
    position: relative;
    min-width: 0;
  }

  /* A quiet pill: the category reads as a word with its swatch until the
     pointer is on it. */
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 100%;
    height: 24px;
    padding: 0 var(--sp-2);
    border-radius: 999px;
    background: var(--bg-sunken);
    color: var(--fg-muted);
    font-size: var(--text-xs);
    font-weight: 550;
    transition: background var(--fast) var(--ease);
  }
  .chip:hover:not(:disabled),
  .chip[aria-expanded='true'] {
    background: var(--bg-active);
    color: var(--fg);
  }
  .chip:disabled {
    opacity: 0.45;
  }
  /* A suggestion, not a choice: the same chip, in a lighter voice. */
  .chip.suggested {
    background: none;
    border: 1px dashed var(--border-strong);
    color: var(--fg-subtle);
  }

  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .name.none {
    color: var(--fg-faint);
  }

  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }
  .dot.empty {
    border: 1px solid var(--border-strong);
    background: none;
  }

  .pop {
    position: absolute;
    z-index: 40;
    top: calc(100% + 4px);
    left: 0;
    display: flex;
    flex-direction: column;
    width: 250px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    box-shadow: var(--shadow-lg);
  }
  .right .pop {
    left: auto;
    right: 0;
  }

  .search {
    margin: var(--sp-2);
    height: 30px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg);
    color: var(--fg);
    font-size: var(--text-sm);
    user-select: text;
  }
  .search:focus {
    outline: none;
    border-color: var(--accent);
  }

  .rows {
    max-height: 280px;
    overflow-y: auto;
    padding: 0 var(--sp-1) var(--sp-1);
  }

  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    min-height: var(--row-h);
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    color: var(--fg);
    font-size: var(--text-sm);
    text-align: left;
  }
  .row .name {
    flex: 1;
  }
  .row.on {
    background: var(--bg-hover);
  }
  .row.new {
    color: var(--accent);
  }

  .empty {
    margin: 0;
    padding: var(--sp-3) var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
</style>
