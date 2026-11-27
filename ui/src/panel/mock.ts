// Sample cards for the panel page in a browser, where there is no shell:
// `npm run dev`, then http://localhost:5273/panel.html. Reached only through
// the development guard in `source.ts`, so none of this is in a release build.
//
// `?cards=meeting,mail,talk` picks which cards are up and in what order --
// any of meeting, countdown, invite, mail, recording, notes, ask, question,
// talk, thinking, speaking, confirm, typing, failed, done and busy -- and
// `?theme=` is read by `main.ts`. With no `?cards=`, a meeting, a message, a
// conversation and notes: three drawn and "+1 more".
//
// The samples answer the way the shell's owners would, a moment later, so
// what the page does between a press and the answer can be watched: an action
// item taken or left flips, a box ticks, a reply goes busy, then "Sending…"
// with Undo, then "Sent" and away (type "fail" in it to see the words come
// back with an error), a note or a question during a recording lands on the
// card, the countdown counts down into a recording, and a card that goes by
// itself drains and goes -- holding still while the pointer is on it, as the
// hub does. The height the page reports is left on `<html data-panel-height>`
// for a screenshot script to read.
//
// No timer starts until `mockSource` is called, so loading this module (as a
// test might) leaves nothing running.
import type { Panel, PanelAction, PanelBlock, PanelExpiry, Person, Proposal, Turn } from './panel'
import type { PanelSource } from './source'

const DEFAULT = ['meeting', 'mail', 'talk', 'notes']

/** How long the sample shell takes to answer a press. */
const ANSWER_MS = 500

/** The assistant's name in the samples. */
const NAME = 'Asha'

const act = (id: string, label: string, more: Partial<PanelAction> = {}): PanelAction => ({
  id,
  label,
  primary: false,
  danger: false,
  ...more,
})

const person = (name: string, email: string, more: Partial<Person> = {}): Person => ({
  name,
  email,
  organizer: false,
  you: false,
  ...more,
})

const said = (who: Turn['who'], text: string, pending = false): Turn =>
  who === 'assistant' ? { who, name: NAME, text, pending } : { who, text, pending }

/** An expiry as an owner sets it: the hub fills in the time left and whether it is held. */
const after = (ms: number, action = 'expire'): PanelExpiry => ({
  afterMs: ms,
  action,
  remainingMs: ms,
  held: false,
})

const minutesAgo = (minutes: number) => Date.now() - minutes * 60_000

const TEAM: Person[] = [
  person('Josh Miller', 'josh@northwind.example', {
    detail: 'Finance lead at Northwind · last met on 3 November',
    status: 'accepted',
    organizer: true,
  }),
  person('Priya Raman', 'priya@jatayu.example', {
    detail: 'Design · on your team',
    status: 'tentative',
  }),
  person('Sam Patel', 'sam@jatayu.example', { status: 'accepted', you: true }),
  person('Ama Owusu', 'ama@northwind.example', {
    detail: 'Product manager at Northwind · wrote on Monday about the roadmap',
    status: 'needsAction',
  }),
  person('Lena Fischer', 'lena@northwind.example', {
    detail: 'Engineering, Berlin office',
    status: 'declined',
  }),
]

const SAMPLES: Record<string, () => Panel> = {
  // A meeting coming up, with its brief.
  meeting: () => ({
    id: 'event:mock:product-sync',
    owner: 'meeting',
    kind: 'meeting',
    tone: 'attention',
    kicker: 'Meeting in 5 min',
    title: 'Product sync with Josh and Priya',
    meter: false,
    busy: false,
    blocks: [
      {
        type: 'event',
        title: 'Product sync with Josh and Priya',
        when: '10:00–10:30',
        date: 'Today',
        place: 'Room 4',
        join: 'Zoom',
        calendar: 'Work',
        people: TEAM,
      },
      { type: 'people', heading: 'Who’s coming', people: TEAM, folded: false },
      {
        type: 'text',
        heading: 'What it’s about',
        text: 'The second-half roadmap and the two open design roles. Josh wants the Q3 numbers settled before anything else.',
        folded: false,
      },
      {
        type: 'list',
        heading: 'To raise',
        folded: false,
        items: [
          {
            text: 'Ask Josh whether the Q3 numbers are final',
            detail: 'With Josh Miller',
            check: { action: 'done:mock-1', done: false },
          },
          { text: 'Send Priya the offsite dates', check: { action: 'done:mock-2', done: true } },
        ],
      },
      {
        type: 'list',
        heading: 'Worth covering',
        folded: false,
        items: [
          {
            text: 'The revised forecast Josh asked for',
            detail: 'Mail from Josh Miller, 9 November',
          },
        ],
      },
      {
        type: 'list',
        heading: 'Agenda',
        folded: true,
        items: [
          { text: 'Roadmap for the second half' },
          { text: 'Hiring: the two open design roles' },
          { text: 'Budget for the offsite' },
        ],
      },
      {
        type: 'text',
        heading: 'Last time',
        text: 'Agreed to freeze the scope by the end of the month. Northwind announced a new office in Lisbon since.',
        folded: true,
      },
    ],
    actions: [
      act('take', 'Take notes', { primary: true, icon: 'mic' }),
      act('join', 'Join', { icon: 'join' }),
      act('notNow', 'Not now'),
    ],
  }),

  // A meeting about to be recorded, counting down.
  countdown: () => ({
    id: 'event:mock:standup',
    owner: 'meeting',
    kind: 'meeting',
    tone: 'attention',
    kicker: 'Recording starts in',
    title: 'Design standup',
    count: '12s',
    meter: false,
    busy: false,
    blocks: [
      {
        type: 'event',
        title: 'Design standup',
        when: '9:30–9:45',
        date: 'Today',
        join: 'Google Meet',
        calendar: 'Work',
        people: TEAM.slice(1, 4),
      },
    ],
    actions: [
      act('take', 'Record now', { primary: true, icon: 'mic' }),
      act('cancel', 'Don’t record', { icon: 'x' }),
    ],
  }),

  // An invitation that came by mail.
  invite: () => ({
    id: 'mail:mock:invite',
    owner: 'mail',
    kind: 'invite',
    tone: 'attention',
    kicker: 'Invitation from Ama Owusu',
    title: 'Northwind roadmap review',
    meter: false,
    busy: false,
    blocks: [
      {
        type: 'event',
        title: 'Northwind roadmap review',
        when: '14:00–15:00',
        date: 'Thu 27 Nov',
        place: 'Northwind, 4th floor',
        join: 'Microsoft Teams',
        calendar: 'Work',
        people: [
          person('Ama Owusu', 'ama@northwind.example', { organizer: true, status: 'accepted' }),
          person('Josh Miller', 'josh@northwind.example', { status: 'accepted' }),
          person('Lena Fischer', 'lena@northwind.example', { status: 'tentative' }),
          person('Sam Patel', 'sam@jatayu.example', { status: 'needsAction', you: true }),
        ],
        rsvp: 'needsAction',
      },
      {
        type: 'text',
        heading: 'Description',
        text: 'Walk through the first-half roadmap before it goes to the board, and agree who owns the Lisbon launch.',
        folded: false,
      },
      {
        type: 'list',
        heading: 'Agenda',
        folded: false,
        items: [
          { text: 'H1 roadmap, top to bottom', detail: '30 min · Ama' },
          { text: 'Lisbon launch owner', detail: '15 min' },
          { text: 'Open questions', detail: '15 min' },
        ],
      },
      {
        type: 'list',
        heading: 'Worth covering',
        folded: true,
        items: [
          { text: 'Your note from 3 November: “Push for a Q2 beta”' },
          { text: 'Josh is still waiting on the revised forecast' },
        ],
      },
    ],
    actions: [
      act('accepted', 'Accept', { primary: true, icon: 'check' }),
      act('tentative', 'Maybe'),
      act('declined', 'Decline', { icon: 'x' }),
    ],
    expires: after(45_000),
  }),

  // New mail, with a box to reply in and suggested replies.
  mail: () => ({
    id: 'mail:mock:josh',
    owner: 'mail',
    kind: 'mail',
    tone: 'neutral',
    kicker: 'New mail · Work',
    title: 'Q3 numbers — are they final?',
    meter: false,
    busy: false,
    blocks: [
      {
        type: 'mail',
        from: person('Josh Miller', 'josh@northwind.example'),
        subject: 'Q3 numbers — are they final?',
        snippet:
          'Hi Sam, before Thursday’s review could you confirm the Q3 numbers are final? Finance wants to lock the forecast by Friday, and I’d rather not send them a draft. The sheet is attached.',
        received: '2:14 pm',
        account: 'sam@work.example',
        attachments: 1,
      },
      {
        type: 'input',
        id: 'reply',
        placeholder: 'Reply to Josh…',
        submit: 'Send',
        multiline: true,
        busy: false,
        suggestions: [
          {
            label: 'They’re final',
            text: 'Hi Josh,\n\nYes, the Q3 numbers are final. Go ahead and lock the forecast.\n\nThanks',
          },
          {
            label: 'Not yet',
            text: 'Hi Josh,\n\nNot quite yet. I’ll have the final numbers to you by Thursday morning.\n\nThanks',
          },
          { label: 'Let’s talk', text: 'Hi Josh,\n\nCan we talk it through tomorrow at ten?' },
        ],
      },
    ],
    actions: [
      act('archive', 'Archive', { icon: 'archive' }),
      act('read', 'Mark read', { icon: 'read' }),
      act('open', 'Open', { icon: 'open' }),
    ],
    expires: after(45_000),
  }),

  // A meeting being recorded: the clock, the level, your notes, questions.
  recording: () => ({
    id: 'recording:mock',
    owner: 'recording',
    kind: 'meeting',
    tone: 'listening',
    kicker: 'Recording',
    title: 'Product sync with Josh and Priya',
    meter: true,
    busy: false,
    blocks: [
      { type: 'activity', mode: 'recording' },
      { type: 'clock', sinceMs: minutesAgo(12.4), label: 'Recording for' },
      {
        type: 'list',
        heading: 'Your notes',
        folded: false,
        items: [
          { text: 'Josh: Q3 numbers final by Friday', detail: 'At 3 min' },
          { text: 'Priya to shortlist offsite venues', detail: 'At 9 min' },
        ],
      },
      {
        type: 'conversation',
        turns: [
          said('you', 'What did Josh say about the budget?'),
          said(
            'assistant',
            'That the offsite is capped at £12k, and anything above needs his sign-off.',
          ),
        ],
      },
      {
        type: 'input',
        id: 'note',
        placeholder: 'Add a note',
        submit: 'Add',
        multiline: false,
        busy: false,
        modes: [
          { id: 'note', label: 'Note', placeholder: 'Add a note', submit: 'Add' },
          { id: 'ask', label: 'Ask', placeholder: 'Ask about the meeting so far', submit: 'Ask' },
        ],
      },
    ],
    actions: [act('stop', 'Stop and write notes', { primary: true, icon: 'stop' })],
  }),

  // Notes written, with the action items found in them.
  notes: () => ({
    id: 'notes:mock',
    owner: 'notes',
    kind: 'notes',
    tone: 'success',
    kicker: 'Notes are ready',
    title: 'Notes from Product sync',
    meter: false,
    busy: false,
    blocks: [
      {
        type: 'note',
        title: 'Product sync with Josh and Priya',
        excerpt:
          'Agreed to freeze the Q3 numbers by Friday. The offsite moves to March; Priya shortlists venues. Both design roles open next week.',
        detail: 'In Notes · a 28-minute meeting',
      },
      {
        type: 'proposals',
        items: [
          {
            id: 'p1',
            title: 'Confirm the Q3 numbers with Finance',
            detail: 'For you',
            due: 'Fri',
            state: 'pending',
          },
          {
            id: 'p2',
            title: 'Shortlist three offsite venues',
            detail: 'Priya Raman',
            due: 'Next Wed',
            state: 'pending',
          },
          { id: 'p3', title: 'Post the two design roles', detail: 'For you', state: 'pending' },
        ],
      },
    ],
    actions: [
      act('acceptAll', 'Add all', { primary: true, icon: 'check' }),
      act('open', 'Open', { icon: 'open' }),
    ],
    expires: after(120_000),
  }),

  // A routine asking before it sends something.
  ask: () => ({
    id: 'ask:mock:send',
    owner: 'ask',
    kind: 'assistant',
    tone: 'attention',
    kicker: 'Morning brief asks',
    title: 'Send this?',
    meter: false,
    busy: false,
    blocks: [
      {
        type: 'quote',
        label: 'To Priya Raman · Offsite dates',
        text: 'Hi Priya,\n\nThe offsite is moving to 12–13 March. Could you shortlist three venues near the station by next Wednesday?\n\nThanks,\nSam',
      },
    ],
    actions: [
      act('allow', 'Send', { primary: true, icon: 'send' }),
      act('deny', 'Don’t send', { icon: 'x' }),
      act('later', 'Later', { icon: 'later' }),
    ],
  }),

  // A job asking a question, with answers to pick and a box for another.
  question: () => ({
    id: 'ask:mock:question',
    owner: 'ask',
    kind: 'assistant',
    tone: 'attention',
    kicker: 'Trip planning asks',
    title: 'Which train should I book to Leeds?',
    meter: false,
    busy: false,
    blocks: [
      {
        type: 'choices',
        prompt: 'Each gets you there before your 11:00 with Northwind.',
        choices: [
          act('choice:0', '07:03 · arrives 09:14'),
          act('choice:1', '07:33 · arrives 09:48'),
          act('choice:2', '08:03 · arrives 10:12'),
        ],
      },
      {
        type: 'input',
        id: 'answer',
        placeholder: 'Or answer in your own words…',
        submit: 'Answer',
        multiline: false,
        busy: false,
      },
    ],
    actions: [act('later', 'Later', { icon: 'later' })],
  }),

  // A conversation from the talk key, hearing the person.
  talk: () => ({
    id: 'talk',
    owner: 'talk',
    kind: 'assistant',
    tone: 'listening',
    kicker: 'Voice',
    title: `Talking with ${NAME}`,
    meter: true,
    busy: false,
    blocks: [
      { type: 'activity', mode: 'hearing' },
      {
        type: 'conversation',
        turns: [
          said('you', 'What’s left on my plate today?'),
          said(
            'assistant',
            'Three things: the Q3 numbers for Josh, Priya’s offsite dates, and Lena at 4 pm.',
          ),
          said('you', 'Move the one with Lena to tomorrow and', true),
        ],
      },
      {
        type: 'input',
        id: 'say',
        placeholder: `Type to ${NAME}…`,
        submit: 'Send',
        multiline: true,
        busy: false,
      },
    ],
    actions: [
      act('end', 'End', { icon: 'close' }),
      act('mute', 'Mute', { icon: 'micOff' }),
      act('open', 'Open', { icon: 'open' }),
    ],
  }),

  // The assistant at work with a tool.
  thinking: () => ({
    id: 'talk:thinking',
    owner: 'talk',
    kind: 'assistant',
    tone: 'working',
    kicker: 'Voice',
    title: `Talking with ${NAME}`,
    meter: false,
    busy: false,
    blocks: [
      {
        type: 'activity',
        mode: 'working',
        tool: { name: 'search_mail', label: 'Searching your mail for “offsite”…', icon: 'mail' },
      },
      {
        type: 'conversation',
        turns: [
          said('you', 'When did Priya last write about the offsite?'),
          said('assistant', '', true),
        ],
      },
    ],
    actions: [act('end', 'End', { icon: 'close' }), act('open', 'Open', { icon: 'open' })],
  }),

  // The assistant speaking, with what it made on the way.
  speaking: () => ({
    id: 'talk:speaking',
    owner: 'talk',
    kind: 'assistant',
    tone: 'working',
    kicker: 'Voice',
    title: `Talking with ${NAME}`,
    meter: false,
    busy: false,
    blocks: [
      { type: 'activity', mode: 'speaking' },
      {
        type: 'conversation',
        turns: [
          said(
            'you',
            'Remind me to send Josh the forecast on Friday, and put the venue visit on Tuesday.',
          ),
          said('assistant', 'Done: a task for Friday, and the visit is on Tuesday at 10:00', true),
        ],
      },
      {
        type: 'task',
        title: 'Send Josh the revised forecast',
        detail: 'Added to your tasks',
        due: 'Fri',
        done: false,
        check: 'done:mock-task',
      },
      {
        type: 'event',
        title: 'Venue visit',
        when: '10:00–11:00',
        date: 'Tue 2 Dec',
        place: 'The Old Mill, Leeds',
        calendar: 'Personal',
      },
      {
        type: 'note',
        title: 'Offsite ideas',
        excerpt: 'Morning: the roadmap. Afternoon: a walk, then dinner. Ask Lena about a speaker.',
        detail: 'Saved to Notes',
      },
    ],
    actions: [
      act('end', 'End', { icon: 'close' }),
      act('mute', 'Mute', { icon: 'micOff' }),
      act('open', 'Open', { icon: 'open' }),
    ],
  }),

  // The assistant needing a yes before it acts.
  confirm: () => ({
    id: 'talk:confirm',
    owner: 'talk',
    kind: 'assistant',
    tone: 'attention',
    kicker: `${NAME} asks`,
    title: 'Move “Design review” to tomorrow at 3 pm?',
    meter: false,
    busy: false,
    blocks: [
      { type: 'activity', mode: 'waiting', label: 'Waiting for your say-so' },
      {
        type: 'conversation',
        turns: [
          said('you', 'Can you move my three o’clock to tomorrow?'),
          said('assistant', 'It has four people invited, so I need your say-so first.'),
        ],
      },
      {
        type: 'quote',
        label: 'Everyone invited will be told',
        text: 'Design review\nTomorrow, 15:00–16:00 (was today, 15:00)',
      },
    ],
    actions: [
      act('allow', 'Allow', { primary: true, icon: 'check' }),
      act('deny', 'Don’t allow', { icon: 'x' }),
      act('always', 'Always allow'),
      act('later', 'Later', { icon: 'later' }),
    ],
  }),

  // Voice typing, listening.
  typing: () => ({
    id: 'voice-typing',
    owner: 'voiceTyping',
    kind: 'voiceTyping',
    tone: 'listening',
    kicker: 'Voice typing',
    title: 'Typing into Notes',
    body: 'Press Ctrl+Alt+M to finish',
    meter: true,
    busy: false,
    blocks: [
      { type: 'activity', mode: 'dictating' },
      {
        type: 'conversation',
        turns: [said('heard', 'Remind me to send the invoice to Northwind on Friday and', true)],
      },
    ],
    actions: [
      act('cancel', 'Cancel', { icon: 'x' }),
      act('stop', 'Stop', { primary: true, icon: 'stop' }),
    ],
  }),

  // Voice typing that could not type, offering what was said.
  failed: () => ({
    id: 'voice-typing-failed',
    owner: 'voiceTyping',
    kind: 'voiceTyping',
    tone: 'trouble',
    kicker: 'Voice typing',
    title: 'Nothing was typed',
    body: 'The window you were in does not take pasted text. What you said is below.',
    meter: false,
    busy: false,
    blocks: [{ type: 'quote', text: 'Remind me to send the invoice on Friday.' }],
    actions: [
      act('dismiss', 'Dismiss'),
      act('copy', 'Copy', {
        primary: true,
        icon: 'copy',
        copy: 'Remind me to send the invoice on Friday.',
      }),
    ],
  }),

  // Done, and fine.
  done: () => ({
    id: 'voice-typing-done',
    owner: 'voiceTyping',
    kind: 'voiceTyping',
    tone: 'success',
    kicker: 'Voice typing',
    title: 'Typed into Notes',
    meter: false,
    busy: false,
    blocks: [{ type: 'activity', mode: 'done', label: '14 words' }],
    expires: after(6_000),
  }),

  // Working something out.
  busy: () => ({
    id: 'notes:mock:writing',
    owner: 'notes',
    kind: 'notes',
    tone: 'working',
    kicker: 'Meeting notes',
    title: 'Writing up the standup',
    body: 'This takes about a minute.',
    meter: false,
    busy: true,
    blocks: [{ type: 'activity', mode: 'working', label: 'Writing the notes' }],
    actions: [act('open', 'Open', { icon: 'open' })],
  }),
}

/** A speaking voice, as the level it would read: bursts with pauses between. */
function speech(tick: number): number {
  const talking = Math.sin(tick / 14) > -0.2
  const body = Math.abs(Math.sin(tick / 3)) * 0.4 + Math.abs(Math.sin(tick / 7)) * 0.5
  return talking ? Math.min(1, body) : 0
}

/** A card with each of its blocks passed through `edit`. */
function mapBlocks(card: Panel, edit: (block: PanelBlock) => PanelBlock): Panel {
  return { ...card, blocks: card.blocks?.map(edit) }
}

/** A card with every box that sends `action` -- in a list, or a task's -- turned over. */
function ticked(card: Panel, action: string): Panel {
  return mapBlocks(card, (block) => {
    if (block.type === 'task' && block.check === action) return { ...block, done: !block.done }
    if (block.type !== 'list') return block
    return {
      ...block,
      items: block.items.map((item) =>
        item.check?.action === action
          ? { ...item, check: { ...item.check, done: !item.check.done } }
          : item,
      ),
    }
  })
}

/** Whether `card` has a box that sends `action`. */
function hasCheck(card: Panel, action: string): boolean {
  return (card.blocks ?? []).some(
    (block) =>
      (block.type === 'task' && block.check === action) ||
      (block.type === 'list' && block.items.some((item) => item.check?.action === action)),
  )
}

/** The card's action items, each passed through `edit`. */
function decided(card: Panel, edit: (item: Proposal) => Proposal): Panel {
  return mapBlocks(card, (block) =>
    block.type === 'proposals' ? { ...block, items: block.items.map(edit) } : block,
  )
}

/** Whether every action item on the card is decided. */
function allDecided(card: Panel): boolean {
  return (card.blocks ?? []).every(
    (block) => block.type !== 'proposals' || block.items.every((item) => item.state !== 'pending'),
  )
}

/** The card's box `id` with `edit` applied. */
function withInput(
  card: Panel,
  id: string,
  edit: (input: Extract<PanelBlock, { type: 'input' }>) => PanelBlock,
): Panel {
  return mapBlocks(card, (block) =>
    block.type === 'input' && block.id === id ? edit(block) : block,
  )
}

/** The card's conversation with `turns` added at the end (one is made where there is none). */
function withTurns(card: Panel, turns: Turn[], keep = 4): Panel {
  const blocks = card.blocks ?? []
  if (!blocks.some((block) => block.type === 'conversation')) {
    const at = blocks.findIndex((block) => block.type === 'input')
    const conversation: PanelBlock = { type: 'conversation', turns }
    const next = [...blocks]
    next.splice(at < 0 ? next.length : at, 0, conversation)
    return { ...card, blocks: next }
  }
  return mapBlocks(card, (block) => {
    if (block.type !== 'conversation') return block
    const settled = block.turns.map((turn) => ({ ...turn, pending: false }))
    return { ...block, turns: [...settled, ...turns].slice(-keep) }
  })
}

/** The card's activity block replaced. */
function withActivity(card: Panel, activity: Extract<PanelBlock, { type: 'activity' }>): Panel {
  return mapBlocks(card, (block) => (block.type === 'activity' ? activity : block))
}

/** What the hub keeps for a card that goes by itself. */
interface Deadline {
  afterMs: number
  action: string
  /** When it goes, while not held. */
  ends: number
  /** What was left when it was held. */
  left: number
  held: boolean
}

export function mockSource(search: string): PanelSource {
  const wanted = new URLSearchParams(search).get('cards')?.split(',') ?? DEFAULT
  let stack = wanted.flatMap((name) => {
    const make = SAMPLES[name.trim()]
    return make ? [make()] : []
  })
  const listeners = new Set<(stack: Panel[]) => void>()
  const deadlines = new Map<string, Deadline>()
  /** A mail card as it was before its reply was sent, for Undo. */
  const unsent = new Map<string, Panel>()
  let expiring: ReturnType<typeof setTimeout> | undefined
  let counting: ReturnType<typeof setInterval> | undefined

  /** Do `then` after `ms`, as the shell would answer. */
  function later(ms: number, then: () => void) {
    setTimeout(then, ms)
  }

  function find(id: string): Panel | undefined {
    return stack.find((card) => card.id === id)
  }

  function patch(id: string, edit: (card: Panel) => Panel) {
    stack = stack.map((card) => (card.id === id ? edit(card) : card))
    publish()
  }

  function drop(id: string) {
    stack = stack.filter((card) => card.id !== id)
    publish()
  }

  /** The stack as the hub sends it: each expiry with the time left and whether it is held. */
  function drawn(): Panel[] {
    const now = Date.now()
    return stack.map((card) => {
      const deadline = deadlines.get(card.id)
      if (!card.expires || !deadline) return structuredClone(card)
      const left = deadline.held ? deadline.left : Math.max(0, deadline.ends - now)
      return {
        ...structuredClone(card),
        expires: { ...card.expires, remainingMs: Math.round(left), held: deadline.held },
      }
    })
  }

  /** Keep a deadline for each card with an expiry, started when it first appeared. */
  function track() {
    const now = Date.now()
    for (const id of deadlines.keys()) {
      if (!find(id)?.expires) deadlines.delete(id)
    }
    for (const card of stack) {
      const expires = card.expires
      if (!expires) continue
      const known = deadlines.get(card.id)
      if (known && known.afterMs === expires.afterMs && known.action === expires.action) continue
      deadlines.set(card.id, {
        afterMs: expires.afterMs,
        action: expires.action,
        ends: now + expires.afterMs,
        left: expires.afterMs,
        held: false,
      })
    }
    clearTimeout(expiring)
    const next = [...deadlines.values()].filter((d) => !d.held).map((d) => d.ends)
    if (next.length === 0) return
    expiring = setTimeout(expire, Math.max(0, Math.min(...next) - now) + 10)
  }

  /** The cards whose time is up go, as the hub sends their owners `expires.action`. */
  function expire() {
    const now = Date.now()
    const due = [...deadlines.entries()].filter(([, d]) => !d.held && d.ends <= now)
    for (const [id, deadline] of due) {
      console.debug('panel: expired', { id, action: deadline.action })
      deadlines.delete(id)
      stack = stack.filter((card) => card.id !== id)
    }
    publish()
  }

  /** The countdown sample counts down, and turns into a recording at nought. */
  function count() {
    if (counting !== undefined || !stack.some((card) => card.count?.endsWith('s'))) return
    counting = setInterval(() => {
      const ticking = stack.filter((card) => card.count?.endsWith('s'))
      if (ticking.length === 0) {
        clearInterval(counting)
        counting = undefined
        return
      }
      for (const card of ticking) {
        const left = Number.parseInt(card.count ?? '0', 10) - 1
        if (left > 0) {
          stack = stack.map((c) => (c.id === card.id ? { ...c, count: `${left}s` } : c))
        } else {
          const recording = SAMPLES.recording?.()
          stack = stack.flatMap((c) =>
            c.id !== card.id ? [c] : recording ? [{ ...recording, title: card.title }] : [],
          )
        }
      }
      publish()
    }, 1000)
  }

  function publish() {
    track()
    count()
    const copy = drawn()
    for (const listener of listeners) listener(structuredClone(copy))
  }

  // ── what the owners would do ──────────────────────────────────────────

  function decide(card: Panel, action: string) {
    const [verb, id] = action.split(':')
    const state = verb === 'accept' ? 'accepted' : 'rejected'
    let next = decided(card, (item) => (item.id === id ? { ...item, state } : item))
    if (allDecided(next)) {
      next = { ...next, kicker: 'All decided', actions: [act('open', 'Open', { icon: 'open' })] }
      later(4000, () => drop(card.id))
    }
    patch(card.id, () => next)
  }

  function acceptAll(card: Panel) {
    patch(card.id, (c) => ({
      ...decided(c, (item) => (item.state === 'pending' ? { ...item, state: 'accepted' } : item)),
      kicker: 'Added to your tasks',
      actions: [],
    }))
    later(1500, () => drop(card.id))
  }

  function rsvp(card: Panel, response: 'accepted' | 'tentative' | 'declined') {
    patch(card.id, (c) => ({
      ...mapBlocks(c, (block) => (block.type === 'event' ? { ...block, rsvp: response } : block)),
      tone: 'success',
      kicker: 'Answered',
      actions: [],
      expires: undefined,
    }))
    later(1800, () => drop(card.id))
  }

  function mute(card: Panel) {
    const muted = card.blocks?.some((b) => b.type === 'activity' && b.mode === 'muted')
    patch(card.id, (c) => ({
      ...withActivity(c, { type: 'activity', mode: muted ? 'hearing' : 'muted' }),
      meter: muted ?? false,
      actions: c.actions?.map((a) =>
        a.id === 'mute'
          ? { ...a, label: muted ? 'Mute' : 'Unmute', icon: muted ? 'micOff' : 'mic' }
          : a,
      ),
    }))
  }

  /** A reply typed on a mail card: busy, then sending with Undo, then sent and away. */
  function reply(card: Panel, text: string) {
    if (/fail/i.test(text)) {
      patch(card.id, (c) =>
        withInput(c, 'reply', (input) => ({
          ...input,
          busy: false,
          error: 'The server would not take it. Your words are back in the box.',
        })),
      )
      return
    }
    unsent.set(card.id, card)
    patch(card.id, (c) =>
      withInput(c, 'reply', (input) => ({ ...input, busy: true, error: undefined })),
    )
    later(700, () => {
      if (!find(card.id)) return
      patch(card.id, (c) => ({
        ...c,
        tone: 'working',
        kicker: 'Sending',
        blocks: [
          { type: 'activity', mode: 'sending', label: 'Sending in 5 seconds…' },
          { type: 'quote', label: 'Your reply', text },
        ],
        actions: [act('undo', 'Undo', { primary: true })],
        expires: undefined,
      }))
      later(5000, () => {
        if (find(card.id)?.kicker !== 'Sending') return
        unsent.delete(card.id)
        patch(card.id, (c) => ({
          ...c,
          tone: 'success',
          kicker: 'Sent',
          blocks: [{ type: 'activity', mode: 'done', label: 'Sent to Josh Miller' }],
          actions: [],
        }))
        later(3000, () => drop(card.id))
      })
    })
  }

  /** A note or a question typed during a recording. */
  function recordingTyped(card: Panel, mode: string, text: string) {
    if (mode === 'ask') {
      patch(card.id, (c) =>
        withTurns(
          withInput(c, 'note', (input) => ({ ...input, busy: true })),
          [said('you', text), said('assistant', '', true)],
        ),
      )
      later(1600, () =>
        patch(card.id, (c) =>
          withInput(
            withTurns(
              mapBlocks(c, (block) =>
                block.type === 'conversation'
                  ? { ...block, turns: block.turns.filter((t) => t.text !== '') }
                  : block,
              ),
              [said('assistant', 'So far: Priya will send the venues by Wednesday.')],
            ),
            'note',
            (input) => ({ ...input, busy: false }),
          ),
        ),
      )
      return
    }
    const clock = card.blocks?.find((b) => b.type === 'clock')
    const minute = clock?.type === 'clock' ? Math.floor((Date.now() - clock.sinceMs) / 60_000) : 0
    patch(card.id, (c) =>
      mapBlocks(c, (block) =>
        block.type === 'list' && block.heading === 'Your notes'
          ? { ...block, items: [...block.items, { text, detail: `At ${minute} min` }].slice(-3) }
          : block,
      ),
    )
  }

  /** Words typed to the assistant: heard, thought about, answered. */
  function talkTyped(card: Panel, text: string) {
    patch(card.id, (c) =>
      withActivity(withTurns(c, [said('you', text), said('assistant', '', true)]), {
        type: 'activity',
        mode: 'thinking',
      }),
    )
    later(1200, () =>
      patch(card.id, (c) =>
        withActivity(
          mapBlocks(c, (block) =>
            block.type === 'conversation'
              ? {
                  ...block,
                  turns: block.turns.map((t) =>
                    t.who === 'assistant' && t.pending
                      ? { ...t, text: 'All right: Lena is now tomorrow at 4 pm.', pending: false }
                      : t,
                  ),
                }
              : block,
          ),
          { type: 'activity', mode: 'listening' },
        ),
      ),
    )
  }

  /** The shell's answer to a press, a moment later. */
  function answer(id: string, action: string, text: string | null) {
    later(ANSWER_MS, () => {
      const card = find(id)
      if (!card) return
      if (action.startsWith('accept:') || action.startsWith('reject:')) return decide(card, action)
      if (action === 'acceptAll') return acceptAll(card)
      if (hasCheck(card, action)) return patch(id, (c) => ticked(c, action))
      if (action === 'mute') return mute(card)
      if (action === 'undo') {
        const before = unsent.get(id)
        unsent.delete(id)
        return before ? patch(id, () => before) : drop(id)
      }
      if (card.kind === 'invite' && ['accepted', 'tentative', 'declined'].includes(action)) {
        return rsvp(card, action as 'accepted' | 'tentative' | 'declined')
      }
      if (text !== null) {
        if (card.owner === 'mail') return reply(card, text)
        if (card.owner === 'recording') return recordingTyped(card, action, text)
        if (card.owner === 'talk') return talkTyped(card, text)
      }
      drop(id)
    })
  }

  return {
    state: async () => {
      track()
      count()
      return drawn()
    },

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

    // The main window has said nothing: `?theme=`, the stored choice or the
    // desktop decides.
    theme: async () => null,
    onTheme: async () => () => {},

    action: async (id, action, copied, text = null) => {
      console.debug('panel: pressed', { id, action, copied, text })
      answer(id, action, text)
    },

    resize: async (height) => {
      console.debug('panel: height', height)
      document.documentElement.dataset.panelHeight = String(height)
    },

    focus: async (want) => {
      console.debug('panel: keyboard', want ? 'wanted' : 'given back')
    },

    hold: async (id, held) => {
      const deadline = deadlines.get(id)
      if (!deadline || deadline.held === held) return
      const now = Date.now()
      if (held) {
        deadline.left = Math.max(0, deadline.ends - now)
        deadline.held = true
      } else {
        deadline.ends = now + deadline.left
        deadline.held = false
      }
      publish()
    },

    drag: async () => {
      console.debug('panel: drag (the window moves in the shell)')
    },

    resetPlace: async () => {
      console.debug('panel: back to its corner')
    },
  }
}
