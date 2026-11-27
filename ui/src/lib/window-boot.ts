/**
 * The page-level start-up the secondary windows (`panel/`, `writing/`,
 * `voicetyping/`) share: show the theme the main window chose, find the
 * window's root element, mount its component.
 *
 * The side-effect imports -- `lib/legacy` first, then the three stylesheets --
 * stay in each window's `main.ts`: they must run before anything here reads
 * the theme, and `scripts/legacy.test.mjs` looks for `lib/legacy` there.
 */
import { mount, type Component } from 'svelte'
import { parseThemeChoice, resolveTheme, showTheme, watchSystemScheme } from './themes'

interface BootOptions {
  /**
   * The theme to show instead of the one the main window stored. A review
   * build in a browser takes `?theme=` this way.
   */
  forced?: string | null
  /**
   * Keep following the main window's choice and the desktop's light or dark,
   * for a window that outlives what it draws. `storage` fires in every other
   * window of this origin, never the one that wrote.
   */
  follow?: boolean
}

/** Show the theme, then mount `component` on `#rootId` (throws if the page has none). */
export function bootWindow(
  rootId: string,
  component: Component,
  { forced = null, follow = false }: BootOptions = {},
) {
  const theme = () => resolveTheme(parseThemeChoice(forced ?? localStorage.getItem('jatayu.theme')))

  showTheme(theme())

  if (follow) {
    window.addEventListener('storage', (e) => {
      if (e.key === 'jatayu.theme') showTheme(theme())
    })
    watchSystemScheme(() => showTheme(theme()))
  }

  const target = document.getElementById(rootId)
  if (!target) throw new Error(`#${rootId} is missing from ${rootId}.html`)

  return mount(component, { target })
}
