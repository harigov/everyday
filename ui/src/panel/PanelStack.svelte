<script lang="ts">
  // The stack of cards. The shell decides what each says; this draws them,
  // sends presses back, and tells the shell how tall the result is so the
  // window can be exactly that tall. See `panel.ts` for the contract and
  // `cards.ts` for the rules this follows.
  //
  // The window is up while people type and talk, so it is kept cheap: the
  // cards animate only opacity and transform, the page listens for nothing it
  // does not need (no wheel listener -- the lists scroll natively), and the
  // meter's twenty readings a second change one card's bars and nothing else.
  import { onMount } from 'svelte'
  import { fade, fly } from 'svelte/transition'
  import PanelCard from './PanelCard.svelte'
  import {
    checkKey,
    checkView,
    copies,
    isLocked,
    keepCards,
    listsBudget,
    moreLabel,
    nextExpiry,
    pushLevel,
    reportHeight,
    settleChecks,
    settlePresses,
    signature,
    silence,
    tryCopy,
    visible,
    without,
    type CheckPress,
    type Press,
  } from './cards'
  import { openSource, type PanelSource } from './source'
  import type { Panel, PanelAction, PanelCheck, PanelItem } from './panel'

  /** How long after a change the shell is told the height, so a burst of changes is one message. */
  const SETTLE_MS = 40

  let stack = $state<Panel[]>([])
  /** The meter's bars, by card. */
  let levels = $state<Record<string, number[]>>({})
  /** The cards whose folded lists have been opened. */
  let expanded = $state<Record<string, boolean>>({})
  /** The buttons that were pressed lately, by card; and the boxes, by card and action. */
  let presses = $state<Record<string, Press>>({})
  let checks = $state<Record<string, CheckPress>>({})
  /** The time those are judged by; moved on whenever the shell speaks and when a press runs out. */
  let now = $state(performance.now())
  /** Whether the person asked their desktop for less movement. */
  let still = $state(matchMedia('(prefers-reduced-motion: reduce)').matches)
  let root = $state<HTMLElement | null>(null)

  let source: PanelSource | null = null
  let lastHeight: number | null = null
  let settling: ReturnType<typeof setTimeout> | undefined

  const view = $derived(visible(stack))
  const budget = $derived(listsBudget(window.screen.availHeight, view.shown.length))

  function take(next: Panel[]) {
    stack = next
    now = performance.now()
    levels = keepCards(
      levels,
      next.filter((card) => card.meter),
    )
    expanded = keepCards(expanded, next)
    presses = settlePresses(presses, next, now)
    checks = settleChecks(checks, next, now)
    // The window is hidden while there is nothing to show, and made its first
    // size again when it next opens (`panel/window.rs`). The stack that
    // follows must be reported even if it is as tall as the last one was.
    if (next.length === 0) lastHeight = null
    schedule()
  }

  // ── telling the shell how tall the cards are ──────────────────────────

  function schedule() {
    if (settling === undefined) settling = setTimeout(report, SETTLE_MS)
  }

  function report() {
    settling = undefined
    if (!root || !source) return
    const next = reportHeight(lastHeight, root.getBoundingClientRect().height)
    lastHeight = next.last
    if (next.send === null) return
    source.resize(next.send).catch((e: unknown) => {
      console.error(e)
      lastHeight = null
    })
  }

  /** A window that is shown again is told again: it may have been given its first size back. */
  function seen() {
    if (document.visibilityState !== 'visible') return
    lastHeight = null
    schedule()
  }

  // ── presses ───────────────────────────────────────────────────────────

  async function press(card: Panel, action: PanelAction) {
    const shell = source
    const at = performance.now()
    if (!shell || isLocked(presses[card.id], card, at)) return
    presses[card.id] = { signature: signature(card), at }
    now = at
    // Before anything else is awaited, so the click still counts as the person's.
    const copied = copies(action) ? await tryCopy(action.copy, navigator.clipboard) : false
    try {
      await shell.action(card.id, action.id, copied)
    } catch (e) {
      console.error(e)
      presses = without(presses, card.id)
    }
  }

  async function tick(card: Panel, item: PanelItem) {
    const shell = source
    const check = item.check
    if (!shell || !check) return
    const key = checkKey(card.id, check.action)
    const at = performance.now()
    if (checkView(check, checks[key], at).locked) return
    checks[key] = { done: check.done, at }
    now = at
    try {
      await shell.action(card.id, check.action, false)
    } catch (e) {
      console.error(e)
      checks = without(checks, key)
    }
  }

  // A press that the shell never answers runs out, and the buttons come back.
  $effect(() => {
    const ends = nextExpiry(presses, checks)
    if (ends === null) return
    const wait = Math.max(0, ends - performance.now()) + 10
    const expiry = setTimeout(() => {
      now = performance.now()
      presses = settlePresses(presses, stack, now)
      checks = settleChecks(checks, stack, now)
    }, wait)
    return () => clearTimeout(expiry)
  })

  onMount(() => {
    let live = true
    let heard = false
    const stops: Array<() => void> = []
    /** Keep a way to stop listening, or use it now if the page is already gone. */
    const keep = (stop: () => void) => (live ? stops.push(stop) : stop())

    const query = matchMedia('(prefers-reduced-motion: reduce)')
    const follow = (event: MediaQueryListEvent) => (still = event.matches)
    query.addEventListener('change', follow)

    const observer = new ResizeObserver(schedule)
    if (root) observer.observe(root)

    void (async () => {
      const opened = await openSource()
      if (!live) return
      source = opened
      const stopState = await opened.onState((next) => {
        heard = true
        take(next)
      })
      keep(stopState)
      const stopLevel = await opened.onLevel(({ id, level }) => {
        if (!stack.some((card) => card.id === id && card.meter)) return
        levels[id] = pushLevel(levels[id] ?? silence(), level)
      })
      keep(stopLevel)
      // A page that loaded after the cards went up asks where things stand --
      // unless the shell has already said, which is newer.
      const current = await opened.state()
      if (live && !heard) take(current)
      schedule()
    })().catch(console.error)

    return () => {
      live = false
      for (const stop of stops) stop()
      query.removeEventListener('change', follow)
      observer.disconnect()
      clearTimeout(settling)
    }
  })
</script>

<svelte:document onvisibilitychange={seen} />

<div class="stack" class:up={view.shown.length > 0} bind:this={root}>
  {#each view.shown as card (card.id)}
    <div
      class="slot"
      in:fly={{ y: -8, duration: still ? 0 : 160 }}
      out:fade={{ duration: still ? 0 : 120 }}
    >
      <PanelCard
        {card}
        levels={levels[card.id]}
        open={expanded[card.id] ?? false}
        locked={isLocked(presses[card.id], card, now)}
        lists={budget}
        checkOf={(check: PanelCheck) =>
          checkView(check, checks[checkKey(card.id, check.action)], now)}
        onpress={(action: PanelAction) => void press(card, action)}
        oncheck={(item: PanelItem) => void tick(card, item)}
        ontoggle={() => (expanded[card.id] = !(expanded[card.id] ?? false))}
      />
    </div>
  {/each}
  {#if view.more > 0}
    <p class="rest" transition:fade={{ duration: still ? 0 : 120 }}>{moreLabel(view.more)}</p>
  {/if}
</div>

<style>
  :global(html),
  :global(body) {
    margin: 0;
    background: transparent;
    overflow: hidden;
  }

  /* In a browser, with no desktop behind the window: the theme's own backdrop. */
  :global(body.mock) {
    min-height: 100vh;
    background: var(--backdrop);
  }

  /* The window is as tall as this. Its padding is the room the cards' shadows
     need inside a window that clips what is outside it, and it is only there
     while there are cards: an empty stack is no height at all. */
  .stack {
    box-sizing: border-box;
    width: 100%;
    max-width: 380px;
    margin-left: auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .stack.up {
    padding: 4px 12px 16px;
  }

  .slot {
    min-width: 0;
  }

  /* Cards that are waiting: a quiet line, not a card. It is over whatever is
     behind the window, so it has a ground of its own. */
  .rest {
    align-self: flex-end;
    margin: 0;
    padding: 3px 10px;
    border: 1px solid var(--border-strong);
    border-radius: 999px;
    background: var(--float);
    box-shadow: var(--shadow-sm);
    font-family: var(--font-ui);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--fg-muted);
    -webkit-user-select: none;
    user-select: none;
  }
</style>
