// How tall a message body comes out, from its markup and the sizes of its
// pictures. The sandbox that keeps a message in its frame also stops anything
// asking the frame how tall it turned out, so this lays the message out on paper
// instead: a block's children stack, a table's columns are shared out the way a
// browser shares them, and a box has the padding, the margins and the type its
// own styles and the sender's sheet give it.
//
// A template is a wrapper table around a table around a table, with its spacing
// in the padding of cells and whole rows given to a gap. Counting lines and rows
// over that adds up cells that sit side by side and misses everything that is
// not text, which left a designed message with a screen of white under it.
//
// A picture is the one thing markup cannot say: most state a width or nothing,
// and text below one that comes out taller than guessed ends up past the bottom
// of the frame, where it looks like the end of the message. So `readPictures`
// has the main window load the same address, which needs nothing of the frame,
// and keeps only the size. The backend fetches a picture once however many ask.
//
// Like `mailbody.ts` it reads `sanitize::sanitize`'s output, and nothing here is
// a wall: a misjudged box costs some blank or a scrollbar inside the frame. It
// does run on the main thread on a stranger's markup, so what a message can cost
// is limited (`MOST_STEPS`, `MAX_RULES`), and no word of a message is kept at
// module level: all of it hangs off what `measureBody` returns, which goes when
// the message leaves the screen, as a lock takes it.

import {
  attribute,
  classesOf,
  declarations,
  decodeEntities,
  lengthOnScreen,
  tokenize,
  type Token,
} from './mailbody'

// ── The sender's sheet ────────────────────────────────────────────────

interface Compound {
  tag: string | null
  id: string | null
  classes: string[]
}

interface Declaration {
  property: string
  value: string
  important: boolean
}

interface Rule {
  /** Outermost first. Every combinator is read as a descendant. */
  path: Compound[]
  declared: Declaration[]
  /** Specificity, which `declare` compares. */
  weight: number
  /** Where it came in the sheets: a later rule wins a tie. */
  order: number
}

/** Lookups go through Maps, as in `mailbody.ts`: a class is a stranger's word. */
type Sheet = Map<string, Rule[]>

interface SheetReader {
  sheet: Sheet
  rules: number
}

/** Across every sheet in a message, each selector of a list counting for one. */
const MAX_RULES = 2000
/** How deep one `@media` goes in another. An unclosed one is read to the end of
 *  the sheet, so this is also how many times a sheet can be read over. */
const MAX_SHEET_NESTING = 4

const WEIGHT_ID = 100
const WEIGHT_CLASS = 10
const WEIGHT_TAG = 1
const WEIGHT_INLINE = 500_000
const WEIGHT_IMPORTANT = 1_000_000

/** `td[class=col]`, which is how a template of a certain age writes `td.col`. */
function withoutClassAttributes(text: string): string {
  return text.replace(
    /\[\s*(class|id)\s*[~|^*$]?=\s*(?:"([\w-]+)"|'([\w-]+)'|([\w-]+))\s*\]/gi,
    (_, name: string, a?: string, b?: string, c?: string) =>
      `${name.toLowerCase() === 'id' ? '#' : '.'}${a ?? b ?? c ?? ''}`,
  )
}

function compound(raw: string): Compound | null {
  const text = withoutClassAttributes(raw)
  const m = /^(\*|[a-z][a-z0-9]*)?((?:[.#][\w-]+)*)$/i.exec(text)
  if (!m || (!m[1] && !m[2])) return null
  const tag = !m[1] || m[1] === '*' ? null : m[1].toLowerCase()
  const parts = (m[2] ?? '').match(/[.#][\w-]+/g) ?? []
  const ids = parts.filter((p) => p.startsWith('#')).map((p) => p.slice(1))
  if (ids.length > 1) return null
  const classes = parts.filter((p) => p.startsWith('.')).map((p) => p.slice(1).toLowerCase())
  return { tag, id: ids[0] ?? null, classes }
}

/** `null` for a selector this does not follow: a pseudo-class, an attribute. A
 *  rule like that is about a hover or a client's quirk rather than a layout. */
function selector(text: string): Compound[] | null {
  if (/[+~]/.test(text)) return null
  const path: Compound[] = []
  for (const part of text.replace(/>/g, ' ').trim().split(/\s+/)) {
    const read = compound(part)
    if (!read) return null
    path.push(read)
  }
  return path.length ? path : null
}

/** Whether a media query holds in a frame `width` wide. A device's width is not
 *  the frame's, and a query about a pixel ratio or the dark theme is about
 *  neither, so those count as not holding. Nor does a type that is not a screen:
 *  a template's `@media yahoo` is a hack aimed at one client. */
function mediaHolds(query: string, width: number): boolean {
  return query.split(',').some((branch) => {
    const type = /^\s*(?:only\s+)?([a-z-]+)(?=\s|$)/i.exec(branch)?.[1]?.toLowerCase()
    if (type && type !== 'all' && type !== 'screen') return false
    if (/\bnot\b|device-|-ratio|prefers-|-resolution/i.test(branch)) return false
    const bound = (name: string) => {
      const m = new RegExp(`\\b${name}\\s*:\\s*([\\d.]+)\\s*([a-z]*)`, 'i').exec(branch)
      return m ? (length(`${m[1]}${m[2]}`, 16, null) ?? NaN) : NaN
    }
    return !(width > bound('max-width')) && !(width < bound('min-width'))
  })
}

/** What this reads. Everything else in a sheet, which is mostly colour, need not
 *  be matched against every element. */
const READ = new Set(
  (
    'display width min-width max-width height min-height max-height box-sizing float ' +
    'padding padding-top padding-right padding-bottom padding-left ' +
    'margin margin-top margin-right margin-bottom margin-left ' +
    'border border-top border-right border-bottom border-left border-width border-style ' +
    'border-top-width border-right-width border-bottom-width border-left-width ' +
    'border-collapse border-spacing ' +
    'font font-size font-family font-weight font-style line-height letter-spacing ' +
    'white-space text-transform'
  ).split(' '),
)

/** The sides of `padding: 1px 2px 3px 4px`, however many of them it spells. */
function sides(value: string): [string, string, string, string] {
  const v = value.trim().split(/\s+/)
  const top = v[0] ?? '0'
  const right = v[1] ?? top
  const bottom = v[2] ?? top
  return [top, right, bottom, v[3] ?? right]
}

const BORDER_WIDTHS = new Map([
  ['thin', '1px'],
  ['medium', '3px'],
  ['thick', '5px'],
])

/** The width in a `border: 1px solid red`. No style at all, or `none`, has none. */
function borderWidth(value: string): string {
  if (/\b(?:none|hidden)\b/i.test(value)) return '0'
  const size = /(?:^|\s)([\d.]+[a-z]*|thin|medium|thick)(?=\s|$)/i.exec(value)?.[1]
  return size ? (BORDER_WIDTHS.get(size.toLowerCase()) ?? size) : '3px'
}

const SIDES = ['top', 'right', 'bottom', 'left'] as const

/** A rule's declarations as the longhands this reads, once, so that applying one
 *  to an element costs a Map write. */
function expand(list: [string, string][]): Declaration[] {
  const out: Declaration[] = []
  for (const [property, raw] of list) {
    if (!READ.has(property)) continue
    const bang = raw.lastIndexOf('!')
    const important = bang >= 0 && /^!\s*important\s*$/i.test(raw.slice(bang))
    const value = important ? raw.slice(0, bang).trimEnd() : raw
    const set = (p: string, v: string) => out.push({ property: p, value: v, important })
    if (property === 'padding' || property === 'margin') {
      sides(value).forEach((v, i) => set(`${property}-${SIDES[i]}`, v))
    } else if (property === 'font') {
      // `font: inherit` and a system font's name say nothing that can be read.
      const m = /(?:^|\s)([\d.]+[a-z%]+)(?:\s*\/\s*([\d.]+[a-z%]*|normal))?\s+(\S.*)$/i.exec(value)
      if (!m?.[1]) continue
      set('font-size', m[1])
      if (m[3]) set('font-family', m[3])
      if (/\bbold\b|\b[6-9]00\b/i.test(value.slice(0, m.index))) set('font-weight', 'bold')
      set('line-height', m[2] ?? 'normal')
    } else if (property === 'border') {
      for (const side of SIDES) set(`border-${side}-width`, borderWidth(value))
    } else if (/^border-(?:top|right|bottom|left)$/.test(property)) {
      set(`${property}-width`, borderWidth(value))
    } else if (property === 'border-width') {
      sides(value).forEach((v, i) => set(`border-${SIDES[i]}-width`, v))
    } else if (property === 'border-style') {
      if (/^(?:none|hidden)\b/i.test(value)) {
        for (const side of SIDES) set(`border-${side}-width`, '0')
      }
    } else {
      set(property, value)
    }
  }
  return out
}

function readSheet(css: string, width: number, reader: SheetReader, nesting = 0): void {
  let pos = 0
  while (reader.rules < MAX_RULES) {
    const open = css.indexOf('{', pos)
    if (open < 0) return
    let depth = 1
    let close = open + 1
    for (; close < css.length && depth > 0; close++) {
      if (css[close] === '{') depth++
      else if (css[close] === '}') depth--
    }
    // What comes before a `;` is a statement of its own, `@charset "utf-8";`.
    const before = css.slice(pos, open)
    const prelude = before.slice(before.lastIndexOf(';') + 1).trim()
    const inside = css.slice(open + 1, depth === 0 ? close - 1 : close)
    pos = close
    if (prelude.startsWith('@')) {
      if (
        nesting < MAX_SHEET_NESTING &&
        /^@media\b/i.test(prelude) &&
        mediaHolds(prelude.slice(6), width)
      ) {
        readSheet(inside, width, reader, nesting + 1)
      }
      continue
    }
    const declared = expand(declarations(inside))
    if (declared.length === 0) continue
    for (const text of prelude.split(',')) {
      if (reader.rules >= MAX_RULES) return
      const path = selector(text)
      const last = path?.[path.length - 1]
      if (!path || !last) continue
      const weight = path.reduce(
        (sum, c) =>
          sum + (c.id ? WEIGHT_ID : 0) + c.classes.length * WEIGHT_CLASS + (c.tag ? WEIGHT_TAG : 0),
        0,
      )
      const key = last.id
        ? `#${last.id}`
        : last.classes[0]
          ? `.${last.classes[0]}`
          : (last.tag ?? '*')
      const rule: Rule = { path, declared, weight, order: reader.rules++ }
      const list = reader.sheet.get(key)
      if (list) list.push(rule)
      else reader.sheet.set(key, [rule])
    }
  }
}

/** A sheet with its comments out. By hand rather than by a pattern, which would
 *  read to the end of the sheet again from every `/*` that is never closed. */
function withoutComments(css: string): string {
  let out = ''
  let pos = 0
  for (;;) {
    const open = css.indexOf('/*', pos)
    if (open < 0) break
    out += css.slice(pos, open)
    const close = css.indexOf('*/', open + 2)
    if (close < 0) return out.replaceAll('<!--', '').replaceAll('-->', '')
    pos = close + 2
  }
  return (out + css.slice(pos)).replaceAll('<!--', '').replaceAll('-->', '')
}

// ── What is declared on an element ────────────────────────────────────

type Declared = Map<string, { value: string; weight: number }>

function declare(style: Declared, property: string, value: string, weight: number): void {
  const held = style.get(property)
  if (!held || weight >= held.weight) style.set(property, { value, weight })
}

function declareAll(style: Declared, declared: Declaration[], weight: number): void {
  for (const d of declared) {
    declare(style, d.property, d.value, weight + (d.important ? WEIGHT_IMPORTANT : 0))
  }
}

// ── Lengths and type ──────────────────────────────────────────────────

/** More than any page, and few enough digits that nothing downstream overflows. */
const LARGEST_LENGTH = 100_000

/** A CSS length in pixels. `em` and `percentOf` are what it is relative to;
 *  `null` where it is `auto`, or a percentage of something with no size. */
function length(value: string | undefined | null, em: number, percentOf: number | null) {
  const m = /^\s*(-?[\d.]+)\s*(px|pt|em|rem|ex|in|cm|mm|%)?\s*$/i.exec(value ?? '')
  if (!m) return null
  const n = parseFloat(m[1] ?? '')
  if (!Number.isFinite(n)) return null
  let px: number
  switch ((m[2] ?? 'px').toLowerCase()) {
    case 'pt':
      px = (n * 4) / 3
      break
    case 'em':
      px = n * em
      break
    case 'rem':
      px = n * 16
      break
    case 'ex':
      px = n * em * 0.5
      break
    case 'in':
      px = n * 96
      break
    case 'cm':
      px = (n * 96) / 2.54
      break
    case 'mm':
      px = (n * 96) / 25.4
      break
    case '%':
      if (percentOf === null) return null
      px = (n / 100) * percentOf
      break
    default:
      px = n
  }
  return Math.max(-LARGEST_LENGTH, Math.min(LARGEST_LENGTH, px))
}

/** What a browser's own sheet makes of a heading's type and margins, in ems. */
const HEADINGS = new Map([
  ['h1', { size: 2, margin: 0.67 }],
  ['h2', { size: 1.5, margin: 0.83 }],
  ['h3', { size: 1.17, margin: 1 }],
  ['h4', { size: 1, margin: 1.33 }],
  ['h5', { size: 0.83, margin: 1.67 }],
  ['h6', { size: 0.67, margin: 2.33 }],
])
const FONT_SIZES = new Map([
  ['xx-small', 9],
  ['x-small', 10],
  ['small', 13],
  ['medium', 16],
  ['large', 18],
  ['x-large', 24],
  ['xx-large', 32],
])
/** `<font size="1">` to `7`, where `+2` is counted from the `3` nothing states. */
const FONT_TAG_SIZES = [10, 13, 16, 18, 24, 32, 48]

/** What a child takes from its parent. */
interface Type {
  size: number
  leading: number
  /** A unitless `line-height`, which scales with the child's type where one in
   *  pixels is handed down as it is. */
  ratio: number | null
  pre: boolean
  nowrap: boolean
  /** The face and weight, in the terms `canvas.font` wants. */
  face: string
  bold: boolean
  italic: boolean
  /** Extra room between letters, in pixels. */
  spacing: number
  upper: boolean
}

/** The fonts this window loads for itself, one of which `BASE_STYLE` asks for
 *  first. The frame has neither, so a canvas here must not pick them either. */
const NOT_IN_A_FRAME = /['"]?(?:Source Sans 3|Inter) Variable['"]?\s*,?\s*/gi

/** Words that hand a property down, or back, rather than saying anything. */
const NOT_A_VALUE = /^(?:inherit|initial|unset|revert|revert-layer)$/i

/** `normal`, for the faces a message ends up in. */
const NORMAL_LEADING = 1.2

/** The width of an average character as a share of its type's size, for where
 *  there is no canvas to ask. */
const CHAR_EM = 0.5

/** Longer than this a run of text is measured by the character: nobody reads it
 *  word by word, and a hostile one should not cost a call per word. */
const MOST_MEASURED_WORDS = 5000

const MOST_MEASURED_ENTRIES = 50_000

// ── What is laid out ──────────────────────────────────────────────────

const BOLD = new Set('b strong th h1 h2 h3 h4 h5 h6'.split(' '))
/** The rest of the tags are inline, as a browser takes one it does not know. */
const BLOCKS = new Set(
  (
    'address article aside blockquote caption center dd details dir div dl dt fieldset figcaption ' +
    'figure footer form h1 h2 h3 h4 h5 h6 header hgroup hr legend li listing main menu nav ol p ' +
    'plaintext pre section summary ul xmp'
  ).split(' '),
)
const NOT_DRAWN = new Set('head title meta link base col colgroup script template'.split(' '))
const PART_OF_TABLE = new Set(['thead', 'tbody', 'tfoot'])
const CELL = new Set(['td', 'th'])
const LISTS = new Set(['ul', 'ol', 'menu', 'dir'])
const ROW_GROUP = /^table-(?:row|header|footer)-group$/

/** A browser's own room for a list's bullets, and under a `dd`. */
const LIST_INDENT_PX = 40

/** Past this a body is not a message anybody wrote, and a recursive walk has to
 *  stop somewhere. */
const MAX_DEPTH = 150

/** Enough for any message by a long way, and about a tenth of a second of work
 *  when a hostile one reaches it. `measureBody` then gives up on the message. */
const MOST_STEPS = 3_000_000

/** A picture that says nothing of its size, and how tall one is for its width
 *  when it only says that. Nothing can be known until it arrives. */
const UNSTATED_PICTURE_PX = 100
const UNSTATED_PICTURE_RATIO = 0.5

/** `colspan`'s own limit, in the HTML standard. */
const MOST_COLUMNS_SPANNED = 1000

interface Flowed {
  height: number
  /** Margins still free to collapse with whatever is next to this. */
  top: number
  bottom: number
}

/** How wide a box can go, and how wide it would go if nothing made it wrap. */
interface Wants {
  least: number
  most: number
}

const NOTHING: Wants = { least: 0, most: 0 }

/** A message's markup, read once. Only the width changes after this. */
interface Parsed {
  html: string
  tokens: Token[]
  kids: Map<number, number[]>
  sheets: string[]
  classes: Map<number, string[]>
  attributes: Map<number, Map<string, string | null>>
  inline: Map<number, Declaration[]>
  /** `body>:first-child` and the like, on the card: `[property, weight]`. */
  edges: Map<number, [string, number][]>
  pen: CanvasRenderingContext2D | null
  penFont: string
  measured: Map<string, number>
  imagesHidden: boolean
  /** The pictures that have arrived, by address. */
  pictureSizes: Map<string, { width: number; height: number }>
}

/** The same at one width, which decides which of a sheet's breakpoints hold. */
interface Layout extends Parsed {
  sheet: Sheet
  styles: Map<number, Declared>
  wanted: Map<number, Wants>
  steps: number
}

function spend(layout: Layout, steps = 1): void {
  layout.steps += steps
  if (layout.steps > MOST_STEPS) throw new Error('too much to lay out')
}

function kidsOf(tokens: Token[]): Map<number, number[]> {
  const kids = new Map<number, number[]>()
  for (const [i, t] of tokens.entries()) {
    if (t.kind === 'close' || t.kind === 'comment' || t.kind === 'raw') continue
    const list = kids.get(t.parent)
    if (list) list.push(i)
    else kids.set(t.parent, [i])
  }
  return kids
}

function attrOf(parsed: Parsed, at: number, name: string): string | null {
  const t = parsed.tokens[at]
  if (!t) return null
  let held = parsed.attributes.get(at)
  if (!held) {
    held = new Map()
    parsed.attributes.set(at, held)
  }
  let value = held.get(name)
  if (value === undefined) {
    value = attribute(parsed.html.slice(t.at, t.end), name)
    held.set(name, value)
  }
  return value
}

function classesAt(parsed: Parsed, at: number): string[] {
  let classes = parsed.classes.get(at)
  if (!classes) {
    const t = parsed.tokens[at]
    classes = t ? classesOf(parsed.html.slice(t.at, t.end)).filter(Boolean) : []
    parsed.classes.set(at, classes)
  }
  return classes
}

function fits(layout: Layout, c: Compound, at: number): boolean {
  const t = layout.tokens[at]
  if (!t || (c.tag !== null && c.tag !== t.tag)) return false
  if (c.id !== null && attrOf(layout, at, 'id') !== c.id) return false
  if (c.classes.length === 0) return true
  const classes = classesAt(layout, at)
  return c.classes.every((k) => classes.includes(k))
}

function matches(layout: Layout, at: number, path: Compound[]): boolean {
  const last = path[path.length - 1]
  if (!last || !fits(layout, last, at)) return false
  let need = path.length - 2
  for (let p = layout.tokens[at]?.parent ?? -1; need >= 0 && p >= 0;) {
    spend(layout)
    const c = path[need]
    if (c && fits(layout, c, p)) need--
    p = layout.tokens[p]?.parent ?? -1
  }
  // The document's own `html` and `body` hold everything, and are not in the
  // markup to be found.
  while (need >= 0 && (path[need]?.tag === 'body' || path[need]?.tag === 'html')) need--
  return need < 0
}

function styleOf(layout: Layout, at: number): Declared {
  const known = layout.styles.get(at)
  if (known) return known
  const style: Declared = new Map()
  const t = layout.tokens[at]
  if (t) {
    const keys = [t.tag, '*', ...classesAt(layout, at).map((c) => `.${c}`)]
    const id = attrOf(layout, at, 'id')
    if (id) keys.push(`#${id}`)
    const found = new Set<Rule>()
    for (const key of keys) {
      for (const rule of layout.sheet.get(key) ?? []) {
        spend(layout)
        if (matches(layout, at, rule.path)) found.add(rule)
      }
    }
    // First, since the sender's sheet comes after the page's and wins a tie.
    for (const [property, weight] of layout.edges.get(at) ?? []) {
      declare(style, property, '0', weight)
    }
    // `declare` sees to the specificity, so this order only decides a tie.
    for (const rule of [...found].sort((a, b) => a.order - b.order)) {
      spend(layout, rule.declared.length)
      declareAll(style, rule.declared, rule.weight)
    }
    let inline = layout.inline.get(at)
    if (!inline) {
      inline = expand(declarations(attrOf(layout, at, 'style') ?? ''))
      layout.inline.set(at, inline)
    }
    declareAll(style, inline, WEIGHT_INLINE)
  }
  layout.styles.set(at, style)
  return style
}

function typeOf(parsed: Parsed, at: number, style: Declared, outer: Type): Type {
  const tag = parsed.tokens[at]?.tag ?? ''
  const own = (property: string): string | undefined => {
    const value = style.get(property)?.value.trim()
    return value && !NOT_A_VALUE.test(value) ? value : undefined
  }
  let size = outer.size
  const heading = HEADINGS.get(tag)
  if (heading) size = outer.size * heading.size
  else if (tag === 'small') size = outer.size * 0.83
  else if (tag === 'big') size = outer.size * 1.2
  else if (tag === 'font') {
    const stated = attrOf(parsed, at, 'size')?.trim() ?? ''
    const n = parseInt(stated, 10)
    const index = /^[+-]/.test(stated) ? 3 + n : n
    if (Number.isFinite(index)) {
      size = FONT_TAG_SIZES[Math.max(1, Math.min(7, index)) - 1] ?? size
    }
  }
  const stated = own('font-size')
  if (stated) {
    size =
      FONT_SIZES.get(stated.toLowerCase()) ?? length(stated, outer.size, outer.size) ?? outer.size
  }
  const leadingValue = own('line-height')
  let leading = outer.ratio === null ? outer.leading : outer.ratio * size
  let ratio = outer.ratio
  if (leadingValue) {
    if (/^normal$/i.test(leadingValue)) {
      ratio = NORMAL_LEADING
      leading = ratio * size
    } else if (/^[\d.]+$/.test(leadingValue)) {
      ratio = parseFloat(leadingValue)
      leading = ratio * size
    } else {
      const px = length(leadingValue, size, size)
      if (px !== null) {
        ratio = null
        leading = px
      }
    }
  }
  const white = own('white-space')?.toLowerCase()
  const weight = own('font-weight')?.toLowerCase()
  const fontStyle = own('font-style')?.toLowerCase()
  const transform = own('text-transform')?.toLowerCase()
  const spacing = own('letter-spacing')
  return {
    size,
    leading,
    ratio,
    pre: white ? white.startsWith('pre') : outer.pre || tag === 'pre',
    // A browser's own `white-space` for `pre`, which `BASE_STYLE` changes.
    nowrap: white
      ? white === 'nowrap' || white === 'pre'
      : outer.nowrap || tag === 'nobr' || tag === 'pre',
    face: own('font-family')?.replace(NOT_IN_A_FRAME, '').trim() || outer.face,
    bold: weight
      ? weight === 'bold' || weight === 'bolder' || parseInt(weight, 10) >= 600
      : outer.bold || BOLD.has(tag),
    italic: fontStyle ? fontStyle !== 'normal' : outer.italic || tag === 'i' || tag === 'em',
    spacing:
      spacing === undefined
        ? outer.spacing
        : /^normal$/i.test(spacing)
          ? 0
          : (length(spacing, size, null) ?? outer.spacing),
    upper: transform ? transform === 'uppercase' : outer.upper,
  }
}

/**
 * The width of `word` in `type`. A frame's text is drawn with the system's fonts
 * and so is a canvas's in this window, so this is the width the frame will give
 * it, where an average per character is wrong by a line on most paragraphs.
 */
function widthOf(parsed: Parsed, word: string, type: Type): number {
  // `font-size: 0` is how a template takes the space out from between columns,
  // and a canvas told `0px` measures as if at its default of 10px.
  if (type.size <= 0) return 0
  const shown = type.upper ? word.toUpperCase() : word
  const extra = type.spacing * shown.length
  const { pen } = parsed
  if (!pen) return lengthOnScreen(shown) * type.size * CHAR_EM * (type.upper ? 1.15 : 1) + extra
  const weight = `${type.italic ? 'italic ' : ''}${type.bold ? '700 ' : ''}${type.size}px`
  const font = `${weight} ${type.face}`
  const key = `${font}\n${shown}`
  let width = parsed.measured.get(key)
  if (width === undefined) {
    if (parsed.penFont !== font) {
      // A canvas ignores a font it cannot read and keeps the one it had, so
      // what is measured next would depend on what was measured last.
      pen.font = '1px serif'
      const before = pen.font
      pen.font = font
      if (pen.font === before) pen.font = `${weight} sans-serif`
      parsed.penFont = font
    }
    width = pen.measureText(shown).width
    if (parsed.measured.size >= MOST_MEASURED_ENTRIES) parsed.measured.clear()
    parsed.measured.set(key, width)
  }
  return width + extra
}

function displayOf(tag: string, style: Declared): string {
  const stated = style.get('display')?.value.trim().toLowerCase()
  if (stated && !NOT_A_VALUE.test(stated)) return stated
  if (tag === 'table') return 'table'
  if (CELL.has(tag)) return 'table-cell'
  if (tag === 'tr') return 'table-row'
  if (PART_OF_TABLE.has(tag)) return 'table-row-group'
  return BLOCKS.has(tag) ? 'block' : 'inline'
}

interface Edges {
  top: number
  bottom: number
  /** Everything to the left and right together. */
  across: number
}

/** A box's padding and border. `fallback` is a cell's `cellpadding`, and a list
 *  has a browser's own room for its bullets unless a style says otherwise. */
function padding(style: Declared, tag: string, em: number, of: number, fallback = 0): Edges {
  const pad = (s: string, unstated = fallback) => {
    const px = length(style.get(`padding-${s}`)?.value, em, of)
    return px === null ? unstated : Math.max(0, px)
  }
  const border = (s: string) =>
    Math.max(0, length(style.get(`border-${s}-width`)?.value, em, of) ?? 0)
  const left = pad('left', LISTS.has(tag) ? LIST_INDENT_PX : fallback)
  return {
    top: pad('top') + border('top'),
    bottom: pad('bottom') + border('bottom'),
    across: left + pad('right') + border('left') + border('right'),
  }
}

/** A browser's own margins for a tag, over which `style` has the last word. */
function margins(tag: string, style: Declared, type: Type, of: number, nestedList: boolean) {
  const heading = HEADINGS.get(tag)
  let top = 0
  let bottom = 0
  let across = 0
  if (heading) top = bottom = heading.margin * type.size
  else if (tag === 'p' || tag === 'dl') top = bottom = type.size
  else if (LISTS.has(tag)) top = bottom = nestedList ? 0 : type.size
  else if (tag === 'blockquote' || tag === 'figure') {
    top = bottom = type.size
    across = LIST_INDENT_PX * 2
  } else if (tag === 'dd') across = LIST_INDENT_PX
  else if (tag === 'pre') bottom = type.size
  else if (tag === 'hr' || tag === 'details') top = bottom = type.size * 0.5
  const side = (name: string, fallback: number) => {
    const stated = style.get(`margin-${name}`)?.value
    return stated === undefined ? fallback : (length(stated, type.size, of) ?? 0)
  }
  const statedAcross =
    style.has('margin-left') || style.has('margin-right')
      ? side('left', tag === 'dd' ? LIST_INDENT_PX : 0) + side('right', 0)
      : across
  return { top: side('top', top), bottom: side('bottom', bottom), across: statedAcross }
}

/** A box's own width, where a style beats the `width` attribute even when it says
 *  `100%` of something that has no size yet, or `auto`. */
function statedWidth(
  style: Declared,
  widthAttribute: string | null,
  em: number,
  percentOf: number | null,
): number | null {
  const declared = style.get('width')?.value
  return length(declared ?? widthAttribute, em, percentOf)
}

/** The room a box's content has inside `available`. `stated` is whether its
 *  width is its own rather than whatever it is put in. */
function contentWidth(
  style: Declared,
  widthAttribute: string | null,
  em: number,
  available: number,
  around: number,
): { width: number; stated: boolean } {
  const stated = statedWidth(style, widthAttribute, em, available)
  const limit = length(style.get('max-width')?.value, em, available)
  const least = length(style.get('min-width')?.value, em, available)
  let width: number
  if (stated === null) width = available - around
  else if (style.get('box-sizing')?.value.trim().toLowerCase() === 'border-box') {
    width = stated - around
  } else width = stated
  if (limit !== null) width = Math.min(width, limit)
  if (least !== null) width = Math.max(width, least)
  return { width: Math.max(0, width), stated: stated !== null }
}

/** The size a picture will be drawn at, or `null` for one that will not be. */
function picture(
  layout: Layout,
  at: number,
  style: Declared,
  type: Type,
  available: number,
): { width: number; height: number } | null {
  if (!pictureIsDrawn(layout, at)) return null
  const attrWidth = length(attrOf(layout, at, 'width'), type.size, available)
  const attrHeight = length(attrOf(layout, at, 'height'), type.size, null)
  const styleWidth = length(style.get('width')?.value, type.size, available)
  const styleHeight = length(style.get('height')?.value, type.size, null)
  // `BASE_STYLE` says `img { max-width: 100%; height: auto }`: unless a style
  // says otherwise, its height follows its shape rather than its `height`.
  const limit = length(style.get('max-width')?.value, type.size, available) ?? available
  const naturalWidth = styleWidth ?? attrWidth
  const naturalHeight = styleHeight ?? attrHeight
  const real = layout.pictureSizes.get(attrOf(layout, at, 'src') ?? '')
  if (real) {
    const shape = real.height / real.width
    const width = Math.min(limit, naturalWidth ?? (styleHeight ?? real.height) / shape, available)
    return { width, height: styleHeight ?? width * shape }
  }
  if (naturalWidth === null && naturalHeight === null) {
    return { width: Math.min(limit, UNSTATED_PICTURE_PX), height: UNSTATED_PICTURE_PX }
  }
  const width = Math.min(limit, naturalWidth ?? naturalHeight ?? 0, available)
  if (naturalHeight === null) return { width, height: width * UNSTATED_PICTURE_RATIO }
  if (styleHeight !== null) return { width, height: styleHeight }
  const shape = attrWidth && attrHeight ? attrHeight / attrWidth : null
  return { width, height: shape === null ? naturalHeight : width * shape }
}

/** The frame's policy refuses anything else, and a refused picture with no shape
 *  of its own draws nothing: an address the sanitiser left out, or a relative
 *  one, which has nothing to be relative to here. */
function pictureIsDrawn(parsed: Parsed, at: number): boolean {
  const src = attrOf(parsed, at, 'src') ?? ''
  if (!/^(?:karmayoga:|https?:\/\/karmayoga\.localhost|data:)/i.test(src)) return false
  return !(parsed.imagesHidden && src.includes('/mail/img/'))
}

/** Whether a run of text draws anything: whitespace between tags does not, and
 *  a `&nbsp;` is how a blank line is written. */
function textIsDrawn(raw: string): boolean {
  return /[^\s]|\u00a0/.test(raw)
}

/** Text as it is laid out: runs of space are one space. */
function drawnText(raw: string): string {
  return decodeEntities(raw)
    .replace(/[ \t\r\n\f]+/g, ' ')
    .trim()
}

function textWants(parsed: Parsed, raw: string, type: Type): Wants {
  if (!textIsDrawn(raw)) return NOTHING
  const rows = type.pre ? decodeEntities(raw).split('\n') : [drawnText(raw)]
  const space = widthOf(parsed, ' ', type)
  let least = 0
  let most = 0
  for (const row of rows) {
    const words = row.split(' ')
    if (words.length > MOST_MEASURED_WORDS) {
      most = Math.max(most, lengthOnScreen(row) * type.size * CHAR_EM)
      continue
    }
    let across = 0
    for (const word of words) {
      const width = widthOf(parsed, word, type)
      least = Math.max(least, width)
      across += width + space
    }
    most = Math.max(most, across - space)
  }
  // Nothing but `&nbsp;`, which is how a spacer cell keeps itself open.
  if (most <= 0) return { least: space, most: space }
  return { least: type.nowrap ? most : least, most }
}

/** A picture's width with nothing to fit into. One with a percentage for a width
 *  or a limit can be squeezed to nothing, and `BASE_STYLE`'s limit is on all of
 *  them, so a picture does not hold a column open unless it insists. */
function pictureWants(layout: Layout, at: number, style: Declared, type: Type): Wants {
  if (!pictureIsDrawn(layout, at)) return NOTHING
  const most =
    length(style.get('width')?.value, type.size, null) ??
    length(attrOf(layout, at, 'width'), type.size, null) ??
    layout.pictureSizes.get(attrOf(layout, at, 'src') ?? '')?.width ??
    UNSTATED_PICTURE_PX
  const squeezes =
    (style.get('max-width')?.value ?? '').includes('%') ||
    (style.get('width')?.value ?? '').includes('%')
  const floor = length(style.get('min-width')?.value, type.size, null) ?? 0
  return { least: squeezes ? floor : most, most }
}

/** How much a table cell can be squeezed, and how much room it would like, which
 *  is how a table shares its columns out. */
function wants(layout: Layout, at: number, outer: Type, depth: number): Wants {
  const known = layout.wanted.get(at)
  if (known) return known
  const t = layout.tokens[at]
  if (!t || depth > MAX_DEPTH) return NOTHING
  let out: Wants
  if (t.kind === 'text') out = textWants(layout, layout.html.slice(t.at, t.end), outer)
  else out = elementWants(layout, at, t, outer, depth)
  layout.wanted.set(at, out)
  return out
}

function elementWants(layout: Layout, at: number, t: Token, outer: Type, depth: number): Wants {
  const style = styleOf(layout, at)
  const display = displayOf(t.tag, style)
  if (display === 'none' || NOT_DRAWN.has(t.tag)) return NOTHING
  const type = typeOf(layout, at, style, outer)
  if (t.tag === 'img') return pictureWants(layout, at, style, type)
  const isRow = display === 'table-row'
  const isCell = CELL.has(t.tag)
  const box = padding(style, t.tag, type.size, 0, isCell ? cellPaddingOf(layout, at, type) : 0)
  let least = 0
  let most = 0
  let run = 0
  for (const kid of layout.kids.get(at) ?? []) {
    const k = layout.tokens[kid]
    if (!k) continue
    const w = wants(layout, kid, type, depth + 1)
    if (isRow) {
      least += w.least
      most += w.most
    } else if (k.tag === 'br') {
      most = Math.max(most, run)
      run = 0
    } else if (k.kind === 'text' || sitsOnALine(layout, kid, k)) {
      run += w.most
      least = Math.max(least, w.least)
    } else {
      most = Math.max(most, run, w.most)
      least = Math.max(least, w.least)
      run = 0
    }
  }
  most = Math.max(most, run)
  if (type.nowrap) least = most
  const edge = box.across + (isCell ? cellBorderOf(layout, at, style, type) * 2 : 0)
  least += edge
  most += edge
  const widthAttr = t.tag === 'table' || isCell ? attrOf(layout, at, 'width') : null
  const stated = statedWidth(style, widthAttr, type.size, null)
  if (stated !== null) {
    most = isCell ? Math.max(most, stated) : stated
    // `BASE_STYLE`'s `table { max-width: 100% }` does not let a `width="600"` in
    // a cell shrink: the cell holds it open, and the page runs off to the side.
    if (!isCell) least = stated
  }
  const limit = length(style.get('max-width')?.value, type.size, null)
  if (limit !== null) most = Math.min(most, limit)
  const floor = length(style.get('min-width')?.value, type.size, null)
  if (floor !== null) {
    least = Math.max(least, floor)
    most = Math.max(most, floor)
  }
  return { least: Math.min(least, most), most }
}

function sitsOnALine(layout: Layout, at: number, t: Token): boolean {
  const style = styleOf(layout, at)
  const display = displayOf(t.tag, style)
  return display === 'inline' || sidesOn(layout, at, t.tag, style, display) !== null
}

function tableOf(layout: Layout, cell: number): number {
  let p = layout.tokens[cell]?.parent ?? -1
  while (p >= 0 && layout.tokens[p]?.tag !== 'table') p = layout.tokens[p]?.parent ?? -1
  return p
}

/** The `cellpadding` of the table a cell is in, or a browser's own 1px. */
function cellPaddingOf(layout: Layout, cell: number, type: Type): number {
  return length(attrOf(layout, tableOf(layout, cell), 'cellpadding'), type.size, null) ?? 1
}

/** The line a `<table border="1">` draws around each of its cells, per side. A
 *  word has to fit inside it too, or a column at its narrowest wraps every row. */
function cellBorderOf(layout: Layout, cell: number, style: Declared, type: Type): number {
  if (style.has('border-top-width')) return 0
  const border = length(attrOf(layout, tableOf(layout, cell), 'border'), type.size, null) ?? 0
  return border > 0 ? 1 : 0
}

/** A picture that text flows around, by its style or the older `align`. */
function floatsBeside(layout: Layout, at: number, style: Declared): boolean {
  const float = style.get('float')?.value.trim().toLowerCase()
  if (float) return float === 'left' || float === 'right'
  const align = attrOf(layout, at, 'align')?.toLowerCase()
  return align === 'left' || align === 'right'
}

/** Whether `at` sits beside things rather than under them. Old templates float
 *  their columns with `<table align="left">`, newer ones use `inline-block`, and
 *  a lone `display: table-cell` sits in a row a browser makes up for it. */
function sidesOn(
  layout: Layout,
  at: number,
  tag: string,
  style: Declared,
  display: string,
): 'floats' | 'inline' | null {
  const float = style.get('float')?.value.trim().toLowerCase()
  if (float === 'left' || float === 'right') return 'floats'
  if (display === 'table-cell') return 'floats'
  if (tag === 'table') {
    const align = attrOf(layout, at, 'align')?.toLowerCase()
    if (align === 'left' || align === 'right') return 'floats'
  }
  return display === 'inline-block' || display === 'inline-table' ? 'inline' : null
}

/** A run of children: lines of inline things, and blocks that stack under them.
 *  `contained` is a box margins cannot collapse out of, like a table's cell. */
function flow(
  layout: Layout,
  children: number[],
  width: number,
  type: Type,
  contained: boolean,
  depth: number,
): Flowed {
  const room = Math.max(1, width)
  let height = 0
  let top = 0
  let pending = 0
  let started = false
  let lineX = 0
  let lineHeight = 0
  /** The line so far is floated boxes: space between those takes no room. */
  let floating = false
  /** A picture floated to one side: lines this high up are that much shorter. */
  let besideBottom = 0
  let besideWidth = 0
  const lineRoom = () => (height < besideBottom ? room - besideWidth : room)
  /** What goes first on a line and does not fit beside the picture goes below it. */
  const startLineFor = (w: number) => {
    if (lineX === 0 && w > lineRoom()) height = Math.max(height, besideBottom)
  }

  const endLine = () => {
    lineX = 0
    floating = false
    if (lineHeight === 0) return
    if (started || contained) height += pending
    else top = Math.max(top, pending)
    pending = 0
    height += lineHeight
    started = true
    lineHeight = 0
  }

  const place = (w: number, h: number, leading: number) => {
    if (lineX > 0 && lineX + w > lineRoom()) endLine()
    startLineFor(w)
    lineX += w
    lineHeight = Math.max(lineHeight, h, leading)
  }

  const fill = (w: number, own: Type) => {
    lineHeight = Math.max(lineHeight, own.leading)
    if (own.nowrap) {
      lineX += w
      return
    }
    if (lineX > 0 && lineX + w > lineRoom()) {
      endLine()
      lineHeight = own.leading
    }
    startLineFor(w)
    const across = lineRoom()
    if (w <= across) {
      lineX += w
      return
    }
    // A word wider than the line breaks: `overflow-wrap` in `BASE_STYLE`. All at
    // once, since `letter-spacing` can make a word as wide as a sender likes.
    const whole = Math.ceil(w / across) - 1
    lineHeight = own.leading * whole
    endLine()
    lineHeight = own.leading
    lineX = w - whole * across
  }

  const words = (list: string[], own: Type) => {
    const space = widthOf(layout, ' ', own)
    list.forEach((word, i) => {
      if (i > 0 && lineX > 0) lineX += space
      if (word) fill(widthOf(layout, word, own), own)
    })
  }

  const text = (raw: string, own: Type) => {
    if (own.pre) {
      floating = false
      const rows = decodeEntities(raw).split('\n')
      rows.forEach((row, i) => {
        const last = i === rows.length - 1
        // A newline at the very end ends a line. It does not start another.
        if (row === '' && last) return
        lineHeight = Math.max(lineHeight, own.leading)
        const list = row.split(' ')
        if (list.length > MOST_MEASURED_WORDS) {
          fill(lengthOnScreen(row) * own.size * CHAR_EM, own)
        } else {
          words(list, own)
        }
        if (!last) endLine()
      })
      return
    }
    if (!textIsDrawn(raw)) {
      // Space between two inline things on a line still takes room on it.
      if (lineX > 0 && raw.length > 0 && !floating) lineX += widthOf(layout, ' ', own)
      return
    }
    floating = false
    const collapsed = decodeEntities(raw).replace(/[ \t\r\n\f]+/g, ' ')
    const list = collapsed.split(' ').filter(Boolean)
    if (list.length === 0) {
      fill(widthOf(layout, ' ', own), own)
      return
    }
    if (list.length > MOST_MEASURED_WORDS) {
      fill(lengthOnScreen(collapsed) * own.size * CHAR_EM, own)
      return
    }
    const space = widthOf(layout, ' ', own)
    if (collapsed.startsWith(' ') && lineX > 0) lineX += space
    words(list, own)
    if (collapsed.endsWith(' ') && lineX > 0) lineX += space
  }

  const stack = (child: Flowed) => {
    // A box with nothing in it lets its margins collapse straight through.
    if (child.height === 0) {
      pending = Math.max(pending, child.top, child.bottom)
      return
    }
    // A block beside a floated picture really does wrap around it, but its lines
    // are worked out elsewhere, at full width. Below it errs tall, not short.
    height = Math.max(height, besideBottom)
    if (started || contained) height += Math.max(pending, child.top)
    else top = Math.max(top, pending, child.top)
    height += child.height
    pending = child.bottom
    started = true
  }

  const walk = (list: number[], own: Type, level: number) => {
    for (const at of list) {
      const t = layout.tokens[at]
      if (!t) continue
      if (t.kind === 'text') {
        text(layout.html.slice(t.at, t.end), own)
        continue
      }
      const style = styleOf(layout, at)
      const display = displayOf(t.tag, style)
      if (display === 'none' || NOT_DRAWN.has(t.tag) || level > MAX_DEPTH) continue
      if (t.tag === 'br') {
        if (lineHeight === 0) lineHeight = own.leading
        endLine()
        continue
      }
      const childType = typeOf(layout, at, style, own)
      if (t.tag === 'img') {
        const drawn = picture(layout, at, style, childType, room)
        if (!drawn) continue
        if (floatsBeside(layout, at, style)) {
          // Its margins and `hspace` are small beside what is being judged here,
          // and a line either way, so they are left out.
          const top = height + pending
          if (top >= besideBottom) {
            besideWidth = drawn.width
            besideBottom = top + drawn.height
          } else if (besideWidth + drawn.width <= room) {
            besideWidth += drawn.width
            besideBottom = Math.max(besideBottom, top + drawn.height)
          } else {
            // No room beside the last one, so it starts under it.
            besideWidth = Math.max(besideWidth, drawn.width)
            besideBottom += drawn.height
          }
          continue
        }
        if (display === 'block') {
          endLine()
          stack({ height: drawn.height, top: 0, bottom: 0 })
        } else {
          // A picture sits on the line's baseline, over a descender's room.
          const gap = (own.leading - own.size) / 2 + own.size * 0.2
          floating = false
          place(drawn.width, drawn.height + Math.max(0, gap), own.leading)
        }
        continue
      }
      const widthAttr = t.tag === 'table' || CELL.has(t.tag) ? attrOf(layout, at, 'width') : null
      const side = sidesOn(layout, at, t.tag, style, display)
      if (side) {
        const margin = margins(t.tag, style, childType, room, false)
        const box = padding(style, t.tag, childType.size, room)
        // A table's stated width takes its own padding in, and `tableHeight`
        // takes it back out.
        const inside = t.tag === 'table' ? 0 : box.across
        const around = margin.across + inside
        const fit = contentWidth(style, widthAttr, childType.size, room, around)
        const wanted = Math.max(0, wants(layout, at, own, level + 1).most - inside)
        const content = fit.stated ? fit.width : Math.min(fit.width, wanted)
        const laid = boxHeight(layout, at, content, childType, style, level + 1)
        const across = Math.min(room, content + around)
        floating = side === 'floats'
        place(across, laid + margin.top + margin.bottom, floating ? 0 : own.leading)
        floating = side === 'floats'
        continue
      }
      if (display === 'inline' || display === 'contents') {
        walk(layout.kids.get(at) ?? [], childType, level + 1)
        continue
      }
      endLine()
      const margin = margins(t.tag, style, childType, room, inList(layout, at))
      if (display === 'table' && (t.tag === 'table' || holdsRows(layout, at))) {
        const fit = contentWidth(style, widthAttr, childType.size, room, margin.across)
        stack({
          height: tableHeight(layout, at, Math.min(fit.width, room), childType, style, level + 1),
          top: margin.top,
          bottom: margin.bottom,
        })
        continue
      }
      const box = padding(style, t.tag, childType.size, room)
      if (t.tag === 'hr') {
        const rule = (side: string) =>
          length(style.get(`border-${side}-width`)?.value, childType.size, room) ?? 1
        stack({ height: rule('top') + rule('bottom'), top: margin.top, bottom: margin.bottom })
        continue
      }
      const fit = contentWidth(style, widthAttr, childType.size, room, box.across + margin.across)
      // A closed `<details>` has its room kept: a frame cannot grow when it is
      // opened, and it would be read through a slot.
      const inside = flow(
        layout,
        layout.kids.get(at) ?? [],
        fit.width,
        childType,
        box.top > 0 || box.bottom > 0 || style.has('height'),
        level + 1,
      )
      const content = clamp(inside.height, style, childType.size)
      stack({
        height: content + box.top + box.bottom,
        top: box.top > 0 ? margin.top : Math.max(margin.top, inside.top),
        bottom: box.bottom > 0 ? margin.bottom : Math.max(margin.bottom, inside.bottom),
      })
    }
  }

  walk(children, type, depth)
  endLine()
  height = Math.max(height, besideBottom)
  if (contained) return { height: height + pending, top: 0, bottom: 0 }
  return { height, top, bottom: pending }
}

function inList(layout: Layout, at: number): boolean {
  for (let p = layout.tokens[at]?.parent ?? -1; p >= 0; p = layout.tokens[p]?.parent ?? -1) {
    if (LISTS.has(layout.tokens[p]?.tag ?? '')) return true
  }
  return false
}

/** A block's stated height, and its limits. */
function clamp(content: number, style: Declared, em: number): number {
  const stated = length(style.get('height')?.value, em, null)
  const least = length(style.get('min-height')?.value, em, null)
  const most = length(style.get('max-height')?.value, em, null)
  let height = stated ?? content
  if (least !== null) height = Math.max(height, least)
  if (most !== null) height = Math.min(height, most)
  return height
}

/** An `inline-block` or floated box: its padding around what flows inside it. */
function boxHeight(
  layout: Layout,
  at: number,
  width: number,
  type: Type,
  style: Declared,
  depth: number,
): number {
  const t = layout.tokens[at]
  if (t?.tag === 'table') return tableHeight(layout, at, width, type, style, depth)
  if (displayOf(t?.tag ?? '', style) === 'inline-table' && holdsRows(layout, at)) {
    return tableHeight(layout, at, width, type, style, depth)
  }
  const box = padding(style, t?.tag ?? '', type.size, width)
  const inside = flow(layout, layout.kids.get(at) ?? [], width, type, true, depth)
  return clamp(inside.height, style, type.size) + box.top + box.bottom
}

// ── Tables ────────────────────────────────────────────────────────────

function elementsOf(layout: Layout, at: number): number[] {
  return (layout.kids.get(at) ?? []).filter((k) => {
    const kind = layout.tokens[k]?.kind
    return kind === 'open' || kind === 'void'
  })
}

function isRowPart(layout: Layout, at: number): boolean {
  const tag = layout.tokens[at]?.tag ?? ''
  const display = displayOf(tag, styleOf(layout, at))
  return (
    tag === 'tr' ||
    PART_OF_TABLE.has(tag) ||
    display === 'table-row' ||
    display === 'table-cell' ||
    ROW_GROUP.test(display)
  )
}

/** Whether a `display: table` that is not a `<table>` holds anything a table is
 *  made of. One that does not is a block by another name. */
function holdsRows(layout: Layout, at: number): boolean {
  return elementsOf(layout, at).some((k) => isRowPart(layout, k))
}

function rowsOf(layout: Layout, at: number): number[] {
  const rows: number[] = []
  for (const kid of elementsOf(layout, at)) {
    const tag = layout.tokens[kid]?.tag ?? ''
    const display = displayOf(tag, styleOf(layout, kid))
    if (display === 'none') continue
    if (tag === 'tr' || display === 'table-row') rows.push(kid)
    else if (PART_OF_TABLE.has(tag) || ROW_GROUP.test(display)) rows.push(...rowsOf(layout, kid))
  }
  return rows
}

function cellsOf(layout: Layout, row: number): number[] {
  return elementsOf(layout, row).filter((c) => {
    const tag = layout.tokens[c]?.tag ?? ''
    return displayOf(tag, styleOf(layout, c)) !== 'none' && !NOT_DRAWN.has(tag)
  })
}

/** Whether a row's cells really are a row's. The usual way to make a template fit
 *  a phone is `td { display: block }`, and those stack. */
function rowIsCells(layout: Layout, cells: number[]): boolean {
  return cells.every(
    (c) => displayOf(layout.tokens[c]?.tag ?? '', styleOf(layout, c)) === 'table-cell',
  )
}

function spanOf(layout: Layout, cell: number): number {
  const span = parseInt(attrOf(layout, cell, 'colspan') ?? '1', 10)
  return span >= 1 ? Math.min(span, MOST_COLUMNS_SPANNED) : 1
}

function sum(values: number[]): number {
  return values.reduce((total, v) => total + v, 0)
}

/** `values` taken down towards `floors` until they fit `room`, if they have to. */
function squeeze(values: number[], floors: number[], room: number): number[] {
  const total = sum(values)
  if (total <= room) return values
  const floor = sum(floors)
  if (floor >= room) return floors
  const share = (room - floor) / (total - floor)
  return values.map((v, i) => {
    const least = floors[i] ?? 0
    return least + (v - least) * share
  })
}

/**
 * A table's columns, shared out the way a browser does it: a column is as wide as
 * its widest cell wants, what is stated comes first, nothing goes under its
 * longest word, and a table fills its width whatever its cells say.
 */
function columnWidths(
  layout: Layout,
  rows: { row: number; cells: number[]; type: Type }[],
  room: number,
  depth: number,
): number[] {
  const columns: { least: number; most: number; stated: number | null }[] = []
  const column = (i: number) => {
    let held = columns[i]
    if (!held) {
      held = { least: 0, most: 0, stated: null }
      columns[i] = held
    }
    return held
  }
  const spanning: { start: number; span: number; want: Wants }[] = []
  for (const { cells, type } of rows) {
    let at = 0
    for (const cell of cells) {
      const span = spanOf(layout, cell)
      spend(layout, span)
      const want = wants(layout, cell, type, depth + 1)
      if (span === 1) {
        const held = column(at)
        held.least = Math.max(held.least, want.least)
        held.most = Math.max(held.most, want.most)
        if (held.stated === null) {
          const style = styleOf(layout, cell)
          held.stated = statedWidth(style, attrOf(layout, cell, 'width'), type.size, room)
        }
      } else {
        spanning.push({ start: at, span, want })
      }
      at += span
    }
  }
  for (const { start, span, want } of spanning) {
    let least = 0
    let most = 0
    for (let i = 0; i < span; i++) {
      least += column(start + i).least
      most += column(start + i).most
    }
    for (let i = 0; i < span; i++) {
      const held = column(start + i)
      held.least += Math.max(0, want.least - least) / span
      held.most += Math.max(0, want.most - most) / span
    }
  }
  if (columns.length === 0) return []
  const filled = Array.from({ length: columns.length }, (_, i) => column(i))
  const leasts = filled.map((c) => c.least)
  const stated = filled.map((c) => (c.stated === null ? null : Math.max(c.stated, c.least)))
  const claimed = sum(stated.map((w) => w ?? 0))
  if (!stated.includes(null)) {
    const widths = stated.map((w) => w ?? 0)
    if (claimed >= room) return squeeze(widths, leasts, room)
    // With nobody else to give the rest to, a stated width is only a share.
    if (claimed <= 0) return widths.map(() => room / widths.length)
    return widths.map((w) => (w * room) / claimed)
  }
  const free = Math.max(0, room - claimed)
  const autos = filled.filter((c) => c.stated === null)
  const autoMost = sum(autos.map((c) => c.most))
  const autoLeast = sum(autos.map((c) => c.least))
  const widths = filled.map((c, i) => {
    const own = stated[i]
    if (own !== null && own !== undefined) return own
    if (autoMost <= free) {
      return autoMost > 0 ? c.most + ((free - autoMost) * c.most) / autoMost : free / autos.length
    }
    if (autoLeast >= free) return c.least
    return c.least + ((free - autoLeast) * (c.most - c.least)) / (autoMost - autoLeast)
  })
  return squeeze(widths, leasts, room)
}

function tableHeight(
  layout: Layout,
  at: number,
  width: number,
  type: Type,
  style: Declared,
  depth: number,
): number {
  const isTable = layout.tokens[at]?.tag === 'table'
  const collapsed = /^collapse/i.test(style.get('border-collapse')?.value.trim() ?? '')
  const spacingValue = style.get('border-spacing')?.value
  const spacing = collapsed
    ? 0
    : (length(spacingValue?.trim().split(/\s+/).pop(), type.size, null) ??
      length(attrOf(layout, at, 'cellspacing'), type.size, null) ??
      (isTable ? 2 : 0))
  const cellPadding = isTable
    ? (length(attrOf(layout, at, 'cellpadding'), type.size, null) ?? 1)
    : 0
  const box = padding(style, '', type.size, width)
  const attrBorder = isTable
    ? Math.max(0, length(attrOf(layout, at, 'border'), type.size, null) ?? 0)
    : 0
  // `<table border="1">` draws a line around each of its cells as well.
  const cellBorder = attrBorder > 0 ? 1 : 0
  const inner = Math.max(0, width - box.across - attrBorder * 2)

  // A `display: table` with its cells straight inside has a row a browser makes
  // up for them, and here that row is itself.
  const found = rowsOf(layout, at)
  const rowList = found.length > 0 || isTable ? found : [at]
  const rows = rowList.map((row) => ({
    row,
    cells: cellsOf(layout, row),
    type: row === at ? type : typeOf(layout, row, styleOf(layout, row), type),
  }))
  const grid = rows.filter(({ cells }) => rowIsCells(layout, cells))
  const columnCount = Math.max(
    0,
    ...grid.map(({ cells }) => sum(cells.map((c) => spanOf(layout, c)))),
  )
  const widths = columnWidths(layout, grid, inner - spacing * (columnCount + 1), depth)

  let total = 0
  for (const { row, cells, type: rowType } of rows) {
    if (!rowIsCells(layout, cells)) {
      total += flow(layout, cells, inner, rowType, true, depth + 1).height
      continue
    }
    let tallest = 0
    let column = 0
    for (const cell of cells) {
      const span = spanOf(layout, cell)
      spend(layout, span)
      let cellRoom = spacing * (span - 1)
      for (let i = 0; i < span; i++) cellRoom += widths[column + i] ?? 0
      column += span
      const cellStyle = styleOf(layout, cell)
      const cellType = typeOf(layout, cell, cellStyle, rowType)
      const pad = padding(cellStyle, '', cellType.size, inner, cellPadding)
      const drawnBorder = cellStyle.has('border-top-width') ? 0 : cellBorder
      const content = flow(
        layout,
        layout.kids.get(cell) ?? [],
        Math.max(0, cellRoom - pad.across - drawnBorder * 2),
        cellType,
        true,
        depth + 1,
      )
      const stated =
        length(cellStyle.get('height')?.value, cellType.size, null) ??
        length(attrOf(layout, cell, 'height'), cellType.size, null)
      const least = length(cellStyle.get('min-height')?.value, cellType.size, null) ?? 0
      const rowspan = Math.max(1, parseInt(attrOf(layout, cell, 'rowspan') ?? '1', 10) || 1)
      const lines = collapsed ? drawnBorder : drawnBorder * 2
      const height = Math.max(content.height + pad.top + pad.bottom + lines, stated ?? 0, least)
      tallest = Math.max(tallest, height / rowspan)
    }
    const rowStated =
      row === at
        ? null
        : (length(styleOf(layout, row).get('height')?.value, rowType.size, null) ??
          length(attrOf(layout, row, 'height'), rowType.size, null))
    total += Math.max(tallest, rowStated ?? 0)
  }
  if (rows.length > 0) total += spacing * (rows.length + 1)
  // A `<caption>`, or what a `display: table` holds beside its rows.
  const loose = (layout.kids.get(at) ?? []).filter((k) => {
    const kind = layout.tokens[k]?.kind
    if (kind === 'text') return !isTable
    return (kind === 'open' || kind === 'void') && !isRowPart(layout, k)
  })
  total += flow(layout, loose, inner, type, true, depth + 1).height
  total += box.top + box.bottom + attrBorder * 2
  const stated =
    length(style.get('height')?.value, type.size, null) ??
    (isTable ? length(attrOf(layout, at, 'height'), type.size, null) : null)
  return Math.max(total, stated ?? 0)
}

// ── What `MailThread.svelte` calls ────────────────────────────────────

/** What a body starts out as before a sheet says otherwise. `BASE_STYLE` in
 *  `mailview.rs` does, in every document that is really shown. */
const BODY_TYPE: Type = {
  size: 15,
  leading: 24,
  ratio: 1.6,
  pre: false,
  nowrap: false,
  face: 'sans-serif',
  bold: false,
  italic: false,
  spacing: 0,
  upper: false,
}

/** What this needs to know of a body: `RenderedBody`, from `renderBody`. */
export interface BodyToMeasure {
  html: string
  /** On a page of its own, with that page's padding. Otherwise on the card, whose
   *  sheet takes the margins off the first and last things in a message. */
  paper: boolean
  /** Whether a picture nobody has allowed yet is left out. Its rule in the sheet
   *  is about an attribute, which is not a selector this follows. */
  imagesHidden: boolean
}

/** Past this, a frame is not the problem. */
const MOST_PX = 60_000

/** What the type really came to, and fractions of a pixel. It errs tall, since a
 *  little blank costs less than a scrollbar inside the thread's own. */
const SLACK_RATIO = 1.02
const SLACK_PX = 8

/** `body>:first-child` and the rest of what `readForTheme` writes for a message on
 *  the card, whose selectors are not ones a sheet here follows. A leading `<style>`
 *  is the first child, so the first element after it gets `body>style:first-child+*`
 *  and nothing goes further in. */
function cardEdges(tokens: Token[], kids: Map<number, number[]>): Parsed['edges'] {
  const edges: Parsed['edges'] = new Map()
  const add = (at: number | undefined, property: string, weight: number) => {
    if (at === undefined) return
    const list = edges.get(at)
    if (list) list.push([property, weight])
    else edges.set(at, [[property, weight]])
  }
  const elements = (at: number) =>
    (kids.get(at) ?? []).filter((k) => tokens[k]?.kind === 'open' || tokens[k]?.kind === 'void')
  const top = elements(-1)
  const first = top[0]
  const last = top[top.length - 1]
  const sheetFirst =
    first !== undefined && tokens.some((t, i) => t.kind === 'raw' && t.parent === -1 && i < first)
  add(first, 'margin-top', 11)
  if (first !== undefined && !sheetFirst) add(elements(first)[0], 'margin-top', 21)
  add(last, 'margin-bottom', 11)
  if (last !== undefined) {
    const inside = elements(last)
    add(inside[inside.length - 1], 'margin-bottom', 21)
  }
  return edges
}

function newPen(): CanvasRenderingContext2D | null {
  try {
    return typeof document === 'undefined'
      ? null
      : document.createElement('canvas').getContext('2d')
  } catch {
    return null
  }
}

function parse(body: BodyToMeasure): Parsed {
  const opened = /<body[^>]*>/i.exec(body.html)
  const start = opened ? opened.index + opened[0].length : 0
  const end = body.html.lastIndexOf('</body>')
  const head = body.html.slice(0, opened?.index ?? 0)
  const inner = body.html.slice(start, end >= start ? end : undefined)
  const tokens = tokenize(inner)
  const kids = kidsOf(tokens)
  const sheets = [...head.matchAll(/<style[^>]*>([\s\S]*?)<\/style>/gi)].map((m) => m[1] ?? '')
  for (const t of tokens) {
    if (t.kind === 'raw') sheets.push(inner.slice(t.at, t.end))
  }
  return {
    html: inner,
    tokens,
    kids,
    sheets: sheets.map(withoutComments),
    classes: new Map(),
    attributes: new Map(),
    inline: new Map(),
    edges: body.paper ? new Map<number, [string, number][]>() : cardEdges(tokens, kids),
    pen: newPen(),
    penFont: '',
    measured: new Map(),
    imagesHidden: body.imagesHidden,
    pictureSizes: new Map(),
  }
}

function layOut(parsed: Parsed, widthPx: number): number {
  const reader: SheetReader = { sheet: new Map(), rules: 0 }
  for (const css of parsed.sheets) readSheet(css, widthPx, reader)
  const layout: Layout = {
    ...parsed,
    sheet: reader.sheet,
    styles: new Map(),
    wanted: new Map(),
    steps: 0,
  }

  // The body element is not in the markup, but a sheet still reaches it: taking
  // off `BASE_STYLE`'s padding is usually the first thing a template does.
  const bodyStyle: Declared = new Map()
  for (const rule of reader.sheet.get('body') ?? []) {
    if (rule.path.length <= 2) declareAll(bodyStyle, rule.declared, rule.weight)
  }
  for (const rule of reader.sheet.get('html') ?? []) {
    if (rule.path.length > 1) continue
    declareAll(
      bodyStyle,
      rule.declared.filter((d) => /^(?:font-|line-height)/.test(d.property)),
      -1,
    )
  }
  const type = typeOf(layout, -1, bodyStyle, BODY_TYPE)
  const box = padding(bodyStyle, 'body', type.size, widthPx)
  const margin = margins('body', bodyStyle, type, widthPx, false)
  const around = box.across + margin.across
  const room = contentWidth(bodyStyle, null, type.size, widthPx, around).width
  const flowed = flow(layout, layout.kids.get(-1) ?? [], room, type, true, 0)
  return flowed.height + box.top + box.bottom + margin.top + margin.bottom
}

/** How tall the frame for a body should be, at any width. */
export interface BodyHeight {
  (widthPx: number): number
  /** The addresses of the pictures the message draws. */
  pictures(): string[]
  /** How big a picture turned out to be, for every height asked for after this. */
  pictureArrived(address: string, width: number, height: number): void
}

/**
 * How tall the frame for `body` should be, at any width. The markup is read once
 * and kept in what this returns, and only there, for as long as the caller holds
 * it. Neither this nor what it returns ever throws: this runs while a thread is
 * being drawn, on a stranger's markup.
 */
export function measureBody(body: BodyToMeasure): BodyHeight {
  const fallback = Math.max(400, Math.min(MOST_PX, Math.ceil(body.html.length / 8)))
  let parsed: Parsed | null
  try {
    parsed = parse(body)
  } catch {
    parsed = null
  }
  const heightAt = (widthPx: number) => {
    if (!parsed) return fallback
    try {
      const laid = layOut(parsed, widthPx)
      if (!Number.isFinite(laid)) return MOST_PX
      return Math.min(MOST_PX, Math.ceil(Math.max(0, laid) * SLACK_RATIO) + SLACK_PX)
    } catch {
      // Too much work, or too deep, which will be no less at the next width.
      parsed = null
      return fallback
    }
  }
  return Object.assign(heightAt, {
    pictures() {
      const read = parsed
      if (!read) return []
      const drawn = new Set<string>()
      read.tokens.forEach((token, at) => {
        if (token.tag === 'img' && pictureIsDrawn(read, at)) {
          drawn.add(attrOf(read, at, 'src') ?? '')
        }
      })
      return [...drawn]
    },
    pictureArrived(address: string, width: number, height: number) {
      // A picture with no size of its own, as an SVG can be, says nothing new.
      if (width > 0 && height > 0) parsed?.pictureSizes.set(address, { width, height })
    },
  })
}

/** Pictures tend to arrive together, and laying a message out again is real work:
 *  this much quiet, so a dozen of them are one more layout rather than twelve. */
const PICTURES_SETTLE_MS = 150

/** A stranger's markup can name any number of them, and every one is a request. */
const MOST_PICTURES_READ = 200

/** The pictures of a message that have arrived, by address. */
export type PictureSizes = Map<string, { width: number; height: number }>

/**
 * Load `height`'s pictures, keeping only their sizes, and call `arrived` when
 * that has changed the answer. Hands back what stops it.
 *
 * `known` outlives any one `height`: a theme or the history opening changes the
 * markup and not the pictures, and asking again would drop the frame to a guess
 * until they had all come back.
 */
export function readPictures(
  height: BodyHeight,
  arrived: () => void,
  known: PictureSizes = new Map(),
  newImage: () => HTMLImageElement = () => new Image(),
): () => void {
  let waiting: ReturnType<typeof setTimeout> | null = null
  const wanted = height.pictures().slice(0, MOST_PICTURES_READ)
  const unread = wanted.filter((address) => {
    const size = known.get(address)
    if (size) height.pictureArrived(address, size.width, size.height)
    return !size
  })
  const reading = unread.map((address) => {
    const image = newImage()
    image.onload = () => {
      const { naturalWidth: width, naturalHeight: tall } = image
      known.set(address, { width, height: tall })
      height.pictureArrived(address, width, tall)
      waiting ??= setTimeout(() => {
        waiting = null
        arrived()
      }, PICTURES_SETTLE_MS)
    }
    image.src = address
    return image
  })
  return () => {
    if (waiting !== null) clearTimeout(waiting)
    for (const image of reading) {
      image.onload = null
      image.src = ''
    }
  }
}
