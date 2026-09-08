// Context menus: what goes in one, and where it goes on the screen.
//
// Types and geometry only, with no Svelte and no store in it, so the two
// decisions worth getting wrong quietly -- which corner a menu opens from
// when it is raised near an edge, and what a list of conditionally included
// items looks like once half of them are missing -- can be checked by
// `scripts/menu.test.mjs` rather than by opening the app and right-clicking
// in the bottom corner.
//
// The menu itself is `components/ContextMenu.svelte`, opened through the
// `menu` store in `menu.svelte.ts`.

import type { IconName } from './icons'

/** Something a menu can do, or open a submenu of. */
export interface MenuAction {
  kind?: 'item'
  label: string
  icon?: IconName
  /**
   * A colour swatch in place of an icon. What a palette submenu is made of,
   * so "the teal one" can be picked by looking rather than by reading.
   */
  dot?: string
  /** Drawn ticked: a choice that is currently in force. */
  checked?: boolean
  /** Styled as destructive. Always last in its group. */
  danger?: boolean
  disabled?: boolean
  /** Quiet text on the right: a shortcut, a date, a count. */
  hint?: string
  /** A nested menu. Mutually exclusive with `run`. */
  items?: MenuItem[]
  run?: () => unknown
}

/** A row in a menu: something to do, a rule, or a heading over a group. */
export type MenuItem = { kind: 'separator' } | { kind: 'heading'; label: string } | MenuAction

/** A separator, as a value: menus are built as arrays of these. */
export const SEP: MenuItem = { kind: 'separator' }

/** How close to the edge of the window a menu is allowed to sit. */
export const MENU_MARGIN = 6

export interface Point {
  x: number
  y: number
}

export interface Size {
  width: number
  height: number
}

/**
 * Fit one axis: keep the preferred position if the menu fits, otherwise
 * open back towards the pointer, and only clamp when neither side has room.
 *
 * Flipping rather than clamping is what keeps the pointer *outside* the
 * menu near an edge. A clamped menu slides up under the cursor, so the item
 * that lands beneath it is one the user never chose -- and on the release
 * of a right-click-and-hold, is the one they get.
 */
function fit(at: number, size: number, extent: number): number {
  const last = extent - MENU_MARGIN - size
  if (at <= last) return Math.max(MENU_MARGIN, at)
  const flipped = at - size
  if (flipped >= MENU_MARGIN) return flipped
  // Taller or wider than the window: nothing fits, so show the top-left of
  // it and let it scroll.
  return Math.max(MENU_MARGIN, last)
}

/** Where a menu raised at `at` should be drawn. */
export function placeMenu(at: Point, size: Size, viewport: Size): Point {
  return {
    x: fit(at.x, size.width, viewport.width),
    y: fit(at.y, size.height, viewport.height),
  }
}

/**
 * Where a submenu should be drawn.
 *
 * `parent` is the panel it opens out of -- its `left` and `right` edges --
 * plus the `top` of the item that owns it, so the child opens beside its own
 * row rather than beside the top of the menu.
 *
 * A submenu is not flipped vertically, only slid: flipping it would put the
 * first item of a list a long way from the row that opened it.
 */
export function placeSubmenu(
  parent: { left: number; right: number; top: number },
  size: Size,
  viewport: Size,
): Point {
  const x =
    parent.right + size.width <= viewport.width - MENU_MARGIN
      ? parent.right
      : Math.max(MENU_MARGIN, parent.left - size.width)
  const last = viewport.height - MENU_MARGIN - size.height
  return { x, y: Math.max(MENU_MARGIN, Math.min(parent.top, last)) }
}

/**
 * Tidy a built menu: drop the separators that ended up with nothing between
 * them, and the headings left standing over an empty group.
 *
 * Menus here are written as one array with the inapplicable items filtered
 * out -- "Unpin" only on a pinned entry, "Make it a plan again" only on a
 * record -- and the alternative to this is every builder reasoning about
 * which of its own separators survive, which is how a menu ends up opening
 * with a rule across the top of it.
 */
export function tidyMenu(items: (MenuItem | false | null | undefined)[]): MenuItem[] {
  const rows = items.filter((item): item is MenuItem => !!item)
  const out: MenuItem[] = []
  for (const item of rows) {
    if (item.kind === 'separator') {
      // Never first, and never doubled.
      if (out.length === 0 || out[out.length - 1]!.kind === 'separator') continue
      // A separator directly after a heading would rule off the heading from
      // the group it names.
      if (out[out.length - 1]!.kind === 'heading') continue
    }
    if (item.kind === 'heading' && out[out.length - 1]?.kind === 'heading') out.pop()
    out.push(item)
  }
  // Nothing trailing either: a rule or a heading with no rows under it.
  while (out.length > 0) {
    const last = out[out.length - 1]!
    if (last.kind !== 'separator' && last.kind !== 'heading') break
    out.pop()
  }
  return out
}
