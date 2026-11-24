// A meeting's brief as the interface reads it: the cached one, brought up to
// date with what has happened to its talking points since it was made. Used
// by the brief's own view (`components/MeetingBriefView.svelte`), the
// calendar's rail and the meeting offer's banner.
import { api } from './api'
import { VaultError } from './types'
import type { MeetingBrief, TalkingPoint } from './types'

/**
 * A cached brief, with each talking point's tick read from its task.
 *
 * A brief is made some minutes before its meeting and kept; a point ticked
 * off since -- on the card in the corner, in the task list, by the assistant
 * -- would still read as waiting. Ask for the tasks again, one each (a
 * brief carries a handful), and fall back to what the brief says for any
 * that cannot be read. A task that is gone, or was cancelled, is not
 * something to raise any more, and is left out.
 */
export async function settledBrief(brief: MeetingBrief | null): Promise<MeetingBrief | null> {
  if (!brief || brief.talkingPoints.length === 0) return brief
  const points = await Promise.all(
    brief.talkingPoints.map(async (point) => {
      try {
        const task = await api.task(point.taskId)
        if (task.status === 'cancelled') return null
        return { ...point, done: task.status === 'done' }
      } catch (e) {
        return e instanceof VaultError && e.code === 'not_found' ? null : point
      }
    }),
  )
  return { ...brief, talkingPoints: points.filter((p): p is TalkingPoint => p !== null) }
}
