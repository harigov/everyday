// The filled icons: the app bar's, and every sidebar's.
//
// `icons.ts` is line art, for toolbars, menus and buttons, where a row of
// solid shapes would shout. Here the same ideas are drawn as filled shapes
// in two tones -- what Apple calls hierarchical symbols -- because an app's
// icon and a sidebar's rows are where the eye goes looking, and outlines at
// that size read as thin grey scribbles on a coloured backdrop.
//
// Each symbol is up to four layers on the same 24-unit grid as `icons.ts`:
//   - `back`  the secondary shape, drawn in the current colour at reduced
//             strength (see `Symbol.svelte`), so a symbol has depth without
//             a second colour of its own.
//   - `front` the primary shape, in the current colour.
//   - `cut`   detail punched out of `front` -- a check in a disc, lines on a
//             page -- so whatever is behind shows through. Cut rather than
//             painted, because a symbol has one colour and the backdrop is
//             not known.
//   - `mark`  detail painted at full strength over `back` -- the hands on a
//             clock face. `{day}` in a mark is replaced by today's date.
// An element with `class="s"` is stroked rather than filled (round caps,
// 2.3 units unless its own style says otherwise); everything else is filled.
//
// House rules, so additions stay consistent:
//   - Same live area as the line icons, roughly 20×20 inside the 24 box,
//     and the same silhouettes, so a sidebar row does not change shape
//     between its line and filled forms.
//   - The bigger, simpler shape is `front`; the supporting one `back`.
//   - Detail that must read is cut, never drawn in another colour.
//
// A name here that is also an `IconName` is that icon's filled form, and
// `Symbol.svelte` falls back to the line icon for any name without one. The
// few names that are not icons exist because one line icon serves two
// purposes the filled forms need to tell apart: a pencil is Drafts, but
// Notes is a page.

import type { IconName } from './icons'

export interface SymbolArt {
  back?: string
  front: string
  cut?: string
  mark?: string
}

export type SymbolName =
  | IconName
  | 'note'
  | 'notebook'
  | 'send'
  | 'health-today'
  | 'health-activity'
  | 'health-sleep'
  | 'health-vitals'
  | 'health-nutrition'
  | 'health-medications'
  | 'health-conditions'
  | 'health-records'

const page = (x: number, y: number, w: number, h: number) =>
  `<rect x="${x}" y="${y}" width="${w}" height="${h}" rx="3.2"/>`
const header = '<path d="M3 7.2A3.2 3.2 0 0 1 6.2 4h11.6A3.2 3.2 0 0 1 21 7.2V9.6H3Z"/>'

export const SYMBOLS: Partial<Record<SymbolName, SymbolArt>> = {
  // ── Apps ────────────────────────────────────────────────────────────────
  compass: {
    back: '<circle cx="12" cy="12" r="9.5"/>',
    front: '<path d="M16.6 7.4 13.7 13.7 7.4 16.6 10.3 10.3Z"/>',
    cut: '<circle cx="12" cy="12" r="1.3"/>',
  },
  note: {
    back: '<rect x="7" y="2" width="13" height="16" rx="2.6"/>',
    front: '<rect x="4" y="5" width="13.5" height="17" rx="2.6"/>',
    cut:
      '<rect x="6.8" y="10" width="8" height="1.7" rx=".85"/>' +
      '<rect x="6.8" y="13.6" width="8" height="1.7" rx=".85"/>' +
      '<rect x="6.8" y="17.2" width="5" height="1.7" rx=".85"/>',
  },
  check: {
    back: '<circle cx="12" cy="12" r="10.5"/>',
    front: '<circle cx="12" cy="12" r="8"/>',
    cut: '<path class="s" d="m8.4 12.2 2.5 2.5 4.8-5"/>',
  },
  calendar: {
    back: page(3, 4, 18, 17.5),
    front: header,
    mark: '<text x="12" y="19" text-anchor="middle" font-size="9" font-weight="760" font-family="Inter Variable, Inter, sans-serif">{day}</text>',
  },
  book: {
    back: '<path d="M5 18.5A2.5 2.5 0 0 1 7.5 16H20v6H7.5A2.5 2.5 0 0 1 5 19.5Z"/>',
    front: '<path d="M5 4A2 2 0 0 1 7 2h12a1 1 0 0 1 1 1v13H7.5A2.5 2.5 0 0 0 5 18.5Z"/>',
    cut: '<path d="M13 2h4v6.5l-2-1.4-2 1.4Z"/>',
  },
  mail: {
    back: '<rect x="2.5" y="4.5" width="19" height="15" rx="3.2"/>',
    front: '<path d="M3.1 6.4A3.2 3.2 0 0 1 5.7 4.5h12.6a3.2 3.2 0 0 1 2.6 1.9L12 13.2Z"/>',
  },
  rss: {
    back: '<path d="M3.5 3a17.5 17.5 0 0 1 17.5 17.5H3.5Z"/>',
    front:
      '<circle cx="6.4" cy="17.6" r="2.4"/><path class="s" d="M4.5 10.6a8.9 8.9 0 0 1 8.9 8.9"/>',
  },
  wallet: {
    back: '<rect x="5.5" y="2" width="13" height="8.5" rx="2" transform="rotate(-9 12 6)"/>',
    front: '<rect x="2.5" y="6.5" width="19" height="15" rx="3.5"/>',
    cut: '<rect x="14.5" y="11.6" width="7" height="4.8" rx="2.4"/>',
  },
  hourglass: {
    back:
      '<path d="M7 4.4h10v2.2c0 1.7-.9 2.9-2.4 3.9L12 12.3l-2.6-1.8C7.9 9.5 7 8.3 7 6.6Z"/>' +
      '<path d="M7 19.6h10v-2.2c0-1.7-.9-2.9-2.4-3.9L12 11.7l-2.6 1.8C7.9 14.5 7 15.7 7 17.4Z"/>',
    front:
      '<rect x="4.5" y="2" width="15" height="2.6" rx="1.3"/>' +
      '<rect x="4.5" y="19.4" width="15" height="2.6" rx="1.3"/>' +
      '<path d="M8.6 19.4c.4-1.8 1.7-3 3.4-3s3 1.2 3.4 3Z"/>' +
      '<path d="M9 6.6h6c-.5 1.1-1.5 1.9-3 2.8-1.5-.9-2.5-1.7-3-2.8Z"/>',
  },
  'heart-pulse': {
    front:
      '<path d="M12 21.5 4.6 14.2C2.9 12.6 2 10.9 2 8.9 2 5.9 4.3 3.5 7.3 3.5c1.8 0 3.3.9 4.7 2.6 1.4-1.7 2.9-2.6 4.7-2.6 3 0 5.3 2.4 5.3 5.4 0 2-.9 3.7-2.6 5.3Z"/>',
    cut: '<path class="s" style="stroke-width:1.9" d="M5.5 12.4h3.6l1.3-2.1 2.2 5 2.3-7.2 1.5 4.3h2.6"/>',
  },
  notebook: {
    back: '<rect x="15" y="3.5" width="5.5" height="17" rx="2"/>',
    front: '<rect x="3.5" y="2" width="15" height="20" rx="3"/>',
    cut:
      '<rect x="13.2" y="2" width="1.8" height="20"/>' +
      '<rect x="6.5" y="6.5" width="4.5" height="2.6" rx="1.1"/>',
  },
  sparkle: {
    back: '<path d="m18.2 14.6 1 2.3 2.3 1-2.3 1-1 2.3-1-2.3-2.3-1 2.3-1Z"/>',
    front: '<path d="M10.5 2.5 12.6 7.9 18 10 12.6 12.1 10.5 17.5 8.4 12.1 3 10 8.4 7.9Z"/>',
  },
  // The wing of the application icon (from src/lib/wing.ts): the long front
  // feather is `front`, the eight shorter ones behind it `back`.
  wing: {
    back:
      '<path d="M16.79 3.36L15.90 3.64L15.14 3.97L14.20 4.50L13.26 5.18L12.75 5.62L12.11 6.30L11.49 7.10L10.91 8.05L10.42 9.16L10.14 10.02L9.95 10.88L9.83 11.99L9.83 12.96L9.96 14.11L10.21 15.17L10.49 15.98L10.87 16.82L11.39 17.70L11.90 18.40L12.31 18.87L12.35 17L12.52 15.19L12.82 13.39L13.27 11.61L13.60 10.56L13.96 9.58L14.36 8.62L14.83 7.63L15.39 6.58L15.86 5.78L16.48 4.82L17.16 3.85L17.19 3.67L17.12 3.50L16.98 3.39L16.79 3.36Z"/>' +
      '<path d="M12.66 4.35L12.48 4.41L12.05 4.68L11.20 5.34L10.52 6.03L9.88 6.84L9.36 7.69L8.92 8.65L8.66 9.46L8.43 10.56L8.36 11.40L8.40 12.52L8.55 13.47L8.85 14.55L9.17 15.32L9.59 16.11L10.29 17.09L10.94 17.81L11.58 18.36L12.05 18.71L12.31 18.86L11.91 17.27L11.64 15.67L11.48 14.11L11.44 12.43L11.49 11.34L11.58 10.44L11.72 9.37L11.91 8.38L12.13 7.45L12.39 6.50L13 4.76L13 4.60L12.93 4.47L12.80 4.38L12.66 4.35Z"/>' +
      '<path d="M9.25 6.19L9.11 6.24L8.98 6.36L8.28 7.25L7.82 8.01L7.39 8.98L7.13 9.86L6.98 10.82L6.94 11.39L6.95 11.97L7.05 12.90L7.29 13.87L7.56 14.59L7.99 15.45L8.50 16.21L9.16 16.98L9.71 17.47L10.40 17.98L11.18 18.43L11.81 18.71L12.31 18.87L11.59 17.58L10.98 16.26L10.45 14.84L10.04 13.39L9.70 11.65L9.51 9.88L9.49 8.30L9.61 6.47L9.57 6.35L9.49 6.26L9.38 6.20L9.25 6.19Z"/>' +
      '<path d="M6.75 8.58L6.62 8.64L6.51 8.77L6.20 9.44L5.94 10.22L5.75 11.17L5.71 11.72L5.74 12.67L5.86 13.45L6.03 14.10L6.32 14.85L6.62 15.44L6.96 15.96L7.42 16.55L8.06 17.19L8.54 17.57L9.13 17.96L9.78 18.29L10.23 18.48L11.01 18.71L11.66 18.83L12.30 18.88L11.35 17.88L10.47 16.83L9.69 15.70L8.99 14.52L8.28 13.03L7.75 11.60L7.38 10.26L7.08 8.77L6.95 8.62L6.75 8.58Z"/>' +
      '<path d="M5.16 11.24L5.02 11.30L4.96 11.36L4.89 11.52L4.76 12.51L4.76 13.15L4.83 13.81L5.01 14.62L5.23 15.22L5.55 15.87L5.92 16.45L6.27 16.90L6.72 17.37L7.31 17.86L7.74 18.14L8.41 18.49L8.80 18.65L9.59 18.89L10.30 19L11.07 19.04L11.84 18.98L12.30 18.88L11.44 18.37L10.32 17.59L9.51 16.93L9.03 16.49L8.30 15.75L7.50 14.81L6.62 13.58L5.97 12.46L5.44 11.37L5.32 11.26L5.16 11.24Z"/>' +
      '<path d="M4.37 13.92L4.24 13.99L4.19 14.05L4.15 14.29L4.24 14.98L4.39 15.59L4.59 16.15L4.91 16.77L5.26 17.30L5.62 17.73L5.95 18.05L6.55 18.54L6.96 18.79L7.42 19.03L7.80 19.19L8.48 19.39L9.05 19.49L9.59 19.53L10.41 19.48L11.07 19.36L11.71 19.16L12.30 18.88L11.18 18.56L10.12 18.16L9.05 17.65L8.12 17.11L7.34 16.58L6.53 15.95L5.63 15.12L4.61 13.99L4.50 13.93L4.37 13.92Z"/>' +
      '<path d="M4.36 16.42L4.25 16.46L4.17 16.55L4.13 16.65L4.14 16.78L4.32 17.23L4.65 17.85L4.89 18.20L5.23 18.62L5.65 19.01L6.03 19.31L6.54 19.62L7.14 19.90L8.03 20.14L8.60 20.21L9.07 20.22L9.96 20.11L10.58 19.94L11.12 19.71L11.67 19.40L12.08 19.09L12.29 18.89L11.22 18.87L10.30 18.77L9.24 18.58L8.24 18.32L7.43 18.04L6.54 17.65L5.64 17.18L4.54 16.46L4.36 16.42Z"/>' +
      '<path d="M4.90 18.60L4.79 18.65L4.72 18.74L4.70 18.86L4.73 18.97L5.10 19.46L5.42 19.79L5.79 20.10L6.37 20.46L7.18 20.80L7.69 20.92L8.17 20.98L9 20.96L9.83 20.79L10.60 20.47L11.10 20.17L11.58 19.78L12.06 19.25L12.29 18.90L11.41 19.11L10.49 19.27L9.59 19.34L8.63 19.35L7.79 19.28L6.94 19.15L6.03 18.93L5.04 18.61L4.90 18.60Z"/>',
    front:
      '<path d="M21.38 3.50L20.41 3.57L19.18 3.79L18.18 4.08L16.98 4.58L15.88 5.21L14.84 5.98L14.04 6.73L13.31 7.58L12.65 8.54L12.08 9.62L11.60 10.84L11.35 11.81L11.18 12.83L11.11 14.09L11.20 15.33L11.37 16.31L11.60 17.15L11.83 17.81L12.11 18.46L12.32 18.85L12.86 16.96L13.23 15.90L13.65 14.84L14.32 13.38L15.31 11.55L15.99 10.47L16.60 9.59L17.26 8.71L18.05 7.76L18.72 7.01L19.65 6.07L20.74 5.06L21.89 4.10L21.95 3.99L21.97 3.86L21.94 3.74L21.88 3.63L21.69 3.51L21.38 3.50Z"/>',
  },
  settings: {
    back:
      '<rect x="3" y="5.5" width="18" height="3" rx="1.5"/>' +
      '<rect x="3" y="15.5" width="18" height="3" rx="1.5"/>',
    front: '<circle cx="8.5" cy="7" r="3.4"/><circle cx="15.5" cy="17" r="3.4"/>',
  },
  lock: {
    back: '<path class="s" d="M7.8 10.5V7.2a4.2 4.2 0 0 1 8.4 0v3.3"/>',
    front: '<rect x="4" y="10" width="16" height="12" rx="3"/>',
    cut: '<rect x="11" y="14" width="2" height="4" rx="1"/>',
  },

  // ── Sidebar rows ────────────────────────────────────────────────────────
  inbox: {
    back: '<path d="M6 3.5h12a1.5 1.5 0 0 1 1.4 1L22 12H2l2.6-7.5A1.5 1.5 0 0 1 6 3.5Z"/>',
    front:
      '<path d="M2 12h6l1.5 2.6h5L16 12h6v6.5A2.5 2.5 0 0 1 19.5 21h-15A2.5 2.5 0 0 1 2 18.5Z"/>',
  },
  clock: {
    back: '<circle cx="12" cy="12" r="10"/>',
    front: '',
    mark: '<path class="s" d="M12 6.8V12l3.4 2"/>',
  },
  pencil: {
    back: '<rect x="11" y="19.4" width="10.5" height="2.2" rx="1.1"/>',
    front: '<path d="M16.4 3.1a2.7 2.7 0 0 1 3.8 3.8L8 19.1l-5 1.4 1.4-5Z"/>',
  },
  send: {
    back: '<path d="M22 2 10.2 13.4l3.3 7.9Z"/>',
    front: '<path d="M22 2 2.3 10.3l7.9 3.1Z"/>',
  },
  layers: {
    back:
      '<path d="m3.6 10.6-1 .5a1 1 0 0 0 0 1.8l8.6 3.9a2 2 0 0 0 1.6 0l8.6-3.9a1 1 0 0 0 0-1.8l-1-.5-7.6 3.4a2 2 0 0 1-1.6 0Z"/>' +
      '<path d="m3.6 15.6-1 .5a1 1 0 0 0 0 1.8l8.6 3.9a2 2 0 0 0 1.6 0l8.6-3.9a1 1 0 0 0 0-1.8l-1-.5-7.6 3.4a2 2 0 0 1-1.6 0Z"/>',
    front:
      '<path d="M12.8 2.2a2 2 0 0 0-1.6 0L2.6 6.1a1 1 0 0 0 0 1.8l8.6 3.9a2 2 0 0 0 1.6 0l8.6-3.9a1 1 0 0 0 0-1.8Z"/>',
  },
  alert: {
    front: '<path d="M10.3 3.4a2 2 0 0 1 3.4 0l8.3 14.4a2 2 0 0 1-1.7 3H3.7a2 2 0 0 1-1.7-3Z"/>',
    cut: '<rect x="11" y="8.4" width="2" height="6.2" rx="1"/><circle cx="12" cy="17.3" r="1.2"/>',
  },
  trash: {
    back: '<path d="M5 8.5h14l-1.1 11.1A2.6 2.6 0 0 1 15.3 22H8.7a2.6 2.6 0 0 1-2.6-2.4Z"/>',
    front:
      '<rect x="2.5" y="4.4" width="19" height="2.8" rx="1.4"/>' +
      '<path d="M8.6 4.4v-.9A1.5 1.5 0 0 1 10.1 2h3.8a1.5 1.5 0 0 1 1.5 1.5v.9Z"/>',
  },
  star: {
    front:
      '<path d="M11.53 2.3a.53.53 0 0 1 .94 0l2.6 5.26a.53.53 0 0 0 .4.29l5.81.85a.53.53 0 0 1 .3.9l-4.2 4.1a.53.53 0 0 0-.15.46l.99 5.78a.53.53 0 0 1-.77.56l-5.2-2.73a.53.53 0 0 0-.5 0l-5.2 2.73a.53.53 0 0 1-.76-.56l.99-5.78a.53.53 0 0 0-.16-.47l-4.2-4.09a.53.53 0 0 1 .3-.9l5.8-.85a.53.53 0 0 0 .4-.3Z"/>',
  },
  tag: {
    front:
      '<path d="M11.6 3.5H19a1.5 1.5 0 0 1 1.5 1.5v7.4a2 2 0 0 1-.6 1.4l-6.6 6.6a2 2 0 0 1-2.8 0l-6-6a2 2 0 0 1 0-2.8l6.6-6.6a2 2 0 0 1 1.5-.5Z"/>',
    cut: '<circle cx="16" cy="8" r="1.7"/>',
  },
  sun: {
    back: '<path class="s" d="M12 2.5v2.2M12 19.3v2.2M2.5 12h2.2M19.3 12h2.2M5.3 5.3l1.6 1.6M17.1 17.1l1.6 1.6M5.3 18.7l1.6-1.6M17.1 6.9l1.6-1.6"/>',
    front: '<circle cx="12" cy="12" r="4.8"/>',
  },
  target: {
    back: '<circle cx="12" cy="12" r="9.5"/>',
    front: '<circle cx="12" cy="12" r="6.2"/>',
    cut: '<circle cx="12" cy="12" r="3.5"/>',
    mark: '<circle cx="12" cy="12" r="1.7"/>',
  },
  chart: {
    back:
      '<rect x="3.5" y="11" width="4.6" height="10" rx="1.6"/>' +
      '<rect x="15.9" y="7" width="4.6" height="14" rx="1.6"/>',
    front: '<rect x="9.7" y="3" width="4.6" height="18" rx="1.6"/>',
  },
  grid: {
    back:
      '<rect x="13.5" y="3" width="7.5" height="7.5" rx="2"/>' +
      '<rect x="3" y="13.5" width="7.5" height="7.5" rx="2"/>',
    front:
      '<rect x="3" y="3" width="7.5" height="7.5" rx="2"/>' +
      '<rect x="13.5" y="13.5" width="7.5" height="7.5" rx="2"/>',
  },
  day: {
    back: page(3, 4, 18, 17.5),
    front: header,
    mark:
      '<rect x="6.5" y="12.6" width="11" height="2" rx="1"/>' +
      '<rect x="6.5" y="16.4" width="7" height="2" rx="1"/>',
  },
  week: {
    back: page(3, 4, 18, 17.5),
    front: header,
    mark:
      '<rect x="8.2" y="11.8" width="1.8" height="7.6" rx=".9"/>' +
      '<rect x="14" y="11.8" width="1.8" height="7.6" rx=".9"/>',
  },
  month: {
    back: page(3, 4, 18, 17.5),
    front: header,
    mark:
      '<rect x="6" y="12" width="3" height="2.6" rx=".8"/>' +
      '<rect x="10.5" y="12" width="3" height="2.6" rx=".8"/>' +
      '<rect x="15" y="12" width="3" height="2.6" rx=".8"/>' +
      '<rect x="6" y="16.2" width="3" height="2.6" rx=".8"/>' +
      '<rect x="10.5" y="16.2" width="3" height="2.6" rx=".8"/>',
  },
  monitor: {
    back: '<rect x="11" y="16" width="2" height="4.5"/><rect x="7.5" y="19.6" width="9" height="2" rx="1"/>',
    front: '<rect x="2" y="3.5" width="20" height="13.5" rx="2.6"/>',
  },
  receipt: {
    front:
      '<path d="M4.5 3.2A1.2 1.2 0 0 1 5.7 2h12.6a1.2 1.2 0 0 1 1.2 1.2V21.4l-2.5-1.3-2.5 1.3-2.5-1.3-2.5 1.3-2.5-1.3-2.5 1.3Z"/>',
    cut:
      '<rect x="7.5" y="6.5" width="9" height="1.9" rx=".95"/>' +
      '<rect x="7.5" y="10.5" width="9" height="1.9" rx=".95"/>' +
      '<rect x="7.5" y="14.5" width="5" height="1.9" rx=".95"/>',
  },
  landmark: {
    back:
      '<rect x="4.6" y="10.5" width="2.5" height="7.5" rx=".6"/>' +
      '<rect x="8.7" y="10.5" width="2.5" height="7.5" rx=".6"/>' +
      '<rect x="12.8" y="10.5" width="2.5" height="7.5" rx=".6"/>' +
      '<rect x="16.9" y="10.5" width="2.5" height="7.5" rx=".6"/>',
    front:
      '<path d="M11.1 2.2a2 2 0 0 1 1.8 0l7.9 3.9c.8.4.5 1.6-.4 1.6H3.6c-.9 0-1.2-1.2-.4-1.6Z"/>' +
      '<rect x="2.5" y="19" width="19" height="2.6" rx="1.3"/>',
  },
  filter: {
    front:
      '<rect x="2.5" y="4.7" width="19" height="2.8" rx="1.4"/>' +
      '<rect x="6" y="10.6" width="12" height="2.8" rx="1.4"/>' +
      '<rect x="9.5" y="16.5" width="5" height="2.8" rx="1.4"/>',
  },
  hidden: {
    front:
      '<path d="M12 5.5c5.2 0 9.4 4.4 10.3 6.5-.9 2.1-5.1 6.5-10.3 6.5S2.6 14.1 1.7 12C2.6 9.9 6.8 5.5 12 5.5Z"/>',
    cut: '<circle cx="12" cy="12" r="3.2"/><path class="s" style="stroke-width:4.4" d="M3.5 3.5l17 17"/>',
    mark: '<path class="s" style="stroke-width:2" d="M3.5 3.5l17 17"/>',
  },
  newspaper: {
    back: '<path d="M2 9h3.5v10.5a1.75 1.75 0 0 1-3.5 0Z"/>',
    front: '<rect x="5" y="2.5" width="17" height="19" rx="2.6"/>',
    cut:
      '<rect x="8" y="6" width="8.5" height="4.2" rx="1"/>' +
      '<rect x="8" y="13" width="11" height="1.8" rx=".9"/>' +
      '<rect x="8" y="16.5" width="7" height="1.8" rx=".9"/>',
  },
  plus: {
    front: '<circle cx="12" cy="12" r="9.5"/>',
    cut: '<path class="s" d="M12 7.5v9M7.5 12h9"/>',
  },
  tick: {
    front: '<circle cx="12" cy="12" r="9.5"/>',
    cut: '<path class="s" d="m7.8 12.3 2.8 2.8 5.6-5.8"/>',
  },
  globe: {
    back: '<circle cx="12" cy="12" r="9.5"/>',
    mark:
      '<path class="s" style="stroke-width:1.7" d="M2.8 12h18.4"/>' +
      '<path class="s" style="stroke-width:1.7" d="M12 2.6c2.6 2.6 3.9 5.7 3.9 9.4s-1.3 6.8-3.9 9.4c-2.6-2.6-3.9-5.7-3.9-9.4S9.4 5.2 12 2.6Z"/>',
    front: '',
  },
  mic: {
    back:
      '<path class="s" d="M5.5 11a6.5 6.5 0 0 0 13 0"/>' +
      '<rect x="11" y="17" width="2" height="4"/><rect x="8" y="19.8" width="8" height="2" rx="1"/>',
    front: '<rect x="8.5" y="2" width="7" height="12.5" rx="3.5"/>',
  },
  people: {
    back:
      '<circle cx="16.8" cy="8.2" r="3.1"/>' +
      '<path d="M15.6 14.4a4.6 4.6 0 0 1 6.9 4v2.1a1 1 0 0 1-1 1h-3.8V20a6.6 6.6 0 0 0-2.1-5.6Z"/>',
    front:
      '<circle cx="9" cy="7.4" r="3.9"/>' +
      '<path d="M2 20.5v-.6a5 5 0 0 1 5-5h4a5 5 0 0 1 5 5v.6a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1Z"/>',
  },
  // A box with its lid: Settings → Backup.
  archive: {
    back: '<path d="M3.5 9.5h17v9a2.5 2.5 0 0 1-2.5 2.5H6a2.5 2.5 0 0 1-2.5-2.5Z"/>',
    front:
      '<rect x="2" y="3" width="20" height="5.5" rx="1.6"/>' + '<path class="s" d="M10 13.5h4"/>',
  },
  upload: {
    back: '<path d="M2.5 14.5h4.5v2.6a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1v-2.6h4.5v4.4a2.6 2.6 0 0 1-2.6 2.6H5.1a2.6 2.6 0 0 1-2.6-2.6Z"/>',
    front: '<path class="s" d="M12 3.5v10.5M7.5 8l4.5-4.5L16.5 8"/>',
  },
  // A folder with its tab: the Files app, and a folder in it.
  folder: {
    back: '<path d="M2 5.5A2.5 2.5 0 0 1 4.5 3h4.2a2.5 2.5 0 0 1 1.9.9L12 5.5h7.5A2.5 2.5 0 0 1 22 8v2H2Z"/>',
    front:
      '<path d="M2 9.5A2.5 2.5 0 0 1 4.5 7h15A2.5 2.5 0 0 1 22 9.5v9a2.5 2.5 0 0 1-2.5 2.5h-15A2.5 2.5 0 0 1 2 18.5Z"/>',
  },
  // A picture: the card, with the sun and the hill on it. The Photos view.
  image: {
    back: '<rect x="3" y="3" width="18" height="18" rx="3.2"/>',
    front:
      '<circle cx="8.5" cy="8.5" r="1.9"/>' +
      '<path d="M3 17.2l4.6-4.6a1.6 1.6 0 0 1 2.2 0l3.2 3.2 1.4-1.4a1.6 1.6 0 0 1 2.2 0L21 15.6v2.2a3.2 3.2 0 0 1-3.2 3.2H6.2A3.2 3.2 0 0 1 3 17.8Z"/>',
  },
  // A page with its corner turned: a file that is not a picture.
  file: {
    back: '<path d="M5 4.2A2.2 2.2 0 0 1 7.2 2H14l6 6v11.8a2.2 2.2 0 0 1-2.2 2.2H7.2A2.2 2.2 0 0 1 5 19.8Z"/>',
    front: '<path d="M14 2v4a2 2 0 0 0 2 2h4Z"/>',
  },
  // ── Health ──────────────────────────────────────────────────────────────
  // The sun: rays behind, disc in front.
  'health-today': {
    back: '<path class="s" d="M12 2.5v2.2M12 19.3v2.2M2.5 12h2.2M19.3 12h2.2M5.3 5.3l1.6 1.6M17.1 17.1l1.6 1.6M5.3 18.7l1.6-1.6M17.1 6.9l1.6-1.6"/>',
    front: '<circle cx="12" cy="12" r="4.8"/>',
  },
  // Rings: three meters, drawn as the ring they replace in other apps' icons.
  'health-activity': {
    back: '<circle cx="12" cy="12" r="9.5"/>',
    front:
      '<path fill-rule="evenodd" d="M12 5.8a6.2 6.2 0 1 0 0 12.4 6.2 6.2 0 0 0 0-12.4Zm0 2.7a3.5 3.5 0 1 1 0 7 3.5 3.5 0 0 1 0-7Z"/>',
    mark: '<circle cx="12" cy="12" r="1.7"/>',
  },
  // The moon, and a star beside it.
  'health-sleep': {
    back: '<circle cx="17.6" cy="6.4" r="1.7"/>',
    front: '<path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/>',
  },
  // A heart with its pulse.
  'health-vitals': {
    back: '<path d="M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4.05 3 5.5l7 7Z"/>',
    front: '',
    mark: '<path class="s" d="M5.5 12.2h3.2l1.8-3 3 6 1.8-3h3.2"/>',
  },
  // An apple and its leaf.
  'health-nutrition': {
    back: '<path d="M12 8.2c0-2.6 1.5-4.4 4-4.7 0 2.5-1.5 4.5-4 4.7Z"/>',
    front:
      '<path d="M12 8.6C10.2 7.2 7.4 7 5.6 8.8 3.6 10.8 4 14.2 5.7 17.2c1.1 2 2.7 3.6 4.1 3.6.9 0 1.4-.5 2.2-.5s1.3.5 2.2.5c1.4 0 3-1.6 4.1-3.6 1.7-3 2.1-6.4.1-8.4-1.8-1.8-4.6-1.6-6.4-.2Z"/>',
  },
  // A capsule, half of it solid.
  'health-medications': {
    back: '<rect x="2.5" y="8.5" width="19" height="7" rx="3.5" transform="rotate(-45 12 12)"/>',
    front: '<path transform="rotate(-45 12 12)" d="M6 8.5h6v7H6a3.5 3.5 0 0 1 0-7Z"/>',
  },
  // A clipboard with a cross.
  'health-conditions': {
    back: '<rect x="4.5" y="4" width="15" height="17.5" rx="3"/>',
    front: '<rect x="8.5" y="2.5" width="7" height="4.5" rx="1.6"/>',
    mark: '<path class="s" d="M12 10.5v6M9 13.5h6"/>',
  },
  // A folder.
  'health-records': {
    back: '<path d="M3 7.5A2.5 2.5 0 0 1 5.5 5h3.7a2 2 0 0 1 1.5.7L12 7h6.5A2.5 2.5 0 0 1 21 9.5v8a2.5 2.5 0 0 1-2.5 2.5h-13A2.5 2.5 0 0 1 3 17.5Z"/>',
    front: '<path d="M3 10.5h18v7a2.5 2.5 0 0 1-2.5 2.5h-13A2.5 2.5 0 0 1 3 17.5Z"/>',
  },
}
