<script lang="ts">
  // A box to type in on a card: a quick reply to a message, a note or a
  // question during a recording, an answer the assistant asked for, words for
  // the assistant. Clicking into it, or on a chip that fills it, is the one
  // time the panel takes the keyboard; Enter sends (Shift+Enter is a new line where the box takes
  // several), Escape gives the keyboard back. Ready-made texts sit above it as
  // chips that fill it, to send as they are or change first; where the box can
  // be used more than one way -- a note, or a question -- the ways are a small
  // segmented control over it. A chip puts the caret in the box, so it is
  // marked `data-keyboard`: the stack asks for the keyboard from the press on
  // it, as from a press in the box (`wantsKeyboard` in `PanelStack.svelte`).
  //
  // What is typed is kept by the stack, not here, so a card the shell re-sends
  // while somebody types does not lose a word, and a send that fails can be
  // put back (`sentAfter` in `cards.ts`). The box grows with what is in it to
  // a few lines, then scrolls inside itself.
  import { tick } from 'svelte'
  import Glyph from '../Glyph.svelte'
  import { cleanText, inputMode, keyAction } from '../cards'
  import type { PanelBlock } from '../panel'

  let {
    block,
    draft,
    picked,
    locked,
    ondraft,
    onmode,
    onsubmit,
  }: {
    block: Extract<PanelBlock, { type: 'input' }>
    /** What is in the box. */
    draft: string
    /** The mode picked above it, if one has been. */
    picked: string | undefined
    /** Sending is off: the shell is busy with the last one, or has not yet answered it. */
    locked: boolean
    ondraft: (text: string) => void
    onmode: (mode: string) => void
    onsubmit: (text: string) => void
  } = $props()

  /** About six lines of the box's text, and then it scrolls. */
  const TALLEST = 132

  const uid = $props.id()
  let box = $state<HTMLTextAreaElement | null>(null)

  const mode = $derived(inputMode(block, picked))
  const modes = $derived(block.modes ?? [])
  const suggestions = $derived(block.suggestions ?? [])
  const ready = $derived(draft.trim() !== '' && !locked)

  /** What the box was last sized for. */
  let fitted: { box: HTMLTextAreaElement | null; draft: string; width: number } = {
    box: null,
    draft: '',
    width: 0,
  }

  // Grow with what is in the box. Read after the box has its new text. Text
  // added at the end can only add lines, so the box is let go to measure its
  // own height only when something was taken out or changed, or the box is
  // new or another width -- not on every key.
  $effect(() => {
    const text = draft
    const el = box
    if (!el) return
    const width = el.clientWidth
    const was = fitted
    fitted = { box: el, draft: text, width }
    if (el !== was.box || width !== was.width || !text.startsWith(was.draft)) {
      el.style.height = 'auto'
    }
    const height = `${Math.min(TALLEST, el.scrollHeight)}px`
    if (el.style.height !== height) el.style.height = height
  })

  function keydown(event: KeyboardEvent) {
    const what = keyAction(event, block.multiline)
    if (what === 'send') {
      event.preventDefault()
      send()
    } else if (what === 'blur') {
      event.preventDefault()
      box?.blur()
    }
  }

  function send() {
    const text = draft.trim()
    if (text === '' || locked) return
    onsubmit(text)
    // Sent: the keyboard goes back to whatever had it (the stack hears the
    // box lose it, and tells the shell).
    box?.blur()
  }

  /** A ready-made text into the box, with the caret at its end. */
  function fill(text: string) {
    ondraft(cleanText(text, block.multiline))
    void tick().then(() => {
      if (!box) return
      box.focus()
      box.setSelectionRange(box.value.length, box.value.length)
    })
  }

  /** A press on a chip or the send button leaves the keyboard in the box. */
  function keep(event: MouseEvent) {
    event.preventDefault()
  }
</script>

<div class="input" class:busy={block.busy}>
  {#if modes.length > 1}
    <div class="modes" role="radiogroup" aria-label="What the box is for">
      {#each modes as option (option.id)}
        <button
          type="button"
          role="radio"
          class="mode"
          class:on={option.id === mode.mode}
          aria-checked={option.id === mode.mode}
          onmousedown={keep}
          onclick={() => onmode(option.id)}
        >
          {option.label}
        </button>
      {/each}
    </div>
  {/if}
  {#if suggestions.length > 0}
    <div class="suggest" role="group" aria-label="Ready-made replies">
      {#each suggestions as suggestion, i (i)}
        <button
          type="button"
          class="pc-chip"
          title={suggestion.text}
          data-keyboard
          onmousedown={keep}
          onclick={() => fill(suggestion.text)}
        >
          {suggestion.label}
        </button>
      {/each}
    </div>
  {/if}
  <div class="field">
    <textarea
      bind:this={box}
      bind:value={() => draft, (text: string) => ondraft(cleanText(text, block.multiline))}
      rows="1"
      placeholder={mode.placeholder}
      aria-label={mode.placeholder}
      aria-describedby={block.error ? `${uid}-error` : undefined}
      aria-busy={block.busy}
      onkeydown={keydown}
    ></textarea>
    <button
      type="button"
      class="send"
      class:ready
      disabled={!ready}
      onmousedown={keep}
      onclick={send}
    >
      {#if block.busy}
        <span class="pc-spinner" aria-hidden="true"></span>
      {:else}
        <Glyph name="send" size={14} weight={2} />
      {/if}
      <span>{mode.submit}</span>
    </button>
  </div>
  {#if block.error}
    <p class="error" id="{uid}-error" role="alert">
      <Glyph name="alert" size={13} weight={2} />
      <span>{block.error}</span>
    </p>
  {/if}
</div>

<style>
  .input {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }

  /* The ways to use the box: the main window's segmented control, a size
     down. */
  .modes {
    align-self: flex-start;
    display: inline-flex;
    gap: 2px;
    padding: 2px;
    border-radius: var(--radius);
    background: var(--bg-sunken);
  }

  .mode {
    height: 24px;
    padding: 0 10px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--fg-subtle);
    font-size: var(--text-xs);
    font-weight: 600;
    cursor: pointer;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }

  .mode:hover {
    color: var(--fg);
  }

  .mode.on {
    background: var(--bg-raised);
    color: var(--fg);
    box-shadow: var(--shadow-sm);
  }

  .suggest {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .suggest .pc-chip {
    min-height: 28px;
    padding: 3px 11px;
    font-size: var(--text-xs);
  }

  .field {
    display: flex;
    align-items: flex-end;
    gap: var(--sp-2);
    padding: 4px 4px 4px 12px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-lg);
    background: var(--bg-raised);
    transition:
      border-color var(--fast) var(--ease),
      box-shadow var(--fast) var(--ease);
  }

  .field:focus-within {
    border-color: color-mix(in oklab, var(--accent) 70%, var(--border-strong));
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 18%, transparent);
  }

  textarea {
    flex: 1;
    min-width: 0;
    max-height: 132px;
    margin: 0;
    padding: 5px 0;
    border: 0;
    outline: none;
    background: transparent;
    resize: none;
    overflow-y: auto;
    overscroll-behavior: contain;
    scrollbar-width: thin;
    font-size: var(--text-base);
    line-height: var(--leading-snug);
    color: var(--fg);
    -webkit-user-select: text;
    user-select: text;
  }

  textarea::placeholder {
    color: var(--fg-subtle);
    opacity: 1;
  }

  .busy textarea {
    opacity: 0.7;
  }

  .send {
    flex: none;
    height: 30px;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 0 12px 0 10px;
    border: 0;
    border-radius: var(--radius-pill);
    background: transparent;
    color: var(--fg-faint);
    font-size: var(--text-sm);
    font-weight: 600;
    cursor: default;
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }

  .send.ready {
    background: var(--accent);
    color: var(--fg-on-accent);
    box-shadow: var(--shadow-sm);
    cursor: pointer;
  }

  .send.ready:hover {
    background: var(--accent-hover);
  }

  .busy .send {
    color: var(--fg-muted);
  }

  .error {
    display: flex;
    align-items: flex-start;
    gap: 6px;
    font-size: var(--text-xs);
    line-height: var(--leading-snug);
    color: var(--danger);
  }

  .error :global(svg) {
    margin-top: 1px;
  }
</style>
