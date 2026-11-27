// How the Feed sets an article for reading: the face, the size, the spacing,
// the width of the column and the colour of the page -- what a read-it-later
// app lets somebody choose, kept on this device (`reading.svelte.ts`).
//
// A story's document is drawn in a sandboxed frame that can read nothing of
// this window, so it cannot follow a custom property defined here. It is
// written with its own (`STYLE` in `crates/jatayu-service/src/news/view.rs`),
// `--read-font`, `--read-size` and `--read-leading`, and the window adds a
// rule setting them before it hands the document over: {@link withReading}.
// The page colour goes the way the theme's always has, as the frame colours
// `applyBodyTheme` paints the document in -- {@link readingPage} names them.
//
// The frame is as tall as `estimateArticleHeight` says the document is, never
// measured, so the type chosen here is also what that estimate is told:
// {@link readingType}.
//
// A newsletter is mail, drawn by mail's own document and often designed down
// to its fonts, so none of this applies to one.

import type { Frame } from './themes'

export const READING_FONTS = ['sans', 'serif'] as const
export type ReadingFont = (typeof READING_FONTS)[number]

export const READING_SPACINGS = ['tight', 'normal', 'loose'] as const
export type ReadingSpacing = (typeof READING_SPACINGS)[number]

export const READING_WIDTHS = ['narrow', 'medium', 'wide'] as const
export type ReadingWidth = (typeof READING_WIDTHS)[number]

/** `app` is the window's own theme; the rest are a page of their own. */
export const READING_PAGES = ['app', 'light', 'sepia', 'dark'] as const
export type ReadingPageName = (typeof READING_PAGES)[number]

/** The sizes the smaller and larger buttons step through, in pixels. */
export const READING_SIZES = [14, 15, 16, 17, 18, 20, 22, 24, 28] as const

export interface ReadingSettings {
  font: ReadingFont
  /** One of {@link READING_SIZES}. */
  size: number
  spacing: ReadingSpacing
  width: ReadingWidth
  page: ReadingPageName
}

/** The document's own defaults, as `STYLE` writes them: nothing chosen. */
export const DEFAULT_READING: ReadingSettings = {
  font: 'sans',
  size: 18,
  spacing: 'normal',
  width: 'medium',
  page: 'app',
}

export const FONT_LABELS: Record<ReadingFont, string> = { sans: 'Sans', serif: 'Serif' }
export const SPACING_LABELS: Record<ReadingSpacing, string> = {
  tight: 'Tight',
  normal: 'Normal',
  loose: 'Loose',
}
export const WIDTH_LABELS: Record<ReadingWidth, string> = {
  narrow: 'Narrow',
  medium: 'Medium',
  wide: 'Wide',
}
export const PAGE_LABELS: Record<ReadingPageName, string> = {
  app: 'Theme',
  light: 'White',
  sepia: 'Sepia',
  dark: 'Night',
}

/**
 * The faces, as font stacks. Sans is the app's reading face (`--font-read` in
 * `theme.css`), which is also the document's default. Serif is only ever a
 * choice: the app sets nothing in one, but a long article is where somebody
 * may want one. Both end in faces most systems have, since the frame cannot
 * load the fonts the app bundles.
 */
export const FONT_STACKS: Record<ReadingFont, string> = {
  sans:
    "'Source Sans 3 Variable', 'Source Sans Pro', 'Seravek', 'Ubuntu', 'Segoe UI Variable Text', " +
    "'Segoe UI', -apple-system, BlinkMacSystemFont, system-ui, 'Noto Sans', sans-serif",
  serif:
    "'Iowan Old Style', 'Charter', 'Bitstream Charter', 'Sitka Text', 'Cambria', 'Noto Serif', " +
    "'Source Serif 4', 'DejaVu Serif', Georgia, serif",
}

/** Line height for each spacing; `normal` is the document's own. */
export const LEADING: Record<ReadingSpacing, number> = { tight: 1.45, normal: 1.6, loose: 1.8 }

/** How wide the reading pane's column may grow, in pixels; `medium` is what it always was. */
export const COLUMN_PX: Record<ReadingWidth, number> = { narrow: 640, medium: 780, wide: 980 }

/** A value from `list`, or `fallback` for anything else. */
function oneOf<T extends string>(list: readonly T[], value: unknown, fallback: T): T {
  return list.find((v) => v === value) ?? fallback
}

/** The step in {@link READING_SIZES} nearest `size`. */
function nearestSize(size: number): number {
  return READING_SIZES.reduce((best, s) => (Math.abs(s - size) < Math.abs(best - size) ? s : best))
}

/** `value` as settings, with anything missing or not understood at its default. */
export function normaliseReading(value: unknown): ReadingSettings {
  const v = (value && typeof value === 'object' ? value : {}) as Record<string, unknown>
  const size = typeof v.size === 'number' && Number.isFinite(v.size) ? v.size : DEFAULT_READING.size
  return {
    font: oneOf(READING_FONTS, v.font, DEFAULT_READING.font),
    size: nearestSize(size),
    spacing: oneOf(READING_SPACINGS, v.spacing, DEFAULT_READING.spacing),
    width: oneOf(READING_WIDTHS, v.width, DEFAULT_READING.width),
    page: oneOf(READING_PAGES, v.page, DEFAULT_READING.page),
  }
}

/** What `jatayu.feed.reading` holds, read back; the defaults for nothing or nonsense. */
export function parseReading(raw: string | null): ReadingSettings {
  if (!raw) return { ...DEFAULT_READING }
  try {
    return normaliseReading(JSON.parse(raw))
  } catch {
    return { ...DEFAULT_READING }
  }
}

/** The next size up (`1`) or down (`-1`), or `size` itself at either end. */
export function stepSize(size: number, direction: 1 | -1): number {
  const at = READING_SIZES.indexOf(nearestSize(size) as (typeof READING_SIZES)[number])
  return READING_SIZES[Math.min(READING_SIZES.length - 1, Math.max(0, at + direction))]!
}

export function isDefaultReading(s: ReadingSettings): boolean {
  return (Object.keys(DEFAULT_READING) as (keyof ReadingSettings)[]).every(
    (key) => s[key] === DEFAULT_READING[key],
  )
}

// ── The document ─────────────────────────────────────────────────────────

/** The rule that sets the document's type, for the end of its head. */
export function readingStyle(s: ReadingSettings): string {
  return (
    `:root{--read-font:${FONT_STACKS[s.font]};` +
    `--read-size:${nearestSize(s.size)}px;--read-leading:${LEADING[s.spacing]}}`
  )
}

/**
 * `html` set in `s`'s type: {@link readingStyle} added at the end of the head,
 * after the document's own defaults, so it is the one that holds. A string
 * with no head is not a document this can add to, and is handed back as it
 * was.
 */
export function withReading(html: string, s: ReadingSettings): string {
  const end = html.search(/<\/head>/i)
  if (end < 0) return html
  return `${html.slice(0, end)}<style>${readingStyle(s)}</style>${html.slice(end)}`
}

// ── The page ─────────────────────────────────────────────────────────────

/** A page of its own: what the frame is painted in, and the pane around it. */
export interface ReadingPage {
  dark: boolean
  /** Handed to `applyBodyTheme` in place of the theme's. */
  frame: Frame
  /**
   * The window's own colour tokens, set on the pane that holds the frame so
   * the title and everything around the article stand on the same page.
   */
  tokens: Record<string, string>
}

const PAGES: Record<Exclude<ReadingPageName, 'app'>, ReadingPage> = {
  light: {
    dark: false,
    frame: { card: '#ffffff', page: '#ffffff', text: '#1c1a17', subtle: '#7b766e' },
    tokens: {
      '--bg': '#ffffff',
      '--bg-raised': '#ffffff',
      '--bg-sunken': '#f2f1ee',
      '--bg-hover': 'rgb(0 0 0 / 0.04)',
      '--fg': '#1c1a17',
      '--fg-muted': '#55514a',
      '--fg-subtle': '#7b766e',
      '--fg-faint': '#9a948b',
      '--border': 'rgb(0 0 0 / 0.1)',
      '--border-strong': 'rgb(0 0 0 / 0.18)',
    },
  },
  sepia: {
    dark: false,
    frame: { card: '#f4ecd8', page: '#f4ecd8', text: '#433422', subtle: '#7a6449' },
    tokens: {
      '--bg': '#f4ecd8',
      '--bg-raised': '#f8f2e3',
      '--bg-sunken': '#ebe0c5',
      '--bg-hover': 'rgb(67 52 34 / 0.07)',
      '--fg': '#433422',
      '--fg-muted': '#5d4a35',
      '--fg-subtle': '#7a6449',
      '--fg-faint': '#958061',
      '--border': 'rgb(67 52 34 / 0.14)',
      '--border-strong': 'rgb(67 52 34 / 0.24)',
    },
  },
  dark: {
    dark: true,
    frame: { card: '#141317', page: '#141317', text: '#dcdae1', subtle: '#8a8792' },
    tokens: {
      '--bg': '#141317',
      '--bg-raised': '#201f24',
      '--bg-sunken': '#0e0d10',
      '--bg-hover': 'rgb(255 255 255 / 0.05)',
      '--fg': '#dcdae1',
      '--fg-muted': '#b4b1bd',
      '--fg-subtle': '#8a8792',
      '--fg-faint': '#6d6a75',
      '--border': 'rgb(255 255 255 / 0.09)',
      '--border-strong': 'rgb(255 255 255 / 0.16)',
    },
  },
}

/** The page `s` chose, or `null` for the window's own theme. */
export function readingPage(s: ReadingSettings): ReadingPage | null {
  return s.page === 'app' ? null : PAGES[s.page]
}

/** The swatch a page is offered by: its paper and its ink. */
export function pageSwatch(name: ReadingPageName): { paper: string; ink: string } | null {
  const page = name === 'app' ? null : PAGES[name]
  return page ? { paper: page.frame.page, ink: page.frame.text } : null
}

/**
 * The pane's inline style for a page of its own: its colour tokens, and the
 * paper and ink themselves. Empty for the window's theme.
 */
export function paneStyle(s: ReadingSettings): string {
  const page = readingPage(s)
  if (!page) return ''
  const tokens = Object.entries(page.tokens).map(([k, v]) => `${k}:${v}`)
  return [
    ...tokens,
    `background:${page.frame.page}`,
    `color:${page.frame.text}`,
    `color-scheme:${page.dark ? 'dark' : 'light'}`,
  ].join(';')
}

// ── Sizing the frame ─────────────────────────────────────────────────────

/**
 * The type `estimateArticleHeight` is told the document is set in: the size,
 * the line height, how wide an average letter is (as a fraction of the size --
 * a serif runs wider than this sans), and the frame's padding as `STYLE`
 * writes it.
 */
export interface ArticleType {
  sizePx: number
  leading: number
  letterEm: number
  /** Padding on each side, from the frame's width. */
  padX: (widthPx: number) => number
  /** Padding above and below together. */
  padY: number
}

/** A story's document in `s`'s type. */
export function readingType(s: ReadingSettings): ArticleType {
  return {
    sizePx: nearestSize(s.size),
    leading: LEADING[s.spacing],
    letterEm: s.font === 'serif' ? 0.53 : 0.5,
    // `padding: 20px clamp(16px, 4vw, 28px) 28px`.
    padX: (width) => Math.min(28, Math.max(16, width * 0.04)),
    padY: 48,
  }
}

// ── How long it takes ────────────────────────────────────────────────────

/** Words a minute, for the "min read" under a title: an unhurried reader's pace. */
export const WORDS_PER_MINUTE = 230

/** Below this many words a story is a note, and saying how long it takes is noise. */
const WORTH_TIMING = 200

/**
 * Minutes to read `text` at {@link WORDS_PER_MINUTE}, rounded up; `null` for
 * something too short to be worth saying.
 */
export function minutesToRead(text: string): number | null {
  const words = text.split(/\s+/).filter((w) => /[\p{L}\p{N}]/u.test(w)).length
  if (words < WORTH_TIMING) return null
  return Math.max(1, Math.ceil(words / WORDS_PER_MINUTE))
}
