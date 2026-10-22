<script lang="ts">
  // Writing help's popup. One selection at a time: offer three things to do
  // with it, ask the model for the one picked, hand the answer to the shell
  // to paste, and be gone. See `crates/everyday-app/src/writing/mod.rs`.
  import { invoke } from '@tauri-apps/api/core'
  import { listen } from '@tauri-apps/api/event'
  import { onMount, tick } from 'svelte'
  import type { WritingAssist } from '../lib/types'
  import {
    CHOICES,
    choiceForKey,
    place,
    preview,
    trouble,
    type Choice,
    type Selection,
    type Trouble,
  } from './writing'

  type View =
    | { kind: 'idle' }
    | { kind: 'menu' }
    | { kind: 'working'; choice: Choice }
    | { kind: 'trouble'; trouble: Trouble }
    | { kind: 'copied'; message: string }

  let selection = $state<Selection | null>(null)
  let view = $state<View>({ kind: 'idle' })
  let card = $state<HTMLElement | null>(null)
  let position = $state<{ left: number; top: number } | null>(null)
  let buttons = $state<HTMLButtonElement[]>([])
  /** The seq a rewrite in flight belongs to; a newer press or a dismissal makes it stale. */
  let asking: number | null = null
  /** Handing the answer to the shell, which hides this window to paste: that blur is not a dismissal. */
  let applying = false

  const overlay = $derived(selection?.mode === 'overlay')
  const pasteKey = /Mac/.test(navigator.platform) ? '⌘V' : 'Ctrl+V'

  function show(next: Selection) {
    selection = next
    asking = null
    view = next.problem
      ? { kind: 'trouble', trouble: { message: next.problem, openMain: false } }
      : { kind: 'menu' }
    void tick().then(() => {
      reposition()
      buttons[0]?.focus()
    })
  }

  function reposition() {
    if (!selection || !card) return
    if (selection.mode !== 'overlay') {
      position = null
      return
    }
    const at =
      selection.x != null && selection.y != null ? { x: selection.x, y: selection.y } : null
    const rect = card.getBoundingClientRect()
    position = place(
      at,
      { w: window.innerWidth, h: window.innerHeight },
      { w: rect.width, h: rect.height },
    )
  }

  function reset() {
    selection = null
    view = { kind: 'idle' }
    asking = null
  }

  async function settle(id: string, outcome: 'cancelled') {
    try {
      await invoke('call', {
        name: 'settle_writing_assist',
        args: { id, outcome },
        requestId: null,
      })
    } catch {
      // Already settled, or the vault went away: either way nothing to do.
    }
  }

  async function dismiss() {
    asking = null
    reset()
    await invoke('writing_dismiss', { assistId: null }).catch(() => {})
  }

  async function pick(choice: Choice) {
    if (!selection || view.kind !== 'menu') return
    const seq = selection.seq
    asking = seq
    view = { kind: 'working', choice }
    let assist: WritingAssist
    try {
      assist = await invoke<WritingAssist>('call', {
        name: 'rewrite_selection',
        args: { text: selection.text, action: choice.action, app: selection.app ?? null },
        requestId: null,
      })
    } catch (raw) {
      if (asking !== seq) return
      const e = raw as { code?: string; message?: string }
      view = { kind: 'trouble', trouble: trouble(String(e?.code ?? ''), String(e?.message ?? raw)) }
      return
    }
    if (asking !== seq) {
      // Dismissed while the model was thinking: the answer is not wanted.
      await settle(assist.id, 'cancelled')
      return
    }
    asking = null
    applying = true
    const applied = await invoke<{ outcome: string; message?: string | null }>('writing_apply', {
      assistId: assist.id,
      text: assist.result ?? '',
    }).catch((e) => ({
      outcome: 'copied',
      message: String((e as { message?: string })?.message ?? e),
    }))
    applying = false
    if (applied.outcome === 'replaced' || applied.outcome === 'cancelled') reset()
    else view = { kind: 'copied', message: applied.message ?? '' }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault()
      void dismiss()
      return
    }
    if (view.kind === 'menu' && !e.ctrlKey && !e.altKey && !e.metaKey) {
      const choice = choiceForKey(e.key)
      if (choice) {
        e.preventDefault()
        void pick(choice)
      }
    }
  }

  /** A click on the overlay outside the card is a click outside the popup. */
  function onBackdrop(e: MouseEvent) {
    // With no card at all -- nothing to show yet, or the shell failed
    // half-way -- the whole overlay is outside it.
    if (!card || !card.contains(e.target as Node)) void dismiss()
  }

  onMount(() => {
    const unlisten = [
      listen<Selection>('everyday://writing/open', (e) => show(e.payload)),
      listen('everyday://writing/close', () => reset()),
    ]
    // A popup that has only just loaded missed the event that made it.
    void invoke<Selection | null>('writing_selection').then((s) => {
      if (s && (!selection || s.seq > selection.seq)) show(s)
    })
    // Losing the keyboard is going somewhere else, overlay or window. While
    // the model is working that matters most: its answer would be pasted
    // into wherever the keyboard went, so the rewrite is abandoned and its
    // answer, when it comes, is recorded as cancelled.
    const onBlur = () => {
      if (!applying && (view.kind === 'menu' || view.kind === 'working')) void dismiss()
    }
    window.addEventListener('blur', onBlur)
    window.addEventListener('resize', reposition)
    return () => {
      for (const u of unlisten) void u.then((f) => f())
      window.removeEventListener('blur', onBlur)
      window.removeEventListener('resize', reposition)
    }
  })
</script>

<svelte:window onkeydown={onKey} />

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="stage" class:overlay class:windowed={!overlay} onmousedown={onBackdrop}>
  {#if selection && view.kind !== 'idle'}
    <div
      class="card"
      bind:this={card}
      role="dialog"
      aria-label="Writing help"
      style:left={position ? `${position.left}px` : null}
      style:top={position ? `${position.top}px` : null}
      style:visibility={overlay && !position ? 'hidden' : null}
    >
      <header>
        <span class="eyebrow">Writing help</span>
        {#if selection.text}
          <p class="quote" title={selection.text}>{preview(selection.text)}</p>
        {/if}
      </header>

      {#if view.kind === 'menu'}
        <div class="choices">
          {#each CHOICES as choice, i (choice.action)}
            <button class="choice" bind:this={buttons[i]} onclick={() => pick(choice)}>
              <span>{choice.label}</span><kbd>{choice.key}</kbd>
            </button>
          {/each}
        </div>
      {:else if view.kind === 'working'}
        <p class="status" role="status"><span class="spinner"></span>{view.choice.doing}…</p>
      {:else if view.kind === 'trouble'}
        <p class="status trouble" role="alert">{view.trouble.message}</p>
        <div class="actions">
          {#if view.trouble.openMain}
            <button class="btn" onclick={() => invoke('writing_open_main')}>Open Every Day</button>
          {/if}
          <button class="btn quiet" onclick={dismiss}>Close</button>
        </div>
      {:else if view.kind === 'copied'}
        <p class="status" role="status">
          Copied instead of pasted{view.message ? ` — ${view.message}` : ''}. Paste it with {pasteKey}.
        </p>
        <div class="actions"><button class="btn quiet" onclick={dismiss}>Close</button></div>
      {/if}

      <footer>Esc to close</footer>
    </div>
  {/if}
</div>

<style>
  :global(html),
  :global(body) {
    margin: 0;
    height: 100%;
    background: transparent;
    overflow: hidden;
    font-family: var(--font-ui);
    color: var(--fg);
  }
  :global(#writing) {
    height: 100%;
  }
  .stage {
    position: fixed;
    inset: 0;
  }
  .card {
    box-sizing: border-box;
    width: 300px;
    padding: 12px;
    border-radius: var(--radius-lg);
    background: var(--bg-raised);
    border: 1px solid var(--border);
    box-shadow: var(--shadow-lg);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .overlay .card {
    position: absolute;
  }
  /* As a window of its own, the window is the card's size already. */
  .windowed .card {
    width: 100%;
    height: 100%;
    box-shadow: none;
  }
  header {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .eyebrow {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--fg-subtle);
  }
  .quote {
    margin: 0;
    font-size: 12.5px;
    color: var(--fg-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .choices {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .choice {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 10px;
    border: 0;
    border-radius: var(--radius);
    background: transparent;
    color: var(--fg);
    font: inherit;
    font-size: 14px;
    text-align: left;
    cursor: pointer;
  }
  .choice:hover,
  .choice:focus-visible {
    background: var(--bg-selected);
    outline: none;
  }
  kbd {
    font: inherit;
    font-size: 11px;
    color: var(--fg-subtle);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 0 5px;
  }
  .status {
    margin: 4px 2px;
    font-size: 13.5px;
    line-height: 1.4;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .trouble {
    color: var(--danger);
  }
  .spinner {
    width: 14px;
    height: 14px;
    flex: none;
    border-radius: 50%;
    border: 2px solid var(--border);
    border-top-color: var(--accent);
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(1turn);
    }
  }
  .actions {
    display: flex;
    gap: 6px;
    justify-content: flex-end;
  }
  .btn {
    font: inherit;
    font-size: 13px;
    padding: 5px 12px;
    border-radius: 999px;
    border: 1px solid transparent;
    background: var(--accent);
    color: var(--fg-on-accent);
    cursor: pointer;
  }
  .btn.quiet {
    background: transparent;
    border-color: var(--border);
    color: var(--fg);
  }
  footer {
    font-size: 11px;
    color: var(--fg-faint);
    text-align: right;
  }
</style>
