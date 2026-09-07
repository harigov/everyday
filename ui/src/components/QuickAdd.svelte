<script lang="ts">
  // One line, one Enter, one task -- and the cursor is still here for the
  // next one.
  //
  // Everything about this component is in service of adding twelve things in
  // a row without touching the mouse: the field never loses focus on submit,
  // the parsed fields are echoed back so you can see the shorthand landed,
  // and Escape gets you out of an input you opened by accident.

  import { todo } from '../lib/todo.svelte'
  import { QUICK_ADD_HINT, parseQuickAdd } from '../lib/quickadd'
  import { friendlyDate, formatClock, formatMinutes } from '../lib/format'
  import { focusOnMount } from '../lib/focus'
  import Icon from './Icon.svelte'
  import type { TaskId, TaskStatus } from '../lib/types'

  let {
    parentId = undefined,
    status = undefined,
    placeholder = 'Add a task',
    autofocus = false,
    compact = false,
    onclose = undefined,
  }: {
    /** Set to add subtasks of this task instead of top-level ones. */
    parentId?: TaskId
    /** The column being added to, on a board. */
    status?: TaskStatus
    placeholder?: string
    autofocus?: boolean
    /** Smaller, for use inside a board column or under a task. */
    compact?: boolean
    /** Called on Escape, so a caller can put its "+" button back. */
    onclose?: () => void
  } = $props()

  let value = $state('')
  let field = $state<HTMLInputElement | null>(null)

  /**
   * Writes in flight, chained.
   *
   * The obvious spelling -- a `busy` flag that ignores Enter while a save is
   * running -- silently eats the second task of anyone typing faster than
   * the disk, which is the exact person this input is for. So submissions
   * queue instead: the field is cleared synchronously and the write joins
   * the back of the chain. Serialising them also keeps `sortOrder`
   * increasing, since each add sees the one before it.
   */
  let chain: Promise<unknown> = Promise.resolve()

  // Echoed back under the field, so `!hi ~2h @fri` is visibly understood
  // before Enter rather than after it.
  const parsed = $derived(parseQuickAdd(value))
  const showing = $derived(
    value.trim().length > 0 &&
      (parsed.tags.length > 0 ||
        parsed.priority !== 'none' ||
        parsed.estimateMinutes !== null ||
        parsed.dueDate !== null ||
        parsed.status !== null),
  )

  function submit() {
    const line = value
    if (!line.trim()) return
    // Cleared synchronously, before anything is awaited: at typing speed the
    // round trip is long enough to type into, and clearing late eats the
    // next task.
    value = ''
    chain = chain
      .then(() => todo.add(line, { parentId, status }))
      .catch(() => {})
      .finally(() => field?.focus())
  }

  export function focus() {
    field?.focus()
  }
</script>

<div class="quickadd" class:compact>
  <span class="mark" aria-hidden="true"><Icon name="plus" size={compact ? 14 : 16} /></span>
  <input
    bind:this={field}
    class="field"
    type="text"
    {placeholder}
    autocomplete="off"
    spellcheck="false"
    bind:value
    use:focusOnMount={autofocus}
    onkeydown={(e) => {
      if (e.key === 'Enter') { e.preventDefault(); submit() }
      if (e.key === 'Escape') {
        e.preventDefault()
        if (value) value = ''
        else onclose?.()
      }
    }}
  />
</div>

{#if showing}
  <div class="parsed" aria-live="polite">
    {#if parsed.dueDate}
      <span class="bit"><Icon name="calendar" size={11} />
        {friendlyDate(parsed.dueDate)}{parsed.dueTime ? ` ${formatClock(parsed.dueTime)}` : ''}
      </span>
    {/if}
    {#if parsed.priority !== 'none'}
      <span class="bit p-{parsed.priority}"><Icon name="flag" size={11} /> {parsed.priority}</span>
    {/if}
    {#if parsed.estimateMinutes}
      <span class="bit"><Icon name="clock" size={11} /> {formatMinutes(parsed.estimateMinutes)}</span>
    {/if}
    {#if parsed.status}
      <span class="bit"><Icon name="board" size={11} /> {parsed.status}</span>
    {/if}
    {#each parsed.tags as tag (tag)}
      <span class="bit">#{tag}</span>
    {/each}
  </div>
{:else if !compact}
  <p class="hint">{QUICK_ADD_HINT}</p>
{/if}

<style>
  .quickadd {
    position: relative;
    display: flex;
    align-items: center;
    flex: none;
  }
  .mark {
    position: absolute;
    left: var(--sp-3);
    display: flex;
    color: var(--fg-faint);
    pointer-events: none;
    transition: color var(--fast) var(--ease);
  }
  .quickadd:focus-within .mark { color: var(--accent); }

  .field {
    width: 100%;
    height: 36px;
    padding: 0 var(--sp-3) 0 34px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-raised);
    color: var(--fg);
    font-size: var(--text-md);
    user-select: text;
    transition: border-color var(--fast) var(--ease), box-shadow var(--fast) var(--ease);
  }
  .field::placeholder { color: var(--fg-faint); }
  .field:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in oklab, var(--accent) 16%, transparent);
  }

  .compact .field {
    height: 30px;
    padding-left: 30px;
    font-size: var(--text-base);
    background: var(--bg-panel);
  }
  .compact .mark { left: var(--sp-2); }

  .hint {
    margin-top: 5px;
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-family: var(--font-mono);
    letter-spacing: -0.01em;
  }

  .parsed {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-1);
    margin-top: 5px;
  }
  .bit {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 1px var(--sp-2);
    border-radius: 99px;
    background: var(--bg-active);
    font-size: 10px;
    font-weight: 550;
    color: var(--fg-muted);
  }
  .bit.p-high, .bit.p-urgent { color: var(--danger); }
</style>
