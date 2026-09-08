// The shelf of ready-made trackers.
//
// Setting up tracking is the moment where a good feature is abandoned: the
// form is right there, and answering "kind, unit, icon, colour, target" five
// times before recording anything is enough work to close the dialog. So the
// common ones are one click, already carrying the answers that are tedious
// to decide and easy to get wrong -- that a dose is measured in mg and a
// severity runs to ten, that a run belongs on the calendar and flossing does
// not.
//
// Everything here is editable afterwards. A preset is a starting point, not
// a category: the fields it fills are the same fields the empty form has.

import type { Tracker, TrackerKind } from './types'

export interface TrackerPreset {
  name: string
  kind: TrackerKind
  icon: string
  color: string
  unit?: string
  defaultValue?: number
  target?: number
  /** Readings whose time of day is worth drawing on the calendar. */
  onCalendar?: boolean
}

export const TRACKER_PRESETS: TrackerPreset[] = [
  // Habits: a tick, and nothing to measure.
  { name: 'Walk', kind: 'check', icon: 'walk', color: '#16a34a' },
  { name: 'Floss', kind: 'check', icon: 'tooth', color: '#0284c7' },
  { name: 'No alcohol', kind: 'check', icon: 'wine', color: '#9333ea' },
  { name: 'No smoking', kind: 'check', icon: 'cigarette', color: '#78716c' },
  { name: 'Journalled', kind: 'check', icon: 'pen', color: '#4f46e5' },
  { name: 'Fed the dog', kind: 'check', icon: 'paw', color: '#f59e0b' },

  // Doses: the unit matters, and each one is its own reading.
  {
    name: 'Medication',
    kind: 'dose',
    icon: 'pill',
    color: '#e11d48',
    unit: 'mg',
    defaultValue: 1,
    onCalendar: true,
  },
  {
    name: 'Vitamin D',
    kind: 'dose',
    icon: 'tablet',
    color: '#f59e0b',
    unit: 'iu',
    defaultValue: 1000,
  },
  {
    name: 'Supplement',
    kind: 'dose',
    icon: 'bottle',
    color: '#16a34a',
    unit: 'mg',
    defaultValue: 500,
  },

  // Symptoms: how bad, out of ten, several times a day if that is the truth.
  { name: 'Headache', kind: 'scale', icon: 'bolt', color: '#e11d48', onCalendar: true },
  { name: 'Pain', kind: 'scale', icon: 'pulse', color: '#db2777', onCalendar: true },
  { name: 'Mood', kind: 'scale', icon: 'smile', color: '#f97316' },
  { name: 'Energy', kind: 'scale', icon: 'battery', color: '#0d9488' },
  { name: 'Anxiety', kind: 'scale', icon: 'frown', color: '#9333ea' },

  // Quantities. The ones measured in minutes draw as spans on the calendar.
  {
    name: 'Exercise',
    kind: 'amount',
    icon: 'run',
    color: '#0284c7',
    unit: 'min',
    defaultValue: 30,
    target: 30,
    onCalendar: true,
  },
  {
    name: 'Meditation',
    kind: 'amount',
    icon: 'meditate',
    color: '#4f46e5',
    unit: 'min',
    defaultValue: 10,
    target: 10,
    onCalendar: true,
  },
  {
    name: 'Sleep',
    kind: 'amount',
    icon: 'bed',
    color: '#4f46e5',
    unit: 'hours',
    defaultValue: 8,
    target: 8,
  },
  {
    name: 'Water',
    kind: 'amount',
    icon: 'water',
    color: '#0284c7',
    unit: 'glasses',
    defaultValue: 1,
    target: 8,
  },
  {
    name: 'Pages read',
    kind: 'amount',
    icon: 'book',
    color: '#78716c',
    unit: 'pages',
    defaultValue: 20,
    target: 20,
  },
  {
    name: 'Steps',
    kind: 'amount',
    icon: 'steps',
    color: '#16a34a',
    unit: 'steps',
    defaultValue: 1000,
    target: 8000,
  },
  {
    name: 'Weight',
    kind: 'amount',
    icon: 'scales',
    color: '#0d9488',
    unit: 'kg',
    defaultValue: 70,
  },
  {
    name: 'Coffee',
    kind: 'amount',
    icon: 'coffee',
    color: '#b45309',
    unit: 'cups',
    defaultValue: 1,
  },
  {
    name: 'Screen time',
    kind: 'amount',
    icon: 'phone',
    color: '#78716c',
    unit: 'min',
    defaultValue: 30,
  },
]

/** Apply a preset to a freshly minted tracker. */
export function fromPreset(tracker: Tracker, preset: TrackerPreset): Tracker {
  return {
    ...tracker,
    name: preset.name,
    kind: preset.kind,
    icon: preset.icon,
    color: preset.color,
    unit: preset.unit ?? '',
    defaultValue: preset.defaultValue ?? 1,
    target: preset.target ?? null,
    onCalendar: preset.onCalendar ?? false,
  }
}

/** What each kind is for, in the words the picker uses. */
export const KIND_COPY: Record<TrackerKind, { label: string; hint: string }> = {
  check: { label: 'Did it', hint: 'A habit: done, or not done yet' },
  dose: { label: 'Took this much', hint: 'Medication and supplements, one reading per dose' },
  scale: { label: 'How bad', hint: 'Pain, symptoms, mood — a severity out of ten' },
  amount: { label: 'A number', hint: 'Minutes, pages, glasses — anything that adds up' },
}
