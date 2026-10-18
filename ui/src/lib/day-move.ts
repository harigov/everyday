// Dragging something on the calendar onto another day.
//
// The band over the week grid and the month's cells have no minutes to
// drop on, only days, so what is dragged there is moved by whole days and
// keeps its time -- unlike the grid's own columns, where an event or a
// block is picked up and put down at a time with pointer events (see
// `TimeGrid`). These use the browser's drag and drop instead, as a task
// dragged in from the rail does, because the thing being aimed at is a
// cell and the browser already knows which one the pointer is over.

/** The drag type a day move carries. Its own, so a task's drop targets ignore it. */
const DAY_MOVE = 'text/x-everyday-day-move'

/** What was picked up, and the day it was picked up from. */
export interface DayMove {
  kind: 'event' | 'block'
  id: string
  /** `YYYY-MM-DD` of the cell it was dragged out of. */
  from: string
}

export function startDayMove(e: DragEvent, move: DayMove) {
  if (!e.dataTransfer) return
  e.dataTransfer.setData(DAY_MOVE, JSON.stringify(move))
  e.dataTransfer.effectAllowed = 'move'
}

/**
 * Is a day move what is being dragged over? Asked on `dragover`, where the
 * browser lets the types be seen but not the data.
 */
export function carriesDayMove(e: DragEvent): boolean {
  return !!e.dataTransfer?.types.includes(DAY_MOVE)
}

/** The day move being dropped, or `null` for anything else. */
export function readDayMove(e: DragEvent): DayMove | null {
  const raw = e.dataTransfer?.getData(DAY_MOVE)
  if (!raw) return null
  try {
    const move = JSON.parse(raw) as Partial<DayMove>
    if (
      (move.kind === 'event' || move.kind === 'block') &&
      typeof move.id === 'string' &&
      typeof move.from === 'string'
    ) {
      return { kind: move.kind, id: move.id, from: move.from }
    }
  } catch {
    // Something else's data under our type; not ours to read.
  }
  return null
}
