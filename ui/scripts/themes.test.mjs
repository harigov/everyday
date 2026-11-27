// The theme contract: every theme in `THEMES` (lib/themes.ts) has a block in
// `styles/themes.css`, and every block sets exactly `THEME_TOKENS` -- not one
// missing, which would leave that colour to whatever the window's previous
// theme had set, and not one extra, which is a token a component may start
// relying on that the other themes never define. Also the small amount of
// logic in `themes.ts`: reading a stored choice and resolving `system`.

import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { runInNewContext } from 'node:vm'
import { load, makeCheck, stubBrowser } from './harness.mjs'

let dark = false
stubBrowser({ matchMedia: () => ({ matches: dark, addEventListener: () => {} }) })

const {
  modules: [themes, apps],
  close,
} = await load(['/src/lib/themes.ts', '/src/lib/apps.ts'])
const { THEMES, THEME_TOKENS, DERIVED_TOKENS, APP_COLOURS, parseThemeChoice, resolveTheme } = themes
const { check, ok, finish } = makeCheck()

// The alpha reader itself, on the forms a theme might write.
for (const [value, want] of [
  ['#fffefc', 1],
  ['#fff', 1],
  ['#ffffffcc', 0.8],
  ['#fffc', 0.8],
  ['rgb(255 255 255 / 0.98)', 0.98],
  ['rgb(255 255 255 / 80%)', 0.8],
  ['rgba(255,255,255,.8)', 0.8],
  ['rgb(1, 2, 3)', 1],
  ['color-mix(in oklab, white 80%, transparent)', null],
]) {
  const got = alphaOf(value)
  ok(`alphaOf(${value}) is ${want}`, want === null ? got === null : Math.abs(got - want) < 0.001)
}

// The shadow reach reader, likewise.
for (const [value, want] of [
  ['0 0 #0000', 0],
  ['0 24px 60px -24px rgb(0 0 0 / 0.6)', 60],
  ['0 20px 40px -48px rgb(0 0 0 / 0.75)', 8],
  ['0 0 0 1px rgb(20 55 110 / 0.06), 0 1px 2px rgb(20 55 110 / 0.06)', 3],
  ['inset 0 1px 0 rgb(255 255 255 / 0.2), -6px 2px 4px #000', 12],
  ['3px 0 rgb(0 0 0)', 3],
  ['0 1rem 3rem -1rem #000', 52],
  ['0 2px 4px color-mix(in oklab, rgb(0 0 0) 40%, transparent), 0 0 0 1px #000', 6],
  ['0 1vw 2vw #000', NaN],
  ['0 var(--y) 4px #000', NaN],
]) {
  check(`sidewaysReach(${value})`, String(sidewaysReach(value)), String(want))
}

const styles = join(dirname(fileURLToPath(import.meta.url)), '..', 'src', 'styles')
const strip = (css) => css.replace(/\/\*[\s\S]*?\*\//g, '')
const themesCss = strip(readFileSync(join(styles, 'themes.css'), 'utf8'))
const themeCss = strip(readFileSync(join(styles, 'theme.css'), 'utf8'))

/** Each `selector { body }` at the top level, with its declarations by name. */
function blocks(css) {
  const out = []
  const re = /([^{}]+)\{([^{}]*)\}/g
  for (let m; (m = re.exec(css));) {
    const decls = new Map()
    for (const part of m[2].split(';')) {
      const at = part.indexOf(':')
      if (at < 0) continue
      decls.set(part.slice(0, at).trim(), part.slice(at + 1).trim())
    }
    out.push({ selector: m[1].trim(), decls })
  }
  return out
}

/** WCAG contrast between two opaque hex colours; 0 for anything else, which
 *  fails the 4.5 the callers ask for -- a token this cannot read is a token
 *  nobody has checked. */
function contrast(a, b) {
  const lum = (hex) => {
    const m = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(hex.trim())
    if (!m) return null
    const h = m[1].length === 3 ? [...m[1]].map((c) => c + c).join('') : m[1]
    const [r, g, bl] = [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16) / 255)
    const lin = (c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4)
    return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(bl)
  }
  const la = lum(a)
  const lb = lum(b)
  if (la === null || lb === null) return 0
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05)
}

/**
 * A colour's alpha, 0 to 1, for the forms a theme writes: hex of 3, 4, 6 or
 * 8 digits, and `rgb()`/`rgba()` with commas or a slash and a number or a
 * percentage. Anything else -- a `color-mix()`, a `var()` -- is `null`, which
 * the check that uses this fails rather than lets through.
 */
function alphaOf(value) {
  const v = value.trim().toLowerCase()
  const hex = /^#([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/.exec(v)
  if (hex) {
    const h = hex[1]
    if (h.length === 3 || h.length === 6) return 1
    const a = h.length === 4 ? h[3] + h[3] : h.slice(6)
    return parseInt(a, 16) / 255
  }
  const fn = /^rgba?\((.*)\)$/.exec(v)
  if (!fn) return null
  const parts = fn[1].split(/\s*[,/]\s*|\s+/).filter(Boolean)
  if (parts.length === 3) return 1
  if (parts.length !== 4) return null
  const a = parts[3]
  const n = Number(a.endsWith('%') ? a.slice(0, -1) : a)
  if (!Number.isFinite(n)) return null
  return a.endsWith('%') ? n / 100 : n
}

/** `value` split at each `sep` (a comma, or any whitespace) outside parentheses. */
function splitTop(value, sep) {
  const out = []
  let depth = 0
  let part = ''
  for (const ch of value) {
    if (ch === '(') depth++
    else if (ch === ')') depth--
    if (depth === 0 && (sep === ',' ? ch === ',' : /\s/.test(ch))) {
      if (part.trim()) out.push(part.trim())
      part = ''
    } else part += ch
  }
  if (part.trim()) out.push(part.trim())
  return out
}

/**
 * How far past its box's left or right edge a `box-shadow` list paints, in
 * pixels, as WebKit counts it for deciding what overlaps what: the offset,
 * plus the spread, plus 1.4 times the blur (`ShadowData::paintingExtent` --
 * a Gaussian at half the radius is invisible in 8 bits by then). Inset
 * shadows paint inside the box and reach nowhere. Lengths in px, rem or em
 * (at 16px) or a bare 0; anything this cannot read -- another unit, a
 * `var()` -- is `NaN`, which the check that uses it fails rather than lets
 * through.
 */
function sidewaysReach(value) {
  let reach = 0
  for (const shadow of splitTop(value, ',')) {
    const tokens = splitTop(shadow, ' ')
    if (tokens.includes('inset')) continue
    const lengths = []
    for (const token of tokens) {
      const length = /^(-?\d*\.?\d+)(px|rem|em)?$/.exec(token)
      if (length) lengths.push(parseFloat(length[1]) * (length[2] && length[2] !== 'px' ? 16 : 1))
      else if (/^var\(/.test(token) || /^-?\d/.test(token)) return NaN
    }
    const [x = 0, , blur = 0, spread = 0] = lengths
    reach = Math.max(reach, Math.abs(x) + spread + Math.ceil(blur * 1.4))
  }
  return reach
}

const sorted = (xs) => [...xs].sort()
const tokensOf = (decls) => sorted([...decls.keys()].filter((k) => k.startsWith('--')))

// themes.css holds theme blocks and nothing else.
const found = new Map()
for (const { selector, decls } of blocks(themesCss)) {
  const m = /^\[data-theme='([a-z-]+)'\]$/.exec(selector)
  ok(`themes.css: "${selector}" is a theme block`, m)
  if (!m) continue
  ok(`themes.css: ${m[1]} is defined once`, !found.has(m[1]))
  found.set(m[1], decls)
}

check(
  'every theme in THEMES has a block, and no block is unlisted',
  sorted(found.keys()),
  sorted(THEMES.map((t) => t.id)),
)

for (const theme of THEMES) {
  const decls = found.get(theme.id)
  if (!decls) continue
  check(`${theme.id}: sets exactly the contract's tokens`, tokensOf(decls), sorted(THEME_TOKENS))
  check(`${theme.id}: color-scheme matches THEMES`, decls.get('color-scheme'), theme.scheme)
  const other = [...decls.keys()].filter((k) => !k.startsWith('--') && k !== 'color-scheme')
  check(`${theme.id}: no properties besides tokens and color-scheme`, other, [])
  // A glass theme frosts and spaces its panels; a flat one does neither.
  const flat = decls.get('--glass-blur') === 'none' && decls.get('--shell-gap') === '0'
  check(`${theme.id}: glass flag matches its frame tokens`, !flat, theme.glass)
  // A framed document is handed these, since it cannot read the stylesheet.
  check(`${theme.id}: frame.card is --card`, theme.frame.card, decls.get('--card'))
  check(`${theme.id}: frame.text is --fg`, theme.frame.text, decls.get('--fg'))
  check(`${theme.id}: frame.subtle is --fg-subtle`, theme.frame.subtle, decls.get('--fg-subtle'))
  if (!theme.glass)
    check(`${theme.id}: a flat frame.page is --bg`, theme.frame.page, decls.get('--bg'))
  // Menus and popovers stand over text, and a webview may draw no blur
  // under them: at 0.8 the words under a context menu read through its own.
  const float = alphaOf(decls.get('--float') ?? '')
  ok(
    `${theme.id}: --float is a colour this test can read (${decls.get('--float')})`,
    float !== null,
  )
  ok(`${theme.id}: --float is at least 0.97 opaque`, float !== null && float >= 0.97)
  // A panel's shadow reaching into the panel beside it makes WebKit give
  // everything under that strip a compositing layer of its own, remade on
  // every frame of a scroll (see the foot of the glass notes in themes.css).
  // A shadow it cannot read is NaN, which fails both comparisons.
  const gap = parseFloat(decls.get('--shell-gap') ?? '0') || 0
  const reach = sidewaysReach(decls.get('--shell-shadow') ?? '')
  ok(
    `${theme.id}: --shell-shadow reaches ${reach}px sideways, inside the ${gap}px gap`,
    reach === 0 || reach < gap,
  )
  // Words on a filled button: a primary button's label on --accent, an
  // overdue badge's on --danger. White on a dark theme's pale orange was
  // 2.2:1; WCAG asks 4.5:1 of text this size.
  for (const [fg, bg] of [
    ['--fg-on-accent', '--accent'],
    ['--fg-on-accent', '--accent-hover'],
    ['--fg-on-danger', '--danger'],
  ]) {
    const ratio = contrast(decls.get(fg) ?? '', decls.get(bg) ?? '')
    ok(`${theme.id}: ${fg} on ${bg} is ${ratio.toFixed(2)}:1, at least 4.5:1`, ratio >= 4.5)
  }
}

// theme.css: layout on :root, the derived tokens on [data-theme], and no
// colour token anywhere else for a theme to be shadowed by.
for (const { selector, decls } of blocks(themeCss)) {
  if (selector === '[data-theme]') {
    check(
      'theme.css: [data-theme] sets the derived tokens',
      tokensOf(decls),
      sorted(DERIVED_TOKENS),
    )
    continue
  }
  check('theme.css: only :root and [data-theme] rules', selector, ':root')
  const leaked = tokensOf(decls).filter(
    (t) => THEME_TOKENS.includes(t) || DERIVED_TOKENS.includes(t),
  )
  check('theme.css: :root sets no theme token', leaked, [])
}

// Every app has a colour, and nothing but an app does: an app added to
// `apps.ts` without one would draw its icons in an invalid colour.
check('APP_COLOURS is every app', sorted(APP_COLOURS), sorted(apps.APP_ORDER))

// Ids kept from when there were two themes, so a saved choice still means the same.
check('Paper keeps the id `light`', THEMES.find((t) => t.name === 'Paper')?.id, 'light')
check('Ink keeps the id `dark`', THEMES.find((t) => t.name === 'Ink')?.id, 'dark')

check('a stored theme is read back', parseThemeChoice('aurora'), 'aurora')
check('nothing stored is the desktop', parseThemeChoice(null), 'system')
check('an unknown theme is the desktop', parseThemeChoice('sepia'), 'system')
check('system is kept as chosen', parseThemeChoice('system'), 'system')

dark = false
check('system on a light desktop is Paper', resolveTheme('system'), 'light')
dark = true
check('system on a dark desktop is Ink', resolveTheme('system'), 'dark')
check('a chosen theme ignores the desktop', resolveTheme('lagoon'), 'lagoon')

// `public/boot.js`, which sets the theme before the application has loaded,
// must land on the theme the application then applies: otherwise the first
// frame is one theme and the next another, which is the flash it is there to
// prevent. Run as the page runs it, against every stored value.
const bootJs = readFileSync(join(styles, '..', '..', 'public', 'boot.js'), 'utf8')
function bootTheme(stored, desktopDark) {
  let set = null
  runInNewContext(bootJs, {
    localStorage: { getItem: (key) => (key === 'jatayu.theme' ? stored : null) },
    matchMedia: () => ({ matches: desktopDark }),
    document: { documentElement: { setAttribute: (name, value) => (set = { [name]: value }) } },
  })
  return set?.['data-theme']
}
for (const stored of [...THEMES.map((t) => t.id), 'system', null, 'sepia']) {
  for (const desktop of [false, true]) {
    dark = desktop
    check(
      `boot.js shows what the app does for ${stored} on a ${desktop ? 'dark' : 'light'} desktop`,
      bootTheme(stored, desktop),
      resolveTheme(parseThemeChoice(stored)),
    )
  }
}

await close()
finish('themes')
