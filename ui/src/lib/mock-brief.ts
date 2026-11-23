// A brief for the demo build: what the morning and evening brief would say
// about a plausible day, with the digest the pictures are drawn from. Built
// against the clock it is given, so the day it describes is always today.
// The daily podcast is here too: a script, and an audio file the mock's play
// address serves as a minute of silence.
//
// Imports types alone, as the other mock modules do: `mock.ts` loads this.

import type {
  Brief,
  BriefDay,
  BriefDigest,
  BriefKind,
  BriefSection,
  Routine,
  RoutineRun,
  RoutineRunId,
  Script,
  BriefWeatherDay,
} from './types'

/** `2026-11-21` for `at`'s own day. */
function dayOf(at: Date): string {
  const p = (n: number) => String(n).padStart(2, '0')
  return `${at.getFullYear()}-${p(at.getMonth() + 1)}-${p(at.getDate())}`
}

/** An instant on `day` at `h:m`, local. */
function on(day: Date, h: number, m = 0): string {
  const at = new Date(day)
  at.setHours(h, m, 0, 0)
  return at.toISOString()
}

function daysAgo(now: Date, n: number): string {
  const at = new Date(now)
  at.setDate(at.getDate() - n)
  return dayOf(at)
}

/** What the morning brief covers in the demo, in order. */
export const MOCK_SECTIONS: Record<BriefKind, BriefSection[]> = {
  morning: [
    'schedule',
    'people',
    'tasks',
    'mail',
    'weather',
    'goals',
    'occasions',
    'tidbit',
    'reading',
  ],
  evening: ['review', 'schedule', 'people', 'tasks', 'mail', 'weather', 'goals'],
  weekly: [
    'review',
    'week',
    'schedule',
    'tasks',
    'people',
    'goals',
    'weather',
    'occasions',
    'mail',
    'tidbit',
  ],
  // The podcast is made from the Feed's sources, not from the day's sections.
  podcast: [],
}

/** What each brief's routine is called. */
export const MOCK_ROUTINE_NAMES: Record<BriefKind, string> = {
  morning: 'Morning brief',
  evening: 'Evening brief',
  weekly: 'Weekly brief',
  podcast: 'Daily podcast',
}

/**
 * The routine `set_brief` makes the first time a brief is switched on: off,
 * at the hour it usually comes. The podcast carries what it is made from --
 * every source, a quarter of an hour, the one narrator.
 */
export function mockBriefRoutine(id: string, kind: BriefKind, at = new Date()): Routine {
  const when = at.toISOString()
  const trigger: Routine['trigger'] =
    kind === 'weekly'
      ? { type: 'schedule', at: '08:00', days: ['sun'] }
      : {
          type: 'schedule',
          at: kind === 'morning' ? '07:00' : kind === 'evening' ? '20:00' : '06:00',
          days: [],
        }
  return {
    id,
    name: MOCK_ROUTINE_NAMES[kind],
    instructions: '',
    trigger,
    graceMinutes: kind === 'evening' ? 180 : kind === 'weekly' || kind === 'podcast' ? 360 : 240,
    enabled: false,
    kind: {
      type: 'brief',
      brief: kind,
      sections: MOCK_SECTIONS[kind],
      length: kind === 'weekly' ? 'long' : 'standard',
      ...(kind === 'podcast'
        ? { podcast: { sources: [], minutes: 15, speakers: [{ id: 'narrator', name: '' }] } }
        : {}),
    },
    delivery: { ask: false },
    maxSteps: 8,
    createdAt: when,
    updatedAt: when,
  }
}

/** The week ahead after `now`, for the weekly brief. */
function weekAfter(now: Date): BriefDay[] {
  const plan: [number, [number, number, string, number, string?][], string[], string[]][] = [
    [
      1,
      [
        [9, 30, 'Team stand-up', 3],
        [10, 0, 'Board meeting', 8, 'Room 4'],
        [14, 0, 'Acme renewal', 2, 'Acme HQ'],
        [16, 0, 'Hiring sync', 3],
      ],
      [],
      ['Send the grant report'],
    ],
    [
      2,
      [
        [9, 30, 'Team stand-up', 3],
        [13, 0, 'Lunch with Jo', 1],
      ],
      [],
      [],
    ],
    [3, [[7, 15, 'Train to Lisbon', 0]], ['Lisbon offsite'], ['Book the offsite dinner']],
    [4, [], ['Lisbon offsite'], []],
    [
      5,
      [
        [9, 30, 'Team stand-up', 3],
        [11, 0, '1:1 with Ana', 1],
        [15, 0, 'Quarterly planning', 6],
      ],
      [],
      ['Draft Q1 plan'],
    ],
    [6, [[10, 0, 'Parkrun', 0]], [], []],
    [7, [], [], []],
  ]
  return plan.map(([ahead, events, allDay, due]) => {
    const day = new Date(now)
    day.setDate(day.getDate() + ahead)
    const timed = events.map(([h, m, title, guests, location]) => ({
      title,
      start: on(day, h, m),
      end: on(day, h + 1, m),
      guests,
      location: location ?? '',
      color: guests > 0 ? '#2563eb' : '#db2777',
    }))
    return {
      date: dayOf(day),
      meetings: timed.filter((e) => e.guests > 0).length,
      minutes: timed.filter((e) => e.guests > 0).length * 60,
      allDay,
      events: timed,
      due,
    }
  })
}

function outlookAfter(now: Date): BriefWeatherDay[] {
  const days: [string, number, number, number][] = [
    ['Light rain', 11, 5, 80],
    ['Overcast', 10, 4, 30],
    ['Sunny', 18, 11, 0],
    ['Partly cloudy', 19, 12, 10],
    ['Rain', 9, 6, 90],
    ['Clear', 8, 1, 0],
    ['Fog', 7, 2, 20],
  ]
  return days.map(([condition, high, low, rainChance], i) => {
    const day = new Date(now)
    day.setDate(day.getDate() + i + 1)
    return { date: dayOf(day), condition, high, low, rainChance }
  })
}

function digestFor(kind: BriefKind, now: Date): BriefDigest {
  const ahead = new Date(now)
  if (kind === 'evening') ahead.setDate(ahead.getDate() + 1)
  const until = new Date(now)
  until.setDate(until.getDate() + 7)
  const weekly = kind === 'weekly'
  return {
    kind,
    day: dayOf(now),
    ahead: dayOf(ahead),
    until: weekly ? dayOf(until) : null,
    week: weekly ? weekAfter(now) : [],
    zone: Intl.DateTimeFormat().resolvedOptions().timeZone,
    madeAt: now.toISOString(),
    firstName: 'Hari',
    sections: MOCK_SECTIONS[kind],
    events: [
      {
        id: 'ev-standup',
        calendarId: 'cal-work',
        title: 'Team stand-up',
        start: on(ahead, 9, 30),
        end: on(ahead, 9, 45),
        allDay: false,
        location: '',
        color: '#2563eb',
        calendar: 'Work',
        guests: ['Tom Li', 'Ana Ruiz', 'Sam Okafor'],
        organizer: 'Tom Li',
        busy: true,
        video: true,
        clash: false,
      },
      {
        id: 'ev-acme',
        calendarId: 'cal-work',
        title: 'Acme renewal',
        start: on(ahead, 14, 0),
        end: on(ahead, 15, 0),
        allDay: false,
        location: 'Acme HQ, 1 Market St',
        color: '#2563eb',
        calendar: 'Work',
        guests: ['Priya Shah', 'Tom Li'],
        organizer: 'Priya Shah',
        busy: true,
        video: false,
        clash: true,
      },
      {
        id: 'ev-dentist',
        calendarId: 'cal-home',
        title: 'Dentist',
        start: on(ahead, 14, 45),
        end: on(ahead, 15, 30),
        allDay: false,
        location: 'Harbour Dental',
        color: '#db2777',
        calendar: 'Home',
        guests: [],
        organizer: '',
        busy: true,
        video: false,
        clash: true,
      },
      {
        id: 'ev-gym',
        calendarId: 'cal-home',
        title: 'Climbing with Sam',
        start: on(ahead, 18, 0),
        end: on(ahead, 19, 30),
        allDay: false,
        location: 'The Boulder Shed',
        color: '#db2777',
        calendar: 'Home',
        guests: ['Sam Okafor'],
        organizer: '',
        busy: true,
        video: false,
        clash: false,
      },
    ],
    blocks: [
      {
        title: 'Grant report',
        start: on(ahead, 10, 30),
        end: on(ahead, 12, 0),
        purpose: 'Win the research grant',
      },
    ],
    free: [
      { start: on(ahead, 8, 0), end: on(ahead, 9, 30), minutes: 90 },
      { start: on(ahead, 12, 0), end: on(ahead, 14, 0), minutes: 120 },
      { start: on(ahead, 15, 30), end: on(ahead, 18, 0), minutes: 150 },
    ],
    people: [
      {
        name: 'Priya Shah',
        email: 'priya@acme.example',
        meetings: ['Acme renewal'],
        nextAt: on(ahead, 14, 0),
        relationship: 'Head of operations at Acme, our biggest client',
        company: 'Acme',
        circle: 'Work',
        lastContact: daysAgo(now, 19),
        lastNote: {
          title: 'Acme quarterly review',
          day: daysAgo(now, 40),
          excerpt:
            'Priya asked for a three-year price lock and a named support contact; we said we would come back with numbers before the renewal.',
        },
        waitingOnYou: true,
      },
      {
        name: 'Tom Li',
        email: 'tom@example.com',
        meetings: ['Team stand-up', 'Acme renewal'],
        nextAt: on(ahead, 9, 30),
        relationship: 'Engineering lead on your team',
        company: '',
        circle: 'Work',
        lastContact: daysAgo(now, 1),
        lastNote: null,
        waitingOnYou: false,
      },
      {
        name: 'Sam Okafor',
        email: 'sam@example.com',
        meetings: ['Team stand-up', 'Climbing with Sam'],
        nextAt: on(ahead, 9, 30),
        relationship: 'Friend from university',
        company: '',
        circle: 'Close friends',
        lastContact: daysAgo(now, 12),
        lastNote: null,
        waitingOnYou: false,
      },
    ],
    due: [
      {
        id: 'task-grant',
        title: 'Send the grant report to Dr Mehta',
        due: dayOf(ahead),
        dueTime: '17:00:00',
        priority: 'high',
        purpose: 'Win the research grant',
        estimateMinutes: 90,
      },
      {
        id: 'task-pricing',
        title: 'Draft Acme pricing options',
        due: dayOf(ahead),
        priority: 'medium',
        purpose: 'Work',
      },
    ],
    overdue: [
      {
        id: 'task-expenses',
        title: 'File October expenses',
        due: daysAgo(now, 9),
        priority: 'none',
        purpose: '',
      },
    ],
    overdueCount: 3,
    done:
      kind !== 'morning'
        ? [
            {
              id: 'task-slides',
              title: 'Finish the board slides',
              priority: 'high',
              purpose: 'Work',
            },
            { id: 'task-call', title: 'Call the plumber', priority: 'none', purpose: '' },
          ]
        : [],
    met: kind === 'evening' ? ['09:30 Team stand-up', '14:00 Acme renewal'] : [],
    karmaToday: kind === 'evening' ? 35 : weekly ? 240 : null,
    mail: [
      {
        threadId: 'th-renewal',
        subject: 'Renewal terms for next year',
        with: 'Priya Shah',
        waiting: 'onYou',
        since: on(new Date(now.getTime() - 3 * 86_400_000), 16, 20),
      },
      {
        threadId: 'th-venue',
        subject: 'Venue for the offsite',
        with: 'Ana Ruiz',
        waiting: 'onYou',
        since: on(new Date(now.getTime() - 86_400_000), 11, 5),
      },
      {
        threadId: 'th-invoice',
        subject: 'Invoice 42',
        with: 'Northwind Accounts',
        waiting: 'onThem',
        since: on(new Date(now.getTime() - 6 * 86_400_000), 9, 0),
      },
    ],
    unreadImportant: 4,
    weather: {
      place: 'London',
      temperatureUnit: '°C',
      windUnit: 'km/h',
      precipitationUnit: 'mm',
      now: kind === 'morning' ? 6 : null,
      nowCondition: kind === 'morning' ? 'Overcast' : '',
      condition: 'Light rain',
      high: 11,
      low: 5,
      rainChance: 80,
      rain: 3.2,
      wind: 24,
      gusts: 41,
      sunrise: '07:21',
      sunset: '16:02',
      outlook: weekly ? outlookAfter(now) : [],
    },
    goals: [
      {
        trackerId: 'tr-run',
        name: 'Running',
        color: '#0d9488',
        icon: 'running',
        target: 'at least 3 days a week',
        value: 1,
        aim: 3,
        limit: false,
        from: daysAgo(now, 4),
        to: dayOf(new Date(now.getTime() + 2 * 86_400_000)),
        standing: 'short',
        onPace: 2,
      },
      {
        trackerId: 'tr-read',
        name: 'Reading',
        color: '#7c3aed',
        icon: 'book',
        target: 'at least 140 pages a week',
        value: 96,
        aim: 140,
        limit: false,
        from: daysAgo(now, 4),
        to: dayOf(new Date(now.getTime() + 2 * 86_400_000)),
        standing: 'within',
        onPace: 100,
      },
      {
        trackerId: 'tr-coffee',
        name: 'Coffee',
        color: '#b45309',
        icon: 'coffee',
        target: 'at most 2 cups a day',
        value: 1,
        aim: 2,
        limit: true,
        from: dayOf(now),
        to: dayOf(now),
        standing: 'within',
      },
    ],
    activeGoals: [
      'Win the research grant (Work)',
      'Learn Spanish (Myself)',
      'Run a half marathon (Health)',
    ],
    occasions: [
      {
        name: 'Mum',
        what: 'birthday',
        date: dayOf(new Date(now.getTime() + 3 * 86_400_000)),
        lastContact: daysAgo(now, 8),
        daysOver: 0,
        circle: '',
      },
      {
        name: 'Jo Bennett',
        what: 'catchUp',
        lastContact: daysAgo(now, 75),
        daysOver: 15,
        circle: 'Friends',
      },
    ],
    stories: [
      {
        storyId: 'st-1',
        title: 'What a decade of grant reviews says about winning proposals',
        source: 'Nature Careers',
        url: 'https://example.com/grants',
        publishedAt: on(now, 6, 10),
        summary:
          'Reviewers reward a single clear aim over breadth, an analysis of 4,000 reviews finds.',
      },
      {
        storyId: 'st-2',
        title: 'The quiet revolution in half-marathon training plans',
        source: 'Runner’s World',
        url: 'https://example.com/running',
        publishedAt: on(now, 5, 0),
        summary: 'Fewer, easier miles and one long run a week are beating high-volume plans.',
      },
      {
        storyId: 'st-3',
        title: 'Spanish learners: why listening first works',
        source: 'The Linguist',
        url: 'https://example.com/spanish',
        publishedAt: on(now, 4, 30),
        summary: 'Comprehensible input before speaking speeds up fluency for adults.',
      },
    ],
    previous: null,
    gaps: [],
  }
}

const MORNING_PARTS = (now: Date) => [
  {
    section: 'schedule' as const,
    title: 'Your day',
    body: `- **9:30** Team stand-up (15 min, video)
- **10:30** Grant report, the time you set aside
- **2 pm** Acme renewal at Acme HQ, with Priya and Tom
- **2:45** Dentist, which **clashes with the renewal**
- **6 pm** Climbing with Sam

The renewal and the dentist overlap by fifteen minutes. Move the dentist or leave the renewal early, and decide before lunch. The stretch from **12 to 2** is free; it is the last quiet time before the afternoon.`,
  },
  {
    section: 'people' as const,
    title: 'Who you’re meeting',
    body: `**Priya Shah** runs operations at Acme. At the quarterly review on ${new Date(now.getTime() - 40 * 86_400_000).toLocaleDateString(undefined, { day: 'numeric', month: 'long' })} she asked for a three-year price lock and a named support contact, and you promised numbers before the renewal. Her mail about the terms has waited three days for a reply.

**Tom Li** is on both the stand-up and the renewal; you spoke yesterday.

**Sam Okafor** is climbing with you at six. You last caught up twelve days ago.`,
  },
  {
    section: 'tasks' as const,
    title: 'Due today',
    body: `- **Send the grant report to Dr Mehta** by 5 pm, about an hour and a half; the 10:30 block fits it.
- Draft the Acme pricing options, ideally before the 2 pm.

Three things are overdue; **File October expenses** is the oldest, nine days late.`,
  },
  {
    section: 'mail' as const,
    title: 'Waiting on you',
    body: `- **Priya Shah**, "Renewal terms for next year", three days. Answer before the meeting.
- Ana Ruiz, "Venue for the offsite", since yesterday.
- Northwind Accounts have not answered about Invoice 42 in six days; worth a nudge.`,
  },
  {
    section: 'weather' as const,
    title: 'Weather',
    body: 'A grey 6° now, with **rain likely from mid-afternoon**: take an umbrella to Acme. It is dark by four.',
  },
  {
    section: 'goals' as const,
    title: 'Goals',
    body: 'Running is at **one of three** this week, one behind pace. A short run in the 12–2 gap would put you back on track. Reading is on pace at 96 of 140 pages.',
  },
  {
    section: 'occasions' as const,
    title: 'People to remember',
    body: '**Mum’s birthday** is in three days. You have not caught up with **Jo Bennett** in over two months; a message this week would do.',
  },
  {
    section: 'tidbit' as const,
    title: 'Something to know',
    body: 'For a half marathon, most of the gain comes from easy running: about **80% of training at a conversational pace** is what most elite plans use.',
  },
  {
    section: 'reading' as const,
    title: 'Worth reading',
    body: `- **What a decade of grant reviews says about winning proposals** (Nature Careers): one clear aim beats breadth, worth a look before you send the report.
- **The quiet revolution in half-marathon training plans** (Runner’s World): fewer, easier miles.`,
  },
]

const EVENING_PARTS = [
  {
    section: 'review' as const,
    title: 'How today went',
    body: 'You **finished the board slides** and got the plumber booked, and the Acme renewal went ahead. The grant report slipped; it is now due tomorrow at five.',
  },
  {
    section: 'schedule' as const,
    title: 'Tomorrow',
    body: `- **9:30** Team stand-up
- **2 pm** Acme renewal, which **clashes with the dentist** at 2:45
- **6 pm** Climbing with Sam

Bring your climbing shoes; pack them tonight.`,
  },
  {
    section: 'tasks' as const,
    title: 'Due tomorrow',
    body: '- **Send the grant report** by 5 pm. The 10:30 block is the time for it.',
  },
]

const WEEKLY_PARTS = [
  {
    section: 'review' as const,
    title: 'Last week',
    body: 'You **finished the board slides** and closed eleven tasks, and Karma came to 240, your best week this month. The grant report slipped and is due on Monday.',
  },
  {
    section: 'week' as const,
    title: 'The week ahead',
    body: `- **Monday is the big one**: the board meeting at 10, the Acme renewal at 2, and the grant report due by 5.
- **Tuesday** is light; lunch with Jo at 1.
- **Wednesday and Thursday** are the Lisbon offsite. The train leaves at **7:15** on Wednesday.
- **Friday**: quarterly planning at 3, and the Q1 plan is due.
- The weekend is clear.

Set up Monday today: the grant report needs about ninety minutes and **Monday has no gap for it**.`,
  },
  {
    section: 'schedule' as const,
    title: 'Today',
    body: 'Nothing on the calendar. A good day to get the grant report done, and to pack for Lisbon.',
  },
  {
    section: 'tasks' as const,
    title: 'Due this week',
    body: `- **Send the grant report** (Monday by 5)
- Book the offsite dinner (Wednesday)
- Draft the Q1 plan (Friday)

Three things are overdue; File October expenses is the oldest.`,
  },
  {
    section: 'people' as const,
    title: 'Who you’ll see this week',
    body: '**Priya Shah** on Monday at the renewal: she is waiting on your reply about the terms and asked last time for a three-year price lock. **Jo Bennett** on Tuesday: you have not caught up since September.',
  },
  {
    section: 'goals' as const,
    title: 'Goals this week',
    body: 'Three runs is realistic if you run today, Tuesday and Saturday; Lisbon will take Wednesday and Thursday.',
  },
  {
    section: 'weather' as const,
    title: 'The week’s weather',
    body: 'Rain today and again on Friday. **Lisbon is sunny and 18 to 19°**; pack light.',
  },
  {
    section: 'occasions' as const,
    title: 'People to remember',
    body: '**Mum’s birthday** is on Wednesday, while you are in Lisbon: send something tomorrow.',
  },
]

/** What the narrator says, a paragraph a line: the demo's daily podcast. */
const PODCAST_LINES = [
  'Good morning, Hari. This is your podcast for today: four stories from the shows and feeds you follow, about fifteen minutes.',
  'First, the winter ferry timetable. Island Life reports that the Thursday boat to the outer islands stops from the first of December until March, so anything you want to send out has to go on Wednesday.',
  'Second, the tides. The Slow Water podcast spent half an hour with a harbour master who still reads them from a notebook, and the part worth your time is the story of the spring tide that lifted a trawler onto the quay.',
  'Third, from the newsletters. The Sunday Reader has a long piece on rope: why a three-strand line still beats most modern braids for mooring, and what the old sailmakers knew about stretch.',
  'Last, something short from Hacker News: a small team has rebuilt an old chart plotter on a single board, and the thread is better than the article.',
  "That's all for today. Your run is at seven; the first story will be a good one to start with. Have a good one.",
]

const PODCAST_PARTS = [
  {
    section: null,
    title: 'In this episode',
    body: `- **The winter ferry timetable**, from Island Life. The Thursday boat stops from 1 December.
- **A harbour master and the tides**, from Slow Water. Half an hour; the trawler on the quay is at the twenty minute mark.
- **Why a three-strand line still wins**, from The Sunday Reader.
- **A chart plotter on a single board**, from Hacker News.`,
  },
]

/** The podcast's script: one narrator, a line a paragraph. */
function podcastScript(now: Date): Script {
  return {
    title: `Daily podcast, ${dayOf(now)}`,
    speakers: [{ id: 'narrator', name: '' }],
    lines: PODCAST_LINES.map((text) => ({ speaker: 'narrator', text })),
  }
}

/** The audio file the podcast was rendered to: the mock's play address serves it as silence. */
export const MOCK_PODCAST_AUDIO = 'file-daily-podcast'

const SPOKEN: Record<BriefKind, string> = {
  podcast: PODCAST_LINES.join('\n\n'),
  weekly: `Good morning, Hari. Here's your week.

Last week went well: the board slides are done and you closed eleven tasks. The grant report slipped, and it is due tomorrow at five.

Tomorrow is the big day: the board meeting at ten, the Acme renewal at two, and the report. There is no gap for the report tomorrow, so do it today. Tuesday is light. Wednesday and Thursday are the Lisbon offsite, and the train leaves at a quarter past seven. Friday is quarterly planning.

Mum's birthday is on Wednesday while you are away, so send something tomorrow. Lisbon will be sunny. Enjoy your Sunday.`,
  morning: `Good morning, Hari. It's a full afternoon after a quiet morning, and the grant report is due by five.

Three things matter today. First, the Acme renewal at two, with Priya. She is still waiting on your reply about the terms, so answer her before lunch. Second, your dentist appointment at a quarter to three clashes with the renewal, so move one of them this morning. Third, the grant report: you set aside half past ten for it, which is about right.

It will rain from mid-afternoon, so take an umbrella to Acme. Running is one behind for the week, and the gap between twelve and two would fit a short run.

Mum's birthday is in three days. Have a good day.`,
  evening: `Good evening, Hari. A good day: the board slides are done and the plumber is booked.

The grant report slipped to tomorrow, and it is due at five. Tomorrow starts with the stand-up at half past nine, and the Acme renewal at two still clashes with the dentist.

Pack your climbing shoes tonight. Sleep well.`,
}

const HEADLINE: Record<BriefKind, string> = {
  podcast:
    'Four stories from what you follow: **the winter ferry timetable**, a harbour master on the tides, and a long one on rope.',
  weekly:
    'A front-loaded week: **Monday has the board, the renewal and the grant report**, then two days in Lisbon.',
  morning:
    'A quiet morning and a full afternoon: **the grant report is due by 5**, and the Acme renewal clashes with the dentist.',
  evening:
    'The slides are done; **the grant report slipped to tomorrow at 5**, and tomorrow’s renewal still clashes with the dentist.',
}

/** A brief of `kind` about the day `now` falls on. */
export function mockBrief(kind: BriefKind, now = new Date()): Brief {
  const parts =
    kind === 'morning'
      ? MORNING_PARTS(now)
      : kind === 'weekly'
        ? WEEKLY_PARTS
        : kind === 'podcast'
          ? PODCAST_PARTS
          : EVENING_PARTS
  return {
    kind,
    day: dayOf(now),
    headline: HEADLINE[kind],
    parts,
    spoken: SPOKEN[kind],
    // The podcast is its script, and the audio rendered from it.
    ...(kind === 'podcast' ? { script: podcastScript(now), audio: MOCK_PODCAST_AUDIO } : {}),
    digest: digestFor(kind, now),
  }
}

/** The written brief as one Markdown document: what the run's summary holds. */
export function mockBriefMarkdown(brief: Brief): string {
  return [`# ${brief.headline}`, ...brief.parts.map((p) => `## ${p.title}\n\n${p.body}`)].join(
    '\n\n',
  )
}

/** A finished run of a brief routine, holding `brief`. */
export function mockBriefRun(
  id: RoutineRunId,
  routineId: string,
  brief: Brief,
  at: Date,
): RoutineRun {
  return {
    id,
    routineId,
    routineName: MOCK_ROUTINE_NAMES[brief.kind],
    startedAt: at.toISOString(),
    finishedAt: at.toISOString(),
    outcome: 'done',
    reason: '',
    summary: mockBriefMarkdown(brief),
    seen: false,
    steps: 3,
    brief,
  }
}

/** What the player says for a brief: its spoken version, a sentence at a time. */
export function mockBriefSentences(kind: BriefKind = 'morning'): string[] {
  return SPOKEN[kind]
    .split(/(?<=[.!?])\s+/)
    .map((s) => s.trim())
    .filter(Boolean)
}
