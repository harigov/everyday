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

const STYLE_END = /<\/style/gi

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
      STYLE_END.lastIndex = end
      const stop = STYLE_END.exec(html)?.index ?? html.length
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

// Maps, here and for colours, since these are looked up with a stranger's word:
// a plain object answers to `constructor` and `__proto__` as well.
const NAMED_ENTITIES = new Map([
  ['amp', '&'],
  ['lt', '<'],
  ['gt', '>'],
  ['quot', '"'],
  ['apos', "'"],
  ['nbsp', ' '],
])

function decodeEntities(s: string): string {
  if (!s.includes('&')) return s
  return s.replace(
    /&(?:#(\d+)|#x([\da-f]+)|([a-z]+));/gi,
    (whole: string, dec: string, hex: string, name: string) => {
      if (name) return NAMED_ENTITIES.get(name.toLowerCase()) ?? whole
      const code = dec ? parseInt(dec, 10) : parseInt(hex, 16)
      if (code === 0xa0) return ' '
      return code > 0 && code <= 0x10ffff ? String.fromCodePoint(code) : whole
    },
  )
}

/** Anything a sender wrote: a word, an emoji, a lone "?". Not whitespace, a rule
 *  typed out of dashes or box characters, or an invisible formatting character. */
const SUBSTANCE = /(?!\u200d|\ufe0f)[^\s\-_=*~\u2500-\u257f\u200b\u200c\ufeff]/u

/** Something written, or a picture -- not blank lines, rules or a lone `&nbsp;`. */
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

/** What says a reply header is a header, where a sentence can start with "On" and
 *  end in "wrote:" as well: a year, a time, a date or an address. */
const DATED = /\b\d{4}\b|\b\d{1,2}:\d{2}\b|\b\d{1,2}[/.]\d{1,2}\b|@/

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
 * under everything it was handed. It sits below the history, and without this
 * every word of that history would count as being above the sender's last remark
 * and stay in view.
 *
 * Whole phrases, and only as the very first thing after the quote (see
 * {@link isStampedFooter}), because "the virus scan passed" or "those are
 * confidential until Friday" under a quote is somebody's answer.
 */
const STAMPED_FOOTER =
  /\bthis (?:e-?mail|message|communication|transmission)\b[^.]{0,160}\b(?:confidential|privileged|intended)\b|\bconfidentiality notice\b|\bdisclaimer\s*:|\bintended recipient\b|\byou received this (?:message|e-?mail)\b|\bto unsubscribe\b|\bunsubscribe from\b|\bview this discussion\b|\bscanned (?:for|by)\b[^.]{0,60}\bvirus/i

function isStampedFooter(text: string): boolean {
  const found = STAMPED_FOOTER.exec(text)
  return found !== null && !SUBSTANCE.test(text.slice(0, found.index))
}

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
  /** Nothing else is written between line `n` and the quote after `at`. */
  const quoteIsNext = (at: number, n: number): boolean => {
    const quote = tokens.findIndex((t, i) => i > at && t.kind === 'open' && t.tag === 'blockquote')
    return quote >= 0 && (lines[n + 1]?.token ?? tokens.length) >= quote
  }
  const lines = linesOf(html, tokens)
  tokens.forEach((t, i) => {
    if (t.kind !== 'open') return
    const source = html.slice(t.at, t.end)
    if (historyFollows(source)) candidates.set(i, tokens.length)
    else if (classesOf(source).includes('moz-cite-prefix')) candidates.set(i, throughNextQuote(i))
    else if (isQuoteElement(source, t.tag)) {
      candidates.set(i, t.pair < 0 ? tokens.length : t.pair + 1)
    }
  })
  lines.forEach((line, n) => {
    if (FORWARDED.test(line.text)) {
      // A forward with its subject edited: whatever follows was sent.
      candidates.set(line.token, -1)
    } else if (isWroteLine(line.text)) {
      const at = climb(html, tokens, line.token)
      const opener = tokens[at]
      if (opener?.tag === 'blockquote') {
        // The header is inside the quote it belongs to, as Apple Mail writes it.
        candidates.set(at, opener.pair < 0 ? tokens.length : opener.pair + 1)
      } else if (DATED.test(line.text) || quoteIsNext(at, n)) {
        candidates.set(at, throughNextQuote(at))
      }
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
    // An empty marker, which Outlook leaves on mail it did not quote anything in:
    // nothing to fold, and a control that opens nothing.
    if (!hasSubstance(html, tokens, at, end)) continue
    if (!hasSubstance(html, tokens, 0, at)) return null
    if (!hasSubstance(html, tokens, end, tokens.length)) return at
    const after = tokens.slice(end).filter((t) => t.kind === 'text')
    const trailing = after.map((t) => decodeEntities(html.slice(t.at, t.end))).join(' ')
    if (isStampedFooter(trailing)) return at
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
    } else if (t.kind === 'text' && !SUBSTANCE.test(decodeEntities(html.slice(t.at, t.end)))) {
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

/** Every colour name CSS has, then Outlook's own two: the system's text and page
 *  colours, as a Windows desktop answered when the message was written, and always
 *  black on white there. */
const NAMED_COLOURS = new Map<string, Rgb>(
  (
    'aliceblue:f0f8ff antiquewhite:faebd7 aqua:00ffff aquamarine:7fffd4 azure:f0ffff beige:f5f5dc ' +
    'bisque:ffe4c4 black:000000 blanchedalmond:ffebcd blue:0000ff blueviolet:8a2be2 brown:a52a2a ' +
    'burlywood:deb887 cadetblue:5f9ea0 chartreuse:7fff00 chocolate:d2691e coral:ff7f50 ' +
    'cornflowerblue:6495ed cornsilk:fff8dc crimson:dc143c cyan:00ffff darkblue:00008b ' +
    'darkcyan:008b8b darkgoldenrod:b8860b darkgray:a9a9a9 darkgreen:006400 darkgrey:a9a9a9 ' +
    'darkkhaki:bdb76b darkmagenta:8b008b darkolivegreen:556b2f darkorange:ff8c00 ' +
    'darkorchid:9932cc darkred:8b0000 darksalmon:e9967a darkseagreen:8fbc8f darkslateblue:483d8b ' +
    'darkslategray:2f4f4f darkslategrey:2f4f4f darkturquoise:00ced1 darkviolet:9400d3 ' +
    'deeppink:ff1493 deepskyblue:00bfff dimgray:696969 dimgrey:696969 dodgerblue:1e90ff ' +
    'firebrick:b22222 floralwhite:fffaf0 forestgreen:228b22 fuchsia:ff00ff gainsboro:dcdcdc ' +
    'ghostwhite:f8f8ff gold:ffd700 goldenrod:daa520 gray:808080 green:008000 greenyellow:adff2f ' +
    'grey:808080 honeydew:f0fff0 hotpink:ff69b4 indianred:cd5c5c indigo:4b0082 ivory:fffff0 ' +
    'khaki:f0e68c lavender:e6e6fa lavenderblush:fff0f5 lawngreen:7cfc00 lemonchiffon:fffacd ' +
    'lightblue:add8e6 lightcoral:f08080 lightcyan:e0ffff lightgoldenrodyellow:fafad2 ' +
    'lightgray:d3d3d3 lightgreen:90ee90 lightgrey:d3d3d3 lightpink:ffb6c1 lightsalmon:ffa07a ' +
    'lightseagreen:20b2aa lightskyblue:87cefa lightslategray:778899 lightslategrey:778899 ' +
    'lightsteelblue:b0c4de lightyellow:ffffe0 lime:00ff00 limegreen:32cd32 linen:faf0e6 ' +
    'magenta:ff00ff maroon:800000 mediumaquamarine:66cdaa mediumblue:0000cd mediumorchid:ba55d3 ' +
    'mediumpurple:9370db mediumseagreen:3cb371 mediumslateblue:7b68ee mediumspringgreen:00fa9a ' +
    'mediumturquoise:48d1cc mediumvioletred:c71585 midnightblue:191970 mintcream:f5fffa ' +
    'mistyrose:ffe4e1 moccasin:ffe4b5 navajowhite:ffdead navy:000080 oldlace:fdf5e6 olive:808000 ' +
    'olivedrab:6b8e23 orange:ffa500 orangered:ff4500 orchid:da70d6 palegoldenrod:eee8aa ' +
    'palegreen:98fb98 paleturquoise:afeeee palevioletred:db7093 papayawhip:ffefd5 peachpuff:ffdab9 ' +
    'peru:cd853f pink:ffc0cb plum:dda0dd powderblue:b0e0e6 purple:800080 rebeccapurple:663399 ' +
    'red:ff0000 rosybrown:bc8f8f royalblue:4169e1 saddlebrown:8b4513 salmon:fa8072 ' +
    'sandybrown:f4a460 seagreen:2e8b57 seashell:fff5ee sienna:a0522d silver:c0c0c0 skyblue:87ceeb ' +
    'slateblue:6a5acd slategray:708090 slategrey:708090 snow:fffafa springgreen:00ff7f ' +
    'steelblue:4682b4 tan:d2b48c teal:008080 thistle:d8bfd8 tomato:ff6347 turquoise:40e0d0 ' +
    'violet:ee82ee wheat:f5deb3 white:ffffff whitesmoke:f5f5f5 yellow:ffff00 yellowgreen:9acd32 ' +
    'windowtext:000000 window:ffffff'
  )
    .split(' ')
    .map((pair): [string, Rgb] => {
      const [name = '', value = ''] = pair.split(':')
      return [
        name,
        [
          parseInt(value.slice(0, 2), 16),
          parseInt(value.slice(2, 4), 16),
          parseInt(value.slice(4, 6), 16),
        ],
      ]
    }),
)

const NOT_A_COLOUR = /^(?:inherit|initial|unset|revert|transparent|currentcolor|none|auto)$/

/** A colour worth reasoning about; `'clear'` for `inherit`, `transparent` or an
 *  almost clear `rgba`, which paint nothing; `'unknown'` for what this cannot read. */
function readColour(value: string): Rgb | 'clear' | 'unknown' {
  const v = value
    .replace(/!important/i, '')
    .trim()
    .toLowerCase()
  const named = NAMED_COLOURS.get(v)
  if (named) return named
  if (NOT_A_COLOUR.test(v)) return 'clear'
  let m = /^#([\da-f]{3,8})$/.exec(v)
  if (m) {
    const hex = m[1] ?? ''
    if (hex.length === 3 || hex.length === 4) {
      const [r, g, b] = [...hex].map((c) => parseInt(c + c, 16))
      if (hex.length === 4 && parseInt(hex.charAt(3) + hex.charAt(3), 16) < 26) return 'clear'
      return [r ?? 0, g ?? 0, b ?? 0]
    }
    if (hex.length === 6 || hex.length === 8) {
      if (hex.length === 8 && parseInt(hex.slice(6), 16) < 26) return 'clear'
      return [
        parseInt(hex.slice(0, 2), 16),
        parseInt(hex.slice(2, 4), 16),
        parseInt(hex.slice(4, 6), 16),
      ]
    }
    return 'unknown'
  }
  const almostClear = (alpha: string | undefined) =>
    alpha !== undefined && (alpha.endsWith('%') ? parseFloat(alpha) / 100 : parseFloat(alpha)) < 0.1
  m = /^rgba?\(\s*([\d.]+%?)[\s,]+([\d.]+%?)[\s,]+([\d.]+%?)(?:[\s,/]+([\d.]+%?))?\s*\)$/.exec(v)
  if (m) {
    const part = (p: string | undefined) =>
      p?.endsWith('%') ? (parseFloat(p) * 255) / 100 : parseFloat(p ?? '0')
    if (almostClear(m[4])) return 'clear'
    return [part(m[1]), part(m[2]), part(m[3])]
  }
  m =
    /^hsla?\(\s*(-?[\d.]+)(?:deg)?[\s,]+([\d.]+)%[\s,]+([\d.]+)%(?:[\s,/]+([\d.]+%?))?\s*\)$/.exec(
      v,
    )
  if (m) {
    if (almostClear(m[4])) return 'clear'
    const hue = ((parseFloat(m[1] ?? '0') % 360) + 360) % 360
    return fromHsl(hue / 360, parseFloat(m[2] ?? '0') / 100, parseFloat(m[3] ?? '0') / 100)
  }
  return 'unknown'
}

function parseColour(value: string): Rgb | null {
  const colour = readColour(value)
  return typeof colour === 'string' ? null : colour
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
  for (const [, value] of css.matchAll(/(?:^|[;{\s])color\s*:\s*([^;}]+)/gi)) {
    if (value && readColour(value) === 'unknown') return true
  }
  for (const [, value] of css.matchAll(/(?:^|[;{\s])background(?:-color)?\s*:\s*([^;}]+)/gi)) {
    if (!value || /^\s*(?:none|transparent|inherit|initial|unset)\b/i.test(value)) continue
    const colour = backgroundColour(value)
    // Stricter than behind a line of text: nothing but a designer tints a whole
    // rule, even by a shade.
    if (!colour || luminance(colour) < 0.97) return true
  }
  return false
}

/** A colour this cannot judge the contrast of, in a declaration that sets one:
 *  `lab()`, `var()` or a misspelling. It could be anything, on a page it was not
 *  chosen for, and white is the page it was chosen for. */
function isUnreadable(property: string, value: string): boolean {
  if (property === 'color' || property === 'background-color') {
    return readColour(value) === 'unknown'
  }
  return (
    property === 'background' && /\b(?:hwb|lab|lch|oklab|oklch|color|color-mix|var)\(/i.test(value)
  )
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
        if (isUnreadable(property, value)) return true
      }
      if (isUnreadable('color', attribute(source, 'color') ?? 'inherit')) return true
      if (isUnreadable('color', attribute(source, 'bgcolor') ?? 'inherit')) return true
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
        changed = true
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
export function estimateWrittenHeight(doc: string, widthPx: number, imagesHidden = false): number {
  const inner = doc.replace(/^[\s\S]*<body[^>]*>/i, '').replace(/<\/body>[\s\S]*$/i, '')
  const tokens = tokenize(inner)
  const charsPerLine = Math.max(20, Math.floor(widthPx / 8.2))
  const notDrawn = new Map<number, boolean>()
  let lines = 0
  let extra = 0
  let chars = 0
  const endLine = () => {
    if (chars > 0) lines += Math.ceil(chars / charsPerLine)
    chars = 0
  }
  for (const [i, t] of tokens.entries()) {
    const source = t.kind === 'open' || t.kind === 'void' ? inner.slice(t.at, t.end) : ''
    if (notDrawn.get(t.parent)) {
      if (t.kind === 'open') notDrawn.set(i, true)
      continue
    }
    const hides = declarations(attribute(source, 'style') ?? '').some(
      ([p, v]) => p === 'display' && /^none\b/i.test(v),
    )
    if (t.kind === 'open' && (hides || t.tag === 'summary')) {
      notDrawn.set(i, true)
      continue
    }
    if (t.kind === 'text') {
      const text = decodeEntities(inner.slice(t.at, t.end))
      if (tokenIsPreformatted(tokens, t)) {
        const rows = text.split('\n')
        rows.slice(0, -1).forEach((row) => {
          lines += Math.max(1, Math.ceil(lengthOnScreen(row) / charsPerLine))
        })
        chars += lengthOnScreen(rows[rows.length - 1] ?? '')
      } else {
        // A `&nbsp;` is how a blank line is written, where whitespace between
        // two tags is nothing at all.
        const blank = /\u00a0|&nbsp;|&#160;/i.test(inner.slice(t.at, t.end))
        const words = text.replace(/\s+/g, ' ').trim()
        chars += blank ? Math.max(1, lengthOnScreen(words)) : lengthOnScreen(words)
      }
    } else if (t.kind === 'void' && t.tag === 'br') {
      // A break with nothing on its line is a blank line.
      if (chars === 0) lines++
      endLine()
    } else if (t.kind === 'void' && t.tag === 'img') {
      const waiting = imagesHidden && source.includes('/mail/img/')
      if (!hides && !waiting) extra += pictureHeight(source, widthPx)
    } else if (t.kind === 'void' && t.tag === 'hr') {
      endLine()
      extra += PARAGRAPH_GAP_PX
    } else if ((t.kind === 'open' || t.kind === 'close') && BLOCK.has(t.tag)) {
      endLine()
      if (t.kind === 'open') {
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

/** Han, kana and hangul are drawn about twice as wide as the Latin letter the
 *  line length is worked out from. */
const WIDE =
  /[\u1100-\u115f\u2e80-\ua4cf\uac00-\ud7a3\uf900-\ufaff\ufe30-\ufe6f\uff00-\uff60\uffe0-\uffe6]/g

function lengthOnScreen(text: string): number {
  return text.length + (text.match(WIDE)?.length ?? 0)
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
