<script lang="ts">
  // The list view: a task tree, cut into sections.
  //
  // Grouping is a property of the view rather than of the query, so the same
  // loaded set can be re-cut without another round trip -- which is what
  // makes flipping between "by due date" and "by priority" instant.

  import { FILTER_LABELS, TASK_FILTERS, todo, type GroupBy } from '../lib/todo.svelte'
  import { friendlyDate, plural } from '../lib/format'
  import { isoDate, todayIso } from '../lib/time'
  import { menu } from '../lib/menu.svelte'
  import { purpose } from '../lib/purpose.svelte'
  import { SEP, tidyMenu, type MenuItem } from '../lib/menu'
  import EmptyState from './EmptyState.svelte'
  import TaskRow from './TaskRow.svelte'
  import { rovingFocus } from '../lib/roving'
  import type { TaskNode } from '../lib/todo.svelte'
  import { priorityRank } from '../lib/types'
  import type { Task } from '../lib/types'
  import { STATUS_LABELS, PRIORITY_LABELS } from '../lib/labels'

  /**
   * The bucket a deadline falls into.
   *
   * Overdue is first and separate: something that slipped is a different
   * kind of problem from something that is merely soon, and folding the two
   * together is how a list stops being looked at.
   */
  function dueBucket(task: Task): { key: string; label: string; order: number } {
    const today = todayIso()
    if (!task.dueDate) return { key: 'none', label: 'No date', order: 9 }
    if (task.dueDate < today) return { key: 'overdue', label: 'Overdue', order: 0 }
    if (task.dueDate === today) return { key: 'today', label: 'Today', order: 1 }
    // Within a week, the weekday alone is what people navigate by; beyond
    // it, the date. `friendlyDate` already draws that line.
    const week = new Date()
    week.setDate(week.getDate() + 7)
    const horizon = isoDate(week)
    if (task.dueDate <= horizon) {
      return { key: task.dueDate, label: friendlyDate(task.dueDate), order: 2 }
    }
    return { key: 'later', label: 'Later', order: 8 }
  }

  interface Group {
    key: string
    label: string
    order: number
    nodes: TaskNode[]
  }

  const groups = $derived.by((): Group[] => {
    const roots = todo.tree
    if (todo.groupBy === 'none') {
      return [{ key: 'all', label: '', order: 0, nodes: roots }]
    }

    const out = new Map<string, Group>()
    for (const node of roots) {
      let bucket: { key: string; label: string; order: number }
      if (todo.groupBy === 'due') {
        bucket = dueBucket(node.task)
      } else if (todo.groupBy === 'purpose') {
        // The resolved purpose, not the task's own: a task under a filed
        // project belongs in that project's section, which is the whole
        // point of inheritance. Unfiled work sorts last rather than first —
        // there is usually a lot of it, and it is not the answer anyone
        // opened this grouping to see.
        const resolved = node.task.purpose ?? todo.projectOf(node.task.projectId)?.purpose ?? null
        const label = purpose.describe(resolved)
        bucket = resolved
          ? { key: `${resolved.type}:${resolved.id}`, label: label.name, order: 0 }
          : { key: 'none', label: 'Not filed', order: 9 }
      } else if (todo.groupBy === 'status') {
        const key = node.task.status
        bucket = {
          key,
          label: STATUS_LABELS[key] ?? key,
          order: ['backlog', 'todo', 'doing', 'blocked', 'done', 'cancelled'].indexOf(key),
        }
      } else {
        const key = node.task.priority
        bucket = {
          key,
          label: PRIORITY_LABELS[key] ?? key,
          // Most important first, and "unprioritised" at the bottom rather
          // than at the top where `none` would otherwise sort.
          order: key === 'none' ? 9 : 4 - priorityRank(key),
        }
      }
      const group = out.get(bucket.key) ?? { ...bucket, nodes: [] }
      group.nodes.push(node)
      out.set(bucket.key, group)
    }
    return [...out.values()].sort((a, b) => a.order - b.order || a.key.localeCompare(b.key))
  })

  const GROUPS = $derived.by((): { id: GroupBy; label: string }[] => [
    { id: 'due', label: 'Due date' },
    { id: 'status', label: 'Status' },
    { id: 'priority', label: 'Priority' },
    // Offered only where there is somewhere to file things. A grouping whose
    // every row would say "Not filed" is a menu item that teaches people the
    // feature does not work.
    ...(purpose.enabled ? [{ id: 'purpose' as const, label: 'Goal' }] : []),
    { id: 'none', label: 'Nothing' },
  ])

  /**
   * The list itself, where there is no task under the pointer.
   *
   * The same three controls the header carries, within reach of where you
   * are looking rather than at the top of the pane.
   */
  function listMenu(): MenuItem[] {
    return tidyMenu([
      {
        label: 'New task',
        icon: 'plus',
        hint: 'Ctrl+N',
        // The capture line is `TodoView`'s, and this component is inside it
        // rather than around it; `App.svelte` reaches the search field the
        // same way for Ctrl+F.
        run: () => document.querySelector<HTMLInputElement>('.quickadd .field')?.focus(),
      },
      SEP,
      {
        label: 'Group by',
        icon: 'layers',
        items: GROUPS.map((g) => ({
          label: g.label,
          checked: todo.groupBy === g.id,
          run: () => (todo.groupBy = g.id),
        })),
      },
      {
        label: 'Show',
        icon: 'circle',
        items: TASK_FILTERS.map((f) => ({
          label: FILTER_LABELS[f],
          checked: todo.statusFilter === f,
          run: () => todo.setStatusFilter(f),
        })),
      },
      todo.narrowed && {
        label: 'Clear the filters',
        icon: 'close',
        run: () => todo.clearFilters(),
      },
      SEP,
      todo.boardable && {
        label: 'Switch to the board',
        icon: 'board',
        run: () => todo.setView('board'),
      },
    ])
  }
</script>

<!-- One tab stop for the whole list, and the arrow keys inside it: see
     `lib/roving.ts`. A task row carries a tick, a title and a disclosure, so
     without this a list of two thousand tasks was six thousand stops between
     the capture line and anything after it. Up and Down move between rows,
     Left and Right between the controls of one. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="scroll list"
  oncontextmenu={(e) => menu.show(e, listMenu())}
  use:rovingFocus={todo.selectedTask}
>
  <!-- `visible` rather than `tasks`: a scope with work in it that the filter
       bar has narrowed to nothing is a different thing to be told than a
       scope that is genuinely empty, and saying "no tasks yet" over a list
       somebody has just filtered is how a filter looks like a bug. -->
  {#if todo.visible.length === 0}
    <!-- The two narrowings are told apart, because only one of them can be
         counted. The text filter is applied by the backend, so `tasks` is
         already down to what matched it -- offering "there are 0 tasks here
         in all" over a project of forty was arithmetic on a number that
         means something else. The chips and dropdowns are applied here, so
         over those the total is honest and worth saying. -->
    {#if todo.filter.trim()}
      <EmptyState lead={`Nothing matches “${todo.filter.trim()}”.`}>
        {#snippet note()}Try fewer words, or a different list.{/snippet}
        {#snippet action()}
          <button class="btn" onclick={() => todo.clearFilters()}>Clear the filters</button>
        {/snippet}
      </EmptyState>
    {:else if todo.narrowed}
      <EmptyState lead="Nothing under these filters.">
        {#snippet note()}
          {plural(todo.tasks.length, 'task')}
          {todo.tasks.length === 1 ? 'is' : 'are'} in this list in all.
        {/snippet}
        {#snippet action()}
          <button class="btn" onclick={() => todo.clearFilters()}>Clear the filters</button>
        {/snippet}
      </EmptyState>
    {:else if todo.scope.kind === 'today'}
      <EmptyState lead="Nothing due today.">
        {#snippet note()}Anything overdue would be here too.{/snippet}
      </EmptyState>
    {:else if todo.scope.kind === 'inbox'}
      <EmptyState lead="The inbox is empty.">
        {#snippet note()}Tasks land here when you add one without a project.{/snippet}
      </EmptyState>
    {:else}
      <EmptyState lead="No tasks yet.">
        {#snippet note()}
          Add one above — try <code>Buy milk @tomorrow !high</code>.
        {/snippet}
      </EmptyState>
    {/if}
  {:else}
    {#each groups as group (group.key)}
      {#if group.label}
        <div class="grouphead">
          <span class="eyebrow" class:late={group.key === 'overdue'}>{group.label}</span>
          <span class="n">{group.nodes.length}</span>
        </div>
      {/if}
      {#each group.nodes as node (node.task.id)}
        <TaskRow {node} />
      {/each}
    {/each}
  {/if}
</div>

<style>
  .list {
    flex: 1;
    display: flex;
    flex-direction: column;
    /* The tail clears the floating assistant button. */
    padding: 0 var(--sp-4) var(--fab-clear);
  }

  .grouphead {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    padding: var(--sp-5) var(--sp-2) var(--sp-2);
  }
  .eyebrow.late {
    color: var(--danger);
  }
  .n {
    font-size: var(--text-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }

  code {
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    padding: 1px 5px;
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
  }
</style>
