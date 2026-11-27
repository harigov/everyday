<script lang="ts">
  // One meeting's brief, drawn the same wherever it is shown: the calendar's
  // rail today, and anywhere else that wants the whole of it.
  //
  // What to know walking in, in the order it is wanted: where things stand,
  // what is open between you and the people coming (what you owe and are
  // owed), the invitation's agenda, who is coming and who they are, the things
  // you meant to raise with them, and the background. Every part is left out when the
  // brief has nothing for it, so a sparse brief is short rather than a
  // column of empty headings. What it was written from, and what could not be
  // looked at, are last and quiet -- the point is to be able to trust the
  // rest, not to read this.
  //
  // The only thing it does is tick a talking point off. That is the task's
  // own completion, written the way every other tick in the app is (so the
  // list, the board and the card in the corner all agree), and the view is
  // handed back changed rather than edited in place: whoever holds the brief
  // owns it.

  import { api } from '../lib/api'
  import { errorMessage } from '../lib/errors'
  import { plural, relativeTime } from '../lib/format'
  import { todo } from '../lib/todo.svelte'
  import type { MeetingBrief, TalkingPoint, TaskId } from '../lib/types'
  import Icon from './Icon.svelte'

  let {
    brief,
    onchange,
  }: {
    brief: MeetingBrief
    /** The brief with a talking point's tick changed, once it is written. */
    onchange?: (next: MeetingBrief) => void
  } = $props()

  /** Points whose tick is being written, so a second press waits. */
  let writing = $state<TaskId[]>([])
  let error = $state<string | null>(null)

  /** The brief with one point's tick set, handed to whoever holds it. */
  function setDone(taskId: TaskId, done: boolean) {
    onchange?.({
      ...brief,
      talkingPoints: brief.talkingPoints.map((p) => (p.taskId === taskId ? { ...p, done } : p)),
    })
  }

  async function tick(point: TalkingPoint) {
    if (writing.includes(point.taskId)) return
    writing = [...writing, point.taskId]
    error = null
    // Drawn ticked at once, and put back if the write does not go: the box
    // should not wait on a read and a commit.
    setDone(point.taskId, !point.done)
    try {
      // The task as it is now, not as the brief remembers it: the direction
      // of the toggle is read from it.
      const written = await todo.toggleDoneOf(await api.task(point.taskId))
      if (!written) throw new Error('That could not be saved. Try again.')
      // What was written, which is the other way only when the task had
      // been ticked somewhere else since the brief was made.
      setDone(point.taskId, written.status === 'done')
    } catch (e) {
      setDone(point.taskId, point.done)
      error = errorMessage(e)
    } finally {
      writing = writing.filter((id) => id !== point.taskId)
    }
  }

  /** What the brief was made from, in a sentence: "the invite, 2 mail threads and a web search". */
  const madeFrom = $derived.by(() => {
    const count = (kind: string) => brief.sources.filter((s) => s.kind === kind).length
    const parts: string[] = []
    if (count('invite')) parts.push('the invite')
    if (count('contact')) parts.push(plural(count('contact'), 'contact card'))
    if (count('mail')) parts.push(plural(count('mail'), 'mail thread'))
    if (count('note')) parts.push(plural(count('note'), 'note'))
    if (count('meeting')) {
      parts.push(count('meeting') === 1 ? 'the last meeting' : plural(count('meeting'), 'meeting'))
    }
    if (count('task')) parts.push(plural(count('task'), 'task'))
    if (count('web')) {
      parts.push(
        count('web') === 1 ? 'a web search' : plural(count('web'), 'web search', 'web searches'),
      )
    }
    return new Intl.ListFormat(undefined, { style: 'long', type: 'conjunction' }).format(parts)
  })

  /** The line under the brief: what it was made from, and when. */
  const provenance = $derived.by(() => {
    if (!madeFrom) return ''
    const verb = brief.written ? 'Written' : 'Put together'
    return `${verb} from ${madeFrom}, ${relativeTime(brief.preparedAt)}.`
  })
</script>

<div class="brief">
  {#if brief.summary}<p class="summary">{brief.summary}</p>{/if}

  {#if brief.open.length > 0}
    <section>
      <span class="eyebrow">Open</span>
      <ul class="lines">
        {#each brief.open as item, i (i)}<li>{item}</li>{/each}
      </ul>
    </section>
  {/if}

  {#if brief.agenda.length > 0}
    <section>
      <span class="eyebrow">Agenda</span>
      <ul class="lines">
        {#each brief.agenda as item, i (i)}<li>{item}</li>{/each}
      </ul>
    </section>
  {/if}

  {#if brief.people.length > 0}
    <section>
      <span class="eyebrow">With</span>
      <ul class="people">
        {#each brief.people as person, i (person.email ?? person.name + i)}
          <li title={person.email}>
            <b>{person.name}</b>
            <span>{person.line}</span>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if brief.talkingPoints.length > 0}
    <section>
      <span class="eyebrow">To raise</span>
      <ul class="points">
        {#each brief.talkingPoints as point (point.taskId)}
          <li class:done={point.done}>
            <button
              class="tick"
              class:on={point.done}
              disabled={writing.includes(point.taskId)}
              title={point.done ? 'Mark as not done' : 'Mark as done'}
              aria-label={point.done ? 'Mark as not done' : 'Mark as done'}
              aria-pressed={point.done}
              onclick={() => void tick(point)}
            >
              <Icon name={point.done ? 'check' : 'circle'} size={16} weight={1.6} />
            </button>
            <span class="what">
              <span class="text">{point.text}</span>
              <small>with {point.with}</small>
            </span>
          </li>
        {/each}
      </ul>
      {#if error}<p class="error">{error}</p>{/if}
    </section>
  {/if}

  {#if brief.context.length > 0}
    <section>
      <span class="eyebrow">Context</span>
      <ul class="lines">
        {#each brief.context as line, i (i)}<li>{line}</li>{/each}
      </ul>
    </section>
  {/if}

  {#if provenance || brief.gaps.length > 0}
    <footer>
      {#if provenance}
        <p class="hint" title={brief.sources.map((s) => s.title).join('\n')}>{provenance}</p>
      {/if}
      {#each brief.gaps as gap, i (i)}<p class="hint gap">{gap}</p>{/each}
    </footer>
  {/if}
</div>

<style>
  .brief {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    min-width: 0;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
  }

  .summary {
    font-size: var(--text-base);
    line-height: var(--leading-normal);
    color: var(--fg);
    user-select: text;
  }

  ul {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    margin: 0;
    padding: 0;
    list-style: none;
    min-width: 0;
    user-select: text;
  }
  .lines li {
    position: relative;
    padding-left: var(--sp-4);
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
    color: var(--fg-muted);
  }
  .lines li::before {
    content: '';
    position: absolute;
    left: 3px;
    top: 0.62em;
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: var(--fg-faint);
  }

  .people li {
    display: flex;
    flex-direction: column;
    gap: 1px;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }
  .people b {
    font-weight: 600;
    color: var(--fg);
  }
  .people span {
    color: var(--fg-muted);
  }

  .points li {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-2);
  }
  /* The same circle that ticks a task in the list: it is the same act. */
  .tick {
    display: flex;
    flex: none;
    margin-top: 1px;
    color: var(--fg-faint);
    transition:
      color var(--fast) var(--ease),
      scale var(--fast) var(--ease);
  }
  .tick:hover {
    color: var(--accent);
    scale: 1.08;
  }
  .tick.on {
    color: var(--accent);
  }
  .tick:disabled {
    opacity: 0.5;
    pointer-events: none;
  }
  .what {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
    font-size: var(--text-sm);
    line-height: var(--leading-normal);
  }
  .text {
    color: var(--fg);
  }
  .what small {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .points li.done .text {
    color: var(--fg-subtle);
    text-decoration: line-through;
  }

  footer {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .hint {
    font-size: var(--text-xs);
    color: var(--fg-faint);
  }
  .hint.gap {
    font-style: italic;
  }

  /* A phone: the tick's circle has a hit area a finger can land on, taken
     out of the row's own padding so that the list does not grow. */
  :global([data-phone]) .tick {
    align-items: center;
    justify-content: center;
    width: 40px;
    height: 40px;
    margin: -10px -8px -10px -10px;
  }
</style>
