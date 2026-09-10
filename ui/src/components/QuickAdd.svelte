<script lang="ts">
  // One line, one Enter, one task -- and the cursor is still here for the
  // next one.
  //
  // Everything about this component is in service of adding twelve things in
  // a row without touching the mouse: the field never loses focus on submit,
  // the parsed fields are echoed back so you can see the shorthand landed,
  // and Escape gets you out of an input you opened by accident.

  import { api } from '../lib/api'
  import { purpose as purposeStore } from '../lib/purpose.svelte'
  import { quick, slot } from '../lib/quick.svelte'
  import { todo } from '../lib/todo.svelte'
  import { QUICK_ADD_HINT, parseQuickAdd } from '../lib/quickadd'
  import { friendlyDate, formatClock, formatMinutes } from '../lib/format'
  import { focusOnMount } from '../lib/focus'
  import Icon from './Icon.svelte'
  import type { Task, TaskId, TaskStatus } from '../lib/types'
  import Suggestions from './Suggestions.svelte'

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

  // ── what the quick model noticed ─────────────────────────────────────
  //
  // Two jobs, and both of them arrive *after* the task exists. The grammar
  // above is deterministic, offline, instant and tested, and for the lines it
  // handles it is better than a model -- so the model is only ever asked
  // about the residue: a line that parsed to no fields at all and still reads
  // like it had a date in it, and the purpose the grammar has no syntax for.
  //
  // Nothing here is allowed to delay the Enter. `submit` clears the field and
  // queues the write exactly as it did; these hang off the end of that chain.

  let chips = $state<{ key: string; label: string }[]>([])
  let pending = $state<{ task: Task; patch: Partial<Task> } | null>(null)
  const suggestSlot = slot<null>()

  /**
   * Is there anything left for a model to find?
   *
   * False when the grammar already understood the line, which is the common
   * case and the one that must cost nothing. `showing` is the same predicate
   * the echo row under the field uses, so the rule is: if the shorthand is
   * visibly working, the model is not asked.
   */
  function hasResidue(line: string): boolean {
    if (showing) return false
    // A bare noun phrase -- "milk", "ring the vet" -- has no residue either.
    // Three words and a hint of time is the cheapest filter that separates
    // "book the flights sometime next week" from "milk".
    return /\b(today|tomorrow|tonight|next|this|before|after|by|on|at|in)\b/i.test(line)
  }

  function submit() {
    const line = value
    if (!line.trim()) return
    // Cleared synchronously, before anything is awaited: at typing speed the
    // round trip is long enough to type into, and clearing late eats the
    // next task.
    const residue = hasResidue(line)
    value = ''
    // Whatever was suggested about the *previous* task goes now. Twelve tasks
    // in a row is what this box is for, and a chip offering a due date for
    // the one before is one tap from filing it on the wrong task.
    suggestSlot.cancel()
    chips = []
    pending = null
    chain = chain
      .then(() => todo.add(line, { parentId, status }))
      .then((task) => {
        if (task) void suggest(task, line, residue)
      })
      .catch(() => {})
      .finally(() => field?.focus())
  }

  /**
   * Ask about a task that has already been written.
   *
   * Both answers become chips against the *same* task, so adding three tasks
   * quickly leaves the suggestions for the last one -- which is the one still
   * on screen. The slot's generation counter is what makes that true rather
   * than a race.
   */
  async function suggest(task: Task, line: string, residue: boolean) {
    const found: { key: string; label: string }[] = []
    const patch: Partial<Task> = {}

    // Through the slot, so an answer about a task two Enters ago is dropped
    // rather than drawn. Without it the round trip outlives the task it was
    // about, which at typing speed is the common case rather than the edge.
    await suggestSlot.track(
      async () => {
        await gather(task, line, residue, found, patch)
        return null
      },
      () => {
        if (found.length === 0) return
        pending = { task, patch }
        chips = found
      },
    )
  }

  async function gather(
    task: Task,
    line: string,
    residue: boolean,
    found: { key: string; label: string }[],
    patch: Partial<Task>,
  ) {
    const [draft, labels] = await Promise.all([
      residue && quick.enabled('todo.parse')
        ? api.quickTaskFromLine(line).catch(() => null)
        : Promise.resolve(null),
      quick.enabled('todo.purpose')
        ? api.quickTaskLabels(task.title).catch(() => null)
        : Promise.resolve(null),
    ])

    if (draft?.dueDate && !task.dueDate) {
      patch.dueDate = draft.dueDate
      patch.dueTime = draft.dueTime
      found.push({
        key: 'due',
        label:
          friendlyDate(draft.dueDate) + (draft.dueTime ? ` ${formatClock(draft.dueTime)}` : ''),
      })
    }
    if (draft?.estimateMinutes && task.estimateMinutes == null) {
      patch.estimateMinutes = draft.estimateMinutes
      found.push({ key: 'estimate', label: formatMinutes(draft.estimateMinutes) })
    }
    if (labels?.purpose && !task.purpose) {
      patch.purpose = labels.purpose
      found.push({ key: 'purpose', label: purposeStore.describe(labels.purpose).name })
    }
  }

  /** Apply one chip. The write is the ordinary one; nothing here is special. */
  function accept(key: string) {
    const held = pending
    if (!held) return
    const { task, patch } = held
    if (key === 'due') {
      task.dueDate = patch.dueDate ?? null
      task.dueTime = patch.dueTime ?? null
    } else if (key === 'estimate') {
      task.estimateMinutes = patch.estimateMinutes ?? null
    } else if (key === 'purpose') {
      task.purpose = patch.purpose ?? null
    }
    todo.patch(task.id, {
      dueDate: task.dueDate,
      dueTime: task.dueTime,
      estimateMinutes: task.estimateMinutes,
      purpose: task.purpose,
    })
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
      if (e.key === 'Enter') {
        e.preventDefault()
        submit()
      }
      if (e.key === 'Escape') {
        e.preventDefault()
        if (value) value = ''
        else onclose?.()
      }
    }}
  />
</div>

<!-- After the task exists, never before. A suggestion that delayed the Enter
     would have broken the one thing this component is for. -->
<Suggestions
  items={chips}
  label="Also:"
  onaccept={accept}
  ondismiss={() => suggestSlot.dismiss(() => (chips = []))}
/>

{#if showing}
  <div class="parsed" aria-live="polite">
    {#if parsed.dueDate}
      <span class="bit"
        ><Icon name="calendar" size={11} />
        {friendlyDate(parsed.dueDate)}{parsed.dueTime ? ` ${formatClock(parsed.dueTime)}` : ''}
      </span>
    {/if}
    {#if parsed.priority !== 'none'}
      <span class="bit p-{parsed.priority}"><Icon name="flag" size={11} /> {parsed.priority}</span>
    {/if}
    {#if parsed.estimateMinutes}
      <span class="bit"
        ><Icon name="clock" size={11} /> {formatMinutes(parsed.estimateMinutes)}</span
      >
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
  .quickadd:focus-within .mark {
    color: var(--accent);
  }

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
    transition:
      border-color var(--fast) var(--ease),
      box-shadow var(--fast) var(--ease);
  }
  .field::placeholder {
    color: var(--fg-faint);
  }
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
  .compact .mark {
    left: var(--sp-2);
  }

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
    font-size: 11px;
    font-weight: 550;
    color: var(--fg-muted);
  }
  .bit.p-high,
  .bit.p-urgent {
    color: var(--danger);
  }
</style>
