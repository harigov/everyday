// Behaviour checks for the daily briefs' rules: the section catalogue the
// settings edit, and the arithmetic the brief page draws with -- the day at a
// glance, its lanes, and the counts along the top. See docs/plans/briefs.md.

import { load, makeCheck } from './harness.mjs'

const { module: brief, close } = await load('/src/lib/brief.ts')
const {
  DEFAULT_SECTIONS,
  KIND_INFO,
  briefSpec,
  clampMinutes,
  goalFraction,
  listeningMinutes,
  meetingLoad,
  moveSection,
  paceFraction,
  podcastChoices,
  podcastOf,
  podcastPlayRequest,
  podcastTitle,
  scriptMinutes,
  sectionTitle,
  sectionsFor,
  sourceKindLabel,
  statsOf,
  timelineOf,
  toggleSection,
  toggleSource,
  at,
} = brief

const { check, ok, finish } = makeCheck()

// ── The catalogue mirrors the core ─────────────────────────────────────

check('the morning has no review', sectionsFor('morning').includes('review'), false)
check('the evening does', sectionsFor('evening').includes('review'), true)
check('a heading reads as the core writes it', sectionTitle('schedule', 'evening'), 'Tomorrow')
check('and in the morning', sectionTitle('schedule', 'morning'), 'Your day')
check('the morning defaults start with the day', DEFAULT_SECTIONS.morning[0], 'schedule')
check('the evening defaults start with the review', DEFAULT_SECTIONS.evening[0], 'review')

check(
  'moving a section up swaps it with the one before',
  moveSection(['schedule', 'people', 'tasks'], 'tasks', -1),
  ['schedule', 'tasks', 'people'],
)
check('the first cannot go further up', moveSection(['schedule', 'people'], 'schedule', -1), [
  'schedule',
  'people',
])
check(
  'a section switched back on goes where the default order puts it',
  toggleSection(['schedule', 'tasks', 'goals'], 'people', true, 'morning'),
  ['schedule', 'people', 'tasks', 'goals'],
)
check(
  'switching off takes it out',
  toggleSection(['schedule', 'people'], 'people', false, 'morning'),
  ['schedule'],
)

// ── The day at a glance ─────────────────────────────────────────────────

const day = '2026-11-21'
const t = (h, m = 0) =>
  new Date(`${day}T${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}:00`).toISOString()
const event = (title, from, to, extra = {}) => ({
  id: title,
  calendarId: 'c',
  title,
  start: t(...from),
  end: t(...to),
  allDay: false,
  location: '',
  color: '#2563eb',
  calendar: 'Work',
  guests: [],
  organizer: '',
  busy: true,
  video: false,
  clash: false,
  ...extra,
})
const digest = (extra = {}) => ({
  kind: 'morning',
  day,
  ahead: day,
  zone: 'UTC',
  firstName: 'Hari',
  sections: ['schedule', 'tasks', 'weather', 'mail'],
  events: [],
  blocks: [],
  free: [],
  people: [],
  due: [],
  overdue: [],
  overdueCount: 0,
  done: [],
  met: [],
  mail: [],
  unreadImportant: 0,
  goals: [],
  activeGoals: [],
  occasions: [],
  stories: [],
  gaps: [],
  ...extra,
})

check('an empty day draws nothing', timelineOf(digest()), null)

const overlapping = digest({
  events: [
    event('Renewal', [14], [15], { clash: true }),
    event('Dentist', [14, 30], [15, 30], { clash: true }),
    event('Stand-up', [9, 30], [9, 45]),
    event('Holiday', [0], [0], { allDay: true }),
  ],
  blocks: [{ title: 'Report', start: t(10, 30), end: t(12), purpose: '' }],
})
const line = timelineOf(overlapping, Date.parse(t(11)))
check('the window is eight to eight', [line.from, line.to], [Date.parse(t(8)), Date.parse(t(20))])
check('one tick an hour', line.hours.length, 13)
check(
  'all-day events are apart',
  line.allDay.map((e) => e.title),
  ['Holiday'],
)
check(
  'overlapping items take two lanes, the rest reuse the first',
  line.items.map((i) => [i.title, i.lane]),
  [
    ['Stand-up', 0],
    ['Report', 0],
    ['Renewal', 0],
    ['Dentist', 1],
  ],
)
check('two lanes in all', line.lanes, 2)
check('now is inside the window', line.now, Date.parse(t(11)))
check('a position is a percentage of the window', at(line, Date.parse(t(14))), 50)

const early = timelineOf(digest({ events: [event('Flight', [6, 15], [7, 30])] }))
check('an early start widens the window to the hour', early.from, Date.parse(t(6)))

// ── Counts ───────────────────────────────────────────────────────────────

check('meeting load counts timed, busy events', meetingLoad(overlapping), {
  meetings: 3,
  minutes: 135,
})
const stats = statsOf(
  digest({
    ...overlapping,
    sections: ['schedule', 'tasks', 'mail', 'weather'],
    free: [{ start: t(12), end: t(14), minutes: 120 }],
    due: [{ id: 'a', title: 'A', priority: 'high', purpose: '' }],
    overdueCount: 2,
    mail: [{ threadId: 'x', subject: 'S', with: 'P', waiting: 'onYou', since: t(8) }],
    weather: {
      place: 'London',
      temperatureUnit: '°C',
      windUnit: 'km/h',
      precipitationUnit: 'mm',
      nowCondition: '',
      condition: 'Light rain',
      high: 11.4,
      low: 5,
      rainChance: 80,
      sunrise: '',
      sunset: '',
    },
  }),
)
check(
  'one count per covered section',
  stats.map((s) => s.id),
  ['meetings', 'free', 'tasks', 'mail', 'weather'],
)
check('a clash makes the meetings count a warning', stats[0].warn, true)
check('overdue is said beside what is due', stats[2].label, 'due · 2 overdue')
check('the weather shows its high', stats[4].value, '11°')
check(
  'a section not covered has no count',
  statsOf(digest({ sections: ['tasks'] })).map((s) => s.id),
  ['tasks'],
)

// ── Goals and timing ─────────────────────────────────────────────────────

check('a target is a fraction of its aim', goalFraction({ value: 1, aim: 3 }), 1 / 3)
check('past its aim it is full', goalFraction({ value: 5, aim: 3 }), 1)
check('the pace mark is the even-pace value over the aim', paceFraction({ aim: 4, onPace: 2 }), 0.5)
check('no pace, no mark', paceFraction({ aim: 4 }), null)
check(
  'listening time is at 150 words a minute',
  listeningMinutes({ spoken: 'word '.repeat(450) }),
  3,
)

// ── The daily podcast ────────────────────────────────────────────────────

check('a podcast covers no section', sectionsFor('podcast'), [])
check('and starts with none', DEFAULT_SECTIONS.podcast, [])
check('it is named for what it is', KIND_INFO.podcast.name, 'Daily podcast')

check('too short a podcast is the shortest', clampMinutes(2), 5)
check('too long is the longest', clampMinutes(90), 60)
check('a fraction is a whole number of minutes', clampMinutes(22.4), 22)
check('no number at all is the usual length', clampMinutes(Number.NaN), 15)

check('a spec with no podcast says every source, a quarter of an hour', podcastOf({}), {
  sources: [],
  minutes: 15,
  speakers: [],
})
check(
  'what a spec says is kept, the narrator with it',
  podcastOf({
    podcast: { sources: ['a', 'b'], minutes: 30, speakers: [{ id: 'narrator', name: '' }] },
  }),
  { sources: ['a', 'b'], minutes: 30, speakers: [{ id: 'narrator', name: '' }] },
)

check('choosing a source adds it', toggleSource(['a'], 'b', true), ['a', 'b'])
check('choosing one twice keeps it once', toggleSource(['a', 'b'], 'b', true), ['a', 'b'])
check('letting go takes it out', toggleSource(['a', 'b'], 'a', false), ['b'])
check('letting go of what is not there changes nothing', toggleSource(['a'], 'z', false), ['a'])

const web = { type: 'web', url: 'https://example.com/feed' }
check('a show is a podcast', sourceKindLabel({ kind: web, podcast: { speed: null } }), 'Podcast')
check(
  'a sender is a newsletter',
  sourceKindLabel({ kind: { type: 'mail', sender: 'a@b.c' } }),
  'Newsletter',
)
check('a site is a feed', sourceKindLabel({ kind: web }), 'Feed')
check(
  'podcasts come first, then newsletters, each by title',
  podcastChoices([
    { source: { title: 'Zed feed', kind: web } },
    { source: { title: 'Sunday Reader', kind: { type: 'mail', sender: 'x@y.z' } } },
    { source: { title: 'slow water', kind: web, podcast: {} } },
    { source: { title: 'Alpha feed', kind: web } },
    { source: { title: 'Island Life', kind: web, podcast: {} } },
  ]).map((c) => c.source.title),
  ['Island Life', 'slow water', 'Sunday Reader', 'Alpha feed', 'Zed feed'],
)

const withPodcast = {
  kind: {
    type: 'brief',
    brief: 'podcast',
    sections: [],
    length: 'standard',
    podcast: { sources: ['a'], minutes: 20, speakers: [] },
  },
}
check('the podcast is kept on the spec', briefSpec(withPodcast).podcast.minutes, 20)
ok(
  'no other brief is saved with a podcast key',
  !('podcast' in briefSpec({ kind: { type: 'brief', brief: 'morning', sections: ['schedule'] } })),
)

check(
  'a podcast with no audio cannot be played yet',
  podcastPlayRequest({ day: '2026-11-22' }),
  null,
)
const request = podcastPlayRequest({ day: '2026-11-22', audio: 'file-1' })
check(
  'its audio is played as a file',
  [request.what, request.fileId, request.kind],
  [{ type: 'file', id: 'file-1' }, 'file-1', 'audio'],
)
check('it is shown as the show it is', request.source, 'Daily podcast')
ok('and titled with its day', request.title === podcastTitle('2026-11-22'))
ok('the title starts with the show', podcastTitle('2026-11-22').startsWith('Daily podcast — '))

check('no script, no minutes', scriptMinutes(null), 0)
check('an empty script, none either', scriptMinutes({ lines: [] }), 0)
check('a short one is a minute at least', scriptMinutes({ lines: [{ text: 'Hello there.' }] }), 1)
check(
  'a script is said at 150 words a minute',
  scriptMinutes({ lines: [{ text: 'word '.repeat(200) }, { text: 'word '.repeat(250) }] }),
  3,
)

await close()
finish('brief')
