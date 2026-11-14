// `lib/screentime.ts`: Screen time's rules that need no store.
//
// The type checker covers the shape of these; what it cannot cover is a
// rule whose failure is quiet. A week that starts on the wrong day, a month
// that ends on the 31st of February, a step forward from the 31st of January
// that lands in March, "60 min" where an hour was meant, "under a minute"
// for a day with nothing in it, a colour that changes when the list is
// re-sorted, a pasted code taken for one it is not -- none of them throws,
// and all of them are a number somebody reads wrongly.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

// `locale()` reads `navigator.language`. British English, so the examples
// read as the plan writes them ("3–9 Nov"); Node's own `navigator` is a
// getter-only property, so it is redefined rather than assigned -- see
// `format.test.mjs`.
Object.defineProperty(globalThis, 'navigator', {
  value: { userAgent: 'node', language: 'en-GB' },
  configurable: true,
})

const { module: st, close } = await load('/src/lib/screentime.ts')
const {
  CATEGORY_COLORS,
  CONNECTION_PREFIX,
  NEUTRAL_COLOR,
  base64Url,
  categoryColor,
  comparison,
  formatDuration,
  itemName,
  keyKind,
  keyText,
  parseConnectionCode,
  rangeLabel,
  rangeOf,
  rowName,
  shortDuration,
  stepAnchor,
} = st

/** Intl puts thin and narrow no-break spaces around a range's dash; a test
 *  reads them as plain ones. */
const plain = (s) => s.replace(/[\u00a0\u2009\u202f]/g, ' ').replace(/\s+/g, ' ')

// ── rangeOf ──────────────────────────────────────────────────────────
//
// 5 November 2026 is a Thursday; 1 November a Sunday.

assert.deepEqual(rangeOf('day', '2026-11-05'), { from: '2026-11-05', to: '2026-11-05' })
assert.deepEqual(
  rangeOf('week', '2026-11-05'),
  { from: '2026-11-02', to: '2026-11-08' },
  'a week starts on Monday by default',
)
assert.deepEqual(
  rangeOf('week', '2026-11-05', 0),
  { from: '2026-11-01', to: '2026-11-07' },
  'or on Sunday where the reader’s week does',
)
assert.deepEqual(
  rangeOf('week', '2026-11-01'),
  { from: '2026-10-26', to: '2026-11-01' },
  'a Sunday is the last day of a Monday week',
)
assert.deepEqual(
  rangeOf('week', '2026-11-01', 0),
  { from: '2026-11-01', to: '2026-11-07' },
  'and the first of a Sunday week',
)
assert.deepEqual(
  rangeOf('week', '2026-11-08', 6),
  { from: '2026-11-07', to: '2026-11-13' },
  'a Saturday week too',
)
assert.deepEqual(
  rangeOf('week', '2026-12-31'),
  { from: '2026-12-28', to: '2027-01-03' },
  'a week runs across a new year',
)
assert.deepEqual(rangeOf('month', '2026-11-05'), { from: '2026-11-01', to: '2026-11-30' })
assert.deepEqual(rangeOf('month', '2026-02-14'), { from: '2026-02-01', to: '2026-02-28' })
assert.deepEqual(
  rangeOf('month', '2028-02-10'),
  { from: '2028-02-01', to: '2028-02-29' },
  'a leap February has 29 days',
)
assert.deepEqual(rangeOf('month', '2026-12-31'), { from: '2026-12-01', to: '2026-12-31' })
assert.deepEqual(rangeOf('month', '2026-04-30'), { from: '2026-04-01', to: '2026-04-30' })
assert.deepEqual(rangeOf('month', '2026-01-01'), { from: '2026-01-01', to: '2026-01-31' })

// ── stepAnchor ───────────────────────────────────────────────────────

assert.equal(stepAnchor('day', '2026-03-01', -1), '2026-02-28')
assert.equal(stepAnchor('day', '2026-12-31', 1), '2027-01-01')
assert.equal(stepAnchor('week', '2026-12-30', 1), '2027-01-06')
assert.equal(stepAnchor('week', '2026-11-05', -2), '2026-10-22')
assert.equal(
  stepAnchor('month', '2026-01-31', 1),
  '2026-02-28',
  'a month on from the 31st of January is still February',
)
assert.equal(stepAnchor('month', '2026-03-31', -1), '2026-02-28')
assert.equal(stepAnchor('month', '2026-12-15', 1), '2027-01-15')
assert.equal(stepAnchor('month', '2026-01-15', -1), '2025-12-15')
// Stepped, a month anchor still names the month it should.
assert.deepEqual(rangeOf('month', stepAnchor('month', '2026-01-31', 1)), {
  from: '2026-02-01',
  to: '2026-02-28',
})

// ── rangeLabel ───────────────────────────────────────────────────────

const today = '2026-11-05'
assert.equal(rangeLabel('day', today, today, today), 'Today')
assert.equal(rangeLabel('day', '2026-11-04', '2026-11-04', today), 'Yesterday')
{
  const label = plain(rangeLabel('day', '2026-11-02', '2026-11-02', today))
  assert.match(label, /^Mon,? 2 Nov$/, `a day this year is its weekday and date: ${label}`)
}
{
  const label = rangeLabel('day', '2025-11-03', '2025-11-03', today)
  assert.ok(label.includes('2025'), `another year says which: ${label}`)
}
{
  const label = plain(rangeLabel('week', '2026-11-02', '2026-11-08', today))
  assert.match(label, /^2 ?– ?8 Nov$/, `a week in one month names it once: ${label}`)
}
{
  const label = plain(rangeLabel('week', '2026-10-26', '2026-11-01', today))
  assert.match(label, /^26 Oct ?– ?1 Nov$/, `a week across two months names both: ${label}`)
}
{
  const label = rangeLabel('week', '2026-12-28', '2027-01-03', today)
  assert.ok(label.includes('2026') && label.includes('2027'), `and across two years: ${label}`)
}
assert.equal(rangeLabel('month', '2026-11-01', '2026-11-30', today), 'November 2026')
assert.equal(rangeLabel('day', 'nonsense', 'nonsense', today), '', 'not a day is no label')
assert.equal(rangeLabel('day', '2026-02-30', '2026-02-30', today), '', 'nor a day that is not one')

// ── durations ────────────────────────────────────────────────────────

assert.equal(formatDuration(0), '0 min', 'nothing is nothing, not "under a minute"')
assert.equal(formatDuration(1), 'under a minute')
assert.equal(formatDuration(29), 'under a minute')
assert.equal(formatDuration(30), '1 min', 'minutes round to the nearest')
assert.equal(formatDuration(59), '1 min')
assert.equal(formatDuration(45 * 60), '45 min')
assert.equal(formatDuration(59 * 60 + 40), '1 h', 'never "60 min"')
assert.equal(formatDuration(2 * 3600 + 5 * 60), '2 h 05 min')
assert.equal(formatDuration(2 * 3600 + 5 * 60 + 20), '2 h 05 min')
assert.equal(formatDuration(3 * 3600), '3 h')
assert.equal(formatDuration(25 * 3600 + 60), '25 h 01 min', 'a week’s hours are hours, not days')
assert.equal(formatDuration(-5), '0 min')
assert.equal(formatDuration(Number.NaN), '0 min')

assert.equal(shortDuration(0), '0:00')
assert.equal(shortDuration(45 * 60), '0:45')
assert.equal(shortDuration(2 * 3600 + 5 * 60), '2:05')
assert.equal(shortDuration(10 * 3600), '10:00')

// ── comparison ───────────────────────────────────────────────────────

assert.equal(comparison(120, 100, 'week'), '+20% on the week before')
assert.equal(comparison(80, 100, 'day'), '−20% on the day before', 'a real minus sign')
assert.equal(comparison(0, 100, 'month'), '−100% on the month before')
assert.equal(comparison(1001, 1000, 'month'), 'About the same as the month before')
assert.equal(comparison(100, 0, 'week'), '', 'nothing to compare with says nothing')
assert.equal(comparison(0, 0, 'day'), '')
assert.equal(comparison(300, 100, 'week'), '+200% on the week before')

// ── keys and names ───────────────────────────────────────────────────

assert.equal(keyText('site:youtube.com'), 'youtube.com')
assert.equal(keyText('app:org.gnome.ptyxis'), 'org.gnome.ptyxis')
assert.equal(keyText('bare'), 'bare')
assert.equal(keyKind('site:youtube.com'), 'site')
assert.equal(keyKind('app:firefox'), 'app')
const item = { key: 'app:firefox', kind: 'app', name: 'Firefox', browserRest: false }
assert.equal(itemName(item), 'Firefox')
assert.equal(itemName({ ...item, browserRest: true }), 'Firefox (other)')
const row = { key: 'app:code', defaultName: 'Visual Studio Code', label: null }
assert.equal(rowName(row), 'Visual Studio Code')
assert.equal(rowName({ ...row, label: { name: 'Editor' } }), 'Editor')
assert.equal(rowName({ ...row, label: { name: '' } }), 'Visual Studio Code', 'empty keeps it')

// ── colours ──────────────────────────────────────────────────────────

assert.ok(CATEGORY_COLORS.length >= 6, 'enough colours to tell the starters apart')
assert.ok(CATEGORY_COLORS.every((c) => /^#[0-9a-f]{6}$/i.test(c)))
const cat = (id, sortOrder, color = '') => ({
  id,
  name: id,
  color,
  builtin: '',
  sortOrder,
  createdAt: '',
  updatedAt: '',
})
const a = cat('a', 0)
const b = cat('b', 1, '#123456')
const c = cat('c', 2)
const cats = [a, b, c]
assert.equal(categoryColor(undefined, cats), NEUTRAL_COLOR, 'uncategorised is the neutral')
assert.equal(categoryColor(b, cats), '#123456', 'a category’s own colour wins')
assert.equal(categoryColor(a, cats), CATEGORY_COLORS[0], 'else the palette by its place')
assert.equal(categoryColor(c, cats), CATEGORY_COLORS[2])
assert.equal(
  categoryColor(c, [c, b, a]),
  categoryColor(c, cats),
  'by its place in the order, not in the array',
)
assert.equal(categoryColor(c, cats), categoryColor(c, cats), 'and the same every time')
{
  const many = Array.from({ length: CATEGORY_COLORS.length + 2 }, (_, i) => cat(`k${i}`, i))
  const last = many.at(-1)
  assert.equal(
    categoryColor(last, many),
    CATEGORY_COLORS[(many.length - 1) % CATEGORY_COLORS.length],
    'the palette goes round again',
  )
}

// ── connection codes ─────────────────────────────────────────────────

const conn = { endpoint: 'http://127.0.0.1:7399', token: 'tok-123', device: 'laptop' }
const code = CONNECTION_PREFIX + base64Url(JSON.stringify(conn))
assert.ok(code.startsWith('karmayoga-screentime:'))
assert.ok(!code.includes('='), 'no padding')
assert.ok(!/[+/]/.test(code.slice(CONNECTION_PREFIX.length)), 'the url-safe alphabet')
assert.deepEqual(parseConnectionCode(code), conn)
assert.deepEqual(parseConnectionCode(`  ${code}\n`), conn, 'whitespace from a paste is ignored')
{
  const fancy = { ...conn, device: 'Hari’s läptop' }
  const round = parseConnectionCode(CONNECTION_PREFIX + base64Url(JSON.stringify(fancy)))
  assert.deepEqual(round, fancy, 'a name in any script survives')
}
{
  const noDevice = { endpoint: 'https://nas.example.ts.net', token: 't' }
  assert.deepEqual(parseConnectionCode(CONNECTION_PREFIX + base64Url(JSON.stringify(noDevice))), {
    ...noDevice,
    device: '',
  })
}
const refused = [
  '',
  'karmayoga-screentime:',
  'not-a-code',
  'karmayoga-pairing:' + base64Url(JSON.stringify(conn)),
  CONNECTION_PREFIX + base64Url('not json'),
  CONNECTION_PREFIX + base64Url('"a string"'),
  CONNECTION_PREFIX + base64Url(JSON.stringify({ ...conn, token: '' })),
  CONNECTION_PREFIX + base64Url(JSON.stringify({ ...conn, endpoint: 'javascript:alert(1)' })),
  CONNECTION_PREFIX + base64Url(JSON.stringify({ endpoint: conn.endpoint })),
  code + '==',
  code + '!',
]
for (const bad of refused) {
  assert.equal(parseConnectionCode(bad), null, `not a connection code: ${bad}`)
}

await close()
console.log('screentime: all checks passed')
