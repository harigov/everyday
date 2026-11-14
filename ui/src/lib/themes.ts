/**
 * The themes, and the contract every one of them keeps.
 *
 * A theme is a `[data-theme='…']` block in `styles/themes.css` setting every
 * token in `THEME_TOKENS` -- the colours, shadows and frame shape the
 * components are written against (see the contract at the foot of
 * `styles/theme.css` for what each one is for). This file is the other half:
 * which themes there are, what they are called, whether each is light or
 * dark, and in what order Settings lists them. `scripts/themes.test.mjs`
 * reads the stylesheet and fails if the two disagree -- a theme here with no
 * block, a block missing a token, a token no theme should set, or a block
 * whose `color-scheme` is not the scheme named here.
 *
 * Nothing else in the interface names a theme. A component that needs to
 * know whether the window is dark -- to render a mail body to match, say --
 * asks `app.dark`, never `app.theme === 'dark'`: there are several dark
 * themes now.
 */

export type Scheme = 'light' | 'dark'

export interface ThemeInfo {
  /** The `data-theme` value, and what is kept in `everyday.theme`. */
  id: string
  name: string
  scheme: Scheme
  /** A coloured backdrop with floating, translucent panels, rather than flat. */
  glass: boolean
  /** The colours a framed document is drawn in -- a mail body, a feed story,
   *  an attachment shown as a page, the quote under a reply. A frame's
   *  document cannot see this window's custom properties, so it is handed
   *  these instead. `card` and `text` are the theme's `--card` and `--fg`,
   *  and `subtle` its `--fg-subtle` (`themes.test.mjs` holds them to the
   *  stylesheet); `page` is what a frame that fills a pane stands on, which a
   *  flat theme can match exactly and a glass theme, whose panes are
   *  translucent and a frame cannot be, draws as its card. */
  frame: Frame
}

export interface Frame {
  card: string
  page: string
  text: string
  subtle: string
}

/** Light ones first, then dark; within each, the flat original first. `light`
 *  and `dark` keep the ids they had when they were the only two, so a choice
 *  saved before there were more still names the same theme. */
export const THEMES = [
  {
    id: 'light',
    name: 'Paper',
    scheme: 'light',
    glass: false,
    frame: { card: '#f7f6f3', page: '#f7f6f3', text: '#1c1a17', subtle: '#8b857c' },
  },
  {
    id: 'lagoon',
    name: 'Lagoon',
    scheme: 'light',
    glass: true,
    frame: { card: '#f3f7fc', page: '#f3f7fc', text: '#0e1a2b', subtle: '#6a788d' },
  },
  {
    id: 'blossom',
    name: 'Blossom',
    scheme: 'light',
    glass: true,
    frame: { card: '#fcf4f6', page: '#fcf4f6', text: '#2b1520', subtle: '#957381' },
  },
  {
    id: 'meadow',
    name: 'Meadow',
    scheme: 'light',
    glass: true,
    frame: { card: '#f3f8f3', page: '#f3f8f3', text: '#10231a', subtle: '#6c8173' },
  },
  {
    id: 'dune',
    name: 'Dune',
    scheme: 'light',
    glass: true,
    frame: { card: '#fbf5ec', page: '#fbf5ec', text: '#24190f', subtle: '#8e7e6c' },
  },
  {
    id: 'dark',
    name: 'Ink',
    scheme: 'dark',
    glass: false,
    frame: { card: '#2a292f', page: '#17161a', text: '#eceaf0', subtle: '#7d7a86' },
  },
  {
    id: 'aurora',
    name: 'Aurora',
    scheme: 'dark',
    glass: true,
    frame: { card: '#2a1d57', page: '#2a1d57', text: '#f3f1ff', subtle: '#958dc8' },
  },
  {
    id: 'ember',
    name: 'Ember',
    scheme: 'dark',
    glass: true,
    frame: { card: '#3f1d1a', page: '#3f1d1a', text: '#fff3ec', subtle: '#b8958a' },
  },
  {
    id: 'dusk',
    name: 'Dusk',
    scheme: 'dark',
    glass: true,
    frame: { card: '#262a6b', page: '#262a6b', text: '#f5f6ff', subtle: '#969dcf' },
  },
  {
    id: 'tide',
    name: 'Tide',
    scheme: 'dark',
    glass: true,
    frame: { card: '#0f3552', page: '#0f3552', text: '#effaff', subtle: '#88afc0' },
  },
] as const satisfies readonly ThemeInfo[]

export type ThemeId = (typeof THEMES)[number]['id']

/** What the person chose: a theme, or to follow the desktop between Paper and
 *  Ink. */
export type ThemeChoice = ThemeId | 'system'

/** Every custom property a theme block sets. Grouped as the contract in
 *  `theme.css` is. */
export const THEME_TOKENS = [
  // The frame.
  '--backdrop',
  '--chrome',
  '--sidebar',
  '--surface',
  '--shell-gap',
  '--shell-radius',
  '--shell-rule',
  '--shell-shadow',
  // The content.
  '--bg',
  '--bg-sunken',
  '--bg-raised',
  '--bg-panel',
  '--bg-hover',
  '--bg-active',
  '--bg-selected',
  '--card',
  // Floating layers.
  '--float',
  '--glass-blur',
  '--edge',
  '--scrim',
  // Text and lines.
  '--fg',
  '--fg-muted',
  '--fg-subtle',
  '--fg-faint',
  '--fg-on-accent',
  '--border',
  '--border-strong',
  // Meaning.
  '--accent',
  '--accent-hover',
  '--app-lightness',
  '--app-chroma',
  '--danger',
  '--success',
  '--warning',
  '--info',
  '--star',
  '--highlight',
  '--aqi-1',
  '--aqi-2',
  '--aqi-5',
  '--aqi-6',
  // Elevation.
  '--shadow-sm',
  '--shadow',
  '--shadow-lg',
] as const

/** The apps with a colour of their own, each a `--app-<id>` token derived in
 *  `theme.css` from the theme's `--app-lightness` and `--app-chroma`. */
export const APP_COLOURS = [
  'overview',
  'notes',
  'todo',
  'calendar',
  'library',
  'mail',
  'feed',
  'money',
  'screentime',
  'journal',
  'assistant',
] as const

/** Tokens computed from the others once, in `theme.css`'s `[data-theme]` rule,
 *  rather than set by each theme. */
export const DERIVED_TOKENS = [
  '--aqi-3',
  '--aqi-4',
  ...APP_COLOURS.map((app) => `--app-${app}`),
] as const

const BY_ID = new Map<string, ThemeInfo>(THEMES.map((t) => [t.id, t]))

export function themeInfo(id: ThemeId): ThemeInfo {
  return BY_ID.get(id)!
}

/** A stored or `?theme=` value, checked: anything that is not a theme -- one
 *  since removed, a typo in a URL -- is the desktop's choice. */
export function parseThemeChoice(raw: string | null): ThemeChoice {
  return raw !== null && BY_ID.has(raw) ? (raw as ThemeId) : 'system'
}

function prefersDark(): boolean {
  return typeof matchMedia === 'function' && matchMedia('(prefers-color-scheme: dark)').matches
}

/** The theme to draw for a choice: itself, or Paper or Ink by the desktop. */
export function resolveTheme(choice: ThemeChoice): ThemeId {
  if (choice !== 'system') return choice
  return prefersDark() ? 'dark' : 'light'
}

/** Set a theme on the window. `data-theme` is always present -- `system` is
 *  resolved here, not left to a media query -- so the stylesheet needs one
 *  block per theme and no second copy of Ink inside `prefers-color-scheme`. */
export function showTheme(id: ThemeId): void {
  document.documentElement.setAttribute('data-theme', id)
}

/** Call `onchange` when the desktop switches between light and dark. */
export function watchSystemScheme(onchange: () => void): void {
  if (typeof matchMedia !== 'function') return
  matchMedia('(prefers-color-scheme: dark)').addEventListener?.('change', onchange)
}
