// Sample cards for the panel page in a browser, where there is no shell:
// `npm run dev`, then http://localhost:5273/panel.html. Reached only through
// the development guard in `source.ts`, so none of this is in a release build.
//
// `?cards=meeting,typing,talk,failed` picks which cards are up and in what
// order (the others are `done` and `busy`); `?theme=` is read by `main.ts`.
// Pressing a button takes its card down after a moment, ticking a box ticks
// it, so what the page does between a press and the shell's answer can be
// watched. The height the page reports is left on `<html data-panel-height>`
// for a screenshot script to read.
import type { Panel } from './panel'
import type { PanelSource } from './source'

const DEFAULT = ['meeting', 'typing', 'talk', 'failed']

/** How long the sample shell takes to answer a press. */
const ANSWER_MS = 500

const SAMPLES: Record<string, () => Panel> = {
  // A meeting about to be recorded, with its brief.
  meeting: () => ({
    id: 'event:mock:product-sync',
    owner: 'meeting',
    tone: 'attention',
    kicker: 'Recording starts in',
    title: 'Product sync with Josh and Priya',
    count: '12s',
    body: '10:00–10:30 · Room 4, or the Zoom link in the invite',
    meter: false,
    busy: false,
    sections: [
      {
        heading: 'Agenda',
        folded: false,
        items: [
          { text: 'Roadmap for the second half' },
          { text: 'Hiring: the two open design roles' },
          { text: 'Budget for the offsite' },
        ],
      },
      {
        heading: 'With',
        folded: false,
        items: [
          { text: 'Josh Miller', detail: 'Finance lead at Northwind · last met on 3 November' },
          { text: 'Priya Raman', detail: 'Design · on your team' },
        ],
      },
      {
        heading: 'To raise',
        folded: false,
        items: [
          {
            text: 'Ask Josh whether the Q3 numbers are final',
            detail: 'With Josh Miller',
            check: { action: 'done:mock-1', done: false },
          },
          {
            text: 'Send Priya the offsite dates',
            check: { action: 'done:mock-2', done: true },
          },
        ],
      },
      {
        heading: 'Context',
        folded: true,
        items: [
          {
            text: 'Last time: agreed to freeze the scope by the end of the month',
            detail: 'Meeting notes, 3 November',
          },
          {
            text: 'Josh asked for a revised forecast before the next review',
            detail: 'Mail from Josh Miller, 9 November',
          },
          { text: 'Northwind announced a new office in Lisbon', detail: 'A look on the web' },
        ],
      },
    ],
    actions: [
      { id: 'cancel', label: 'Cancel', primary: false },
      { id: 'take', label: 'Take notes', primary: true },
    ],
  }),

  // Voice typing, listening.
  typing: () => ({
    id: 'voice-typing',
    owner: 'voiceTyping',
    tone: 'listening',
    kicker: 'Voice typing',
    title: 'Listening',
    body: 'Press Ctrl+Alt+M to finish',
    lines: [{ text: 'Remind me to send the invoice to Northwind on Friday and', pending: true }],
    meter: true,
    busy: false,
    actions: [
      { id: 'cancel', label: 'Cancel', primary: false },
      { id: 'stop', label: 'Stop', primary: true },
    ],
  }),

  // A conversation from the talk key, waiting on an answer.
  talk: () => ({
    id: 'talk',
    owner: 'talk',
    tone: 'attention',
    kicker: 'Assistant',
    title: 'Waiting for your answer',
    body: 'Move “Design review” to tomorrow at 3 pm',
    lines: [
      { who: 'You', text: 'Can you move my three o’clock to tomorrow?', pending: false },
      {
        who: 'Asha',
        text: 'I can. It has four people invited, so I need your say-so first.',
        pending: true,
      },
    ],
    meter: false,
    busy: false,
    actions: [
      { id: 'deny', label: 'Don’t allow', primary: false },
      { id: 'allow', label: 'Allow', primary: true },
    ],
  }),

  // Voice typing that could not type, offering what was said.
  failed: () => ({
    id: 'voice-typing-failed',
    owner: 'voiceTyping',
    tone: 'trouble',
    kicker: 'Voice typing',
    title: 'Nothing was typed',
    body: 'The window you were in does not take pasted text. What you said is below.',
    lines: [{ text: 'Remind me to send the invoice on Friday.', pending: false }],
    meter: false,
    busy: false,
    actions: [
      { id: 'dismiss', label: 'Dismiss', primary: false },
      {
        id: 'copy',
        label: 'Copy',
        primary: true,
        copy: 'Remind me to send the invoice on Friday.',
      },
    ],
  }),

  // Done, and fine.
  done: () => ({
    id: 'voice-typing-done',
    owner: 'voiceTyping',
    tone: 'success',
    kicker: 'Voice typing',
    title: 'Typed',
    body: 'Into Notes',
    meter: false,
    busy: false,
  }),

  // Working something out.
  busy: () => ({
    id: 'meeting-notes',
    owner: 'meeting',
    tone: 'working',
    kicker: 'Meeting notes',
    title: 'Writing up the standup',
    body: 'This takes about a minute.',
    meter: false,
    busy: true,
    actions: [{ id: 'open', label: 'Open', primary: false }],
  }),
}

/** A speaking voice, as the level it would read: bursts with pauses between. */
function speech(tick: number): number {
  const talking = Math.sin(tick / 14) > -0.2
  const body = Math.abs(Math.sin(tick / 3)) * 0.4 + Math.abs(Math.sin(tick / 7)) * 0.5
  return talking ? Math.min(1, body) : 0
}

/** The card with the box that sends `action` turned over. */
function ticked(card: Panel, action: string): Panel {
  return {
    ...card,
    sections: card.sections?.map((section) => ({
      ...section,
      items: section.items.map((item) => {
        if (item.check?.action !== action) return item
        return { ...item, check: { ...item.check, done: !item.check.done } }
      }),
    })),
  }
}

export function mockSource(search: string): PanelSource {
  const wanted = new URLSearchParams(search).get('cards')?.split(',') ?? DEFAULT
  let stack = wanted.flatMap((name) => {
    const make = SAMPLES[name.trim()]
    return make ? [make()] : []
  })
  const listeners = new Set<(stack: Panel[]) => void>()

  function publish() {
    for (const listener of listeners) listener(structuredClone(stack))
  }

  /** The shell's answer to a press, a moment later. */
  function answer(id: string, action: string) {
    setTimeout(() => {
      if (action.startsWith('done:')) {
        stack = stack.map((card) => (card.id === id ? ticked(card, action) : card))
      } else {
        stack = stack.filter((card) => card.id !== id)
      }
      publish()
    }, ANSWER_MS)
  }

  return {
    state: async () => structuredClone(stack),

    onState: async (handler) => {
      listeners.add(handler)
      return () => listeners.delete(handler)
    },

    onLevel: async (handler) => {
      let tick = 0
      const timer = setInterval(() => {
        tick += 1
        for (const card of stack) if (card.meter) handler({ id: card.id, level: speech(tick) })
      }, 50)
      return () => clearInterval(timer)
    },

    action: async (id, action, copied) => {
      console.debug('panel: pressed', { id, action, copied })
      answer(id, action)
    },

    resize: async (height) => {
      console.debug('panel: height', height)
      document.documentElement.dataset.panelHeight = String(height)
    },
  }
}
