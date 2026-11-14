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
