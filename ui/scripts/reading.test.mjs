// `lib/reading.ts`: how the Feed sets an article.
//
// What the type checker cannot see here is a setting that quietly does
// nothing: a stored value from an older build read back as something else, a
// size that steps past the end of the list, a rule added where the document's
// own defaults still win, or a "min read" on a three-line note.

import assert from 'node:assert/strict'
import { load } from './harness.mjs'

const { module: reading, close } = await load('/src/lib/reading.ts')
const {
  DEFAULT_READING,
  FONT_STACKS,
  LEADING,
  READING_SIZES,
  isDefaultReading,
  minutesToRead,
  normaliseReading,
  paneStyle,
  parseReading,
  readingPage,
  readingStyle,
  readingType,
  stepSize,
  withReading,
} = reading

// ── What is kept ─────────────────────────────────────────────────────────

assert.deepEqual(parseReading(null), DEFAULT_READING, 'nothing kept is the defaults')
assert.deepEqual(parseReading('not json'), DEFAULT_READING, 'nonsense is the defaults')
assert.deepEqual(parseReading('[1,2]'), DEFAULT_READING, 'a list is not settings')
assert.deepEqual(
  parseReading(
    JSON.stringify({ font: 'serif', size: 22, spacing: 'loose', width: 'wide', page: 'sepia' }),
  ),
  { font: 'serif', size: 22, spacing: 'loose', width: 'wide', page: 'sepia' },
  'what was kept is read back as it was',
)
assert.deepEqual(
  normaliseReading({ font: 'comic', size: 19, page: 'neon', extra: true }),
  { ...DEFAULT_READING, size: 18 },
  'a value this build does not know is its default, and a size between steps the nearest one',
)
assert.equal(
  normaliseReading({ size: 1000 }).size,
  READING_SIZES.at(-1),
  'a size past the end is the largest',
)
assert.equal(normaliseReading({ size: Number.NaN }).size, DEFAULT_READING.size)
assert.ok(isDefaultReading(DEFAULT_READING))
assert.ok(!isDefaultReading({ ...DEFAULT_READING, page: 'dark' }))

// ── Stepping the size ────────────────────────────────────────────────────

assert.equal(stepSize(18, 1), 20)
assert.equal(stepSize(18, -1), 17)
assert.equal(stepSize(READING_SIZES[0], -1), READING_SIZES[0], 'the smallest stays the smallest')
assert.equal(
  stepSize(READING_SIZES.at(-1), 1),
  READING_SIZES.at(-1),
  'the largest stays the largest',
)

// ── The document ─────────────────────────────────────────────────────────

{
  const serif = { ...DEFAULT_READING, font: 'serif', size: 22, spacing: 'tight' }
  const rule = readingStyle(serif)
  assert.ok(rule.includes(`--read-font:${FONT_STACKS.serif}`), rule)
  assert.ok(rule.includes('--read-size:22px'), rule)
  assert.ok(rule.includes(`--read-leading:${LEADING.tight}`), rule)

  const doc =
    '<!DOCTYPE html><html><head><style>:root{--read-size:18px}</style></head><body><p>x</p></body></html>'
  const set = withReading(doc, serif)
  // After the document's own defaults, so it is the rule that holds.
  assert.ok(set.indexOf('--read-size:22px') > set.indexOf('--read-size:18px'), set)
  assert.ok(set.indexOf('--read-size:22px') < set.indexOf('</head>'), set)
  assert.equal(withReading('<p>no head</p>', serif), '<p>no head</p>', 'nothing to add a rule to')
}

// ── The page ─────────────────────────────────────────────────────────────

assert.equal(readingPage(DEFAULT_READING), null, "the theme's page is the theme's own")
assert.equal(paneStyle(DEFAULT_READING), '', 'and the pane is left alone')
{
  const sepia = { ...DEFAULT_READING, page: 'sepia' }
  const page = readingPage(sepia)
  assert.equal(page.dark, false)
  assert.equal(page.frame.page, page.tokens['--bg'], 'the frame and the pane are one page')
  const style = paneStyle(sepia)
  assert.ok(style.includes(`background:${page.frame.page}`), style)
  assert.ok(style.includes('--fg:'), style)
  assert.equal(readingPage({ ...DEFAULT_READING, page: 'dark' }).dark, true)
}

// ── Sizing the frame ─────────────────────────────────────────────────────

{
  const type = readingType({ ...DEFAULT_READING, size: 24, spacing: 'loose', font: 'serif' })
  assert.equal(type.sizePx, 24)
  assert.equal(type.leading, LEADING.loose)
  assert.ok(type.letterEm > readingType(DEFAULT_READING).letterEm, 'a serif runs wider')
  // `padding: 20px clamp(16px, 4vw, 28px) 28px`.
  assert.equal(type.padX(200), 16)
  assert.equal(type.padX(500), 20)
  assert.equal(type.padX(2000), 28)
}

// ── How long it takes ────────────────────────────────────────────────────

assert.equal(minutesToRead('A short note.'), null, 'a note is not timed')
assert.equal(minutesToRead('word '.repeat(199)), null)
assert.equal(minutesToRead('word '.repeat(200)), 1)
assert.equal(minutesToRead('word '.repeat(231 * 6)), 7, 'rounded up')
assert.equal(
  minutesToRead(`${'word '.repeat(150)}${' — '.repeat(100)}`),
  null,
  'a dash is not a word',
)

await close()
console.log('reading: all checks passed')
