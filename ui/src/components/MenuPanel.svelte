<script lang="ts">
  // One panel of a context menu, and -- by importing itself -- any submenu
  // hanging off it.
  //
  // The panel measures itself and then places itself, rather than being told
  // where to go: what it does near the edge of the window depends on how big
  // it is, and only it knows that. `placeMenu` decides the rest.

  import { placeMenu, placeSubmenu, type MenuAction, type MenuItem } from '../lib/menu'
  import Icon from './Icon.svelte'
  import Self from './MenuPanel.svelte'

  let {
    items,
    at = null,
    anchor = null,
    autofocus = false,
    onclose,
    onexit,
  }: {
    items: MenuItem[]
    /** Root panel: where the menu was raised, in client coordinates. */
    at?: { x: number; y: number } | null
    /** Submenu: the panel it opens out of, and the top of its own row. */
    anchor?: { left: number; right: number; top: number } | null
    /** Take focus on open. Set when the keyboard opened this panel. */
    autofocus?: boolean
    /** Close this panel only. On the root, that is the whole menu. */
    onclose: () => void
    /** Close every panel: an item was chosen, or the menu was dismissed. */
    onexit: () => void
  } = $props()

  let panel = $state<HTMLElement | null>(null)
  let rows = $state<(HTMLButtonElement | null)[]>([])
  let pos = $state<{ x: number; y: number } | null>(null)

  /** The item whose submenu is open, and whether a key opened it. */
  let openIndex = $state<number | null>(null)
  let openByKey = $state(false)
  let subAnchor = $state<{ left: number; right: number; top: number } | null>(null)

  // Measured, then placed, then given the keyboard -- in that order and in
  // one pass. Until it is placed it is drawn where it was asked for and left
  // transparent, so nothing is seen jumping into position.
  $effect(() => {
    if (!panel) return
    const box = panel.getBoundingClientRect()
    const size = { width: box.width, height: box.height }
    const viewport = { width: window.innerWidth, height: window.innerHeight }
    pos = anchor ? placeSubmenu(anchor, size, viewport) : at ? placeMenu(at, size, viewport) : null
    // A panel opened by the keyboard puts the caret on its first item; one
    // opened by the mouse takes focus itself, so Escape and the arrow keys
    // work without a row being highlighted under a pointer that is about to
    // choose a different one anyway.
    if (autofocus) focusAt(0, 1)
    else panel.focus({ preventScroll: true })
  })

  function isAction(item: MenuItem): item is MenuAction {
    return item.kind !== 'separator' && item.kind !== 'heading'
  }

  function focusable(i: number): boolean {
    const item = items[i]
    return !!item && isAction(item) && !item.disabled
  }

  /** Focus the first focusable row from `from`, stepping by `step`. */
  function focusAt(from: number, step: 1 | -1) {
    for (let i = from; i >= 0 && i < items.length; i += step) {
      if (focusable(i)) return void rows[i]?.focus()
    }
  }

  function focusedIndex(): number {
    return rows.findIndex((row) => !!row && row === document.activeElement)
  }

  /** Move the caret, wrapping at both ends the way a menu is expected to. */
  function move(step: 1 | -1) {
    const at = focusedIndex()
    const from = at < 0 ? (step === 1 ? 0 : items.length - 1) : at + step
    if (from < 0 || from >= items.length) return focusAt(step === 1 ? 0 : items.length - 1, step)
    const before = document.activeElement
    focusAt(from, step)
    // Nothing left in that direction: wrap rather than stopping dead.
    if (document.activeElement === before) focusAt(step === 1 ? 0 : items.length - 1, step)
  }

  function openSub(i: number, byKey: boolean) {
    const row = rows[i]
    if (!panel || !row) return
    const box = panel.getBoundingClientRect()
    subAnchor = { left: box.left, right: box.right, top: row.getBoundingClientRect().top }
    openByKey = byKey
    openIndex = i
  }

  function closeSub() {
    const i = openIndex
    openIndex = null
    if (i !== null) rows[i]?.focus()
  }

  function choose(item: MenuItem, i: number) {
    if (!isAction(item) || item.disabled) return
    if (item.items) return openSub(i, false)
    // Closed before it runs: an action that opens a dialog must not open it
    // underneath the menu that asked for it.
    onexit()
    void item.run?.()
  }

  function hover(item: MenuItem, i: number) {
    if (!isAction(item)) return
    if (item.items) openSub(i, false)
    else if (openIndex !== null) openIndex = null
  }

  function onKeydown(e: KeyboardEvent) {
    const i = focusedIndex()
    const item = i >= 0 ? items[i] : null
    switch (e.key) {
      case 'ArrowDown':
        move(1)
        break
      case 'ArrowUp':
        move(-1)
        break
      case 'Home':
        focusAt(0, 1)
        break
      case 'End':
        focusAt(items.length - 1, -1)
        break
      case 'ArrowRight':
        if (item && isAction(item) && item.items) openSub(i, true)
        else return
        break
      case 'ArrowLeft':
        // Only a submenu goes back; on the root the key belongs to whatever
        // is behind the menu.
        if (!anchor) return
        onclose()
        break
      case 'Escape':
        onclose()
        break
      case 'Tab':
        onexit()
        break
      default:
        return
    }
    e.preventDefault()
    // Submenus are nested in the panel that owns them, so an unstopped key
    // would be handled again by every panel above this one.
    e.stopPropagation()
  }
</script>

<div
  bind:this={panel}
  class="menu"
  class:placed={!!pos}
  role="menu"
  tabindex="-1"
  style="left: {pos?.x ?? anchor?.right ?? at?.x ?? 0}px; top: {pos?.y ??
    anchor?.top ??
    at?.y ??
    0}px"
  onkeydown={onKeydown}
>
  {#each items as item, i (i)}
    {#if item.kind === 'separator'}
      <div class="rule" role="separator"></div>
    {:else if item.kind === 'heading'}
      <div class="head"><span class="eyebrow">{item.label}</span></div>
    {:else}
      <button
        bind:this={rows[i]}
        class="item"
        class:danger={item.danger}
        class:open={openIndex === i}
        role={item.checked === undefined ? 'menuitem' : 'menuitemcheckbox'}
        aria-checked={item.checked}
        tabindex="-1"
        disabled={item.disabled}
        aria-haspopup={item.items ? 'menu' : undefined}
        aria-expanded={item.items ? openIndex === i : undefined}
        onpointerenter={() => hover(item, i)}
        onclick={() => choose(item, i)}
      >
        <span class="mark" aria-hidden="true">
          {#if item.dot}
            <span class="dot" style="--c: {item.dot}"></span>
          {:else if item.checked}
            <Icon name="tick" size={13} weight={2.2} />
          {:else if item.icon}
            <Icon name={item.icon} size={14} />
          {/if}
        </span>
        <span class="label">{item.label}</span>
        {#if item.hint}<span class="hint">{item.hint}</span>{/if}
        <!-- A swatch keeps the gutter, so the colour in force is ticked on
             the other side rather than having its colour hidden by the tick
             that says it is the one. -->
        {#if item.checked && item.dot}
          <span class="on" aria-hidden="true"><Icon name="tick" size={13} weight={2.2} /></span>
        {/if}
        {#if item.items}<span class="chev"><Icon name="chevron" size={13} /></span>{/if}
      </button>

      {#if item.items && openIndex === i}
        <Self
          items={item.items}
          anchor={subAnchor}
          autofocus={openByKey}
          onclose={closeSub}
          {onexit}
        />
      {/if}
    {/if}
  {/each}
</div>

<style>
  .menu {
    position: fixed;
    z-index: 71;
    min-width: 180px;
    max-width: 320px;
    max-height: calc(100vh - 12px);
    overflow-y: auto;
    padding: var(--sp-1);
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-lg);
    /* Measured before it is placed; see the effect above. Transparent rather
       than hidden for that one frame: `visibility: hidden` cannot hold focus,
       and the menu takes focus the moment it opens. */
    opacity: 0;
    pointer-events: none;
  }
  .menu.placed {
    opacity: 1;
    pointer-events: auto;
  }
  .menu:focus {
    outline: none;
  }

  .item {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    height: 28px;
    padding: 0 var(--sp-2);
    border-radius: var(--radius-sm);
    font-size: var(--text-sm);
    color: var(--fg-muted);
    text-align: left;
  }
  .item:hover,
  .item:focus-visible,
  .item.open {
    background: var(--bg-hover);
    color: var(--fg);
    outline: none;
  }
  .item:disabled {
    color: var(--fg-faint);
    cursor: default;
  }
  .item:disabled:hover {
    background: none;
    color: var(--fg-faint);
  }
  .item.danger {
    color: var(--danger);
  }
  .item.danger:hover,
  .item.danger:focus-visible {
    background: color-mix(in oklab, var(--danger) 12%, transparent);
    color: var(--danger);
  }

  /* One gutter for the tick, the swatch and the icon, so labels line up
     whether or not the row has any of them. */
  .mark {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 15px;
    flex: none;
    color: var(--fg-faint);
  }
  .item:hover .mark,
  .item:focus-visible .mark {
    color: inherit;
  }
  .dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--c);
  }

  .label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .hint {
    flex: none;
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }

  .chev,
  .on {
    display: flex;
    flex: none;
    color: var(--fg-faint);
  }

  .rule {
    height: 1px;
    margin: var(--sp-1) var(--sp-2);
    background: var(--border);
  }

  .head {
    padding: var(--sp-2) var(--sp-2) var(--sp-1);
  }
</style>
