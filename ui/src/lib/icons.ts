// The application's icon set.
//
// Drawn rather than typed. The toolbar and sidebar used to be built from
// whatever Unicode symbols came closest -- ▨ for highlight, ❝ for quote, ⌁
// for lock -- which meant their size, weight and baseline were decided by
// whichever font on the user's machine happened to claim that codepoint.
// The result was a row of mismatched glyphs that no amount of CSS could
// align, and on a machine missing one of them, a tofu box.
//
// These are inline SVG instead: one geometry, one stroke weight, one
// baseline, everywhere, and they inherit `currentColor` so hover, active and
// disabled states come for free.
//
// House rules, so additions stay consistent:
//   - 24×24 viewBox, artwork inset to roughly a 20×20 live area.
//   - Strokes only, no fills, except where a filled state is meaningful
//     (a starred entry). `Icon` supplies stroke, cap and join.
//   - Round caps and joins, and geometry snapped to whole or half units so
//     horizontal and vertical strokes land on pixel boundaries at 1x.
//
// Geometry follows the Lucide conventions (ISC licensed) so the shapes are
// the ones people already recognise from every other editor they use.

export type IconName = keyof typeof ICONS

export const ICONS = {
  // ── Formatting marks ───────────────────────────────────────────────────
  bold: '<path d="M6 12h9a4 4 0 0 1 0 8H7a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1h7a4 4 0 0 1 0 8"/>',

  italic: '<path d="M19 4h-9"/><path d="M14 20H5"/><path d="m15 4-6 16"/>',

  underline: '<path d="M6 4v6a6 6 0 0 0 12 0V4"/><path d="M4 20h16"/>',

  // A marker pen mid-stroke, with the wet edge it just laid down.
  highlight:
    '<path d="m9 11-6 6v3h9l3-3"/>' +
    '<path d="m22 12-4.6 4.6a2 2 0 0 1-2.8 0l-5.2-5.2a2 2 0 0 1 0-2.8L14 4"/>',

  // ── Blocks ─────────────────────────────────────────────────────────────
  heading: '<path d="M6 12h12"/><path d="M6 20V4"/><path d="M18 20V4"/>',

  // A full-height rule with the text indented beside it: what a blockquote
  // looks like on the page, rather than a pair of quotation marks, whose
  // shape and weight would be whatever the font decided.
  quote: '<path d="M4 5.5v13"/><path d="M9.5 7h11"/><path d="M9.5 12h11"/><path d="M9.5 17h11"/>',

  code: '<path d="m16 18 6-6-6-6"/><path d="m8 6-6 6 6 6"/>',

  bulletList:
    '<path d="M8 6h13"/><path d="M8 12h13"/><path d="M8 18h13"/>' +
    '<path d="M3.5 6h.01"/><path d="M3.5 12h.01"/><path d="M3.5 18h.01"/>',

  orderedList:
    '<path d="M10 6h11"/><path d="M10 12h11"/><path d="M10 18h11"/>' +
    '<path d="M4 6h1v4"/><path d="M4 10h2"/>' +
    '<path d="M6 18H4c0-1 2-2 2-3s-1-1.5-2-1"/>',

  taskList:
    '<rect width="7" height="7" x="3" y="3.5" rx="1.5"/>' +
    '<path d="m3.5 17 2 2 4-4"/>' +
    '<path d="M14 7h7"/><path d="M14 13h7"/><path d="M14 19h7"/>',

  // The rule itself, with the paragraphs it separates ghosted above and
  // below. A bare line in the middle of the box would read as a minus sign.
  divider:
    '<path d="M6 6h12" opacity=".4"/>' +
    '<path d="M3 12h18"/>' +
    '<path d="M6 18h12" opacity=".4"/>',

  link:
    '<path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/>' +
    '<path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/>',

  // ── Navigation and chrome ──────────────────────────────────────────────
  layers:
    '<path d="M12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83Z"/>' +
    '<path d="m6.08 9.5-3.5 1.6a1 1 0 0 0 0 1.81l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9a1 1 0 0 0 0-1.83l-3.5-1.59"/>' +
    '<path d="m6.08 15-3.5 1.6a1 1 0 0 0 0 1.81l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9a1 1 0 0 0 0-1.83L17.9 15"/>',

  star:
    '<path d="M11.53 2.3a.53.53 0 0 1 .94 0l2.6 5.26a.53.53 0 0 0 .4.29l5.81.85a.53.53 0 0 1 .3.9l-4.2 4.1a.53.53 0 0 0-.15.46l.99 5.78a.53.53 0 0 1-.77.56l-5.2-2.73a.53.53 0 0 0-.5 0l-5.2 2.73a.53.53 0 0 1-.76-.56l.99-5.78a.53.53 0 0 0-.16-.47l-4.2-4.09a.53.53 0 0 1 .3-.9l5.8-.85a.53.53 0 0 0 .4-.3Z"/>',

  pin:
    '<path d="M12 17v5"/>' +
    '<path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1Z"/>',

  place:
    '<path d="M20 10c0 4.99-5.54 10.19-7.4 11.8a1 1 0 0 1-1.2 0C9.54 20.19 4 14.99 4 10a8 8 0 0 1 16 0"/>' +
    '<circle cx="12" cy="10" r="3"/>',

  search: '<circle cx="11" cy="11" r="7.5"/><path d="m21 21-4.35-4.35"/>',

  // Drawn, not the ⚠ codepoint, which most platforms substitute with a
  // full-colour emoji that ignores `color` entirely.
  alert:
    '<path d="m21.73 18-8-14a2 2 0 0 0-3.46 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/>' +
    '<path d="M12 9.5v4"/><path d="M12 17.2h.01"/>',

  plus: '<path d="M5 12h14"/><path d="M12 5v14"/>',

  trash:
    '<path d="M3.5 6h17"/>' +
    '<path d="M19 6v13a2.5 2.5 0 0 1-2.5 2.5h-9A2.5 2.5 0 0 1 5 19V6"/>' +
    '<path d="M8.5 6V4.5A2 2 0 0 1 10.5 2.5h3a2 2 0 0 1 2 2V6"/>' +
    '<path d="M10 11v6"/><path d="M14 11v6"/>',

  close: '<path d="M18 6 6 18"/><path d="m6 6 12 12"/>',

  lock:
    '<rect width="16" height="11" x="4" y="10.5" rx="2.5"/>' +
    '<path d="M7.5 10.5V7a4.5 4.5 0 0 1 9 0v3.5"/>',

  // ── The todo app ───────────────────────────────────────────────────────

  // An empty checkbox and a ticked one. Circles rather than squares: the
  // tick target sits in a text line beside a title, and a round target reads
  // as "toggle me" where a square reads as part of the layout.
  circle: '<circle cx="12" cy="12" r="8.5"/>',

  check: '<circle cx="12" cy="12" r="8.5"/><path d="m8.5 12 2.5 2.5 4.5-5"/>',

  // A bare tick, for the places that already have their own container.
  tick: '<path d="M20 6 9 17l-5-5"/>',

  // Three columns with cards stacked to different heights: the shape of a
  // kanban board rather than of a table.
  board:
    '<rect width="5" height="16" x="3" y="4" rx="1.5"/>' +
    '<rect width="5" height="11" x="9.5" y="4" rx="1.5"/>' +
    '<rect width="5" height="7" x="16" y="4" rx="1.5"/>',

  // Rows with their checkboxes: a task list, distinct from `bulletList`,
  // which is a formatting mark in the editor.
  list:
    '<path d="M10 6h11"/><path d="M10 12h11"/><path d="M10 18h11"/>' +
    '<path d="m3 6 1.5 1.5L7 5"/><path d="m3 12 1.5 1.5L7 11"/><path d="m3 18 1.5 1.5L7 17"/>',

  calendar:
    '<rect width="18" height="17" x="3" y="4" rx="2.5"/>' +
    '<path d="M3 9.5h18"/><path d="M8 2.5v4"/><path d="M16 2.5v4"/>',

  clock: '<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5V12l3 1.8"/>',

  // Today: a sun over the horizon. Reads at 16px where a calendar with a
  // number in it does not.
  sun:
    '<circle cx="12" cy="12" r="4"/>' +
    '<path d="M12 3v2"/><path d="M12 19v2"/><path d="M5.6 5.6l1.4 1.4"/>' +
    '<path d="M17 17l1.4 1.4"/><path d="M3 12h2"/><path d="M19 12h2"/>' +
    '<path d="M5.6 18.4 7 17"/><path d="M17 7l1.4-1.4"/>',

  // A tray with the lip drawn in: where captured-but-unfiled things land.
  inbox:
    '<path d="M21 12v6a2.5 2.5 0 0 1-2.5 2.5h-13A2.5 2.5 0 0 1 3 18v-6"/>' +
    '<path d="M3 12h5l1.5 2.5h5L16 12h5"/>' +
    '<path d="M5.5 12 8 4.5a1 1 0 0 1 1-.7h6a1 1 0 0 1 1 .7L18.5 12"/>',

  flag: '<path d="M4 21V4.5"/><path d="M4 5h13l-2.5 4 2.5 4H4"/>',

  tag:
    '<path d="M11.6 3.5H19a1.5 1.5 0 0 1 1.5 1.5v7.4a2 2 0 0 1-.6 1.4l-6.6 6.6a2 2 0 0 1-2.8 0l-6-6a2 2 0 0 1 0-2.8l6.6-6.6a2 2 0 0 1 1.5-.5"/>' +
    '<path d="M16.5 7.5h.01"/>',

  // Disclosure. Drawn pointing right; the views rotate it for "open", so
  // there is one shape and one animation rather than two glyphs.
  chevron: '<path d="m9.5 5 7 7-7 7"/>',

  // ── The calendar app ───────────────────────────────────────────────────

  // A month grid: the week rows drawn, rather than a page with a number on
  // it. Distinct from `calendar`, which is the "a date is involved" glyph.
  month:
    '<rect width="18" height="17" x="3" y="4" rx="2.5"/>' +
    '<path d="M3 9.5h18"/><path d="M9 9.5v11"/><path d="M15 9.5v11"/>' +
    '<path d="M3 15h18"/>',

  // Seven day columns, of which one is emphasised: a week.
  week:
    '<rect width="18" height="17" x="3" y="4" rx="2.5"/>' +
    '<path d="M3 9.5h18"/><path d="M9 9.5v11"/><path d="M15 9.5v11"/>',

  // One column, with the day's hours ruled across it.
  day:
    '<rect width="18" height="17" x="3" y="4" rx="2.5"/>' +
    '<path d="M3 9.5h18"/><path d="M7 13h10"/><path d="M7 16.5h10"/>',

  // Start tracking. A triangle, drawn as a stroke like everything else, so
  // it carries the same weight as the icons beside it.
  play: '<path d="M8 5.5v13l11-6.5Z"/>',

  // Stop. A square, so the pair reads as a transport control.
  stop: '<rect width="12" height="12" x="6" y="6" rx="2"/>',

  // Refresh: an arc with an arrowhead, not a closed circle -- a closed one
  // reads as "loading" and this is something you press.
  refresh:
    '<path d="M20.5 12a8.5 8.5 0 1 1-2.5-6"/>' +
    '<path d="M20.5 4.5V10H15"/>',

  // A subscribed calendar comes from somewhere else. Meridians and a
  // parallel, which reads as "the network" at 15px where a cloud does not.
  globe:
    '<circle cx="12" cy="12" r="8.5"/>' +
    '<path d="M3.5 12h17"/>' +
    '<path d="M12 3.5a13 13 0 0 1 0 17a13 13 0 0 1 0-17"/>',

  // Hidden: the eye with the stroke through it. For a calendar that is
  // still subscribed but not drawn.
  hidden:
    '<path d="M10.7 6.2A9 9 0 0 1 12 6c5 0 9 6 9 6a15 15 0 0 1-2.4 2.9"/>' +
    '<path d="M6.6 6.9A15 15 0 0 0 3 12s4 6 9 6a9 9 0 0 0 3.6-.8"/>' +
    '<path d="M9.9 9.9a3 3 0 0 0 4.2 4.2"/>' +
    '<path d="M3 3l18 18"/>',

  // A file being read in, for importing an .ics.
  upload:
    '<path d="M21 15v3.5a2.5 2.5 0 0 1-2.5 2.5h-13A2.5 2.5 0 0 1 3 18.5V15"/>' +
    '<path d="M8 8.5 12 4.5l4 4"/><path d="M12 4.5V16"/>',

  // Two dots over two: the handle you grab to drag a card.
  grip:
    '<circle cx="9.5" cy="6.5" r="1.1"/><circle cx="14.5" cy="6.5" r="1.1"/>' +
    '<circle cx="9.5" cy="12" r="1.1"/><circle cx="14.5" cy="12" r="1.1"/>' +
    '<circle cx="9.5" cy="17.5" r="1.1"/><circle cx="14.5" cy="17.5" r="1.1"/>',

  // Two tracks with a handle on each: legible at 16px in a way that a
  // twelve-toothed cogwheel is not.
  settings:
    '<path d="M20 7h-8"/><path d="M6 7H4"/><path d="M20 17h-2"/><path d="M12 17H4"/>' +
    '<circle cx="9" cy="7" r="3"/><circle cx="15" cy="17" r="3"/>',
} as const
