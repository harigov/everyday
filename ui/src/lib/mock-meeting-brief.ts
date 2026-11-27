// The meeting briefs' half of the in-memory backend.
//
// The real service gathers a brief from the invite, the Contacts shelf, mail,
// notes and the last meeting with the same people, and has the medium model
// write it up (`crates/jatayu-service/src/meeting/brief.rs`). The mock has
// none of that, so it writes one from what it can see -- the calendar event
// it is handed and the tasks that name somebody to raise them with -- plus a
// few lines of invented background for the people the mock calendar already
// has. Enough to look at the brief in the calendar rail, on the offer banner
// and in the task list's marks, and to tick a talking point off.
//
// Like the service, it keeps what it made: `meeting_brief` answers only from
// there, so an event nothing has been prepared for answers `null` until
// "Prepare brief" is pressed, and both states can be seen. The exception is
// the "Design sync" the dev-only offer trigger raises (`mock-meetings.ts`),
// which is given its brief at once, as a real offer's would have by the time
// anything asks -- the service prepares it a quarter of an hour ahead.

import { VaultError } from './types'
import type {
  CalendarEvent,
  MeetingBrief,
  MeetingBriefPerson,
  MeetingBriefSource,
  TalkingPoint,
  Task,
} from './types'

/** What has been prepared, by the key an offer carries: `event:{calendarId}:{uid}`. */
const prepared = new Map<string, MeetingBrief>()

/** The account the mock vault belongs to: never somebody to brief on. */
const OWNER = 'me@gmail.com'

/** The three things the mock knows about each person its calendar has. */
const BACKGROUND: Record<string, { line: string; contact?: string }> = {
  'priya@example.com': {
    line: 'Design lead. Organised this one, and asked for the numbers behind each option.',
    contact: 'it-priya',
  },
  'marcus.webb@example.com': {
    line: 'Engineering. Moved the build to the new runner; you last spoke on Tuesday.',
    contact: 'it-marcus',
  },
  'nadia@example.com': { line: 'Product. Wrote the planning doc this follows from.' },
  'josh@northwind.example': {
    line: 'Northwind, procurement. You met in March; their renewal is due next month.',
  },
}

/** What a model would have made of a meeting it was told something about. */
const WRITTEN: Record<
  string,
  { summary: string; open: string[]; context: string[]; mail?: string }
> = {
  'Design review': {
    summary: 'Choose between the two design options, using the numbers Priya asked you to bring.',
    open: [
      'You owe Priya what each option costs to build (she asked on Tuesday)',
      'Still open from last time: the type scale',
    ],
    context: [
      'Priya’s last mail, on Tuesday, asked what each option costs to build.',
      'Your note “Type scale options” sets the two side by side; neither has a cost yet.',
      'At the last review the type scale was left open.',
    ],
    mail: 'Re: Design options — numbers',
  },
  'Design sync': {
    summary: 'Settle the type scale before the build moves.',
    open: ['Josh is waiting on the finished pages before their renewal (since last week)'],
    context: [
      'Last week it was agreed to settle the type scale before the build moves.',
      'Josh from Northwind asked for the finished pages before their renewal.',
    ],
    mail: 'Northwind renewal — what they will see',
  },
}

/** `Ann Lee <ann@x.com>`, or a bare name, or a bare address, as the calendar gave it. */
function whoIs(raw: string): { name: string; email?: string } {
  const angled = /^(.*?)\s*<([^<>]+)>$/.exec(raw.trim())
  if (angled) return { name: angled[1]!.trim() || angled[2]!, email: angled[2]!.toLowerCase() }
  return raw.includes('@') ? { name: raw.trim(), email: raw.trim().toLowerCase() } : { name: raw }
}

/** The people at a meeting other than the person it is being prepared for. */
function peopleOf(ev: CalendarEvent): MeetingBriefPerson[] {
  const seen = new Set<string>()
  const people: MeetingBriefPerson[] = []
  for (const raw of ev.attendees ?? []) {
    const who = whoIs(raw)
    if (who.email === OWNER || who.name === 'Sam Weatherby') continue
    const key = who.email ?? who.name.toLowerCase()
    if (seen.has(key)) continue
    seen.add(key)
    const known = who.email ? BACKGROUND[who.email] : undefined
    people.push({
      name: who.name,
      ...(who.email ? { email: who.email } : {}),
      ...(known?.contact ? { contact: known.contact } : {}),
      line: known?.line ?? 'Not on your Contacts shelf yet.',
    })
  }
  return people
}

/** Whether a task's "raise with" names this person: by address, or by name or first name. */
function raisedWith(task: Task, person: MeetingBriefPerson): boolean {
  const first = person.name.split(/\s+/)[0]!.toLowerCase()
  return (task.raiseWith ?? []).some(
    (r) =>
      (!!r.email && r.email.toLowerCase() === person.email) ||
      r.name.toLowerCase() === person.name.toLowerCase() ||
      r.name.toLowerCase() === first,
  )
}

/** The agenda an invite's description holds: its sentences, minus a join link. */
function agendaOf(ev: CalendarEvent): string[] {
  return ev.description
    .split(/\r?\n|(?<=[.!?])\s+/)
    .map((line) => line.trim())
    .filter((line) => line && !/https?:\/\//.test(line))
}

/** The first of a person's names, which is what a line about them reads best with. */
function firstName(name: string): string {
  return name.split(/\s+/)[0]!
}

/** Put a brief together for an event, from the mock's own tasks. */
function make(ev: CalendarEvent, tasks: readonly Task[]): MeetingBrief {
  const people = peopleOf(ev)
  const talkingPoints: TalkingPoint[] = []
  for (const task of tasks) {
    if (task.status === 'cancelled') continue
    const who = people.find((p) => raisedWith(task, p))
    if (who) {
      talkingPoints.push({
        taskId: task.id,
        text: task.title,
        with: who.name,
        done: task.status === 'done',
      })
    }
  }

  const written = WRITTEN[ev.title]
  const sources: MeetingBriefSource[] = [{ kind: 'invite', id: ev.id, title: ev.title }]
  for (const p of people) {
    if (p.contact) sources.push({ kind: 'contact', id: p.contact, title: p.name })
  }
  if (written) {
    sources.push({ kind: 'mail', id: 'mail-thread-1', title: written.mail ?? ev.title })
    sources.push({ kind: 'note', id: 'note-type-scale', title: 'Type scale options' })
    sources.push({ kind: 'meeting', id: 'rec-last-review', title: `${ev.title}, last time` })
  }
  for (const t of talkingPoints) sources.push({ kind: 'task', id: t.taskId, title: t.text })
  const outside = people.find((p) => p.email && !p.email.endsWith('@example.com'))
  if (outside?.email) {
    const org = outside.email.split('@')[1]!
    sources.push({ kind: 'web', id: `https://www.${org}/about`, title: org.split('.')[0]! })
  }

  const names = people.map((p) => firstName(p.name))
  return {
    key: `event:${ev.calendarId}:${ev.uid}`,
    eventId: ev.id,
    calendarId: ev.calendarId,
    uid: ev.uid,
    title: ev.title,
    start: ev.start,
    end: ev.end,
    ...(ev.location ? { location: ev.location } : {}),
    summary:
      written?.summary ?? (names.length ? `${ev.title} with ${names.join(', ')}.` : ev.title),
    open: written?.open ?? [],
    agenda: agendaOf(ev),
    people,
    context: written?.context ?? [],
    talkingPoints,
    sources,
    written: true,
    gaps: written ? ['Mail on your Work account was not read: the assistant may not read it.'] : [],
    preparedAt: new Date().toISOString(),
    tz: 'UTC',
  }
}

/**
 * `meeting_brief`: what has been prepared for an event, or `null`.
 *
 * Answers from the cache as it was made, `done` flags and all, the way the
 * service's would: a task ticked since is for the interface to notice.
 */
export function mockMeetingBrief(
  calendarId: string,
  uid: string,
  tasks: readonly Task[],
): MeetingBrief | null {
  const key = `event:${calendarId}:${uid}`
  let brief = prepared.get(key)
  if (!brief && uid.startsWith('evt-offer-')) {
    // The offer `mockTriggerMeetingOffer` raises: not an event in the mock
    // calendar, so it is described here. Starts now, as its offer says.
    const start = new Date()
    brief = make(
      {
        id: uid.replace(/^evt-offer-/, ''),
        calendarId,
        uid,
        title: 'Design sync',
        description:
          'Weekly design sync.\nReview the type scale options\nHand over the pages for Northwind',
        location: '',
        start: start.toISOString(),
        end: new Date(start.getTime() + 30 * 60_000).toISOString(),
        localDate: start.toISOString().slice(0, 10),
        endDate: start.toISOString().slice(0, 10),
        tz: 'UTC',
        allDay: false,
        status: 'confirmed',
        organizer: 'priya@example.com',
        attendees: ['Priya Raman <priya@example.com>', 'Josh Okafor <josh@northwind.example>'],
        url: '',
        busy: true,
        updatedAt: start.toISOString(),
      },
      tasks,
    )
    prepared.set(key, brief)
  }
  return brief ? structuredClone(brief) : null
}

/** `prepare_meeting_brief`: make one now, as the service would after a few seconds' work. */
export async function mockPrepareMeetingBrief(
  eventId: string,
  events: readonly CalendarEvent[],
  tasks: readonly Task[],
): Promise<MeetingBrief> {
  const ev = events.find((e) => e.id === eventId)
  if (!ev) throw new VaultError('not_found', `event ${eventId} not found`)
  await new Promise((resolve) => setTimeout(resolve, 1200))
  const brief = make(ev, tasks)
  prepared.set(brief.key, brief)
  return structuredClone(brief)
}
