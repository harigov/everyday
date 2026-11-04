// What a stored message body looks like once it is being read: where the
// sender's own words end and the history they replied over begins, whether
// the message was designed for a page of its own, and -- when it was not --
// its colours moved onto the app's page.
//
// All of it happens here, at display, rather than in `everyday-mail`'s
// sanitiser at sync: which theme is on and whether the history is open are
// this window's to know, and a body already stored is sealed exactly as it
// was written and is never sanitised again, so a pass at sync would miss
// every message anybody already has.
//
// This reads `sanitize::sanitize`'s output, not a stranger's markup: ammonia
// has already been through it, so its tags balance and its attributes are
// quoted, which is why a small tokenizer is enough. Nothing here is a wall.
// The result still goes into the same sandboxed frame under the same content
// security policy, so a misreading costs a badly drawn message and no more.

// ── Tokens ────────────────────────────────────────────────────────────

interface Token {
  kind: 'open' | 'close' | 'void' | 'text' | 'raw' | 'comment'
  /** Offsets into the HTML this came from. */
  at: number
  end: number
  /** Lower case. Empty for a text or a comment. */
  tag: string
  /** The enclosing element's `open` token, or -1 at the top. */
  parent: number
  /** An `open`'s `close` and a `close`'s `open`. -1 when never closed. */
  pair: number
}

const TAG =
  /<!--[\s\S]*?-->|<\/?([a-zA-Z][^\s/>]*)(?:\s+[^\s=>/]+(?:\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]*))?)*\s*\/?>/g

const VOID = new Set('area base br col embed hr img input link meta source track wbr'.split(' '))
const BLOCK = new Set(
  (
    'table tbody thead tfoot tr td th div p center section article header footer aside main nav ' +
    'ul ol li dl dt dd h1 h2 h3 h4 h5 h6 blockquote pre hr details summary figure figcaption address'
  ).split(' '),
)

function tokenize(html: string): Token[] {
  const tokens: Token[] = []
  const open: number[] = []
  const top = () => open[open.length - 1] ?? -1
  const text = (at: number, end: number) => {
    if (end > at) tokens.push({ kind: 'text', at, end, tag: '', parent: top(), pair: -1 })
  }
  let pos = 0
  TAG.lastIndex = 0
  for (let m = TAG.exec(html); m; m = TAG.exec(html)) {
    text(pos, m.index)
    const end = m.index + m[0].length
    pos = end
    if (m[0].startsWith('<!--')) {
      tokens.push({ kind: 'comment', at: m.index, end, tag: '', parent: top(), pair: -1 })
      continue
    }
    const tag = (m[1] ?? '').toLowerCase()
    if (m[0].startsWith('</')) {
      let at = open.length - 1
      while (at >= 0 && tokens[open[at] ?? -1]?.tag !== tag) at--
      if (at < 0) continue
      const index = tokens.length
      const matching = open[at] ?? -1
      for (const i of open.splice(at)) {
        const opened = tokens[i]
        if (opened) opened.pair = index
      }
      tokens.push({ kind: 'close', at: m.index, end, tag, parent: top(), pair: matching })
    } else if (VOID.has(tag)) {
      tokens.push({ kind: 'void', at: m.index, end, tag, parent: top(), pair: -1 })
    } else if (tag === 'style') {
      // Not markup: a `>` selector in here is not the end of anything.
      const close = html.toLowerCase().indexOf('</style', end)
      const stop = close < 0 ? html.length : close
      tokens.push({ kind: 'raw', at: end, end: stop, tag, parent: top(), pair: -1 })
      const after = html.indexOf('>', stop)
      pos = after < 0 ? html.length : after + 1
      TAG.lastIndex = pos
    } else {
      open.push(tokens.length)
      tokens.push({
        kind: 'open',
        at: m.index,
        end,
        tag,
        parent: open[open.length - 2] ?? -1,
        pair: -1,
      })
    }
  }
  text(pos, html.length)
  return tokens
}

function attribute(source: string, name: string): string | null {
  const m = new RegExp(`\\s${name}\\s*=\\s*(?:"([^"]*)"|'([^']*)'|([^\\s>]+))`, 'i').exec(source)
  return m ? decodeEntities(m[1] ?? m[2] ?? m[3] ?? '') : null
}

function withAttribute(source: string, name: string, value: string | null): string {
  const pattern = new RegExp(`\\s${name}\\s*=\\s*(?:"[^"]*"|'[^']*'|[^\\s>]+)`, 'i')
  const written =
    value === null ? '' : ` ${name}="${value.replaceAll('&', '&amp;').replaceAll('"', '&quot;')}"`
  if (pattern.test(source)) return source.replace(pattern, () => written)
  return written
    ? source.replace(/\s*(\/?)>$/, (_, slash: string) => `${written}${slash}>`)
    : source
}

const NAMED_ENTITIES: Record<string, string> = {
  amp: '&',
  lt: '<',
  gt: '>',
  quot: '"',
  apos: "'",
  nbsp: ' ',
}

function decodeEntities(s: string): string {
  if (!s.includes('&')) return s
  return s.replace(
    /&(?:#(\d+)|#x([\da-f]+)|([a-z]+));/gi,
    (whole: string, dec: string, hex: string, name: string) => {
      if (name) return NAMED_ENTITIES[name.toLowerCase()] ?? whole
      const code = dec ? parseInt(dec, 10) : parseInt(hex, 16)
      if (code === 0xa0) return ' '
      return code > 0 && code <= 0x10ffff ? String.fromCodePoint(code) : whole
    },
  )
}

const SUBSTANCE = /[\p{L}\p{N}]/u

/** A word, or a picture -- not blank lines, rules or a lone `&nbsp;`. */
function hasSubstance(html: string, tokens: Token[], from: number, to: number): boolean {
  for (let i = from; i < to; i++) {
    const t = tokens[i]
    if (!t) break
    if (t.kind === 'void' && t.tag === 'img') return true
    if (t.kind === 'text' && SUBSTANCE.test(decodeEntities(html.slice(t.at, t.end)))) return true
  }
  return false
}

// ── Where the history starts ──────────────────────────────────────────

/**
 * The classes mail clients put around the part they quoted, which ends where
 * the element does. `details` is this app's own, for a plain-text message
 * (`mailview.rs`'s `plain_text_body`), and `gmail_quote` is written by more than
 * Gmail -- Superhuman, for one. Not `gmail_extra`, which holds a signature too.
 */
const QUOTE_CLASSES = new Set([
  'gmail_quote',
  'gmail_quote_container',
  'yahoo_quoted',
  'protonmail_quote',
  'tutanota_quote',
  'zmail_extra',
  'ms-outlook-mobile-reference-message',
  'front-blockquote',
])

/**
 * Where Outlook starts what it quoted. Its header is a box of its own with the
 * earlier message's words as siblings under it, so the history runs from here
 * to the end of the message rather than to the end of the element.
 */
const HISTORY_FOLLOWS = new Set([
  'divrplyfwdmsg',
  'x_divrplyfwdmsg',
  'appendonsend',
  'mail-editor-reference-message-container',
  'mail-editor-reference-message',
  'outlookmessageheader',
  'x_outlookmessageheader',
])

function classesOf(source: string): string[] {
  return (attribute(source, 'class') ?? '').toLowerCase().split(/\s+/)
}

function isQuoteElement(source: string, tag: string): boolean {
  if (tag === 'details') return true
  if (tag === 'blockquote' && attribute(source, 'type')?.toLowerCase() === 'cite') return true
  const id = attribute(source, 'id')?.toLowerCase() ?? ''
  return (
    classesOf(source).some((c) => QUOTE_CLASSES.has(c) || c.endsWith('yahoo_quoted')) ||
    id.endsWith('yahoo_quoted')
  )
}

function historyFollows(source: string): boolean {
  const id = attribute(source, 'id')?.toLowerCase() ?? ''
  return HISTORY_FOLLOWS.has(id) || classesOf(source).some((c) => HISTORY_FOLLOWS.has(c))
}

/** The reply-header lines `everyday_mail::text::is_wrote_header` knows, in the
 *  same languages. */
function isWroteLine(line: string): boolean {
  const lower = line.toLowerCase()
  if (!lower.endsWith(':') || lower.length < 8) return false
  return (
    (lower.startsWith('on ') && lower.endsWith('wrote:')) ||
    (lower.startsWith('el ') && lower.includes('escribió')) ||
    (lower.startsWith('le ') && lower.includes('a écrit')) ||
    (lower.startsWith('am ') && lower.includes('schrieb')) ||
    (lower.startsWith('il ') && lower.includes('ha scritto'))
  )
}

const ORIGINAL_MESSAGE = /^-{2,}\s*original message\s*-{2,}$/i
const FORWARDED = /^(-{2,}\s*forwarded message\s*-{2,}|begin forwarded message:?)$/i
const HEADER_FROM = /^(from|von|de|da)\s?:/i
const HEADER_SENT = /^(sent|date|gesendet|datum|envoyé|enviado|fecha|inviato|data)\s?:/i
const HEADER_TO = /^(to|an|à|a|para)\s?:/i
const HEADER_SUBJECT = /^(subject|betreff|objet|asunto|oggetto)\s?:/i

/** One line of what a message shows, and the token it begins at. */
interface Line {
  text: string
  token: number
}

function linesOf(html: string, tokens: Token[]): Line[] {
  const lines: Line[] = []
  let text = ''
  let token = -1
  const flush = () => {
    const trimmed = text.replace(/\s+/g, ' ').trim()
    if (trimmed && token >= 0) lines.push({ text: trimmed, token })
    text = ''
    token = -1
  }
  tokens.forEach((t, i) => {
    if (t.kind === 'open' || t.kind === 'close' || t.kind === 'void') {
      if (BLOCK.has(t.tag) || t.tag === 'br') flush()
      else if (t.kind === 'open' && token < 0) token = i
    } else if (t.kind === 'text') {
      const chunk = decodeEntities(html.slice(t.at, t.end))
      if (token < 0 && chunk.trim()) token = i
      text += chunk
    }
  })
  flush()
  return lines
}

/** An Outlook-style `From:`/`Sent:`/`To:`/`Subject:` block starting at
 *  `lines[at]` -- `everyday_mail::text::find_outlook_block`'s test, read from
 *  rendered lines. Any bold around a label is already gone by this point. */
function isHeaderBlock(lines: Line[], at: number): boolean {
  const window = lines.slice(at, at + 6).map((l) => l.text)
  return (
    HEADER_FROM.test(window[0] ?? '') &&
    window.some((l) => HEADER_SENT.test(l)) &&
    window.some((l) => HEADER_TO.test(l)) &&
    window.some((l) => HEADER_SUBJECT.test(l))
  )
}

/** Up out of every wrapper that holds nothing before this token, so a fold
 *  takes a header's rule and its padded box along with its words. */
function climb(html: string, tokens: Token[], from: number): number {
  let at = from
  for (;;) {
    const parent = tokens[at]?.parent ?? -1
    if (parent < 0 || hasSubstance(html, tokens, parent, at)) return at
    at = parent
  }
}

/**
 * What a list server or a mail gateway stamps on the very end of a message,
 * under everything it was handed. It sits below the history, and without
 * this every word of that history would count as being above the sender's
 * last remark and stay in view.
 */
const STAMPED_FOOTER =
  /confidential|disclaimer|intended recipient|privileged|unsubscribe|you received this|view this (?:message|discussion)|virus/i

/**
 * The token where the history in this message begins, or `null` when nothing
 * should be folded.
 *
 * Only history with nothing of the sender's after it folds. Someone replying
 * inline, or underneath what they quote, has written around the quote, and
 * folding it would take their answer away. A first candidate with words after
 * it is therefore passed over for a later one, and with none left the message
 * is shown whole -- the same trailing-only choice Gmail makes.
 *
 * A forward is shown whole as well: the "history" of a forward is what was
 * sent.
 */
function historyStart(html: string, tokens: Token[], subject: string): number | null {
  if (/^\s*(fwd?|wg|tr|rv)\s*:/i.test(subject)) return null

  // Where a candidate starts, and the token after the last of what it quotes.
  const candidates = new Map<number, number>()
  /** An attribution owns the quote under it and only that, so an answer written
   *  below the quote can still be told from one above it. */
  const throughNextQuote = (at: number): number => {
    const quote = tokens.findIndex((t, i) => i > at && t.kind === 'open' && t.tag === 'blockquote')
    const pair = tokens[quote]?.pair ?? -1
    return pair < 0 ? tokens.length : pair + 1
  }
  tokens.forEach((t, i) => {
    if (t.kind !== 'open') return
    const source = html.slice(t.at, t.end)
    if (historyFollows(source)) candidates.set(i, tokens.length)
    else if (classesOf(source).includes('moz-cite-prefix')) candidates.set(i, throughNextQuote(i))
    else if (isQuoteElement(source, t.tag)) {
      candidates.set(i, t.pair < 0 ? tokens.length : t.pair + 1)
    }
  })
  const lines = linesOf(html, tokens)
  lines.forEach((line, n) => {
    if (FORWARDED.test(line.text)) {
      // A forward with its subject edited: whatever follows was sent.
      candidates.set(line.token, -1)
    } else if (isWroteLine(line.text)) {
      const at = climb(html, tokens, line.token)
      candidates.set(at, throughNextQuote(at))
    } else if (ORIGINAL_MESSAGE.test(line.text) || isHeaderBlock(lines, n)) {
      candidates.set(climb(html, tokens, line.token), tokens.length)
    }
  })

  for (const at of [...candidates.keys()].sort((a, b) => a - b)) {
    const end = candidates.get(at) ?? -1
    if (end < 0) return null
    const firstWords = decodeEntities(
      html.slice(tokens[at]?.at ?? 0, (tokens[at]?.at ?? 0) + 400).replace(/<[^>]*>/g, ' '),
    )
    if (/forwarded message/i.test(firstWords)) return null
    if (!hasSubstance(html, tokens, 0, at)) return null
    if (!hasSubstance(html, tokens, end, tokens.length)) return at
    const after = tokens.slice(end).filter((t) => t.kind === 'text')
    const trailing = after.map((t) => html.slice(t.at, t.end)).join(' ')
    if (STAMPED_FOOTER.test(trailing)) return at
  }
  return null
}

/** Back over the blank lines, rules and empty wrappers just above `from`, so
 *  a fold does not leave a gap the size of a sender's spacing behind it. */
function trimBack(html: string, tokens: Token[], from: number): number {
  let at = from
  for (;;) {
    const t = tokens[at - 1]
    if (!t) return at
    if (t.kind === 'comment' || (t.kind === 'void' && (t.tag === 'br' || t.tag === 'hr'))) {
      at--
    } else if (t.kind === 'open') {
      at--
    } else if (t.kind === 'text' && !decodeEntities(html.slice(t.at, t.end)).trim()) {
      at--
    } else if (t.kind === 'close') {
      const opened = t.pair
      if (opened < 0 || hasSubstance(html, tokens, opened, at)) return at
      at = opened
    } else {
      return at
    }
  }
}

/** `html` up to `tokens[upTo]`, with everything still open there closed. */
function cutAt(html: string, tokens: Token[], upTo: number): string {
  const cutToken = tokens[upTo]
  if (!cutToken) return html
  let out = html.slice(0, cutToken.at)
  for (let p = cutToken.parent; p >= 0; p = tokens[p]?.parent ?? -1) {
    out += `</${tokens[p]?.tag}>`
  }
  return out
}

// ── Colours ───────────────────────────────────────────────────────────

type Rgb = readonly [number, number, number]

const NAMED_COLOURS: Record<string, Rgb> = {
  black: [0, 0, 0],
  white: [255, 255, 255],
  red: [255, 0, 0],
  green: [0, 128, 0],
  lime: [0, 255, 0],
  blue: [0, 0, 255],
  navy: [0, 0, 128],
  teal: [0, 128, 128],
  aqua: [0, 255, 255],
  cyan: [0, 255, 255],
  purple: [128, 0, 128],
  fuchsia: [255, 0, 255],
  magenta: [255, 0, 255],
  maroon: [128, 0, 0],
  olive: [128, 128, 0],
  yellow: [255, 255, 0],
  orange: [255, 165, 0],
  gray: [128, 128, 128],
  grey: [128, 128, 128],
  silver: [192, 192, 192],
  darkblue: [0, 0, 139],
  darkgray: [169, 169, 169],
  darkgrey: [169, 169, 169],
  lightgray: [211, 211, 211],
  lightgrey: [211, 211, 211],
  // Outlook's own: the system's text and page colours, as a Windows desktop
  // answered when the message was written, and always black on white there.
  windowtext: [0, 0, 0],
  window: [255, 255, 255],
}

/** A colour worth reasoning about; `null` for `inherit`, `transparent`, an
 *  almost clear `rgba`, or anything this does not recognise. */
function parseColour(value: string): Rgb | null {
  const v = value
    .replace(/!important/i, '')
    .trim()
    .toLowerCase()
  const named = NAMED_COLOURS[v]
  if (named) return named
  let m = /^#([\da-f]{3,8})$/.exec(v)
  if (m) {
    const hex = m[1] ?? ''
    if (hex.length === 3 || hex.length === 4) {
      const [r, g, b] = [...hex].map((c) => parseInt(c + c, 16))
      if (hex.length === 4 && parseInt(hex.charAt(3) + hex.charAt(3), 16) < 26) return null
      return [r ?? 0, g ?? 0, b ?? 0]
    }
    if (hex.length === 6 || hex.length === 8) {
      if (hex.length === 8 && parseInt(hex.slice(6), 16) < 26) return null
      return [
        parseInt(hex.slice(0, 2), 16),
        parseInt(hex.slice(2, 4), 16),
        parseInt(hex.slice(4, 6), 16),
      ]
    }
    return null
  }
  m = /^rgba?\(\s*([\d.]+%?)[\s,]+([\d.]+%?)[\s,]+([\d.]+%?)(?:[\s,/]+([\d.]+%?))?\s*\)$/.exec(v)
  if (m) {
    const part = (p: string | undefined) =>
      p?.endsWith('%') ? (parseFloat(p) * 255) / 100 : parseFloat(p ?? '0')
    if (m[4] && (m[4].endsWith('%') ? parseFloat(m[4]) / 100 : parseFloat(m[4])) < 0.1) return null
    return [part(m[1]), part(m[2]), part(m[3])]
  }
  return null
}

/** The colour a `background` or `background-color` value paints, when it
 *  paints one -- a shorthand can name it anywhere among its other words. */
function backgroundColour(value: string): Rgb | null {
  // Whitespace inside `rgb(1, 2, 3)` is not a separator between words.
  for (const word of value.match(/[a-z]+\([^)]*\)|[^\s]+/gi) ?? []) {
    const colour = parseColour(word)
    if (colour) return colour
  }
  return null
}

function luminance([r, g, b]: Rgb): number {
  const channel = (c: number) => {
    const s = c / 255
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4
  }
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

function contrast(a: Rgb, b: Rgb): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x)
  return ((hi ?? 0) + 0.05) / ((lo ?? 0) + 0.05)
}

function chroma([r, g, b]: Rgb): number {
  return Math.max(r, g, b) - Math.min(r, g, b)
}

/** White, or close enough that a sender only meant "the page". */
function isPaperWhite(colour: Rgb): boolean {
  return luminance(colour) >= 0.8 && chroma(colour) < 24
}

function hex([r, g, b]: Rgb): string {
  const byte = (n: number) =>
    Math.round(Math.min(255, Math.max(0, n)))
      .toString(16)
      .padStart(2, '0')
  return `#${byte(r)}${byte(g)}${byte(b)}`
}

function toHsl([r, g, b]: Rgb): [number, number, number] {
  const [rn, gn, bn] = [r / 255, g / 255, b / 255]
  const hi = Math.max(rn, gn, bn)
  const lo = Math.min(rn, gn, bn)
  const l = (hi + lo) / 2
  if (hi === lo) return [0, 0, l]
  const d = hi - lo
  const s = l > 0.5 ? d / (2 - hi - lo) : d / (hi + lo)
  const h =
    hi === rn
      ? (gn - bn) / d + (gn < bn ? 6 : 0)
      : hi === gn
        ? (bn - rn) / d + 2
        : (rn - gn) / d + 4
  return [h / 6, s, l]
}

function fromHsl(h: number, s: number, l: number): Rgb {
  const channel = (t: number) => {
    const k = (t + 1) % 1
    const q = l < 0.5 ? l * (1 + s) : l + s - l * s
    const p = 2 * l - q
    if (k < 1 / 6) return p + (q - p) * 6 * k
    if (k < 1 / 2) return q
    if (k < 2 / 3) return p + (q - p) * (2 / 3 - k) * 6
    return p
  }
  return [channel(h + 1 / 3) * 255, channel(h) * 255, channel(h - 1 / 3) * 255]
}

const READABLE = 4.5

/**
 * A text colour readable on `behind`: `null` to leave it as it was, `''` to
 * drop it and take the page's own text colour, or a new one.
 *
 * Black, and the grey next to it, was only ever a sender's way of saying
 * "text", so on the app's own page it becomes the app's text. A colour that
 * meant something -- a navy signature, a blue link -- keeps its hue and
 * moves along lightness until it can be read, which is also where a muted
 * grey lands, so a sign-off stays quieter than the message above it.
 */
function readable(colour: Rgb, behind: Rgb, onPage: boolean): string | null {
  if (contrast(colour, behind) >= READABLE) return null
  const pageIsDark = luminance(behind) < 0.18
  if (onPage && chroma(colour) < 40) {
    const nearest = pageIsDark ? luminance(colour) < 0.12 : luminance(colour) > 0.6
    if (nearest) return ''
  }
  const [h, s, start] = toHsl(colour)
  for (let step = 1; step <= 25; step++) {
    const l = pageIsDark ? start + ((1 - start) * step) / 25 : start * (1 - step / 25)
    const moved = fromHsl(h, s, l)
    if (contrast(moved, behind) >= READABLE) return hex(moved)
  }
  return pageIsDark ? '#ffffff' : '#000000'
}

interface Page {
  background: Rgb
  text: Rgb
}

// ── A style attribute ─────────────────────────────────────────────────

/** Split on `;`, but not the one in `url(data:image/png;base64,…)`. */
function declarations(style: string): [string, string][] {
  const out: [string, string][] = []
  let depth = 0
  let quote = ''
  let start = 0
  const push = (end: number) => {
    const part = style.slice(start, end)
    const colon = part.indexOf(':')
    if (colon > 0)
      out.push([part.slice(0, colon).trim().toLowerCase(), part.slice(colon + 1).trim()])
  }
  for (let i = 0; i < style.length; i++) {
    const c = style.charAt(i)
    if (quote) {
      if (c === quote) quote = ''
    } else if (c === '"' || c === "'") quote = c
    else if (c === '(') depth++
    else if (c === ')') depth--
    else if (c === ';' && depth <= 0) {
      push(i)
      start = i + 1
    }
  }
  push(style.length)
  return out
}

/** A size within reach of body text, so dropping it costs only a mismatch:
 *  Word's 11pt, Gmail's `small`, the 14px of half the clients out there. */
function isBodySize(value: string): boolean {
  const m = /^([\d.]+)(px|pt)$/i.exec(value.trim())
  if (!m) return /^(small|medium)$/i.test(value.trim())
  const size = parseFloat(m[1] ?? '0')
  return m[2]?.toLowerCase() === 'pt' ? size >= 9 && size <= 13 : size >= 12 && size <= 17
}

const MONOSPACE = /mono|courier|consolas|menlo/i

// ── Whether a message was designed ────────────────────────────────────

const PIXELS = /^([\d.]+)(px|pt)?$/i

function widthInPixels(value: string | null | undefined): number {
  const m = PIXELS.exec((value ?? '').trim())
  if (!m) return 0
  return parseFloat(m[1] ?? '0') * (m[2]?.toLowerCase() === 'pt' ? 4 / 3 : 1)
}

/** A sheet only a designer writes: a breakpoint or dark-mode rule (what Word
 *  and Gmail put in a message never has one), or a page with a tint. */
function sheetIsDesigned(css: string): boolean {
  if (/@media[^{]*(?:-width|prefers-color-scheme)/i.test(css)) return true
  for (const [, value] of css.matchAll(/(?:^|[;{\s])background(?:-color)?\s*:\s*([^;}]+)/gi)) {
    if (!value || /^\s*(?:none|transparent|inherit|initial|unset)\b/i.test(value)) continue
    const colour = backgroundColour(value)
    // Stricter than behind a line of text: nothing but a designer tints a whole
    // rule, even by a shade.
    if (!colour || luminance(colour) < 0.97) return true
  }
  return false
}

/**
 * Whether `tokens[..end]` is a designed message -- laid out for a page of its
 * own, which is nearly always white -- rather than somebody writing.
 *
 * Designed means a share of the words sit on a colour the sender chose, or the
 * message opens with a table of a fixed width that holds most of it, or it
 * carries a background picture, several wide pictures, or a sheet only a designer
 * writes. Text colours alone say nothing: Outlook sets black on every span of a
 * two-line note. Neither does white behind a line of text, which is what pasting
 * into Gmail leaves. Nor does a fixed-width table with words above it, which is
 * somebody's signature under what they wrote.
 *
 * Getting this wrong in either direction leaves a message readable. A designed
 * one taken for writing still has its text colours moved and its painted
 * panels kept; writing taken for designed is drawn on white, which is how every
 * message used to be drawn.
 */
function isDesigned(html: string, tokens: Token[], end: number): boolean {
  let total = 0
  let painted = 0
  let fixed = 0
  let wideImages = 0
  let writtenAboveFixed = false
  const inPainted = new Map<number, boolean>()
  const inFixed = new Map<number, boolean>()
  const inHidden = new Map<number, boolean>()
  for (let i = 0; i < end; i++) {
    const t = tokens[i]
    if (!t) break
    if (t.kind === 'raw' && sheetIsDesigned(html.slice(t.at, t.end))) return true
    const fixedHere = inFixed.get(t.parent) ?? false
    if (t.kind === 'text') {
      // A newsletter's preheader comes first, and nobody sees it.
      if (inHidden.get(t.parent)) continue
      const words = decodeEntities(html.slice(t.at, t.end)).replace(/\s+/g, '')
      total += words.length
      if (inPainted.get(t.parent)) painted += words.length
      if (fixedHere) fixed += words.length
      else if (fixed === 0 && SUBSTANCE.test(words)) writtenAboveFixed = true
    } else if (t.kind === 'void' && t.tag === 'img') {
      const source = html.slice(t.at, t.end)
      const style = declarations(attribute(source, 'style') ?? '')
      const width =
        widthInPixels(style.find(([p]) => p === 'width')?.[1]) ||
        widthInPixels(attribute(source, 'width'))
      if (width >= 300) wideImages++
    } else if (t.kind === 'open') {
      const source = html.slice(t.at, t.end)
      const style = declarations(attribute(source, 'style') ?? '')
      if (attribute(source, 'background')) return true
      for (const [property, value] of style) {
        if (property.startsWith('background') && /url\(/i.test(value)) return true
      }
      const fill =
        style.find(([p]) => p === 'background-color' || p === 'background')?.[1] ??
        attribute(source, 'bgcolor')
      const colour = fill ? backgroundColour(fill) : null
      const paints =
        BLOCK.has(t.tag) &&
        fill !== undefined &&
        fill !== null &&
        !/^\s*(?:none|transparent|inherit|initial|unset)\b/i.test(fill) &&
        !(colour && isPaperWhite(colour))
      inPainted.set(i, paints || (inPainted.get(t.parent) ?? false))
      const hides = style.some(([p, v]) => p === 'display' && /^none\b/i.test(v))
      inHidden.set(i, hides || (inHidden.get(t.parent) ?? false))

      let width = 0
      if (t.tag === 'table') {
        width = Math.max(
          widthInPixels(style.find(([p]) => p === 'width')?.[1]),
          widthInPixels(style.find(([p]) => p === 'max-width')?.[1]),
          widthInPixels(attribute(source, 'width')),
        )
      }
      inFixed.set(i, fixedHere || width >= 480)
    }
  }
  if (wideImages >= 3) return true
  if (total > 0 && painted >= 20 && painted / total >= 0.2) return true
  return total > 0 && !writtenAboveFixed && fixed / total >= 0.6
}

// ── Moving a message onto the page ────────────────────────────────────

function adaptSheet(css: string, page: Page): string {
  return css
    .replace(
      /(^|[;{\s])(color\s*:\s*)([^;}]+)/gi,
      (whole, lead: string, property: string, value: string) => {
        const colour = parseColour(value)
        if (!colour) return whole
        const moved = readable(colour, page.background, true)
        if (moved === null) return whole
        return moved ? `${lead}${property}${moved}` : `${lead}color:inherit`
      },
    )
    .replace(
      /(^|[;{\s])background(?:-color)?\s*:\s*([^;}]+)/gi,
      (whole, lead: string, value: string) => {
        const colour = backgroundColour(value)
        return colour && isPaperWhite(colour) ? `${lead}background:transparent` : whole
      },
    )
    .replace(/(^|[;{\s])font-family\s*:\s*([^;}]+)/gi, (whole, lead: string, value: string) =>
      MONOSPACE.test(value) ? whole : `${lead}font-family:inherit`,
    )
    .replace(/(^|[;{\s])font-size\s*:\s*([^;}]+)/gi, (whole, lead: string, value: string) =>
      isBodySize(value) ? `${lead}font-size:inherit` : whole,
    )
}

/**
 * The message's own markup with its colours moved onto `page`: white behind
 * text gone, text readable on whatever is really behind it, and the fonts of a
 * sender's client left at the door.
 *
 * Inline, element by element, because the answer depends on what is behind
 * each one: a line on the page reads against the page, and a line in a cell the
 * sender kept coloured -- a highlight, a bar across a signature -- against that
 * cell.
 */
function adapt(html: string, tokens: Token[], page: Page): string {
  const behind = new Map<number, Rgb>()
  let out = ''
  let pos = 0
  tokens.forEach((t, i) => {
    if (t.kind === 'raw') {
      out += html.slice(pos, t.at) + adaptSheet(html.slice(t.at, t.end), page)
      pos = t.end
      return
    }
    if (t.kind !== 'open') return
    const source = html.slice(t.at, t.end)
    const inherited = behind.get(t.parent) ?? page.background
    const style = declarations(attribute(source, 'style') ?? '')
    const kept: [string, string][] = []
    let fill: Rgb | null = null
    let ink: Rgb | null = null
    let changed = false

    for (const [property, value] of style) {
      if (property === 'background-color' || property === 'background') {
        const colour = /url\(/i.test(value) ? null : backgroundColour(value)
        if (colour && isPaperWhite(colour)) {
          changed = true
          continue
        }
        if (colour) fill = colour
      }
      if (property === 'color') {
        const colour = parseColour(value)
        if (colour) {
          ink = colour
          changed = true
          continue
        }
      }
      if (property === 'font-family' && !MONOSPACE.test(value)) {
        changed = true
        continue
      }
      if (property === 'font-size' && isBodySize(value)) {
        changed = true
        continue
      }
      kept.push([property, value])
    }

    let tag = source
    const bgcolor = attribute(source, 'bgcolor')
    if (bgcolor !== null) {
      const colour = backgroundColour(bgcolor)
      if (colour && isPaperWhite(colour)) tag = withAttribute(tag, 'bgcolor', null)
      else if (colour) fill ??= colour
    }
    if (t.tag === 'font') {
      const colourAttr = attribute(source, 'color')
      const parsed = colourAttr === null ? null : parseColour(colourAttr)
      if (parsed) {
        ink = parsed
        tag = withAttribute(tag, 'color', null)
      }
      if (!MONOSPACE.test(attribute(source, 'face') ?? 'mono')) {
        tag = withAttribute(tag, 'face', null)
      }
      if (/^[23]$/.test(attribute(source, 'size') ?? '')) tag = withAttribute(tag, 'size', null)
    }

    const surface = fill ?? inherited
    behind.set(i, surface)
    const onPage = surface === page.background
    if (ink) {
      const moved = readable(ink, surface, onPage)
      if (moved === null) kept.push(['color', hex(ink)])
      else if (moved) kept.push(['color', moved])
    } else if (fill && contrast(page.text, fill) < READABLE) {
      // A colour kept behind text that was left to inherit the page's: the
      // sender wrote that for a page of a different colour.
      kept.push(['color', luminance(fill) < 0.18 ? '#f4f2f7' : '#1c1a17'])
      changed = true
    }

    if (changed) {
      const written = kept.map(([p, v]) => `${p}:${v}`).join(';')
      tag = withAttribute(tag, 'style', written || null)
    }
    if (tag !== source) {
      out += html.slice(pos, t.at) + tag
      pos = t.end
    }
  })
  return out + html.slice(pos)
}

// ── What `mailview.ts` calls ──────────────────────────────────────────

export interface ReadBody {
  /** The markup to show. */
  html: string
  /** There is history in this message that `html` leaves out, or would leave
   *  out if it were asked to. */
  hasHistory: boolean
  /** Designed for a page of its own -- see {@link isDesigned}. */
  designed: boolean
}

export interface ReadOptions {
  /** The colours of the page this will be drawn on when it is not designed. */
  page: { background: string; text: string }
  /** Keep the history in `html`. */
  showHistory?: boolean
  /** The message's subject, which is how a forward is told from a reply. */
  subject?: string
}

/** Read `inner` -- the markup inside a body document's `<body>`. */
export function readBody(inner: string, options: ReadOptions): ReadBody {
  const tokens = tokenize(inner)
  const start = historyStart(inner, tokens, options.subject ?? '')
  const cut = start === null ? null : trimBack(inner, tokens, start)
  // Judged on the sender's own words: a one-line reply is not a newsletter for
  // having one in the history under it.
  const designed = isDesigned(inner, tokens, cut ?? tokens.length)
  const hasHistory = cut !== null
  const folded = cut !== null && !options.showHistory
  if (designed) {
    return { html: folded ? cutAt(inner, tokens, cut) : inner, hasHistory, designed }
  }
  const page: Page = {
    background: parseColour(options.page.background) ?? [255, 255, 255],
    text: parseColour(options.page.text) ?? [0, 0, 0],
  }
  // Moved before it is cut, since a tag rewritten here changes every offset.
  const shown = folded ? cutAt(inner, tokens, cut) : inner
  return { html: adapt(shown, tokenize(shown), page), hasHistory, designed }
}

/** The body's line height: `BASE_STYLE`'s 15px at 1.6. */
const LINE_PX = 24
/** The margin a browser gives a paragraph at that size. */
const PARAGRAPH_GAP_PX = 15
/** A picture sits on the line's baseline, with the room for a descender under it. */
const BASELINE_PX = 8
/** A picture that does not say how tall it is. */
const UNSTATED_PICTURE_PX = 100

/** The height a picture states, in its style or its attribute, at the width it
 *  will be drawn: `max-width:100%` scales one wider than the column down. */
function pictureHeight(source: string, column: number): number {
  const style = declarations(attribute(source, 'style') ?? '')
  const stated = (property: string) =>
    widthInPixels(style.find(([p]) => p === property)?.[1]) ||
    widthInPixels(attribute(source, property))
  const height = stated('height')
  if (!height) return UNSTATED_PICTURE_PX
  return Math.round(height * Math.min(1, column / (stated('width') || column))) + BASELINE_PX
}

/**
 * How tall a written message comes out at `widthPx` -- one drawn on the card,
 * with no padding of its own (`mailview.ts`'s `renderBody`). A guess, since the
 * sandbox that keeps a message in its frame also stops anything asking the frame
 * how tall it turned out, and it decides how much blank there is under a
 * one-line reply, so it walks the lines the message really has: a blank line
 * counts, and a paragraph's gap counts only where a sender did not take it away,
 * as Word's `p.MsoNormal` always does.
 *
 * It errs tall, by a line or so on a long message, since a little blank costs
 * less than a scrollbar inside the thread's own.
 */
export function estimateWrittenHeight(doc: string, widthPx: number): number {
  const inner = doc.replace(/^[\s\S]*<body[^>]*>/i, '').replace(/<\/body>[\s\S]*$/i, '')
  const tokens = tokenize(inner)
  const charsPerLine = Math.max(20, Math.floor(widthPx / 8.2))
  let lines = 0
  let extra = 0
  let chars = 0
  const endLine = () => {
    if (chars > 0) lines += Math.ceil(chars / charsPerLine)
    chars = 0
  }
  for (const t of tokens) {
    if (t.kind === 'text') {
      const text = decodeEntities(inner.slice(t.at, t.end))
      if (tokenIsPreformatted(tokens, t)) {
        const rows = text.split('\n')
        rows.slice(0, -1).forEach((row) => {
          lines += Math.max(1, Math.ceil(row.length / charsPerLine))
        })
        chars += rows[rows.length - 1]?.length ?? 0
      } else {
        // A `&nbsp;` is how a blank line is written, where whitespace between
        // two tags is nothing at all.
        const blank = /\u00a0|&nbsp;|&#160;/i.test(inner.slice(t.at, t.end))
        chars += blank ? Math.max(1, text.trim().length) : text.replace(/\s+/g, ' ').trim().length
      }
    } else if (t.kind === 'void' && t.tag === 'br') {
      // A break with nothing on its line is a blank line.
      if (chars === 0) lines++
      endLine()
    } else if (t.kind === 'void' && t.tag === 'img') {
      extra += pictureHeight(inner.slice(t.at, t.end), widthPx)
    } else if (t.kind === 'void' && t.tag === 'hr') {
      endLine()
      extra += PARAGRAPH_GAP_PX
    } else if ((t.kind === 'open' || t.kind === 'close') && BLOCK.has(t.tag)) {
      endLine()
      if (t.kind === 'open') {
        const source = inner.slice(t.at, t.end)
        if (t.tag === 'tr') extra += 4
        else if (/^h[1-6]$/.test(t.tag)) extra += LINE_PX + PARAGRAPH_GAP_PX
        else if (t.tag === 'ul' || t.tag === 'ol' || t.tag === 'blockquote' || t.tag === 'pre') {
          extra += PARAGRAPH_GAP_PX
        } else if (t.tag === 'p' && !classesOf(source).includes('msonormal')) {
          const margin = declarations(attribute(source, 'style') ?? '').find(
            ([p]) => p === 'margin' || p === 'margin-bottom',
          )?.[1]
          if (!margin || !/^0(?:[a-z]+)?\b/i.test(margin)) extra += PARAGRAPH_GAP_PX
        }
      }
    }
  }
  endLine()
  // Fractions of a pixel, and a sender's own spacing nothing here reads.
  const slack = 6
  return Math.min(60000, Math.max(LINE_PX, lines * LINE_PX) + extra + slack)
}

/**
 * The same for a designed message, on a page with padding of its own. It has
 * bigger type and roomier rows than this can read from the markup, and that adds
 * up over its length, so the allowance grows with it: running out of frame near
 * the end of a long newsletter costs a scrollbar inside the thread's own, where
 * a short one should not end in a page of white.
 */
export function estimateDesignedHeight(doc: string, widthPx: number): number {
  const rows = (doc.match(/<tr[\s>]/gi) ?? []).length
  const lines = estimateWrittenHeight(doc, Math.max(200, widthPx - 32))
  const allowance = Math.max(0, lines - 600) * 0.25
  return Math.min(60000, Math.round(lines + allowance) + 32 + rows * 16 + 16)
}

function tokenIsPreformatted(tokens: Token[], token: Token): boolean {
  for (let p = token.parent; p >= 0; p = tokens[p]?.parent ?? -1) {
    if (tokens[p]?.tag === 'pre') return true
  }
  return false
}

/** Whether markup would be drawn on a page of its own -- {@link isDesigned},
 *  for a caller with no history to think about. */
export function looksDesigned(inner: string): boolean {
  const tokens = tokenize(inner)
  return isDesigned(inner, tokens, tokens.length)
}

/** {@link adapt} on its own, for a caller with no history to think about. */
export function adaptToPage(inner: string, page: ReadOptions['page']): string {
  return adapt(inner, tokenize(inner), {
    background: parseColour(page.background) ?? [255, 255, 255],
    text: parseColour(page.text) ?? [0, 0, 0],
  })
}
