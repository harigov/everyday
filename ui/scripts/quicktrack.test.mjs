// Behaviour checks for the quick-track grammar.
//
// Same reasoning as `quickadd.test.mjs`: a capture grammar is a set of rules
// whose failures are quiet and expensive. A line that logs against the wrong
// tracker is a number in the wrong year of a chart. A name matched
// case-sensitively makes a second "Swim" beside the first and splits a
// history in two. A `Dose` guessed from `400mg` changes how a day adds up
// with nothing on screen to say so. And a word swallowed by a bad parse is
// the one failure people notice immediately and never forgive.
//
// No test framework, deliberately: one dependency-free file, run by
// `npm run check`, with the TypeScript loaded through Vite so it compiles
// exactly as the application compiles it.

import { load, makeCheck } from './harness.mjs'

const { module: quicktrack, close } = await load('/src/lib/quicktrack.ts')
const { describeQuickTrack, parseQuickTrack, readValue } = quicktrack

const { check, finish } = makeCheck()

const tracker = (over = {}) => ({
  id: 't',
  name: 'Swimming',
  kind: 'amount',
  icon: 'dot',
  color: '#000',
  unit: 'min',
  defaultValue: 30,
  target: null,
  scaleMax: 10,
  onCalendar: false,
  archived: false,
  sortOrder: 0,
  createdAt: '2026-01-01T00:00:00Z',
  updatedAt: '2026-01-01T00:00:00Z',
  ...over,
})

// ── reading a measurement ───────────────────────────────────────────────

check('a bare number is a number', readValue('20'), { value: 20, unit: '' })
check('a decimal survives', readValue('78.4'), { value: 78.4, unit: '' })
check('a unit comes back with it', readValue('400mg'), { value: 400, unit: 'mg' })
// Minutes, so a 45-minute run draws as a span on the calendar and not a dot.
check('minutes are minutes', readValue('60min'), { value: 60, unit: 'min' })
check('hours become minutes', readValue('2h'), { value: 120, unit: 'min' })
check('and so does the compound form', readValue('1h30'), { value: 90, unit: 'min' })
check('...with the m spelled out', readValue('1h30m'), { value: 90, unit: 'min' })
check('a severity carries its own ceiling', readValue('7/10'), {
  value: 7,
  unit: '',
  outOf: 10,
})
check('a percentage keeps its sign', readValue('96%'), { value: 96, unit: '%' })

// Things that are not measurements, and must not be read as one — or the
// last word of a name would be eaten.
check('a word is not a measurement', readValue('floss'), null)
check('...nor is a mixed token', readValue('3x4'), null)
check('...nor a division by nothing', readValue('7/0'), null)
check('...nor an empty string', readValue(''), null)

// ── matching what already exists ────────────────────────────────────────

{
  const all = [tracker()]
  const got = parseQuickTrack('swimming 45min', all)
  check('an existing tracker is found, whatever the case', got.target.kind, 'existing')
  check('...and takes the value given', got.value, 45)
}

{
  // The whole point of matching first: `#swim` on Friday must find the
  // Swimming made on Monday rather than starting a second history.
  const all = [tracker({ name: 'swim' })]
  check(
    'a name is matched case-insensitively',
    parseQuickTrack('SWIM 30', all).target.kind,
    'existing',
  )
}

{
  // An archived tracker is still matched. Logging against something you
  // retired should find it, not make a twin with the same name.
  const all = [tracker({ archived: true })]
  check(
    'an archived tracker is still found',
    parseQuickTrack('swimming 20', all).target.kind,
    'existing',
  )
}

{
  const all = [tracker()]
  check('no number falls back to the default', parseQuickTrack('swimming', all).value, 30)
}

{
  // A check is one however it was written. "floss 3" is not three
  // flossings, and the vault would clamp it anyway.
  const all = [tracker({ name: 'Floss', kind: 'check', defaultValue: 1 })]
  check('a check is always one', parseQuickTrack('floss 3', all).value, 1)
}

// ── creating from a guess ───────────────────────────────────────────────

{
  const got = parseQuickTrack('swim 60min')
  check('an unknown name with minutes is an amount in minutes', got.target, {
    kind: 'new',
    name: 'swim',
    trackerKind: 'amount',
    unit: 'min',
    scaleMax: 10,
  })
  check('...with the value read off the line', got.value, 60)
}

check('a bare name is a habit', parseQuickTrack('floss').target, {
  kind: 'new',
  name: 'floss',
  trackerKind: 'check',
  unit: '',
  scaleMax: 10,
})

check('n out of m is a scale with that ceiling', parseQuickTrack('mood 7/10').target, {
  kind: 'new',
  name: 'mood',
  trackerKind: 'scale',
  unit: '',
  scaleMax: 10,
})
check('...and an unusual ceiling is honoured', parseQuickTrack('pain 3/5').target.scaleMax, 5)

// A dose and an amount store the same number and differ only in how a day
// adds up, so guessing wrong is invisible until a chart is drawn. `400mg`
// is an amount in mg, which is true; a dose is an interpretation.
check('a dose is never guessed', parseQuickTrack('ibuprofen 400mg').target.trackerKind, 'amount')
check('...and keeps its unit', parseQuickTrack('ibuprofen 400mg').target.unit, 'mg')

// A name of several words is ordinary, and only the last word is a
// candidate measurement.
{
  const got = parseQuickTrack('resting heart rate 58')
  check('a multi-word name survives', got.target.name, 'resting heart rate')
  check('...and its number is read', got.value, 58)
}
check(
  'a name whose last word is a word keeps it',
  parseQuickTrack('walk the dog').target.name,
  'walk the dog',
)

// ── what it refuses to do ───────────────────────────────────────────────

check('an empty line names nothing', parseQuickTrack('').target, null)
check('...and neither does whitespace', parseQuickTrack('   ').target, null)

{
  // A number with no name is not a reading of anything. Handed back whole
  // so the field can say so, rather than logged against nothing.
  const got = parseQuickTrack('42')
  check('a bare number names no tracker', got.target, null)
  check('...and is kept rather than dropped', got.rest, '42')
}

// ── the preview line ────────────────────────────────────────────────────
//
// This is how the grammar is taught: a hint says what is possible, the
// preview says what *this* line means.

check(
  'an existing amount reads with its unit',
  describeQuickTrack(parseQuickTrack('swimming 45min', [tracker()])),
  'Swimming — 45 min',
)
check(
  'an existing check reads as done',
  describeQuickTrack(parseQuickTrack('floss', [tracker({ name: 'Floss', kind: 'check' })])),
  'Floss — done',
)
check(
  'a scale reads out of its ceiling',
  describeQuickTrack(
    parseQuickTrack('mood 7', [tracker({ name: 'Mood', kind: 'scale', scaleMax: 10 })]),
  ),
  'Mood — 7 out of 10',
)
// A new tracker says so, and says what it will be. This is the one moment
// to catch a wrong guess -- afterwards it is a merge in the Overview.
check(
  'a new amount announces itself',
  describeQuickTrack(parseQuickTrack('swim 60min')),
  'New tracker “swim” (in min) — 60',
)
check(
  'a new habit announces itself',
  describeQuickTrack(parseQuickTrack('floss')),
  'New tracker “floss” (a habit) — done',
)
check(
  'a new scale says its ceiling',
  describeQuickTrack(parseQuickTrack('pain 3/5')),
  'New tracker “pain” (out of 5) — 3',
)
check('an empty line previews nothing', describeQuickTrack(parseQuickTrack('')), '')
check(
  'a nameless number says what is wrong',
  describeQuickTrack(parseQuickTrack('42')),
  'No tracker named in that',
)
check(
  'a decimal loses no precision and gains no zeroes',
  describeQuickTrack(parseQuickTrack('weight 78.40kg')),
  'New tracker “weight” (in kg) — 78.4',
)

await close()
finish('quick-track')
