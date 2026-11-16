// The theme contract: every theme in `THEMES` (lib/themes.ts) has a block in
// `styles/themes.css`, and every block sets exactly `THEME_TOKENS` -- not one
// missing, which would leave that colour to whatever the window's previous
// theme had set, and not one extra, which is a token a component may start
// relying on that the other themes never define. Also the small amount of
// logic in `themes.ts`: reading a stored choice and resolving `system`.

import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
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

await close()
finish('themes')
