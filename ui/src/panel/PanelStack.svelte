<script lang="ts">
  // The stack of cards. The shell decides what each says; this draws them,
  // sends presses and typed words back, and tells the shell what only the
  // page can know: how tall the cards are (so the window is exactly that
  // tall, up to most of the screen -- past that the stack scrolls), which
  // cards are being read or written in (the pointer on one, typing in one,
  // words typed in one's box and not yet sent -- a card that is held does not
  // go by itself), when a box wants the keyboard and when it is done with it,
  // and when the head of a card is pressed to move the window. See `panel.ts`
  // for the contract and `cards.ts` for the rules this follows.
  //
  // The window is up while people type and talk, so it is kept cheap: the
  // cards animate only opacity and transform, the page listens for nothing it
  // does not need (no wheel listener -- the lists scroll natively), and the
  // meter's twenty readings a second change one card's bars and nothing else.
  import { onMount } from 'svelte'
  import { fade, fly } from 'svelte/transition'
  import { cubicOut } from 'svelte/easing'
  import PanelCard from './PanelCard.svelte'
  import {
    HOVER_LAPSE_MS,
    blocksBudget,
    cardChrome,
    checkKey,
    checkView,
    copies,
    decisionAction,
    decisionKey,
    followsPointer,
    holdChanges,
    inputKey,
    inputLocked,
    inputMode,
    inputsOf,
    isLocked,
    keepByCard,
    keepCards,
    moreLabel,
    nextExpiry,
    pickTheme,
    proposalView,
    pushLevel,
    reportHeight,
    sentAfter,
    settleChecks,
    settleDecisions,
    settlePresses,
    signature,
    silence,
    tryCopy,
    visible,
    waitingSends,
    without,
    writingIn,
    type CheckPress,
    type DecisionPress,
    type Press,
    type Sent,
  } from './cards'
  import { MOCK, openSource, type PanelSource } from './source'
  import { showTheme } from '../lib/themes'
  import type { Panel, PanelAction, PanelCheck, PanelInput, Proposal } from './panel'

  /** How long after a change the shell is told the height, so a burst of changes is one message. */
  const SETTLE_MS = 40

  /** Where the main window keeps the theme it was given, in this origin's storage. */
  const THEME_KEY = 'jatayu.theme'

  /** A review in a browser can force a theme (`?theme=`); the shell never does. */
  const forced = MOCK ? new URLSearchParams(location.search).get('theme') : null

  let stack = $state<Panel[]>([])
  /** The meter's bars, by card. */
  let levels = $state<Record<string, number[]>>({})
  /** The cards whose folded blocks have been opened ("More"), and those folded to their head. */
  let expanded = $state<Record<string, boolean>>({})
  let folded = $state<Record<string, boolean>>({})
  /** The buttons pressed lately, by card; the boxes ticked and items decided, by card and name. */
  let presses = $state<Record<string, Press>>({})
  let checks = $state<Record<string, CheckPress>>({})
  let decisions = $state<Record<string, DecisionPress>>({})
  /** What is typed in each box, the mode picked over it, and what was last sent from it. */
  let drafts = $state<Record<string, string>>({})
  let modes = $state<Record<string, string>>({})
  let sent = $state<Record<string, Sent>>({})
  /** The card under the pointer, and the card whose box has the keyboard. */
  let hovered = $state<string | null>(null)
  let typing = $state<string | null>(null)
  /** The time those are judged by; moved on whenever the shell speaks and when a press runs out. */
  let now = $state(performance.now())
  /** Whether the person asked their desktop for less movement. */
  let still = $state(matchMedia('(prefers-reduced-motion: reduce)').matches)
  /** The window's scroller, and the column of cards in it, which is measured. */
  let root = $state<HTMLElement | null>(null)
  let column = $state<HTMLElement | null>(null)

  let source: PanelSource | null = null
  let lastHeight: number | null = null
  let settling: ReturnType<typeof setTimeout> | undefined
  /** The cards the shell has been told are held. */
  let told: string[] = []
  /** Whether the shell has been told the page wants the keyboard. */
  let keyboard = false
  /**
   * Whether the webview has been seen to follow the pointer since it last
   * left the window (`followsPointer`), where the pointer was last seen, and
   * the timer that lets go a card held by the pointer when it has not.
   */
  let followed = false
  let lastPoint: { x: number; y: number } | null = null
  let lapse: ReturnType<typeof setTimeout> | undefined

  const view = $derived(visible(stack))
  /** The cards held: the one under the pointer, the one typed in, those with words waiting. */
  const wanted = $derived([hovered, typing, ...writingIn(drafts)])

  function take(next: Panel[]) {
    stack = next
    now = performance.now()
    levels = keepCards(
      levels,
      next.filter((card) => card.meter),
    )
    expanded = keepCards(expanded, next)
    folded = keepCards(folded, next)
    presses = settlePresses(presses, next, now)
    checks = settleChecks(checks, next, now)
    decisions = settleDecisions(decisions, next, now)
    drafts = keepByCard(drafts, next)
    modes = keepByCard(modes, next)
    settleSent(next)
    if (hovered !== null && !next.some((card) => card.id === hovered)) hovered = null
    // A box taken away with the keyboard in it says nothing as it goes.
    if (typing !== null && !next.some((card) => card.id === typing)) {
      typing = null
      wantKeyboard(false)
    }
    // The window is hidden while there is nothing to show, and made its first
    // size again when it next opens (`panel/window.rs`). The stack that
    // follows must be reported even if it is as tall as the last one was.
    if (next.length === 0) lastHeight = null
    schedule()
  }

  /**
   * What became of each submit, given the boxes as they now are: still going,
   * gone through, or failed -- and then its words go back in the box, unless
   * something new has been typed there since.
   */
  function settleSent(next: readonly Panel[]) {
    const boxes = inputsOf(next)
    const kept: Record<string, Sent> = {}
    let changed = false
    for (const [key, was] of Object.entries(sent)) {
      const after = sentAfter(was, boxes.get(key), now)
      if (after.sent) kept[key] = after.sent
      if (after.sent !== was) changed = true
      if (after.restore !== null && (drafts[key] ?? '').trim() === '') drafts[key] = after.restore
    }
    if (changed) sent = kept
  }

  // ── telling the shell how tall the cards are ──────────────────────────

  function schedule() {
    if (settling === undefined) settling = setTimeout(report, SETTLE_MS)
  }

  /**
   * The column of cards is measured, not the scroller around it: that is
   * never taller than the window, so a window that had to grow would never
   * hear that it should.
   */
  function report() {
    settling = undefined
    if (!column || !source) return
    const next = reportHeight(lastHeight, column.getBoundingClientRect().height)
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

  async function tick(card: Panel, check: PanelCheck) {
    const shell = source
    if (!shell) return
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

  async function decide(card: Panel, item: Proposal, to: 'accepted' | 'rejected') {
    const shell = source
    if (!shell) return
    const key = decisionKey(card.id, item.id)
    const at = performance.now()
    if (proposalView(item, decisions[key], at).locked) return
    decisions[key] = { from: item.state, to, at }
    now = at
    try {
      await shell.action(card.id, decisionAction(item.id, to), false)
    } catch (e) {
      console.error(e)
      decisions = without(decisions, key)
    }
  }

  async function submit(card: Panel, input: PanelInput, text: string) {
    const shell = source
    const words = text.trim()
    if (!shell || words === '') return
    const key = inputKey(card.id, input.id)
    const at = performance.now()
    if (inputLocked(sent[key], input, at)) return
    const { action } = inputMode(input, modes[key])
    sent[key] = { text, at, error: input.error, seen: false }
    drafts[key] = ''
    now = at
    try {
      await shell.action(card.id, action, false, words)
      // Taken: the box is free for the next, unless the owner made it busy
      // (`Sent.acked`). Not if it has been sent from again since.
      const was = sent[key]
      if (was?.at === at) sent[key] = { ...was, acked: true }
    } catch (e) {
      console.error(e)
      sent = without(sent, key)
      if ((drafts[key] ?? '').trim() === '') drafts[key] = text
    }
  }

  // A press that the shell never answers runs out, and the buttons come back.
  $effect(() => {
    const ends = nextExpiry(presses, checks, decisions, waitingSends(sent, now))
    if (ends === null) return
    const wait = Math.max(0, ends - performance.now()) + 10
    const expiry = setTimeout(() => {
      now = performance.now()
      presses = settlePresses(presses, stack, now)
      checks = settleChecks(checks, stack, now)
      decisions = settleDecisions(decisions, stack, now)
      settleSent(stack)
    }, wait)
    return () => clearTimeout(expiry)
  })

  // ── holding a card, and the keyboard ──────────────────────────────────

  // The shell is told which cards are held whenever that changes: the one
  // under the pointer, the one being typed in, and every one with words in a
  // box that have not been sent -- until they are sent or cleared, a card
  // left while the person looks something up elsewhere must not take them
  // with it when it goes.
  $effect(() => {
    const present = view.shown.map((card) => card.id)
    const want = wanted
    const shell = source
    if (!shell) return
    const { held, changes } = holdChanges(told, want, present)
    told = held
    for (const [id, on] of changes) shell.hold(id, on).catch(console.error)
  })

  function wantKeyboard(want: boolean) {
    if (want === keyboard || !source) return
    keyboard = want
    source.focus(want).catch(console.error)
  }

  /** A box to type in: the panel's text fields. */
  function isBox(target: EventTarget | null): target is HTMLElement {
    return target instanceof HTMLElement && target.matches('textarea, input')
  }

  /** The id of the card `target` is in. */
  function cardOf(target: Element): string | null {
    return target.closest<HTMLElement>('[data-card]')?.dataset.card ?? null
  }

  /**
   * Whether a press on `target` will put the caret in a box: a press in the
   * box itself, or on a button that fills it and moves the caret there (the
   * ready-made replies, marked `data-keyboard`). Such a button keeps the press
   * from focusing anything and focuses the box from script; WebView2 holds
   * the `focusin` from that back until the window is active, which a panel
   * that has not asked for the keyboard never is.
   */
  function wantsKeyboard(target: EventTarget | null): boolean {
    if (isBox(target)) return true
    return target instanceof Element && target.closest('[data-keyboard]') !== null
  }

  // ── the page's own listeners ──────────────────────────────────────────

  /**
   * A press about to put the caret in a box asks for the keyboard first,
   * before the webview focuses it -- from the press itself, which is what the
   * desktop lets a window take the keyboard for.
   */
  function pointerDown(event: PointerEvent) {
    pointerSeen(event)
    if (wantsKeyboard(event.target)) wantKeyboard(true)
  }

  /**
   * A press on a card's head, not on a button in it, moves the window; a
   * second press of a double-click puts it back in its corner. `mousedown`,
   * not `pointerdown`, for its `detail`, the click count.
   */
  function mouseDown(event: MouseEvent) {
    if (event.button !== 0 || !(event.target instanceof Element) || !source) return
    const grip = event.target.closest('[data-grip]')
    if (!grip || event.target.closest('button, a, input, textarea, [role="button"]')) return
    if (event.detail >= 2) source.resetPlace().catch(console.error)
    else source.drag().catch(console.error)
  }

  function focusIn(event: FocusEvent) {
    if (!isBox(event.target)) return
    typing = cardOf(event.target)
    wantKeyboard(true)
  }

  function focusOut(event: FocusEvent) {
    if (!isBox(event.target)) return
    // From one box straight into another: the keyboard is still wanted.
    if (isBox(event.relatedTarget) && root?.contains(event.relatedTarget)) return
    typing = null
    wantKeyboard(false)
  }

  /**
   * The pointer seen over the window -- gone over something, moved, pressed,
   * let go: the card it is on is under it, or none between cards. Until the
   * webview has shown that it follows the pointer, and so will hear it leave
   * (`followsPointer`), that lapses `HOVER_LAPSE_MS` after the last sign of
   * it: a macOS panel that is not the key window hears a press, and nothing
   * of the pointer after, so a card pressed and left would otherwise be held
   * for good.
   */
  function pointerSeen(event: PointerEvent) {
    if (followsPointer(event, lastPoint)) followed = true
    lastPoint = { x: event.clientX, y: event.clientY }
    hovered = event.target instanceof Element ? cardOf(event.target) : null
    clearTimeout(lapse)
    lapse = followed ? undefined : setTimeout(lapsed, HOVER_LAPSE_MS)
  }

  function lapsed() {
    lapse = undefined
    hovered = null
  }

  /**
   * The pointer has gone, or may have: nothing is under it, and the next
   * time it comes the webview must show again that it follows it. Where it
   * was last seen is kept, so the webview's own move to that same place,
   * after the layout changes, is still not taken for the pointer's.
   */
  function pointerOff() {
    clearTimeout(lapse)
    lapse = undefined
    followed = false
    hovered = null
  }

  /**
   * The pointer has left the window altogether. WebKitGTK does not always
   * send the cards a `pointerleave` when the pointer goes straight off the
   * window (a fast move, a warp) -- only the document hears it go, as a
   * `mouseout` to nowhere -- and a card that stayed held would never go by
   * itself.
   */
  function pointerGone(event: MouseEvent) {
    if (event.relatedTarget === null) pointerOff()
  }

  /**
   * The window has lost the keyboard: the person clicked into something
   * else. The box that had it hears no `focusout` in WebKitGTK when the whole
   * window loses focus, so neither the hold nor the keyboard request would
   * ever be let go; the pointer is gone too, by then, or is about to be. (On
   * macOS, too, a panel that stops being the key window stops hearing the
   * pointer.) Words typed in the box stay, and keep their card held.
   */
  function windowBlurred() {
    typing = null
    pointerOff()
    wantKeyboard(false)
  }

  // ── the theme ─────────────────────────────────────────────────────────

  /** The main window's theme as the shell last said it, or null before it has. */
  let shellTheme: string | null = null

  function stored(): string | null {
    try {
      return localStorage.getItem(THEME_KEY)
    } catch {
      return null
    }
  }

  function paint() {
    const dark = matchMedia('(prefers-color-scheme: dark)').matches
    showTheme(pickTheme(forced, shellTheme, stored(), dark))
  }

  onMount(() => {
    let live = true
    let heard = false
    let heardTheme = false
    const stops: Array<() => void> = []
    /** Keep a way to stop listening, or use it now if the page is already gone. */
    const keep = (stop: () => void) => (live ? stops.push(stop) : stop())

    const motion = matchMedia('(prefers-reduced-motion: reduce)')
    const follow = (event: MediaQueryListEvent) => (still = event.matches)
    motion.addEventListener('change', follow)

    // The stored choice and the desktop's scheme still count when the shell
    // has not said which theme the main window shows.
    const scheme = matchMedia('(prefers-color-scheme: dark)')
    const onStorage = (event: StorageEvent) => {
      if (event.key === THEME_KEY) paint()
    }
    scheme.addEventListener('change', paint)
    window.addEventListener('storage', onStorage)
    window.addEventListener('blur', windowBlurred)
    document.addEventListener('mouseout', pointerGone)

    const observer = new ResizeObserver(schedule)
    if (column) observer.observe(column)
    const el = root
    if (el) {
      el.addEventListener('pointerdown', pointerDown)
      el.addEventListener('mousedown', mouseDown)
      el.addEventListener('focusin', focusIn)
      el.addEventListener('focusout', focusOut)
      el.addEventListener('pointerover', pointerSeen)
      el.addEventListener('pointermove', pointerSeen)
      el.addEventListener('pointerup', pointerSeen)
      el.addEventListener('pointerleave', pointerOff)
    }

    /** The main window's theme, from the shell: apart, so a failure here never keeps cards away. */
    const followTheme = (opened: PanelSource) => {
      void (async () => {
        keep(
          await opened.onTheme((theme) => {
            heardTheme = true
            shellTheme = theme
            paint()
          }),
        )
        const theme = await opened.theme()
        if (live && !heardTheme) {
          shellTheme = theme
          paint()
        }
      })().catch(console.error)
    }

    void (async () => {
      const opened = await openSource()
      if (!live) return
      source = opened
      keep(
        await opened.onState((next) => {
          heard = true
          take(next)
        }),
      )
      keep(
        await opened.onLevel(({ id, level }) => {
          if (!stack.some((card) => card.id === id && card.meter)) return
          levels[id] = pushLevel(levels[id] ?? silence(), level)
        }),
      )
      followTheme(opened)
      // A page that loaded after the cards went up asks where things stand --
      // unless the shell has already said, which is newer.
      const current = await opened.state()
      if (live && !heard) take(current)
      schedule()
    })().catch(console.error)

    return () => {
      live = false
      for (const stop of stops) stop()
      motion.removeEventListener('change', follow)
      scheme.removeEventListener('change', paint)
      window.removeEventListener('storage', onStorage)
      window.removeEventListener('blur', windowBlurred)
      document.removeEventListener('mouseout', pointerGone)
      observer.disconnect()
      if (el) {
        el.removeEventListener('pointerdown', pointerDown)
        el.removeEventListener('mousedown', mouseDown)
        el.removeEventListener('focusin', focusIn)
        el.removeEventListener('focusout', focusOut)
        el.removeEventListener('pointerover', pointerSeen)
        el.removeEventListener('pointermove', pointerSeen)
        el.removeEventListener('pointerup', pointerSeen)
        el.removeEventListener('pointerleave', pointerOff)
      }
      clearTimeout(settling)
      clearTimeout(lapse)
    }
  })
</script>

<svelte:document onvisibilitychange={seen} />

<div class="stack" bind:this={root}>
  <div class="cards" class:up={view.shown.length > 0} bind:this={column}>
    {#each view.shown as card (card.id)}
      <div
        class="slot"
        data-card={card.id}
        in:fly={{ y: 10, duration: still ? 0 : 200, easing: cubicOut }}
        out:fly={{ x: 28, duration: still ? 0 : 160, easing: cubicOut }}
      >
        <PanelCard
          {card}
          levels={levels[card.id]}
          open={expanded[card.id] ?? false}
          folded={folded[card.id] ?? false}
          locked={isLocked(presses[card.id], card, now)}
          holding={wanted.includes(card.id)}
          budget={blocksBudget(window.screen.availHeight, view.shown.length, cardChrome(card))}
          checkOf={(check: PanelCheck) =>
            checkView(check, checks[checkKey(card.id, check.action)], now)}
          proposalOf={(item: Proposal) =>
            proposalView(item, decisions[decisionKey(card.id, item.id)], now)}
          inputOf={(input: PanelInput) => {
            const key = inputKey(card.id, input.id)
            return {
              draft: drafts[key] ?? '',
              picked: modes[key],
              locked: inputLocked(sent[key], input, now),
            }
          }}
          onpress={(action: PanelAction) => void press(card, action)}
          oncheck={(check: PanelCheck) => void tick(card, check)}
          ondecide={(item: Proposal, to: 'accepted' | 'rejected') => void decide(card, item, to)}
          ondraft={(input: PanelInput, text: string) =>
            (drafts[inputKey(card.id, input.id)] = text)}
          onmode={(input: PanelInput, mode: string) => (modes[inputKey(card.id, input.id)] = mode)}
          onsubmit={(input: PanelInput, text: string) => void submit(card, input, text)}
          ontoggle={() => (expanded[card.id] = !(expanded[card.id] ?? false))}
          onfold={() => (folded[card.id] = !(folded[card.id] ?? false))}
        />
      </div>
    {/each}
    {#if view.more > 0}
      <p class="rest" transition:fade={{ duration: still ? 0 : 120 }}>{moreLabel(view.more)}</p>
    {/if}
  </div>
</div>

<style>
  /* The window is as tall as the column of cards, up to most of the screen
     (`MOST_OF_AREA` in `panel/window.rs`); a taller stack scrolls here, as a
     whole, so the box and the buttons of the card at the foot can still be
     reached. Sideways it never scrolls: a card sliding out is cut off at the
     window's edge, as it would be anyway. No scroll bar: one that took room
     would squeeze every card for a moment each time the stack grows, before
     the window has caught up. A wheel, a touchpad and the keyboard scroll it
     all the same, and a card cut off at the window's foot shows there is
     more. */
  .stack {
    width: 100%;
    max-width: 400px;
    max-height: 100vh;
    margin-left: auto;
    overflow-x: hidden;
    overflow-y: auto;
    overscroll-behavior: contain;
    scrollbar-width: none;
  }

  .stack::-webkit-scrollbar {
    display: none;
  }

  /* 400 logical pixels wide: the cards 376, and 12 pixels either side for
     their shadows, which a window clips at its edge. A little more below,
     where the shadows fall. The padding is only there while there are cards:
     an empty stack is no height at all. */
  .cards {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .cards.up {
    padding: 12px 12px 20px;
  }

  .slot {
    min-width: 0;
  }

  /* Cards that are waiting: a quiet pill, not a card. It is over whatever is
     behind the window, so it has a ground of its own. */
  .rest {
    align-self: flex-end;
    padding: 3px 10px;
    border-radius: var(--radius-pill);
    background: var(--float);
    box-shadow:
      0 0 0 1px var(--border-strong),
      var(--shadow-sm);
    font-family: var(--font-ui);
    font-size: var(--text-xs);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--fg-muted);
    -webkit-user-select: none;
    user-select: none;
  }
</style>
