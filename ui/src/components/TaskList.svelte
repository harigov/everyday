<script lang="ts">
  // The list view: a task tree, cut into sections.
  //
  // Grouping is a property of the view rather than of the query, so the same
  // loaded set can be re-cut without another round trip -- which is what
  // makes flipping between "by due date" and "by priority" instant.

  import { todo } from '../lib/todo.svelte'
  import { friendlyDate, todayIso } from '../lib/format'
  import TaskRow from './TaskRow.svelte'
  import type { TaskNode } from '../lib/todo.svelte'
  import type { Task } from '../lib/types'

  const STATUS_LABELS: Record<string, string> = {
    backlog: 'Backlog',
    todo: 'To do',
    doing: 'Doing',
    blocked: 'Blocked',
    done: 'Done',
    cancelled: 'Cancelled',
  }

  const PRIORITY_LABELS: Record<string, string> = {
    urgent: 'Urgent',
    high: 'High',
    medium: 'Medium',
    low: 'Low',
    none: 'Unprioritised',
  }

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
    const p = (n: number) => String(n).padStart(2, '0')
    const horizon = `${week.getFullYear()}-${p(week.getMonth() + 1)}-${p(week.getDate())}`
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
          order: key === 'none' ? 9 : 4 - todo.rank(key),
        }
      }
      const group = out.get(bucket.key) ?? { ...bucket, nodes: [] }
      group.nodes.push(node)
      out.set(bucket.key, group)
    }
    return [...out.values()].sort((a, b) => a.order - b.order || a.key.localeCompare(b.key))
  })
</script>

<div class="scroll list">
  {#if todo.tasks.length === 0}
    <div class="blank">
      {#if todo.filter.trim()}
        <p>Nothing matches “{todo.filter}”.</p>
      {:else if todo.scope.kind === 'today'}
        <p>Nothing due today.</p>
        <p class="quiet">Anything overdue would be here too.</p>
      {:else if todo.scope.kind === 'inbox'}
        <p>The inbox is empty.</p>
        <p class="quiet">Tasks land here when you add one without a project.</p>
      {:else}
        <p>No tasks yet.</p>
        <p class="quiet">Add one above — try <code>Buy milk @tomorrow !high</code>.</p>
      {/if}
    </div>
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
  .list { flex: 1; padding: 0 var(--sp-4) var(--sp-10); }

  .grouphead {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    padding: var(--sp-5) var(--sp-2) var(--sp-2);
  }
  .eyebrow.late { color: var(--danger); }
  .n { font-size: var(--text-xs); color: var(--fg-faint); font-variant-numeric: tabular-nums; }

  .blank { padding: var(--sp-12) var(--sp-4); text-align: center; color: var(--fg-subtle); }
  .blank .quiet { margin-top: var(--sp-2); font-size: var(--text-sm); color: var(--fg-faint); }
  .blank code {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    padding: 1px 5px;
    border-radius: var(--radius-sm);
    background: var(--bg-sunken);
  }
</style>
