<script lang="ts">
  // Choose a category: a button that reads as the value, and a popover that
  // searches every category, grouped under its heading.
  //
  // Searched rather than drawn whole, unlike `PurposePicker`: the starter
  // set alone is a few dozen categories, and the commonest way in is typing
  // the first three letters of "Groceries". The typed line also makes one --
  // the commonest reason not to find a category is that it does not exist
  // yet, and sending somebody off to the Budget to add it is how a field
  // stops being used.
  //
  // "No category" is a first-class row, not a hidden gesture: taking a
  // category off is as ordinary as putting one on.
  //
  // One component for both of its callers. A transaction's detail shows the
  // value on the button; the bar over many ticked rows has no single value
  // to show, so it passes `buttonLabel` and the button says "Categorise".

  import { tick } from 'svelte'
  import { money } from '../lib/money.svelte'
  import { dismissable } from '../lib/dismiss'
  import { focusOnMount } from '../lib/focus'
  import type { MoneyCategory, MoneyCategoryId } from '../lib/types'
  import Icon from './Icon.svelte'

  interface Props {
    value: MoneyCategoryId | null | undefined
    /** Called with the choice; `null` is "no category". */
    onchange: (id: MoneyCategoryId | null) => unknown
    /** Said on the button in place of the value. */
    buttonLabel?: string
    /** What the button says when nothing is set. */
    placeholder?: string
    /** Offer "No category". */
    allowNone?: boolean
    disabled?: boolean
    /** Which edge of the button the popover lines up with. */
    align?: 'left' | 'right'
    /** The button's accessible name, when its text is not enough. */
    label?: string
    /** Drawn as a pill `.btn` (a toolbar) rather than a field (a form). */
    pill?: boolean
  }

  const {
    value,
    onchange,
    buttonLabel,
    placeholder = 'No category',
    allowNone = true,
    disabled = false,
    align = 'left',
    label,
    pill = false,
  }: Props = $props()

  let open = $state(false)
  let text = $state('')
  /** The highlighted row, as an index into `options`. */
  let active = $state(0)
  let busy = $state(false)
  let listEl = $state<HTMLElement | null>(null)

  /** A class unique to this instance, so `dismissable` knows its own button. */
  const uid = `mcp-${Math.random().toString(36).slice(2, 9)}`

  const current = $derived(money.categoryById(value))
  const typed = $derived(text.trim())
  const needle = $derived(typed.toLowerCase())

  function headingOf(c: MoneyCategory): string {
    const group = c.group.trim()
    if (group) return group
    if (c.kind === 'income') return 'Income'
    if (c.kind === 'transfer') return 'Transfers'
    return 'Other'
  }

  /** What is offered, in the order drawn: "No category", each match, then "New…". */
  type Option =
    | { kind: 'none' }
    | { kind: 'category'; category: MoneyCategory; heading: string }
    | { kind: 'new'; name: string }

  const options = $derived.by((): Option[] => {
    const out: Option[] = []
    if (allowNone && needle === '') out.push({ kind: 'none' })
    const matches = money.activeCategories.filter(
      (c) =>
        needle === '' ||
        c.name.toLowerCase().includes(needle) ||
        c.group.toLowerCase().includes(needle),
    )
    // Grouped by heading in the order the headings first appear, so the
    // list keeps the shape the Budget gives it.
    const order: string[] = []
    const byHeading = new Map<string, MoneyCategory[]>()
    for (const c of matches) {
      const h = headingOf(c)
      if (!byHeading.has(h)) {
        byHeading.set(h, [])
        order.push(h)
      }
      byHeading.get(h)!.push(c)
    }
    for (const h of order) {
      for (const c of byHeading.get(h)!) out.push({ kind: 'category', category: c, heading: h })
    }
    const exact = money.activeCategories.some((c) => c.name.trim().toLowerCase() === needle)
    if (typed !== '' && !exact) out.push({ kind: 'new', name: typed })
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

  function choose(id: MoneyCategoryId | null) {
    close()
    void onchange(id)
  }

  async function create(name: string) {
    if (busy) return
    busy = true
    const made = await money.newCategory(name)
    busy = false
    if (made) choose(made.id)
  }

  function pick(option: Option | undefined) {
    if (!option) return
    if (option.kind === 'none') choose(null)
    else if (option.kind === 'category') choose(option.category.id)
    else void create(option.name)
  }

  async function reveal() {
    await tick()
    listEl?.querySelector<HTMLElement>('[data-active="true"]')?.scrollIntoView({ block: 'nearest' })
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      active = Math.max(0, Math.min(options.length - 1, active + 1))
      void reveal()
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      active = Math.max(0, active - 1)
      void reveal()
    } else if (e.key === 'Enter') {
      e.preventDefault()
      // Typing resets the highlight to the top, so it is never past the end
      // of a list that has just narrowed -- but clamp anyway.
      pick(options[Math.min(active, options.length - 1)])
    } else if (e.key === 'Escape') {
      // Claimed, so the list behind does not also take it as "close the
      // transaction".
      e.preventDefault()
      e.stopPropagation()
      close()
    }
  }
</script>

<div class="picker-host" class:right={align === 'right'}>
  <button
    class="{uid} trigger"
    class:btn={pill}
    class:field-like={!pill}
    class:set={!!current && !buttonLabel}
    {disabled}
    aria-haspopup="listbox"
    aria-expanded={open}
    aria-label={label}
    onclick={toggle}
  >
    {#if buttonLabel}
      <Icon name="tag" size={14} />
      <span class="name">{buttonLabel}</span>
    {:else if current}
      <span class="dot" style:background={current.color || 'var(--fg-faint)'}></span>
      <span class="name">{current.name}</span>
    {:else}
      <span class="dot empty"></span>
      <span class="name muted">{placeholder}</span>
    {/if}
    <span class="chev" aria-hidden="true"><Icon name="chevron" size={11} /></span>
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
        {#each options as option, i (option.kind === 'category' ? option.category.id : option.kind)}
          {@const prev = options[i - 1]}
          {#if option.kind === 'category' && (prev?.kind !== 'category' || prev.heading !== option.heading)}
            <div class="heading" aria-hidden="true">{option.heading}</div>
          {/if}
          {#if option.kind === 'none'}
            <button
              class="row"
              class:on={active === i}
              data-active={active === i}
              role="option"
              aria-selected={!buttonLabel && !value}
              onmouseenter={() => (active = i)}
              onclick={() => choose(null)}
            >
              <span class="dot empty"></span>
              <span class="name">No category</span>
              {#if !buttonLabel && !value}<Icon name="tick" size={13} />{/if}
            </button>
          {:else if option.kind === 'category'}
            {@const c = option.category}
            <button
              class="row"
              class:on={active === i}
              data-active={active === i}
              role="option"
              aria-selected={value === c.id}
              onmouseenter={() => (active = i)}
              onclick={() => choose(c.id)}
            >
              <span class="dot" style:background={c.color || 'var(--fg-faint)'}></span>
              <span class="name">{c.name}</span>
              {#if value === c.id}<Icon name="tick" size={13} />{/if}
            </button>
          {:else if option.kind === 'new'}
            <button
              class="row new"
              class:on={active === i}
              data-active={active === i}
              role="option"
              aria-selected="false"
              disabled={busy}
              onmouseenter={() => (active = i)}
              onclick={() => void create(option.name)}
            >
              <Icon name="plus" size={13} />
              <span class="name">New category “{option.name}”</span>
            </button>
          {/if}
        {:else}
          <p class="none">No categories yet. Type a name to make one.</p>
        {/each}
      </div>
    </div>
  {/if}
</div>

<style>
  .picker-host {
    position: relative;
    min-width: 0;
  }

  .trigger {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-width: 0;
    max-width: 100%;
  }

  /* The form shape: reads as the value until the pointer is on it, the way
     `PurposeField` does. */
  .field-like {
    width: 100%;
    min-height: 30px;
    padding: 0 var(--sp-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--bg-raised);
    color: var(--fg-muted);
    font-size: var(--text-sm);
    text-align: left;
  }
  .field-like:hover:not(:disabled),
  .field-like:focus-visible {
    border-color: var(--border-strong);
  }
  .field-like.set {
    color: var(--fg);
  }
  .trigger:disabled {
    opacity: 0.45;
  }

  .name {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .btn .name {
    flex: none;
  }
  .name.muted {
    color: var(--fg-faint);
  }
  .chev {
    display: grid;
    place-items: center;
    flex: none;
    color: var(--fg-faint);
    rotate: 90deg;
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
    width: 280px;
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
    max-height: 300px;
    overflow-y: auto;
    padding: 0 var(--sp-1) var(--sp-1);
  }

  .heading {
    padding: var(--sp-2) var(--sp-2) 2px;
    font-size: var(--text-xs);
    font-weight: 650;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--fg-faint);
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
  .row.on {
    background: var(--bg-hover);
  }
  .row.new {
    color: var(--accent);
  }

  .none {
    margin: 0;
    padding: var(--sp-3) var(--sp-2);
    font-size: var(--text-sm);
    color: var(--fg-faint);
  }
</style>
