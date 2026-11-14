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

export type SymbolName = IconName | 'note' | 'notebook' | 'send'

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
  upload: {
    back: '<path d="M2.5 14.5h4.5v2.6a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1v-2.6h4.5v4.4a2.6 2.6 0 0 1-2.6 2.6H5.1a2.6 2.6 0 0 1-2.6-2.6Z"/>',
    front: '<path class="s" d="M12 3.5v10.5M7.5 8l4.5-4.5L16.5 8"/>',
  },
}
